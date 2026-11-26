//! System and application audio through PipeWire, independent of the
//! video backend.
//!
//! - `System`: the default sink's monitor (`stream.capture.sink`), i.e.
//!   what the speakers play.
//! - `Node(id)`: that node (a sink is captured through its monitor).
//! - `Application(name)`: the output streams whose `application.name`
//!   or `application.process.binary` is `name`, linked directly.
//! - `ExcludeSelf`: unsupported (PipeWire has no process-excluding
//!   monitor; it would need a per-application mix).
//!
//! Chunks are interleaved PCM copied into CPU buffers (the PipeWire
//! buffer goes back at once). `wall_us` follows the sample count
//! ([`clock`]); `pts_ns` is the same instant on `CLOCK_MONOTONIC`.

mod clock;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;
use pw::spa::param::ParamType;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::format_utils;
use pw::spa::pod::Object;
use pw::spa::utils::{Direction, SpaTypes};
use pw::stream::{StreamFlags, StreamRc};

use crate::types::{
    AudioCaps, AudioConfig, AudioMeta, AudioNegotiated, AudioSource, AudioSourceKind, Availability,
    CaptureError, Requirement, SampleFormat, StopReason,
};

use self::clock::SampleClock;
use super::super::backend::{AudioBackend, BackendSession};
use super::super::clock::{monotonic_ns, wall_now_us};
use super::super::output::AudioOut;
use super::pipewire::{self as pwx, LoopCtx, LoopThread, Remote, StreamEnd};

pub(crate) fn backend() -> Arc<dyn AudioBackend> {
    Arc::new(PipeWireAudio)
}

/// `PW_KEY_TARGET_OBJECT` (PipeWire 0.3.44); the `pipewire` crate only
/// names it behind a version feature.
const TARGET_OBJECT: &str = "target.object";

/// How long the availability probe waits for the session daemon.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

fn error(step: &str, message: impl std::fmt::Display) -> CaptureError {
    pwx::error(None, &format!("PipeWire audio: {step}"), message)
}

#[derive(Debug)]
struct PipeWireAudio;

impl AudioBackend for PipeWireAudio {
    fn availability(&self) -> Availability {
        // One connection to the session daemon, closed again at once.
        match LoopThread::spawn(
            None,
            "istmo-screen-capture-audio-probe",
            Remote::SessionDaemon,
            PROBE_TIMEOUT,
            |_ctx, ready| {
                // The caller gave up waiting when this fails.
                let _unwanted = ready.send(Ok(()));
            },
        ) {
            Ok((_probe, ())) => Availability::Ready,
            Err(err) => Availability::NotOnThisSession(err.to_string()),
        }
    }

    fn caps(&self) -> AudioCaps {
        AudioCaps {
            sources: vec![
                AudioSourceKind::System,
                AudioSourceKind::Application,
                AudioSourceKind::Node,
            ],
            formats: vec![SampleFormat::S16, SampleFormat::F32],
            may_be_silent: false,
        }
    }

    fn open(
        &self,
        config: &AudioConfig,
        out: AudioOut,
    ) -> Result<(Box<dyn BackendSession>, AudioNegotiated), CaptureError> {
        let target = Target::of(&config.source)?;
        let format_pod = format_pod(config)?;
        let config = config.clone();
        let (thread, negotiated) = LoopThread::spawn(
            None,
            "istmo-screen-capture-audio",
            Remote::SessionDaemon,
            pwx::SETUP_TIMEOUT,
            move |ctx, ready| run(ctx, ready, &target, &format_pod, config, out),
        )?;
        Ok((Box::new(PipeWireSession { _thread: thread }), negotiated))
    }
}

/// A running capture; dropping it stops the loop.
#[derive(Debug)]
struct PipeWireSession {
    _thread: LoopThread,
}

impl BackendSession for PipeWireSession {}

/// Where the capture stream links.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    DefaultMonitor,
    Node(u32),
    Application(String),
}

impl Target {
    fn of(source: &AudioSource) -> Result<Self, CaptureError> {
        match source {
            AudioSource::System => Ok(Self::DefaultMonitor),
            AudioSource::Node(id) => Ok(Self::Node(*id)),
            AudioSource::Application(name) => Ok(Self::Application(name.clone())),
            AudioSource::ExcludeSelf => Err(CaptureError::unsatisfiable(None, Requirement::Audio)),
        }
    }
}

