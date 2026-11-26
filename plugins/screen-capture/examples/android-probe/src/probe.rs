//! The probe commands. Each returns a plain-text report; failures are
//! reported, never panicked on.

use std::collections::BTreeSet;
use std::fmt::{Debug, Write as _};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use istmo_core::{
    FrameSink, NativeFrame, NativeHandleId, Runtime, SinkPolicy, StreamItem, TypedStream,
};
use istmo_screen_capture::{
    AudioConfig, AudioEvent, AudioMeta, AudioSource, BufferMemory, CapabilityQuery, CaptureError,
    CaptureEvent, ConsentConfig, FormatChoice, FormatRequest, FormatTier, FrameMeta, FrameRate,
    PixelFormat, PixelLayout, PreviewFit, PreviewSurfaceKind, PreviewTarget, ProjectionMode,
    ProjectionRequest, SampleFormat, ScreenCaptureClient, ScreenCaptureClientExt,
    ScreenshotRequest, SessionConfig, SourceFilter, SourceSelector,
};

/// How long a stream waits for a frame before it counts as idle.
const IDLE: Duration = Duration::from_secs(2);

/// A command's outcome: the report, or why it could not run.
type Outcome = Result<String, String>;

fn block<T, E: Debug>(future: impl Future<Output = Result<T, E>>) -> Result<T, String> {
    pollster::block_on(future).map_err(|e| format!("{e:?}"))
}

fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
}

fn consenting() -> ConsentConfig {
    ConsentConfig {
        allow_picker: true,
        ..ConsentConfig::default()
    }
}

fn gpu_formats() -> FormatRequest {
    FormatRequest {
        tiers: vec![FormatTier::new(vec![FormatChoice {
            format: PixelFormat::Rgba8,
            memory: BufferMemory::HardwareBuffer,
            modifiers: Vec::new(),
        }])],
    }
}

/// One line per event, without the (long) negotiated stream.
fn event_line(event: &CaptureEvent) -> String {
    match event {
        CaptureEvent::Started(n) | CaptureEvent::Renegotiated(n) => format!(
            "{} {}x{} {:?} {:?} tier {} copies {:?} pool {} fps {:?} source {:?} {:?} indicator {:?}",
            if matches!(event, CaptureEvent::Started(_)) {
                "Started"
            } else {
                "Renegotiated"
            },
            n.size.width,
            n.size.height,
            n.format,
            n.memory,
            n.tier,
            n.copies,
            n.pool_depth,
            n.fps,
            n.source.kind,
            n.source.id.value,
            n.indicator,
        ),
        other => format!("{other:?}"),
    }
}

/// Events queued so far, each stamped with the seconds since [`Instant`].
#[derive(Debug, Default)]
struct EventLog {
    lines: Vec<String>,
    stopped: bool,
}

impl EventLog {
    fn drain<E: Debug + istmo_core::Message>(
        &mut self,
        stream: &TypedStream<E, CaptureError>,
        since: Instant,
        line: impl Fn(&E) -> String,
        stops: impl Fn(&E) -> bool,
    ) {
        while let Some(item) = stream.try_recv() {
            let at = since.elapsed().as_secs_f32();
            match item {
                Ok(StreamItem::Event(event)) => {
                    self.stopped |= stops(&event);
                    self.lines.push(format!("  {at:6.2}s {}", line(&event)));
                }
                Ok(StreamItem::Failed(err)) => {
                    self.lines.push(format!("  {at:6.2}s failed {err:?}"));
                }
                Ok(StreamItem::Completed | StreamItem::Cancelled) => {
                    self.lines.push(format!("  {at:6.2}s end of events"));
                    self.stopped = true;
                }
                Err(err) => {
                    self.lines.push(format!("  {at:6.2}s transport {err:?}"));
                    self.stopped = true;
                }
            }
        }
    }

    fn capture(&mut self, stream: &TypedStream<CaptureEvent, CaptureError>, since: Instant) {
        self.drain(stream, since, event_line, |e| {
            matches!(e, CaptureEvent::Stopped(_))
        });
    }

    fn audio(&mut self, stream: &TypedStream<AudioEvent, CaptureError>, since: Instant) {
        self.drain(
            stream,
            since,
            |e| format!("{e:?}"),
            |e| matches!(e, AudioEvent::Stopped(_)),
        );
    }

