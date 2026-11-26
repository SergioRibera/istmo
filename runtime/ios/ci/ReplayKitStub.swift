// Stand-in for the ReplayKit in-app capture API (`RPScreenRecorder`)
// the screen-capture native backend uses on iOS < 27.

import CoreMedia
import Foundation

public enum RPSampleBufferType: Int, Sendable {
    case video = 1
    case audioApp = 2
    case audioMic = 3
}

/// Attachment key of a video sample buffer: its
/// `CGImagePropertyOrientation` as an `NSNumber`.
public let RPVideoSampleOrientationKey: String = "RPVideoSampleOrientationKey"

public struct RPRecordingError: Error, Sendable {
    public enum Code: Int, Sendable {
        case unknown = -5800
        case userDeclined = -5801
        case disabled = -5802
        case failedToStart = -5803
        case failed = -5804
        case insufficientStorage = -5805
        case interrupted = -5806
        case contentResize = -5807
        case broadcastInvalidSession = -5808
        case systemDormancy = -5809
        case entitlements = -5810
        case activePhoneCall = -5811
    }
    public var code: Code
}

open class RPScreenRecorder: NSObject {
    open class func shared() -> RPScreenRecorder { RPScreenRecorder() }
    open var isAvailable: Bool { false }
    open var isRecording: Bool { false }
    open var isMicrophoneEnabled: Bool = false
    open func startCapture(handler captureHandler: ((CMSampleBuffer, RPSampleBufferType, Error?) -> Void)?,
                           completionHandler: ((Error?) -> Void)? = nil) {}
    open func stopCapture(handler: ((Error?) -> Void)? = nil) {}
}
