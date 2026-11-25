/*
 * C ABI of the istmo iOS transport (`istmo-ios` crate).
 *
 * The Rust side is the source of truth; `crates/ios/tests/header_abi.rs`
 * fails when this file and `crates/ios/src/cdecl_exports.rs` drift
 * (callback table fields, exported functions, parameter counts).
 *
 * Buffers: every `(const uint8_t *ptr, size_t len)` pair must point at
 * `len` readable bytes, or be `(NULL, 0)`. Buffers handed to a callback
 * are borrowed for the duration of that call only; copy them out before
 * returning. Strings are UTF-8 without a terminator.
 */

#ifndef ISTMO_FFI_H
#define ISTMO_FFI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*IstmoOnCallFn)(
    void *ctx,
    uint64_t call_id,
    const uint8_t *plugin_id_utf8,
    size_t plugin_id_len,
    uint64_t instance_id,
    const uint8_t *method_utf8,
    size_t method_len,
    const uint8_t *payload,
    size_t payload_len);

typedef void (*IstmoOnCancelFn)(void *ctx, uint64_t call_id);

typedef void (*IstmoOnCreateInstanceFn)(
    void *ctx,
    uint64_t call_id,
    const uint8_t *plugin_id_utf8,
    size_t plugin_id_len,
    const uint8_t *payload,
    size_t payload_len);

typedef void (*IstmoOnDestroyInstanceFn)(void *ctx, uint64_t instance_id);

typedef void (*IstmoOnRespondFn)(
    void *ctx,
    uint64_t call_id,
    bool ok,
    const uint8_t *payload,
    size_t payload_len);

typedef void (*IstmoOnEventFn)(
    void *ctx,
    uint64_t stream_id,
    const uint8_t *payload,
    size_t payload_len);

/* reason: 0 = complete, 1 = cancelled, 2 = error (payload = error bytes). */
typedef void (*IstmoOnStreamEndFn)(
    void *ctx,
    uint64_t stream_id,
    uint32_t reason,
    const uint8_t *err_payload,
    size_t err_payload_len);

typedef void (*IstmoOnReleaseNativeHandleFn)(void *ctx, uint64_t handle_id);

typedef void (*IstmoOnNotifyFn)(
    void *ctx,
    const uint8_t *plugin_id_utf8,
    size_t plugin_id_len,
    uint64_t instance_id,
    const uint8_t *method_utf8,
    size_t method_len,
    const uint8_t *payload,
    size_t payload_len);

/*
 * Outbound callback table. Every function pointer is required:
 * `istmo_ios_start` returns false when any is NULL. Callbacks run on the
 * transport's pump thread, one at a time. `ctx` is passed back verbatim
 * and must stay valid until `istmo_ios_shutdown` returns true.
 */
typedef struct IstmoIosCallbacks {
    void *ctx;
    IstmoOnCallFn on_call;
    IstmoOnCancelFn on_cancel;
    IstmoOnCreateInstanceFn on_create_instance;
    IstmoOnDestroyInstanceFn on_destroy_instance;
    IstmoOnRespondFn on_respond;
    IstmoOnEventFn on_event;
    IstmoOnStreamEndFn on_stream_end;
    IstmoOnReleaseNativeHandleFn on_release_native_handle;
    IstmoOnNotifyFn on_notify;
} IstmoIosCallbacks;

/* Starts the runtime and the pump thread. False on a NULL callback, a
 * second start, or a startup failure. */
bool istmo_ios_start(IstmoIosCallbacks callbacks);

/* Stops the runtime. True when the pump thread was joined, i.e. no
 * callback is running or will run again and `ctx` may be released.
 * False when the transport was not running or when called from a
 * callback (the pump then exits once that callback returns). */
bool istmo_ios_shutdown(void);

void istmo_ios_submit_response(
    uint64_t call_id,
    bool ok,
    const uint8_t *payload,
    size_t payload_len);

void istmo_ios_submit_event(
    uint64_t stream_id,
    const uint8_t *payload,
    size_t payload_len);

/* reason: same encoding as `IstmoOnStreamEndFn`. */
void istmo_ios_submit_stream_end(
    uint64_t stream_id,
    uint32_t reason,
    const uint8_t *err_payload,
    size_t err_payload_len);

/* instance_id 0 = stateless call. */
void istmo_ios_submit_call(
    uint64_t call_id,
    const uint8_t *plugin_id_utf8,
    size_t plugin_id_len,
    uint64_t instance_id,
    const uint8_t *method_utf8,
    size_t method_len,
    const uint8_t *payload,
    size_t payload_len);

/* Answer bytes `istmo_ios_call` hands back: copy them out, then return
 * the buffer with `istmo_ios_free_reply`. */
typedef struct IstmoIosReply {
    uint8_t *ptr;
    size_t len;
    size_t cap;
} IstmoIosReply;

/* `istmo_ios_call` results. PENDING: the answer comes through
 * `on_respond` under the call id; `reply` is left empty. */
