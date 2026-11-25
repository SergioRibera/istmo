# Rust ↔ Kotlin latency on real Android devices

Measured on branch `mobile-latency` (based on `screen-capture`: native frame
sinks, automatic typed shortcut, Android ticket release model, slot pools,
ABI handshake) with `examples/bench-android`
(`nix run .#bench-android-device -- --serial <id>`).

| Device | SoC / cores | Android | Raw results |
|---|---|---|---|
| Xiaomi 2201116PG ("veux") | SM6375: 6× Cortex-A55 (0xd05, cap 439, 1.8 GHz) + 2× Cortex-A78 (0xd41, cap 1024, 2.2 GHz), `schedutil` | 13 / SDK 33 | `2201116pg-20261125T214004Z.txt` (+ `.freq.txt`); an earlier full-mode run: `2201116pg-20261125T212601Z.txt` |
| Lenovo TB373FU | MT8792: 4× Cortex-A510 (0xd46, cap 221, 2.2 GHz) + 3× A715 (0xd4d, cap 996, 3.2 GHz) + 1× A715 (cap 1024, 3.35 GHz); governor not readable | 16 / SDK 36 | `tb373fu-20261125T214640Z.txt` (+ `.freq.txt`); the unchanged old bench: `tb373fu-20261125T205958Z-legacy.txt` |

Method: every cell runs 100 discarded warm-up iterations and 1000 measured
ones, each timed individually (`CLOCK_MONOTONIC` in Rust, `System.nanoTime()`
in Kotlin); the raw files carry p50/p95/p99/min/max/mean, the first (cold)
sample, and the CPU each involved thread last ran on (`/proc/self/task/*/stat`).
CPU frequency, Android thermal status and battery temperature were sampled
every 2 s from the shell (`*.freq.txt`). Devices plugged in, screen on, app in
the foreground. Timer granularity: 52 ns (Xiaomi), 77 ns (Lenovo); cells below
~0.2 µs are clock-limited (the legacy batch means resolve them).

**Clock base.** `System.nanoTime()` and Rust's `clock_gettime(CLOCK_MONOTONIC)`
share the base on both devices: 0 / 10 000 Rust reads fell outside the
bracketing Kotlin reads, so cross-language stamps subtract directly.

**Context caveats.** The Lenovo reported Android thermal status 2 (MODERATE,
a cached SKIN 45 °C event) for the whole run; the Xiaomi stayed at 0. Unpinned
cells migrate between little and big clusters and DVFS states — the sensitivity
section shows how much that matters.

## 1. Headline numbers (64 B payload, idle, unpinned; µs)

| Path | Xiaomi p50 / p95 | Lenovo p50 / p95 |
|---|---|---|
| bare JNI Kotlin → Rust (registered native, legacy batch mean) | 0.029 | 0.024 |
| bare JNI Kotlin → Rust with `byte[64]` (batch mean) | 0.136 | 0.129 |
| bare JNI Rust → Kotlin `CallStaticVoidMethod` | 0.21 / 0.26 | 0.08 / 0.23 |
| … with a new `byte[64]` | 0.37 / 1.3 | 0.31 / 0.31 |
| JNI attach + detach of a native thread | 39.5 / 94 | 6.5 / 8.6 |
| Rust → Rust typed shortcut (batch mean) | 0.030 | 0.050 |
| Rust → Rust `DispatchMode::Inline` | 0.99 / 0.99 | 1.0 / 1.15 |
| Rust → Rust `DispatchMode::Isolated` | 244 / 438 | 73 / 183 |
| **Rust → Kotlin** (generated client → generated Kotlin dispatcher) | **83 / 111** | **81 / 163** |
| Rust → Kotlin, raw bytes (`Runtime::call`) | 83 / 123 | 82 / 182 |
| **Kotlin → Rust** (generated client → generated host, Isolated) | **173 / 302** | **238 / 751** |
| Kotlin → Rust raw `IstmoRuntime.call` → Inline host | 53 / 76 | 56 / 74 |
| Kotlin → Rust raw `IstmoRuntime.call` → Isolated host | 202 / 257 | 236 / 345 |
| stream item Kotlin → Rust, paced 200 µs | 142 / 248 | 28 / 367 |
| stream item Rust → Kotlin, paced 200 µs | 496 / 1053 | 154 / 827 |
| stream burst Kotlin → Rust (items/s) | 30 k | 105 k |
| stream burst Rust → Kotlin (items/s) | **fails**: `StreamOverflowException` after 760 items | 69 k |
| futex / condvar / flume round trip | 15 / 20 / 28 | 29 / 31 / 29 |
| flume round trip, spin ≤ 20 µs before parking | 1.8 | 0.85 |
| named thread spawn → body running | 176 / 596 | 72 / 100 |
| Kotlin `Looper` post / `withContext(Dispatchers.Default)` round trip | 19 / 50 | 37 / 53 |

