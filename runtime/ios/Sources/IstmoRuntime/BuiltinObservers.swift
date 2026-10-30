import Foundation
import Network
#if canImport(UIKit)
import UIKit
#endif
#if canImport(LocalAuthentication)
import LocalAuthentication
#endif
#if canImport(CoreNFC) && os(iOS)
import CoreNFC
#endif
#if canImport(AVFoundation)
import AVFoundation
#endif
#if canImport(CoreLocation)
import CoreLocation
#endif
#if canImport(CoreHaptics) && os(iOS)
import CoreHaptics
#endif

/// Native observers that bridge Apple system events to the typed
/// `publishXxx` helpers. Each observer registers lazily on
/// `register()` and tears down on `unregister()` — call sites are
/// typically `AppDelegate.application(_:didFinishLaunchingWithOptions:)`
/// / `applicationWillTerminate`.
///
/// `IstmoBuiltinObservers(runtime:).registerAll()` wires every
/// observer at once; opt out by instantiating individual observers.

/// One-shot `publishDeviceInfo` from `UIDevice` / `ProcessInfo` /
/// `sysctl`.
public enum IstmoDeviceInfoObserver {

    public static func publishOnce(runtime: IstmoRuntime) {
        let proc = ProcessInfo.processInfo
        var model: String? = nil
        var manufacturer: String? = "Apple"
        var osName = "macos"
        var osVersion: String? = proc.operatingSystemVersionString
        let arch = currentArch()
        var formFactor: FormFactor = .desktop
        var vendorId: String? = nil
        var screen: ScreenMetrics? = nil

        #if canImport(UIKit)
        let device = UIDevice.current
        model = sysctlString("hw.machine") ?? device.model
        switch device.userInterfaceIdiom {
        case .phone: formFactor = .phone
        case .pad: formFactor = .tablet
        case .tv: formFactor = .tv
        case .carPlay: formFactor = .unknown
        case .mac: formFactor = .desktop
        default:
            #if os(watchOS)
            formFactor = .watch
            #elseif os(visionOS)
            formFactor = .unknown
            #else
            formFactor = .phone
            #endif
        }
        #if os(iOS)
        osName = "ios"
        #elseif os(tvOS)
        osName = "tvos"
        #elseif os(watchOS)
        osName = "watchos"
        #elseif os(visionOS)
        osName = "visionos"
        #endif
        vendorId = device.identifierForVendor?.uuidString
        let main = UIScreen.main
        let scale = Float(main.scale)
        screen = ScreenMetrics(
            logicalWidth: UInt32(main.bounds.width),
            logicalHeight: UInt32(main.bounds.height),
            scale: scale,
            refreshHz: nil
        )
        #else
        model = sysctlString("hw.model")
        #endif

        let ram = proc.physicalMemory
        runtime.publishDeviceInfo(
            model: model,
            manufacturer: manufacturer,
            osName: osName,
            osVersion: osVersion,
            arch: arch,
            formFactor: formFactor,
            cpuCores: UInt32(proc.processorCount),
            ramTotalBytes: ram > 0 ? ram : nil,
            screen: screen,
            vendorId: vendorId
        )
    }

    private static func currentArch() -> String {
        #if arch(arm64)
        return "aarch64"
        #elseif arch(x86_64)
        return "x86_64"
        #elseif arch(arm)
        return "arm"
        #elseif arch(i386)
        return "i386"
        #else
        return "unknown"
        #endif
    }

    private static func sysctlString(_ name: String) -> String? {
        var size: Int = 0
        if sysctlbyname(name, nil, &size, nil, 0) != 0 { return nil }
        var buf = [CChar](repeating: 0, count: size)
        if sysctlbyname(name, &buf, &size, nil, 0) != 0 { return nil }
        return String(cString: buf)
    }
}

/// One-shot `publishCapabilities` from `LAContext` / NFC / sensors.
public enum IstmoCapabilitiesObserver {

