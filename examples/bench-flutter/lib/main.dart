// Flutter-side harness. Measures four paths, mirroring the shape the
// Android harness uses so the two tables can sit next to each other:
//
//   JniNoop         — MethodChannel with no payload at all.
//   JniNoopBytes    — MethodChannel + ByteArray decode, no echo back.
//   MethodChannel   — the current echo baseline.
//   Pigeon          — Pigeon FFI/JNI host (opt-in; uncomment the import
//                     and the `_timePigeon` call once `dart run pigeon`
//                     has produced `lib/generated/bench_api.g.dart`).
//
// Every finished line is pushed through the `report` channel so the
// Kotlin / Swift side can echo it into logcat / os_log, which the host
// shell script scrapes between `BENCH_BEGIN` and `BENCH_END`.

import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

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
  static const _echo = MethodChannel('dev.istmo.bench/echo');
  static const _report = MethodChannel('dev.istmo.bench/report');
  static const _batchIters = 1000;
  static const _batches = 100;
  static const _payloadSizes = [0, 64, 1024, 65536];

  String _output = '';
  bool _running = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) async {
      final auto = (await _report.invokeMethod<bool>('shouldAutoRun')) ?? false;
      if (auto && !_running) _run();
    });
  }

  Future<void> _run() async {
    setState(() {
      _running = true;
      _output = 'running…';
    });
    await _report.invokeMethod('begin');

    final buf = StringBuffer();
    final header = 'mode             payload    p50       p95       p99       '
        'min       max';
    final sep = '-' * header.length;
    buf.writeln(header);
    buf.writeln(sep);
    await _report.invokeMethod('line', header);
    await _report.invokeMethod('line', sep);

    // Baselines first.
    final noopStats = await _timeMethodChannel('noop', Uint8List(0));
    await _emit(buf, 'JniNoop', 0, noopStats);

    for (final size in _payloadSizes) {
      final payload = _makePayload(size);
      final stats = await _timeMethodChannel('noopBytes', payload);
      await _emit(buf, 'JniNoopBytes', size, stats);
    }

    for (final size in _payloadSizes) {
      final payload = _makePayload(size);
      final stats = await _timeMethodChannel('echo', payload);
      await _emit(buf, 'MethodChannel', size, stats);
    }

    // Pigeon FFI/JNI path lands here after the generator runs:
    // final host = BenchHost();
    // for (final size in _payloadSizes) {
    //   final payload = _makePayload(size);
    //   final stats = await _timePigeon(host, payload);
    //   await _emit(buf, 'Pigeon', size, stats);
    // }

    await _report.invokeMethod('end');
    setState(() {
      _running = false;
      _output = buf.toString();
    });
  }

  Uint8List _makePayload(int size) {
    final payload = Uint8List(size);
    for (var i = 0; i < size; i++) {
      payload[i] = i & 0xff;
    }
    return payload;
  }

  Future<BenchStats> _timeMethodChannel(String method, Uint8List payload) async {
    // Warm-up.
    await _echo.invokeMethod<Object?>(method, payload);
    final samples = List<int>.filled(_batches, 0);
    for (var b = 0; b < _batches; b++) {
      final start = DateTime.now().microsecondsSinceEpoch;
      for (var i = 0; i < _batchIters; i++) {
        await _echo.invokeMethod<Object?>(method, payload);
      }
      final end = DateTime.now().microsecondsSinceEpoch;
      samples[b] = (end - start) * 1000 ~/ _batchIters; // ns/iter
    }
    return BenchStats.fromSamples(samples);
  }

  // Future<BenchStats> _timePigeon(BenchHost host, Uint8List payload) async {
  //   await host.echo(payload); // warm-up
  //   final samples = List<int>.filled(_batches, 0);
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

  Future<void> _emit(
    StringBuffer buf,
    String mode,
    int payload,
    BenchStats s,
  ) async {
    final line = '${mode.padRight(15)} ${payload.toString().padLeft(7)}B  '
        '${s.p50.toString().padLeft(6)} ns '
        '${s.p95.toString().padLeft(6)} ns '
        '${s.p99.toString().padLeft(6)} ns '
        '${s.min.toString().padLeft(6)} ns '
        '${s.max.toString().padLeft(6)} ns';
    buf.writeln(line);
    await _report.invokeMethod('line', line);
    if (mounted) setState(() => _output = buf.toString());
  }

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
