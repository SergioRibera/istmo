// Pigeon definition. Run `dart run pigeon --input lib/bench_api.dart` to
// regenerate the Dart + Kotlin + Swift bindings into `lib/generated/`,
// `android/.../BenchPigeon.kt` and `ios/Runner/BenchPigeon.swift`.
//
// The host method is a trivial echo: it returns the bytes it was
// handed. The whole point of this binding is to measure how long a
// one-method call costs through the MethodChannel path and (when
// enabled) the native-interop path Flutter 3.50 shipped.

import 'package:pigeon/pigeon.dart';

@ConfigurePigeon(
  PigeonOptions(
    dartOut: 'lib/generated/bench_api.g.dart',
    dartPackageName: 'bench_flutter',
    kotlinOut:
        'android/app/src/main/kotlin/dev/istmo/bench_flutter/BenchPigeon.kt',
    kotlinOptions: KotlinOptions(
      package: 'dev.istmo.bench_flutter',
      useJni: true,
    ),
    swiftOut: 'ios/Runner/BenchPigeon.swift',
    swiftOptions: SwiftOptions(useFfi: true),
  ),
)
@HostApi()
abstract class BenchHost {
  Uint8List echo(Uint8List payload);
}
