// Stand-in for the ScreenCaptureKit API the screen-capture native backend
// uses in-process on iOS / iPadOS 27 (the macOS API surface: picker,
// content filter, stream, configuration, frame info), as the iOS 27
// "Capturing screen content on iOS" article presents it.

import CoreGraphics
import CoreMedia
import Foundation

@available(iOS 27.0, macOS 12.3, *)
public enum SCStreamOutputType: Int, Sendable {
    case screen = 0
    case audio = 1
    case microphone = 2
}

@available(iOS 27.0, macOS 12.3, *)
public enum SCFrameStatus: Int, Sendable {
    case complete = 0
    case idle = 1
    case blank = 2
    case suspended = 3
    case started = 4
    case stopped = 5
}

@available(iOS 27.0, macOS 12.3, *)
public struct SCStreamFrameInfo: Hashable, RawRepresentable, Sendable {
    public let rawValue: String
    public init(rawValue: String) { self.rawValue = rawValue }
    public static let status = SCStreamFrameInfo(rawValue: "SCStreamFrameInfoStatus")
    public static let displayTime = SCStreamFrameInfo(rawValue: "SCStreamFrameInfoDisplayTime")
    public static let scaleFactor = SCStreamFrameInfo(rawValue: "SCStreamFrameInfoScaleFactor")
    public static let contentScale = SCStreamFrameInfo(rawValue: "SCStreamFrameInfoContentScale")
    public static let contentRect = SCStreamFrameInfo(rawValue: "SCStreamFrameInfoContentRect")
    public static let dirtyRects = SCStreamFrameInfo(rawValue: "SCStreamFrameInfoDirtyRects")
    @available(iOS 27.0, macOS 27.0, *)
    public static let videoOrientation = SCStreamFrameInfo(rawValue: "SCStreamFrameInfoVideoOrientation")
}

@available(iOS 27.0, macOS 12.3, *)
public struct SCStreamError: Error, Sendable {
    public enum Code: Int, Sendable {
        case userDeclined = -3801
        case failedToStart = -3802
        case missingEntitlements = -3803
        case failedApplicationConnectionInvalid = -3804
        case failedApplicationConnectionInterrupted = -3805
        case failedNoMatchingApplicationContext = -3806
        case attemptToStartStreamState = -3807
        case attemptToStopStreamState = -3808
        case attemptToUpdateFilterState = -3809
        case attemptToConfigState = -3810
        case internalError = -3811
        case invalidParameter = -3812
        case noWindowList = -3813
        case noDisplayList = -3814
        case noCaptureSource = -3815
        case removingStream = -3816
        case userStopped = -3817
        case failedToStartAudioCapture = -3818
        case failedToStopAudioCapture = -3819
        case failedToStartMicrophoneCapture = -3820
        case systemStoppedStream = -3821
    }
    public var code: Code
}

@available(iOS 27.0, macOS 12.3, *)
open class SCContentFilter: NSObject {
    @available(iOS 27.0, macOS 14.0, *)
    open var contentRect: CGRect { .zero }
    @available(iOS 27.0, macOS 14.0, *)
    open var pointPixelScale: Float { 1 }
}

@available(iOS 27.0, macOS 12.3, *)
open class SCStreamConfiguration: NSObject {
    open var width: Int = 0
    open var height: Int = 0
    open var minimumFrameInterval: CMTime = .zero
    open var pixelFormat: OSType = 0
    open var scalesToFit: Bool = false
    open var showsCursor: Bool = true
    open var queueDepth: Int = 3
    open var capturesAudio: Bool = false
    open var sampleRate: Int = 48_000
    open var channelCount: Int = 2
    open var excludesCurrentProcessAudio: Bool = false
}

@available(iOS 27.0, macOS 12.3, *)
public protocol SCStreamOutput: NSObjectProtocol {
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType)
}

@available(iOS 27.0, macOS 12.3, *)
public protocol SCStreamDelegate: NSObjectProtocol {
    func stream(_ stream: SCStream, didStopWithError error: Error)
}

@available(iOS 27.0, macOS 12.3, *)
open class SCStream: NSObject {
    public init(filter: SCContentFilter, configuration streamConfig: SCStreamConfiguration,
                delegate: SCStreamDelegate?) {}
    open func addStreamOutput(_ output: SCStreamOutput, type: SCStreamOutputType,
                              sampleHandlerQueue: DispatchQueue?) throws {}
    open func removeStreamOutput(_ output: SCStreamOutput, type: SCStreamOutputType) throws {}
    open func startCapture(completionHandler: ((Error?) -> Void)? = nil) {}
    open func stopCapture(completionHandler: ((Error?) -> Void)? = nil) {}
    open func updateConfiguration(_ streamConfig: SCStreamConfiguration,
                                  completionHandler: ((Error?) -> Void)? = nil) {}
}

@available(iOS 27.0, macOS 14.0, *)
public protocol SCContentSharingPickerObserver: NSObjectProtocol {
    func contentSharingPicker(_ picker: SCContentSharingPicker, didCancelFor stream: SCStream?)
    func contentSharingPicker(_ picker: SCContentSharingPicker, didUpdateWith filter: SCContentFilter,
                              for stream: SCStream?)
    func contentSharingPickerStartDidFailWithError(_ error: Error)
}

@available(iOS 27.0, macOS 14.0, *)
open class SCContentSharingPicker: NSObject {
    public static let shared = SCContentSharingPicker()
    open var isActive: Bool = false
    open func add(_ observer: SCContentSharingPickerObserver) {}
    open func remove(_ observer: SCContentSharingPickerObserver) {}
    open func present() {}
}
