import SwiftUI
import UIKit

private let deviceAccent = Color(uiColor: UIColor { traits in
    traits.userInterfaceStyle == .dark ? .systemMint : UIColor(red: 0.02, green: 0.36, blue: 0.31, alpha: 1)
})

struct ContentView: View {
    @StateObject private var store = DeviceStore()
    @StateObject private var session = RemoteSession()
    @State private var editor: EditorRoute?
    @State private var scanning = false
    @State private var showingPairingHelp = false
    @State private var navigationPath: [UUID] = []
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack(path: $navigationPath) {
            deviceLibrary
                .navigationTitle("Mini Remote")
                .navigationBarTitleDisplayMode(.large)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button { showingPairingHelp = true } label: {
                            Image(systemName: "questionmark.circle")
                                .font(.title3)
                                .foregroundStyle(.secondary)
                        }
                        .accessibilityLabel("Pairing help")
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        Button { presentEditor(nil) } label: {
                            Image(systemName: "plus")
                                .font(.headline.weight(.semibold))
                                .frame(width: 40, height: 40)
                                .background(.tint.opacity(0.12), in: Circle())
                        }
                        .accessibilityLabel("Add device")
                    }
                }
                .navigationDestination(for: UUID.self) { deviceID in
                    if let device = store.devices.first(where: { $0.id == deviceID }) {
                        RemoteDesktopScreen(session: session, device: device) {
                            session.disconnect()
                            navigationPath.removeAll()
                        }
                        .toolbar(.hidden, for: .tabBar)
                    } else {
                        ContentUnavailableView("Device removed", systemImage: "desktopcomputer.trianglebadge.exclamationmark")
                    }
                }
                .sheet(isPresented: $scanning) {
                    PairingScanner { invitation, token in
                        let device = store.devices.first { $0.host == invitation.host && $0.port == invitation.port }
                            ?? SavedDevice(name: invitation.name, host: invitation.host, port: invitation.port)
                        try store.save(device, token: token)
                        scanning = false
                        session.connect(device, token: token)
                        navigationPath.append(device.id)
                    }
                }
                .sheet(item: $editor) { route in
                    DeviceEditorSheet(
                        store: store,
                        device: route.device,
                        purpose: route.purpose,
                        onCancel: { editor = nil },
                        onConnect: { device, token, clipboard in
                            editor = nil
                            session.connect(device, token: token, shareClipboard: clipboard)
                            navigationPath.append(device.id)
                        },
                        onSaved: { editor = nil }
                    )
                    .presentationDetents(route.purpose == .connect ? [.medium, .large] : [.large])
                    .presentationDragIndicator(.visible)
                }
                .sheet(isPresented: $showingPairingHelp) {
                    PairingGuideSheet(onScan: {
                        showingPairingHelp = false
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { scanning = true }
                    }, onManual: {
                        showingPairingHelp = false
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { presentEditor(nil) }
                    })
                    .presentationDetents([.medium, .large])
                    .presentationDragIndicator(.visible)
                }
                .alert("Couldn’t save device", isPresented: Binding(
                    get: { errorMessage != nil },
                    set: { if !$0 { errorMessage = nil } }
                )) {
                    Button("OK", role: .cancel) { errorMessage = nil }
                } message: {
                    Text(errorMessage ?? "Please try again.")
                }
        }
        .tint(deviceAccent)
    }

    private var deviceLibrary: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                libraryHeader
                if store.devices.isEmpty {
                    emptyState
                } else {
                    scanButton
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Text("YOUR DEVICES")
                                .font(.caption.weight(.bold))
                                .tracking(1.1)
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text("\(store.devices.count)")
                                .font(.caption.weight(.semibold).monospacedDigit())
                                .foregroundStyle(.secondary)
                        }

                        ForEach(store.devices) { device in
                            DeviceCard(
                                device: device,
                                hasToken: store.token(for: device) != nil,
                                onConnect: { connect(device) },
                                onEdit: { presentEditor(device) },
                                onDelete: { store.remove(device) }
                            )
                        }
                    }
                }

                tailscaleTip
            }
            .padding(.horizontal, 20)
            .padding(.top, 10)
            .padding(.bottom, 32)
            .frame(maxWidth: 680)
            .frame(maxWidth: .infinity)
        }
        .background(Color(uiColor: .systemGroupedBackground))
    }

    private var scanButton: some View {
        Button {
            scanning = true
        } label: {
            Label("Scan pairing code", systemImage: "qrcode.viewfinder")
                .font(.body.weight(.semibold))
                .frame(maxWidth: .infinity, minHeight: 28)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
    }

    private var libraryHeader: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                ZStack {
                    RoundedRectangle(cornerRadius: 17, style: .continuous)
                        .fill(LinearGradient(colors: [.mint, .cyan], startPoint: .topLeading, endPoint: .bottomTrailing))
                    Image(systemName: "rectangle.connected.to.line.below")
                        .font(.system(size: 23, weight: .semibold))
                        .foregroundStyle(Color(white: 0.09))
                }
                .frame(width: 54, height: 54)

                VStack(alignment: .leading, spacing: 3) {
                    Text("Your desktops")
                        .font(.title2.weight(.bold))
                        .fixedSize(horizontal: false, vertical: true)
                    Text("Private sessions over your tailnet")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }
        }
        .padding(.vertical, 4)
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 17) {
            HStack(spacing: 13) {
                Image(systemName: "sparkles.rectangle.stack")
                    .font(.system(size: 22, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 50, height: 50)
                    .background(LinearGradient(colors: [.mint, .teal], startPoint: .topLeading, endPoint: .bottomTrailing), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                VStack(alignment: .leading, spacing: 3) {
                    Text("Set up your first desktop")
                        .font(.headline)
                    Text("A private connection in three steps")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }

            VStack(alignment: .leading, spacing: 15) {
                OnboardingStep(number: "1", symbol: "lock.shield", title: "Join your tailnet", detail: "Install Tailscale on this iPhone and the desktop, then sign in to the same tailnet.")
                OnboardingStep(number: "2", symbol: "desktopcomputer", title: "Show a pairing code", detail: "On the desktop, open Mini Remote Viewer → Setup → Pair an iPhone or iPad.")
                OnboardingStep(number: "3", symbol: "qrcode.viewfinder", title: "Scan and connect", detail: "Scan the one-time code, confirm the desktop name, and tap Pair & Connect.")
            }

            scanButton

            HStack(spacing: 5) {
                Text("Prefer manual setup?")
                    .foregroundStyle(.secondary)
                Button("Enter address and token") { presentEditor(nil) }
                    .fontWeight(.semibold)
            }
            .font(.footnote)
            .frame(maxWidth: .infinity)
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 26, style: .continuous))
    }

    private var tailscaleTip: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "lock.shield.fill")
                .font(.title3)
                .foregroundStyle(.tint)
                .frame(width: 34, height: 34)
                .background(.tint.opacity(0.1), in: RoundedRectangle(cornerRadius: 11, style: .continuous))
            VStack(alignment: .leading, spacing: 4) {
                Text("Connect privately")
                    .font(.subheadline.weight(.semibold))
                Text("Keep Tailscale on for both devices. Pair by QR or use the desktop’s Tailscale IPv4 address.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 4)
            Button { showingPairingHelp = true } label: {
                Image(systemName: "arrow.up.right.circle")
                    .font(.title3)
            }
            .accessibilityLabel("Open pairing setup guide")
        }
        .padding(15)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 20, style: .continuous))
    }

    private func presentEditor(_ device: SavedDevice?) {
        editor = EditorRoute(device: device ?? SavedDevice(), purpose: device == nil ? .add : .edit)
    }

    private func connect(_ device: SavedDevice) {
        if let token = store.token(for: device), !token.isEmpty {
            session.connect(device, token: token)
            navigationPath.append(device.id)
        } else {
            editor = EditorRoute(device: device, purpose: .connect)
        }
    }
}

