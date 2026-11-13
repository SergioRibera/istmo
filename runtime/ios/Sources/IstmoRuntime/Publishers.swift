import Foundation

/// Typed `publishXxx` helpers for the first-party plugins bundled with
/// `istmo-plugins`. Each helper encodes the plugin's bincode message
/// shape and forwards to the generic `publishEarlyLatest`.
///
/// Call sites: Swift observer wiring (`NSNotificationCenter`,
/// `NWPathMonitor`, `UIDevice.batteryStateDidChangeNotification`,
/// `UIAccessibility.*DidChangeNotification`, …) in the app's
/// `AppDelegate` / main scene.

private let SYSTEM_INFO_LOCALE_CHANNEL = "istmo.system_info.locale"
private let SYSTEM_INFO_TIMEZONE_CHANNEL = "istmo.system_info.timezone"
private let SYSTEM_INFO_FORMATS_CHANNEL = "istmo.system_info.formats"
private let DEVICE_INFO_CHANNEL = "istmo.device_info"
private let THEME_PREFERENCE_CHANNEL = "istmo.theme_preference"
private let NETWORK_STATE_CHANNEL = "istmo.network_state"
private let BATTERY_STATE_CHANNEL = "istmo.battery_state"
private let STORAGE_INFO_CHANNEL = "istmo.storage_info"
private let CAPABILITIES_CHANNEL = "istmo.capabilities"

public enum LayoutDirection: UInt64 {
    case ltr = 0
    case rtl = 1
}

public enum MeasurementSystem: UInt64 {
    case metric = 0
    case usCustomary = 1
    case ukImperial = 2
}

public enum TemperatureUnit: UInt64 {
    case celsius = 0
    case fahrenheit = 1
}

/// Mirrors `istmo_plugins::system_info::CalendarSystem`.
/// `.other` carries the raw platform identifier (e.g. `"indian"`).
public enum CalendarSystem {
    case gregorian
    case buddhist
    case japanese
    case islamic
    case hebrew
    case persian
    case other(String)

    fileprivate var discriminant: UInt64 {
        switch self {
        case .gregorian: return 0
        case .buddhist: return 1
        case .japanese: return 2
        case .islamic: return 3
        case .hebrew: return 4
        case .persian: return 5
        case .other: return 6
        }
    }
}

public enum FormFactor: UInt64 {
    case phone = 0
    case tablet = 1
    case watch = 2
    case tv = 3
    case desktop = 4
    case web = 5
    case unknown = 6
}

public enum ColorScheme: UInt64 {
    case light = 0
    case dark = 1
    case auto = 2
}

public enum LinkKind: UInt64 {
    case none = 0
    case wifi = 1
    case cellular = 2
    case ethernet = 3
    case vpn = 4
    case bluetooth = 5
    case other = 6
}

public enum ChargingState: UInt64 {
    case unknown = 0
    case charging = 1
    case discharging = 2
    case notCharging = 3
    case full = 4
}

public enum PowerSource: UInt64 {
    case unknown = 0
    case battery = 1
    case ac = 2
    case usb = 3
    case wireless = 4
}

public enum ThermalState: UInt64 {
    case nominal = 0
    case fair = 1
    case serious = 2
    case critical = 3
}

public enum ExternalMedium: UInt64 {
    case unknown = 0
    case sdCard = 1
    case usb = 2
    case network = 3
}

public struct ScreenMetrics {
    public let logicalWidth: UInt32
    public let logicalHeight: UInt32
    public let scale: Float
    public let refreshHz: Float?

    public init(logicalWidth: UInt32, logicalHeight: UInt32, scale: Float, refreshHz: Float?) {
        self.logicalWidth = logicalWidth
        self.logicalHeight = logicalHeight
        self.scale = scale
        self.refreshHz = refreshHz
    }
}

public struct ExternalVolume {
    public let freeBytes: UInt64
    public let totalBytes: UInt64
    public let medium: ExternalMedium

    public init(freeBytes: UInt64, totalBytes: UInt64, medium: ExternalMedium) {
        self.freeBytes = freeBytes
        self.totalBytes = totalBytes
        self.medium = medium
    }
}

