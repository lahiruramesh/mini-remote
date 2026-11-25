import Foundation
import Security
import SwiftUI

enum MobileQuality: String, Codable, CaseIterable, Identifiable {
    case automatic
    case lowData
    case balanced
    case sharp

    var id: Self { self }

    var title: String {
        switch self {
        case .automatic: "Automatic"
        case .lowData: "Low data"
        case .balanced: "Balanced"
        case .sharp: "Sharp"
        }
    }

    var detail: String {
        switch self {
        case .automatic: "Uses a lower profile on cellular and Low Data Mode."
        case .lowData: "Up to 480p · 15 fps · 850 kbps target"
        case .balanced: "Up to 720p · 24 fps · 2.2 Mbps target"
        case .sharp: "Up to 1080p · 30 fps · 4.8 Mbps target"
        }
    }

    var wireName: String {
        switch self {
        case .lowData: "low_data"
        case .automatic, .balanced: "balanced"
        case .sharp: "sharp"
        }
    }
}

struct SavedDevice: Identifiable, Codable, Equatable {
    var id = UUID()
    var name = ""
    var host = ""
    var port: UInt16 = 47990
    var quality: MobileQuality = .automatic
}

@MainActor
final class DeviceStore: ObservableObject {
    @Published private(set) var devices: [SavedDevice] = []
    private let preferences = UserDefaults.standard
    private let storageKey = "miniremote.devices.v1"

    init() {
        if let data = preferences.data(forKey: storageKey),
           let devices = try? JSONDecoder().decode([SavedDevice].self, from: data) {
            self.devices = devices
        }
    }

    func save(_ device: SavedDevice, token: String?) throws {
        if let token {
            try setToken(token.trimmingCharacters(in: .whitespacesAndNewlines), for: device)
        } else if let previous = devices.first(where: { $0.id == device.id }),
                  previous.host != device.host || previous.port != device.port {
            try setToken("", for: device)
        }
        if let existing = devices.firstIndex(where: { $0.id == device.id }) {
            devices[existing] = device
        } else {
            devices.append(device)
        }
        persist()
    }

    func remove(_ device: SavedDevice) {
        try? setToken("", for: device)
        devices.removeAll { $0.id == device.id }
        persist()
    }

    func token(for device: SavedDevice) -> String? {
        guard devices.contains(where: { $0.id == device.id && $0.host == device.host && $0.port == device.port }) else {
            return nil
        }
        var attributes = key(for: device)
        attributes[kSecReturnData] = true
        attributes[kSecMatchLimit] = kSecMatchLimitOne
        var item: CFTypeRef?
        guard SecItemCopyMatching(attributes as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    private func persist() {
        if let data = try? JSONEncoder().encode(devices) {
            preferences.set(data, forKey: storageKey)
        }
    }

    private func key(for device: SavedDevice) -> [CFString: Any] {
        [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: "local.miniremote.ios.pairing",
            kSecAttrAccount: device.id.uuidString
        ]
    }

    private func setToken(_ token: String, for device: SavedDevice) throws {
        let query = key(for: device)
        var status: OSStatus
        if token.isEmpty {
            status = SecItemDelete(query as CFDictionary)
            if status == errSecItemNotFound { return }
        } else {
            let updates: [CFString: Any] = [kSecValueData: Data(token.utf8)]
            status = SecItemUpdate(query as CFDictionary, updates as CFDictionary)
            if status == errSecItemNotFound {
                var newItem = query
                newItem[kSecValueData] = Data(token.utf8)
                newItem[kSecAttrAccessible] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
                status = SecItemAdd(newItem as CFDictionary, nil)
            }
        }
        guard status == errSecSuccess else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
    }
}
