# Rust ↔ Kotlin latency after the fast-bridge changes

Branch `fast-bridge` (from `main` 1f52214), measured on the Lenovo
TB373FU (MT8792, Android 16 / SDK 36) with
`nix run .#bench-android-device -- --serial HA299SXG`, modes `full`,
`probe`, `legacy` and `spin`, each in its own process.

| | Before | After |
|---|---|---|
| Raw results | `tb373fu-20261125T214640Z.txt` (+ `.freq.txt`) | `tb373fu-20261125T233845Z.txt` (+ `.freq.txt`) |
| Native ABI | 1 | 2 (`PROTOCOL_VERSION` unchanged, 4) |

Same method as `REPORT.md`: 100 discarded warm-up iterations, 1000
measured, each timed on its own; µs below unless noted. Both runs had
Android thermal status 2 for the whole run (the cached SKIN event the
first report describes), device plugged in, screen on, app in the
foreground. The Xiaomi of the first report was not available; every
"before" number is the Lenovo's.

What changed, in the order of the commits:

1. Hosted calls never create a thread. `DispatchMode::Inline` polls the
   host on the delivering thread and hands a host that waits to a few
   persistent async workers; `DispatchMode::Isolated` and
   `#[istmo::blocking]` methods run on persistent blocking workers
   (grown while all are busy, up to 64). Generated hosts are Inline per
   method (`Dispatch::method_mode`).
2. An inline host's answer is the return value of the JNI call
   (`nativeCall`) / C call (`istmo_ios_call`): no pump, no `onRespond`,
   no caller wake.
3. Kotlin backends start with `CoroutineStart.UNDISPATCHED` on the
   delivering thread.
4. Rust → Kotlin calls (and stream items) go from the Rust thread
   straight to `IstmoRuntime.onCallDirect` / `onEvent` while nothing is
   queued for the pump; the reply is the return value.
5. Bounded, adaptive spin (≤ 20 µs) before parking on the waits that
   remain.
6. Exact-size Kotlin encode buffers, one copy per JNI array.
7. Pooled Rust memory for `submitBytes`; fewer per-frame objects in
   `submit(Image)`.
8. Credit-based backpressure for Rust → Kotlin streams and batched
   delivery from the pump.

## 1. Headline (64 B payload, idle, unpinned)

| Path | Before p50 / p95 | After p50 / p95 | p50 change |
|---|---|---|---|
| **Kotlin → Rust**, generated client → generated host (`RustEcho(client)`) | 238.1 / 751.5 | **3.5 / 7.1** | −98.5 % (68×) |
| Kotlin → Rust raw `IstmoRuntime.call` → Inline host | 55.9 / 73.8 | 2.8 / 5.0 | −95 % |
| Kotlin → Rust raw → Isolated host (blocking worker) | 236.0 / 344.5 | 41.8 / 59.7 | −82 % |
| **Rust → Kotlin**, generated client → generated dispatcher (`KotlinEcho(client)`) | 81.2 / 163.4 | **6.5 / 8.9** | −92 % (12×) |
| Rust → Kotlin raw `Runtime::call` | 81.6 / 182.1 | 6.5 / 9.2 | −92 % |
| Rust → Rust `DispatchMode::Isolated` | 73.5 / 182.8 | 18.5 / 38.8 | −75 % |
| Rust → Rust `DispatchMode::Inline` | 1.0 / 1.2 | 1.2 / 1.2 | +0.2 µs (task allocation) |
| stream burst Rust → Kotlin, 64 B | 68.8 k items/s | 121.6 k items/s, no overflow | +77 % |
| stream burst Kotlin → Rust, 64 B | 105.0 k items/s | 222.8 k items/s | +112 % |
| `submitBytes` 8 MB, submit → recv | 6 556 / 8 381 | 1 600 / 2 744 | −76 % |

Under all-core load (`busyAll`, every core running a spinner):

| Path | Before p50 / p95 | After p50 / p95 |
|---|---|---|
| Kotlin → Rust generated | 12 067 / 23 999 | **3.2 / 4.5** |
| Kotlin → Rust raw → Isolated | 15 935 / 24 008 | 21.8 / 99.2 |
| Rust → Rust Isolated | 83.9 / 23 960 | 5.5 / 5.8 |
| Rust → Kotlin generated | 21.8 / 34.4 | 4.6 / 38.5 |
| Kotlin → Rust raw → Inline | 16.0 / 21.8 | 2.8 / 3.1 |

