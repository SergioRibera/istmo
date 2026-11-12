import Foundation

/// `UserDefaults` backend: one suite per namespace.
///
/// The suite is named `<bundle id>.istmo.<namespace>`. `UserDefaults`
/// refuses a suite named after the app's own bundle id, and a bare
/// namespace could collide with another framework's suite, so the
/// namespace is never used as the suite name verbatim. `keys()` and
/// `clear()` read the suite's persistent domain only — the full
/// `dictionaryRepresentation()` also contains `NSGlobalDomain` and the
/// app's standard defaults.
///
/// A key read as the wrong type fails with `DataStoreError.corrupted`,
/// like the desktop and Android backends.
final class DataStoreBackendImpl: DataStoreBackend {

    private let defaults: UserDefaults
    private let suiteName: String

    init(config: DataStoreConfig) throws {
        let app = Bundle.main.bundleIdentifier ?? "app"
        let suiteName = "\(app).istmo.\(config.namespace)"
        guard let store = UserDefaults(suiteName: suiteName) else {
            throw DataStoreError.backend("UserDefaults(suiteName: \(suiteName)) is unavailable")
        }
        self.defaults = store
        self.suiteName = suiteName
    }

    func get_string(key: String) async throws -> String? {
        guard let raw = defaults.object(forKey: key) else { return nil }
        guard let value = raw as? String else { throw mismatch(key, "string") }
        return value
    }

    func set_string(key: String, value: String) async throws {
        defaults.set(value, forKey: key)
    }

    func get_i64(key: String) async throws -> Int64? {
        guard let raw = defaults.object(forKey: key) else { return nil }
        guard let n = raw as? NSNumber, !Self.isBool(n), !Self.isFloatingPoint(n) else {
            throw mismatch(key, "i64")
        }
        return n.int64Value
    }

    func set_i64(key: String, value: Int64) async throws {
        defaults.set(NSNumber(value: value), forKey: key)
    }

    func get_f64(key: String) async throws -> Double? {
        guard let raw = defaults.object(forKey: key) else { return nil }
        guard let n = raw as? NSNumber, !Self.isBool(n), Self.isFloatingPoint(n) else {
            throw mismatch(key, "f64")
        }
        return n.doubleValue
    }

    func set_f64(key: String, value: Double) async throws {
        defaults.set(NSNumber(value: value), forKey: key)
    }

    func get_bool(key: String) async throws -> Bool? {
        guard let raw = defaults.object(forKey: key) else { return nil }
        guard let n = raw as? NSNumber, Self.isBool(n) else { throw mismatch(key, "bool") }
        return n.boolValue
    }

    func set_bool(key: String, value: Bool) async throws {
        defaults.set(value, forKey: key)
    }

    func get_bytes(key: String) async throws -> Data? {
        guard let raw = defaults.object(forKey: key) else { return nil }
        guard let value = raw as? Data else { throw mismatch(key, "bytes") }
        return value
    }

    func set_bytes(key: String, value: Data) async throws {
        defaults.set(value, forKey: key)
    }

    func remove(key: String) async throws -> Bool {
        guard storedKeys().contains(key) else { return false }
        defaults.removeObject(forKey: key)
        return true
    }

    func contains(key: String) async throws -> Bool {
        storedKeys().contains(key)
    }

    func keys() async throws -> [String] {
        Array(storedKeys())
    }

    func clear() async throws {
        defaults.removePersistentDomain(forName: suiteName)
    }

    /// Keys of this suite only. `object(forKey:)` would also see keys
    /// inherited from `NSGlobalDomain` and the registration domain.
    private func storedKeys() -> Set<String> {
        Set(defaults.persistentDomain(forName: suiteName).map { Array($0.keys) } ?? [])
    }

    private func mismatch(_ key: String, _ wanted: String) -> DataStoreError {
        .corrupted("key `\(key)` holds another type but caller asked for \(wanted)")
    }

    /// Plist booleans come back as `CFBoolean`, which `as? NSNumber`
    /// happily bridges to 0 / 1; tell them apart by CF type.
    private static func isBool(_ n: NSNumber) -> Bool {
        CFGetTypeID(n) == CFBooleanGetTypeID()
    }

    private static func isFloatingPoint(_ n: NSNumber) -> Bool {
        let type = String(cString: n.objCType)
        return type == "d" || type == "f"
    }
}

final class DataStoreFactoryImpl: DataStoreFactory {
    func create(config: DataStoreConfig) async throws -> DataStoreBackend {
        try DataStoreBackendImpl(config: config)
    }
}
