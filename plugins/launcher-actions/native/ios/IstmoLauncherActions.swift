import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
#endif

/// App side of Home Screen quick actions.
///
/// iOS gives a library no way to observe quick actions: they reach the
/// scene delegate. Forward each one here and it is published as a
/// `DeepLink` on the `istmo.deeplinks` queue (buffered until Rust
/// subscribes), with `source = "launcher-action:<type>"`:
///
/// ```swift
/// func windowScene(_ scene: UIWindowScene,
///                  performActionFor item: UIApplicationShortcutItem,
///                  completionHandler: @escaping (Bool) -> Void) {
///     completionHandler(IstmoLauncherActions.perform(item))
/// }
///
/// func scene(_ scene: UIScene, willConnectTo session: UISceneSession,
///            options: UIScene.ConnectionOptions) {
///     if let item = options.shortcutItem { IstmoLauncherActions.perform(item) }
/// }
/// ```
public enum IstmoLauncherActions {
    /// `userInfo` key holding the action's URL (static and dynamic items).
    public static let urlKey = "url"

    #if canImport(UIKit)
    /// Publish `item` as a deep link. `false` when it carries no URL (not
    /// an istmo launcher action).
    @discardableResult
    public static func perform(_ item: UIApplicationShortcutItem) -> Bool {
        guard let url = item.userInfo?[urlKey] as? String, !url.isEmpty else { return false }
        publish(url: url, id: item.type)
        return true
    }
    #endif

    /// Publish `url` as the deep link of the action `id`.
    public static func publish(url: String, id: String) {
        let now = UInt64(max(0, Date().timeIntervalSince1970 * 1000))
        IstmoRuntime.shared.publishEarlyQueue(
            channel: LauncherActionLink.deepLinksChannel,
            capacity: LauncherActionLink.deepLinksCapacity,
            payload: LauncherActionLink.encode(
                uri: url,
                source: LauncherActionLink.sourcePrefix + id,
                receivedAtMs: now
            )
        )
    }
}