const fn spa_format(format: SampleFormat) -> AudioFormat {
    match format {
        SampleFormat::S16 => AudioFormat::S16LE,
        SampleFormat::F32 => AudioFormat::F32LE,
    }
}

fn sample_format(format: AudioFormat) -> Option<SampleFormat> {
    if format == AudioFormat::S16LE {
        Some(SampleFormat::S16)
    } else if format == AudioFormat::F32LE {
        Some(SampleFormat::F32)
    } else {
        None
    }
}

/// The `EnumFormat` pod: the sample format, and rate / channels only
/// when the caller set them (the graph's own otherwise). Properties are
/// hints, so PipeWire's converter resamples / remixes instead of
/// failing to link.
fn format_pod(config: &AudioConfig) -> Result<Vec<u8>, CaptureError> {
    let mut info = AudioInfoRaw::new();
    info.set_format(spa_format(config.format));
    if let Some(rate) = config.rate {
        info.set_rate(rate);
    }
    if let Some(channels) = config.channels {
        info.set_channels(u32::from(channels));
        let mut position = [0; pw::spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
        match channels {
            1 => position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_MONO,
            2 => {
                position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
                position[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
            }
            // Leave the layout to the graph.
            _ => info.set_flags(pw::spa::param::audio::AudioInfoRawFlags::UNPOSITIONED),
        }
        info.set_position(position);
    }
    pwx::serialize(
        None,
        "audio format pod",
        Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        },
    )
}

/// State of the audio stream, on its loop thread.
struct StreamData {
    out: AudioOut,
    source: AudioSource,
    negotiated: Option<AudioNegotiated>,
    ready: Option<flume::Sender<Result<AudioNegotiated, CaptureError>>>,
    clock: SampleClock,
}

impl StreamData {
    fn report(&mut self, result: Result<AudioNegotiated, CaptureError>) {
        if let Some(ready) = self.ready.take() {
            // The caller gave up waiting when this fails.
            let _unwanted = ready.send(result);
        }
    }
}

fn run(
    ctx: LoopCtx,
    ready: &flume::Sender<Result<AudioNegotiated, CaptureError>>,
    target: &Target,
    format_pod: &[u8],
    config: AudioConfig,
    out: AudioOut,
) {
    let fail = |err| {
        // The caller gave up waiting when this fails.
        let _unwanted = ready.send(Err(err));
    };
    let linked = match target {
        Target::DefaultMonitor => Linked::Sink(None),
        Target::Node(id) => Linked::Sink(Some(*id)),
        Target::Application(name) => match application_streams(&ctx, name).and_then(|nodes| {
            nodes
                .first()
                .copied()
                .ok_or_else(|| error("application", format!("no output stream of `{name}`")))
        }) {
            Ok(node) => Linked::Stream(node),
            Err(err) => {
                fail(err);
                return;
            }
        },
    };
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
        *pw::keys::NODE_NAME => "istmo-screen-capture-audio",
    };
    match linked {
        Linked::Sink(node) => {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
            if let Some(node) = node {
                props.insert(TARGET_OBJECT, node.to_string());
            }
        }
        Linked::Stream(node) => props.insert(TARGET_OBJECT, node.to_string()),
    }
    let stream = match StreamRc::new(ctx.core.clone(), "istmo-screen-capture-audio", props) {
        Ok(stream) => stream,
        Err(err) => {
            fail(error("create the stream", err));
            return;
        }
    };
    let data = Rc::new(RefCell::new(StreamData {
        out,
        source: config.source,
        negotiated: None,
        ready: Some(ready.clone()),
        clock: SampleClock::default(),
    }));
    let listener = stream
        .add_local_listener_with_user_data(Rc::clone(&data))
        .state_changed({
            let mainloop = ctx.mainloop.clone();
            move |_, data, old, new| {
                let Some(end) = StreamEnd::of(&old, &new) else {
                    return;
                };
                let err = end.into_error(None);
                let mut data = data.borrow_mut();
                data.out.fail(err.clone());
                data.report(Err(err));
                mainloop.quit();
            }
        })
        .param_changed(|_, data, id, param| on_format(&mut data.borrow_mut(), id, param))
        .process({
            let mainloop = ctx.mainloop.clone();
            move |stream, data| {
                if !on_chunk(stream, &mut data.borrow_mut()) {
                    mainloop.quit();
                }
            }
        })
        .register();
    let _listener = match listener {
        Ok(listener) => listener,
        Err(err) => {
            fail(error("register the stream listener", err));
            return;
        }
    };
    let connected = pwx::pod(None, format_pod).and_then(|pod| {
        // No RT_PROCESS: `process` then runs on this loop thread, like
        // `param_changed`, so the shared state is never touched from two
        // threads, and it may allocate.
        stream
            .connect(
                Direction::Input,
                None,
                StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::DONT_RECONNECT,
                &mut [pod],
            )
            .map_err(|err| error("connect the stream", err))
    });
    if let Err(err) = connected {
        fail(err);
        return;
    }
    ctx.run();
    data.borrow().out.stop(StopReason::Closed);
}

