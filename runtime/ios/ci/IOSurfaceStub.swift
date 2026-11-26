// Stand-in for the IOSurface API the screen-capture native backend uses.

import Foundation

/// `IOSurfaceRef` (a Core Foundation type: a class).
public final class IOSurfaceRef {}

public typealias IOSurfaceID = UInt32

public func IOSurfaceGetID(_ buffer: IOSurfaceRef) -> IOSurfaceID { 0 }
public func IOSurfaceGetPlaneCount(_ buffer: IOSurfaceRef) -> Int { 0 }
public func IOSurfaceGetBaseAddress(_ buffer: IOSurfaceRef) -> UnsafeMutableRawPointer { UnsafeMutableRawPointer(bitPattern: 1)! }
public func IOSurfaceGetBaseAddressOfPlane(_ buffer: IOSurfaceRef, _ planeIndex: Int) -> UnsafeMutableRawPointer { UnsafeMutableRawPointer(bitPattern: 1)! }
public func IOSurfaceGetBytesPerRow(_ buffer: IOSurfaceRef) -> Int { 0 }
public func IOSurfaceGetBytesPerRowOfPlane(_ buffer: IOSurfaceRef, _ planeIndex: Int) -> Int { 0 }
public func IOSurfaceGetHeight(_ buffer: IOSurfaceRef) -> Int { 0 }
public func IOSurfaceGetHeightOfPlane(_ buffer: IOSurfaceRef, _ planeIndex: Int) -> Int { 0 }