enum {
    ISTMO_IOS_CALL_PENDING = 0,
    ISTMO_IOS_CALL_OK = 1,
    ISTMO_IOS_CALL_ERR = 2,
};

/* Like `istmo_ios_submit_call`, but a host that answers before returning
 * (an inline host that did not wait) answers here: OK or ERR with the
 * bytes in `*reply`. instance_id 0 = stateless call. */
int32_t istmo_ios_call(
    uint64_t call_id,
    const uint8_t *plugin_id_utf8,
    size_t plugin_id_len,
    uint64_t instance_id,
    const uint8_t *method_utf8,
    size_t method_len,
    const uint8_t *payload,
    size_t payload_len,
    IstmoIosReply *reply);

/* Frees the bytes `istmo_ios_call` left in `reply` and empties it. */
void istmo_ios_free_reply(IstmoIosReply *reply);

/* Cooperatively cancels a call started with `istmo_ios_submit_call` or
 * `istmo_ios_call`. A late or unknown call_id is ignored. */
void istmo_ios_submit_cancel(uint64_t call_id);

/* Fire-and-forget call: no response follows. instance_id 0 = stateless. */
void istmo_ios_submit_notify(
    const uint8_t *plugin_id_utf8,
    size_t plugin_id_len,
    uint64_t instance_id,
    const uint8_t *method_utf8,
    size_t method_len,
    const uint8_t *payload,
    size_t payload_len);

void istmo_ios_submit_early_latest(
    const uint8_t *channel_utf8,
    size_t channel_len,
    const uint8_t *payload,
    size_t payload_len);

void istmo_ios_submit_early_queue(
    const uint8_t *channel_utf8,
    size_t channel_len,
    uint32_t capacity,
    const uint8_t *payload,
    size_t payload_len);

/*
 * Native entry version (`NATIVE_ABI_VERSION`); the Swift runtime refuses
 * to start against a library reporting another one.
 */
uint32_t istmo_abi_version(void);

/*
 * Frame sinks. Submits return an `IstmoSubmitStatus` code; the values
 * mirror Rust's `SubmitStatus` (a test keeps them in sync).
 */
enum {
    ISTMO_SUBMIT_ACCEPTED = 0,
    ISTMO_SUBMIT_DISPLACED = 1,
    ISTMO_SUBMIT_CLOSED = 2,
    ISTMO_SUBMIT_UNKNOWN_SINK = 3,
    ISTMO_SUBMIT_BAD_META = 4,
    ISTMO_SUBMIT_UNSUPPORTED = 5,
    ISTMO_SUBMIT_NOT_STARTED = 6,
    ISTMO_SUBMIT_INVALID = 7,
    ISTMO_SUBMIT_FULL = 8,
    ISTMO_SUBMIT_INTERNAL = 9,
    ISTMO_SUBMIT_TYPE_MISMATCH = 10,
};

/* `kind` of a retained object. */
enum {
    ISTMO_RETAINED_OTHER = 0,
    ISTMO_RETAINED_CV_PIXEL_BUFFER = 1,
    ISTMO_RETAINED_IO_SURFACE = 2,
    ISTMO_RETAINED_CM_SAMPLE_BUFFER = 3,
};

/* One producer session on a sink, owned by Swift's `IstmoFrameSender`. */
typedef struct IstmoFrameProducer IstmoFrameProducer;

typedef void (*IstmoReleaseFn)(void *object);

/* Open a producer; NULL with the reason in `*status` on failure.
 * `fingerprint` names the metadata type (0: unchecked). */
IstmoFrameProducer * istmo_ios_open_frame_producer(
    uint64_t sink,
    uint64_t fingerprint,
    int32_t *status);

/* End the stream: completed when `error` is NULL, failed with the UTF-8
 * message otherwise. The handle stays valid until freed. */
void istmo_ios_end_frame_producer(
    const IstmoFrameProducer *producer,
    const uint8_t *error,
    size_t error_len);

/* Free the handle; never use it afterwards. */
void istmo_ios_free_frame_producer(IstmoFrameProducer *producer);

/*
 * Takes over the +1 reference in `object` whatever the status, releasing
 * it through `release` (on any thread) once the frame drops or at once
 * when it is refused. Only a NULL `object` or `release` leaves ownership
 * with the caller (status INVALID).
 */
int32_t istmo_ios_submit_native_buffer(
    const IstmoFrameProducer *producer,
    uint32_t kind,
    void *object,
    IstmoReleaseFn release,
    const uint8_t *meta,
    size_t meta_len);

/* Copies `(bytes, len)` into a frame. */
int32_t istmo_ios_submit_frame_bytes(
    const IstmoFrameProducer *producer,
    const uint8_t *bytes,
    size_t len,
    const uint8_t *meta,
    size_t meta_len);

#ifdef __cplusplus
}
#endif

#endif /* ISTMO_FFI_H */
