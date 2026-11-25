import Foundation
import Network
import SwiftUI

private final class Attempt: @unchecked Sendable {
    private let lock = NSLock()
    private var transport: SessionTransport?
    private var cancelled = false
    private var lastVideo = Date()

    func set(_ transport: SessionTransport) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !cancelled else { return false }
        self.transport = transport
        return true
    }

    func withVideo(_ display: () -> Void) {
        lock.lock()
        defer { lock.unlock() }
        guard !cancelled else { return }
        lastVideo = Date()
        display()
    }

    var secondsSinceVideo: TimeInterval {
        lock.lock()
        defer { lock.unlock() }
        return Date().timeIntervalSince(lastVideo)
    }

    func stop() {
        lock.lock()
        cancelled = true
        let existing = transport
        transport = nil
        lock.unlock()
        existing?.stop()
    }
}

@MainActor
final class RemoteSession: ObservableObject {
    enum Phase: Equatable {
        case idle
        case connecting
        case streaming
        case failed(String)
    }

    @Published private(set) var phase: Phase = .idle
    @Published private(set) var resolution: CGSize = .zero
    @Published private(set) var clipboardAvailable = false
    @Published private(set) var remoteClipboard: String?
    @Published private(set) var streamLabel = ""
    @Published private(set) var layoutNotice: String?
    @Published private(set) var phoneWorkspaceActive = false
    @Published private(set) var canControl = false
    let video = RemoteVideo()

    private struct Request {
        var device: SavedDevice
        let token: String
        let clipboard: Bool
    }

    private var current: Attempt?
    private var request: Request?
    private var transport: SessionTransport?
    private var heartbeat: Task<Void, Never>?
    private let pathMonitor = NWPathMonitor()
    private var constrained = false
    private var activeProfile: MobileQuality = .balanced
    private var profileNegotiated = false

    init() {
        pathMonitor.pathUpdateHandler = { [weak self] path in
            let constrained = path.isExpensive || path.isConstrained
            Task { @MainActor in
                guard let self else { return }
                self.constrained = constrained
                if constrained, self.phase == .streaming, self.canReduceQuality {
                    self.start(.lowData, replacingConnection: true)
                }
            }
        }
        pathMonitor.start(queue: DispatchQueue(label: "miniremote.network-path"))
    }

    deinit {
        pathMonitor.cancel()
        heartbeat?.cancel()
        current?.stop()
    }

    func connect(_ device: SavedDevice, token: String, shareClipboard: Bool = false) {
        let secret = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard (1...128).contains(secret.utf8.count) else {
            disconnect()
            phase = .failed("Enter the host’s pairing token.")
            return
        }
        request = Request(device: device, token: secret, clipboard: shareClipboard)
        let profile = device.quality == .automatic ? (constrained ? .lowData : .balanced) : device.quality
        start(profile, replacingConnection: false)
    }

    func disconnect() {
        closeTransport()
        request = nil
        phase = .idle
        streamLabel = ""
    }

    func setPhoneWorkspace(_ enabled: Bool) {
        guard phase == .streaming, var request else { return }
        request.device.phoneWorkspace = enabled
        self.request = request
        start(activeProfile, replacingConnection: true)
    }

    func move(x: Double, y: Double) {
        input("pointer_move", ["x": max(0, min(1, x)), "y": max(0, min(1, y))], droppable: true)
    }

    func button(_ name: String, pressed: Bool) {
        input("pointer_button", ["button": name, "pressed": pressed])
    }

    func scroll(x: Double, y: Double) {
        input("scroll", ["x": max(-100, min(100, x)), "y": max(-100, min(100, y))])
    }

    func key(_ code: String, pressed: Bool) {
        input("key", ["code": code, "pressed": pressed])
    }

    func tapKey(_ code: String, modifiers: [String] = []) {
        guard canControl else { return }
        transport?.sendKeys(chord(code, modifiers))
    }