    public static func publishOnce(runtime: IstmoRuntime) {
        var hasBiometric = false
        var hasFaceId = false
        var hasFingerprint = false
        #if canImport(LocalAuthentication)
        let ctx = LAContext()
        var error: NSError?
        if ctx.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: &error) {
            hasBiometric = true
            if #available(iOS 11.0, macOS 10.15, *) {
                switch ctx.biometryType {
                case .faceID: hasFaceId = true
                case .touchID: hasFingerprint = true
                default: break
                }
            }
        }
        #endif

        var hasNfc = false
        #if canImport(CoreNFC) && os(iOS)
        if #available(iOS 11.0, *) {
            hasNfc = NFCNDEFReaderSession.readingAvailable
        }
        #endif

        var hasHaptics = false
        var hasCameraFront = false
        var hasCameraBack = false
        var hasGps = false
        var hasBluetooth = false
        var hasMicrophone = false
        var hasTelephony = false

        #if canImport(UIKit) && os(iOS)
        if #available(iOS 13.0, *) {
            hasHaptics = CHHapticEngine.capabilitiesForHardware().supportsHaptics
        }
        #endif

        #if canImport(AVFoundation)
        hasCameraFront = AVCaptureDevice.default(
            .builtInWideAngleCamera, for: .video, position: .front
        ) != nil
        hasCameraBack = AVCaptureDevice.default(
            .builtInWideAngleCamera, for: .video, position: .back
        ) != nil
        hasMicrophone = AVCaptureDevice.default(for: .audio) != nil
        #endif

        #if canImport(CoreLocation)
        hasGps = CLLocationManager.locationServicesEnabled()
        #endif

        #if canImport(CoreBluetooth)
        hasBluetooth = true
        #endif

        #if canImport(CoreTelephony) && os(iOS)
        hasTelephony = true
        #endif

        runtime.publishCapabilities(
            hasBiometric: hasBiometric,
            hasFaceId: hasFaceId,
            hasFingerprint: hasFingerprint,
            hasNfc: hasNfc,
            hasHaptics: hasHaptics,
            hasCameraFront: hasCameraFront,
            hasCameraBack: hasCameraBack,
            hasGps: hasGps,
            hasBluetooth: hasBluetooth,
            hasMicrophone: hasMicrophone,
            hasTelephony: hasTelephony
        )
    }
}

/// `publishLocale` / `publishTimezone` / `publishFormats` on
/// `NSLocale.currentLocaleDidChangeNotification` and
/// `NSSystemTimeZoneDidChangeNotification`.
public final class IstmoSystemInfoObserver {

    private weak var runtime: IstmoRuntime?
    private var tokens: [NSObjectProtocol] = []

    public init(runtime: IstmoRuntime) {
        self.runtime = runtime
    }

    public func register() {
        guard tokens.isEmpty else { return }
        let center = NotificationCenter.default
        tokens.append(center.addObserver(
            forName: NSLocale.currentLocaleDidChangeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in self?.publishAll() })
        tokens.append(center.addObserver(
            forName: .NSSystemTimeZoneDidChange,
            object: nil,
            queue: .main
        ) { [weak self] _ in self?.publishAll() })
        publishAll()
    }

    public func unregister() {
        let center = NotificationCenter.default
        tokens.forEach { center.removeObserver($0) }
        tokens.removeAll()
    }

    private func publishAll() {
        guard let runtime = runtime else { return }
        let locale = Locale.current
        let language = locale.languageCode?.lowercased() ?? "en"
        let region = locale.regionCode?.uppercased()
        let script = locale.scriptCode
        let bcp47 = locale.identifier.replacingOccurrences(of: "_", with: "-")
        let rtlLanguages: Set<String> = [
            "ar", "arc", "ckb", "dv", "fa", "ha", "he", "khw", "ks",
            "ps", "sd", "ug", "ur", "yi"
        ]
        let direction: LayoutDirection = rtlLanguages.contains(language) ? .rtl : .ltr
        runtime.publishLocale(
            bcp47: bcp47,
            language: language,
            region: region,
            script: script,
            direction: direction
        )

        let tz = TimeZone.current
        runtime.publishTimezone(
            id: tz.identifier,
            offsetSeconds: Int32(tz.secondsFromGMT()),
            isDst: tz.isDaylightSavingTime(),
            abbreviation: tz.abbreviation() ?? tz.identifier
        )

        let cal = Calendar.current
        let appleFirst = cal.firstWeekday
        let isoFirstDay: UInt8 = appleFirst == 1 ? 7 : UInt8(appleFirst - 1)
        let uses24h: Bool = {
            let formatter = DateFormatter.dateFormat(fromTemplate: "j", options: 0,
                locale: locale) ?? ""
            return !formatter.contains("a")
        }()
        let measurement: MeasurementSystem = {
            switch region {
            case "US", "LR", "MM": return .usCustomary
            case "GB": return .ukImperial
            default: return .metric
            }
        }()
        let tempUnit: TemperatureUnit = {
            switch region {
            case "US", "BS", "BZ", "KY", "LR", "PW", "FM", "MH",
                 "PR", "GU", "VI", "AS", "MP":
                return .fahrenheit
            default: return .celsius
            }
        }()
        let formatter = NumberFormatter()
        formatter.locale = locale
        runtime.publishFormats(
            uses24h: uses24h,
            firstDayOfWeek: isoFirstDay,
            measurement: measurement,
            temperatureUnit: tempUnit,
            currencyCode: locale.currencyCode,
            decimalSeparator: formatter.decimalSeparator ?? ".",
            groupSeparator: formatter.groupingSeparator ?? ",",
            calendar: calendarFromIdentifier(cal.identifier)
        )
    }

