// iOS impl of the `PermissionsBackend` protocol emitted by
// `istmo-build::generate_swift_host`.
//
// The Rust caller uses Android-style permission strings (e.g.
// `"android.permission.POST_NOTIFICATIONS"`) — this file's job is to map
// them onto the iOS SDK's per-framework authorisation APIs. iOS has no
// unified permission API; each SDK owns its own gate.
//
// Coverage today is deliberately narrow — the demo only exercises
// notifications. Additional Android permission strings should be added
// here as new plugins land; unknown ids return `.notSupported`.

import Foundation
import IstmoRuntime
import UserNotifications

public final class PermissionsBackendImpl: PermissionsBackend {

    public init() {}

    public func check(permission: String) async throws -> PermissionStatus {
        switch permission {
        case "android.permission.POST_NOTIFICATIONS":
            return await notificationStatus()
        default:
            return .notSupported
        }
    }

    public func request(permissions: [String]) async throws -> [PermissionOutcome] {
        var out: [PermissionOutcome] = []
        out.reserveCapacity(permissions.count)
        for permission in permissions {
            switch permission {
            case "android.permission.POST_NOTIFICATIONS":
                out.append(PermissionOutcome(
                    permission: permission,
                    status: await requestNotifications(),
                ))
            default:
                out.append(PermissionOutcome(permission: permission, status: .notSupported))
            }
        }
        return out
    }

    /// iOS has no "should show rationale" concept — the OS decides when to
    /// re-prompt, and denied means denied. Always `false` per contract.
    public func should_show_rationale(permission: String) async throws -> Bool {
        false
    }

    // MARK: - Notifications

    private func notificationStatus() async -> PermissionStatus {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        return Self.map(settings.authorizationStatus)
    }

    private func requestNotifications() async -> PermissionStatus {
        do {
            let granted = try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .badge, .sound])
            if granted {
                return .granted
            }
            // Second-and-later calls on a previously-denied gate resolve
            // immediately with `granted == false`; the OS never re-prompts.
            let settings = await UNUserNotificationCenter.current().notificationSettings()
            return Self.map(settings.authorizationStatus)
        } catch {
            // The docs list very few conditions that throw here (e.g. an
            // app extension calling it). Report as denied so the caller
            // still gets a stable outcome.
            NSLog("PermissionsBackendImpl: requestAuthorization threw: \(error)")
            return .denied
        }
    }

    private static func map(_ status: UNAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .authorized, .provisional, .ephemeral:
            return .granted
        case .denied:
            return .permanentlyDenied
        case .notDetermined:
            return .notDetermined
        @unknown default:
            return .notSupported
        }
    }
}