The 12–16 ms collapse under load is gone: it was the per-call thread
creation waiting for a CFS slice.

## 2. Every payload size (idle)

p50 / p95 µs.

| Path | 0 B | 64 B | 1 KB | 64 KB |
|---|---|---|---|---|
| Rust → Kotlin generated, before | 58.2 / 151.1 | 81.2 / 163.4 | 88.2 / 344.9 | 326.5 / 985.6 |
| after | 17.0 / 33.5 | 6.5 / 8.9 | 9.5 / 12.7 | 79.2 / 104.1 |
| Rust → Kotlin raw, before | 43.1 / 77.2 | 81.6 / 182.1 | 83.1 / 236.6 | 289.3 / 835.6 |
| after | 8.3 / 11.4 | 6.5 / 9.2 | 8.5 / 10.8 | 76.8 / 105.8 |
| Kotlin → Rust generated, before | 132.6 / 216.9 | 238.1 / 751.5 | 256.0 / 345.8 | 478.2 / 715.7 |
| after | 4.0 / 5.2 | 3.5 / 7.1 | 5.2 / 6.5 | 73.5 / 112.3 |
| Kotlin → Rust raw → Inline, before | 29.2 / 53.8 | 55.9 / 73.8 | 60.9 / 89.9 | 119.4 / 209.2 |
| after | 3.1 / 4.8 | 2.8 / 5.0 | 3.0 / 4.3 | 29.0 / 42.5 |
| Kotlin → Rust raw → Isolated, before | 200.5 / 316.0 | 236.0 / 344.5 | 234.8 / 332.4 | 293.6 / 500.5 |
| after | 41.5 / 69.8 | 41.8 / 59.7 | 40.2 / 59.7 | 76.0 / 138.5 |
| Rust → Rust Isolated, before | 76.0 / 307.1 | 73.5 / 182.8 | 97.8 / 133.8 | 103.5 / 149.2 |
| after | 17.8 / 59.0 | 18.5 / 38.8 | 19.1 / 63.9 | 25.8 / 52.2 |
| Rust → Rust Inline, before | 0.9 / 0.9 | 1.0 / 1.2 | 1.0 / 1.2 | 5.5 / 5.8 |
| after | 1.2 / 1.2 | 1.2 / 1.2 | 1.2 / 1.2 | 5.8 / 6.1 |

The 0 B Rust → Kotlin generated cell (17.0) is above the 64 B one (6.5):
it is the first size measured, while the JIT still compiles the direct
path; the raw 0 B cell right after it is at 8.3.

**64 KB** (the target was ~40 % off): Kotlin → Rust generated −85 %,
Rust → Kotlin generated −76 %, Kotlin → Rust raw → Inline −76 %. Most of
that is the removed thread hops; the copy work itself, per stage (probe,
p50, before → after): Kotlin encode of the request 54 → 20.9
(exact-size `IstmoBytesOut` instead of `ByteArrayOutputStream` growth +
`toByteArray`), Kotlin dispatcher reply encode 41 → 18.4, Rust read of
the reply array 16 → 6.1 (one `GetByteArrayRegion` into spare capacity,
no zero fill). What is left is one copy each way that the JVM needs
(`NewByteArray` of the reply, `copyOfRange` of the decoded `ByteArray`).

## 3. Sensitivity (64 B, p50)

| Cell | little before → after | big before → after | busy1 before → after | busyAll before → after |
|---|---|---|---|---|
| Rust → Kotlin generated | 571.1 → 36.6 | 215.2 → 14.2 | 618.8 → 12.5 | 21.8 → 4.6 |
| Rust → Kotlin raw | 791.7 → 36.1 | 104.4 → 6.5 | 690.7 → 6.5 | 21.5 → 6.8 |
| Kotlin → Rust generated | 954.4 → 16.6 | 219.1 → 3.1 | 981.6 → 3.4 | 12 066.8 → 3.2 |
| Kotlin → Rust raw → Inline | 505.2 → 13.2 | 161.5 → 2.7 | 468.3 → 2.8 | 16.0 → 2.8 |
| Kotlin → Rust raw → Isolated | 972.8 → 468.0 | 221.0 → 135.4 | 945.2 → 846.7 | 15 934.8 → 21.8 |
| Rust → Rust Isolated | 568.1 → 214.2 | 151.7 → 32.1 | 765.5 → 301.8 | 83.9 (p95 23 960) → 5.5 (p95 5.8) |
| Rust → Rust Inline | 2.8 → 3.7 | 2.2 → 3.9 | 5.6 → 6.5 | 1.2 → 1.2 |

