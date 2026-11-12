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

/* Cooperatively cancels a call started with `istmo_ios_submit_call`.
 * A late or unknown call_id is ignored. */
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

#ifdef __cplusplus
}
#endif

#endif /* ISTMO_FFI_H */