    func shortcut(_ action: String) {
        guard canControl, ["next_app", "previous_app", "close_window"].contains(action) else { return }
        transport?.send("window_shortcut", data: ["shortcut": action])
    }

    func sendText(_ text: String) -> Bool {
        guard canControl, !text.isEmpty else { return false }
        if clipboardAvailable && profileNegotiated {
            return paste(text)
        }
        guard text.utf8.count <= 128 else { return false }
        var events: [(String, Bool)] = []
        for char in text {
            guard let (code, shift) = asciiKey(char) else { return false }
            events += chord(code, shift ? ["ShiftLeft"] : [])
        }
        transport?.sendKeys(events)
        return true
    }

    func paste(_ text: String) -> Bool {
        guard canControl, clipboardAvailable, profileNegotiated, !text.isEmpty,
              text.utf8.count <= 131_072 else { return false }
        transport?.send("paste_text", data: ["text": text])
        return true
    }

    private var canReduceQuality: Bool {
        activeProfile == .balanced && request?.device.quality == .automatic
    }

    private func closeTransport() {
        heartbeat?.cancel()
        heartbeat = nil
        current?.stop()
        current = nil
        transport = nil
        resolution = .zero
        layoutNotice = nil
        phoneWorkspaceActive = false
        clipboardAvailable = false
        remoteClipboard = nil
        canControl = false
        profileNegotiated = false
        video.reset()
    }

    private func start(_ profile: MobileQuality, replacingConnection: Bool) {
        guard let request else { return }
        closeTransport()
        phase = .connecting
        activeProfile = profile
        streamLabel = "Connecting"
        let attempt = Attempt()
        current = attempt
        let video = video
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            if replacingConnection { Thread.sleep(forTimeInterval: 1) }
            do {
                let transport = try SessionTransport.connect(host: request.device.host, port: request.device.port)
                guard attempt.set(transport) else { return }
                try transport.authenticate(token: request.token, clipboard: request.clipboard, profile: profile,
                                           phoneWorkspace: request.device.phoneWorkspace == true)
                DispatchQueue.main.async { [weak self] in
                    self?.transportBecameAvailable(transport, attempt: attempt)
                }
                var headersReceived = false
                var shown = false
                while true {
                    let frame = try transport.readFrame()
                    if frame.kind == 1 {
                        guard headersReceived else { throw TransportError.invalidPacket }
                        attempt.withVideo { video.append(frame.payload) }
                        if !shown {
                            shown = true
                            DispatchQueue.main.async { [weak self] in
                                guard let self, self.current === attempt else { return }
                                self.phase = .streaming
                            }
                        }
                        continue
                    }
                    guard let envelope = try JSONSerialization.jsonObject(with: frame.payload) as? [String: Any],
                          let kind = envelope["message"] as? String else { throw TransportError.invalidPacket }
                    let data = envelope["data"] as? [String: Any] ?? [:]
                    if kind == "error" {
                        throw SessionRejected(message: data["message"] as? String ?? "The connector declined the connection.")
                    }
                    if kind == "ready" {
                        guard !headersReceived, let width = data["width"] as? Int,
                              let height = data["height"] as? Int,
                              (1...7680).contains(width), (1...4320).contains(height) else {
                            throw TransportError.invalidPacket
                        }
                        headersReceived = true
                    }
                    if ["ready", "input_status", "clipboard_status", "clipboard"].contains(kind) {
                        DispatchQueue.main.async { [weak self] in
                            self?.handle(kind, data: data, attempt: attempt)
                        }
                    }
                }
            } catch {
                DispatchQueue.main.async { [weak self] in
                    guard let self, self.current === attempt else { return }
                    if self.canReduceQuality,
                       let failure = error as? TransportError,
                       case .unavailable = failure,
                       self.transport != nil {
                        self.start(.lowData, replacingConnection: true)
                    } else {
                        self.closeTransport()
                        self.request = nil
                        self.phase = .failed(error.localizedDescription)
                    }
                }
            }
        }
    }

    private func transportBecameAvailable(_ transport: SessionTransport, attempt: Attempt) {
        guard current === attempt else { return }
        self.transport = transport
        heartbeat = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                guard let self, self.current === attempt, !Task.isCancelled else { return }
                if self.canReduceQuality && attempt.secondsSinceVideo > 8.0 {
                    self.start(.lowData, replacingConnection: true)
                    return
                }
                transport.send("ping")
            }
        }
    }

    private func handle(_ kind: String, data: [String: Any], attempt: Attempt) {
        guard current === attempt else { return }
        switch kind {
        case "ready":
            resolution = CGSize(width: data["width"] as? Int ?? 0, height: data["height"] as? Int ?? 0)
            phoneWorkspaceActive = data["phone_workspace"] as? Bool ?? false
            if request?.device.phoneWorkspace == true && !phoneWorkspaceActive {
                layoutNotice = data["phone_workspace"] == nil
                    ? "Update the desktop connector to use Phone Workspace."
                    : "Phone Workspace is unavailable for this display. Showing the regular desktop."
            }
            let negotiated = data["profile"] as? String
            profileNegotiated = negotiated != nil
            let name = switch negotiated {
            case "low_data": MobileQuality.lowData.title
            case "balanced": MobileQuality.balanced.title
            case "sharp": MobileQuality.sharp.title
            default: "Desktop"
            }
            let layout = phoneWorkspaceActive ? "Phone Workspace" : name
            streamLabel = "\(layout) · \(Int(resolution.width)) × \(Int(resolution.height))"
        case "input_status":
            canControl = data["enabled"] as? Bool ?? false
        case "clipboard_status":
            clipboardAvailable = request?.clipboard == true && (data["enabled"] as? Bool ?? false)
            if !clipboardAvailable { remoteClipboard = nil }
        case "clipboard":
            if clipboardAvailable, let text = data["text"] as? String, text.utf8.count <= 131_072 {
                remoteClipboard = text
            }
        default: break
        }
    }

    private func input(_ kind: String, _ values: [String: Any], droppable: Bool = false) {
        guard canControl else { return }
        var data = values
        data["event"] = kind
        transport?.send("input", data: data, droppable: droppable)
    }
}