Paths without a thread hop no longer care where threads land. The
hand-off to a blocking worker (Isolated) still pays one wake, which is
slow on the little cluster and next to a single spinner (`busy1`).

## 4. Streams

Per-item latency p50 / p95 (µs, emit → receive) and throughput.

| Cell | Before | After |
|---|---|---|
| Kotlin → Rust paced, 64 B | 28.2 / 367.1 | 24.7 / 386.1 |
| Kotlin → Rust paced, 1 KB | 25.8 / 319.6 | 28.4 / 388.3 |
| Kotlin → Rust paced, 64 KB | 148.5 / 659.0 | 103.0 / 572.0 |
| Kotlin → Rust burst, 64 B | 105.0 k/s | 222.8 k/s |
| Kotlin → Rust burst, 1 KB | 179.6 k/s | 167.3 k/s |
| Kotlin → Rust burst, 64 KB | 10.9 k/s | 14.7 k/s |
| Rust → Kotlin paced, 64 B | 153.5 / 827.0 | 251.9 / 1 209.6 (own process, section 6: 113.2 / 555.6) |
| Rust → Kotlin paced, 1 KB | 98.5 / 731.8 | 321.5 / 1 051.8 |
| Rust → Kotlin paced, 64 KB | 433.0 / 1 389.0 | 406.7 / 1 165.2 |
| Rust → Kotlin burst, 64 B | 68.8 k/s | 121.6 k/s (own process: 171.9 k/s) |
| Rust → Kotlin burst, 1 KB | 55.1 k/s | 113.7 k/s |
| Rust → Kotlin burst, 64 KB | 16.1 k/s | 8.1 k/s |

- **No `StreamOverflowException` any more**: the Kotlin collector grants
  credit (window = `bufferCapacity`, returned every half window after
  `emit`), the Rust pump holds an item until there is credit, and a
  producer that runs ahead waits on its bounded channel. Every burst
  cell delivered all 1 100 items. (The Lenovo did not overflow before
  either; the Xiaomi, which failed after 760 items, was not available.)
- Rust → Kotlin 64 KB bursts got slower (16.1 → 8.1 k/s): the producer
  now waits for the collector instead of the pump copying 64 KB arrays
  into an unbounded backlog as fast as it can.
- Rust → Kotlin **paced** items did not get faster in the main run, and
  the 1 KB cell is slower (98.5 → 321.5); the 64 B cell measured in its
  own process (section 6, spin on) is 113.2 against 153.5 before. This
  cell moves a lot between runs (before: 153.5 at 64 B but 98.5 at
  1 KB). Two wakes per item remain — an async worker after the
  producer's `send` (encoder and pump now run one after the other on
  it) and the Kotlin collector — each 30–100 µs from idle on this SoC.
  The paced producer itself also slowed (3.3 k → 2.3 k items/s with a
  200 µs gap), so part of it is the producer thread's own wake-up.
- A Kotlin-side batching channel (`buffer(64).produceIn` + one JNI call
  per batch) was tried for Kotlin → Rust and reverted: it held the items
  of a backend that emits without suspending until 64 were queued
  (9–16 ms paced latency). Kotlin → Rust items cross one by one on the
  emitting thread; the throughput gain comes from the cheaper inbound
  path and the consumer-side spin.

## 5. Frame sinks (1 000 frames after 50 warm-up, `queue(4)`)

p50 µs, submit → recv (submit cost in brackets).

| Path | 4 KB before → after | 1 MB before → after | 8 MB before → after |
|---|---|---|---|
| `submitBytes` | 35.6 (17.2) → 28.3 (15.9) | 564.3 (238.5) → 309.0 (106.1) | 6 555.9 (6 423.8) → 1 599.9 (1 150.8) |
| slot pool | 39.2 (12.9) → 22.2 (12.9) | 63.4 (13.9) → 181.2 (13.2) | 272.4 (15.8) → 262.1 (14.8) |
| `submit(HardwareBuffer)` | 54.2 (19.9) → 53.5 (19.1) | 191.8 (16.9) → 180.1 (14.8) | 324.9 (28.5) → 281.2 (21.2) |
| `submit(Image)` | 679.9 (545.5) → 133.3 (230.7) | 617.8 (488.3) → 675.5 (523.9) | 499.8 (424.8) → 553.2 (475.6) |