public struct ColorRgba {
    public let r: UInt8
    public let g: UInt8
    public let b: UInt8
    public let a: UInt8

    public init(r: UInt8, g: UInt8, b: UInt8, a: UInt8 = 255) {
        self.r = r
        self.g = g
        self.b = b
        self.a = a
    }
}

private func writeOptionString(_ out: inout Data, _ value: String?) {
    Bincode.writeOption(&out, value) { s, v in Bincode.writeString(&s, v) }
}

private func writeOptionF32(_ out: inout Data, _ value: Float?) {
    Bincode.writeOption(&out, value) { s, v in Bincode.writeF32(&s, v) }
}

private func writeOptionU64(_ out: inout Data, _ value: UInt64?) {
    Bincode.writeOption(&out, value) { s, v in Bincode.writeVarintU64(&s, v) }
}

private func writeCalendar(_ out: inout Data, _ calendar: CalendarSystem) {
    Bincode.writeVarintU64(&out, calendar.discriminant)
    if case let .other(id) = calendar {
        Bincode.writeString(&out, id)
    }
}

extension IstmoRuntime {

    public func publishLocale(
        bcp47: String,
        language: String,
        region: String?,
        script: String?,
        direction: LayoutDirection
    ) {
        var out = Data()
        Bincode.writeString(&out, bcp47)
        Bincode.writeString(&out, language)
        writeOptionString(&out, region)
        writeOptionString(&out, script)
        Bincode.writeVarintU64(&out, direction.rawValue)
        publishEarlyLatest(channel: SYSTEM_INFO_LOCALE_CHANNEL, payload: out)
    }

    public func publishTimezone(
        id: String,
        offsetSeconds: Int32,
        isDst: Bool,
        abbreviation: String
    ) {
        var out = Data()
        Bincode.writeString(&out, id)
        Bincode.writeVarintI32(&out, offsetSeconds)
        Bincode.writeBool(&out, isDst)
        Bincode.writeString(&out, abbreviation)
        publishEarlyLatest(channel: SYSTEM_INFO_TIMEZONE_CHANNEL, payload: out)
    }

    public func publishFormats(
        uses24h: Bool,
        firstDayOfWeek: UInt8,
        measurement: MeasurementSystem,
        temperatureUnit: TemperatureUnit,
        currencyCode: String?,
        decimalSeparator: String,
        groupSeparator: String,
        calendar: CalendarSystem
    ) {
        var out = Data()
        Bincode.writeBool(&out, uses24h)
        Bincode.writeU8(&out, firstDayOfWeek)
        Bincode.writeVarintU64(&out, measurement.rawValue)
        Bincode.writeVarintU64(&out, temperatureUnit.rawValue)
        writeOptionString(&out, currencyCode)
        Bincode.writeString(&out, decimalSeparator)
        Bincode.writeString(&out, groupSeparator)
        writeCalendar(&out, calendar)
        publishEarlyLatest(channel: SYSTEM_INFO_FORMATS_CHANNEL, payload: out)
    }

    public func publishDeviceInfo(
        model: String?,
        manufacturer: String?,
        osName: String,
        osVersion: String?,
        arch: String,
        formFactor: FormFactor,
        cpuCores: UInt32,
        ramTotalBytes: UInt64?,
        screen: ScreenMetrics?,
        vendorId: String?
    ) {
        var out = Data()
        writeOptionString(&out, model)
        writeOptionString(&out, manufacturer)
        Bincode.writeString(&out, osName)
        writeOptionString(&out, osVersion)
        Bincode.writeString(&out, arch)
        Bincode.writeVarintU64(&out, formFactor.rawValue)
        Bincode.writeVarintU32(&out, cpuCores)
        writeOptionU64(&out, ramTotalBytes)
        Bincode.writeOption(&out, screen) { s, v in
            Bincode.writeVarintU32(&s, v.logicalWidth)
            Bincode.writeVarintU32(&s, v.logicalHeight)
            Bincode.writeF32(&s, v.scale)
            writeOptionF32(&s, v.refreshHz)
        }
        writeOptionString(&out, vendorId)
        publishEarlyLatest(channel: DEVICE_INFO_CHANNEL, payload: out)
    }

