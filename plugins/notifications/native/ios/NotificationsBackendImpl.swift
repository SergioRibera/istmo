import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
#endif
import UserNotifications

/// The iOS backend of `istmo.notifications`, over `UNUserNotificationCenter`.
///
/// Native: show, replace, actions (4), inline reply, pictures and icons
/// as attachments, threads, interruption levels, relevance, sounds,
/// scheduling (with the 64-request top-up), delivered / pending lists,
/// dismissals (custom dismiss action), taps when closed, background
/// actions, communication notifications (iOS 15+, with the entitlement).
/// Simulated: silent updates (passive, no sound, list only in the
/// foreground), progress as text, choices as buttons, snooze, channels,
/// lock-screen privacy (category placeholder), call style (buttons),
/// timeouts (while running). The app badge is never touched.
///
/// Every input is validated like the Rust host (`NotificationsValidate`,
/// the port of `src/validate.rs`); posts fail with `PermissionDenied`
/// while the user denies notifications; calls for one id run in order in
/// its lane, so a cancel lands after the post before it and a queued
/// update never brings a cancelled notification back.
public final class NotificationsBackendImpl: NotificationsBackend {
    private let core = NotificationsCore.shared

    public init() {
        IstmoNotifications.install()
    }

    // MARK: capabilities and permission

    public func capabilities() async throws -> Notifications.Capabilities {
        var features: [Notifications.FeatureSupport] = []
        func add(_ support: Notifications.Support, _ list: [Notifications.Feature]) {
            features += list.map { Notifications.FeatureSupport(feature: $0, support: support) }
        }
        let messaging: Notifications.Support = core.communication ? .native : .simulated
        add(.native, [.show, .update, .schedule, .repeat, .missed, .actions, .inlineReply, .picture, .groups,
                      .customSound, .interruption, .listDelivered, .listPending, .dismissal, .tapWhenClosed,
                      .backgroundAction, .permissionRequest, .openSettings])
        add(messaging, [.messaging, .avatar])
        add(.simulated, [.silentUpdate, .progressBar, .choices, .snooze, .channels, .lockScreen, .callStyle])
        add(.whileRunning, [.timeout])
        add(.unsupported, [.livePromotion, .ongoing, .chronometer, .exactAlarms, .doNotDisturb])
        return Notifications.Capabilities(
            backend: "ios", features: features, maxActions: UInt32(NotificationsCore.maxActions),
            maxPending: 64, maxActive: nil)
    }

