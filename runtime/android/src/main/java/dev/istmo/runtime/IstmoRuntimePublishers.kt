package dev.istmo.runtime

import java.io.ByteArrayOutputStream

/**
 * Typed `publishXxx` helpers for the first-party plugins bundled with
 * `istmo-plugins`. Each helper encodes the plugin's bincode message
 * shape and forwards to the generic `nativeSubmitEarlyLatest`.
 *
 * Call sites: Kotlin observer wiring (BroadcastReceiver,
 * AccessibilityManager, ConnectivityManager, PowerManager, …) in the
 * app's `Application` / main activity.
 */

private const val SYSTEM_INFO_LOCALE_CHANNEL = "istmo.system_info.locale"
private const val SYSTEM_INFO_TIMEZONE_CHANNEL = "istmo.system_info.timezone"
private const val SYSTEM_INFO_FORMATS_CHANNEL = "istmo.system_info.formats"
private const val DEVICE_INFO_CHANNEL = "istmo.device_info"
private const val THEME_PREFERENCE_CHANNEL = "istmo.theme_preference"
private const val NETWORK_STATE_CHANNEL = "istmo.network_state"
private const val BATTERY_STATE_CHANNEL = "istmo.battery_state"
private const val STORAGE_INFO_CHANNEL = "istmo.storage_info"
private const val CAPABILITIES_CHANNEL = "istmo.capabilities"

enum class LayoutDirection(val discriminant: Int) {
    LTR(0),
    RTL(1),
}

enum class MeasurementSystem(val discriminant: Int) {
    METRIC(0),
    US_CUSTOMARY(1),
    UK_IMPERIAL(2),
}

enum class TemperatureUnit(val discriminant: Int) {
    CELSIUS(0),
    FAHRENHEIT(1),
}

/**
 * Mirrors `istmo_plugins::system_info::CalendarSystem`.
 * `OTHER` carries the raw platform identifier (e.g. `"indian"`).
 */
sealed class CalendarSystem(val discriminant: Int) {
    object Gregorian : CalendarSystem(0)
    object Buddhist : CalendarSystem(1)
    object Japanese : CalendarSystem(2)
    object Islamic : CalendarSystem(3)
    object Hebrew : CalendarSystem(4)
    object Persian : CalendarSystem(5)
    data class Other(val identifier: String) : CalendarSystem(6)
}

enum class FormFactor(val discriminant: Int) {
    PHONE(0),
    TABLET(1),
    WATCH(2),
    TV(3),
    DESKTOP(4),
    WEB(5),
    UNKNOWN(6),
}

enum class ColorScheme(val discriminant: Int) {
    LIGHT(0),
    DARK(1),
    AUTO(2),
}

enum class LinkKind(val discriminant: Int) {
    NONE(0),
    WIFI(1),
    CELLULAR(2),
    ETHERNET(3),
    VPN(4),
    BLUETOOTH(5),
    OTHER(6),
}

enum class ChargingState(val discriminant: Int) {
    UNKNOWN(0),
    CHARGING(1),
    DISCHARGING(2),
    NOT_CHARGING(3),
    FULL(4),
}

enum class PowerSource(val discriminant: Int) {
    UNKNOWN(0),
    BATTERY(1),
    AC(2),
    USB(3),
    WIRELESS(4),
}

enum class ThermalState(val discriminant: Int) {
    NOMINAL(0),
    FAIR(1),
    SERIOUS(2),
    CRITICAL(3),
}

enum class ExternalMedium(val discriminant: Int) {
    UNKNOWN(0),
    SD_CARD(1),
    USB(2),
    NETWORK(3),
}

data class ScreenMetrics(
    val logicalWidth: Int,
    val logicalHeight: Int,
    val scale: Float,
    val refreshHz: Float?,
)

data class ExternalVolume(
    val freeBytes: Long,
    val totalBytes: Long,
    val medium: ExternalMedium,
)

private fun writeU8(out: ByteArrayOutputStream, value: Int) {
    out.write(value and 0xFF)
}

private fun writeEnum(out: ByteArrayOutputStream, discriminant: Int) {
    Bincode.writeEnumDiscriminant(out, discriminant)
}