    public func publishThemePreference(
        colorScheme: ColorScheme,
        accentColor: ColorRgba?,
        highContrast: Bool,
        fontScale: Float,
        reduceMotion: Bool,
        reduceTransparency: Bool,
        boldText: Bool,
        invertColors: Bool,
        screenReaderActive: Bool
    ) {
        var out = Data()
        Bincode.writeVarintU64(&out, colorScheme.rawValue)
        Bincode.writeOption(&out, accentColor) { s, v in
            Bincode.writeU8(&s, v.r)
            Bincode.writeU8(&s, v.g)
            Bincode.writeU8(&s, v.b)
            Bincode.writeU8(&s, v.a)
        }
        Bincode.writeBool(&out, highContrast)
        Bincode.writeF32(&out, fontScale)
        Bincode.writeBool(&out, reduceMotion)
        Bincode.writeBool(&out, reduceTransparency)
        Bincode.writeBool(&out, boldText)
        Bincode.writeBool(&out, invertColors)
        Bincode.writeBool(&out, screenReaderActive)
        publishEarlyLatest(channel: THEME_PREFERENCE_CHANNEL, payload: out)
    }

    public func publishNetworkState(
        online: Bool,
        link: LinkKind,
        metered: Bool,
        expensive: Bool,
        constrained: Bool
    ) {
        var out = Data()
        Bincode.writeBool(&out, online)
        Bincode.writeVarintU64(&out, link.rawValue)
        Bincode.writeBool(&out, metered)
        Bincode.writeBool(&out, expensive)
        Bincode.writeBool(&out, constrained)
        publishEarlyLatest(channel: NETWORK_STATE_CHANNEL, payload: out)
    }

    public func publishBatteryState(
        level: Float?,
        charging: ChargingState,
        powerSource: PowerSource,
        powerSaver: Bool,
        thermalState: ThermalState
    ) {
        var out = Data()
        writeOptionF32(&out, level)
        Bincode.writeVarintU64(&out, charging.rawValue)
        Bincode.writeVarintU64(&out, powerSource.rawValue)
        Bincode.writeBool(&out, powerSaver)
        Bincode.writeVarintU64(&out, thermalState.rawValue)
        publishEarlyLatest(channel: BATTERY_STATE_CHANNEL, payload: out)
    }

    public func publishStorageInfo(
        primaryFreeBytes: UInt64,
        primaryTotalBytes: UInt64,
        external: ExternalVolume?
    ) {
        var out = Data()
        Bincode.writeVarintU64(&out, primaryFreeBytes)
        Bincode.writeVarintU64(&out, primaryTotalBytes)
        Bincode.writeOption(&out, external) { s, v in
            Bincode.writeVarintU64(&s, v.freeBytes)
            Bincode.writeVarintU64(&s, v.totalBytes)
            Bincode.writeVarintU64(&s, v.medium.rawValue)
        }
        publishEarlyLatest(channel: STORAGE_INFO_CHANNEL, payload: out)
    }

    public func publishCapabilities(
        hasBiometric: Bool,
        hasFaceId: Bool,
        hasFingerprint: Bool,
        hasNfc: Bool,
        hasHaptics: Bool,
        hasCameraFront: Bool,
        hasCameraBack: Bool,
        hasGps: Bool,
        hasBluetooth: Bool,
        hasMicrophone: Bool,
        hasTelephony: Bool
    ) {
        var out = Data()
        Bincode.writeBool(&out, hasBiometric)
        Bincode.writeBool(&out, hasFaceId)
        Bincode.writeBool(&out, hasFingerprint)
        Bincode.writeBool(&out, hasNfc)
        Bincode.writeBool(&out, hasHaptics)
        Bincode.writeBool(&out, hasCameraFront)
        Bincode.writeBool(&out, hasCameraBack)
        Bincode.writeBool(&out, hasGps)
        Bincode.writeBool(&out, hasBluetooth)
        Bincode.writeBool(&out, hasMicrophone)
        Bincode.writeBool(&out, hasTelephony)
        publishEarlyLatest(channel: CAPABILITIES_CHANNEL, payload: out)
    }
}
