// Stand-in for the CoreMedia API (and the CoreAudioTypes it re-exports)
// the screen-capture native backend uses: sample buffers, their
// attachments, timestamps, clocks and audio data.

@_exported import CoreVideo
import Foundation

public typealias OSStatus = Int32
public typealias CMItemCount = Int
public typealias CMAttachmentMode = UInt32
public typealias AudioFormatID = UInt32
public typealias AudioFormatFlags = UInt32

/// `CMSampleBuffer` (a Core Foundation type: a class).
public final class CMSampleBuffer {}
/// `CMFormatDescription` (a Core Foundation type: a class).
public final class CMFormatDescription {}
public typealias CMAudioFormatDescription = CMFormatDescription
/// `CMBlockBuffer` (a Core Foundation type: a class).
public final class CMBlockBuffer {}
/// `CMClock` (a Core Foundation type: a class).
public final class CMClock {}

public struct CMTimeFlags: OptionSet, Sendable {
    public let rawValue: UInt32
    public init(rawValue: UInt32) { self.rawValue = rawValue }
    public static let valid = CMTimeFlags(rawValue: 1)
}

public struct CMTime: Sendable {
    public var value: Int64
    public var timescale: Int32
    public var flags: CMTimeFlags
    public var epoch: Int64
    public init(value: Int64, timescale: Int32) {
        self.value = value
        self.timescale = timescale
        flags = .valid
        epoch = 0
    }
    public static var zero: CMTime { CMTime(value: 0, timescale: 1) }
    public var isValid: Bool { flags.contains(.valid) }
    public var seconds: Double { timescale == 0 ? 0 : Double(value) / Double(timescale) }
}

public struct AudioStreamBasicDescription: Sendable {
    public var mSampleRate: Double
    public var mFormatID: AudioFormatID
    public var mFormatFlags: AudioFormatFlags
    public var mBytesPerPacket: UInt32
    public var mFramesPerPacket: UInt32
    public var mBytesPerFrame: UInt32
    public var mChannelsPerFrame: UInt32
    public var mBitsPerChannel: UInt32
    public var mReserved: UInt32
}

public var kAudioFormatLinearPCM: AudioFormatID { 0x6C70_636D }
public var kAudioFormatFlagIsFloat: AudioFormatFlags { 1 << 0 }
public var kAudioFormatFlagIsBigEndian: AudioFormatFlags { 1 << 1 }
public var kAudioFormatFlagIsSignedInteger: AudioFormatFlags { 1 << 2 }
public var kAudioFormatFlagIsNonInterleaved: AudioFormatFlags { 1 << 5 }

public func CMClockGetHostTimeClock() -> CMClock { CMClock() }
public func CMClockGetTime(_ clock: CMClock) -> CMTime { .zero }

public func CMSampleBufferIsValid(_ sbuf: CMSampleBuffer) -> Bool { false }
public func CMSampleBufferDataIsReady(_ sbuf: CMSampleBuffer) -> Bool { false }
public func CMSampleBufferGetImageBuffer(_ sbuf: CMSampleBuffer) -> CVImageBuffer? { nil }
public func CMSampleBufferGetPresentationTimeStamp(_ sbuf: CMSampleBuffer) -> CMTime { .zero }
public func CMSampleBufferGetNumSamples(_ sbuf: CMSampleBuffer) -> CMItemCount { 0 }
public func CMSampleBufferGetFormatDescription(_ sbuf: CMSampleBuffer) -> CMFormatDescription? { nil }
public func CMSampleBufferGetDataBuffer(_ sbuf: CMSampleBuffer) -> CMBlockBuffer? { nil }
public func CMSampleBufferGetSampleAttachmentsArray(_ sbuf: CMSampleBuffer, createIfNecessary: Bool) -> CFArray? { nil }
public func CMAudioFormatDescriptionGetStreamBasicDescription(_ desc: CMAudioFormatDescription)
    -> UnsafePointer<AudioStreamBasicDescription>? { nil }
public func CMGetAttachment(_ target: CFTypeRef, key: CFString,
                            attachmentModeOut: UnsafeMutablePointer<CMAttachmentMode>?) -> CFTypeRef? { nil }

public func CMBlockBufferGetDataLength(_ theBuffer: CMBlockBuffer) -> Int { 0 }
public func CMBlockBufferCopyDataBytes(_ theSourceBuffer: CMBlockBuffer, atOffset offsetToData: Int,
                                       dataLength: Int, destination: UnsafeMutableRawPointer) -> OSStatus { 0 }

public struct AudioBuffer {
    public var mNumberChannels: UInt32
    public var mDataByteSize: UInt32
    public var mData: UnsafeMutableRawPointer?
}

public struct AudioBufferList {
    public var mNumberBuffers: UInt32
    public var mBuffers: AudioBuffer
    public static func sizeInBytes(maximumBuffers: Int) -> Int { 0 }
    public static func allocate(maximumBuffers: Int) -> UnsafeMutableAudioBufferListPointer {
        UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer<AudioBufferList>.allocate(capacity: 1))
    }
}

public struct UnsafeMutableAudioBufferListPointer: RandomAccessCollection {
    public var unsafeMutablePointer: UnsafeMutablePointer<AudioBufferList>
    public init(_ p: UnsafeMutablePointer<AudioBufferList>) { unsafeMutablePointer = p }
    public var startIndex: Int { 0 }
    public var endIndex: Int { 0 }
    public subscript(index: Int) -> AudioBuffer {
        AudioBuffer(mNumberChannels: 0, mDataByteSize: 0, mData: nil)
    }
}

public var kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment: UInt32 { 1 << 0 }

public func CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
    _ sbuf: CMSampleBuffer, bufferListSizeNeededOut: UnsafeMutablePointer<Int>?,
    bufferListOut: UnsafeMutablePointer<AudioBufferList>?, bufferListSize: Int,
    blockBufferAllocator: CFAllocator?, blockBufferMemoryAllocator: CFAllocator?, flags: UInt32,
    blockBufferOut: UnsafeMutablePointer<CMBlockBuffer?>?
) -> OSStatus { 0 }

public var kCMSampleAttachmentKey_DisplayImmediately: CFString { NSObject() }
