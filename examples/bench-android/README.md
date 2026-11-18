# bench-android

On-device istmo call-overhead benchmark. Compares `DispatchMode::Isolated`,
`DispatchMode::Inline`, and the typed in-process shortcut across four
payload sizes, reporting p50 / p95 / p99 in nanoseconds.

## What it measures

Each run does 1 000 echo roundtrips, 100 batches per cell, 400 cells per
device. Warm-up batch is discarded. For each cell the harness records
total ns, min, max, and three percentiles.

Payload sizes: 0, 64, 1024, 65 536 bytes.
Modes: `Isolated`, `Inline`, `Typed`.

The Rust side is a single cdylib: three echo hosts (one per mode) plus a
typed shortcut registered on the inline one. The Kotlin side is a tiny
`Activity` with a Run button and a monospace output pane.

## Building

Build the Rust cdylib for each target ABI and drop the shared objects
into the Gradle project:

```
cd examples/bench-android
cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p bench-android --release
cd android
./gradlew :app:installRelease
```

Open the app on the device and press "Run benchmark matrix". Results
appear in a monospace table ready to paste into the blog.

## Device matrix for the blog

Minimum useful coverage:

| Tier | Example | Chipset |
|---|---|---|
| Low | Moto G Play (2024) | SD 680 |
| Mid | Pixel 7a | Tensor G2 |
| High | Pixel 9 Pro | Tensor G4 |

Add a flagship Qualcomm (SD 8 Gen 3) when available.

## Caveats

- Debug builds skew heavily — always install the `release` variant.
- Thermal state matters; run with the device cool and plugged in.
- Battery saver / performance mode affect scheduling — leave the device
  on balanced profile.
- Android's Little-Big scheduler can migrate the bench thread between
  cores. The harness does not pin an affinity; variance between batches
  is expected.
