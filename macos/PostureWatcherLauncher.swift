import AVFoundation
import AppKit
import CoreImage
import Foundation

@main
final class PostureWatcherLauncher: NSObject, NSApplicationDelegate, AVCaptureVideoDataOutputSampleBufferDelegate {
    private var process: Process?
    private let session = AVCaptureSession()
    private let captureQueue = DispatchQueue(label: "local.posture-watcher.capture")
    private let ciContext = CIContext()
    private var lastWrite = Date.distantPast
    private var frameURL: URL!
    private var intervalSeconds = 30.0

    func applicationDidFinishLaunching(_ notification: Notification) {
        requestCameraThenRun()
    }

    func applicationWillTerminate(_ notification: Notification) {
        session.stopRunning()
        process?.terminate()
    }

    private func requestCameraThenRun() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            startCaptureAndAnalyzer()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async {
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
            intervalSeconds = Double(ProcessInfo.processInfo.environment["POSTURE_WATCHER_INTERVAL_SECS"] ?? "30") ?? 30.0
            try configureCapture()
            runPostureWatcher(inputURL: frameURL, supportURL: supportURL)
            session.startRunning()
        } catch {
            showMessage(error.localizedDescription)
            NSApp.terminate(nil)
        }
    }

    private func configureCapture() throws {
        session.beginConfiguration()
        session.sessionPreset = .hd1280x720

        guard let device = selectedCamera() else {
            throw AppError.message("Could not find the requested camera.")
        }
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
        let requested = ProcessInfo.processInfo.environment["POSTURE_WATCHER_CAMERA"] ?? "Logitech Webcam C930e"
        let discovery = AVCaptureDevice.DiscoverySession(
            deviceTypes: [.external, .builtInWideAngleCamera, .continuityCamera],
            mediaType: .video,
            position: .unspecified
        )
        return discovery.devices.first { $0.localizedName == requested }
            ?? discovery.devices.first { $0.localizedName.contains(requested) }
            ?? AVCaptureDevice.default(for: .video)
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
        let interval = env["POSTURE_WATCHER_INTERVAL_SECS"] ?? "30"
        let outDir = supportURL.appendingPathComponent("analysis").path

        let task = Process()
        task.executableURL = URL(fileURLWithPath: binaryPath)
        task.arguments = [
            "live-file",
            "--input", inputURL.path,
            "--port", port,
            "--window-secs", window,
            "--interval-secs", interval,
            "--out-dir", outDir
        ]
        task.currentDirectoryURL = URL(fileURLWithPath: bundle.resourcePath ?? NSHomeDirectory())

        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = pipe

        pipe.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return }
            fputs(text, stderr)
        }

        task.terminationHandler = { _ in
            DispatchQueue.main.async {
                pipe.fileHandleForReading.readabilityHandler = nil
                NSApp.terminate(nil)
            }
        }

        do {
            try task.run()
            process = task
        } catch {
            showMessage("Could not start posture-watcher: \(error.localizedDescription)")
            NSApp.terminate(nil)
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
        } catch {
            fputs("Posture Watcher frame write failed: \(error.localizedDescription)\n", stderr)
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
        let alert = NSAlert()
        alert.messageText = "Posture Watcher"
        alert.informativeText = text
        alert.alertStyle = .warning
        alert.runModal()
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
