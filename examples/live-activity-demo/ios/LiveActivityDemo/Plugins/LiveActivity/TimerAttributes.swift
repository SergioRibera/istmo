import Foundation
#if canImport(ActivityKit)
import ActivityKit

/// `ActivityAttributes` conformer for the timer live activity.
///
/// Wrapped inside the `LiveTimer` namespace enum so its `Attributes`
/// and `Attributes.ContentState` types do not collide with the generated
/// wire structs `TimerAttributes` / `TimerState` emitted into
/// `DemoTypes.swift` by `build.rs`. The Rust half (`app.rs`) is the
/// single source of truth for the wire shape; this file only adds the
/// ActivityKit-facing sugar plus the bincode ↔ ActivityKit bridge.
public enum LiveTimer {

    @available(iOS 16.1, *)
    public struct Attributes: ActivityAttributes {

        public struct ContentState: Codable, Hashable {
            public let elapsedSeconds: UInt32
            public let label: String

            public init(elapsedSeconds: UInt32, label: String) {
                self.elapsedSeconds = elapsedSeconds
                self.label = label
            }

            static func decode(from data: Data) throws -> Self {
                let codecs = DemoCodecsImpl()
                var cursor = Bincode.Cursor(data)
                let wire = try codecs.readTimerState(&cursor)
                return Self(elapsedSeconds: wire.elapsed_seconds, label: wire.label)
            }

            func encode() throws -> Data {
                let codecs = DemoCodecsImpl()
                var out = Data()
                codecs.writeTimerState(&out, TimerState(elapsed_seconds: elapsedSeconds, label: label))
                return out
            }
        }

        public let title: String
        public let targetSeconds: UInt32

        public init(title: String, targetSeconds: UInt32) {
            self.title = title
            self.targetSeconds = targetSeconds
        }

        static func decode(from data: Data) throws -> Self {
            let codecs = DemoCodecsImpl()
            var cursor = Bincode.Cursor(data)
            let wire = try codecs.readTimerAttributes(&cursor)
            return Self(title: wire.title, targetSeconds: wire.target_seconds)
        }

        func encode() throws -> Data {
            let codecs = DemoCodecsImpl()
            var out = Data()
            codecs.writeTimerAttributes(&out, TimerAttributes(title: title, target_seconds: targetSeconds))
            return out
        }
    }
}

#endif
