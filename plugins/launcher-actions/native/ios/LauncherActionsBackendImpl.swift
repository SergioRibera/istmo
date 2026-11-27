import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
#endif
#if canImport(UserNotifications)
import UserNotifications
#endif

/// Reference backend for `istmo.launcher_actions`: Home Screen quick
/// actions (`UIApplication.shortcutItems`) and the icon badge.
///
/// Every quick action carries its URL in `userInfo["url"]`; the app hands
/// the item to ``IstmoLauncherActions/perform(_:)``, which publishes the
/// deep link. Pinning, progress, attention and recent documents have no
/// iOS API and throw `LauncherActions.LauncherError.unsupported`.
public final class LauncherActionsBackendImpl: LauncherActionsBackend {
    /// Quick actions iOS shows, static and dynamic together.
    public static let shownActions = 4

    public init() {}

    public func capabilities() async throws -> LauncherActions.Capabilities {
        LauncherActions.Capabilities(
            dynamicActions: true,
            maxActions: UInt32(Self.dynamicSlots()),
            pin: false,
            badge: .count,
            progress: false,
            attention: false,
            recentDocuments: false
        )
    }

    public func set_actions(actions: [LauncherActions.LauncherAction]) async throws {
        let max = Self.dynamicSlots()
        guard actions.count <= max else { throw LauncherActions.LauncherError.tooMany(UInt32(max)) }
        #if canImport(UIKit)
        await MainActor.run {
            UIApplication.shared.shortcutItems = actions.map(Self.item)
        }
        #else
        throw LauncherActions.LauncherError.unsupported
        #endif
    }

    public func clear_actions() async throws {
        #if canImport(UIKit)
        await MainActor.run { UIApplication.shared.shortcutItems = [] }
        #else
        throw LauncherActions.LauncherError.unsupported
        #endif
    }

    /// iOS does not rank quick actions by use.
    public func report_used(id: String) async throws {}

    public func request_pin(action: LauncherActions.LauncherAction) async throws -> LauncherActions.PinOutcome {
        throw LauncherActions.LauncherError.unsupported
    }

    public func set_badge(badge: LauncherActions.Badge) async throws {
        let count: Int
        switch badge {
        case .clear: count = 0
        case let .count(n): count = Int(n)
        case .label, .unknown: throw LauncherActions.LauncherError.unsupported
        }
        #if canImport(UIKit) && canImport(UserNotifications)
        // The badge is a notification permission: without it iOS drops
        // the number silently, so say so instead.
        let center = UNUserNotificationCenter.current()
        let allowed: Bool = await withCheckedContinuation { cont in
            center.getNotificationSettings { settings in
                cont.resume(returning: settings.badgeSetting == .enabled)
            }
        }
        guard allowed else { throw LauncherActions.LauncherError.notPermitted }
        if #available(iOS 16.0, *) {
            do {
                try await center.setBadgeCount(count)
            } catch {
                throw LauncherActions.LauncherError.backend(error.localizedDescription)
            }
        } else {
            await MainActor.run { UIApplication.shared.applicationIconBadgeNumber = count }
        }
        #else
        _ = count
        throw LauncherActions.LauncherError.unsupported
        #endif
    }

    public func set_progress(progress: LauncherActions.Progress) async throws {
        throw LauncherActions.LauncherError.unsupported
    }

    public func request_attention(attention: LauncherActions.Attention) async throws {
        throw LauncherActions.LauncherError.unsupported
    }

    public func add_recent_document(path: String) async throws {
        throw LauncherActions.LauncherError.unsupported
    }

    public func clear_recent_documents() async throws {
        throw LauncherActions.LauncherError.unsupported
    }

    /// Slots left for dynamic actions after the static ones in Info.plist.
    static func dynamicSlots() -> Int {
        #if canImport(UIKit)
        let declared = Bundle.main.object(forInfoDictionaryKey: "UIApplicationShortcutItems") as? [Any]
        return max(0, shownActions - (declared?.count ?? 0))
        #else
        return shownActions
        #endif
    }

    #if canImport(UIKit)
    static func item(_ action: LauncherActions.LauncherAction) -> UIApplicationShortcutItem {
        var icon: UIApplicationShortcutIcon?
        if case let .symbol(name)? = action.icon, #available(iOS 13.0, *) {
            icon = UIApplicationShortcutIcon(systemImageName: name)
        }
        return UIApplicationShortcutItem(
            type: action.id,
            localizedTitle: action.title,
            localizedSubtitle: action.subtitle,
            icon: icon,
            userInfo: [IstmoLauncherActions.urlKey: action.url as NSString]
        )
    }
    #endif
}
