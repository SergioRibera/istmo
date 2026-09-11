import Foundation
#if canImport(ActivityKit)
import ActivityKit

/// Demo-specific `LiveActivityHandler` that binds the wire types
/// (`TimerAttributes` / `TimerState` from `DemoTypes.swift`) to the
/// ActivityKit-facing `LiveTimer.Attributes` conformer.
///
/// Subclassing `ActivityKitLiveActivityHandler<Attrs>` gives the demo
/// the ActivityKit plumbing (Activity<Attrs>.request /
/// .update / .end / registry) for free — the only overrides the
/// consumer writes are the four bincode encode/decode hooks, which
/// delegate straight into the generated `DemoCodecsImpl`.
///
/// The concrete ActivityAttributes conformer is `LiveTimer.Attributes`,
/// not the generated wire struct; keeping them separate lets Widget
/// Extension targets (a follow-up bundle) reuse the same content-state
/// type without pulling the whole demo target into the widget's
/// compilation.
@available(iOS 16.1, *)
final class TimerLiveActivityHandler: ActivityKitLiveActivityHandler<LiveTimer.Attributes> {

    init() {
        super.init(activityType: "timer")
    }

    override func decodeAttributes(_ data: Data) throws -> LiveTimer.Attributes {
        try LiveTimer.Attributes.decode(from: data)
    }

    override func decodeState(_ data: Data) throws -> LiveTimer.Attributes.ContentState {
        try LiveTimer.Attributes.ContentState.decode(from: data)
    }

    override func encodeAttributes(_ attributes: LiveTimer.Attributes) throws -> Data {
        try attributes.encode()
    }

    override func encodeState(_ state: LiveTimer.Attributes.ContentState) throws -> Data {
        try state.encode()
    }
}

#endif
