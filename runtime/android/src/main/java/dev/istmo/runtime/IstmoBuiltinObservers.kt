package dev.istmo.runtime

import android.app.ActivityManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.BatteryManager
import android.os.Build
import android.os.PowerManager
import android.os.StatFs
import android.os.storage.StorageManager
import android.provider.Settings
import android.view.accessibility.AccessibilityManager
import java.util.Calendar
import java.util.Locale as JvmLocale
import java.util.TimeZone as JvmTimeZone

/**
 * Native observers that bridge Android system events to the typed
 * `publishXxx` helpers. Each observer registers lazily on
 * [register] and tears down on [unregister] — call sites are
 * typically `Application.onCreate` / `onTerminate` or the main
 * activity's lifecycle callbacks.
 *
 * `start(context)` on [IstmoBuiltinObservers] wires every observer
 * at once; opt-out by instantiating individual observers directly.
 */

/** One-shot `publishDeviceInfo` from `Build.*` + `ActivityManager`. */
object IstmoDeviceInfoObserver {
    fun publishOnce(context: Context) {
        val activityManager =
            context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
        val memInfo = activityManager?.let {
            val info = ActivityManager.MemoryInfo()
            it.getMemoryInfo(info)
            info
        }
        val ramTotal = memInfo?.totalMem?.takeIf { it > 0 }
        val config = context.resources.configuration
        val uiMask = config.uiMode and Configuration.UI_MODE_TYPE_MASK
        val formFactor = when (uiMask) {
            Configuration.UI_MODE_TYPE_WATCH -> FormFactor.WATCH
            Configuration.UI_MODE_TYPE_TELEVISION, Configuration.UI_MODE_TYPE_APPLIANCE ->
                FormFactor.TV
            else -> if (
                config.smallestScreenWidthDp >= 600
            ) FormFactor.TABLET else FormFactor.PHONE
        }
        val metrics = context.resources.displayMetrics
        val screen = ScreenMetrics(
            logicalWidth = (metrics.widthPixels / metrics.density).toInt(),
            logicalHeight = (metrics.heightPixels / metrics.density).toInt(),
            scale = metrics.density,
            refreshHz = null,
        )
        val vendorId: String? = try {
            Settings.Secure.getString(context.contentResolver, Settings.Secure.ANDROID_ID)
        } catch (_: SecurityException) {
            null
        }
        IstmoRuntime.publishDeviceInfo(
            model = Build.MODEL,
            manufacturer = Build.MANUFACTURER,
            osName = "android",
            osVersion = Build.VERSION.RELEASE,
            arch = Build.SUPPORTED_ABIS.firstOrNull() ?: "unknown",
            formFactor = formFactor,
            cpuCores = Runtime.getRuntime().availableProcessors(),
            ramTotalBytes = ramTotal,
            screen = screen,
            vendorId = vendorId,
        )
    }
}

/** One-shot `publishCapabilities` from `PackageManager`. */
object IstmoCapabilitiesObserver {
    fun publishOnce(context: Context) {
        val pm = context.packageManager
        fun has(feature: String) = pm.hasSystemFeature(feature)
        IstmoRuntime.publishCapabilities(
            hasBiometric = has(PackageManager.FEATURE_FINGERPRINT) ||
                (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q &&
                    has(PackageManager.FEATURE_FACE)),
            hasFaceId = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q &&
                has(PackageManager.FEATURE_FACE),
            hasFingerprint = has(PackageManager.FEATURE_FINGERPRINT),
            hasNfc = has(PackageManager.FEATURE_NFC),
            hasHaptics = (context.getSystemService(Context.VIBRATOR_SERVICE) as? android.os.Vibrator)
                ?.hasVibrator() == true,
            hasCameraFront = has(PackageManager.FEATURE_CAMERA_FRONT),
            hasCameraBack = has(PackageManager.FEATURE_CAMERA),
            hasGps = has(PackageManager.FEATURE_LOCATION_GPS),
            hasBluetooth = has(PackageManager.FEATURE_BLUETOOTH),
            hasMicrophone = has(PackageManager.FEATURE_MICROPHONE),
            hasTelephony = has(PackageManager.FEATURE_TELEPHONY),
        )
    }
}

/**
 * `publishLocale` / `publishTimezone` / `publishFormats` on
 * `ACTION_LOCALE_CHANGED`, `ACTION_TIMEZONE_CHANGED` and
 * `ACTION_TIME_CHANGED`.
 */
