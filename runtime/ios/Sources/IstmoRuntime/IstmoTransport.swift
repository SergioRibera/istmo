import Foundation

public struct IstmoIosCallbacks {
    public var ctx: UnsafeMutableRawPointer?
    public var on_call: @convention(c) (
        UnsafeMutableRawPointer?,
        UInt64,
        UnsafePointer<UInt8>?,
        Int,
        UInt64,
        UnsafePointer<UInt8>?,
        Int,
        UnsafePointer<UInt8>?,
        Int
    ) -> Void
    public var on_cancel: @convention(c) (
        UnsafeMutableRawPointer?, UInt64
    ) -> Void
    public var on_create_instance: @convention(c) (
        UnsafeMutableRawPointer?,
        UInt64,
        UnsafePointer<UInt8>?, Int,
        UnsafePointer<UInt8>?, Int
    ) -> Void
    public var on_destroy_instance: @convention(c) (
        UnsafeMutableRawPointer?, UInt64
    ) -> Void
    public var on_respond: @convention(c) (
        UnsafeMutableRawPointer?,
        UInt64,
        Bool,
        UnsafePointer<UInt8>?, Int
    ) -> Void
    public var on_event: @convention(c) (
        UnsafeMutableRawPointer?,
        UInt64,
        UnsafePointer<UInt8>?, Int
    ) -> Void
    public var on_stream_end: @convention(c) (
        UnsafeMutableRawPointer?,
        UInt64,
        UInt32,
        UnsafePointer<UInt8>?, Int
    ) -> Void
    public var on_release_native_handle: @convention(c) (
        UnsafeMutableRawPointer?, UInt64
    ) -> Void
}

@_silgen_name("istmo_ios_start")
public func istmo_ios_start(_ callbacks: IstmoIosCallbacks) -> Bool

@_silgen_name("istmo_ios_shutdown")
public func istmo_ios_shutdown()

@_silgen_name("istmo_ios_submit_response")
public func istmo_ios_submit_response(
    _ callId: UInt64,
    _ ok: Bool,
    _ payload: UnsafePointer<UInt8>?,
    _ payloadLen: Int
)

@_silgen_name("istmo_ios_submit_event")
public func istmo_ios_submit_event(
    _ streamId: UInt64,
    _ payload: UnsafePointer<UInt8>?,
    _ payloadLen: Int
)

@_silgen_name("istmo_ios_submit_stream_end")
public func istmo_ios_submit_stream_end(
    _ streamId: UInt64,
    _ reason: UInt32,
    _ errPayload: UnsafePointer<UInt8>?,
    _ errPayloadLen: Int
)

@_silgen_name("istmo_ios_submit_call")
public func istmo_ios_submit_call(
    _ callId: UInt64,
    _ pluginId: UnsafePointer<UInt8>?,
    _ pluginIdLen: Int,
    _ instanceId: UInt64,
    _ method: UnsafePointer<UInt8>?,
    _ methodLen: Int,
    _ payload: UnsafePointer<UInt8>?,
    _ payloadLen: Int
)

@_silgen_name("istmo_ios_submit_early_latest")
public func istmo_ios_submit_early_latest(
    _ channel: UnsafePointer<UInt8>?,
    _ channelLen: Int,
    _ payload: UnsafePointer<UInt8>?,
    _ payloadLen: Int
)

@_silgen_name("istmo_ios_submit_early_queue")
public func istmo_ios_submit_early_queue(
    _ channel: UnsafePointer<UInt8>?,
    _ channelLen: Int,
    _ capacity: UInt32,
    _ payload: UnsafePointer<UInt8>?,
    _ payloadLen: Int
)