All sizes (p50 µs, idle):

| Path | Device | 0 B | 64 B | 1 KB | 64 KB |
|---|---|---|---|---|---|
| Rust → Kotlin generated | Xiaomi | 103 | 83 | 127 | 368 |
| | Lenovo | 58 | 81 | 88 | 327 |
| Kotlin → Rust generated (Isolated) | Xiaomi | 196 | 173 | 204 | 688 |
| | Lenovo | 133 | 238 | 256 | 478 |
| Kotlin → Rust raw → Inline | Xiaomi | 66 | 53 | 48 | 113 |
| | Lenovo | 29 | 56 | 61 | 119 |
| Rust → Rust Isolated | Xiaomi | 168 | 244 | 455 | 184 |
| | Lenovo | 76 | 73 | 98 | 103 |
| Rust → Rust Inline | Xiaomi | 0.89 | 0.99 | 11.5 | 6.4 |
| | Lenovo | 0.92 | 1.0 | 1.0 | 5.5 |

First (cold) calls: Rust → Kotlin 5.2 ms (Xiaomi) / 2.1 ms (Lenovo) — the first
`Dispatchers.Default` worker, class loading and JIT; Kotlin → Rust 1.4–5.8 ms /
0.9–14.6 ms; the first bare JNI call 33 µs / 20 µs.

### Why Isolated costs 0.15–0.8 ms (the old Xiaomi 0.4–0.8 ms result)

`DispatchMode::Isolated` — the default `Dispatch::mode()` and what every
generated host declares — runs each call on a **new named OS thread**
(`runtime/exec.rs`: `thread::Builder::spawn` + `pollster::block_on`). The cost
is thread creation and scheduling, not istmo logic:

- spawn → body running: 176 µs (Xiaomi) / 72 µs (Lenovo) p50; spawn → join:
  354 / 111 µs. `nativeSubmitCall` alone spends 66 / 51 µs inside the spawn
  call (the `clone`, 2 MiB stack `mmap`, TLS) before the thread even runs.
- Placement decides the rest: new tasks often land on a little core at low
  frequency. Spawn → start pinned to little cores is 274 / 398 µs, pinned to
  big cores 132 / 95 µs. In the legacy back-to-back batch loop the previous
  threads are still exiting (stack `munmap`) while the next ones start, and
  the per-call mean drifts up to 480–850 µs (Xiaomi); on the Lenovo the legacy
  run flips from 157 µs to 837 µs half-way through, reproduced in two runs.
- Under all-core load the new thread waits for a CFS slice: Isolated calls go
  to **4.1 ms p50 / 8 ms p95 (Xiaomi) and 12–16 ms p50 (Lenovo)**, while Inline
  stays at ~1 µs.

Inline is ~1 µs because the host future is polled on the caller; the typed
shortcut is a plain trait call (3–60 ns).

## 2. Per-hop breakdown (`probe` mode, 64 B, 1000 calls)