- `submitBytes` now copies the Java array straight into a pooled Rust
  buffer (one copy instead of zero-fill + copy + convert + `to_vec`, no
  fresh multi-MB allocation once warm): −82 % submit cost at 8 MB, −56 %
  at 1 MB. 8 MB still costs 1.15 ms — the `memcpy` of 8 MB on this SoC;
  the slot pool (no copy) stays the path for large CPU frames.
- `submit(Image)`: 4 KB improved (fence fd closed at once, no per-call
  `Duration`, no `isClosed` round trip); 1 MB and 8 MB are unchanged
  within noise. The remaining ~0.4–0.5 ms over the `HardwareBuffer` path
  is `Image.getHardwareBuffer()` creating a new `HardwareBuffer` per
  frame, which no public API lets us cache (an `Image` exposes no stable
  buffer identity before that call).
- The slot pool 1 MB submit → recv p50 (63 → 181) moved while its submit
  cost did not: the consumer wake, unchanged code.

## 6. Spin before park, on vs off (64 B; own process; idle, then every core busy)

p50 / p95 µs unless marked.

| Cell | idle, spin on | idle, spin off | busy, spin on | busy, spin off |
|---|---|---|---|---|
| Rust → Kotlin stream paced | 113.2 / 555.6 | 211.7 / 863.2 | 73.5 / 91.3 | 74.2 / 279.4 |
| Rust → Kotlin stream burst | 171.9 k/s | 71.4 k/s | 67.7 k/s | 40.3 k/s |
| Kotlin → Rust stream paced | 21.2 / 369.5 | 25.5 / 620.0 | 4.5 / 9.2 | 4.5 / 9.2 |
| Kotlin → Rust stream burst | 305.0 k/s | 204.0 k/s | 725.7 k/s | 719.7 k/s |
| `submitBytes` 4 KB submit → recv | 11.4 / 469.8 | 41.6 / 500.6 | 5.3 / 6.8 | 5.6 / 8.7 |
| Rust → Kotlin generated call | 18.5 / 58.1 | 15.2 / 19.4 | 6.5 / 8.2 | 5.8 / 7.7 |
| Kotlin → Rust generated call | 3.7 / 4.9 | 3.4 / 11.1 | 3.6 / 4.9 | 3.4 / 4.6 |
| Kotlin → Rust raw → Isolated | 82.8 / 151.5 | 75.5 / 95.5 | 18.2 / 25.2 | 16.7 / 23.6 |
| Rust → Rust Isolated | 36.0 / 70.7 | 36.9 / 64.5 | 6.2 / 31.4 | 5.5 / 5.8 |

- Idle, spinning pays wherever a thread still waits for another: stream
  items and frames, 1.5–2.4× throughput and up to ¼ of the latency.
- With every core busy it is neutral on the medians and better on the
  stream tails, with one exception: the Rust → Rust Isolated p95 (31.4
  vs 5.8), where the caller spins in `recv_blocking` while the blocking
  worker it waits for needs that core.
- Calls that no longer wait (the direct paths) are unaffected; their
  idle differences (18.5 vs 15.2) are within run-to-run noise.
- Policy: at most 20 µs, start at 5 µs; a hit stretches the budget to
  twice what it needed, a miss halves it, repeated misses skip up to 64
  waits; never more spinning threads than cores − 1.

## 7. Per-hop breakdown (`probe`, 64 B, ns)

### Rust → Kotlin → Rust (`KotlinBench.echo`), direct call

| Stage | p50 | mean |
|---|---|---|
| encode (Rust codec) | 77 | 88 |
| `Runtime::call`: route, interned names, `byte[]` | 615 | 750 |
| JNI Rust → Kotlin `CallStaticObjectMethod` (caller thread) | 307 | 348 |
| `onCallDirect`: job + start in place | 8 231 | 13 609 |
| decode (Kotlin dispatcher) | 1 154 | 2 362 |
| backend body | 615 | 668 |
| reply encode (Kotlin dispatcher) | 1 153 | 1 838 |
| return to Rust (coroutine completion, reply as return value) | 7 308 | 13 683 |
| reply `byte[]` copy + complete the call | 308 | 495 |
| recv (answer already there) | 307 | 262 |
| decode (Rust codec) | 77 | 99 |
| **total** | **20 538** (before 86 385) | **34 208** (before 168 091) |

### Kotlin → Rust → Kotlin (`RustBench.echo`, generated client and host)