/// What the capture stream links to.
#[derive(Debug, Clone, Copy)]
enum Linked {
    /// A sink's monitor (the default one for `None`).
    Sink(Option<u32>),
    /// An application's output stream.
    Stream(u32),
}

fn on_format(data: &mut StreamData, id: u32, param: Option<&pw::spa::pod::Pod>) {
    let Some(param) = param else { return };
    if id != ParamType::Format.as_raw() {
        return;
    }
    let Ok((MediaType::Audio, MediaSubtype::Raw)) = format_utils::parse_format(param) else {
        return;
    };
    let mut info = AudioInfoRaw::new();
    if let Err(err) = info.parse(param) {
        data.report(Err(error("parse the format", format!("{err:?}"))));
        return;
    }
    let Some(format) = sample_format(info.format()) else {
        data.report(Err(error(
            "format",
            format!("unexpected {:?}", info.format()),
        )));
        return;
    };
    let negotiated = AudioNegotiated {
        source: data.source.clone(),
        rate: info.rate(),
        channels: u8::try_from(info.channels()).unwrap_or(u8::MAX),
        format,
    };
    data.negotiated = Some(negotiated.clone());
    data.report(Ok(negotiated));
}

/// Copy one chunk out. `false` once the sink is gone.
fn on_chunk(stream: &pw::stream::Stream, data: &mut StreamData) -> bool {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return true;
    };
    let Some(negotiated) = data.negotiated.clone() else {
        return true;
    };
    let Some(plane) = buffer.datas_mut().first_mut() else {
        return true;
    };
    let chunk = plane.chunk();
    let (offset, size) = (chunk.offset() as usize, chunk.size() as usize);
    let Some(bytes) = plane.data() else {
        return true;
    };
    let end = offset.saturating_add(size).min(bytes.len());
    let Some(pcm) = bytes.get(offset..end).filter(|pcm| !pcm.is_empty()) else {
        return true;
    };
    let frame_bytes = negotiated.format.bytes() as usize * usize::from(negotiated.channels.max(1));
    let frames = pcm.len() / frame_bytes;
    if frames == 0 {
        return true;
    }
    let pcm = pcm.get(..frames * frame_bytes).unwrap_or(pcm).to_vec();
    drop(buffer);

    let now_wall = wall_now_us();
    let now_mono = monotonic_ns();
    let stamp = data.clock.stamp(frames as u64, negotiated.rate, now_wall);
    // The same instant on CLOCK_MONOTONIC.
    let pts_ns =
        now_mono.saturating_sub(now_wall.saturating_sub(stamp.wall_us).saturating_mul(1000));
    let meta = AudioMeta {
        seq: 0,
        pts_ns,
        wall_us: stamp.wall_us,
        rate: negotiated.rate,
        channels: negotiated.channels,
        format: negotiated.format,
        frames: u32::try_from(frames).unwrap_or(u32::MAX),
        discontinuity: stamp.reanchored_by_us.is_some(),
    };
    if let Some(drift_us) = stamp.reanchored_by_us {
        tracing::debug!(
            drift_us,
            "screen-capture audio: re-anchored the chunk clock"
        );
    }
    data.out.deliver(meta, pcm)
}

