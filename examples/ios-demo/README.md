# istmo iOS demo

Minimal SwiftUI app that echoes a string through a Rust-hosted `Echo`
plugin. Every character round-trips across the frame protocol via the
`istmo-ios` transport crate. This is the iOS analogue of
`examples/android-demo/`.

## Layout

```
examples/ios-demo/
├── Cargo.toml                     staticlib package (libistmo_ios_demo.a)
├── src/lib.rs                     Echo trait, EchoImpl, istmo::runtime!
└── ios/
    ├── project.yml                xcodegen spec
    ├── build-rust.sh              Xcode pre-build phase → cargo per arch
    ├── IstmoDemo/                 App sources
    │   ├── App.swift              @main + IstmoRuntime.shared.start()
    │   ├── ContentView.swift      Text field + button + result label
    │   ├── EchoClient.swift       Hand-written mirror of generate_swift_client
    │   ├── Info.plist
    │   └── Assets.xcassets/
    └── IstmoRuntime/              SPM package (Swift-side runtime)
        ├── Package.swift
        └── Sources/IstmoRuntime/
            ├── IstmoRuntime.swift      Singleton, call/stream, continuations map
            ├── IstmoTransport.swift    @_silgen_name for istmo_ios_* + callback trampolines
            ├── Bincode.swift           Varint + string + data primitives
            └── PluginException.swift
```

## Prerequisites

Everything runs on macOS with Xcode. Linux authoring works for the Rust
side, but building the app requires the Apple toolchain.

* Xcode 15 or newer with an iOS 14+ simulator installed.
* `xcodegen` — `brew install xcodegen`.
* Rust targets:
  * `rustup target add aarch64-apple-ios` — device.
  * `rustup target add aarch64-apple-ios-sim` — Apple Silicon simulator.
  * (Optional) `rustup target add x86_64-apple-ios` — Intel simulator.

## Build + run

From the repo root:

```sh
just ios-demo
```

That runs `xcodegen generate` (if the `.xcodeproj` is missing), then
`xcodebuild build`, then `xcrun simctl install` + `launch` against the
booted simulator. Bring one up first with
`xcrun simctl boot 'iPhone 15'` if needed.

Or step by step:

```sh
just ios-bootstrap       # xcodegen generate (one-shot per project.yml change)
just ios-build           # xcodebuild build (Debug, iPhone 15 simulator)
just ios-run             # simctl install + launch
```

Prefer Xcode? Open `examples/ios-demo/ios/IstmoDemo.xcodeproj` after
`ios-bootstrap`. The pre-build phase runs `build-rust.sh` on every
build; a warm cargo cache turns it into a ~50 ms no-op per arch.

## What the demo exercises

1. Rust hosts `Echo::echo(text) -> Result<String, EchoError>`.
2. Swift `EchoClient.echo(text)` bincode-encodes the argument, calls
   `IstmoRuntime.shared.call(pluginId, "echo", payload)`.
3. `istmo_ios_submit_call` hands the frame to Rust; the runtime routes
   it to the hosted `EchoImpl`.
4. Rust encodes the reply and returns it via `Frame::Respond`.
5. The pump thread invokes `on_respond`, which resumes the pending
   `CheckedContinuation` on the Swift side.
6. UI shows the result. Domain errors (`empty input`) surface as
   `EchoException`; runtime errors as `runtime error: …`.

Every step goes through the frame protocol. No `@_cdecl` or extern C
symbol lives in the demo app itself — the `IstmoRuntime` SPM package
owns all FFI.

## Follow-ups

* Codegen `EchoClient.swift` from `istmo-build`'s
  `generate_swift_client` output instead of hand-writing it. Golden
  fixture already pins the contract.
* Extend `Echo` with methods that internally consume iOS-side
  platform plugins once those land (`PushClient`, `HealthClient`, …).
* Replace the Bincode.swift primitive-only port with a fuller port as
  new payload shapes appear.
