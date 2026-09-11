// Entry point for the live-activity demo on iOS.
//
// winit's iOS backend takes over `UIApplicationMain` internally, so we
// live off a plain `main.swift` rather than SwiftUI's `@main App`. The
// Swift side does the bare minimum:
//
// 1. Boots `IstmoRuntime.shared.start()` (installs the transport pump).
// 2. Registers the `LiveActivityDispatcher` so Rust-side `Frame::Call`
//    for `istmo.live_activity` reaches `LiveActivityBackendImpl` +
//    `TimerLiveActivityHandler` (ActivityKit-backed).
// 3. Calls `istmo_run_ios()` — emitted by `#[istmo::mobile_app]` in the
//    Rust staticlib — which runs the eframe event loop.

import Foundation
import IstmoRuntime

@_silgen_name("istmo_run_ios")
func istmo_run_ios() -> Int32

do {
    try IstmoRuntime.shared.start()
} catch {
    fatalError("IstmoRuntime.start() failed: \(error)")
}

let backend = LiveActivityBackendImpl()
if #available(iOS 16.1, *) {
    backend.register(handler: TimerLiveActivityHandler())
}

IstmoRuntime.shared.registerHandler(
    LiveActivityDispatcher.PLUGIN_ID,
    LiveActivityDispatcher(backend: backend, codecs: LiveActivityCodecsImpl())
)

_ = istmo_run_ios()
