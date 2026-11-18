// Harness that times three Flutter interop paths:
//
// 1. MethodChannel (classic) — JSON-free `StandardMethodCodec` echo.
// 2. Pigeon-generated class with `useJni: true` on Android and
//    `useFfi: true` on iOS. Lands after `dart run pigeon --input
//    lib/bench_api.dart` has produced `lib/generated/bench_api.g.dart`
//    and the native sides implement `BenchHost`. Until then the
//    constructor call throws at runtime; the UI reports it.
//
// Results are rendered in a monospace block ready to copy into the
// blog.

import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

// Uncomment after running `dart run pigeon --input lib/bench_api.dart`:
// import 'generated/bench_api.g.dart';

void main() {
  runApp(const BenchApp());
}

class BenchApp extends StatelessWidget {
  const BenchApp({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'istmo bench comparison',
        home: const BenchHome(),
      );
}

class BenchHome extends StatefulWidget {
  const BenchHome({super.key});

  @override
  State<BenchHome> createState() => _BenchHomeState();
}

class _BenchHomeState extends State<BenchHome> {
  static const _channel = MethodChannel('dev.istmo.bench/echo');
  static const _batchIters = 1000;
  static const _batches = 100;
  static const _payloadSizes = [0, 64, 1024, 65536];

  String _output = '';
  bool _running = false;

  Future<void> _run() async {
    setState(() {
      _running = true;
      _output = 'running…';
    });
    final buf = StringBuffer();
    buf.writeln(
        'mode              payload     p50        p95        p99        min        max');
    buf.writeln(
        '---------------------------------------------------------------------------');

    for (final size in _payloadSizes) {
      final payload = Uint8List(size);
      for (var i = 0; i < size; i++) {
        payload[i] = i & 0xff;
      }
      final mc = await _timeMethodChannel(payload);
      buf.writeln(_format('MethodChannel', size, mc));

      // Enable once the generated bindings are present:
      // final pg = await _timePigeon(payload);
      // buf.writeln(_format('Pigeon FFI/JNI', size, pg));
    }

    setState(() {
      _running = false;
      _output = buf.toString();
    });
  }

  Future<BenchStats> _timeMethodChannel(Uint8List payload) async {
    final samples = List<int>.filled(_batches, 0);
    // Warm-up.
    await _channel.invokeMethod<Uint8List>('echo', payload);
    for (var b = 0; b < _batches; b++) {
      final start = DateTime.now().microsecondsSinceEpoch;
      for (var i = 0; i < _batchIters; i++) {
        await _channel.invokeMethod<Uint8List>('echo', payload);
      }
      final end = DateTime.now().microsecondsSinceEpoch;
      samples[b] = (end - start) * 1000 ~/ _batchIters; // ns/iter
    }
    return BenchStats.fromSamples(samples);
  }

  // Future<BenchStats> _timePigeon(Uint8List payload) async {
  //   final host = BenchHost();
  //   final samples = List<int>.filled(_batches, 0);
  //   await host.echo(payload); // warm-up
  //   for (var b = 0; b < _batches; b++) {
  //     final start = DateTime.now().microsecondsSinceEpoch;
  //     for (var i = 0; i < _batchIters; i++) {
  //       await host.echo(payload);
  //     }
  //     final end = DateTime.now().microsecondsSinceEpoch;
  //     samples[b] = (end - start) * 1000 ~/ _batchIters;
  //   }
  //   return BenchStats.fromSamples(samples);
  // }

  String _format(String mode, int payload, BenchStats s) =>
      '${mode.padRight(15)} ${payload.toString().padLeft(7)}B  '
      '${s.p50.toString().padLeft(6)} ns '
      '${s.p95.toString().padLeft(6)} ns '
      '${s.p99.toString().padLeft(6)} ns '
      '${s.min.toString().padLeft(6)} ns '
      '${s.max.toString().padLeft(6)} ns';

  @override
  Widget build(BuildContext context) => Scaffold(
        appBar: AppBar(title: const Text('Flutter interop bench')),
        body: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              FilledButton(
                onPressed: _running ? null : _run,
                child: const Text('Run benchmark'),
              ),
              const SizedBox(height: 16),
              Expanded(
                child: SingleChildScrollView(
                  scrollDirection: Axis.horizontal,
                  child: Text(
                    _output,
                    style: const TextStyle(
                      fontFamily: 'monospace',
                      fontSize: 11,
                    ),
                  ),
                ),
              ),
            ],
          ),
        ),
      );
}

class BenchStats {
  BenchStats({
    required this.p50,
    required this.p95,
    required this.p99,
    required this.min,
    required this.max,
  });

  factory BenchStats.fromSamples(List<int> samples) {
    final sorted = List<int>.from(samples)..sort();
    return BenchStats(
      p50: sorted[sorted.length ~/ 2],
      p95: sorted[(sorted.length * 95) ~/ 100],
      p99: sorted[(sorted.length * 99) ~/ 100],
      min: sorted.first,
      max: sorted.last,
    );
  }

  final int p50;
  final int p95;
  final int p99;
  final int min;
  final int max;
}