    fn report(&self, out: &mut String) {
        let _ = writeln!(out, "events:");
        for line in &self.lines {
            let _ = writeln!(out, "{line}");
        }
    }
}

/// What a run of frames looked like.
#[derive(Debug, Default)]
struct FrameStats {
    frames: u64,
    hardware: u64,
    cpu: u64,
    repeats: u64,
    skipped: u64,
    seq_gaps: u64,
    pts_backwards: u64,
    last_seq: Option<u64>,
    last_pts: Option<u64>,
    first_pts: Option<u64>,
    latency_us: Vec<i64>,
    sizes: BTreeSet<(u32, u32)>,
    generations: BTreeSet<u64>,
    slots: BTreeSet<u32>,
    descs: BTreeSet<String>,
    nonzero_cpu: u64,
}

impl FrameStats {
    fn add(&mut self, frame: &NativeFrame<FrameMeta>) {
        let meta = &frame.meta;
        self.frames += 1;
        self.latency_us
            .push(now_us() - i64::try_from(meta.wall_us).unwrap_or(i64::MAX));
        match meta.memory {
            BufferMemory::HardwareBuffer => {
                if let Some(buffer) = frame
                    .buffer
                    .downcast_ref::<istmo::android::HardwareBuffer>()
                {
                    self.hardware += 1;
                    let d = buffer.desc();
                    self.descs.insert(format!(
                        "{}x{} format {} usage {:#x} stride {}",
                        d.width, d.height, d.format, d.usage, d.stride
                    ));
                }
            }
            BufferMemory::Cpu => {
                self.cpu += 1;
                if frame
                    .buffer
                    .as_bytes()
                    .is_some_and(|b| b.iter().any(|v| *v != 0))
                {
                    self.nonzero_cpu += 1;
                }
            }
            _ => {}
        }
        if meta.repeat {
            self.repeats += 1;
        }
        self.skipped += u64::from(meta.skipped_before);
        if let Some(last) = self.last_seq
            && meta.seq != last + 1
        {
            self.seq_gaps += 1;
        }
        self.last_seq = Some(meta.seq);
        if let Some(last) = self.last_pts
            && meta.pts_ns < last
        {
            self.pts_backwards += 1;
        }
        self.first_pts.get_or_insert(meta.pts_ns);
        self.last_pts = Some(meta.pts_ns);
        self.sizes.insert((meta.size.width, meta.size.height));
        self.generations.insert(meta.buffer.pool_generation);
        self.slots.insert(meta.buffer.slot);
    }

    fn report(&mut self, out: &mut String, elapsed: Duration) {
        self.latency_us.sort_unstable();
        let pick = |q: usize| {
            self.latency_us
                .get(self.latency_us.len() * q / 100)
                .copied()
                .unwrap_or(0)
        };
        let span_s = match (self.first_pts, self.last_pts) {
            (Some(a), Some(b)) if b > a => (b - a) as f64 / 1e9,
            _ => 0.0,
        };
        let _ = writeln!(
            out,
            "frames {} in {:.2}s ({:.1}/s by pts over {:.2}s); HardwareBuffer {} cpu {} (non-blank {}) repeats {} skipped {}",
            self.frames,
            elapsed.as_secs_f64(),
            if span_s > 0.0 {
                (self.frames.saturating_sub(1)) as f64 / span_s
            } else {
                0.0
            },
            span_s,
            self.hardware,
            self.cpu,
            self.nonzero_cpu,
            self.repeats,
            self.skipped,
        );
        let _ = writeln!(
            out,
            "seq gaps {} pts backwards {}; wall-clock latency us p50 {} p95 {} max {}",
            self.seq_gaps,
            self.pts_backwards,
            pick(50),
            pick(95),
            self.latency_us.last().copied().unwrap_or(0),
        );
        let _ = writeln!(
            out,
            "sizes {:?} pool generations {:?} slots {:?}",
            self.sizes, self.generations, self.slots
        );
        for desc in &self.descs {
            let _ = writeln!(out, "AHardwareBuffer {desc}");
        }
    }
}

