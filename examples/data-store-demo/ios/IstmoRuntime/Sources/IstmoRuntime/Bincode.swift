// Minimal port of the bincode 2 "standard" configuration (VarintEncoding +
// LittleEndian) that the istmo wire format uses.
//
// Covers every shape the plugin dispatcher codegen emits:
//
// * varint u8/u16/u32/u64 and i8/i16/i32/i64 (zigzag + varint for signed).
// * varint-length-prefixed String / Data.
// * Bool as a single byte.
// * f32 / f64 as fixed-width IEEE 754 little-endian (bincode 2 keeps
//   floats fixed-width under `VarintEncoding`).
// * Vec / Option combinators with cursor-carrying closures.
//
// Extend per new type as plugin surfaces grow. The generator picks the
// helper it needs from this file; anything the generator names must exist
// here or the Swift compiler surfaces a clear error at the codegen output
// site.

import Foundation

public enum Bincode {

    // MARK: - Writer

    public static func writeBool(_ out: inout Data, _ value: Bool) {
        out.append(value ? 1 : 0)
    }

    public static func writeU8(_ out: inout Data, _ value: UInt8) {
        out.append(value)
    }

    public static func writeI8(_ out: inout Data, _ value: Int8) {
        out.append(UInt8(bitPattern: value))
    }

    /// Bincode 2 "standard" varint encoding for unsigned integers:
    /// values < 251 fit in one byte; larger values are tagged 251/252/253
    /// followed by 2/4/8 little-endian bytes. See `varint::encode_u64` in
    /// the bincode source.
    public static func writeVarintU64(_ out: inout Data, _ value: UInt64) {
        if value < 251 {
            out.append(UInt8(value))
        } else if value <= UInt64(UInt16.max) {
            out.append(251)
            appendLE(&out, UInt16(value))
        } else if value <= UInt64(UInt32.max) {
            out.append(252)
            appendLE(&out, UInt32(value))
        } else {
            out.append(253)
            appendLE(&out, value)
        }
    }

    public static func writeVarintI64(_ out: inout Data, _ value: Int64) {
        writeVarintU64(&out, zigzag(value))
    }

    public static func writeVarintU32(_ out: inout Data, _ value: UInt32) {
        writeVarintU64(&out, UInt64(value))
    }

    public static func writeVarintI32(_ out: inout Data, _ value: Int32) {
        writeVarintI64(&out, Int64(value))
    }

    public static func writeF32(_ out: inout Data, _ value: Float) {
        appendLE(&out, value.bitPattern)
    }

    public static func writeF64(_ out: inout Data, _ value: Double) {
        appendLE(&out, value.bitPattern)
    }

    public static func writeString(_ out: inout Data, _ value: String) {
        let bytes = Array(value.utf8)
        writeVarintU64(&out, UInt64(bytes.count))
        out.append(contentsOf: bytes)
    }

    public static func writeData(_ out: inout Data, _ value: Data) {
        writeVarintU64(&out, UInt64(value.count))
        out.append(value)
    }

    /// Length-prefixed `Vec<T>`. `writer` runs per element and advances
    /// the buffer via inout. `T` is inferred from the closure signature.
    public static func writeVec<T>(
        _ out: inout Data,
        _ items: [T],
        _ writer: (inout Data, T) -> Void
    ) {
        writeVarintU64(&out, UInt64(items.count))
        for item in items {
            writer(&out, item)
        }
    }

    /// One-tag-then-value `Option<T>` encoding.
    public static func writeOption<T>(
        _ out: inout Data,
        _ value: T?,
        _ writer: (inout Data, T) -> Void
    ) {
        if let v = value {
            out.append(1)
            writer(&out, v)
        } else {
            out.append(0)
        }
    }

    // MARK: - Client encode(&payload, value) overloads
    //
    // The client generator emits `Bincode.encode(&payload, arg)` per
    // argument; adding a new arg type in a contract means adding a
    // matching overload here.

    public static func encode(_ out: inout Data, _ value: String) {
        writeString(&out, value)
    }

    public static func encode(_ out: inout Data, _ value: Int32) {
        writeVarintI32(&out, value)
    }

    public static func encode(_ out: inout Data, _ value: Bool) {
        writeBool(&out, value)
    }

    // MARK: - Reader

    public struct Cursor {
        public var data: Data
        public var offset: Int
        public init(_ data: Data) {
            self.data = data
            self.offset = 0
        }
        public var remaining: Int { data.count - offset }
    }

    public enum DecodeError: Error {
        case unexpectedEnd
        case invalidTag(UInt8)
        case invalidUtf8
    }

