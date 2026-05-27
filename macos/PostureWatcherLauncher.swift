import AVFoundation
import AppKit
import CoreImage
import Foundation

final class BadgerPreviewView: NSView {
    private let badgerWidth: CGFloat = 296
    private let badgerHeight: CGFloat = 128
    private var points: [CGPoint] = []
    private var note = ""
    private var message = "waiting"

    override var isFlipped: Bool { true }

    func applyDisplayPayload(_ line: String) {
        guard line.hasPrefix("DISPLAY,") else { return }
        let payload = String(line.dropFirst("DISPLAY,".count))
        let parts = payload.split(separator: ",", omittingEmptySubsequences: false).map(String.init)
        guard let kind = parts.first else { return }

        if kind == "M" {
            points = []
            note = ""
            message = parts.dropFirst().joined(separator: ",")
            needsDisplay = true
            return
        }

        guard kind == "P", parts.count >= 2, let count = Int(parts[1]) else { return }
        var parsed: [CGPoint] = []
        let coordStart = 2
        for index in 0..<count {
            let xIndex = coordStart + index * 2
            let yIndex = xIndex + 1
            guard yIndex < parts.count, let x = Double(parts[xIndex]), let y = Double(parts[yIndex]) else {
                return
            }
            parsed.append(CGPoint(x: x, y: y))
        }
        points = parsed
        note = coordStart + count * 2 < parts.count ? parts[coordStart + count * 2] : ""
        message = ""
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.windowBackgroundColor.setFill()
        bounds.fill()

        let scale = min(bounds.width / badgerHeight, bounds.height / badgerWidth)
        let displaySize = CGSize(width: badgerHeight * scale, height: badgerWidth * scale)
        let origin = CGPoint(
            x: (bounds.width - displaySize.width) / 2,
            y: (bounds.height - displaySize.height) / 2
        )
        let displayRect = CGRect(origin: origin, size: displaySize)

        NSColor.white.setFill()
        displayRect.fill()
        NSColor.black.setStroke()
        let border = NSBezierPath(rect: displayRect)
        border.lineWidth = 1
        border.stroke()

        func mapPoint(_ point: CGPoint) -> CGPoint {
            CGPoint(x: origin.x + point.y * scale, y: origin.y + point.x * scale)
        }

        if !message.isEmpty {
            let attrs: [NSAttributedString.Key: Any] = [
                .font: NSFont.systemFont(ofSize: 16, weight: .semibold),
                .foregroundColor: NSColor.black,
                .paragraphStyle: centeredParagraph()
            ]
            let textRect = displayRect.insetBy(dx: 10, dy: displaySize.height * 0.42)
            message.draw(in: textRect, withAttributes: attrs)
            return
        }

        let guide = NSBezierPath()
        guide.move(to: mapPoint(CGPoint(x: 18, y: badgerHeight / 2)))
        guide.line(to: mapPoint(CGPoint(x: badgerWidth - 18, y: badgerHeight / 2)))
        guide.lineWidth = 1
        guide.stroke()

        if points.count > 1 {
            let curve = NSBezierPath()
            curve.move(to: mapPoint(points[0]))
            for point in points.dropFirst() {
                curve.line(to: mapPoint(point))
            }
            curve.lineWidth = 4
            curve.lineJoinStyle = .round
            curve.lineCapStyle = .round
            curve.stroke()
        }

        for point in points {
            let center = mapPoint(point)
            NSBezierPath(rect: CGRect(x: center.x - 4, y: center.y - 4, width: 8, height: 8)).fill()
        }

        if !note.isEmpty {
            let attrs: [NSAttributedString.Key: Any] = [
                .font: NSFont.monospacedSystemFont(ofSize: 11, weight: .regular),
                .foregroundColor: NSColor.black
            ]
            note.draw(at: CGPoint(x: displayRect.minX + 8, y: displayRect.minY + 8), withAttributes: attrs)
        }
    }

    private func centeredParagraph() -> NSParagraphStyle {
        let style = NSMutableParagraphStyle()
        style.alignment = .center
        return style
    }
}

final class PostureWatcherLauncher: NSObject, NSApplicationDelegate, AVCaptureVideoDataOutputSampleBufferDelegate {
    private var process: Process?
    private var previewWindow: NSWindow?
    private let previewView = BadgerPreviewView()
    private var cameraPopup: NSPopUpButton?
    private var selectedCameraName: String?
    private var analyzerOutputBuffer = ""
    private let session = AVCaptureSession()
    private let captureQueue = DispatchQueue(label: "local.posture-watcher.capture")
    private let ciContext = CIContext()
    private var lastWrite = Date.distantPast
    private var frameURL: URL!
    private var logURL: URL!
    private var intervalSeconds = 5.0

