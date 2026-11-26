package dev.istmo.plugins.launcher_actions

import android.content.Context
import android.content.Intent
import android.content.pm.ShortcutInfo
import android.content.pm.ShortcutManager
import android.graphics.BitmapFactory
import android.graphics.drawable.Icon
import android.net.Uri
import android.os.Build
import android.annotation.TargetApi
import dev.istmo.runtime.BackendException

/**
 * Reference backend for `istmo.launcher_actions` on `ShortcutManager`.
 *
 * Dynamic actions are dynamic shortcuts (API 25+), pinning is
 * `requestPinShortcut` (API 26+). Every shortcut opens
 * [IstmoLauncherActionActivity], which turns it into a deep link. Badge,
 * progress, attention and recent documents have no launcher API on
 * Android (notification dots are the notifications' business) and
 * answer `LauncherError.Unsupported`.
 */
// Every ShortcutManager call is behind `shortcuts`, non-null only on API 25+.
@TargetApi(Build.VERSION_CODES.O)
class LauncherActionsBackendImpl(private val context: Context) : LauncherActionsBackend {

    private val shortcuts: ShortcutManager? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N_MR1) {
            context.getSystemService(ShortcutManager::class.java)
        } else {
            null
        }

    override suspend fun capabilities(): Capabilities {
        val manager = shortcuts
        return Capabilities(
            dynamicActions = manager != null,
            maxActions = manager?.let { maxDynamic(it).toUInt() } ?: 0u,
            pin = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && manager?.isRequestPinShortcutSupported == true,
            badge = BadgeSupport.Unsupported,
            progress = false,
            attention = false,
            recentDocuments = false,
        )
    }

    override suspend fun set_actions(actions: List<LauncherAction>) {
        val manager = requireManager()
        val max = maxDynamic(manager)
        if (actions.size > max) throw BackendException(LauncherError.TooMany(max.toUInt()))
        if (manager.isRateLimitingActive) throw BackendException(LauncherError.RateLimited)
        val infos = actions.mapIndexed { rank, action -> shortcut(action, rank) }
        if (!manager.setDynamicShortcuts(infos)) throw BackendException(LauncherError.RateLimited)
    }

    override suspend fun clear_actions() {
        requireManager().removeAllDynamicShortcuts()
    }

    override suspend fun report_used(id: String) {
        requireManager().reportShortcutUsed(id)
    }

    override suspend fun request_pin(action: LauncherAction): PinOutcome {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) throw BackendException(LauncherError.Unsupported)
        val manager = requireManager()
        val info = shortcut(action, 0)
        if (manager.pinnedShortcuts.any { it.id == action.id }) {
            if (!manager.updateShortcuts(listOf(info))) throw BackendException(LauncherError.RateLimited)
            return PinOutcome.AlreadyPinned
        }
        if (!manager.isRequestPinShortcutSupported) throw BackendException(LauncherError.Unsupported)
        if (!manager.requestPinShortcut(info, null)) {
            throw BackendException(LauncherError.Backend("the launcher refused the pin request"))
        }
        return PinOutcome.Requested
    }

    override suspend fun set_badge(badge: Badge) {
        unsupported()
    }

    override suspend fun set_progress(progress: Progress) {
        unsupported()
    }

    override suspend fun request_attention(attention: Attention) {
        unsupported()
    }

    override suspend fun add_recent_document(path: String) {
        unsupported()
    }

    override suspend fun clear_recent_documents() {
        unsupported()
    }

    private fun unsupported(): Nothing = throw BackendException(LauncherError.Unsupported)

    private fun requireManager(): ShortcutManager =
        shortcuts ?: throw BackendException(LauncherError.Unsupported)

    /** Dynamic slots: the per-activity limit minus the static shortcuts. */
    @TargetApi(Build.VERSION_CODES.N_MR1)
    private fun maxDynamic(manager: ShortcutManager): Int =
        (manager.maxShortcutCountPerActivity - manager.manifestShortcuts.size).coerceAtLeast(0)

    @TargetApi(Build.VERSION_CODES.N_MR1)
    private fun shortcut(action: LauncherAction, rank: Int): ShortcutInfo {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(action.url))
            .setClassName(context, IstmoLauncherActionActivity::class.java.name)
            .putExtra(IstmoLauncherActionActivity.EXTRA_ACTION_ID, action.id)
        val builder = ShortcutInfo.Builder(context, action.id)
            .setShortLabel(action.title)
            .setLongLabel(action.subtitle ?: action.title)
            .setIntent(intent)
            .setRank(rank)
        val icon = action.icon
        if (icon is ActionIcon.Png) {
            val bitmap = BitmapFactory.decodeByteArray(icon.value, 0, icon.value.size)
                ?: throw BackendException(
                    LauncherError.InvalidAction(ActionProblem(action.id, "icon does not decode")),
                )
            builder.setIcon(Icon.createWithBitmap(bitmap))
        }
        return builder.build()
    }
}