private struct OnboardingStep: View {
    let number: String
    let symbol: String
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            Text(number)
                .font(.caption.weight(.bold).monospacedDigit())
                .foregroundStyle(.tint)
                .frame(width: 25, height: 25)
                .background(.tint.opacity(0.12), in: Circle())
            Image(systemName: symbol)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.tint)
                .frame(width: 21)
                .padding(.top, 4)
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                Text(detail)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

private struct PairingGuideSheet: View {
    let onScan: () -> Void
    let onManual: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 19) {
                    HStack(spacing: 13) {
                        Image(systemName: "lock.shield.fill")
                            .font(.title2)
                            .foregroundStyle(.white)
                            .frame(width: 48, height: 48)
                            .background(LinearGradient(colors: [.mint, .teal], startPoint: .topLeading, endPoint: .bottomTrailing), in: RoundedRectangle(cornerRadius: 15, style: .continuous))
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Pair over Tailscale")
                                .font(.headline)
                            Text("Your devices stay on your private tailnet")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                    }

                    VStack(alignment: .leading, spacing: 16) {
                        OnboardingStep(number: "1", symbol: "iphone.gen3", title: "Connect both devices to Tailscale", detail: "Sign in to the same tailnet on this iPhone and the desktop. Leave Tailscale enabled while connecting.")
                        OnboardingStep(number: "2", symbol: "macwindow", title: "Open desktop setup", detail: "In Mini Remote Viewer, choose Setup → Pair an iPhone or iPad, then show a fresh pairing code.")
                        OnboardingStep(number: "3", symbol: "qrcode.viewfinder", title: "Scan the one-time code", detail: "Check the desktop name on the confirmation screen, then tap Pair & Connect. Codes expire; create a new one if needed.")
                    }

                    Button(action: onScan) {
                        Label("Scan pairing code", systemImage: "qrcode.viewfinder")
                            .font(.body.weight(.semibold))
                            .frame(maxWidth: .infinity, minHeight: 28)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)

                    Button("Set up manually instead", action: onManual)
                        .font(.subheadline.weight(.semibold))
                        .frame(maxWidth: .infinity)
                }
                .padding(22)
                .frame(maxWidth: 560)
                .frame(maxWidth: .infinity)
            }
            .navigationTitle("Quick setup")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}