class IstmoSystemInfoObserver(private val context: Context) {
    private var receiver: BroadcastReceiver? = null

    fun register() {
        if (receiver != null) return
        val r = object : BroadcastReceiver() {
            override fun onReceive(c: Context, intent: Intent) {
                publishAll()
            }
        }
        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_LOCALE_CHANGED)
            addAction(Intent.ACTION_TIMEZONE_CHANGED)
            addAction(Intent.ACTION_TIME_CHANGED)
        }
        context.registerReceiver(r, filter)
        receiver = r
        publishAll()
    }

    fun unregister() {
        receiver?.let { context.unregisterReceiver(it) }
        receiver = null
    }

    private fun publishAll() {
        publishLocale()
        publishTimezone()
        publishFormats()
    }

    private fun publishLocale() {
        val l = JvmLocale.getDefault()
        val tag = l.toLanguageTag()
        val direction = when (l.language.lowercase()) {
            "ar", "arc", "ckb", "dv", "fa", "ha", "he", "khw", "ks", "ps", "sd",
            "ug", "ur", "yi" -> LayoutDirection.RTL
            else -> LayoutDirection.LTR
        }
        IstmoRuntime.publishLocale(
            bcp47 = tag,
            language = l.language.lowercase(),
            region = l.country.takeIf { it.isNotEmpty() },
            script = l.script.takeIf { it.isNotEmpty() },
            direction = direction,
        )
    }

    private fun publishTimezone() {
        val tz = JvmTimeZone.getDefault()
        val now = System.currentTimeMillis()
        val offsetMs = tz.getOffset(now)
        IstmoRuntime.publishTimezone(
            id = tz.id,
            offsetSeconds = offsetMs / 1000,
            isDst = tz.inDaylightTime(java.util.Date(now)),
            abbreviation = tz.getDisplayName(
                tz.inDaylightTime(java.util.Date(now)),
                JvmTimeZone.SHORT,
                JvmLocale.US,
            ),
        )
    }

    private fun publishFormats() {
        val l = JvmLocale.getDefault()
        val uses24h = android.text.format.DateFormat.is24HourFormat(context)
        val cal = Calendar.getInstance(l)
        val apple = cal.firstDayOfWeek
        val isoFirstDay = if (apple == Calendar.SUNDAY) 7 else apple - 1
        val region = l.country.uppercase()
        val measurement = when (region) {
            "US", "LR", "MM" -> MeasurementSystem.US_CUSTOMARY
            "GB" -> MeasurementSystem.UK_IMPERIAL
            else -> MeasurementSystem.METRIC
        }
        val tempUnit = when (region) {
            "US", "BS", "BZ", "KY", "LR", "PW", "FM", "MH", "PR", "GU", "VI", "AS", "MP" ->
                TemperatureUnit.FAHRENHEIT
            else -> TemperatureUnit.CELSIUS
        }
        val currency = try {
            java.util.Currency.getInstance(l).currencyCode
        } catch (_: Exception) {
            null
        }
        val symbols = java.text.DecimalFormatSymbols.getInstance(l)
        IstmoRuntime.publishFormats(
            uses24h = uses24h,
            firstDayOfWeek = isoFirstDay,
            measurement = measurement,
            temperatureUnit = tempUnit,
            currencyCode = currency,
            decimalSeparator = symbols.decimalSeparator.toString(),
            groupSeparator = symbols.groupingSeparator.toString(),
            calendar = CalendarSystem.Gregorian,
        )
    }
}

/**
 * `publishThemePreference` on `AccessibilityManager` state changes
 * and `Configuration` updates. Font scale from
 * `Configuration.fontScale`, color scheme from `uiMode & NIGHT_MASK`.
 */
class IstmoThemePreferenceObserver(private val context: Context) {
    private var listener: AccessibilityManager.AccessibilityStateChangeListener? = null
    private val am =
        context.getSystemService(Context.ACCESSIBILITY_SERVICE) as AccessibilityManager

    fun register() {
        if (listener != null) return
        val l = AccessibilityManager.AccessibilityStateChangeListener { publish() }
        am.addAccessibilityStateChangeListener(l)
        listener = l
        publish()
    }

    fun unregister() {
        listener?.let { am.removeAccessibilityStateChangeListener(it) }
        listener = null
    }

