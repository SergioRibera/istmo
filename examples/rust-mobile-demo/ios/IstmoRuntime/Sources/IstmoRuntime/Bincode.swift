// Minimal port of the bincode 2 "standard" configuration (VarintEncoding +
// LittleEndian) that the istmo wire format uses.
//
// Only the shapes the M5 demo actually needs are covered:
//
// * varint u64 / i64 / u32 / i32 (zigzag for signed, unsigned varint for
//   unsigned)
// * varint-length-prefixed String (UTF-8)
// * varint-length-prefixed Data
// * Bool as a single byte (0/1)
//
// Extend per plugin as the surface grows. See
// https://docs.rs/bincode/2/bincode/config/ for the reference config.

import Foundation

public enum Bincode {

    // MARK: - Writer

    public static func writeBool(_ out: inout Data, _ value: Bool) {
        out.append(value ? 1 : 0)
    }

    public static func writeU8(_ out: inout Data, _ value: UInt8) {
        out.append(value)
    }

    /// Bincode 2 "standard" varint encoding for unsigned integers:
    /// values < 251 fit in one byte; larger values are tagged 251/252/253/254
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

    public static func writeString(_ out: inout Data, _ value: String) {
        let bytes = Array(value.utf8)
        writeVarintU64(&out, UInt64(bytes.count))
        out.append(contentsOf: bytes)
    }

    public static func writeData(_ out: inout Data, _ value: Data) {
        writeVarintU64(&out, UInt64(value.count))
        out.append(value)
    }

    /// Overload sugar so generated `Bincode.encode(&payload, x)` calls line
    /// up with the shapes the demo covers. Add overloads per new type.
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

    /// Sugar mirroring the `Bincode.decode(bytes) as T` shape the generator
    /// emits. Currently supports `String`; extend per new type.
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