private struct EditorRoute: Identifiable {
    enum Purpose {
        case add, edit, connect
    }

    let id = UUID()
    let device: SavedDevice
    let purpose: Purpose
}

private struct DeviceCard: View {
    let device: SavedDevice
    let hasToken: Bool
    let onConnect: () -> Void
    let onEdit: () -> Void
    let onDelete: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 15) {
            HStack(alignment: .top, spacing: 13) {
                Image(systemName: "desktopcomputer")
                    .font(.system(size: 20, weight: .medium))
                    .foregroundStyle(.tint)
                    .frame(width: 46, height: 46)
                    .background(.tint.opacity(0.1), in: RoundedRectangle(cornerRadius: 14, style: .continuous))

                VStack(alignment: .leading, spacing: 4) {
                    Text(device.name.isEmpty ? "Desktop" : device.name)
                        .font(.headline)
                        .lineLimit(1)
                    Text(device.host)
                        .font(.subheadline.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
                Menu {
                    Button("Edit device", systemImage: "pencil", action: onEdit)
                    Button("Delete device", systemImage: "trash", role: .destructive, action: onDelete)
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.headline.weight(.bold))
                        .foregroundStyle(.secondary)
                        .frame(width: 34, height: 34)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("Device options")
            }

            HStack(spacing: 8) {
                Label(device.quality.title, systemImage: "sparkles.tv")
                Text("·")
                Text("\(device.port)")
                Spacer()
                if hasToken {
                    Label("Token saved", systemImage: "key.fill")
                        .labelStyle(.titleAndIcon)
                        .font(.caption2.weight(.medium))
                        .foregroundStyle(.secondary)
                }
            }
            .font(.caption.weight(.medium))
            .foregroundStyle(.secondary)

            Button(action: onConnect) {
                HStack {
                    Image(systemName: "arrow.up.right.video.fill")
                    Text("Connect")
                    Spacer()
                    Image(systemName: "arrow.right")
                        .font(.caption.weight(.bold))
                }
                .font(.subheadline.weight(.semibold))
                .padding(.horizontal, 15)
                .padding(.vertical, 12)
                .foregroundStyle(Color(white: 0.09))
                .background(Color.mint, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            }
            .buttonStyle(.plain)
        }
        .padding(16)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(.primary.opacity(0.035)))
    }
}

private struct DeviceEditorSheet: View {
    @ObservedObject var store: DeviceStore
    let purpose: EditorRoute.Purpose
    let onCancel: () -> Void
    let onConnect: (SavedDevice, String, Bool) -> Void
    let onSaved: () -> Void

    @State private var device: SavedDevice
    @State private var portText: String
    @State private var token = ""
    @State private var saveToken: Bool
    @State private var shareClipboard = false
    @State private var localError: String?

