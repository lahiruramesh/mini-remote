import Darwin
import Foundation

enum TransportError: LocalizedError {
    case invalidAddress
    case tunnelMissing
    case unavailable(String)
    case closed
    case invalidPacket

    var errorDescription: String? {
        switch self {
        case .invalidAddress:
            "Enter the host’s Tailscale IPv4 address (100.64.0.0 through 100.127.255.255)."
        case .tunnelMissing:
            "MiniRemote couldn’t find a Tailscale interface. Open Tailscale on this phone, turn it on, and try again."
        case .unavailable(let message): message
        case .closed: "The connector closed the connection."
        case .invalidPacket: "The connector returned an invalid message."
        }
    }
}

private struct TunnelInterface {
    var address: in_addr
    var index: UInt32

    static func discover() throws -> TunnelInterface {
        var interfaces: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&interfaces) == 0, let interfaces else {
            throw TransportError.tunnelMissing
        }
        defer { freeifaddrs(interfaces) }

        var matches = [UInt32: TunnelInterface]()
        var next: UnsafeMutablePointer<ifaddrs>? = interfaces
        while let item = next {
            let interface = item.pointee
            next = interface.ifa_next
            let name = String(cString: interface.ifa_name)
            guard name.hasPrefix("utun"),
                  interface.ifa_flags & UInt32(IFF_UP) != 0,
                  let address = interface.ifa_addr,
                  address.pointee.sa_family == sa_family_t(AF_INET) else { continue }
            let ipv4 = UnsafeRawPointer(address).assumingMemoryBound(to: sockaddr_in.self).pointee
            let index = if_nametoindex(interface.ifa_name)
            if index != 0 && isTailnet(ipv4.sin_addr) {
                matches[index] = TunnelInterface(address: ipv4.sin_addr, index: index)
            }
        }
        guard matches.count == 1, let found = matches.values.first else {
            throw TransportError.tunnelMissing
        }
        return found
    }
}

private func isTailnet(_ address: in_addr) -> Bool {
    let value = UInt32(bigEndian: address.s_addr)
    return value & 0xffc00000 == 0x64400000
}

private func tailnetAddress(_ host: String) -> in_addr? {
    var address = in_addr()
    guard host.withCString({ inet_pton(AF_INET, $0, &address) }) == 1, isTailnet(address) else { return nil }
    return address
}

final class SessionTransport: @unchecked Sendable {
    private let descriptor: Int32
    private let lock = NSLock()
    private let writer = DispatchQueue(label: "miniremote.input")
    private var pendingWrites = 0
    private var stopRequested = false

    private init(descriptor: Int32) { self.descriptor = descriptor }

    deinit {
        Darwin.close(descriptor)
    }

    static func isValidHost(_ host: String) -> Bool { tailnetAddress(host) != nil }

