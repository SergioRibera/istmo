// swift-tools-version:5.9
//
// Reference implementation of the Swift-side istmo runtime.
//
// Standalone Swift package so app targets consume it via SPM the same way
// they'd consume the framework once we ship one. Kept minimal and hand-
// written; `istmo-build` will emit the `<Type>Client.swift` files that
// depend on this package, so the surface here is deliberately stable.
//
// The transport is a pair of C symbols the Rust staticlib exports:
//
// * `istmo_ios_start(callbacks)` — Swift hands over a callback table;
//   the Rust pump calls back into Swift for every outbound Frame.
// * `istmo_ios_submit_*` — Swift pushes inbound Frames (Response,
//   Event, StreamEnd, Call for Rust-hosted plugins, EarlyEvent).
//
// The demo app target links `libistmo_ios_demo.a` (built by cargo) which
// provides those symbols. This package only declares the Swift shape.

import PackageDescription

let package = Package(
    name: "IstmoRuntime",
    platforms: [
        .iOS(.v14),
    ],
    products: [
        .library(name: "IstmoRuntime", targets: ["IstmoRuntime"]),
    ],
    targets: [
        .target(
            name: "IstmoRuntime",
            path: "Sources/IstmoRuntime"
        ),
    ]
)