    private let hadSavedToken: Bool

    init(store: DeviceStore, device: SavedDevice, purpose: EditorRoute.Purpose, onCancel: @escaping () -> Void, onConnect: @escaping (SavedDevice, String, Bool) -> Void, onSaved: @escaping () -> Void) {
        self.store = store
        self.purpose = purpose
        _device = State(initialValue: device)
        _portText = State(initialValue: String(device.port))
        self.onCancel = onCancel
        self.onConnect = onConnect
        self.onSaved = onSaved
        let hasToken = store.token(for: device) != nil
        self.hadSavedToken = hasToken
        _saveToken = State(initialValue: hasToken)
    }

    var body: some View {
        NavigationStack {
            Form {
                if purpose != .connect {
                    deviceSettings
                }

                Section {
                    SecureField("Pairing token", text: $token)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Toggle("Save token in Keychain", isOn: $saveToken)
                    if saveToken {
                        Label("Optional. Turn this off to connect once without saving.", systemImage: "lock.fill")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                } header: {
                    Text("Pairing")
                } footer: {
                    Text(hadSavedToken ? "Leave the token blank to use the saved one." : "Get the pairing token from Mini Remote Connector on your desktop.")
                }

                Section {
                    Toggle("Share clipboard for this session", isOn: $shareClipboard)
                } footer: {
                    Text("Clipboard access starts only after you enable it here.")
                }

            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: onCancel)
                }
                if purpose != .connect {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save only") { submit(connect: false) }
                    }
                }
            }
            .safeAreaInset(edge: .bottom) {
                VStack(spacing: 8) {
                    if let localError {
                        Label(localError, systemImage: "exclamationmark.circle.fill")
                            .foregroundStyle(.red)
                            .font(.footnote)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    Button {
                        submit(connect: true)
                    } label: {
                        Label(purpose == .connect ? "Connect" : "Save & Connect", systemImage: "desktopcomputer")
                            .font(.body.weight(.semibold))
                            .frame(maxWidth: .infinity, minHeight: 28)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 10)
                .background(.regularMaterial)
            }
        }
    }

    private var title: String {
        switch purpose {
        case .add: "Add device"
        case .edit: "Edit device"
        case .connect: "Connect to \(device.name)"
        }
    }

    @ViewBuilder
    private var deviceSettings: some View {
        Section {
            TextField("Desktop name", text: $device.name)
                .textInputAutocapitalization(.words)
            TextField("100.90.12.34", text: $device.host)
                .keyboardType(.numbersAndPunctuation)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            HStack {
                Text("Connector port")
                Spacer()
                TextField("47990", text: $portText)
                    .keyboardType(.numberPad)
                    .multilineTextAlignment(.trailing)
                    .frame(width: 90)
            }
        } header: {
            Text("Device")
        } footer: {
            Text("Use your desktop’s Tailscale IPv4 address. Keep Tailscale enabled on both devices.")
        }

        Section("Video quality") {
            Picker("Quality", selection: $device.quality) {
                ForEach(MobileQuality.allCases) { quality in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(quality.title)
                        Text(quality.detail).font(.caption).foregroundStyle(.secondary)
                    }
                    .tag(quality)
                }
            }
            .pickerStyle(.navigationLink)
        }
    }

    private func submit(connect: Bool) {
        let host = device.host.trimmingCharacters(in: .whitespacesAndNewlines)
        let name = device.name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard SessionTransport.isValidHost(host) else { localError = TransportError.invalidAddress.localizedDescription; return }
        guard let port = UInt16(portText), port > 0 else { localError = "Enter a port between 1 and 65535."; return }
        device.host = host
        device.port = port
        device.name = name.isEmpty ? "Desktop" : name
        let enteredToken = token.trimmingCharacters(in: .whitespacesAndNewlines)
        let connectToken = !enteredToken.isEmpty ? enteredToken : store.token(for: device)
        if connect && (connectToken?.isEmpty ?? true) {
            localError = "Enter a pairing token to connect."
            return
        }

        let tokenUpdate: String?
        if saveToken {
            tokenUpdate = enteredToken.isEmpty ? nil : enteredToken
        } else if hadSavedToken {
            tokenUpdate = ""
        } else {
            tokenUpdate = nil
        }

        do {
            try store.save(device, token: tokenUpdate)
            if connect, let connectToken {
                onConnect(device, connectToken, shareClipboard)
            } else {
                onSaved()
            }
        } catch {
            localError = error.localizedDescription
        }
    }
}

