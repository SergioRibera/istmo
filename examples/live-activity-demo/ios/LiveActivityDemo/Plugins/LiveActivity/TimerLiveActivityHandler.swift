import Foundation
#if canImport(ActivityKit)
import ActivityKit

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