    fun publish() {
        val config = context.resources.configuration
        val night = config.uiMode and Configuration.UI_MODE_NIGHT_MASK
        val scheme = when (night) {
            Configuration.UI_MODE_NIGHT_YES -> ColorScheme.DARK
            Configuration.UI_MODE_NIGHT_NO -> ColorScheme.LIGHT
            else -> ColorScheme.AUTO
        }
        val screenReader = am.isTouchExplorationEnabled || am.isEnabled
        val animScale = try {
            Settings.Global.getFloat(
                context.contentResolver,
                Settings.Global.ANIMATOR_DURATION_SCALE,
            )
        } catch (_: Settings.SettingNotFoundException) {
            1.0f
        }
        val boldText = try {
            Settings.Secure.getInt(context.contentResolver, "font_weight_adjustment", 0) > 0
        } catch (_: Settings.SettingNotFoundException) {
            false
        }
        val invert = try {
            Settings.Secure.getInt(
                context.contentResolver,
                Settings.Secure.ACCESSIBILITY_DISPLAY_INVERSION_ENABLED,
                0,
            ) == 1
        } catch (_: Settings.SettingNotFoundException) {
            false
        }
        val highContrast = try {
            Settings.Secure.getInt(
                context.contentResolver,
                "high_text_contrast_enabled",
                0,
            ) == 1
        } catch (_: Settings.SettingNotFoundException) {
            false
        }
        IstmoRuntime.publishThemePreference(
            colorScheme = scheme,
            accentRgba = null,
            highContrast = highContrast,
            fontScale = config.fontScale,
            reduceMotion = animScale == 0f,
            reduceTransparency = false,
            boldText = boldText,
            invertColors = invert,
            screenReaderActive = screenReader,
        )
    }
}

/**
 * `publishNetworkState` on `ConnectivityManager.NetworkCallback`
 * events. Requires `<uses-permission
 * android:name="android.permission.ACCESS_NETWORK_STATE" />` —
 * provided automatically by the permission pipeline when the app
 * wires `NetworkStateClient`.
 */
class IstmoNetworkStateObserver(private val context: Context) {
    private val cm =
        context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
    private var callback: ConnectivityManager.NetworkCallback? = null

    fun register() {
        if (callback != null) return
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = publish()
            override fun onLost(network: Network) = publish()
            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) =
                publish()
        }
        val request = NetworkRequest.Builder()
            .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_RESTRICTED)
            .build()
        cm.registerNetworkCallback(request, cb)
        callback = cb
        publish()
    }

    fun unregister() {
        callback?.let { cm.unregisterNetworkCallback(it) }
        callback = null
    }

    private fun publish() {
        val caps = cm.activeNetwork?.let { cm.getNetworkCapabilities(it) }
        val online = caps?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) == true
        val link = when {
            caps == null -> LinkKind.NONE
            caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> LinkKind.WIFI
            caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> LinkKind.CELLULAR
            caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> LinkKind.ETHERNET
            caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN) -> LinkKind.VPN
            caps.hasTransport(NetworkCapabilities.TRANSPORT_BLUETOOTH) -> LinkKind.BLUETOOTH
            else -> LinkKind.OTHER
        }
        val metered = cm.isActiveNetworkMetered
        val expensive = metered &&
            caps?.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) == true
        val constrained = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R &&
            caps?.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_RESTRICTED) == false
        IstmoRuntime.publishNetworkState(
            online = online,
            link = link,
            metered = metered,
            expensive = expensive,
            constrained = constrained,
        )
    }
}

/**
 * `publishBatteryState` on `ACTION_BATTERY_CHANGED` /
 * `ACTION_POWER_SAVE_MODE_CHANGED` broadcasts. Thermal state via
 * `PowerManager.currentThermalStatus` (API 29+).
 */
class IstmoBatteryStateObserver(private val context: Context) {
    private var receiver: BroadcastReceiver? = null
    private val pm = context.getSystemService(Context.POWER_SERVICE) as PowerManager