Stamps: Rust `CLOCK_MONOTONIC` in the caller and in `istmo-android` behind the
off-by-default `latency-probe` feature (pump dequeue, before/after the Kotlin
callback, `nativeSubmitResponse` / `nativeSubmitCall` entry and dispatch), and
Kotlin `System.nanoTime()` in a stand-in callback class the pump is pointed at
(`ProbeRuntime`, which forwards to `IstmoRuntime`), around the generated
dispatcher and in the backend. Means add up to the total mean.

### Rust → Kotlin → Rust (`KotlinBench.echo`)

| Stage | Xiaomi p50 / mean | Lenovo p50 / mean |
|---|---|---|
| encode (Rust codec) | 0.2 / 0.2 | 0.2 / 0.2 |
| `Runtime::call`: route + enqueue (includes the futex wake of the pump) | 13.0 / 26.5 | 7.2 / 18.2 |
| pump thread wake | 8.2 / 26.0 | 5.9 / 15.7 |
| pump builds 2 `jstring` + `byte[]` | 4.6 / 6.7 | 1.5 / 3.0 |
| JNI Rust → Kotlin (`CallStaticVoidMethod`) | 1.7 / 2.5 | 0.6 / 1.6 |
| **`scope.launch` on `Dispatchers.Default` + worker wake** | **73.5 / 144.0** | **41.2 / 81.3** |
| decode (Kotlin dispatcher) | 10.1 / 13.3 | 4.0 / 6.8 |
| backend body | 1.6 / 3.3 | 0.7 / 1.3 |
| reply encode (Kotlin dispatcher) | 3.1 / 4.7 | 1.2 / 2.5 |
| JNI Kotlin → Rust (`nativeSubmitResponse` entry) | 1.3 / 1.6 | 0.6 / 0.9 |
| `byte[]` copy + `dispatch_inbound` + oneshot send | 12.2 / 14.0 | 7.8 / 13.5 |
| Rust caller wake | 6.8 / 47.1 | 5.6 / 22.6 |
| decode (Rust codec) | 0.7 / 1.0 | 0.3 / 0.5 |
| **total** | **144 / 291** | **86 / 168** |

Also: the pump spends 52 / 30 µs (p50) inside `onCall` just launching the
coroutine, so a second call queued behind it waits that long.

### Kotlin → Rust → Kotlin (`RustBench.echo`, generated client, Isolated host)