    public func permission() async throws -> Notifications.PermissionStatus {
        let settings = await core.center.notificationSettings()
        // `UNAuthorizationStatus` is an open enum: compared, not switched.
        let status = settings.authorizationStatus
        core.noteAuthorization(denied: status == .denied)
        let state: Notifications.PermissionState
        if status == .authorized {
            state = .granted
        } else if status == .denied {
            state = .denied
        } else if status == .provisional {
            state = .provisional
        } else if status == .ephemeral {
            state = .ephemeral
        } else {
            state = .notDetermined
        }
        var timeSensitive: Bool?
        var communication: Bool?
        if #available(iOS 15.0, *) {
            timeSensitive = settings.timeSensitiveSetting == .enabled
            communication = settings.directMessagesSetting == .enabled
        }
        return Notifications.PermissionStatus(
            state: state, alerts: settings.alertSetting == .enabled, sound: settings.soundSetting == .enabled,
            badge: settings.badgeSetting == .enabled, exactAlarms: nil, fullScreen: nil, promoted: nil,
            timeSensitive: timeSensitive, communication: communication, doNotDisturb: nil)
    }

    public func request_permission(request: Notifications.PermissionRequest) async throws
        -> Notifications.PermissionStatus
    {
        var options: UNAuthorizationOptions = []
        if request.alerts { options.insert(.alert) }
        if request.sound { options.insert(.sound) }
        if request.badge { options.insert(.badge) }
        if request.provisional { options.insert(.provisional) }
        if request.critical { options.insert(.criticalAlert) }
        do {
            _ = try await core.center.requestAuthorization(options: options)
        } catch {
            throw Self.platform(error)
        }
        return try await permission()
    }

    public func open_settings(channel: Notifications.ChannelId?) async throws {
        #if canImport(UIKit)
        let link: String
        if #available(iOS 16.0, *) {
            link = UIApplication.openNotificationSettingsURLString
        } else {
            link = UIApplication.openSettingsURLString
        }
        guard let url = URL(string: link) else { throw Notifications.NotificationError.unsupported(.openSettings) }
        await MainActor.run { UIApplication.shared.open(url, options: [:], completionHandler: nil) }
        #else
        throw Notifications.NotificationError.unsupported(.openSettings)
        #endif
    }

    // MARK: channels (simulated: defaults merged into each notification)

    public func set_channels(channels: [Notifications.ChannelSpec]) async throws {
        for (i, spec) in channels.enumerated() { try NotificationsValidate.check(spec, index: i) }
        core.lock.guarded {
            for spec in channels {
                if let old = core.channels[spec.id.value] {
                    // Settings stay as created; only the texts change (Android's rule).
                    core.channels[spec.id.value] = Notifications.ChannelSpec(
                        id: old.id, name: spec.name, description: spec.description, importance: old.importance,
                        sound: old.sound, vibration: old.vibration, lightColor: old.lightColor,
                        showBadge: old.showBadge, lockScreen: old.lockScreen, bypassDnd: old.bypassDnd,
                        group: old.group)
                } else {
                    core.channels[spec.id.value] = spec
                }
            }
        }
    }

    public func delete_channel(id: Notifications.ChannelId) async throws {
        core.lock.guarded { _ = core.channels.removeValue(forKey: id.value) }
    }

    private func merged(_ n: Notifications.Notification) throws -> Notifications.Notification {
        guard let channel = n.channel else { return n }
        guard let spec = core.lock.guarded({ core.channels[channel.value] }) else {
            throw Notifications.NotificationError.unknownChannel(channel.value)
        }
        return n.merging(spec)
    }

    // MARK: showing

    /// Validation (`src/validate.rs`), normalisation, channel defaults:
    /// what the desktop host does before a show or a schedule.
    private func prepared(_ notification: Notifications.Notification) throws -> Notifications.Notification {
        try NotificationsValidate.check(notification)
        return try merged(notification.normalized())
    }

    public func show(notification: Notifications.Notification) async throws {
        let n = try prepared(notification)
        try await core.lanes.run(n.id.value) {
            try await post(n, quiet: false, donate: true)
            let generation = core.bumpGeneration(n.id.value)
            core.lock.guarded {
                core.live[n.id.value] = n
                core.coalescer.shown(n.id.value, nowMs: nowMs())
            }
            armTimeout(n, generation: generation)
        }
        core.emit(.delivered(n.id))
    }

    /// Adds (or replaces) the request of `n` with no trigger; called in
    /// its lane. A denied permission fails with `PermissionDenied`.
    private func post(_ n: Notifications.Notification, quiet: Bool, donate: Bool) async throws {
        if await core.postsDenied() { throw Notifications.NotificationError.permissionDenied }
        let rendered = NotificationsRender.render(n, core.limits(for: n))
        let prepared = await core.content(for: n, rendered: rendered, quiet: quiet, donate: donate)
        do {
            try await core.center.add(UNNotificationRequest(identifier: n.id.value, content: prepared.content,
                                                            trigger: nil))
        } catch {
            prepared.discardFiles()
            throw Self.platform(error)
        }
    }

    /// The host-run timeout (`Timeout` is `WhileRunning` on Apple), for
    /// the show of `generation`: a later show of the same id re-arms its
    /// own.
    private func armTimeout(_ n: Notifications.Notification, generation: UInt64) {
        guard let ms = n.timeoutMs else { return }
        let id = n.id.value
        DispatchQueue.global().asyncAfter(deadline: .now() + Double(ms) / 1000) { [core] in
            let still: Bool = core.lock.guarded {
                guard core.live[id] != nil, core.generations[id] == generation else { return false }
                core.live.removeValue(forKey: id)
                core.coalescer.cancel(id)
                core.generations.removeValue(forKey: id)
                return true
            }
            guard still else { return }
            Task {
                await core.lanes.run(id) { await core.removeDelivered([id]) }
                core.emit(.dismissed(Notifications.Dismissal(id: Notifications.NotificationId(value: id),
                                                             reason: .timeout)))
            }
        }
    }

    public func schedule(notification: Notifications.Notification, trigger: Notifications.Trigger) async throws
        -> Notifications.ScheduledInfo
    {
        let n = try prepared(notification)
        try NotificationsValidate.check(trigger)
        // A past `At` is delivered at once (`Trigger::first_fire`) and
        // replaces what was scheduled under the id.
        if case .at(let ms) = trigger, ms <= nowMs() {
            await core.store.remove(n.id.value)
            try await show(notification: n)
            return Notifications.ScheduledInfo(exact: true, nextFireMs: nowMs())
        }
        return try await core.store.add(n, trigger)
    }

    // MARK: live updates

    public func update(id: Notifications.NotificationId, patch: Notifications.NotificationPatch) async throws {
        try NotificationsValidate.check(patch)
        try await change(id, patch.alertAgain ? .alert : .regular) { n in
            // The whole notification: a patch alone cannot tell (call
            // actions need a call style, fields depend on each other).
            let next = n.applying(patch)
            try NotificationsValidate.check(next)
            return next
        }
    }

    public func set_progress(id: Notifications.NotificationId, progress: Notifications.Progress?) async throws {
        if let bar = progress { try NotificationsValidate.check(bar) }
        try await change(id, .regular) { n in
            n.with(title: n.title, subtitle: n.subtitle, body: n.body, style: n.style, icon: n.icon,
                   actions: n.actions, progress: progress, statusText: n.statusText, count: n.count,
                   ongoing: n.ongoing, url: n.url, userData: n.userData)
        }
    }

    public func append_message(id: Notifications.NotificationId, message: Notifications.ChatMessage) async throws {
        try NotificationsValidate.check(message)
        try await change(id, .alert) { n in
            guard let next = n.appending(message) else {
                throw Notifications.NotificationError.invalid(
                    Notifications.InvalidField(field: "style", problem: .notAllowedHere))
            }
            return next
        }
    }

    /// Changes the shown `id` with `edit` in its lane; posts through the
    /// coalescer. A failed `edit` leaves the notification as it was.
    private func change(
        _ id: Notifications.NotificationId, _ urgency: NotificationsCoalescer<Notifications.Notification>.Urgency,
        _ edit: (Notifications.Notification) throws -> Notifications.Notification
    ) async throws {
        try await core.lanes.run(id.value) {
            let admit: NotificationsCoalescer<Notifications.Notification>.Admit = try core.lock.guarded {
                guard let old = core.live[id.value] else {
                    throw Notifications.NotificationError.notFound(id.value)
                }
                let next = try edit(old)
                core.live[id.value] = next
                return core.coalescer.offer(id.value, next, urgency, nowMs: nowMs())
            }
            switch admit {
            case .now(let n):
                try await post(n, quiet: urgency == .regular, donate: urgency != .regular)
            case .queued:
                armFlush()
            case .dropped:
                break
            }
        }
    }

    /// Posts queued updates when the bucket allows, one timer at a time.
    /// Each goes through its id's lane and only while the id is still
    /// shown: a cancel or timeout taken in between wins.
    private func armFlush() {
        let due: Int64? = core.lock.guarded {
            if core.flushArmed { return nil }
            guard let due = core.coalescer.nextDue() else { return nil }
            core.flushArmed = true
            return due
        }
        guard let due = due else { return }
        let wait = max(Double(due - nowMs()) / 1000, 0)
        DispatchQueue.global().asyncAfter(deadline: .now() + wait) { [weak self] in
            guard let self = self else { return }
            let ready = self.core.lock.guarded { () -> [(String, Notifications.Notification)] in
                self.core.flushArmed = false
                return self.core.coalescer.poll(nowMs: nowMs())
            }
            Task {
                for (id, _) in ready {
                    await self.core.lanes.run(id) {
                        // The latest content, unless the id left the screen.
                        guard let n = self.core.lock.guarded({ self.core.live[id] }) else { return }
                        do {
                            try await self.post(n, quiet: true, donate: false)
                        } catch {
                            self.core.emit(.failed(Notifications.Failure(id: n.id, message: "\(error)")))
                        }
                    }
                }
                self.armFlush()
            }
        }
    }

    // MARK: removing and listing

    public func cancel(id: Notifications.NotificationId) async throws {
        await remove([id.value])
        await core.store.remove(id.value)
    }

    public func cancel_group(group: Notifications.GroupId) async throws {
        var ids: [String] = core.lock.guarded {
            core.live.values.filter { $0.group?.id.value == group.value }.map { $0.id.value }
        }
        let delivered = await core.center.deliveredNotifications()
        for d in delivered where d.request.content.threadIdentifier == group.value {
            if let id = d.request.content.userInfo[NotificationsKeys.id] as? String { ids.append(id) }
        }
        await remove(Array(Set(ids)))
        await core.store.removeAll(extra: ids) { $0.group?.id.value == group.value }
    }

    /// Removes only this plugin's notifications (`istmo.id` in
    /// `userInfo`): the app's own delivered and pending ones stay.
    public func cancel_all() async throws {
        let shown: [String] = core.lock.guarded { Array(core.live.keys) }
        let delivered = await core.center.deliveredNotifications()
        let ours = delivered.compactMap { $0.request.content.userInfo[NotificationsKeys.id] as? String }
        await remove(Array(Set(shown + ours)))
        await core.store.removeAll { _ in true }
        await core.store.removeAllRequests()
    }

    /// Takes `ids` off the screen: their state first (a flush or timeout
    /// waiting in a lane then finds nothing), then each lane removes the
    /// delivered notification after the call in flight on it.
    private func remove(_ ids: [String]) async {
        guard !ids.isEmpty else { return }
        core.forgetState(ids)
        for id in ids {
            await core.lanes.run(id) { await core.removeDelivered([id]) }
        }
    }

    public func pending() async throws -> [Notifications.PendingNotification] {
        core.store.pending()
    }

    public func delivered() async throws -> [Notifications.DeliveredNotification] {
        let delivered = await core.center.deliveredNotifications()
        return delivered.compactMap { d in
            let content = d.request.content
            guard let id = content.userInfo[NotificationsKeys.id] as? String else { return nil }
            return Notifications.DeliveredNotification(
                id: Notifications.NotificationId(value: id), title: content.title.isEmpty ? nil : content.title,
                body: content.body.isEmpty ? nil : content.body,
                group: content.threadIdentifier.isEmpty ? nil : Notifications.GroupId(value: content.threadIdentifier),
                deliveredMs: Int64(d.date.timeIntervalSince1970 * 1000))
        }
    }

    public func complete(ack: Notifications.AckToken) async throws {
        if !core.finish(ack: ack.value) {
            throw Notifications.NotificationError.notFound("ack \(ack.value)")
        }
    }

    public func events() -> AsyncThrowingStream<Notifications.NotificationEvent, Error> {
        core.events()
    }

    private static func platform(_ error: Error) -> Notifications.NotificationError {
        #if canImport(Darwin)
        let code = Int64((error as NSError).code)
        #else
        let code: Int64 = 0
        #endif
        return .platform(Notifications.PlatformError(code: code, message: error.localizedDescription))
    }
}