private struct RemoteDesktopScreen: View {
    @ObservedObject var session: RemoteSession
    let device: SavedDevice
    let onDisconnect: () -> Void

    @State private var inputMode: RemoteInputMode = .direct
    @Environment(\.verticalSizeClass) private var verticalSizeClass
    @State private var keyboardVisible = false
    @State private var textToSend = ""
    @State private var typingError: String?
    @State private var clipboardCopied = false
    @State private var zoom: CGFloat = 1
    @State private var viewportOffset: CGSize = .zero
    @State private var canvasSize: CGSize = .zero
    @State private var trackpadCursor = CGPoint(x: 0.5, y: 0.5)
    @State private var pointerDown = false
    @State private var heldModifiers: Set<String> = []

    var body: some View {
        let compact = verticalSizeClass == .compact
        let layout = compact ? AnyLayout(HStackLayout(spacing: 0)) : AnyLayout(VStackLayout(spacing: 0))
        return layout {
            if !compact { sessionHeader }
            screenSurface
            sessionControls(compact: compact)
        }
        .background(Color.black.ignoresSafeArea())
        .environment(\.colorScheme, .dark)
        .tint(.mint)
        .navigationBarBackButtonHidden(true)
        .toolbar(.hidden, for: .navigationBar)
        .statusBarHidden(true)
        .sheet(isPresented: $keyboardVisible, onDismiss: releaseHeldModifiers) {
            NavigationStack {
                ScrollView { keyboardPanel.padding(.vertical, 12) }
                    .navigationTitle("Remote keyboard")
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Done") { keyboardVisible = false }
                        }
                    }
            }
            .presentationDetents([.height(285), .large])
            .presentationDragIndicator(.visible)
        }
        .onChange(of: session.remoteClipboard) { _, _ in clipboardCopied = false }
        .onDisappear {
            endPointerDrag()
            releaseHeldModifiers()
        }
    }

    private var sessionHeader: some View {
        HStack(spacing: 11) {
            Button(action: onDisconnect) {
                Image(systemName: "chevron.left")
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 44, height: 44)
                    .background(.white.opacity(0.12), in: Circle())
            }
            .accessibilityLabel("Disconnect and return to devices")

            VStack(alignment: .leading, spacing: 2) {
                Text(device.name.isEmpty ? "Desktop" : device.name)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.white)
                    .lineLimit(1)
                HStack(spacing: 5) {
                    Circle().fill(phaseColor).frame(width: 6, height: 6)
                    Text(phaseText)
                        .font(.caption2)
                        .foregroundStyle(.white.opacity(0.66))
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            if !session.streamLabel.isEmpty {
                Text(session.streamLabel)
                    .font(.caption2.weight(.medium).monospaced())
                    .foregroundStyle(.white.opacity(0.82))
                    .lineLimit(1)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 6)
                    .background(.white.opacity(0.1), in: Capsule())
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Color(white: 0.08))
    }

    private var screenSurface: some View {
        GeometryReader { proxy in
            let viewport = viewport(in: proxy.size)
            ZStack {
                Color.black
                if session.phase == .streaming {
                    DesktopVideoView(video: session.video)
                        .frame(width: viewport.fitted.width, height: viewport.fitted.height)
                        .scaleEffect(zoom)
                        .offset(viewport.offset)
                        .accessibilityHidden(true)
                    RemotePointerSurface(
                        tap: { location in pointerTap(location, viewport: viewport) },
                        drag: { state, location, delta in
                            pointerDrag(state, location: location, delta: delta, viewport: viewport)
                        },
                        pinch: { scale, anchor in
                            endPointerDrag()
                            resize(zoom * scale, anchor: anchor, in: proxy.size)
                        },
                        twoFingerDrag: { delta in
                            endPointerDrag()
                            if zoom > 1 {
                                viewportOffset = viewport.clamp(
                                    CGSize(width: viewport.offset.width + delta.width,
                                           height: viewport.offset.height + delta.height)
                                )
                            } else if session.canControl {
                                session.scroll(x: -delta.width / 12, y: delta.height / 12)
                            }
                        }
                    )
                    if inputMode == .trackpad && session.canControl {
                        Circle()
                            .strokeBorder(.white, lineWidth: 1.5)
                            .background(Circle().fill(.black.opacity(0.45)))
                            .frame(width: 16, height: 16)
                            .position(viewport.location(of: trackpadCursor))
                            .allowsHitTesting(false)
                            .accessibilityHidden(true)
                    }
                } else {
                    phasePlaceholder
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .onChange(of: proxy.size, initial: true) { _, size in
                canvasSize = size
                viewportOffset = self.viewport(in: size).offset
            }
            .overlay(alignment: .top) {
                if session.phase == .streaming && !session.canControl {
                    Label("View only", systemImage: "eye")
                        .font(.caption.weight(.medium))
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .background(.ultraThinMaterial, in: Capsule())
                        .padding(.top, 10)
                        .allowsHitTesting(false)
                }
            }
        }
        .clipped()
    }

    private var phasePlaceholder: some View {
        VStack(spacing: 12) {
            if session.phase == .connecting {
                ProgressView().tint(.mint).scaleEffect(1.2)
                Text("Connecting securely…")
                    .foregroundStyle(.white.opacity(0.72))
                    .font(.subheadline)
            } else if case let .failed(message) = session.phase {
                Image(systemName: "wifi.exclamationmark")
                    .font(.system(size: 32, weight: .light))
                    .foregroundStyle(.orange)
                Text("Connection failed")
                    .font(.headline)
                    .foregroundStyle(.white)
                Text(message)
                    .font(.footnote)
                    .foregroundStyle(.white.opacity(0.68))
                    .multilineTextAlignment(.center)
                    .textSelection(.enabled)
            } else {
                Image(systemName: "desktopcomputer")
                    .font(.system(size: 32, weight: .light))
                    .foregroundStyle(.white.opacity(0.45))
                Text("Session ended")
                    .foregroundStyle(.white.opacity(0.72))
                    .font(.subheadline)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func sessionControls(compact: Bool) -> some View {
        let layout = compact ? AnyLayout(VStackLayout(spacing: 2)) : AnyLayout(HStackLayout(spacing: 4))
        return layout {
            if compact {
                HStack(spacing: 4) {
                    Circle().fill(phaseColor).frame(width: 5, height: 5)
                    Text(phaseText).font(.caption2.weight(.semibold)).lineLimit(1)
                }
                .foregroundStyle(.secondary)
                .padding(.bottom, 3)
                .accessibilityElement(children: .combine)
                Button(action: onDisconnect) {
                    controlLabel("Devices", symbol: "chevron.left")
                }
                .accessibilityLabel("Disconnect and return to devices")
            }
            Button { keyboardVisible = true } label: {
                controlLabel("Keyboard", symbol: "keyboard")
            }
            .disabled(!session.canControl)

            Menu {
                Button("Next App", systemImage: "arrow.right.rectangle") { session.shortcut("next_app") }
                Button("Previous App", systemImage: "arrow.left.rectangle") { session.shortcut("previous_app") }
                Button("Close Window", systemImage: "xmark.rectangle") { session.shortcut("close_window") }
            } label: {
                controlLabel("Apps", symbol: "square.stack")
            }
            .disabled(!session.canControl)

            Menu {
                Picker("Pointer mode", selection: $inputMode) {
                    ForEach(RemoteInputMode.allCases) { mode in
                        Label(mode.title, systemImage: mode.symbol).tag(mode)
                    }
                }
                Button("Right Click", systemImage: "cursorarrow.click.2") { click("right") }
                Button("Scroll Up", systemImage: "chevron.up") { session.scroll(x: 0, y: 3) }
                Button("Scroll Down", systemImage: "chevron.down") { session.scroll(x: 0, y: -3) }
            } label: {
                controlLabel("Pointer", symbol: inputMode.symbol)
            }
            .disabled(!session.canControl)

            Menu {
                Section("Pinch to zoom. Drag with two fingers to pan.") {
                    Button("Fit Desktop", systemImage: "arrow.down.right.and.arrow.up.left") {
                        zoom = 1
                        viewportOffset = .zero
                    }
                    Button("Fill Screen", systemImage: "arrow.up.left.and.arrow.down.right") {
                        let fitted = viewport(in: canvasSize).fitted
                        guard fitted.width > 0, fitted.height > 0 else { return }
                        resize(max(canvasSize.width / fitted.width, canvasSize.height / fitted.height),
                               anchor: CGPoint(x: canvasSize.width / 2, y: canvasSize.height / 2),
                               in: canvasSize)
                    }
                }
                Button("Zoom In", systemImage: "plus.magnifyingglass") { zoomFromCenter(by: 1.3) }
                    .disabled(zoom >= 8)
                Button("Zoom Out", systemImage: "minus.magnifyingglass") { zoomFromCenter(by: 1 / 1.3) }
                    .disabled(zoom <= 1)
            } label: {
                controlLabel("View", symbol: "viewfinder")
            }
            .disabled(session.phase != .streaming)
        }
        .buttonStyle(.plain)
        .padding(compact ? .vertical : .horizontal, 5)
        .frame(maxWidth: compact ? 66 : .infinity, maxHeight: compact ? .infinity : nil)
        .background(Color(white: 0.09))
    }

    private func controlLabel(_ title: String, symbol: String) -> some View {
        VStack(spacing: 4) {
            Image(systemName: symbol).font(.system(size: 17, weight: .medium))
            Text(title).font(.caption2.weight(.semibold)).lineLimit(1).minimumScaleFactor(0.8)
        }
        .frame(maxWidth: .infinity, minHeight: 44)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(title)
    }

    private var keyboardPanel: some View {
        VStack(spacing: 8) {
            HStack(spacing: 6) {
                keyButton("Esc", code: "Escape")
                keyButton("Tab", code: "Tab")
                modifierButton("Ctrl", code: "ControlLeft")
                modifierButton("Alt", code: "AltLeft")
                modifierButton("⇧", code: "ShiftLeft")
                modifierButton("⌘", code: "MetaLeft")
                keyButton("Del", code: "Backspace")
                keyButton("Enter", code: "Enter")
            }
            HStack(spacing: 6) {
                keyButton("←", code: "ArrowLeft")
                keyButton("↑", code: "ArrowUp")
                keyButton("↓", code: "ArrowDown")
                keyButton("→", code: "ArrowRight")
                keyButton("Home", code: "Home")
                keyButton("End", code: "End")
            }
            HStack(spacing: 8) {
                TextField("Type a message…", text: $textToSend)
                    .textFieldStyle(.plain)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background(.white.opacity(0.1), in: Capsule())
                    .foregroundStyle(.white)
                    .submitLabel(.send)
                    .onSubmit(sendTypedText)
                Button(action: sendTypedText) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                        .foregroundStyle(.mint)
                }
                .disabled(textToSend.isEmpty)
                .accessibilityLabel("Send text to desktop")
            }
            if let typingError {
                Text(typingError)
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if session.clipboardAvailable {
                HStack(spacing: 12) {
                    PasteButton(payloadType: String.self) { values in
                        guard let text = values.first else { return }
                        typingError = session.paste(text) ? nil : "Clipboard text could not be sent."
                    }
                    .labelStyle(.iconOnly)
                    .accessibilityLabel("Paste mobile clipboard on desktop")
                    Button {
                        guard let text = session.remoteClipboard else { return }
                        UIPasteboard.general.setItems([["public.utf8-plain-text": text]], options: [.localOnly: true])
                        clipboardCopied = true
                    } label: {
                        Label(clipboardCopied ? "Copied" : "Copy from desktop", systemImage: clipboardCopied ? "checkmark" : "document.on.document")
                    }
                    .buttonStyle(SessionControlButtonStyle())
                    .disabled(session.remoteClipboard == nil)
                    Spacer(minLength: 0)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .disabled(!session.canControl)
    }

    private func viewport(in container: CGSize) -> DesktopViewport {
        DesktopViewport(container: container, resolution: session.resolution, zoom: zoom, offset: viewportOffset)
    }

    private func zoomFromCenter(by scale: CGFloat) {
        resize(zoom * scale,
               anchor: CGPoint(x: canvasSize.width / 2, y: canvasSize.height / 2),
               in: canvasSize)
    }

    private func resize(_ requestedZoom: CGFloat, anchor: CGPoint, in container: CGSize) {
        let old = viewport(in: container)
        let updatedZoom = min(max(requestedZoom, 1), 8)
        let ratio = updatedZoom / zoom
        let x = anchor.x - container.width / 2
        let y = anchor.y - container.height / 2
        let offset = CGSize(width: x + (old.offset.width - x) * ratio,
                            height: y + (old.offset.height - y) * ratio)
        zoom = updatedZoom
        viewportOffset = viewport(in: container).clamp(offset)
    }

    private func pointerTap(_ location: CGPoint, viewport: DesktopViewport) {
        guard session.canControl else { return }
        if inputMode == .direct {
            guard let point = viewport.point(at: location, clamped: false) else { return }
            session.move(x: point.x, y: point.y)
        } else {
            session.move(x: trackpadCursor.x, y: trackpadCursor.y)
        }
        click("left")
    }

    private func pointerDrag(_ state: UIGestureRecognizer.State, location: CGPoint, delta: CGSize, viewport: DesktopViewport) {
        guard session.canControl else { return }
        if state == .ended || state == .cancelled || state == .failed {
            endPointerDrag()
            return
        }
        if inputMode == .direct {
            guard state == .began || pointerDown else { return }
            guard let point = viewport.point(at: location, clamped: pointerDown) else { return }
            session.move(x: point.x, y: point.y)
            if state == .began {
                pointerDown = true
                session.button("left", pressed: true)
            }
        } else if state == .changed {
            let location = viewport.location(of: trackpadCursor)
            let next = CGPoint(x: location.x + delta.width, y: location.y + delta.height)
            guard let point = viewport.point(at: next, clamped: true) else { return }
            trackpadCursor = point
            session.move(x: point.x, y: point.y)
            viewportOffset = viewport.reveal(point)
        }
    }

    private func endPointerDrag() {
        if pointerDown {
            session.button("left", pressed: false)
            pointerDown = false
        }
    }

    private func click(_ button: String) {
        session.button(button, pressed: true)
        session.button(button, pressed: false)
    }

    private func keyButton(_ title: String, code: String) -> some View {
        Button {
            session.tapKey(code)
        } label: {
            Text(title)
                .font(.caption2.weight(.semibold))
                .frame(maxWidth: .infinity)
                .frame(minHeight: 44)
                .background(.white.opacity(0.11), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
        }
        .buttonStyle(.plain)
        .foregroundStyle(.white)
    }

    private func modifierButton(_ title: String, code: String) -> some View {
        let held = heldModifiers.contains(code)
        return Button {
            if held {
                session.key(code, pressed: false)
                heldModifiers.remove(code)
            } else {
                session.key(code, pressed: true)
                heldModifiers.insert(code)
            }
        } label: {
            Text(title)
                .font(.caption2.weight(.semibold))
                .frame(maxWidth: .infinity)
                .frame(minHeight: 44)
                .background(held ? Color.mint.opacity(0.65) : Color.white.opacity(0.11), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
        }
        .buttonStyle(.plain)
        .foregroundStyle(.white)
    }

    private func releaseHeldModifiers() {
        for code in heldModifiers { session.key(code, pressed: false) }
        heldModifiers.removeAll()
    }

    private func sendTypedText() {
        guard !textToSend.isEmpty else { return }
        let isASCII = textToSend.unicodeScalars.allSatisfy { $0.isASCII }
        let sent: Bool
        if isASCII {
            sent = session.sendText(textToSend)
        } else if session.clipboardAvailable {
            sent = session.paste(textToSend)
        } else {
            sent = false
        }
        guard sent else {
            typingError = isASCII ? "Text could not be sent. The message may be too long." : "Enable clipboard sharing to paste non-ASCII text."
            return
        }
        typingError = nil
        textToSend = ""
    }

    private var phaseText: String {
        switch session.phase {
        case .idle: "Disconnected"
        case .connecting: "Connecting"
        case .streaming: "Live"
        case .failed: "Needs attention"
        }
    }

    private var phaseColor: Color {
        switch session.phase {
        case .streaming: .mint
        case .connecting: .yellow
        case .failed: .orange
        case .idle: .gray
        }
    }
}

private enum RemoteInputMode: String, CaseIterable, Identifiable {
    case direct
    case trackpad

    var id: Self { self }
    var title: String { self == .direct ? "Touch" : "Trackpad" }
    var symbol: String { self == .direct ? "hand.tap" : "rectangle.dashed" }
}

private struct SessionControlButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .foregroundStyle(.white)
            .background(.white.opacity(configuration.isPressed ? 0.2 : 0.11), in: Capsule())
    }
}