| Stage | Xiaomi p50 / mean | Lenovo p50 / mean |
|---|---|---|
| encode (Kotlin `Bincode`) | 3.6 / 4.3 | 1.3 / 2.0 |
| `IstmoRuntime.call` (continuation, map insert) + JNI entry | 11.1 / 11.4 | 4.6 / 5.6 |
| **strings + `byte[]` copy, route, thread spawn + start, decode** | **108.0 / 122.8** | **84.9 / 100.0** |
| host body | 0.05 / 0.05 | 0.2 / 0.3 |
| reply encode + enqueue + pump wake | 23.6 / 27.2 | 18.5 / 38.5 |
| pump builds `byte[]` | 3.3 / 3.7 | 0.4 / 0.8 |
| JNI Rust → Kotlin | 2.8 / 2.9 | 0.5 / 1.0 |
| `onRespond`: `cont.resume` (dispatch to the caller's event loop) | 31.1 / 32.2 | 9.8 / 14.5 |
| Kotlin caller wake (`runBlocking` loop) | 21.8 / 28.0 | 9.9 / 13.5 |
| decode (Kotlin `Bincode`) | 1.8 / 2.3 | 0.6 / 1.0 |
| **total** | **207 / 235** | **137 / 177** |

The same direction with a raw call to an Inline host (no spawn, no codec):
total 47 / 63 µs (Xiaomi), 32 / 33 µs (Lenovo) — JNI entry 4.5 / 2.5,
`nativeSubmitCall` body with the inline host and the reply enqueue 13.9 / 10.2,
`onRespond` 12.9 / 8.8, caller wake 8.6 / 5.6 (p50).

### 64 KB payloads

Copies take over: Kotlin `Bincode` encode 90 / 54 µs and decode 57 / 35 µs
(`ByteArrayOutputStream` growth), the Kotlin dispatcher's decode 46 / 25 and
reply encode 69 / 41, the pump's `NewByteArray` + copy 47 / 26, Rust's
`convert_byte_array` + dispatch 64 / 16 (Xiaomi / Lenovo, p50). Rust → Kotlin
64 KB totals 394 / 181 µs, Kotlin → Rust 821 / 386 µs.

### What dominates, and why

1. **Thread hand-offs, not crossings.** On both devices 82–94 % of the mean
   unary latency is spent waiting for another thread to run: Rust → Kotlin
   makes three hops (caller → pump → `Dispatchers.Default` worker → caller),
   Kotlin → Rust three hops plus a thread creation (caller → fresh thread →
   pump → caller's event loop). The JNI crossings cost 0.3–3 µs, the codecs
   1–10 µs at 64 B, the backend bodies < 2 µs.
2. **The biggest single hop is per direction:** `scope.launch` onto
   `Dispatchers.Default` (49 % of Rust → Kotlin), and the Isolated thread spawn
   (52–56 % of Kotlin → Rust).
3. **Each wake costs 10–30 µs on an idle phone, far more on a little core.**
   The scheduler has to bring an idle CPU out of its idle state and often up
   from a low DVFS step: futex round trips are 15–29 µs idle but 7 / 4 µs with
   every core busy, and the same Rust → Kotlin call drops from 83 to 50 µs
   (Xiaomi) and 81 to 22 µs (Lenovo) under all-core load. Pinned to the
   little cluster everything is 2–8× slower. Exception: anything that creates
   a thread gets much slower under load (Isolated above).
4. **JNI attach is not on the hot path today** (the pump attaches once; the
   frame releaser is a Java thread), but it costs 39.5 / 6.5 µs per
   attach + detach, so any redesign that calls Kotlin from Rust caller threads
   must keep them attached permanently.
5. **Large buffer allocation**: see frame sinks — a fresh multi-MB `Vec` per
   frame costs milliseconds in page faults.

## 3. Sensitivity (64 B, p50 µs)

| Cell | Device | idle | little | big | 1 spinner | all cores busy |
|---|---|---|---|---|---|---|
| Rust → Kotlin generated | Xiaomi | 83 | 232 | 74 | 56 | 50 |
| | Lenovo | 81 | 571 | 215 | 619 | 22 |
| Kotlin → Rust generated (Isolated) | Xiaomi | 173 | 998 | 199 | 353 | **4137** |
| | Lenovo | 238 | 954 | 219 | 982 | **12 067** |
| Kotlin → Rust raw → Inline | Xiaomi | 53 | 179 | 58 | 38 | 31 |
| | Lenovo | 56 | 505 | 162 | 468 | 16 |
| Rust → Rust Isolated | Xiaomi | 244 | 325 | 87 | 258 | **4074** |
| | Lenovo | 73 | 568 | 152 | 765 | 84 (p95 23 960) |
| Rust → Rust Inline | Xiaomi | 0.99 | 10.8 | 1.15 | 4.1 | 0.99 |
| | Lenovo | 1.0 | 2.8 | 2.2 | 5.6 | 1.15 |
| flume round trip | Xiaomi | 28 | 52 | 16 | 10 | 7 |
| | Lenovo | 29 | 47 | 41 | 213 | 4 |
| flume round trip, spin ≤ 20 µs | Xiaomi | 1.8 | 0.94 | 1.3 | 0.94 | 0.57 (p95 51) |
| | Lenovo | 0.85 | 0.92 | 1.15 | 2.8 | 0.62 |
| thread spawn → start | Xiaomi | 176 | 274 | 132 | 222 | 52 (p95 4036) |
| | Lenovo | 72 | 398 | 95 | 385 | **11 948** |
| `withContext(Default)` round trip | Xiaomi | 50 | 237 | 69 | 20 | 28 |
| | Lenovo | 53 | 534 | 151 | 510 | 14 |

"little" / "big" pin every thread of the process (`sched_setaffinity` on each
task; new threads inherit). The Lenovo's big-cluster pin is slower than
unpinned (four big cores shared by every thread of the app, thermal status 2),
and its single-spinner case behaves like the little-core case: the remaining
light threads were placed on the A510 cluster. **Spin-then-park** (polling
`try_recv` for up to 5–50 µs before parking) turns a 28 µs round trip into
0.85–1.8 µs on both devices whenever the partner answers inside the window;
with every core busy its p95 rises to 51 µs on the Xiaomi (the spinner competes
for the core), so it needs a bound or adaptivity. Cold first calls are listed
in Section 1.

## 4. Frame sinks from Kotlin (1000 frames after 50 warm-up, `queue(4)`)

p50 µs, submit start in Kotlin → Rust `recv` returned; "cost" = time inside the
Kotlin submit call; "ticket" = Rust drops the frame → Kotlin owner closed.

| Path | Device | 4 KB | 1 MB | 8 MB (1080p RGBA) |
|---|---|---|---|---|
| `submitBytes` (copy) | Xiaomi | 52 (cost 44) | 1771 (cost 1782) | 10 163 (cost 10 018) |
| | Lenovo | 36 (cost 17) | 564 (cost 239) | 6 556 (cost 6 424) |
| slot pool (no copy) | Xiaomi | 111 (cost 98) | 106 (cost 91) | 162 (cost 192) |
| | Lenovo | 39 (cost 13) | 63 (cost 14) | 272 (cost 16) |
| `submit(HardwareBuffer, owner)` | Xiaomi | 145 (cost 128, ticket 102) | 159 (cost 136, ticket 147) | 170 (cost 149, ticket 107) |
| | Lenovo | 54 (cost 20, ticket 51) | 192 (cost 17, ticket 47) | 325 (cost 29, ticket 118) |
| `submit(Image)` from `ImageReader` | Xiaomi | 235 (cost 242) | 229 (cost 218) | 220 (cost 223) |
| | Lenovo | 680 (cost 545) | 618 (cost 488) | 500 (cost 425) |

- `submitBytes` of large frames is dominated by the fresh Rust allocation per
  frame (a multi-MB `Vec` → `mmap` + one page fault per 4 KB page), not by the
  copy: 1 MB costs 1.8 / 0.56 ms and 8 MB 10 / 6.6 ms, against 0.06–0.27 ms
  for the slot pool. Above a few hundred KB the slot pool or a `HardwareBuffer`
  is the path.
- On the Xiaomi the submit cost of small frames (44–150 µs) is mostly the wake
  of the consumer: the producer pays for the futex wake and, with wake-up
  preemption on the same core, for the consumer running first.
- `submit(Image)` costs 0.1–0.5 ms more than submitting the same buffer as a
  `HardwareBuffer` (per-call `Image.getHardwareBuffer()` object, fence query).

## 5. On-device validation of the new mechanism

| Item | Xiaomi (SDK 33) | Lenovo (SDK 36) |
|---|---|---|
| ABI handshake (`IstmoRuntime.start` → `IstmoFrames.checkAbi`) | pass: native 1 = runtime 1, in all three launches | pass, same |
| Frame producer open + fingerprint (`FrameSinkId<FrameMeta>`) | pass: 12 / 12 cells, no `TYPE_MISMATCH` or refusal | pass, same |
| `submitBytes` | pass: 1050 / 1050 delivered at 1 MB and 8 MB; at 4 KB 11 frames evicted by `queue(4)` (consumer descheduled > 1.2 ms) and reported through `dropped_before` | pass; 1 evicted at 4 KB |
| Slot pools (`openSlotPool(size, 4)` / `acquireSlot` / `submit(slot)`) | pass: 1050 / 1050 at 4 KB, 1 MB, 8 MB, sizes correct, slots recycled | pass, same |
| `submit(HardwareBuffer, owner)` + ticket releaser | pass: 1050 / 1050 with a non-null `platform_ptr`; every owner closed (a pool of 4 buffers recycled 1050 times) | pass, same |
| `submit(Image)` (API 28+) | pass: every image the listener acquired reached Rust and was closed by the releaser (`maxImages = 4`, 1050 frames). 1–3 of 1050 frames per cell never reached `OnImageAvailableListener` (lost inside `ImageReader` / `BufferQueue`, upstream of istmo) | pass, same |
| API 33+ acquire-fence wait in `submit(Image)` | the path ran on every image without error; no image arrived with a pending fence (the canvas producer posts signalled buffers), so an actual wait was not exercised | same |
| Sink end | `Completed` after `close()` in every cell | same |

No failures in the new mechanism. Problems found elsewhere:

- **Rust-produced stream bursts overflow the Kotlin side.** A Rust `#[stream]`
  pushing items back to back into a Kotlin collector (`runBlocking`) fails with
  `StreamOverflowException` after 760 (64 B) and 910 (1 KB) items on the Xiaomi
  (571 in an earlier run): the pump delivers ~30–70 k items/s and the default
  256-item buffer fills before the collector catches up. That is the documented
  policy (fail rather than block the pump), but a plain producer hits it within
  milliseconds on a mid-range phone.
- The bench's log capture (`scripts/run-bench-android.sh`) hung after
  `BENCH_END`: `adb logcat` piped into a loop that stops reading blocks until
  its next write. Fixed by polling a capture file.

## 6. Kotlin 1.9 compatibility

The bench app now builds with **Kotlin 1.9.24 / AGP 8.2.2** (the demos'
toolchain), with the runtime included from this checkout (also 1.9.24) and the
generated `KotlinBenchDispatcher` that opens `IstmoFrameSender` through
`@OptIn(IstmoGeneratedApi::class)` on an expression. It compiled with no errors
and no warnings and ran on both devices. No runtime fix was needed.

## 7. Comparison with Flutter (same Xiaomi, `results/bench-flutter/`)

Flutter's numbers are batch means of Dart → Android main thread → Dart round
trips; istmo means are shown for a like-for-like comparison (µs).

| Payload | MethodChannel | Pigeon run (its MethodChannel) | istmo Kotlin → Rust Inline | istmo Rust → Kotlin generated | istmo Kotlin → Rust generated (Isolated) |
|---|---|---|---|---|---|
| 0 B | 85 | 173 (169) | 72 | 149 | 219 |
| 64 B | 81 | 172 (165) | 62 | 96 | 189 |
| 1 KB | 85 | 182 (171) | 60 | 147 | 223 |
| 64 KB | 355 | 907 (904) | 156 | 467 | 811 |

The Pigeon session ran about 2× slower than the MethodChannel session across
the board, so within it Pigeon ≈ MethodChannel. istmo's path with one fewer
hop (an Inline host) beats MethodChannel at every size; generated
Rust → Kotlin is in the same range at ≤ 1 KB; generated Kotlin → Rust is
2.3× slower than MethodChannel, entirely because of the Isolated thread spawn,
and 64 KB pays for the extra copies.

## 8. Candidate changes for a lower-latency path (ranked; not implemented)

Gains are p50 at 64 B, idle, Xiaomi / Lenovo, taken from the stages above.

1. **No thread spawn for Kotlin → Rust unary calls.** Serve calls of
   generated hosts whose methods are not `#[istmo::blocking]` inline on the JNI
   thread (as Inline does), or on a persistent, pre-started worker pool,
   instead of `DispatchMode::Isolated`'s fresh thread. Removes the 108 / 85 µs
   spawn stage (52–56 % of the call) and the collapse under load (4.1 / 12 ms
   p50 with every core busy). Expected: generated Kotlin → Rust from 173 / 238 µs
   to the measured raw-Inline level, **~53 / 56 µs (−70–75 %)**, and ~31 / 16 µs
   under load.
2. **Return the reply from the JNI call when the host finishes inline.** A
   synchronous native entry that hands back the reply bytes (resuming the
   continuation on the calling thread, or never suspending) skips the pump for
   the reply, `onRespond`'s `cont.resume` and the caller's event-loop wake: of
   the 47 / 32 µs raw-Inline total only the JNI entry (4.5 / 2.5) and the
   copy + dispatch body (≈ 10 / 7 without its pump wake) remain. Expected:
   **~10–15 / ~8–10 µs**, on top of (1).
3. **Rust → Kotlin: run the generated dispatcher without `scope.launch` on
   `Dispatchers.Default`.** Start it `CoroutineStart.UNDISPATCHED` on the
   delivering thread (non-suspending backends finish before the first
   suspension point; suspending ones continue wherever they resume). Removes
   the 73 / 41 µs launch + worker-wake stage (≈ 49 % of the mean) and the
   52 / 30 µs the pump is held inside `onCall`. Expected: Rust → Kotlin from
   83 / 81 µs to **~25–35 µs**. Risk: a backend that blocks would stall the
   pump; keep the launch for `#[istmo::blocking]` methods or behind an opt-out.
4. **Rust → Kotlin directly from the Rust caller thread, no pump.** With
   caller threads attached once and kept attached (never per call: attach +
   detach costs 39.5 / 6.5 µs), a direct `CallStatic*Method` returning the
   reply removes the enqueue (13 / 7), pump wake (8 / 6), caller wake (7 / 6)
   and the oneshot dispatch (12 / 8). Expected with (3): **~10–15 µs**. Keep the
   pump for fire-and-forget frames, releases and streams.
5. **Spin-then-park on the waits that remain** (Rust `CallHandle` / oneshot
   receive, the pump's selector, the Kotlin caller): measured round trips drop
   from 28–29 µs to 0.85–1.8 µs when the answer comes within the window, so
   each remaining hop costs ~1 µs instead of 10–30 µs when the other side is
   quick: **~20–40 µs per call** on the hop-based paths (streams, frames,
   anything still crossing threads). Needs a short bound (≤ 20 µs) or adaptive
   spinning: under all-core load the spin's p95 rose to 51 µs.
6. **Fewer and pre-sized copies for large payloads.** Pre-size the Kotlin
   `Bincode` writer (or write into a reused direct `ByteBuffer`) and let the
   pump and `nativeSubmit*` hand over one buffer instead of copy +
   `NewByteArray` + copy. The 64 KB stages add up to ~300–370 µs (Xiaomi) /
   ~180–220 µs (Lenovo) of encode, decode and copy; removing the growth copies
   and one copy per side should save **~40 % of a 64 KB call**.
7. **Frame sinks: reuse Rust memory for `submitBytes`.** A fresh multi-MB
   allocation per frame costs 1.8–10 ms; reusing buffers (or steering large
   producers to slot pools, 0.06–0.27 ms) removes **> 95 %** of the 1 MB and
   8 MB submit cost. Also cache what `submit(Image)` derives per call: it costs
   0.1–0.5 ms more than the bare `HardwareBuffer` submit. Ticket return
   (drop → owner closed, 50–150 µs p50) is one condvar wake of the releaser;
   it only matters for producers with very small pools.
8. **Streams: batch delivery or a shared ring buffer.** One JNI callback,
   `trySend` and collector resume per item is what overflows the 256-item
   Kotlin buffer on bursts (Section 5) and costs 154–496 µs per paced item
   Rust → Kotlin. Delivering items in batches per JNI call (or through a
   Rust-owned ring buffer the collector drains) would raise throughput past the
   measured 30–70 k items/s and stop the overflow; paced latency gains come
   from (3)–(5).

Ranking rationale: (1)–(4) remove whole thread hops, which are 82–94 % of every
unary call; (1) and (3) are local changes on the largest measured stages; (2)
and (4) need new native entries; (5)–(8) shave the remaining hops and the
large-payload paths.

## Reproduce

```
nix run .#bench-android-device -- --serial 151b29ecbf9f
nix run .#bench-android-device -- --serial HA299SXG --skip-build
```

Each run executes the `full`, `probe` and `legacy` modes in separate processes.
The script restores `stay_on_while_plugged_in` on exit; MIUI refuses it from
adb, and the activity keeps the screen on with `FLAG_KEEP_SCREEN_ON`.
