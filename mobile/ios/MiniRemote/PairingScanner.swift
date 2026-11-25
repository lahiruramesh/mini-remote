import AVFoundation
import SwiftUI
import UIKit

struct DesktopInvitation: Decodable, Identifiable {
    private let kind: String
    private let version: Int
    let host: String
    let port: UInt16
    let name: String
    let code: String
    private let expiresAt: TimeInterval

    var id: String { code }
    var isExpired: Bool { expiresAt <= Date().timeIntervalSince1970 }

    enum CodingKeys: String, CodingKey {
        case kind, version, host, port, name, code
        case expiresAt = "expires_at"
    }

    static func parse(_ raw: String) -> DesktopInvitation? {
        guard raw.utf8.count <= 1024,
              let invitation = try? JSONDecoder().decode(Self.self, from: Data(raw.utf8)),
              invitation.kind == "miniremote_pairing", invitation.version == 1,
              SessionTransport.isValidHost(invitation.host), invitation.port != 0,
              (1...64).contains(invitation.name.count),
              !invitation.name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
              validHex(invitation.code, length: 32) else { return nil }
        return invitation
    }

    static func validHex(_ value: String, length: Int) -> Bool {
        value.utf8.count == length && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
}

struct PairingScanner: View {
    let onPair: (DesktopInvitation, String) throws -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var invitation: DesktopInvitation?
    @State private var askingCamera = true
    @State private var cameraAvailable = false
    @State private var isPairing = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                if let invitation {
                    confirmation(invitation)
                } else if askingCamera {
                    ProgressView("Opening camera")
                } else if cameraAvailable {
                    CameraPreview(readCode: scan, unavailable: { cameraAvailable = false })
                        .clipShape(.rect(cornerRadius: 20))
                        .frame(maxHeight: 360)
                        .overlay {
                            RoundedRectangle(cornerRadius: 16).strokeBorder(.white.opacity(0.85), lineWidth: 2)
                                .frame(width: 186, height: 186)
                                .allowsHitTesting(false)
                        }
                        .accessibilityLabel("Pairing code scanner")
                    Text("On the desktop, open Mini Remote Viewer → Setup → Pair an iPhone or iPad.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                } else {
                    ContentUnavailableView("Camera unavailable", systemImage: "camera", description: Text("Allow camera access in Settings or use Add device to pair manually."))
                    Button("Open Settings", systemImage: "gear") {
                        if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                    }
                }
                if let error {
                    Label(error, systemImage: "exclamationmark.circle")
                        .foregroundStyle(.red)
                        .multilineTextAlignment(.center)
                        .font(.footnote)
                        .accessibilityAddTraits(.updatesFrequently)
                }
            }
            .padding(22)
            .frame(maxWidth: 500)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .navigationTitle("Scan pairing code")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }.disabled(isPairing)
                }
            }
        }
        .interactiveDismissDisabled(isPairing)
        .task {
            switch AVCaptureDevice.authorizationStatus(for: .video) {
            case .authorized: cameraAvailable = true
            case .notDetermined: cameraAvailable = await AVCaptureDevice.requestAccess(for: .video)
            default: cameraAvailable = false
            }
            askingCamera = false
        }
    }

    private func confirmation(_ invitation: DesktopInvitation) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "desktopcomputer").font(.system(size: 40)).foregroundStyle(.tint)
            Text(invitation.name).font(.title2.weight(.semibold))
            Text("\(invitation.host):\(invitation.port)").font(.subheadline.monospaced()).foregroundStyle(.secondary)
            Text("Pair this desktop and save its credentials in Keychain on this device.")
                .font(.subheadline).multilineTextAlignment(.center).foregroundStyle(.secondary)
            Button {
                Task { await pair(invitation) }
            } label: {
                Group {
                    if isPairing { ProgressView().tint(.white) }
                    else { Text("Pair & Connect").fontWeight(.semibold) }
                }
                .frame(maxWidth: .infinity, minHeight: 28)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(isPairing)
            Button("Scan Another Code", systemImage: "qrcode.viewfinder") {
                self.invitation = nil
                error = nil
            }
            .disabled(isPairing)
        }
    }

    private func scan(_ value: String) {
        guard invitation == nil else { return }
        guard let scanned = DesktopInvitation.parse(value) else {
            error = "Scan a pairing code shown by Mini Remote Viewer."
            return
        }
        error = scanned.isExpired ? "This code expired. Show a new code on the desktop." : nil
        if error == nil { invitation = scanned }
    }

    @MainActor
    private func pair(_ invitation: DesktopInvitation) async {
        guard !isPairing else { return }
        guard !invitation.isExpired else {
            error = "This code expired. Show a new code on the desktop."
            return
        }
        isPairing = true
        error = nil
        defer { isPairing = false }
        do {
            let token = try await Task.detached(priority: .userInitiated) {
                try SessionTransport.redeem(invitation)
            }.value
            try onPair(invitation, token)
        } catch {
            self.error = error.localizedDescription
        }
    }
}

private struct CameraPreview: UIViewRepresentable {
    let readCode: (String) -> Void
    let unavailable: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(readCode: readCode, unavailable: unavailable) }

    func makeUIView(context: Context) -> Preview {
        let view = Preview()
        view.cameraLayer.session = context.coordinator.session
        view.cameraLayer.videoGravity = .resizeAspectFill
        context.coordinator.start()
        return view
    }

    func updateUIView(_ view: Preview, context: Context) {}

    static func dismantleUIView(_ uiView: Preview, coordinator: Coordinator) {
        coordinator.stop()
    }

    final class Preview: UIView {
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        var cameraLayer: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }
    }

    final class Coordinator: NSObject, AVCaptureMetadataOutputObjectsDelegate, @unchecked Sendable {
        let session = AVCaptureSession()
        private let queue = DispatchQueue(label: "miniremote.camera")
        private let readCode: (String) -> Void
        private let unavailable: () -> Void
        private var lastCode: String?

        init(readCode: @escaping (String) -> Void, unavailable: @escaping () -> Void) {
            self.readCode = readCode
            self.unavailable = unavailable
        }

        func start() {
            queue.async { [self] in
                guard let device = AVCaptureDevice.default(for: .video),
                      let input = try? AVCaptureDeviceInput(device: device),
                      session.canAddInput(input) else {
                    DispatchQueue.main.async(execute: unavailable)
                    return
                }
                let output = AVCaptureMetadataOutput()
                guard session.canAddOutput(output) else {
                    DispatchQueue.main.async(execute: unavailable)
                    return
                }
                session.beginConfiguration()
                session.addInput(input)
                session.addOutput(output)
                guard output.availableMetadataObjectTypes.contains(.qr) else {
                    session.commitConfiguration()
                    DispatchQueue.main.async(execute: unavailable)
                    return
                }
                output.setMetadataObjectsDelegate(self, queue: .main)
                output.metadataObjectTypes = [.qr]
                session.commitConfiguration()
                session.startRunning()
            }
        }

        func stop() {
            queue.async { [self] in session.stopRunning() }
        }

        func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput metadataObjects: [AVMetadataObject],
                            from connection: AVCaptureConnection) {
            guard let code = metadataObjects.compactMap({ ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }).first,
                  code != lastCode else { return }
            lastCode = code
            readCode(code)
        }
    }
}
