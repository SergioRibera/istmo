import Foundation

/// iOS impl of the codegen `DataStoreBackend` protocol, backed by
/// `UserDefaults`.
///
/// `UserDefaults` is the closest platform analogue to Android's
/// `SharedPreferences`: a plist-backed keyed store scoped by suite name.
/// Ships with `Foundation`, so this plugin declares zero SwiftPM deps.
///
/// Byte payloads are stored via `set(_:forKey:)` which accepts `Data`
/// directly — no Base64 detour on this side.
final class DataStoreBackendImpl: DataStoreBackend {

    private let defaults: UserDefaults

    init(config: DataStoreConfig) throws {
        // `UserDefaults(suiteName:)` returns nil only when the suite name
        // collides with the process's own `Bundle` identifier or is one of
        // the reserved names (`NSGlobalDomain`, ...). Treat that as a
        // configuration error — the wire contract carries this back to
        // Rust as `DataStoreError.backend`.
        guard let store = UserDefaults(suiteName: config.namespace) else {
            throw DataStoreError.backend(
                "UserDefaults(suiteName: \(config.namespace)) is unavailable"
            )
        }
        self.defaults = store
    }

    func get_string(key: String) async throws -> String? {
        defaults.object(forKey: key) as? String
    }

    func set_string(key: String, value: String) async throws {
        defaults.set(value, forKey: key)
    }

    func get_i64(key: String) async throws -> Int64? {
        guard let raw = defaults.object(forKey: key) else { return nil }
        if let n = raw as? Int { return Int64(n) }
        if let n = raw as? Int64 { return n }
        return nil
    }

    func set_i64(key: String, value: Int64) async throws {
        defaults.set(NSNumber(value: value), forKey: key)
    }

    func get_f64(key: String) async throws -> Double? {
        guard let raw = defaults.object(forKey: key) else { return nil }
        return (raw as? NSNumber)?.doubleValue
    }

    func set_f64(key: String, value: Double) async throws {
        defaults.set(value, forKey: key)
    }

    func get_bool(key: String) async throws -> Bool? {
        guard let raw = defaults.object(forKey: key) else { return nil }
        return (raw as? NSNumber)?.boolValue
    }

    func set_bool(key: String, value: Bool) async throws {
        defaults.set(value, forKey: key)
    }

    func get_bytes(key: String) async throws -> Data? {
        defaults.data(forKey: key)
    }

    func set_bytes(key: String, value: Data) async throws {
        defaults.set(value, forKey: key)
    }

    func remove(key: String) async throws -> Bool {
        guard defaults.object(forKey: key) != nil else { return false }
        defaults.removeObject(forKey: key)
        return true
    }

    func contains(key: String) async throws -> Bool {
        defaults.object(forKey: key) != nil
    }

    func keys() async throws -> [String] {
        Array(defaults.dictionaryRepresentation().keys)
    }

    func clear() async throws {
        for key in defaults.dictionaryRepresentation().keys {
            defaults.removeObject(forKey: key)
        }
    }
}

/// Factory hook consumed by `DataStoreDispatcher`. Each `CreateInstance`
/// frame arriving from Rust invokes `create(config:)` with the
/// `DataStoreConfig` shipped from the Rust `acquire_with` call.
final class DataStoreFactoryImpl: DataStoreFactory {
    func create(config: DataStoreConfig) async throws -> DataStoreBackend {
        try DataStoreBackendImpl(config: config)
    }
}
