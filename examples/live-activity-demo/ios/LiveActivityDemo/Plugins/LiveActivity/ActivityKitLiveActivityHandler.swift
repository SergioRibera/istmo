import Foundation
import IstmoRuntime
#if canImport(ActivityKit)
import ActivityKit

/// Generic ``LiveActivityHandler`` base class for authors whose activity
/// state matches the `ActivityKit` shape: one `ActivityAttributes`
/// conformer whose associated `ContentState` is the mutable snapshot.
///
/// Subclass with:
///
/// ```swift
/// final class TimerLiveActivityHandler:
///     ActivityKitLiveActivityHandler<TimerAttributes> {
///
///     override init() { super.init(activityType: "timer") }
///
///     override func decodeAttributes(_ data: Data) throws -> TimerAttributes {
///         try TimerAttributes.decode(from: data)
///     }
///
///     override func decodeState(_ data: Data) throws -> TimerAttributes.ContentState {
///         try TimerAttributes.ContentState.decode(from: data)
///     }
///
///     override func encodeState(_ state: TimerAttributes.ContentState) throws -> Data {
///         try state.encode()
///     }
///
///     override func encodeAttributes(_ attrs: TimerAttributes) throws -> Data {
///         try attrs.encode()
///     }
/// }
/// ```
///
/// The `encode`/`decode` helpers are the codegen-generated bincode
/// bridges for the consumer's own `#[istmo::message]` types (see
/// `generate_swift_types` / `generate_swift_codecs` in `istmo-build`).
///
/// The handler owns its own `[NativeHandleId: Activity<Attributes>]` map
/// so multiple activities of the same type coexist safely. Handle-id
/// allocation is delegated to ``IstmoRuntime`` via
/// ``IstmoRuntime.shared.allocateHandleId(owner:)`` so the runtime's
/// release routing works transparently.
@available(iOS 16.1, *)
open class ActivityKitLiveActivityHandler<Attributes: ActivityAttributes>: LiveActivityHandler {

    public let activityType: String

    private let queue = DispatchQueue(label: "dev.istmo.plugins.live-activity.handler")
    private var activities: [UInt64: Activity<Attributes>] = [:]

    public init(activityType: String) {
        self.activityType = activityType
    }

    // ---- Subclass hooks --------------------------------------------------

    open func decodeAttributes(_ data: Data) throws -> Attributes {
        fatalError("override decodeAttributes")
    }

    open func decodeState(_ data: Data) throws -> Attributes.ContentState {
        fatalError("override decodeState")
    }

    open func encodeAttributes(_ attributes: Attributes) throws -> Data {
        fatalError("override encodeAttributes")
    }

    open func encodeState(_ state: Attributes.ContentState) throws -> Data {
        fatalError("override encodeState")
    }

    // ---- LiveActivityHandler --------------------------------------------

    public func start(
        attributes: Data,
        initialState: Data,
        style: ActivityStyle,
        staleAfterSeconds: UInt32?,
        androidTierHint _: AndroidTierHint?
    ) async throws -> NativeHandleId {
        let info = ActivityAuthorizationInfo()
        guard info.areActivitiesEnabled else {
            throw ActivityError.disabled
        }

        let attrs: Attributes
        let state: Attributes.ContentState
        do {
            attrs = try decodeAttributes(attributes)
            state = try decodeState(initialState)
        } catch {
            throw ActivityError.decode(String(describing: error))
        }

        do {
            let content = ActivityContent(
                state: state,
                staleDate: staleAfterSeconds.map { Date().addingTimeInterval(TimeInterval($0)) }
            )
            let activity = try request(attributes: attrs, content: content, style: style)
            let id = IstmoRuntime.shared.allocateHandleId(owner: self.activityType)
            queue.sync {
                self.activities[id] = activity
            }
            return NativeHandleId(value: id)
        } catch let err as ActivityError {
            throw err
        } catch {
            let message = String(describing: error)
            if message.contains("exceededMaximum") {
                throw ActivityError.exceededMaximum
            }
            throw ActivityError.backend(message)
        }
    }

