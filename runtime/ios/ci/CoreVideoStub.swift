// Stand-in for the CoreVideo pixel-buffer API the screen-capture native
// backend (and `IstmoFrameSender+Apple.swift`) use.

import Foundation
import IOSurface

/// `CVBuffer` (a Core Foundation type: a class).
public final class CVBuffer {}
public typealias CVImageBuffer = CVBuffer
public typealias CVPixelBuffer = CVImageBuffer
public typealias CVReturn = Int32
public typealias OSType = UInt32

public let kCVReturnSuccess: CVReturn = 0
public let kCVPixelFormatType_32BGRA: OSType = 0x4247_5241
public let kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange: OSType = 0x3432_3076
public let kCVPixelFormatType_420YpCbCr8BiPlanarFullRange: OSType = 0x3432_3066
public let kCVPixelFormatType_ARGB2101010LEPacked: OSType = 0x6C31_3072

public struct CVPixelBufferLockFlags: OptionSet, Sendable {
    public let rawValue: UInt64
    public init(rawValue: UInt64) { self.rawValue = rawValue }
    public static let readOnly = CVPixelBufferLockFlags(rawValue: 1)
}

public func CVPixelBufferGetWidth(_ pixelBuffer: CVPixelBuffer) -> Int { 0 }
public func CVPixelBufferGetHeight(_ pixelBuffer: CVPixelBuffer) -> Int { 0 }
public func CVPixelBufferGetPixelFormatType(_ pixelBuffer: CVPixelBuffer) -> OSType { 0 }
public func CVPixelBufferIsPlanar(_ pixelBuffer: CVPixelBuffer) -> Bool { false }
public func CVPixelBufferGetPlaneCount(_ pixelBuffer: CVPixelBuffer) -> Int { 0 }
public func CVPixelBufferGetBytesPerRow(_ pixelBuffer: CVPixelBuffer) -> Int { 0 }
public func CVPixelBufferGetBytesPerRowOfPlane(_ pixelBuffer: CVPixelBuffer, _ planeIndex: Int) -> Int { 0 }
public func CVPixelBufferGetHeightOfPlane(_ pixelBuffer: CVPixelBuffer, _ planeIndex: Int) -> Int { 0 }
public func CVPixelBufferGetWidthOfPlane(_ pixelBuffer: CVPixelBuffer, _ planeIndex: Int) -> Int { 0 }
public func CVPixelBufferGetBaseAddress(_ pixelBuffer: CVPixelBuffer) -> UnsafeMutableRawPointer? { nil }
public func CVPixelBufferGetBaseAddressOfPlane(_ pixelBuffer: CVPixelBuffer, _ planeIndex: Int) -> UnsafeMutableRawPointer? { nil }
public func CVPixelBufferLockBaseAddress(_ pixelBuffer: CVPixelBuffer, _ lockFlags: CVPixelBufferLockFlags) -> CVReturn { 0 }
public func CVPixelBufferUnlockBaseAddress(_ pixelBuffer: CVPixelBuffer, _ unlockFlags: CVPixelBufferLockFlags) -> CVReturn { 0 }
public func CVPixelBufferGetIOSurface(_ pixelBuffer: CVPixelBuffer?) -> Unmanaged<IOSurfaceRef>? { nil }
