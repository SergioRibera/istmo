# bench-flutter

Call-overhead baseline for the blog. Runs the same echo benchmark on
Flutter's two interop paths, so the numbers can sit next to
[`bench-android`](../bench-android) in the comparison table.

## Paths measured

| Path | Status |
|---|---|
| MethodChannel (classic `StandardMethodCodec`) | always on |
| Pigeon with `useFfi` / `useJni` (Flutter 3.50) | opt-in, see below |

## Setup

### One shot through Nix

```
nix run .#bench-flutter
```

First invocation runs `flutter create` and then stops with a note
asking you to merge our `AndroidManifest.xml`, `MainActivity.kt` and
`AppDelegate.swift` on top of the generated scaffold. Re-run it and
the wrapper does `flutter pub get`, `dart run pigeon`, and
`flutter build apk --release`. The APK lands at
`examples/bench-flutter/build/app/outputs/flutter-apk/app-release.apk`.

### Interactive via the Nix devshell

```
nix develop .#bench-flutter
cd examples/bench-flutter
flutter create --org dev.istmo --project-name bench_flutter .
# merge our AndroidManifest / MainActivity / AppDelegate on top of the
# generated android/ and ios/ trees.
```

### Without Nix

Flutter 3.35+, Android SDK 35 + NDK 26 + JDK 17 on the `PATH`:

```
cd examples/bench-flutter
flutter create --org dev.istmo --project-name bench_flutter .
# merge AndroidManifest + MainActivity + AppDelegate on top of the
# generated platform trees, keeping Gradle's defaults intact.
```

Enable the Pigeon-generated bindings:

```
dart run pigeon --input lib/bench_api.dart
```

That writes:

- `lib/generated/bench_api.g.dart`
- `android/app/src/main/kotlin/dev/istmo/benchflutter/BenchPigeon.kt`
- `ios/Runner/BenchPigeon.swift`

Uncomment the two blocks in `lib/main.dart`, `MainActivity.kt`, and
`AppDelegate.swift` so the Pigeon host is registered and the Dart side
times it.

## Building

```
flutter run --release
```

Tap "Run benchmark". Results render in a monospace block.

## Why MethodChannel in release mode is still slow

The `StandardMethodCodec` serialises every call; Dart isolates and the
platform thread do not share memory. Our measurements on recent Android
devices land around 100–300 µs per round trip even in release builds.
The Pigeon FFI/JNI path lands two to three orders of magnitude below
that, which is the fair comparison for istmo's Inline / Typed numbers.

## What NOT to compare directly

- `MethodChannel` is Flutter's legacy path. Showing it next to istmo's
  Typed shortcut is a 10 000× win that will mislead a reader. Keep it
  in the table as context but headline the comparison against Pigeon
  FFI/JNI.
- Pigeon currently only generates FFI on Apple and JNI on Android.
  Linux and Windows still go through MethodChannel. Note this in the
  blog so a Windows-first reader doesn't feel misled.