fn frames_for(
    sink: &FrameSink<FrameMeta>,
    events: &TypedStream<CaptureEvent, CaptureError>,
    log: &mut EventLog,
    since: Instant,
    duration: Duration,
) -> FrameStats {
    let mut stats = FrameStats::default();
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline && !log.stopped {
        if let Some(frame) = sink.try_recv() {
            stats.add(&frame);
        } else {
            log.capture(events, since);
            if sink.is_closed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    log.capture(events, since);
    stats
}

#[derive(Debug)]
struct Probe {
    client: Arc<ScreenCaptureClient>,
    runtime: Arc<Runtime>,
}

fn probe() -> Result<&'static Probe, String> {
    static PROBE: OnceLock<Probe> = OnceLock::new();
    if let Some(probe) = PROBE.get() {
        return Ok(probe);
    }
    let client = ScreenCaptureClient::acquire().map_err(|e| format!("acquire: {e}"))?;
    let runtime = Arc::clone(client.runtime());
    Ok(PROBE.get_or_init(|| Probe {
        client: Arc::new(client),
        runtime,
    }))
}

/// Run `command` (`name [seconds]`); `surface` is the preview surface's
/// handle (`IstmoRuntime.registerObject`), 0 when there is none.
pub(crate) fn run(command: &str, surface: i64) -> String {
    let mut words = command.split_whitespace();
    let name = words.next().unwrap_or_default();
    let secs = words.next().and_then(|w| w.parse().ok()).unwrap_or(5);
    let duration = Duration::from_secs(secs);
    let surface = NativeHandleId::new(u64::try_from(surface).unwrap_or(0));
    let started = Instant::now();
    let outcome = probe().and_then(|probe| match name {
        "caps" => probe.caps(),
        "stream" => probe.stream(
            SessionConfig::primary_monitor().formats(gpu_formats()),
            duration,
        ),
        "stream-any" => probe.stream(SessionConfig::primary_monitor(), duration),
        "stream-cpu" => probe.stream(SessionConfig::primary_monitor().cpu_frames(), duration),
        "stream-max15" => probe.stream(
            SessionConfig::primary_monitor().fps(FrameRate::Max(15)),
            duration,
        ),
        "stream-exact10" => probe.stream(
            SessionConfig::primary_monitor().fps(FrameRate::Exact(10)),
            duration,
        ),
        "hold" => probe.hold(),
        "shot" => probe.shot(ScreenshotRequest::of(SourceSelector::PrimaryMonitor)),
        "shot-gpu" => {
            probe.shot(ScreenshotRequest::of(SourceSelector::PrimaryMonitor).formats(gpu_formats()))
        }
        "pixels" => probe.pixels(),
        "preview" => probe.preview(surface, duration),
        "fanout" => probe.fanout(surface, duration),
        "audio" => probe.audio(duration),
        "audio-only" => probe.audio_only(duration),
        "grant" => probe.grant(),
        "refuse" => Ok(probe.refusals()),
        other => Err(format!("unknown command `{other}`")),
    });
    match outcome {
        Ok(report) => format!(
            "== {command} ({:.1}s)\n{report}",
            started.elapsed().as_secs_f32()
        ),
        Err(err) => format!(
            "== {command} FAILED after {:.1}s: {err}",
            started.elapsed().as_secs_f32()
        ),
    }
}

impl Probe {
    fn caps(&self) -> Outcome {
        let mut out = String::new();
        let backends = block(self.client.backends())?;
        let _ = writeln!(out, "backends {backends:?}");
        let report = block(self.client.capabilities(CapabilityQuery::default()))?;
        let _ = writeln!(out, "{report:#?}");
        let sources = block(self.client.sources(SourceFilter::default()))?;
        let _ = writeln!(out, "{sources:#?}");
        for scope in [
            istmo_screen_capture::AccessScope::ScreenRecording,
            istmo_screen_capture::AccessScope::Notifications,
            istmo_screen_capture::AccessScope::Microphone,
        ] {
            let state = block(self.client.access(scope))?;
            let _ = writeln!(out, "access {state:?}");
        }
        Ok(out)
    }

    fn stream(&self, config: SessionConfig, duration: Duration) -> Outcome {
        let config = config.pool_depth(4).consent(consenting());
        let session = block(
            self.client
                .open_session(&self.runtime, config, SinkPolicy::queue(4)),
        )?;
        let since = Instant::now();
        let events = session.events().map_err(|e| format!("{e:?}"))?;
        let mut log = EventLog::default();
        let mut stats = frames_for(session.video(), &events, &mut log, since, duration);
        let elapsed = since.elapsed();
        let sink = session.video().stats();
        block(session.close())?;
        std::thread::sleep(Duration::from_millis(300));
        log.capture(&events, since);
        let mut out = String::new();
        stats.report(&mut out, elapsed);
        let _ = writeln!(out, "sink {sink:?}");
        log.report(&mut out);
        Ok(out)
    }

    /// Hold every frame: the backend must stop handing out buffers one
    /// short of its pool, then resume once they come back.
    fn hold(&self) -> Outcome {
        let config = SessionConfig::primary_monitor()
            .formats(gpu_formats())
            .fps(FrameRate::Native)
            .pool_depth(4)
            .consent(consenting());
        let session = block(self.client.open_session(
            &self.runtime,
            config,
            SinkPolicy::queue(16),
        ))?;
        let since = Instant::now();
        let events = session.events().map_err(|e| format!("{e:?}"))?;
        let mut log = EventLog::default();
        let mut held = Vec::new();
        let mut quiet = Instant::now();
        while quiet.elapsed() < IDLE && held.len() < 64 {
            if let Some(frame) = session.video().try_recv() {
                held.push(frame);
                quiet = Instant::now();
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let count = held.len();
        let pool = session.negotiated().pool_depth;
        drop(held);
        let resumed = frames_for(
            session.video(),
            &events,
            &mut log,
            since,
            Duration::from_secs(3),
        )
        .frames;
        block(session.close())?;
        std::thread::sleep(Duration::from_millis(300));
        log.capture(&events, since);
        let mut out = format!(
            "pool {pool}: held {count} frames at the limit; {resumed} frames in 3 s after release\n"
        );
        log.report(&mut out);
        Ok(out)
    }

    fn shot(&self, request: ScreenshotRequest) -> Outcome {
        let request = request.consent(consenting());
        let (info, frames) = block(self.client.screenshot_frames(&self.runtime, request))?;
        let mut out = format!(
            "frames {} {}x{} {:?} {:?} copies {:?}\n",
            info.frames,
            info.negotiated.size.width,
            info.negotiated.size.height,
            info.negotiated.format,
            info.negotiated.memory,
            info.negotiated.copies,
        );
        let mut stats = FrameStats::default();
        for frame in &frames {
            stats.add(frame);
            let _ = writeln!(
                out,
                "meta seq {} pts {} wall {} planes {:?} buffer {:?} content {:?}",
                frame.meta.seq,
                frame.meta.pts_ns,
                frame.meta.wall_us,
                frame.meta.planes,
                frame.meta.buffer,
                frame.meta.content_rect,
            );
        }
        stats.report(&mut out, Duration::ZERO);
        Ok(out)
    }

    fn pixels(&self) -> Outcome {
        let request = ScreenshotRequest::of(SourceSelector::PrimaryMonitor).consent(consenting());
        let image = block(self.client.screenshot_cpu(request, PixelLayout::default()))?;
        let nonzero = image.bytes.iter().filter(|b| **b != 0).count();
        Ok(format!(
            "{}x{} stride {} {:?} {} bytes, {:.1}% non-zero, first pixel {:?}\n",
            image.width,
            image.height,
            image.stride,
            image.format,
            image.bytes.len(),
            nonzero as f64 * 100.0 / image.bytes.len().max(1) as f64,
            image.bytes.get(..4),
        ))
    }

    fn preview_target(surface: NativeHandleId) -> Result<PreviewTarget, String> {
        if surface.get() == 0 {
            return Err("no preview surface (the SurfaceView is not ready)".to_owned());
        }
        Ok(PreviewTarget {
            surface,
            kind: PreviewSurfaceKind::AndroidSurface,
            fit: PreviewFit::Fit,
        })
    }

    fn preview(&self, surface: NativeHandleId, duration: Duration) -> Outcome {
        let target = Self::preview_target(surface)?;
        let config = SessionConfig::primary_monitor().consent(consenting());
        let session = block(self.client.open_preview_owned(config, target))?;
        let since = Instant::now();
        let id = session.session.id();
        let events = self.client.events(id).map_err(|e| format!("{e:?}"))?;
        let mut log = EventLog::default();
        while since.elapsed() < duration && !log.stopped {
            log.capture(&events, since);
            std::thread::sleep(Duration::from_millis(20));
        }
        block(self.client.close(id))?;
        std::thread::sleep(Duration::from_millis(300));
        log.capture(&events, since);
        let mut out = String::new();
        log.report(&mut out);
        Ok(out)
    }

    fn fanout(&self, surface: NativeHandleId, duration: Duration) -> Outcome {
        let target = Self::preview_target(surface)?;
        let sink = self
            .runtime
            .open_frame_sink::<FrameMeta>(SinkPolicy::queue(4))
            .map_err(|e| format!("{e:?}"))?;
        let config = SessionConfig::primary_monitor()
            .formats(gpu_formats())
            .consent(consenting());
        let session = block(
            self.client
                .open_preview_with_frames_owned(config, target, sink.id()),
        )?;
        let since = Instant::now();
        let id = session.session.id();
        let events = self.client.events(id).map_err(|e| format!("{e:?}"))?;
        let mut log = EventLog::default();
        let mut stats = frames_for(&sink, &events, &mut log, since, duration);
        let elapsed = since.elapsed();
        block(self.client.close(id))?;
        std::thread::sleep(Duration::from_millis(300));
        log.capture(&events, since);
        let mut out = String::new();
        stats.report(&mut out, elapsed);
        log.report(&mut out);
        Ok(out)
    }

    fn audio(&self, duration: Duration) -> Outcome {
        let config = SessionConfig::primary_monitor()
            .formats(gpu_formats())
            .audio(AudioConfig::of(AudioSource::System))
            .consent(consenting());
        let session = block(self.client.open_session_with_audio(
            &self.runtime,
            config,
            SinkPolicy::queue(4),
        ))?;
        let since = Instant::now();
        let events = session.events().map_err(|e| format!("{e:?}"))?;
        let audio = session.audio().ok_or("no audio sink")?;
        let mut log = EventLog::default();
        let mut chunks = AudioStats::default();
        let mut video = FrameStats::default();
        while since.elapsed() < duration && !log.stopped {
            let mut idle = true;
            while let Some(chunk) = audio.try_recv() {
                chunks.add(&chunk);
                idle = false;
            }
            if let Some(frame) = session.video().try_recv() {
                video.add(&frame);
                idle = false;
            }
            if idle {
                log.capture(&events, since);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let elapsed = since.elapsed();
        block(session.close())?;
        std::thread::sleep(Duration::from_millis(300));
        log.capture(&events, since);
        let mut out = String::new();
        chunks.report(&mut out, elapsed);
        video.report(&mut out, elapsed);
        log.report(&mut out);
        Ok(out)
    }

    fn audio_only(&self, duration: Duration) -> Outcome {
        let _grant = block(self.client.request_projection(ProjectionRequest {
            mode: ProjectionMode::EntireScreen,
        }))?;
        let config = AudioConfig::of(AudioSource::System).format(SampleFormat::F32);
        let session = block(self.client.open_audio_session(
            &self.runtime,
            config,
            SinkPolicy::queue(32),
        ))?;
        let since = Instant::now();
        let events = session.events().map_err(|e| format!("{e:?}"))?;
        let mut log = EventLog::default();
        let mut chunks = AudioStats::default();
        while since.elapsed() < duration && !log.stopped {
            if let Some(chunk) = session.audio().try_recv() {
                chunks.add(&chunk);
            } else {
                log.audio(&events, since);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let elapsed = since.elapsed();
        block(session.close())?;
        std::thread::sleep(Duration::from_millis(300));
        log.audio(&events, since);
        let mut out = String::new();
        chunks.report(&mut out, elapsed);
        log.report(&mut out);
        Ok(out)
    }

    /// A grant opens one session; the second use is `GrantExpired`.
    fn grant(&self) -> Outcome {
        let grant = block(self.client.request_projection(ProjectionRequest {
            mode: ProjectionMode::EntireScreen,
        }))?;
        let consent = ConsentConfig {
            grant: Some(grant),
            ..ConsentConfig::default()
        };
        let config = SessionConfig::primary_monitor()
            .formats(gpu_formats())
            .consent(consent.clone());
        let first = self.stream(config.clone(), Duration::from_secs(1))?;
        let again = pollster::block_on(self.client.open_session(
            &self.runtime,
            config,
            SinkPolicy::queue(4),
        ));
        Ok(format!(
            "grant {:?}\nfirst use:\n{first}second use: {:?}\n",
            consent.grant,
            again.map(|s| s.negotiated().size)
        ))
    }

    /// Requests the backend must refuse without showing any UI.
    fn refusals(&self) -> String {
        let cases: Vec<(&str, SessionConfig)> = vec![
            ("no consent", SessionConfig::primary_monitor()),
            (
                "all monitors",
                SessionConfig::of(SourceSelector::AllMonitors).consent(consenting()),
            ),
            (
                "focused",
                SessionConfig::of(SourceSelector::Focused).consent(consenting()),
            ),
            (
                "cpu exhaustion readback on gpu",
                SessionConfig::primary_monitor()
                    .formats(gpu_formats())
                    .exhaustion(istmo_screen_capture::Exhaustion::CopyToCpu)
                    .consent(consenting()),
            ),
        ];
        let mut out = String::new();
        for (name, config) in cases {
            let result = pollster::block_on(self.client.open_session(
                &self.runtime,
                config,
                SinkPolicy::queue(4),
            ));
            let _ = writeln!(out, "{name}: {:?}", result.map(|s| s.negotiated().size));
        }
        out
    }
}

#[derive(Debug, Default)]
struct AudioStats {
    chunks: u64,
    frames: u64,
    discontinuities: u64,
    rate: u32,
    channels: u8,
    format: Option<SampleFormat>,
    next_wall_us: Option<i64>,
    max_wall_jump_us: i64,
    peak: f64,
    latency_us: Vec<i64>,
}

impl AudioStats {
    fn add(&mut self, chunk: &NativeFrame<AudioMeta>) {
        let meta = &chunk.meta;
        self.chunks += 1;
        self.frames += u64::from(meta.frames);
        self.rate = meta.rate;
        self.channels = meta.channels;
        self.format = Some(meta.format);
        if meta.discontinuity {
            self.discontinuities += 1;
        }
        let wall = i64::try_from(meta.wall_us).unwrap_or(i64::MAX);
        if let Some(expected) = self.next_wall_us
            && !meta.discontinuity
        {
            self.max_wall_jump_us = self.max_wall_jump_us.max((wall - expected).abs());
        }
        self.next_wall_us =
            Some(wall + i64::from(meta.frames) * 1_000_000 / i64::from(meta.rate.max(1)));
        self.latency_us.push(now_us() - wall);
        if let Some(bytes) = chunk.buffer.as_bytes() {
            let peak = match meta.format {
                SampleFormat::S16 => bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|s| f64::from(i16::from_le_bytes(*s).unsigned_abs()) / 32768.0)
                    .fold(0.0, f64::max),
                SampleFormat::F32 => bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|s| f64::from(f32::from_le_bytes(*s).abs()))
                    .fold(0.0, f64::max),
            };
            self.peak = self.peak.max(peak);
        }
    }

    fn report(&mut self, out: &mut String, elapsed: Duration) {
        self.latency_us.sort_unstable();
        let median = self
            .latency_us
            .get(self.latency_us.len() / 2)
            .copied()
            .unwrap_or(0);
        let _ = writeln!(
            out,
            "audio chunks {} frames {} ({:.2}s of audio in {:.2}s) {} Hz x{} {:?}; discontinuities {}; wall vs sample clock max jump {} us; peak {:.3}; latency p50 {} us",
            self.chunks,
            self.frames,
            self.frames as f64 / f64::from(self.rate.max(1)),
            elapsed.as_secs_f64(),
            self.rate,
            self.channels,
            self.format,
            self.discontinuities,
            self.max_wall_jump_us,
            self.peak,
            median,
        );
    }
}