private struct SessionRejected: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

private func chord(_ key: String, _ modifiers: [String]) -> [(String, Bool)] {
    modifiers.map { ($0, true) } + [(key, true), (key, false)] + modifiers.reversed().map { ($0, false) }
}

private func asciiKey(_ char: Character) -> (String, Bool)? {
    if let value = char.asciiValue {
        switch value {
        case 65...90: return ("Key\(char)", true)
        case 97...122: return ("Key\(char.uppercased())", false)
        case 48...57: return ("Digit\(char)", false)
        default: break
        }
    }
    if let digit = ")!@#$%^&*(".firstIndex(of: char) {
        return ("Digit\(")!@#$%^&*(".distance(from: ")!@#$%^&*(".startIndex, to: digit))", true)
    }
    let punctuation: [Character: (String, Bool)] = [
        " ": ("Space", false), "\n": ("Enter", false), "\t": ("Tab", false),
        "-": ("Minus", false), "_": ("Minus", true), "=": ("Equal", false), "+": ("Equal", true),
        "[": ("BracketLeft", false), "{": ("BracketLeft", true), "]": ("BracketRight", false), "}": ("BracketRight", true),
        "\\": ("Backslash", false), "|": ("Backslash", true), ";": ("Semicolon", false), ":": ("Semicolon", true),
        "'": ("Quote", false), "\"": ("Quote", true), ",": ("Comma", false), "<": ("Comma", true),
        ".": ("Period", false), ">": ("Period", true), "/": ("Slash", false), "?": ("Slash", true),
        "`": ("Backquote", false), "~": ("Backquote", true)
    ]
    return punctuation[char]
}
