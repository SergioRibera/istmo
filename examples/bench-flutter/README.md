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

This is a vanilla Flutter 3.35+ project. Create the platform scaffolding
(the `android/`, `ios/`, `linux/` etc. folders this repo doesn't track
by itself) with:

```
cd examples/bench-flutter
flutter create --org dev.istmo --project-name bench_flutter .
cp -r android android.generated
# Then merge our AndroidManifest / MainActivity on top of the generated
# tree, keeping Gradle's defaults intact.
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