    static func connect(host: String, port: UInt16) throws -> SessionTransport {
        guard port != 0, let address = tailnetAddress(host) else {
            throw TransportError.invalidAddress
        }
        let tunnel = try TunnelInterface.discover()
        let descriptor = Darwin.socket(AF_INET, SOCK_STREAM, IPPROTO_TCP)
        guard descriptor >= 0 else { throw socketError("Couldn’t open the connection") }
        let transport = SessionTransport(descriptor: descriptor)

        // Binding both the source address and the interface keeps the socket off
        // Wi-Fi and cellular routes that happen to use the same CGNAT range.
        var route = tunnel.index
        guard setsockopt(descriptor, IPPROTO_IP, IP_BOUND_IF, &route, socklen_t(MemoryLayout<UInt32>.size)) == 0 else {
            throw TransportError.tunnelMissing
        }
        var source = makeAddress(tunnel.address, port: 0)
        let bound = withUnsafePointer(to: &source) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0 else { throw TransportError.tunnelMissing }

        var one: Int32 = 1
        _ = setsockopt(descriptor, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
        _ = setsockopt(descriptor, IPPROTO_TCP, TCP_NODELAY, &one, socklen_t(MemoryLayout<Int32>.size))

        let flags = fcntl(descriptor, F_GETFL)
        guard flags >= 0, fcntl(descriptor, F_SETFL, flags | O_NONBLOCK) == 0 else {
            throw socketError("Couldn’t configure the connection")
        }
        var destination = makeAddress(address, port: port)
        let connected = withUnsafePointer(to: &destination) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        if connected != 0 {
            guard errno == EINPROGRESS else { throw socketError("Couldn’t reach the connector") }
            var pollDescriptor = pollfd(fd: descriptor, events: Int16(POLLOUT), revents: 0)
            let result = poll(&pollDescriptor, 1, 5_000)
            guard result > 0 else {
                throw TransportError.unavailable("The connector didn’t answer. Check that it is running and the address is correct.")
            }
            var connectionError: Int32 = 0
            var length = socklen_t(MemoryLayout<Int32>.size)
            guard getsockopt(descriptor, SOL_SOCKET, SO_ERROR, &connectionError, &length) == 0,
                  connectionError == 0 else {
                throw socketError("Couldn’t reach the connector", code: connectionError == 0 ? errno : connectionError)
            }
        }
        guard fcntl(descriptor, F_SETFL, flags) == 0 else {
            throw socketError("Couldn’t configure the connection")
        }
        var readTimeout = timeval(tv_sec: 15, tv_usec: 0)
        var writeTimeout = timeval(tv_sec: 4, tv_usec: 0)
        let timeoutSize = socklen_t(MemoryLayout<timeval>.size)
        guard setsockopt(descriptor, SOL_SOCKET, SO_RCVTIMEO, &readTimeout, timeoutSize) == 0,
              setsockopt(descriptor, SOL_SOCKET, SO_SNDTIMEO, &writeTimeout, timeoutSize) == 0 else {
            throw socketError("Couldn’t configure the connection timeout")
        }
        return transport
    }

    func send(_ message: String, data: [String: Any]? = nil, droppable: Bool = false) {
        guard let frame = try? packet(message: message, data: data) else { return }
        enqueue(frame, droppable: droppable)
    }

    func sendKeys(_ events: [(String, Bool)]) {
        var frames = Data()
        for (code, pressed) in events {
            guard let frame = try? packet(message: "input", data: ["event": "key", "code": code, "pressed": pressed]) else { return }
            frames.append(frame)
        }
        if !frames.isEmpty { enqueue(frames, droppable: false) }
    }

    private func enqueue(_ frame: Data, droppable: Bool) {
        lock.lock()
        if stopRequested || (droppable && pendingWrites >= 10) {
            lock.unlock()
            return
        }
        if pendingWrites >= 128 {
            lock.unlock()
            stop()
            return
        }
        pendingWrites += 1
        lock.unlock()

        writer.async { [self] in
            defer {
                lock.lock()
                pendingWrites -= 1
                lock.unlock()
            }
            do {
                try write(frame)
            } catch {
                stop()
            }
        }
    }

    static func redeem(_ invitation: DesktopInvitation) throws -> String {
        let socket = try connect(host: invitation.host, port: invitation.port)
        defer { socket.stop() }
        try socket.write(socket.packet(message: "redeem_invite", data: ["code": invitation.code]))
        let response = try socket.readFrame()
        guard response.kind == 0,
              let object = try JSONSerialization.jsonObject(with: response.payload) as? [String: Any],
              let message = object["message"] as? String,
              let data = object["data"] as? [String: Any] else { throw TransportError.invalidPacket }
        if message == "error", let text = data["message"] as? String {
            throw TransportError.unavailable(text)
        }
        guard message == "paired", let token = data["token"] as? String,
              DesktopInvitation.validHex(token, length: 64) else { throw TransportError.invalidPacket }
        return token
    }

    func authenticate(token: String, clipboard: Bool, profile: MobileQuality) throws {
        try write(packet(message: "authenticate", data: [
            "token": token,
            "clipboard": clipboard,
            "profile": profile.wireName,
        ]))
    }

    func readFrame() throws -> (kind: UInt8, payload: Data) {
        let header = try read(count: 6)
        guard header[0] == 1, header[1] <= 1 else { throw TransportError.invalidPacket }
        let length = header.dropFirst(2).reduce(0) { ($0 << 8) | Int($1) }
        let limit = header[1] == 1 ? 65_536 : 786_688
        guard length > 0, length <= limit else { throw TransportError.invalidPacket }
        return (header[1], try read(count: length))
    }

    func stop() {
        lock.lock()
        let shouldShutdown = !stopRequested
        stopRequested = true
        lock.unlock()
        if shouldShutdown { _ = shutdown(descriptor, SHUT_RDWR) }
    }

    private func packet(message: String, data: [String: Any]?) throws -> Data {
        var envelope: [String: Any] = ["message": message]
        if let data { envelope["data"] = data }
        let payload = try JSONSerialization.data(withJSONObject: envelope)
        let count = payload.count
        guard count <= 786_688 else { throw TransportError.invalidPacket }
        var frame = Data([1, 0, UInt8(truncatingIfNeeded: count >> 24), UInt8(truncatingIfNeeded: count >> 16),
                          UInt8(truncatingIfNeeded: count >> 8), UInt8(truncatingIfNeeded: count)])
        frame.append(payload)
        return frame
    }

    private func read(count: Int) throws -> Data {
        var result = Data(count: count)
        try result.withUnsafeMutableBytes { bytes in
            guard let address = bytes.baseAddress else { throw TransportError.invalidPacket }
            var offset = 0
            while offset < count {
                let received = recv(descriptor, address.advanced(by: offset), count - offset, 0)
                if received == 0 { throw TransportError.closed }
                if received < 0 {
                    if errno == EINTR { continue }
                    throw socketError("The connector stopped responding")
                }
                offset += received
            }
        }
        return result
    }

    private func write(_ data: Data) throws {
        try data.withUnsafeBytes { bytes in
            guard let address = bytes.baseAddress else { throw TransportError.invalidPacket }
            var offset = 0
            while offset < bytes.count {
                let sent = Darwin.send(descriptor, address.advanced(by: offset), bytes.count - offset, 0)
                if sent == 0 { throw TransportError.closed }
                if sent < 0 {
                    if errno == EINTR { continue }
                    throw socketError("Couldn’t send data")
                }
                offset += sent
            }
        }
    }
}

private func makeAddress(_ ip: in_addr, port: UInt16) -> sockaddr_in {
    var address = sockaddr_in()
    address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    address.sin_family = sa_family_t(AF_INET)
    address.sin_addr = ip
    address.sin_port = port.bigEndian
    return address
}

private func socketError(_ prefix: String, code: Int32 = errno) -> TransportError {
    .unavailable("\(prefix). \(String(cString: strerror(code)))")
}
