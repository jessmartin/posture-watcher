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
            var messageStart = 1
            if parts.count > 1 && Self.isOrientationCode(parts[1]) {
                messageStart = 2
            }
            points = []
            note = ""
            message = parts.dropFirst(messageStart).joined(separator: ",")
            needsDisplay = true
            return
        }

        guard kind == "P", parts.count >= 2 else { return }
        var countIndex = 1
        if Self.isOrientationCode(parts[countIndex]) {
            countIndex += 1
        }
        guard countIndex < parts.count, let count = Int(parts[countIndex]) else { return }
        var parsed: [CGPoint] = []
        let coordStart = countIndex + 1
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

    private static func isOrientationCode(_ value: String) -> Bool {
        value == "T" || value == "B"
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
    private var modePopup: NSPopUpButton?
    private var badgerOrientationPopup: NSPopUpButton?
    private var autoModeLabel: NSTextField?
    private var badgerStatusLabel: NSTextField?
    private var tagStatusLabel: NSTextField?
    private var placementStatusLabel: NSTextField?
    private var sampleStatusLabel: NSTextField?
    private var baselineStatusLabel: NSTextField?
    private var selectedCameraName: String?
    private var latestDetectedMode: String?
    private var analyzerOutputBuffer = ""
    private let session = AVCaptureSession()
    private let captureQueue = DispatchQueue(label: "local.posture-watcher.capture")
    private let ciContext = CIContext()
    private var lastWrite = Date.distantPast
    private var lastBurstWrite = Date.distantPast
    private var frameURL: URL!
    private var burstDirURL: URL!
    private var logURL: URL!
    private var intervalSeconds = 5.0
    private var burstWriteIntervalSeconds = 0.25
    private var burstFrameCount = 8
    private var burstFrameIndex = 0

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
            contentRect: NSRect(x: 0, y: 0, width: 260, height: 900),
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

        let modeRow = NSStackView()
        modeRow.orientation = .horizontal
        modeRow.alignment = .centerY
        modeRow.spacing = 8
        modeRow.translatesAutoresizingMaskIntoConstraints = false

        let modeLabel = NSTextField(labelWithString: "Mode")
        let mode = NSPopUpButton(frame: .zero, pullsDown: false)
        mode.addItems(withTitles: ["Auto", "Sitting", "Standing"])
        mode.selectItem(withTitle: UserDefaults.standard.string(forKey: "PostureMode") ?? "Auto")
        mode.target = self
        mode.action = #selector(modeSelectionChanged(_:))
        mode.translatesAutoresizingMaskIntoConstraints = false
        mode.widthAnchor.constraint(equalToConstant: 175).isActive = true
        modePopup = mode

        modeRow.addArrangedSubview(modeLabel)
        modeRow.addArrangedSubview(mode)
        root.addArrangedSubview(modeRow)

        let orientationRow = NSStackView()
        orientationRow.orientation = .horizontal
        orientationRow.alignment = .centerY
        orientationRow.spacing = 8
        orientationRow.translatesAutoresizingMaskIntoConstraints = false

        let orientationLabel = NSTextField(labelWithString: "Badger USB")
        let orientation = NSPopUpButton(frame: .zero, pullsDown: false)
        orientation.addItems(withTitles: ["USB-C Top", "USB-C Bottom"])
        let savedOrientation = UserDefaults.standard.string(forKey: "BadgerOrientation")
            ?? (ProcessInfo.processInfo.environment["POSTURE_WATCHER_BADGER_ORIENTATION"] == "usb-bottom"
                ? "USB-C Bottom"
                : "USB-C Top")
        orientation.selectItem(withTitle: savedOrientation)
        orientation.target = self
        orientation.action = #selector(badgerOrientationChanged(_:))
        orientation.translatesAutoresizingMaskIntoConstraints = false
        orientation.widthAnchor.constraint(equalToConstant: 150).isActive = true
        badgerOrientationPopup = orientation

        orientationRow.addArrangedSubview(orientationLabel)
        orientationRow.addArrangedSubview(orientation)
        root.addArrangedSubview(orientationRow)

        let detectedRow = NSStackView()
        detectedRow.orientation = .horizontal
        detectedRow.alignment = .centerY
        detectedRow.spacing = 8
        detectedRow.translatesAutoresizingMaskIntoConstraints = false

        let detectedLabel = NSTextField(labelWithString: "Detected")
        let detectedMode = NSTextField(labelWithString: "waiting")
        detectedMode.textColor = .secondaryLabelColor
        detectedMode.alignment = .left
        detectedMode.translatesAutoresizingMaskIntoConstraints = false
        detectedMode.widthAnchor.constraint(equalToConstant: 155).isActive = true
        autoModeLabel = detectedMode

        detectedRow.addArrangedSubview(detectedLabel)
        detectedRow.addArrangedSubview(detectedMode)
        root.addArrangedSubview(detectedRow)

        let badgerRow = NSStackView()
        badgerRow.orientation = .horizontal
        badgerRow.alignment = .centerY
        badgerRow.spacing = 8
        badgerRow.translatesAutoresizingMaskIntoConstraints = false

        let badgerLabel = NSTextField(labelWithString: "Badger")
        let statusLabel = NSTextField(labelWithString: "checking")
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.alignment = .left
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.widthAnchor.constraint(equalToConstant: 175).isActive = true
        badgerStatusLabel = statusLabel

        badgerRow.addArrangedSubview(badgerLabel)
        badgerRow.addArrangedSubview(statusLabel)
        root.addArrangedSubview(badgerRow)

        let tagRow = NSStackView()
        tagRow.orientation = .horizontal
        tagRow.alignment = .centerY
        tagRow.spacing = 8
        tagRow.translatesAutoresizingMaskIntoConstraints = false

        let tagLabel = NSTextField(labelWithString: "Tags")
        let tagStatus = NSTextField(labelWithString: "waiting")
        tagStatus.textColor = .secondaryLabelColor
        tagStatus.alignment = .left
        tagStatus.translatesAutoresizingMaskIntoConstraints = false
        tagStatus.widthAnchor.constraint(equalToConstant: 175).isActive = true
        tagStatusLabel = tagStatus

        tagRow.addArrangedSubview(tagLabel)
        tagRow.addArrangedSubview(tagStatus)
        root.addArrangedSubview(tagRow)

        let placementRow = NSStackView()
        placementRow.orientation = .horizontal
        placementRow.alignment = .centerY
        placementRow.spacing = 8
        placementRow.translatesAutoresizingMaskIntoConstraints = false

        let placementLabel = NSTextField(labelWithString: "Placement")
        let placementStatus = NSTextField(labelWithString: "waiting")
        placementStatus.textColor = .secondaryLabelColor
        placementStatus.alignment = .left
        placementStatus.translatesAutoresizingMaskIntoConstraints = false
        placementStatus.widthAnchor.constraint(equalToConstant: 145).isActive = true
        placementStatusLabel = placementStatus

        placementRow.addArrangedSubview(placementLabel)
        placementRow.addArrangedSubview(placementStatus)
        root.addArrangedSubview(placementRow)

        let sampleRow = NSStackView()
        sampleRow.orientation = .horizontal
        sampleRow.alignment = .centerY
        sampleRow.spacing = 8
        sampleRow.translatesAutoresizingMaskIntoConstraints = false

        let sampleLabel = NSTextField(labelWithString: "Samples")
        let sampleStatus = NSTextField(labelWithString: "0/3 sit, 0/3 stand")
        sampleStatus.textColor = .secondaryLabelColor
        sampleStatus.alignment = .left
        sampleStatus.translatesAutoresizingMaskIntoConstraints = false
        sampleStatus.widthAnchor.constraint(equalToConstant: 145).isActive = true
        sampleStatusLabel = sampleStatus

        sampleRow.addArrangedSubview(sampleLabel)
        sampleRow.addArrangedSubview(sampleStatus)
        root.addArrangedSubview(sampleRow)

        let baselineRow = NSStackView()
        baselineRow.orientation = .horizontal
        baselineRow.alignment = .centerY
        baselineRow.spacing = 8
        baselineRow.translatesAutoresizingMaskIntoConstraints = false

        let baselineLabel = NSTextField(labelWithString: "Baseline")
        let baselineStatus = NSTextField(labelWithString: "not set")
        baselineStatus.textColor = .secondaryLabelColor
        baselineStatus.alignment = .left
        baselineStatus.translatesAutoresizingMaskIntoConstraints = false
        baselineStatus.widthAnchor.constraint(equalToConstant: 145).isActive = true
        baselineStatusLabel = baselineStatus

        baselineRow.addArrangedSubview(baselineLabel)
        baselineRow.addArrangedSubview(baselineStatus)
        root.addArrangedSubview(baselineRow)

        previewView.translatesAutoresizingMaskIntoConstraints = false
        previewView.widthAnchor.constraint(equalToConstant: 210).isActive = true
        previewView.heightAnchor.constraint(equalToConstant: 455).isActive = true
        root.addArrangedSubview(previewView)

        let buttonRow = NSStackView()
        buttonRow.orientation = .horizontal
        buttonRow.alignment = .centerY
        buttonRow.spacing = 8
        buttonRow.translatesAutoresizingMaskIntoConstraints = false

        let debugButton = NSButton(title: "Open Debug", target: self, action: #selector(openDebugFrame(_:)))
        debugButton.bezelStyle = .rounded
        debugButton.translatesAutoresizingMaskIntoConstraints = false
        debugButton.widthAnchor.constraint(equalToConstant: 101).isActive = true

        let stickerButton = NSButton(title: "Open Tags", target: self, action: #selector(openStickerSheet(_:)))
        stickerButton.bezelStyle = .rounded
        stickerButton.translatesAutoresizingMaskIntoConstraints = false
        stickerButton.widthAnchor.constraint(equalToConstant: 101).isActive = true

        buttonRow.addArrangedSubview(debugButton)
        buttonRow.addArrangedSubview(stickerButton)
        root.addArrangedSubview(buttonRow)

        let saveButton = NSButton(title: "Save Sample", target: self, action: #selector(saveSample(_:)))
        saveButton.bezelStyle = .rounded
        saveButton.translatesAutoresizingMaskIntoConstraints = false
        saveButton.widthAnchor.constraint(equalToConstant: 210).isActive = true
        root.addArrangedSubview(saveButton)

        let baselineButtonRow = NSStackView()
        baselineButtonRow.orientation = .horizontal
        baselineButtonRow.alignment = .centerY
        baselineButtonRow.spacing = 8
        baselineButtonRow.translatesAutoresizingMaskIntoConstraints = false

        let calibrateButton = NSButton(title: "Calibrate", target: self, action: #selector(calibrateBaseline(_:)))
        calibrateButton.bezelStyle = .rounded
        calibrateButton.translatesAutoresizingMaskIntoConstraints = false
        calibrateButton.widthAnchor.constraint(equalToConstant: 101).isActive = true

        let openBaselineButton = NSButton(title: "Open Base", target: self, action: #selector(openBaseline(_:)))
        openBaselineButton.bezelStyle = .rounded
        openBaselineButton.translatesAutoresizingMaskIntoConstraints = false
        openBaselineButton.widthAnchor.constraint(equalToConstant: 101).isActive = true

        baselineButtonRow.addArrangedSubview(calibrateButton)
        baselineButtonRow.addArrangedSubview(openBaselineButton)
        root.addArrangedSubview(baselineButtonRow)

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
        refreshSampleStatus()
        refreshBaselineStatus()
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

    @objc private func modeSelectionChanged(_ sender: NSPopUpButton) {
        guard let title = sender.selectedItem?.title else { return }
        UserDefaults.standard.set(title, forKey: "PostureMode")
        log("posture mode changed: \(title)")
        restartAnalyzerForModeChange()
    }

    @objc private func badgerOrientationChanged(_ sender: NSPopUpButton) {
        guard let title = sender.selectedItem?.title else { return }
        UserDefaults.standard.set(title, forKey: "BadgerOrientation")
        log("Badger orientation changed: \(title)")
        restartAnalyzerForModeChange()
    }

    @objc private func openStickerSheet(_ sender: NSButton) {
        do {
            let url = try generateStickerSheet()
            NSWorkspace.shared.open(url)
            log("opened sticker sheet: \(url.path)")
        } catch {
            log("sticker sheet open failed: \(error.localizedDescription)")
            showMessage(error.localizedDescription)
        }
    }

    @objc private func openDebugFrame(_ sender: NSButton) {
        do {
            let url = try debugFrameURL()
            NSWorkspace.shared.open(url)
            log("opened debug frame: \(url.path)")
        } catch {
            log("debug frame open failed: \(error.localizedDescription)")
            showMessage(error.localizedDescription)
        }
    }

    @objc private func saveSample(_ sender: NSButton) {
        do {
            let urls = try saveCurrentSample()
            log("saved sample: \(urls.map { $0.path }.joined(separator: ", "))")
            refreshSampleStatus()
            refreshBaselineStatus()
        } catch {
            log("sample save failed: \(error.localizedDescription)")
            showMessage(error.localizedDescription)
        }
    }

    @objc private func calibrateBaseline(_ sender: NSButton) {
        do {
            sender.isEnabled = false
            baselineStatusLabel?.stringValue = "calibrating"
            baselineStatusLabel?.textColor = .systemOrange
            defer { sender.isEnabled = true }
            let output = try runBaselineCalibration()
            if !output.isEmpty {
                log(output)
            }
            refreshSampleStatus()
            refreshBaselineStatus()
        } catch {
            log("baseline calibration failed: \(error.localizedDescription)")
            refreshSampleStatus()
            refreshBaselineStatus()
            showMessage(error.localizedDescription)
        }
    }

    @objc private func openBaseline(_ sender: NSButton) {
        do {
            let url = try baselineFileURL()
            guard FileManager.default.fileExists(atPath: url.path) else {
                throw AppError.message("No baseline file yet. Save good sitting and standing samples, then click Calibrate.")
            }
            NSWorkspace.shared.open(url)
            log("opened baseline: \(url.path)")
        } catch {
            log("open baseline failed: \(error.localizedDescription)")
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
                        self.showCameraPermissionProblem("Camera access was denied.")
                    }
                }
            }
        case .denied, .restricted:
            showCameraPermissionProblem("Camera access is not enabled for Posture Watcher. Enable it in System Settings > Privacy & Security > Camera.")
        @unknown default:
            showCameraPermissionProblem("Unknown camera permission state.")
        }
    }

    private func showCameraPermissionProblem(_ text: String) {
        tagStatusLabel?.stringValue = "camera blocked"
        tagStatusLabel?.textColor = .systemRed
        autoModeLabel?.stringValue = "unknown"
        autoModeLabel?.textColor = .secondaryLabelColor
        placementStatusLabel?.stringValue = "camera blocked"
        placementStatusLabel?.textColor = .systemRed
        previewView.applyDisplayPayload("DISPLAY,M,Camera access needed")
        showMessage(text)
    }

    private func startCaptureAndAnalyzer() {
        do {
            let supportURL = try appSupportURL()
            frameURL = supportURL.appendingPathComponent("latest-frame.jpg")
            burstDirURL = supportURL.appendingPathComponent("burst", isDirectory: true)
            let env = ProcessInfo.processInfo.environment
            intervalSeconds = Double(env["POSTURE_WATCHER_INTERVAL_SECS"] ?? "5") ?? 5.0
            burstWriteIntervalSeconds = Double(env["POSTURE_WATCHER_BURST_FRAME_INTERVAL_SECS"] ?? "0.25") ?? 0.25
            burstFrameCount = max(1, Int(env["POSTURE_WATCHER_BURST_FRAMES"] ?? "8") ?? 8)
            try FileManager.default.createDirectory(at: burstDirURL, withIntermediateDirectories: true)
            log("support directory: \(supportURL.path)")
            log("frame path: \(frameURL.path)")
            log("capture interval: \(intervalSeconds)s")
            log("burst directory: \(burstDirURL.path)")
            log("burst capture: \(burstFrameCount) frames every \(burstWriteIntervalSeconds)s")
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

    private func analyzerModeArgument() -> String {
        let title = modePopup?.selectedItem?.title
            ?? UserDefaults.standard.string(forKey: "PostureMode")
            ?? "Auto"
        switch title {
        case "Sitting":
            return "sitting"
        case "Standing":
            return "standing"
        default:
            return "auto"
        }
    }

    private func badgerOrientationArgument() -> String {
        let title = badgerOrientationPopup?.selectedItem?.title
            ?? UserDefaults.standard.string(forKey: "BadgerOrientation")
            ?? ProcessInfo.processInfo.environment["POSTURE_WATCHER_BADGER_ORIENTATION"]
            ?? "USB-C Top"
        switch title {
        case "USB-C Bottom", "usb-bottom":
            return "usb-bottom"
        default:
            return "usb-top"
        }
    }

    private func runPostureWatcher(inputURL: URL, supportURL: URL) {
        let bundle = Bundle.main
        let binaryPath: String
        do {
            binaryPath = try postureWatcherBinaryPath()
        } catch {
            showMessage(error.localizedDescription)
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
        let burstDir = supportURL.appendingPathComponent("burst", isDirectory: true).path
        let burstFrames = env["POSTURE_WATCHER_BURST_FRAMES"] ?? "\(burstFrameCount)"
        let c7AnchorOffset = env["POSTURE_WATCHER_C7_ANCHOR_OFFSET_TAG_WIDTHS"] ?? "0.75"
        let postureMode = analyzerModeArgument()
        let badgerOrientation = badgerOrientationArgument()

        let task = Process()
        task.executableURL = URL(fileURLWithPath: binaryPath)
        var arguments = [
            "live-file",
            "--input", inputURL.path,
            "--burst-dir", burstDir,
            "--burst-frames", burstFrames,
            "--port", port,
            "--window-secs", window,
            "--interval-secs", interval,
            "--no-person-after-secs", noPersonAfter,
            "--rotate", rotation,
            "--out-dir", outDir,
            "--mode", postureMode,
            "--c7-anchor-offset-tag-widths", c7AnchorOffset,
            "--badger-orientation", badgerOrientation
        ]
        if env["POSTURE_WATCHER_NO_BADGER"] == "1" || !FileManager.default.fileExists(atPath: port) {
            arguments.append("--no-badger")
            log("Badger disabled for this run")
        }
        do {
            arguments.append(contentsOf: ["--baseline", try baselineFileURL().path])
        } catch {
            log("baseline path unavailable: \(error.localizedDescription)")
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

    private func restartAnalyzerForModeChange() {
        guard frameURL != nil else { return }
        do {
            let supportURL = try appSupportURL()
            process?.terminationHandler = nil
            process?.terminate()
            process = nil
            analyzerOutputBuffer = ""
            previewView.applyDisplayPayload("DISPLAY,M,restarting")
            runPostureWatcher(inputURL: frameURL, supportURL: supportURL)
        } catch {
            log("analyzer restart failed: \(error.localizedDescription)")
            showMessage(error.localizedDescription)
        }
    }

    private func generateStickerSheet() throws -> URL {
        let supportURL = try appSupportURL()
        let outURL = supportURL.appendingPathComponent("posture-tags.svg")
        let task = Process()
        task.executableURL = URL(fileURLWithPath: try postureWatcherBinaryPath())
        task.arguments = [
            "stickers",
            "--out", outURL.path
        ]
        task.currentDirectoryURL = URL(fileURLWithPath: Bundle.main.resourcePath ?? NSHomeDirectory())

        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = pipe
        try task.run()
        task.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let text = String(data: data, encoding: .utf8) ?? ""
        if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            log(text.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        guard task.terminationStatus == 0 else {
            throw AppError.message("Could not generate AprilTag sticker sheet.")
        }
        return outURL
    }

    private func debugFrameURL() throws -> URL {
        let supportURL = try appSupportURL()
        let tagDebugURL = supportURL
            .appendingPathComponent("analysis", isDirectory: true)
            .appendingPathComponent("latest-tags.png")
        if FileManager.default.fileExists(atPath: tagDebugURL.path) {
            return tagDebugURL
        }
        let frameURL = supportURL.appendingPathComponent("latest-frame.jpg")
        if FileManager.default.fileExists(atPath: frameURL.path) {
            return frameURL
        }
        throw AppError.message("No camera frame has been captured yet.")
    }

    private func saveCurrentSample() throws -> [URL] {
        let supportURL = try appSupportURL()
        let sampleMode = try currentSampleMode()
        let sampleDir = supportURL
            .appendingPathComponent("samples", isDirectory: true)
            .appendingPathComponent(sampleMode.folderName, isDirectory: true)
        let fm = FileManager.default
        try fm.createDirectory(at: sampleDir, withIntermediateDirectories: true)

        let stamp = sampleTimestamp()
        let latestFrameURL = supportURL.appendingPathComponent("latest-frame.jpg")
        guard fm.fileExists(atPath: latestFrameURL.path) else {
            throw AppError.message("No camera frame has been captured yet.")
        }

        var savedURLs: [URL] = []
        let frameOutURL = sampleDir.appendingPathComponent("\(stamp)-frame.jpg")
        try copyReplacing(from: latestFrameURL, to: frameOutURL)
        savedURLs.append(frameOutURL)

        let analysisDir = supportURL.appendingPathComponent("analysis", isDirectory: true)
        let optionalFiles = [
            ("latest-tags.png", "\(stamp)-tags.png"),
            ("latest-analysis.png", "\(stamp)-analysis.png"),
            ("latest-tags.txt", "\(stamp)-tags.txt")
        ]
        for (inputName, outputName) in optionalFiles {
            let inputURL = analysisDir.appendingPathComponent(inputName)
            guard fm.fileExists(atPath: inputURL.path) else { continue }
            let outputURL = sampleDir.appendingPathComponent(outputName)
            try copyReplacing(from: inputURL, to: outputURL)
            savedURLs.append(outputURL)
        }

        let metadataURL = sampleDir.appendingPathComponent("\(stamp).txt")
        let cameraName = selectedCameraName
            ?? UserDefaults.standard.string(forKey: "SelectedCameraName")
            ?? "unknown"
        let metadata = [
            "created_at=\(ISO8601DateFormatter().string(from: Date()))",
            "mode=\(sampleMode.title)",
            "mode_source=\(sampleMode.source)",
            "camera=\(cameraName)",
            "frame=\(frameOutURL.lastPathComponent)"
        ].joined(separator: "\n") + "\n"
        try metadata.write(to: metadataURL, atomically: true, encoding: .utf8)
        savedURLs.append(metadataURL)

        return savedURLs
    }

    private func runBaselineCalibration() throws -> String {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: try postureWatcherBinaryPath())
        task.arguments = [
            "calibrate-baseline",
            "--samples-dir", try samplesURL().path,
            "--out", try baselineFileURL().path
        ]
        task.currentDirectoryURL = URL(fileURLWithPath: Bundle.main.resourcePath ?? NSHomeDirectory())

        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = pipe
        try task.run()
        task.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let text = String(data: data, encoding: .utf8) ?? ""
        guard task.terminationStatus == 0 else {
            throw AppError.message(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Could not calibrate baseline." : text)
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func refreshBaselineStatus() {
        guard let baselineStatusLabel else { return }
        do {
            let url = try baselineFileURL()
            guard FileManager.default.fileExists(atPath: url.path) else {
                baselineStatusLabel.stringValue = "not set"
                baselineStatusLabel.textColor = .secondaryLabelColor
                baselineStatusLabel.toolTip = "Save good Sitting and Standing samples, then click Calibrate."
                return
            }
            let values = try keyValueFile(url)
            let standing = values["mode.standing.status"] ?? "unknown"
            let sitting = values["mode.sitting.status"] ?? "unknown"
            let standingCount = values["mode.standing.accepted"] ?? "0"
            let sittingCount = values["mode.sitting.accepted"] ?? "0"

            if standing == "ready" && sitting == "ready" {
                baselineStatusLabel.stringValue = "ready"
                baselineStatusLabel.textColor = .systemGreen
            } else if standing == "ready" {
                baselineStatusLabel.stringValue = "need sitting"
                baselineStatusLabel.textColor = .systemOrange
            } else if sitting == "ready" {
                baselineStatusLabel.stringValue = "need standing"
                baselineStatusLabel.textColor = .systemOrange
            } else {
                baselineStatusLabel.stringValue = "needs samples"
                baselineStatusLabel.textColor = .systemOrange
            }
            baselineStatusLabel.toolTip = "standing \(standingCount), sitting \(sittingCount)"
        } catch {
            baselineStatusLabel.stringValue = "error"
            baselineStatusLabel.textColor = .systemRed
            baselineStatusLabel.toolTip = error.localizedDescription
        }
    }

    private func refreshSampleStatus() {
        guard let sampleStatusLabel else { return }
        do {
            let samples = try samplesURL()
            let sitting = try goodSampleCount(in: samples.appendingPathComponent("sitting", isDirectory: true))
            let standing = try goodSampleCount(in: samples.appendingPathComponent("standing", isDirectory: true))
            let auto = try goodSampleCount(in: samples.appendingPathComponent("auto", isDirectory: true))
            sampleStatusLabel.stringValue = "\(sitting)/3 sit, \(standing)/3 stand"
            sampleStatusLabel.textColor = sitting >= 3 && standing >= 3 ? .systemGreen : .systemOrange
            var tip = "Good calibration samples: sitting \(sitting), standing \(standing)."
            if auto > 0 {
                tip += " Auto-folder good samples are ignored by calibration: \(auto)."
            }
            sampleStatusLabel.toolTip = tip
        } catch {
            sampleStatusLabel.stringValue = "unavailable"
            sampleStatusLabel.textColor = .systemRed
            sampleStatusLabel.toolTip = error.localizedDescription
        }
    }

    private func keyValueFile(_ url: URL) throws -> [String: String] {
        let text = try String(contentsOf: url, encoding: .utf8)
        var values: [String: String] = [:]
        for line in text.split(separator: "\n") {
            let parts = line.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard parts.count == 2 else { continue }
            values[String(parts[0]).trimmingCharacters(in: .whitespacesAndNewlines)] = String(parts[1]).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return values
    }

    private func goodSampleCount(in dir: URL) throws -> Int {
        let fm = FileManager.default
        guard fm.fileExists(atPath: dir.path) else { return 0 }
        let urls = try fm.contentsOfDirectory(
            at: dir,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        )
        var count = 0
        for url in urls where url.lastPathComponent.hasSuffix("-tags.txt") {
            let values = try keyValueFile(url)
            if values["placement_status"] == "good" {
                count += 1
            }
        }
        return count
    }

    private func samplesURL() throws -> URL {
        try appSupportURL().appendingPathComponent("samples", isDirectory: true)
    }

    private func baselineFileURL() throws -> URL {
        let dir = try appSupportURL().appendingPathComponent("calibration", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("baseline.txt")
    }

    private func currentSampleMode() throws -> (title: String, folderName: String, source: String) {
        let title = modePopup?.selectedItem?.title
            ?? UserDefaults.standard.string(forKey: "PostureMode")
            ?? "Auto"
        if title == "Auto" {
            if latestDetectedMode == "sitting" {
                return ("Sitting", "sitting", "auto-detected")
            }
            if latestDetectedMode == "standing" {
                return ("Standing", "standing", "auto-detected")
            }
            return ("Auto", "auto", "auto-unknown")
        }
        let folderName = title
            .lowercased()
            .components(separatedBy: CharacterSet.alphanumerics.inverted)
            .filter { !$0.isEmpty }
            .joined(separator: "-")
        return (title, folderName.isEmpty ? "auto" : folderName, "manual")
    }

    private func sampleTimestamp() -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyyMMdd-HHmmss-SSS"
        return formatter.string(from: Date())
    }

    private func copyReplacing(from sourceURL: URL, to destinationURL: URL) throws {
        let fm = FileManager.default
        if fm.fileExists(atPath: destinationURL.path) {
            try fm.removeItem(at: destinationURL)
        }
        try fm.copyItem(at: sourceURL, to: destinationURL)
    }

    private func postureWatcherBinaryPath() throws -> String {
        guard let binaryPath = Bundle.main.path(forResource: "posture-watcher", ofType: nil) else {
            throw AppError.message("Missing posture-watcher inside the app bundle.")
        }
        return binaryPath
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
        if line.hasPrefix("DISPLAY,") {
            DispatchQueue.main.async {
                self.previewView.applyDisplayPayload(line)
            }
            return
        }
        if line.hasPrefix("BADGER,") {
            DispatchQueue.main.async {
                self.applyBadgerStatus(line)
            }
            return
        }
        if line.hasPrefix("TAGS,") {
            DispatchQueue.main.async {
                self.applyTagStatus(line)
            }
            return
        }
        if line.hasPrefix("MODE,") {
            DispatchQueue.main.async {
                self.applyModeStatus(line)
            }
            return
        }
        if line.hasPrefix("PLACEMENT,") {
            DispatchQueue.main.async {
                self.applyPlacementStatus(line)
            }
        }
    }

    private func applyBadgerStatus(_ line: String) {
        let parts = line.split(separator: ",", omittingEmptySubsequences: false).map(String.init)
        guard parts.count >= 2 else { return }
        let status = parts[1]
        switch status {
        case "connected":
            badgerStatusLabel?.stringValue = "connected"
            badgerStatusLabel?.textColor = .systemGreen
        case "disconnected":
            badgerStatusLabel?.stringValue = "not connected"
            badgerStatusLabel?.textColor = .systemRed
        case "disabled":
            badgerStatusLabel?.stringValue = "disabled"
            badgerStatusLabel?.textColor = .secondaryLabelColor
        case "checking":
            badgerStatusLabel?.stringValue = "checking"
            badgerStatusLabel?.textColor = .systemOrange
        default:
            badgerStatusLabel?.stringValue = status
            badgerStatusLabel?.textColor = .secondaryLabelColor
        }
    }

    private func applyModeStatus(_ line: String) {
        let parts = line.split(separator: ",", omittingEmptySubsequences: false).map(String.init)
        guard parts.count >= 3 else { return }
        let mode = parts[1]
        let confidence = parts[2]
        let detail = parts.dropFirst(3).joined(separator: ",")

        switch mode {
        case "sitting":
            latestDetectedMode = "sitting"
            autoModeLabel?.stringValue = "Sitting \(confidence)%"
            autoModeLabel?.textColor = .systemGreen
        case "standing":
            latestDetectedMode = "standing"
            autoModeLabel?.stringValue = "Standing \(confidence)%"
            autoModeLabel?.textColor = .systemGreen
        case "unknown":
            latestDetectedMode = nil
            autoModeLabel?.stringValue = "unknown"
            autoModeLabel?.textColor = .systemOrange
        default:
            latestDetectedMode = nil
            autoModeLabel?.stringValue = mode
            autoModeLabel?.textColor = .secondaryLabelColor
        }
        autoModeLabel?.toolTip = detail.isEmpty ? nil : detail
    }

    private func applyPlacementStatus(_ line: String) {
        let parts = line.split(separator: ",", omittingEmptySubsequences: false).map(String.init)
        guard parts.count >= 3 else { return }
        let status = parts[1]
        let score = parts[2]
        let action = parts.count >= 4 ? parts[3] : ""
        let detail = parts.dropFirst(4).joined(separator: ",")

        switch status {
        case "good":
            placementStatusLabel?.stringValue = "good \(score)%"
            placementStatusLabel?.textColor = .systemGreen
        case "check":
            placementStatusLabel?.stringValue = action.isEmpty ? "check \(score)%" : action
            placementStatusLabel?.textColor = .systemOrange
        case "missing":
            placementStatusLabel?.stringValue = action.isEmpty ? "missing" : action
            placementStatusLabel?.textColor = .systemOrange
        default:
            placementStatusLabel?.stringValue = status
            placementStatusLabel?.textColor = .secondaryLabelColor
        }
        placementStatusLabel?.toolTip = detail.isEmpty ? nil : detail
    }

    private func applyTagStatus(_ line: String) {
        let parts = line.split(separator: ",", omittingEmptySubsequences: false).map(String.init)
        guard parts.count >= 4 else { return }
        let status = parts[1]
        let present = parts[2]
        let missing = parts[3]
        switch status {
        case "ready":
            tagStatusLabel?.stringValue = present.isEmpty ? "ready" : "ready: \(present)"
            tagStatusLabel?.textColor = .systemGreen
        case "missing":
            if present.isEmpty {
                tagStatusLabel?.stringValue = "none seen"
            } else {
                tagStatusLabel?.stringValue = "missing \(missing)"
            }
            tagStatusLabel?.textColor = .systemOrange
        default:
            tagStatusLabel?.stringValue = status
            tagStatusLabel?.textColor = .secondaryLabelColor
        }
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        let now = Date()
        let shouldWriteLatest = now.timeIntervalSince(lastWrite) >= intervalSeconds
        let shouldWriteBurst = now.timeIntervalSince(lastBurstWrite) >= burstWriteIntervalSeconds
        guard shouldWriteLatest || shouldWriteBurst else { return }
        if shouldWriteLatest {
            lastWrite = now
        }
        if shouldWriteBurst {
            lastBurstWrite = now
        }

        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let ciImage = CIImage(cvPixelBuffer: pixelBuffer)
        guard let cgImage = ciContext.createCGImage(ciImage, from: ciImage.extent) else { return }
        let rep = NSBitmapImageRep(cgImage: cgImage)
        guard let data = rep.representation(using: .jpeg, properties: [.compressionFactor: 0.85]) else { return }

        do {
            if shouldWriteBurst {
                let index = burstFrameIndex % max(1, burstFrameCount)
                burstFrameIndex = (burstFrameIndex + 1) % max(1, burstFrameCount)
                let burstURL = burstDirURL.appendingPathComponent(String(format: "burst-%02d.jpg", index))
                try writeJPEGData(data, to: burstURL)
            }
            if shouldWriteLatest {
                try writeJPEGData(data, to: frameURL)
                log("wrote frame: \(frameURL.path)")
            }
        } catch {
            let message = "Posture Watcher frame write failed: \(error.localizedDescription)"
            log(message)
            fputs(message + "\n", stderr)
        }
    }

    private func writeJPEGData(_ data: Data, to url: URL) throws {
        let tmpURL = url.appendingPathExtension("tmp")
        try data.write(to: tmpURL, options: .atomic)
        if FileManager.default.fileExists(atPath: url.path) {
            _ = try FileManager.default.replaceItemAt(url, withItemAt: tmpURL)
        } else {
            try FileManager.default.moveItem(at: tmpURL, to: url)
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
