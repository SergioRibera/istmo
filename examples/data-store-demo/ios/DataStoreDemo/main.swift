// Entry point for the data-store demo on iOS.
//
// winit's iOS backend takes over `UIApplicationMain` internally, so we
// live off a plain `main.swift` rather than SwiftUI's `@main App`. The
// Swift side does the bare minimum:
//
// 1. Boots `IstmoRuntime.shared.start()` (installs the transport pump).
// 2. Registers the `DataStoreDispatcher` so Rust-side `Frame::Call` /
//    `Frame::CreateInstance` for `istmo.data_store` reach
//    `DataStoreBackendImpl` (UserDefaults-backed).
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

IstmoRuntime.shared.registerHandler(
    DataStoreDispatcher.PLUGIN_ID,
    DataStoreDispatcher(factory: DataStoreFactoryImpl(), codecs: DataStoreCodecsImpl())
)

_ = istmo_run_ios()