    public func update(
        handle: NativeHandleId,
        state: Data,
        alert: AlertConfig?
    ) async throws {
        guard let activity = queue.sync(execute: { activities[handle.value] }) else {
            throw ActivityError.handleNotFound
        }

        let decoded: Attributes.ContentState
        do {
            decoded = try decodeState(state)
        } catch {
            throw ActivityError.decode(String(describing: error))
        }

        let content = ActivityContent(state: decoded, staleDate: nil)
        if let alert {
            await activity.update(content, alertConfiguration: alertConfiguration(from: alert))
        } else {
            await activity.update(content)
        }
    }

    public func end(
        handle: NativeHandleId,
        finalState: Data?,
        dismissal: DismissalPolicy
    ) async throws {
        let activity = queue.sync { activities[handle.value] }
        guard let activity else { throw ActivityError.handleNotFound }

        let policy: ActivityUIDismissalPolicy
        switch dismissal {
        case .immediate:
            policy = .immediate
        case .default:
            policy = .default
        case .afterSeconds(let seconds):
            policy = .after(Date().addingTimeInterval(TimeInterval(seconds)))
        }

        let content: ActivityContent<Attributes.ContentState>?
        if let finalState {
            do {
                content = ActivityContent(state: try decodeState(finalState), staleDate: nil)
            } catch {
                throw ActivityError.decode(String(describing: error))
            }
        } else {
            content = nil
        }

        await activity.end(content, dismissalPolicy: policy)
        queue.sync {
            _ = activities.removeValue(forKey: handle.value)
        }
    }

    public func restoreActive() async throws -> [RestoredActivity] {
        var out: [RestoredActivity] = []
        for activity in Activity<Attributes>.activities {
            let id = IstmoRuntime.shared.allocateHandleId(owner: self.activityType)
            queue.sync {
                self.activities[id] = activity
            }
            let attrsData: Data
            let stateData: Data
            do {
                attrsData = try encodeAttributes(activity.attributes)
                stateData = try encodeState(activity.content.state)
            } catch {
                throw ActivityError.decode(String(describing: error))
            }
            out.append(
                RestoredActivity(
                    handle: NativeHandleId(value: id),
                    activity_type: self.activityType,
                    attributes: attrsData,
                    state: stateData
                )
            )
        }
        return out
    }

    public func releaseHandle(_ handle: NativeHandleId) {
        let activity = queue.sync { activities.removeValue(forKey: handle.value) }
        guard let activity else { return }
        Task { await activity.end(nil, dismissalPolicy: .immediate) }
    }

    // ---- Private helpers -------------------------------------------------

    private func request(
        attributes: Attributes,
        content: ActivityContent<Attributes.ContentState>,
        style _: ActivityStyle
    ) throws -> Activity<Attributes> {
        // `Activity.request` throws a small set of enum cases we surface
        // as `ActivityError`. The `style` argument is future-proofing —
        // iOS 18's `.transient` style flag lives on `Activity.request`
        // in newer SDKs; today the plugin ignores it.
        return try Activity<Attributes>.request(attributes: attributes, content: content)
    }

    private func alertConfiguration(from alert: AlertConfig) -> AlertConfiguration {
        let sound: AlertConfiguration.AlertSound
        switch alert.sound {
        case .default:
            sound = .default
        case .named(let name):
            sound = .named(name)
        case .none:
            sound = .default // AlertConfiguration always plays something; the
                             // wire `.none` variant is honoured by omitting
                             // the alert entirely on the caller side.
        }
        return AlertConfiguration(
            title: LocalizedStringResource(stringLiteral: alert.title),
            body: LocalizedStringResource(stringLiteral: alert.body),
            sound: sound
        )
    }
}

#endif