private fun writeOptionString(out: ByteArrayOutputStream, value: String?) {
    Bincode.writeOption(out, value) { s, v -> Bincode.writeString(s, v) }
}

private fun writeOptionF32(out: ByteArrayOutputStream, value: Float?) {
    Bincode.writeOption(out, value) { s, v -> Bincode.writeF32(s, v) }
}

private fun writeOptionU32(out: ByteArrayOutputStream, value: Int?) {
    Bincode.writeOption(out, value?.toLong()) { s, v -> Bincode.writeVarintU64(s, v) }
}

private fun writeOptionU64(out: ByteArrayOutputStream, value: Long?) {
    Bincode.writeOption(out, value) { s, v -> Bincode.writeVarintU64(s, v) }
}

private fun writeCalendar(out: ByteArrayOutputStream, calendar: CalendarSystem) {
    writeEnum(out, calendar.discriminant)
    if (calendar is CalendarSystem.Other) {
        Bincode.writeString(out, calendar.identifier)
    }
}

fun IstmoRuntime.publishLocale(
    bcp47: String,
    language: String,
    region: String?,
    script: String?,
    direction: LayoutDirection,
) {
    val out = ByteArrayOutputStream()
    Bincode.writeString(out, bcp47)
    Bincode.writeString(out, language)
    writeOptionString(out, region)
    writeOptionString(out, script)
    writeEnum(out, direction.discriminant)
    nativeSubmitEarlyLatest(SYSTEM_INFO_LOCALE_CHANNEL, out.toByteArray())
}

fun IstmoRuntime.publishTimezone(
    id: String,
    offsetSeconds: Int,
    isDst: Boolean,
    abbreviation: String,
) {
    val out = ByteArrayOutputStream()
    Bincode.writeString(out, id)
    Bincode.writeVarintI64(out, offsetSeconds.toLong())
    Bincode.writeBool(out, isDst)
    Bincode.writeString(out, abbreviation)
    nativeSubmitEarlyLatest(SYSTEM_INFO_TIMEZONE_CHANNEL, out.toByteArray())
}

fun IstmoRuntime.publishFormats(
    uses24h: Boolean,
    firstDayOfWeek: Int,
    measurement: MeasurementSystem,
    temperatureUnit: TemperatureUnit,
    currencyCode: String?,
    decimalSeparator: String,
    groupSeparator: String,
    calendar: CalendarSystem,
) {
    val out = ByteArrayOutputStream()
    Bincode.writeBool(out, uses24h)
    writeU8(out, firstDayOfWeek)
    writeEnum(out, measurement.discriminant)
    writeEnum(out, temperatureUnit.discriminant)
    writeOptionString(out, currencyCode)
    Bincode.writeString(out, decimalSeparator)
    Bincode.writeString(out, groupSeparator)
    writeCalendar(out, calendar)
    nativeSubmitEarlyLatest(SYSTEM_INFO_FORMATS_CHANNEL, out.toByteArray())
}

fun IstmoRuntime.publishDeviceInfo(
    model: String?,
    manufacturer: String?,
    osName: String,
    osVersion: String?,
    arch: String,
    formFactor: FormFactor,
    cpuCores: Int,
    ramTotalBytes: Long?,
    screen: ScreenMetrics?,
    vendorId: String?,
) {
    val out = ByteArrayOutputStream()
    writeOptionString(out, model)
    writeOptionString(out, manufacturer)
    Bincode.writeString(out, osName)
    writeOptionString(out, osVersion)
    Bincode.writeString(out, arch)
    writeEnum(out, formFactor.discriminant)
    Bincode.writeVarintU64(out, cpuCores.toLong())
    writeOptionU64(out, ramTotalBytes)
    Bincode.writeOption(out, screen) { s, v ->
        Bincode.writeVarintU64(s, v.logicalWidth.toLong())
        Bincode.writeVarintU64(s, v.logicalHeight.toLong())
        Bincode.writeF32(s, v.scale)
        writeOptionF32(s, v.refreshHz)
    }
    writeOptionString(out, vendorId)
    nativeSubmitEarlyLatest(DEVICE_INFO_CHANNEL, out.toByteArray())
}