    public static func readU8(_ c: inout Cursor) throws -> UInt8 {
        guard c.remaining >= 1 else { throw DecodeError.unexpectedEnd }
        let b = c.data[c.offset]
        c.offset += 1
        return b
    }

    public static func readBool(_ c: inout Cursor) throws -> Bool {
        try readU8(&c) != 0
    }

    public static func readVarintU64(_ c: inout Cursor) throws -> UInt64 {
        let tag = try readU8(&c)
        switch tag {
        case 0 ..< 251:
            return UInt64(tag)
        case 251:
            return UInt64(try readLE(&c, UInt16.self))
        case 252:
            return UInt64(try readLE(&c, UInt32.self))
        case 253:
            return try readLE(&c, UInt64.self)
        default:
            throw DecodeError.invalidTag(tag)
        }
    }

    public static func readVarintI64(_ c: inout Cursor) throws -> Int64 {
        unzigzag(try readVarintU64(&c))
    }

    public static func readVarintU32(_ c: inout Cursor) throws -> UInt32 {
        UInt32(try readVarintU64(&c))
    }

    public static func readVarintI32(_ c: inout Cursor) throws -> Int32 {
        Int32(try readVarintI64(&c))
    }

    public static func readF32(_ c: inout Cursor) throws -> Float {
        let bits = try readLE(&c, UInt32.self)
        return Float(bitPattern: bits)
    }

    public static func readF64(_ c: inout Cursor) throws -> Double {
        let bits = try readLE(&c, UInt64.self)
        return Double(bitPattern: bits)
    }

    public static func readString(_ c: inout Cursor) throws -> String {
        let len = Int(try readVarintU64(&c))
        guard c.remaining >= len else { throw DecodeError.unexpectedEnd }
        let slice = c.data.subdata(in: c.offset ..< c.offset + len)
        c.offset += len
        guard let s = String(data: slice, encoding: .utf8) else {
            throw DecodeError.invalidUtf8
        }
        return s
    }

    public static func readData(_ c: inout Cursor) throws -> Data {
        let len = Int(try readVarintU64(&c))
        guard c.remaining >= len else { throw DecodeError.unexpectedEnd }
        let slice = c.data.subdata(in: c.offset ..< c.offset + len)
        c.offset += len
        return slice
    }

    public static func readVec<T>(
        _ c: inout Cursor,
        _ reader: (inout Cursor) throws -> T
    ) throws -> [T] {
        let len = Int(try readVarintU64(&c))
        var out: [T] = []
        out.reserveCapacity(len)
        for _ in 0 ..< len {
            out.append(try reader(&c))
        }
        return out
    }

    public static func readOption<T>(
        _ c: inout Cursor,
        _ reader: (inout Cursor) throws -> T
    ) throws -> T? {
        let tag = try readU8(&c)
        switch tag {
        case 0: return nil
        case 1: return try reader(&c)
        default: throw DecodeError.invalidTag(tag)
        }
    }

    /// Sugar mirroring the `Bincode.decode(bytes) as T` shape the client
    /// generator emits. Currently supports `String`; extend per new type.
    public static func decode(_ bytes: Data) throws -> String {
        var c = Cursor(bytes)
        return try readString(&c)
    }

    // MARK: - Helpers

    private static func zigzag(_ value: Int64) -> UInt64 {
        UInt64(bitPattern: (value << 1) ^ (value >> 63))
    }

    private static func unzigzag(_ value: UInt64) -> Int64 {
        let raw = Int64(bitPattern: value)
        return (raw >> 1) ^ -(raw & 1)
    }

    private static func appendLE(_ out: inout Data, _ value: UInt16) {
        out.append(UInt8(value & 0xff))
        out.append(UInt8((value >> 8) & 0xff))
    }
    private static func appendLE(_ out: inout Data, _ value: UInt32) {
        for i in 0 ..< 4 {
            out.append(UInt8((value >> (i * 8)) & 0xff))
        }
    }
    private static func appendLE(_ out: inout Data, _ value: UInt64) {
        for i in 0 ..< 8 {
            out.append(UInt8((value >> (i * 8)) & 0xff))
        }
    }

    private static func readLE<T: FixedWidthInteger & UnsignedInteger>(
        _ c: inout Cursor, _: T.Type
    ) throws -> T {
        let size = MemoryLayout<T>.size
        guard c.remaining >= size else { throw DecodeError.unexpectedEnd }
        var out: T = 0
        for i in 0 ..< size {
            out |= T(c.data[c.offset + i]) << (i * 8)
        }
        c.offset += size
        return out
    }
}