/// Output stream nodes of an application (by `application.name` or
/// `application.process.binary`), after one registry round trip.
fn application_streams(ctx: &LoopCtx, name: &str) -> Result<Vec<u32>, CaptureError> {
    let registry = ctx
        .core
        .get_registry_rc()
        .map_err(|err| error("registry", err))?;
    let found = Rc::new(RefCell::new(Vec::new()));
    let _globals = registry
        .add_listener_local()
        .global({
            let found = Rc::clone(&found);
            let name = name.to_owned();
            move |global| {
                if global.type_ != pw::types::ObjectType::Node {
                    return;
                }
                let Some(props) = global.props else { return };
                let is_output_stream = props.get("media.class") == Some("Stream/Output/Audio");
                let matches = [
                    props.get("application.name"),
                    props.get("application.process.binary"),
                ]
                .into_iter()
                .flatten()
                .any(|value| value == name);
                if is_output_stream && matches {
                    found.borrow_mut().push(global.id);
                }
            }
        })
        .register();
    let pending = ctx
        .core
        .sync(0)
        .map_err(|err| error("registry sync", err))?;
    let _done = ctx
        .core
        .add_listener_local()
        .done({
            let mainloop = ctx.mainloop.clone();
            move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    mainloop.quit();
                }
            }
        })
        .register();
    // Bounded: a core that never answers the sync must not hang the
    // session.
    let timer = ctx.mainloop.loop_().add_timer({
        let mainloop = ctx.mainloop.clone();
        move |_| mainloop.quit()
    });
    let _armed = timer.update_timer(Some(Duration::from_secs(2)), None);
    ctx.mainloop.run();
    Ok(found.take())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_pods_are_valid_with_and_without_a_shape() {
        let native = format_pod(&AudioConfig::default()).expect("pod");
        assert!(pw::spa::pod::Pod::from_bytes(&native).is_some());
        let fixed = format_pod(&AudioConfig {
            rate: Some(48_000),
            channels: Some(2),
            format: SampleFormat::S16,
            ..AudioConfig::default()
        })
        .expect("pod");
        assert!(pw::spa::pod::Pod::from_bytes(&fixed).is_some());
        assert!(
            fixed.len() > native.len(),
            "rate, channels and positions are added"
        );
        let surround = format_pod(&AudioConfig {
            channels: Some(6),
            ..AudioConfig::default()
        })
        .expect("pod");
        assert!(pw::spa::pod::Pod::from_bytes(&surround).is_some());
    }

    #[test]
    fn sample_formats_round_trip() {
        for format in [SampleFormat::S16, SampleFormat::F32] {
            assert_eq!(sample_format(spa_format(format)), Some(format));
        }
        assert_eq!(sample_format(AudioFormat::S24LE), None);
    }

    /// A few seconds of the default sink monitor (no dialog involved).
    /// Runs only with `ISTMO_SC_TEST_PIPEWIRE` set, on a machine with a
    /// PipeWire session daemon.
    #[test]
    fn live_capture_of_the_default_monitor() {
        if std::env::var_os("ISTMO_SC_TEST_PIPEWIRE").is_none() {
            eprintln!("skipped: set ISTMO_SC_TEST_PIPEWIRE");
            return;
        }
        let runtime = istmo_core::Runtime::mock().runtime;
        let sink = runtime
            .open_frame_sink::<AudioMeta>(istmo_core::SinkPolicy::queue(512))
            .expect("sink");
        let sender = runtime.frame_sender(sink.id()).expect("sender");
        let out = AudioOut::new(sender);
        let config = AudioConfig {
            rate: Some(48_000),
            channels: Some(2),
            format: SampleFormat::S16,
            ..AudioConfig::default()
        };
        let (session, negotiated) = PipeWireAudio.open(&config, out).expect("open");
        eprintln!("negotiated: {negotiated:?}");
        std::thread::sleep(Duration::from_secs(3));
        drop(session);
        let chunks: Vec<_> = std::iter::from_fn(|| sink.try_recv()).collect();
        let frames: u64 = chunks.iter().map(|c| u64::from(c.meta.frames)).sum();
        eprintln!("{} chunks, {frames} frames", chunks.len());
        assert!(
            !chunks.is_empty(),
            "the monitor delivers chunks (silence included)"
        );
        let rate = u64::from(negotiated.rate);
        assert!(
            frames > rate && frames < rate * 4,
            "about three seconds of samples"
        );
        for pair in chunks.windows(2) {
            assert!(pair[1].meta.seq > pair[0].meta.seq);
            assert!(pair[1].meta.wall_us >= pair[0].meta.wall_us);
        }
        let first = &chunks[0].meta;
        let bytes = chunks[0].buffer.as_bytes().map_or(0, <[u8]>::len);
        assert_eq!(
            bytes,
            first.frames as usize * usize::from(first.channels) * 2
        );
    }

    #[test]
    fn excluding_this_process_is_unsatisfiable() {
        assert_eq!(
            Target::of(&AudioSource::ExcludeSelf),
            Err(CaptureError::unsatisfiable(None, Requirement::Audio))
        );
        assert_eq!(Target::of(&AudioSource::Node(42)), Ok(Target::Node(42)));
    }
}
