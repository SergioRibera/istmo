// Stand-in for the AVFoundation display layer the screen-capture native
// backend renders previews into.

import CoreMedia
import Foundation
import QuartzCore

public struct AVLayerVideoGravity: Hashable, Sendable {
    public let rawValue: String
    public init(rawValue: String) { self.rawValue = rawValue }
    public static let resizeAspect = AVLayerVideoGravity(rawValue: "AVLayerVideoGravityResizeAspect")
    public static let resizeAspectFill = AVLayerVideoGravity(rawValue: "AVLayerVideoGravityResizeAspectFill")
    public static let resize = AVLayerVideoGravity(rawValue: "AVLayerVideoGravityResize")
}

public enum AVQueuedSampleBufferRenderingStatus: Int, Sendable {
    case unknown = 0
    case rendering = 1
    case failed = 2
}

open class AVSampleBufferDisplayLayer: CALayer {
    open var videoGravity: AVLayerVideoGravity = .resizeAspect
    open var status: AVQueuedSampleBufferRenderingStatus { .unknown }
    open var isReadyForMoreMediaData: Bool { true }
    open func enqueue(_ sampleBuffer: CMSampleBuffer) {}
    open func flush() {}
    open func flushAndRemoveImage() {}
}

@available(iOS 17.0, *)
open class AVSampleBufferVideoRenderer: NSObject {
    open func enqueue(_ sampleBuffer: CMSampleBuffer) {}
    open func flush() {}
}

public extension AVSampleBufferDisplayLayer {
    @available(iOS 17.0, *)
    var sampleBufferRenderer: AVSampleBufferVideoRenderer { AVSampleBufferVideoRenderer() }
}