    fun register() {
        if (receiver != null) return
        val r = object : BroadcastReceiver() {
            override fun onReceive(c: Context, intent: Intent) = publish(intent)
        }
        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_BATTERY_CHANGED)
            addAction(PowerManager.ACTION_POWER_SAVE_MODE_CHANGED)
        }
        context.registerReceiver(r, filter)
        receiver = r
        // Sticky broadcast — read current state immediately.
        @Suppress("DEPRECATION")
        val sticky = context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        publish(sticky)
    }

    fun unregister() {
        receiver?.let { context.unregisterReceiver(it) }
        receiver = null
    }

    private fun publish(intent: Intent?) {
        val levelInt = intent?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
        val scaleInt = intent?.getIntExtra(BatteryManager.EXTRA_SCALE, -1) ?: -1
        val level = if (levelInt >= 0 && scaleInt > 0) levelInt.toFloat() / scaleInt else null
        val status = intent?.getIntExtra(BatteryManager.EXTRA_STATUS, BatteryManager.BATTERY_STATUS_UNKNOWN)
            ?: BatteryManager.BATTERY_STATUS_UNKNOWN
        val charging = when (status) {
            BatteryManager.BATTERY_STATUS_CHARGING -> ChargingState.CHARGING
            BatteryManager.BATTERY_STATUS_DISCHARGING -> ChargingState.DISCHARGING
            BatteryManager.BATTERY_STATUS_NOT_CHARGING -> ChargingState.NOT_CHARGING
            BatteryManager.BATTERY_STATUS_FULL -> ChargingState.FULL
            else -> ChargingState.UNKNOWN
        }
        val plugged = intent?.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) ?: 0
        val source = when (plugged) {
            BatteryManager.BATTERY_PLUGGED_AC -> PowerSource.AC
            BatteryManager.BATTERY_PLUGGED_USB -> PowerSource.USB
            BatteryManager.BATTERY_PLUGGED_WIRELESS -> PowerSource.WIRELESS
            0 -> PowerSource.BATTERY
            else -> PowerSource.UNKNOWN
        }
        val thermal = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            when (pm.currentThermalStatus) {
                PowerManager.THERMAL_STATUS_NONE, PowerManager.THERMAL_STATUS_LIGHT ->
                    ThermalState.NOMINAL
                PowerManager.THERMAL_STATUS_MODERATE -> ThermalState.FAIR
                PowerManager.THERMAL_STATUS_SEVERE -> ThermalState.SERIOUS
                PowerManager.THERMAL_STATUS_CRITICAL,
                PowerManager.THERMAL_STATUS_EMERGENCY,
                PowerManager.THERMAL_STATUS_SHUTDOWN -> ThermalState.CRITICAL
                else -> ThermalState.NOMINAL
            }
        } else ThermalState.NOMINAL
        IstmoRuntime.publishBatteryState(
            level = level,
            charging = charging,
            powerSource = source,
            powerSaver = pm.isPowerSaveMode,
            thermalState = thermal,
        )
    }
}

/**
 * `publishStorageInfo` sampled from the internal `filesDir` via
 * `StatFs`. Polls once at [publishOnce] — call again on low-storage
 * broadcasts or app resume.
 */
object IstmoStorageInfoObserver {
    fun publishOnce(context: Context) {
        val stat = StatFs(context.filesDir.path)
        val blockSize = stat.blockSizeLong
        val primaryFree = stat.availableBlocksLong * blockSize
        val primaryTotal = stat.blockCountLong * blockSize
        val external: ExternalVolume? = if (
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.N
        ) {
            try {
                val sm =
                    context.getSystemService(Context.STORAGE_SERVICE) as StorageManager
                val uuid = StorageManager.UUID_DEFAULT
                val free = sm.getAllocatableBytes(uuid)
                val total = sm.getCacheQuotaBytes(uuid)
                if (total > 0) {
                    ExternalVolume(
                        freeBytes = free,
                        totalBytes = total,
                        medium = ExternalMedium.UNKNOWN,
                    )
                } else null
            } catch (_: Exception) {
                null
            }
        } else null
        IstmoRuntime.publishStorageInfo(
            primaryFreeBytes = primaryFree,
            primaryTotalBytes = primaryTotal,
            external = external,
        )
    }
}

/**
 * Convenience bundle: wires every observer at once. Call once from
 * `Application.onCreate` (after [IstmoRuntime.start]) and
 * [unregisterAll] from `onTerminate`.
 */
class IstmoBuiltinObservers(private val context: Context) {
    private val systemInfo = IstmoSystemInfoObserver(context)
    private val theme = IstmoThemePreferenceObserver(context)
    private val network = IstmoNetworkStateObserver(context)
    private val battery = IstmoBatteryStateObserver(context)

    fun registerAll() {
        IstmoDeviceInfoObserver.publishOnce(context)
        IstmoCapabilitiesObserver.publishOnce(context)
        IstmoStorageInfoObserver.publishOnce(context)
        systemInfo.register()
        theme.register()
        network.register()
        battery.register()
    }

    fun unregisterAll() {
        systemInfo.unregister()
        theme.unregister()
        network.unregister()
        battery.unregister()
    }
}