    private func calendarFromIdentifier(_ id: Calendar.Identifier) -> CalendarSystem {
        switch id {
        case .gregorian: return .gregorian
        case .buddhist: return .buddhist
        case .japanese: return .japanese
        case .islamic, .islamicCivil, .islamicUmmAlQura, .islamicTabular:
            return .islamic
        case .hebrew: return .hebrew
        case .persian: return .persian
        default: return .other(String(describing: id))
        }
    }
}

/// `publishThemePreference` on `UITraitCollection` + `UIAccessibility`
/// notifications. No-op outside iOS/tvOS.
public final class IstmoThemePreferenceObserver {

    private weak var runtime: IstmoRuntime?
    private var tokens: [NSObjectProtocol] = []

    public init(runtime: IstmoRuntime) {
        self.runtime = runtime
    }

    public func register() {
        guard tokens.isEmpty else { return }
        #if canImport(UIKit) && !os(watchOS)
        let center = NotificationCenter.default
        let names: [Notification.Name] = [
            UIAccessibility.reduceMotionStatusDidChangeNotification,
            UIAccessibility.reduceTransparencyStatusDidChangeNotification,
            UIAccessibility.boldTextStatusDidChangeNotification,
            UIAccessibility.invertColorsStatusDidChangeNotification,
            UIAccessibility.voiceOverStatusDidChangeNotification,
            UIAccessibility.darkerSystemColorsStatusDidChangeNotification,
        ]
        for name in names {
            tokens.append(center.addObserver(
                forName: name, object: nil, queue: .main
            ) { [weak self] _ in self?.publish() })
        }
        #endif
        publish()
    }

    public func unregister() {
        let center = NotificationCenter.default
        tokens.forEach { center.removeObserver($0) }
        tokens.removeAll()
    }

    public func publish() {
        guard let runtime = runtime else { return }
        #if canImport(UIKit) && !os(watchOS)
        let traits = UIScreen.main.traitCollection
        let scheme: ColorScheme = {
            if #available(iOS 12.0, tvOS 10.0, *) {
                switch traits.userInterfaceStyle {
                case .light: return .light
                case .dark: return .dark
                default: return .auto
                }
            }
            return .auto
        }()
        runtime.publishThemePreference(
            colorScheme: scheme,
            accentColor: nil,
            highContrast: UIAccessibility.isDarkerSystemColorsEnabled,
            fontScale: 1.0,
            reduceMotion: UIAccessibility.isReduceMotionEnabled,
            reduceTransparency: UIAccessibility.isReduceTransparencyEnabled,
            boldText: UIAccessibility.isBoldTextEnabled,
            invertColors: UIAccessibility.isInvertColorsEnabled,
            screenReaderActive: UIAccessibility.isVoiceOverRunning
        )
        #else
        runtime.publishThemePreference(
            colorScheme: .auto,
            accentColor: nil,
            highContrast: false,
            fontScale: 1.0,
            reduceMotion: false,
            reduceTransparency: false,
            boldText: false,
            invertColors: false,
            screenReaderActive: false
        )
        #endif
    }
}

/// `publishNetworkState` driven by `NWPathMonitor`.
public final class IstmoNetworkStateObserver {

    private weak var runtime: IstmoRuntime?
    private let monitor = NWPathMonitor()
    private let queue = DispatchQueue(label: "dev.istmo.network-observer")
    private var running = false

    public init(runtime: IstmoRuntime) {
        self.runtime = runtime
    }

    public func register() {
        guard !running else { return }
        monitor.pathUpdateHandler = { [weak self] path in
            self?.publish(path)
        }
        monitor.start(queue: queue)
        running = true
    }

    public func unregister() {
        guard running else { return }
        monitor.cancel()
        running = false
    }

    private func publish(_ path: NWPath) {
        guard let runtime = runtime else { return }
        let online = path.status == .satisfied
        let link: LinkKind = {
            if path.usesInterfaceType(.wifi) { return .wifi }
            if path.usesInterfaceType(.cellular) { return .cellular }
            if path.usesInterfaceType(.wiredEthernet) { return .ethernet }
            if path.usesInterfaceType(.other) { return .other }
            return online ? .other : .none
        }()
        runtime.publishNetworkState(
            online: online,
            link: link,
            metered: path.isExpensive,
            expensive: path.isExpensive,
            constrained: path.isConstrained
        )
    }
}