fun IstmoRuntime.publishThemePreference(
    colorScheme: ColorScheme,
    accentRgba: IntArray?,
    highContrast: Boolean,
    fontScale: Float,
    reduceMotion: Boolean,
    reduceTransparency: Boolean,
    boldText: Boolean,
    invertColors: Boolean,
    screenReaderActive: Boolean,
) {
    require(accentRgba == null || accentRgba.size == 4) {
        "accentRgba must be null or exactly 4 bytes (r,g,b,a)"
    }
    val out = ByteArrayOutputStream()
    writeEnum(out, colorScheme.discriminant)
    Bincode.writeOption(out, accentRgba) { s, v ->
        writeU8(s, v[0])
        writeU8(s, v[1])
        writeU8(s, v[2])
        writeU8(s, v[3])
    }
    Bincode.writeBool(out, highContrast)
    Bincode.writeF32(out, fontScale)
    Bincode.writeBool(out, reduceMotion)
    Bincode.writeBool(out, reduceTransparency)
    Bincode.writeBool(out, boldText)
    Bincode.writeBool(out, invertColors)
    Bincode.writeBool(out, screenReaderActive)
    nativeSubmitEarlyLatest(THEME_PREFERENCE_CHANNEL, out.toByteArray())
}

fun IstmoRuntime.publishNetworkState(
    online: Boolean,
    link: LinkKind,
    metered: Boolean,
    expensive: Boolean,
    constrained: Boolean,
) {
    val out = ByteArrayOutputStream()
    Bincode.writeBool(out, online)
    writeEnum(out, link.discriminant)
    Bincode.writeBool(out, metered)
    Bincode.writeBool(out, expensive)
    Bincode.writeBool(out, constrained)
    nativeSubmitEarlyLatest(NETWORK_STATE_CHANNEL, out.toByteArray())
}

fun IstmoRuntime.publishBatteryState(
    level: Float?,
    charging: ChargingState,
    powerSource: PowerSource,
    powerSaver: Boolean,
    thermalState: ThermalState,
) {
    val out = ByteArrayOutputStream()
    writeOptionF32(out, level)
    writeEnum(out, charging.discriminant)
    writeEnum(out, powerSource.discriminant)
    Bincode.writeBool(out, powerSaver)
    writeEnum(out, thermalState.discriminant)
    nativeSubmitEarlyLatest(BATTERY_STATE_CHANNEL, out.toByteArray())
}

fun IstmoRuntime.publishStorageInfo(
    primaryFreeBytes: Long,
    primaryTotalBytes: Long,
    external: ExternalVolume?,
) {
    val out = ByteArrayOutputStream()
    Bincode.writeVarintU64(out, primaryFreeBytes)
    Bincode.writeVarintU64(out, primaryTotalBytes)
    Bincode.writeOption(out, external) { s, v ->
        Bincode.writeVarintU64(s, v.freeBytes)
        Bincode.writeVarintU64(s, v.totalBytes)
        writeEnum(s, v.medium.discriminant)
    }
    nativeSubmitEarlyLatest(STORAGE_INFO_CHANNEL, out.toByteArray())
}

fun IstmoRuntime.publishCapabilities(
    hasBiometric: Boolean,
    hasFaceId: Boolean,
    hasFingerprint: Boolean,
    hasNfc: Boolean,
    hasHaptics: Boolean,
    hasCameraFront: Boolean,
    hasCameraBack: Boolean,
    hasGps: Boolean,
    hasBluetooth: Boolean,
    hasMicrophone: Boolean,
    hasTelephony: Boolean,
) {
    val out = ByteArrayOutputStream()
    Bincode.writeBool(out, hasBiometric)
    Bincode.writeBool(out, hasFaceId)
    Bincode.writeBool(out, hasFingerprint)
    Bincode.writeBool(out, hasNfc)
    Bincode.writeBool(out, hasHaptics)
    Bincode.writeBool(out, hasCameraFront)
    Bincode.writeBool(out, hasCameraBack)
    Bincode.writeBool(out, hasGps)
    Bincode.writeBool(out, hasBluetooth)
    Bincode.writeBool(out, hasMicrophone)
    Bincode.writeBool(out, hasTelephony)
    nativeSubmitEarlyLatest(CAPABILITIES_CHANNEL, out.toByteArray())
}
