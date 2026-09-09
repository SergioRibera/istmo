// REFERENCE — copy into your Android module under `src/main/aidl/dev/istmo/remote/`.
//
// Wire shape for the `:remote` bridge. Both the app process and the
// `:remote` process implement this Binder interface; each side calls
// `submitEnvelope(bytes)` to push a bincoded Envelope over Binder into
// the other runtime's `inject_wire_envelope(bytes)`.

package dev.istmo.remote;

interface IIstmoBridge {
    /**
     * Ship a bincoded istmo Envelope to the receiver. The receiver
     * pipes the bytes into `Runtime::inject_wire_envelope` on its
     * cdylib. Fire-and-forget from the caller's perspective — the
     * envelope itself carries any correlation id (`CallId` /
     * `StreamId`) needed to route the response back through a
     * subsequent `submitEnvelope` in the opposite direction.
     */
    oneway void submitEnvelope(in byte[] envelope);
}