/// `publishBatteryState` from `UIDevice` + `ProcessInfo`
/// notifications. No-op outside iOS.
public final class IstmoBatteryStateObserver {

    private weak var runtime: IstmoRuntime?
    private var tokens: [NSObjectProtocol] = []

    public init(runtime: IstmoRuntime) {
        self.runtime = runtime
    }

    public func register() {
        guard tokens.isEmpty else { return }
        #if os(iOS)
        UIDevice.current.isBatteryMonitoringEnabled = true
        let center = NotificationCenter.default
        let names: [Notification.Name] = [
            UIDevice.batteryStateDidChangeNotification,
            UIDevice.batteryLevelDidChangeNotification,
            Notification.Name.NSProcessInfoPowerStateDidChange,
            ProcessInfo.thermalStateDidChangeNotification,
        ]
        for name in names {
            tokens.append(center.addObserver(
                forName: name, object: nil, queue: .main
            ) { [weak self] _ in self?.publish() })
        }
        #endif
        publish()
    }

    public func unregister() {
        let center = NotificationCenter.default
        tokens.forEach { center.removeObserver($0) }
        tokens.removeAll()
    }

    private func publish() {
        guard let runtime = runtime else { return }
        var level: Float? = nil
        var charging: ChargingState = .unknown
        var source: PowerSource = .unknown
        var powerSaver = false
        var thermal: ThermalState = .nominal
        #if os(iOS)
        let device = UIDevice.current
        let rawLevel = device.batteryLevel
        level = rawLevel >= 0 ? rawLevel : nil
        switch device.batteryState {
        case .charging: charging = .charging; source = .ac
        case .full: charging = .full; source = .ac
        case .unplugged: charging = .discharging; source = .battery
        case .unknown: charging = .unknown
        @unknown default: charging = .unknown
        }
        powerSaver = ProcessInfo.processInfo.isLowPowerModeEnabled
        thermal = thermalFrom(ProcessInfo.processInfo.thermalState)
        #endif
        runtime.publishBatteryState(
            level: level,
            charging: charging,
            powerSource: source,
            powerSaver: powerSaver,
            thermalState: thermal
        )
    }

    private func thermalFrom(_ s: ProcessInfo.ThermalState) -> ThermalState {
        switch s {
        case .nominal: return .nominal
        case .fair: return .fair
        case .serious: return .serious
        case .critical: return .critical
        @unknown default: return .nominal
        }
    }
}

/// One-shot `publishStorageInfo` from the user's library directory
/// `URLResourceKey.volumeAvailableCapacityKey`. Poll on app resume.
public enum IstmoStorageInfoObserver {

    public static func publishOnce(runtime: IstmoRuntime) {
        let root = (try? FileManager.default.url(
            for: .libraryDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        )) ?? URL(fileURLWithPath: NSHomeDirectory())
        var free: UInt64 = 0
        var total: UInt64 = 0
        if let values = try? root.resourceValues(forKeys: [
            .volumeAvailableCapacityForImportantUsageKey,
            .volumeTotalCapacityKey,
        ]) {
            if let f = values.volumeAvailableCapacityForImportantUsage {
                free = UInt64(f)
            }
            if let t = values.volumeTotalCapacity {
                total = UInt64(t)
            }
        }
        runtime.publishStorageInfo(
            primaryFreeBytes: free,
            primaryTotalBytes: total,
            external: nil
        )
    }
}

/// Bundle: wires every observer at once. Keep a strong reference on
/// your `AppDelegate`; call `registerAll` on launch and
/// `unregisterAll` on terminate.
public final class IstmoBuiltinObservers {

    public let systemInfo: IstmoSystemInfoObserver
    public let theme: IstmoThemePreferenceObserver
    public let network: IstmoNetworkStateObserver
    public let battery: IstmoBatteryStateObserver
    private let runtime: IstmoRuntime

    public init(runtime: IstmoRuntime) {
        self.runtime = runtime
        self.systemInfo = IstmoSystemInfoObserver(runtime: runtime)
        self.theme = IstmoThemePreferenceObserver(runtime: runtime)
        self.network = IstmoNetworkStateObserver(runtime: runtime)
        self.battery = IstmoBatteryStateObserver(runtime: runtime)
    }

    public func registerAll() {
        IstmoDeviceInfoObserver.publishOnce(runtime: runtime)
        IstmoCapabilitiesObserver.publishOnce(runtime: runtime)
        IstmoStorageInfoObserver.publishOnce(runtime: runtime)
        systemInfo.register()
        theme.register()
        network.register()
        battery.register()
    }

    public func unregisterAll() {
        systemInfo.unregister()
        theme.unregister()
        network.unregister()
        battery.unregister()
    }
}
