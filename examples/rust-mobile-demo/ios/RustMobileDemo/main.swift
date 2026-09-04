// Entry point for the full-Rust mobile demo on iOS.
//
// Because winit's iOS backend takes over `UIApplicationMain` internally,
// this shell cannot use SwiftUI's `@main App` — that would try to start a
// second `UIApplication`. Instead we live off a plain `main.swift`, do the
// bare minimum on the Swift side (bring the istmo transport up), and hand
// control to Rust via `istmo_run_ios`.
//
// The Rust side (`#[istmo::mobile_app]` in `src/app.rs`) owns everything
// from here on: winit event loop, wgpu surface, egui rendering, and the
// three plugins (permissions / notifications / sign-in).

import Foundation
import IstmoRuntime

// `istmo_run_ios` is emitted by `#[istmo::mobile_app]` in the Rust
// staticlib. The Rust function runs the eframe event loop, which never
// returns — so this `main.swift` never proceeds past the call.
@_silgen_name("istmo_run_ios")
func istmo_run_ios() -> Int32

do {
    try IstmoRuntime.shared.start()
} catch {
    // No transport = no plugins = every UI action fails. Better to crash
    // now with a legible message than to wait for the first plugin call to
    // hit a `IstmoRuntimeError.startFailed`.
    fatalError("IstmoRuntime.start() failed: \(error)")
}

// Register Swift plugin backends before Rust starts the event loop. Once
// `istmo_run_ios` fires, the pump can deliver inbound `Frame::Call`
// frames at any time — a plugin id missing from the registry surfaces as
// a `PluginError` on the Rust side.
IstmoRuntime.shared.registerHandler(
    PermissionsDispatcher.PLUGIN_ID,
    PermissionsDispatcher(backend: PermissionsBackendImpl(), codecs: PermissionsCodecsImpl())
)
IstmoRuntime.shared.registerHandler(
    NotificationsDispatcher.PLUGIN_ID,
    NotificationsDispatcher(backend: NotificationsBackendImpl(), codecs: NotificationsCodecsImpl())
)

// TODO C.5: publish SafeArea insets (uses early events, not a dispatcher).

_ = istmo_run_ios()