| Stage | p50 | mean |
|---|---|---|
| encode (Kotlin `Bincode`) | 615 | 902 |
| JNI Kotlin → Rust `nativeCall` entry | 538 | 3 374 |
| strings + `byte[]` copy, route, decode (inline host) | 1 615 | 3 115 |
| host body | 0 | 51 |
| reply encode + direct answer | 616 | 749 |
| reply `byte[]` + return to Kotlin | 308 | 635 |
| decode (Kotlin `Bincode`) | 308 | 578 |
| **total** | **4 000** (before 136 923) | **9 407** (before 177 228) |

Raw `IstmoRuntime.call` → Inline host: 3 077 p50 / 4 282 mean (before
31 692 / 33 219).

64 KB totals (p50 / mean, µs): Rust → Kotlin 81.8 / 95.5 (before 180.7 /
271.7), Kotlin → Rust generated 80.2 / 96.1 (before 386.5 / 464.8), raw
→ Inline 27.9 / 37.5 (before 124.6 / 143.6).

No stage waits for another thread any more. What is left on the Rust →
Kotlin side is kotlinx.coroutines itself (the call's `Job`,
`launch(UNDISPATCHED)`, its completion): ~15 µs of the 20.5 µs in this
instrumented run, while the uninstrumented full run measures the whole
call at 6.5 µs. Kotlin → Rust is JNI entries and copies.

## 8. Legacy batch means (p50 of 100 batch means of 1 000 calls)

| Mode | 0 B | 64 B | 1 KB | 64 KB |
|---|---|---|---|---|
| Isolated, before | 161 034 ns | 156 969 ns | 837 033 ns | 890 937 ns |
| Isolated, after | 36 782 ns | 37 730 ns | 38 094 ns | 54 738 ns |
| Inline, before | 824 ns | 912 ns | 933 ns | 5 633 ns |
| Inline, after | 1 042 ns | 1 126 ns | 1 151 ns | 5 937 ns |
| Typed, before → after | 8 → 9 ns | 50 → 49 ns | 64 → 67 ns | 1 113 → 1 095 ns |

The back-to-back Isolated loop no longer drifts to 0.8–0.9 ms (thread
exits overlapping new spawns). Inline pays ~0.2 µs more: the inline poll
now goes through a heap-allocated task, so that a host that waits can
move to the workers.

## 9. What plugin authors need to know

- **Rust hosts (generated)**: every method not marked
  `#[istmo::blocking]` runs on the thread that delivered the call —
  Kotlin and Swift callers' threads included — until its first wait,
  then on the async workers. It must not block (blocking I/O, sleeps,
  `recv_blocking`, waiting on another thread). Mark such methods
  `#[istmo::blocking]` (blocking workers). The typed shortcut already
  asked this of in-process callers.
- **Hand-written `Dispatch`**: default `mode()` is still `Isolated`, now
  persistent blocking workers instead of a thread per call. Up to 64 run
  at once; past that calls queue, so more than 64 blocking calls waiting
  on each other would deadlock. `DispatchMode::OnCallerExecutor` is gone;
  `Dispatch::method_mode(method)` is new.
- **Kotlin backends**: start in place (`CoroutineStart.UNDISPATCHED`) on
  the thread that delivered the call — for direct calls, the Rust
  caller's thread. A `suspend fun` must not block (Kotlin's usual
  main-safety rule); `registerHandler(id, handler, startInPlace = false)`
  keeps the `Dispatchers.Default` start.
- **Ordering**: a direct Rust → Kotlin call or stream item only goes out
  while nothing queued earlier is still on its way, so it never
  overtakes a frame the same thread sent before. Frames from different
  threads were never ordered.
- **Cancellation**: tokens and frames unchanged. A call answered in
  place never suspends, so there is nothing to cancel mid-way.
- **Streams to Kotlin**: bounded backpressure instead of
  `StreamOverflowException`: at most `bufferCapacity` items ahead of the
  collector, the producer waits. Nothing is dropped.
- **Spinning**: hot waits may use up to 20 µs of a core before parking;
  `istmo::spin::set_enabled(false)` turns it off process-wide.
- **iOS**: Rust hosts run inline on the Swift caller's queue
  (`istmo_ios_call` is called from the runtime's serial queue, as the old
  submit was); Swift backends are unchanged (pump + `Task`). Compiled and
  type-checked only; no device run.

## 10. Reproduce

```
nix run .#bench-android-device -- --serial HA299SXG
nix run .#bench-android-device -- --serial HA299SXG --skip-build --modes "spin streams"
```

The script saves and restores `stay_on_while_plugged_in` and
`screen_off_timeout` (left at 120 000 ms on this device).