    func applicationDidFinishLaunching(_ notification: Notification) {
        do {
            let supportURL = try appSupportURL()
            logURL = supportURL.appendingPathComponent("posture-watcher.log")
            log("app launched")
        } catch {
            fputs("Posture Watcher log setup failed: \(error.localizedDescription)\n", stderr)
        }
        setupPreviewWindow()
        requestCameraThenRun()
    }

    func applicationWillTerminate(_ notification: Notification) {
        log("app terminating")
        session.stopRunning()
        process?.terminate()
    }

    private func setupPreviewWindow() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 260, height: 610),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Posture Watcher"

        let root = NSStackView()
        root.orientation = .vertical
        root.alignment = .centerX
        root.spacing = 12
        root.edgeInsets = NSEdgeInsets(top: 14, left: 14, bottom: 14, right: 14)
        root.translatesAutoresizingMaskIntoConstraints = false

        let cameraRow = NSStackView()
        cameraRow.orientation = .horizontal
        cameraRow.alignment = .centerY
        cameraRow.spacing = 8
        cameraRow.translatesAutoresizingMaskIntoConstraints = false

        let cameraLabel = NSTextField(labelWithString: "Camera")
        let popup = NSPopUpButton(frame: .zero, pullsDown: false)
        popup.target = self
        popup.action = #selector(cameraSelectionChanged(_:))
        popup.translatesAutoresizingMaskIntoConstraints = false
        popup.widthAnchor.constraint(equalToConstant: 175).isActive = true
        cameraPopup = popup

        cameraRow.addArrangedSubview(cameraLabel)
        cameraRow.addArrangedSubview(popup)
        root.addArrangedSubview(cameraRow)

        previewView.translatesAutoresizingMaskIntoConstraints = false
        previewView.widthAnchor.constraint(equalToConstant: 210).isActive = true
        previewView.heightAnchor.constraint(equalToConstant: 485).isActive = true
        root.addArrangedSubview(previewView)

        let content = NSView()
        content.addSubview(root)
        NSLayoutConstraint.activate([
            root.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            root.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            root.topAnchor.constraint(equalTo: content.topAnchor),
            root.bottomAnchor.constraint(equalTo: content.bottomAnchor)
        ])
        window.contentView = content
        previewWindow = window
        populateCameraPopup()
        window.center()
        window.makeKeyAndOrderFront(nil)
    }

    private func populateCameraPopup() {
        guard let cameraPopup else { return }
        let devices = availableCameras()
        let preferred = preferredCameraName()
        cameraPopup.removeAllItems()
        for device in devices {
            cameraPopup.addItem(withTitle: device.localizedName)
        }
        let selected = devices.first { $0.localizedName == preferred }
            ?? devices.first { $0.localizedName.contains(preferred) }
            ?? devices.first
        if let selected {
            selectedCameraName = selected.localizedName
            cameraPopup.selectItem(withTitle: selected.localizedName)
        }
    }

    @objc private func cameraSelectionChanged(_ sender: NSPopUpButton) {
        guard let title = sender.selectedItem?.title else { return }
        selectedCameraName = title
        UserDefaults.standard.set(title, forKey: "SelectedCameraName")
        log("camera selection changed: \(title)")
        guard frameURL != nil else { return }
        do {
            session.stopRunning()
            try configureCapture()
            session.startRunning()
            log("AVFoundation session restarted")
        } catch {
            log("camera restart failed: \(error.localizedDescription)")
            showMessage(error.localizedDescription)
        }
    }

    private func requestCameraThenRun() {
        log("camera authorization status: \(AVCaptureDevice.authorizationStatus(for: .video).rawValue)")
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            startCaptureAndAnalyzer()
        case .notDetermined:
            log("requesting camera access")
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async {
                    self.log("camera access prompt result: \(granted)")
                    if granted {
                        self.startCaptureAndAnalyzer()
                    } else {
                        self.showMessage("Camera access was denied.")
                        NSApp.terminate(nil)
                    }
                }
            }
        case .denied, .restricted:
            showMessage("Camera access is not enabled for Posture Watcher. Enable it in System Settings > Privacy & Security > Camera.")
            NSApp.terminate(nil)
        @unknown default:
            showMessage("Unknown camera permission state.")
            NSApp.terminate(nil)
        }
    }

    private func startCaptureAndAnalyzer() {
        do {
            let supportURL = try appSupportURL()
            frameURL = supportURL.appendingPathComponent("latest-frame.jpg")
            intervalSeconds = Double(ProcessInfo.processInfo.environment["POSTURE_WATCHER_INTERVAL_SECS"] ?? "5") ?? 5.0
            log("support directory: \(supportURL.path)")
            log("frame path: \(frameURL.path)")
            log("capture interval: \(intervalSeconds)s")
            try configureCapture()
            runPostureWatcher(inputURL: frameURL, supportURL: supportURL)
            session.startRunning()
            log("AVFoundation session started")
        } catch {
            log("startup failed: \(error.localizedDescription)")
            showMessage(error.localizedDescription)
            NSApp.terminate(nil)
        }
    }

    private func configureCapture() throws {
        session.beginConfiguration()
        session.sessionPreset = .hd1280x720
        for input in session.inputs {
            session.removeInput(input)
        }
        for output in session.outputs {
            session.removeOutput(output)
        }

        guard let device = selectedCamera() else {
            throw AppError.message("Could not find the requested camera.")
        }
        log("selected camera: \(device.localizedName)")
        let input = try AVCaptureDeviceInput(device: device)
        guard session.canAddInput(input) else {
            throw AppError.message("Could not add camera input.")
        }
        session.addInput(input)

        let output = AVCaptureVideoDataOutput()
        output.videoSettings = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ]
        output.alwaysDiscardsLateVideoFrames = true
        output.setSampleBufferDelegate(self, queue: captureQueue)

        guard session.canAddOutput(output) else {
            throw AppError.message("Could not add camera frame output.")
        }
        session.addOutput(output)

        session.commitConfiguration()
    }

    private func selectedCamera() -> AVCaptureDevice? {
        let requested = preferredCameraName()
        let devices = availableCameras()
        log("available cameras: \(devices.map { $0.localizedName }.joined(separator: ", "))")
        let selected = devices.first { $0.localizedName == requested }
            ?? devices.first { $0.localizedName.contains(requested) }
            ?? devices.first
            ?? AVCaptureDevice.default(for: .video)
        if let selected {
            selectedCameraName = selected.localizedName
            DispatchQueue.main.async {
                self.cameraPopup?.selectItem(withTitle: selected.localizedName)
            }
        }
        return selected
    }

    private func availableCameras() -> [AVCaptureDevice] {
        let discovery = AVCaptureDevice.DiscoverySession(
            deviceTypes: [.external, .builtInWideAngleCamera, .continuityCamera],
            mediaType: .video,
            position: .unspecified
        )
        return discovery.devices
    }

    private func preferredCameraName() -> String {
        selectedCameraName
            ?? UserDefaults.standard.string(forKey: "SelectedCameraName")
            ?? ProcessInfo.processInfo.environment["POSTURE_WATCHER_CAMERA"]
            ?? "Logitech Webcam C930e"
    }

    private func runPostureWatcher(inputURL: URL, supportURL: URL) {
        let bundle = Bundle.main
        guard let binaryPath = bundle.path(forResource: "posture-watcher", ofType: nil) else {
            showMessage("Missing posture-watcher inside the app bundle.")
            NSApp.terminate(nil)
            return
        }

        let env = ProcessInfo.processInfo.environment
        let port = env["POSTURE_WATCHER_PORT"] ?? "/dev/cu.usbmodem83201"
        let window = env["POSTURE_WATCHER_WINDOW_SECS"] ?? "120"
        let interval = env["POSTURE_WATCHER_INTERVAL_SECS"] ?? "5"
        let noPersonAfter = env["POSTURE_WATCHER_NO_PERSON_AFTER_SECS"] ?? "30"
        let rotation = env["POSTURE_WATCHER_ROTATE"] ?? "ccw90"
        let outDir = supportURL.appendingPathComponent("analysis").path

        let task = Process()
        task.executableURL = URL(fileURLWithPath: binaryPath)
        var arguments = [
            "live-file",
            "--input", inputURL.path,
            "--port", port,
            "--window-secs", window,
            "--interval-secs", interval,
            "--no-person-after-secs", noPersonAfter,
            "--rotate", rotation,
            "--out-dir", outDir
        ]
        if env["POSTURE_WATCHER_NO_BADGER"] == "1" || !FileManager.default.fileExists(atPath: port) {
            arguments.append("--no-badger")
            log("Badger disabled for this run")
        }
        task.arguments = arguments
        task.currentDirectoryURL = URL(fileURLWithPath: bundle.resourcePath ?? NSHomeDirectory())
        log("launching analyzer: \(binaryPath) \(task.arguments?.joined(separator: " ") ?? "")")

        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = pipe

        pipe.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return }
            fputs(text, stderr)
            self.handleAnalyzerOutput(text)
        }

        task.terminationHandler = { _ in
            DispatchQueue.main.async {
                self.log("analyzer exited with status \(task.terminationStatus)")
                pipe.fileHandleForReading.readabilityHandler = nil
                NSApp.terminate(nil)
            }
        }

        do {
            try task.run()
            process = task
            log("analyzer process started")
        } catch {
            log("analyzer launch failed: \(error.localizedDescription)")
            showMessage("Could not start posture-watcher: \(error.localizedDescription)")
            NSApp.terminate(nil)
        }
    }

    private func handleAnalyzerOutput(_ text: String) {
        log(text.trimmingCharacters(in: .whitespacesAndNewlines))
        analyzerOutputBuffer += text
        while let newline = analyzerOutputBuffer.firstIndex(of: "\n") {
            let line = String(analyzerOutputBuffer[..<newline])
            analyzerOutputBuffer.removeSubrange(...newline)
            handleAnalyzerLine(line.trimmingCharacters(in: .whitespacesAndNewlines))
        }
    }

    private func handleAnalyzerLine(_ line: String) {
        guard line.hasPrefix("DISPLAY,") else { return }
        DispatchQueue.main.async {
            self.previewView.applyDisplayPayload(line)
        }
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        let now = Date()
        guard now.timeIntervalSince(lastWrite) >= intervalSeconds else { return }
        lastWrite = now

        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let ciImage = CIImage(cvPixelBuffer: pixelBuffer)
        guard let cgImage = ciContext.createCGImage(ciImage, from: ciImage.extent) else { return }
        let rep = NSBitmapImageRep(cgImage: cgImage)
        guard let data = rep.representation(using: .jpeg, properties: [.compressionFactor: 0.85]) else { return }

        do {
            let tmpURL = frameURL.appendingPathExtension("tmp")
            try data.write(to: tmpURL, options: .atomic)
            if FileManager.default.fileExists(atPath: frameURL.path) {
                _ = try FileManager.default.replaceItemAt(frameURL, withItemAt: tmpURL)
            } else {
                try FileManager.default.moveItem(at: tmpURL, to: frameURL)
            }
            log("wrote frame: \(frameURL.path)")
        } catch {
            let message = "Posture Watcher frame write failed: \(error.localizedDescription)"
            log(message)
            fputs(message + "\n", stderr)
        }
    }

    private func appSupportURL() throws -> URL {
        let fm = FileManager.default
        let base = try fm.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        let url = base.appendingPathComponent("Posture Watcher", isDirectory: true)
        try fm.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func showMessage(_ text: String) {
        log("alert: \(text)")
        let alert = NSAlert()
        alert.messageText = "Posture Watcher"
        alert.informativeText = text
        alert.alertStyle = .warning
        alert.runModal()
    }

    private func log(_ text: String) {
        let timestamp = ISO8601DateFormatter().string(from: Date())
        let line = "[\(timestamp)] \(text)\n"
        fputs(line, stderr)

        guard let logURL else { return }
        do {
            if !FileManager.default.fileExists(atPath: logURL.path) {
                try line.write(to: logURL, atomically: true, encoding: .utf8)
                return
            }
            let handle = try FileHandle(forWritingTo: logURL)
            try handle.seekToEnd()
            if let data = line.data(using: .utf8) {
                try handle.write(contentsOf: data)
            }
            try handle.close()
        } catch {
            fputs("Posture Watcher log write failed: \(error.localizedDescription)\n", stderr)
        }
    }
}

enum AppError: LocalizedError {
    case message(String)

    var errorDescription: String? {
        switch self {
        case .message(let text): text
        }
    }
}

let app = NSApplication.shared
let delegate = PostureWatcherLauncher()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.activate(ignoringOtherApps: true)
app.run()
