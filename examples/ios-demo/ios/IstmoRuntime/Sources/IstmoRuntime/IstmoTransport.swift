// Binds the C symbols the istmo-ios Rust crate exports.
//
// The linker resolves these against the app target's static archive
// (`libistmo_ios_demo.a` from the cargo build). If the archive is missing,
// the linker error is the intended signal.
//
// All strings crossing the boundary are UTF-8 length-prefixed by (ptr, len)
// pairs. Data buffers are (ptr, len) pairs; Rust copies out synchronously,
// so the pointer only needs to live for the duration of the call.

import Foundation

// MARK: - Callback table

/// C ABI callback table registered with Rust at `istmo_ios_start`. Every
/// field must be non-nil; passing a nil function pointer is undefined
/// behaviour and Rust will crash on the first matching outbound frame.
///
/// `ctx` is passed verbatim to each callback. `IstmoRuntime.start()` fills
/// it with `Unmanaged.passUnretained(runtime).toOpaque()`; each callback
/// recovers the singleton via `Unmanaged.fromOpaque(ctx).takeUnretainedValue()`.
public struct IstmoIosCallbacks {
    public var ctx: UnsafeMutableRawPointer?
    public var on_call: @convention(c) (
        UnsafeMutableRawPointer?, // ctx
        UInt64,                    // call_id
        UnsafePointer<UInt8>?,     // plugin_id utf8
        Int,                       // plugin_id len
        UInt64,                    // instance_id (0 encodes None)
        UnsafePointer<UInt8>?,     // method utf8
        Int,                       // method len
        UnsafePointer<UInt8>?,     // payload
        Int                        // payload len
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
        UInt64,                    // call_id
        Bool,                      // ok
        UnsafePointer<UInt8>?, Int // payload
    ) -> Void
    public var on_event: @convention(c) (
        UnsafeMutableRawPointer?,
        UInt64,                    // stream_id
        UnsafePointer<UInt8>?, Int // payload
    ) -> Void
    public var on_stream_end: @convention(c) (
        UnsafeMutableRawPointer?,
        UInt64,                    // stream_id
        UInt32,                    // reason: 0 Complete, 1 Cancelled, 2 Error
        UnsafePointer<UInt8>?, Int // err payload
    ) -> Void
    public var on_release_native_handle: @convention(c) (
        UnsafeMutableRawPointer?, UInt64
    ) -> Void
}

// MARK: - Extern C symbols (Rust → Swift)

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
