// Stand-in for the slice of CoreGraphics the screen-capture native
// backend uses: geometry and its dictionary representation.

import Foundation

public struct CGFloat: Sendable, Comparable {
    public var native: Double
    public init(_ value: Double) { native = value }
    public init(_ value: Int) { native = Double(value) }
    public static func < (a: CGFloat, b: CGFloat) -> Bool { a.native.isLess(than: b.native) }
}

public extension Double {
    init(_ value: CGFloat) { self = value.native }
}

public struct CGPoint: Sendable {
    public var x: CGFloat
    public var y: CGFloat
    public init(x: CGFloat, y: CGFloat) { self.x = x; self.y = y }
}

public struct CGSize: Sendable {
    public var width: CGFloat
    public var height: CGFloat
    public init(width: CGFloat, height: CGFloat) { self.width = width; self.height = height }
}

public struct CGRect: Sendable {
    public var origin: CGPoint
    public var size: CGSize
    public init(origin: CGPoint, size: CGSize) { self.origin = origin; self.size = size }
    public init(x: Double, y: Double, width: Double, height: Double) {
        origin = CGPoint(x: CGFloat(x), y: CGFloat(y))
        size = CGSize(width: CGFloat(width), height: CGFloat(height))
    }
    public static var zero: CGRect { CGRect(x: 0, y: 0, width: 0, height: 0) }
    public var minX: CGFloat { origin.x }
    public var minY: CGFloat { origin.y }
    public var width: CGFloat { size.width }
    public var height: CGFloat { size.height }
    /// `CGRectMakeWithDictionaryRepresentation`.
    public init?(dictionaryRepresentation dict: CFDictionary) { return nil }
}
