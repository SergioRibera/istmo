//! System and per-process audio through WASAPI loopback, independent of
//! the video backend.
//!
//! - `System`: loopback of the default render endpoint (what the
//!   speakers play).
//! - `Application(name)`: process loopback (build 20348+) including the
//!   process tree of the first process whose executable is `name`
//!   (`spotify` or `spotify.exe`, case-insensitive) or whose pid `name`
//!   spells.
//! - `ExcludeSelf`: process loopback excluding this process's tree
//!   (build 20348+).
//! - `Node(_)`: PipeWire only, `Unsatisfiable(Audio)`.
//!
//! The rate, channel count and sample format of `AudioConfig` are asked
//! for with `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` (the audio engine
//! converts). Loopback delivers no packets while nothing plays; the
//! capture thread fills such gaps with silence so the chunk timeline
//! stays continuous, as a PipeWire monitor's does. `wall_us` follows the
//! sample count (`audio_clock`); `pts_ns` is the engine's QPC position of
//! the chunk, in ns (the same clock as WGC / DXGI `pts_ns`).
//!
//! `System` follows the default render endpoint: when the person picks
//! another output, or the endpoint goes away (`AUDCLNT_E_DEVICE_INVALIDATED`),
//! the stream reopens on the new default at the same rate and channels,
//! and the next chunk carries `discontinuity`.

#![allow(
    unsafe_code,
    reason = "WASAPI COM calls, process-loopback activation and its completion handler"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceCompletionHandler, IAudioCaptureClient,
    IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{GUID, Interface as _};

use crate::types::{
    AudioCaps, AudioConfig, AudioMeta, AudioNegotiated, AudioSource, AudioSourceKind, Availability,
    BackendDetail, CaptureError, Requirement, SampleFormat,
};

use super::super::audio_clock::SampleClock;
use super::super::backend::{AudioBackend, BackendSession};
use super::super::clock::wall_now_us;
use super::super::output::AudioOut;
use super::{os, wide_to_string};

pub(crate) fn backend() -> Arc<dyn AudioBackend> {
    Arc::new(Wasapi)
}

/// `KSDATAFORMAT_SUBTYPE_PCM`.
const SUBTYPE_PCM: GUID = GUID::from_u128(0x0000_0001_0000_0010_8000_00aa_0038_9b71);
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
/// `WAVE_FORMAT_EXTENSIBLE`.
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// The engine's buffer, 100 ns units (200 ms).
const BUFFER_DURATION: i64 = 2_000_000;
/// How often the capture thread reads packets.
const POLL: Duration = Duration::from_millis(10);
/// How long the engine may stay silent before the thread fills the gap
/// with silence.
const GAP: Duration = Duration::from_millis(40);
/// How long setup (activation included) may take.
const SETUP_WAIT: Duration = Duration::from_secs(5);
/// How long dropping a session waits for its thread.
const JOIN_WAIT: Duration = Duration::from_secs(2);
/// How often `System` loopback looks for a new default endpoint.
const ENDPOINT_CHECK: Duration = Duration::from_millis(500);
/// How long a lost endpoint may take to come back before the session
/// fails.
const REOPEN_WAIT: Duration = Duration::from_secs(5);
/// Rate and channels when the source has no mix format (process
/// loopback answers none).
const DEFAULT_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u8 = 2;

fn error(step: &str, message: impl std::fmt::Display) -> CaptureError {
    CaptureError::Protocol(BackendDetail {
        backend: None,
        step: format!("WASAPI: {step}"),
        message: message.to_string(),
    })
}

fn hr(step: &str, err: &windows_core::Error) -> CaptureError {
    error(step, format!("HRESULT {:#010x}", err.code().0))
}

/// Initialises COM (MTA) on this thread for its lifetime.
struct Com;

impl Com {
    fn init() -> Self {
        // SAFETY: balanced by CoUninitialize in drop, on this thread. An
        // already-initialised thread (S_FALSE) is balanced the same way.
        let _initialised = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: balances `init` on the same thread.
        unsafe { CoUninitialize() };
    }
}

/// Where the loopback comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    System,
    Process { pid: u32, include: bool },
}

/// The pid of the first process whose executable is `name`, or the pid
/// `name` spells.
fn process_named(name: &str) -> Option<u32> {
    if let Ok(pid) = name.trim().parse::<u32>() {
        return Some(pid);
    }
    let wanted = name.trim().to_lowercase();
    let wanted = wanted.strip_suffix(".exe").unwrap_or(&wanted).to_owned();
    // SAFETY: a process snapshot, closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0),
        ..PROCESSENTRY32W::default()
    };
    let mut found = None;
    // SAFETY: `entry` is a live PROCESSENTRY32W whose dwSize says so.
    let mut more = unsafe { Process32FirstW(snapshot, &raw mut entry) }.is_ok();
    while more {
        let exe = wide_to_string(&entry.szExeFile).to_lowercase();
        if exe.strip_suffix(".exe").unwrap_or(&exe) == wanted {
            found = Some(entry.th32ProcessID);
            break;
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &raw mut entry) }.is_ok();
    }
    // SAFETY: the snapshot handle is closed once.
    let _closed = unsafe { windows::Win32::Foundation::CloseHandle(snapshot) };
    found
}

fn process_loopback() -> bool {
    os::build() >= os::BUILD_PROCESS_LOOPBACK
}

fn target_of(source: &AudioSource) -> Result<Target, CaptureError> {
    match source {
        AudioSource::System => Ok(Target::System),
        AudioSource::ExcludeSelf | AudioSource::Application(_) if !process_loopback() => {
            Err(CaptureError::OsTooOld(os::version()))
        }
        AudioSource::ExcludeSelf => Ok(Target::Process {
            // SAFETY: no preconditions.
            pid: unsafe { GetCurrentProcessId() },
            include: false,
        }),
        AudioSource::Application(name) => process_named(name)
            .map(|pid| Target::Process { pid, include: true })
            .ok_or_else(|| error("Application", format!("no running process named `{name}`"))),
        AudioSource::Node(_) => Err(CaptureError::unsatisfiable(None, Requirement::Audio)),
    }
}

/// The completion handler of `ActivateAudioInterfaceAsync`, in a module
/// of its own for the code `#[implement]` generates.
mod activation {
    #![allow(
        clippy::inline_always,
        clippy::ref_as_ptr,
        reason = "the COM glue `#[implement]` generates"
    )]

    use windows::Win32::Media::Audio::{
        IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
        IActivateAudioInterfaceCompletionHandler_Impl,
    };
    use windows::Win32::System::Com::{IAgileObject, IAgileObject_Impl};
    use windows::core::implement;

    /// Wakes the thread waiting for the activation.
    #[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
    pub(super) struct Activated(pub(super) flume::Sender<()>);

    impl IAgileObject_Impl for Activated_Impl {}

    impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
        fn ActivateCompleted(
            &self,
            _operation: windows_core::Ref<IActivateAudioInterfaceAsyncOperation>,
        ) -> windows_core::Result<()> {
            // The waiter may have given up.
            let _woken = self.0.send(());
            Ok(())
        }
    }
}

use activation::Activated;

/// An `IAudioClient` for process loopback of `pid`'s tree.
fn activate_process(pid: u32, include: bool) -> Result<IAudioClient, CaptureError> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: if include {
                    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                } else {
                    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                },
            },
        },
    };
    let variant = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: u32::try_from(std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>())
                            .unwrap_or(0),
                        pBlobData: (&raw mut params).cast(),
                    },
                },
            }),
        },
    };
    let (tx, rx) = flume::bounded(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(tx).into();
    // SAFETY: `variant` points to `params`, both alive until the
    // activation completed (waited for below); the handler is a live COM
    // object.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const variant),
            &handler,
        )
    }
    .map_err(|e| hr("ActivateAudioInterfaceAsync", &e))?;
    rx.recv_timeout(SETUP_WAIT).map_err(|_| {
        error(
            "ActivateAudioInterfaceAsync",
            "the activation did not complete",
        )
    })?;
    let mut result = windows_core::HRESULT(0);
    let mut unknown: Option<windows_core::IUnknown> = None;
    // SAFETY: the operation completed; both out values are live locals.
    unsafe { operation.GetActivateResult(&raw mut result, &raw mut unknown) }
        .map_err(|e| hr("GetActivateResult", &e))?;
    result
        .ok()
        .map_err(|e| hr("process loopback activation", &e))?;
    unknown
        .ok_or_else(|| error("process loopback activation", "no interface"))?
        .cast::<IAudioClient>()
        .map_err(|e| hr("IAudioClient", &e))
}

/// The default render endpoint.
fn default_endpoint() -> Result<IMMDevice, CaptureError> {
    // SAFETY: COM is initialised on this thread.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| hr("MMDeviceEnumerator", &e))?;
    // SAFETY: `enumerator` is live.
    unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|e| hr("GetDefaultAudioEndpoint", &e))
}

/// An endpoint's id string.
fn endpoint_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: `device` is live; the string is freed below.
    let id = unsafe { device.GetId() }.ok()?;
    // SAFETY: GetId returned a NUL-terminated string.
    let text = unsafe { id.to_string() }.ok();
    // SAFETY: allocated by the endpoint with CoTaskMemAlloc, freed once.
    unsafe { CoTaskMemFree(Some(id.0.cast_const().cast())) };
    text
}

/// The id of the default render endpoint now.
fn default_endpoint_id() -> Option<String> {
    default_endpoint().ok().as_ref().and_then(endpoint_id)
}

/// An activated client, its mix rate and channel count (when the
/// source has one), and its endpoint id.
type Client = (IAudioClient, Option<(u32, u8)>, Option<String>);

/// The default render endpoint's `IAudioClient`, its mix rate and
/// channel count, and its id.
fn activate_system() -> Result<Client, CaptureError> {
    let device = default_endpoint()?;
    let id = endpoint_id(&device);
    // SAFETY: `device` is live; no activation parameters.
    let client: IAudioClient =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| hr("IMMDevice.Activate", &e))?;
    // SAFETY: `client` is live; the returned format is freed below.
    let mix = unsafe { client.GetMixFormat() }
        .ok()
        .filter(|p| !p.is_null())
        .map(|format| {
            // SAFETY: GetMixFormat returned a valid WAVEFORMATEX; read by
            // value (it is packed).
            let header = unsafe { format.read_unaligned() };
            // SAFETY: the format was allocated by the audio engine with
            // CoTaskMemAlloc and is freed once.
            unsafe { CoTaskMemFree(Some(format.cast_const().cast())) };
            (
                header.nSamplesPerSec,
                u8::try_from(header.nChannels).unwrap_or(DEFAULT_CHANNELS),
            )
        });
    Ok((client, mix, id))
}

/// The extensible format for `rate` × `channels` of `format`.
fn wave_format(rate: u32, channels: u8, format: SampleFormat) -> WAVEFORMATEXTENSIBLE {
    let bits = u16::try_from(format.bytes() * 8).unwrap_or(32);
    let block = u16::from(channels) * (bits / 8);
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: FORMAT_EXTENSIBLE,
            nChannels: u16::from(channels),
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: u16::try_from(
                std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>(),
            )
            .unwrap_or(22),
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: bits,
        },
        dwChannelMask: match channels {
            1 => 0x4,   // SPEAKER_FRONT_CENTER
            2 => 0x3,   // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
            6 => 0x3F,  // 5.1
            8 => 0x63F, // 7.1
            _ => 0,
        },
        SubFormat: match format {
            SampleFormat::S16 => SUBTYPE_PCM,
            SampleFormat::F32 => SUBTYPE_FLOAT,
        },
    }
}

struct Stream {
    /// The endpoint `System` loopback runs on.
    endpoint: Option<String>,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    negotiated: AudioNegotiated,
    block: usize,
}

fn open_stream(config: &AudioConfig, target: Target) -> Result<Stream, CaptureError> {
    let (client, mix, endpoint) = match target {
        Target::System => activate_system()?,
        Target::Process { pid, include } => (activate_process(pid, include)?, None, None),
    };
    let (mix_rate, mix_channels) = mix.unwrap_or((DEFAULT_RATE, DEFAULT_CHANNELS));
    let rate = config.rate.unwrap_or(mix_rate);
    let channels = config.channels.unwrap_or(mix_channels).max(1);
    let format = wave_format(rate, channels, config.format);
    // SAFETY: `format` is a complete WAVEFORMATEXTENSIBLE alive for the
    // call; shared-mode loopback with engine-side conversion.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            BUFFER_DURATION,
            0,
            (&raw const format).cast(),
            None,
        )
    }
    .map_err(|e| hr("IAudioClient.Initialize", &e))?;
    // SAFETY: `client` is initialised.
    let capture: IAudioCaptureClient =
        unsafe { client.GetService() }.map_err(|e| hr("IAudioCaptureClient", &e))?;
    // SAFETY: as above.
    unsafe { client.Start() }.map_err(|e| hr("IAudioClient.Start", &e))?;
    Ok(Stream {
        endpoint,
        client,
        capture,
        negotiated: AudioNegotiated {
            source: config.source.clone(),
            rate,
            channels,
            format: config.format,
        },
        block: usize::from(channels) * usize::try_from(config.format.bytes()).unwrap_or(4),
    })
}

/// The capture loop's clock and gap filling.
struct Timeline {
    clock: SampleClock,
    started: Instant,
    /// Frames delivered since `started`.
    frames: u64,
    /// When the engine last delivered a packet.
    last_data: Instant,
    rate: u32,
}

impl Timeline {
    /// Frames of silence the stream owes: none while packets keep
    /// coming, else how far it fell behind the wall clock (once that
    /// exceeds the gap threshold).
    fn owed(&self) -> u64 {
        let elapsed = self.started.elapsed();
        let due = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX) * u64::from(self.rate)
            / 1_000_000;
        let gap = u64::try_from(GAP.as_micros()).unwrap_or(0) * u64::from(self.rate) / 1_000_000;
        if self.last_data.elapsed() < GAP {
            return 0;
        }
        let behind = due.saturating_sub(self.frames);
        if behind > gap { behind } else { 0 }
    }
}

fn deliver(
    out: &AudioOut,
    timeline: &mut Timeline,
    negotiated: &AudioNegotiated,
    pcm: Vec<u8>,
    frames: u64,
    pts_ns: u64,
    discontinuity: bool,
) -> bool {
    let stamp = timeline.clock.stamp(frames, negotiated.rate, wall_now_us());
    timeline.frames = timeline.frames.saturating_add(frames);
    let meta = AudioMeta {
        seq: 0,
        pts_ns,
        wall_us: stamp.wall_us,
        rate: negotiated.rate,
        channels: negotiated.channels,
        format: negotiated.format,
        frames: u32::try_from(frames).unwrap_or(u32::MAX),
        discontinuity: discontinuity || stamp.reanchored_by_us.is_some(),
    };
    out.deliver(meta, pcm)
}

/// Why reading packets stopped.
enum DrainError {
    /// The endpoint went away: reopen on the default one.
    Invalidated,
    Failed(CaptureError),
}

/// A failed WASAPI call while reading packets.
fn drain_error(step: &str, err: &windows_core::Error) -> DrainError {
    if invalidated(err.code()) {
        DrainError::Invalidated
    } else {
        DrainError::Failed(hr(step, err))
    }
}

fn invalidated(code: windows_core::HRESULT) -> bool {
    code == AUDCLNT_E_DEVICE_INVALIDATED
}

/// Read every queued packet into `out`. `false` once the sink is gone.
/// `discontinuity` marks the first chunk (after a reopen).
fn drain(
    stream: &Stream,
    out: &AudioOut,
    timeline: &mut Timeline,
    discontinuity: &mut bool,
) -> Result<bool, DrainError> {
    loop {
        // SAFETY: `capture` is the started client's service.
        let next = unsafe { stream.capture.GetNextPacketSize() }
            .map_err(|e| drain_error("GetNextPacketSize", &e))?;
        if next == 0 {
            return Ok(true);
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames = 0_u32;
        let mut flags = 0_u32;
        let mut qpc = 0_u64;
        // SAFETY: all out values are live locals; the packet is released
        // below before the next GetBuffer.
        unsafe {
            stream.capture.GetBuffer(
                &raw mut data,
                &raw mut frames,
                &raw mut flags,
                None,
                Some(&raw mut qpc),
            )
        }
        .map_err(|e| drain_error("GetBuffer", &e))?;
        let len = usize::try_from(frames).unwrap_or(0) * stream.block;
        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0.cast_unsigned() != 0 || data.is_null();
        let pcm = if silent {
            vec![0_u8; len]
        } else {
            // SAFETY: GetBuffer handed out `frames` frames of `block`
            // bytes at `data`, valid until ReleaseBuffer.
            unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
        };
        // SAFETY: releases the packet GetBuffer handed out.
        unsafe { stream.capture.ReleaseBuffer(frames) }
            .map_err(|e| drain_error("ReleaseBuffer", &e))?;
        let discontinuity = std::mem::take(discontinuity)
            || flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0.cast_unsigned() != 0;
        let pts = if qpc == 0 {
            os::qpc_now_ns()
        } else {
            qpc.saturating_mul(100)
        };
        timeline.last_data = Instant::now();
        if !deliver(
            out,
            timeline,
            &stream.negotiated,
            pcm,
            u64::from(frames),
            pts,
            discontinuity,
        ) {
            return Ok(false);
        }
    }
}

/// Reopen the stream on the (new) default endpoint, at the rate and
/// channels already negotiated, retrying for [`REOPEN_WAIT`].
fn reopen(
    config: &AudioConfig,
    target: Target,
    negotiated: &AudioNegotiated,
    stop: &AtomicBool,
) -> Result<Stream, CaptureError> {
    let mut pinned = config.clone();
    pinned.rate = Some(negotiated.rate);
    pinned.channels = Some(negotiated.channels);
    let deadline = Instant::now() + REOPEN_WAIT;
    loop {
        match open_stream(&pinned, target) {
            Ok(stream) => return Ok(stream),
            Err(err) if Instant::now() >= deadline || stop.load(Ordering::SeqCst) => {
                return Err(err);
            }
            Err(_) => std::thread::sleep(ENDPOINT_CHECK),
        }
    }
}

/// Whether `System` loopback should move to another endpoint.
fn endpoint_moved(current: Option<&str>, default: Option<&str>) -> bool {
    matches!((current, default), (Some(current), Some(default)) if current != default)
}

fn run(
    mut stream: Stream,
    config: &AudioConfig,
    target: Target,
    out: &AudioOut,
    stop: &AtomicBool,
) {
    let mut discontinuity = false;
    let mut last_check = Instant::now();
    let mut timeline = Timeline {
        clock: SampleClock::default(),
        started: Instant::now(),
        frames: 0,
        rate: stream.negotiated.rate,
        last_data: Instant::now(),
    };
    while !stop.load(Ordering::SeqCst) && !out.is_closed() {
        std::thread::sleep(POLL);
        let mut lost = match drain(&stream, out, &mut timeline, &mut discontinuity) {
            Ok(true) => false,
            Ok(false) => break,
            Err(DrainError::Invalidated) => true,
            Err(DrainError::Failed(err)) => {
                out.fail(err);
                break;
            }
        };
        if target == Target::System && last_check.elapsed() >= ENDPOINT_CHECK {
            last_check = Instant::now();
            lost |= endpoint_moved(stream.endpoint.as_deref(), default_endpoint_id().as_deref());
        }
        if lost {
            // SAFETY: stops the old client before it is released.
            let _stopped = unsafe { stream.client.Stop() };
            match reopen(config, target, &stream.negotiated, stop) {
                Ok(fresh) => {
                    tracing::debug!("wasapi loopback moved to the default endpoint");
                    stream = fresh;
                    discontinuity = true;
                }
                Err(err) => {
                    out.fail(err);
                    return;
                }
            }
            continue;
        }
        // Loopback is silent when nothing plays: no packets at all. Fill
        // the gap so the timeline (and the consumer's clock) keeps going.
        let owed = timeline.owed();
        if owed > 0 {
            let pcm = vec![0_u8; usize::try_from(owed).unwrap_or(0) * stream.block];
            if !deliver(
                out,
                &mut timeline,
                &stream.negotiated,
                pcm,
                owed,
                os::qpc_now_ns(),
                std::mem::take(&mut discontinuity),
            ) {
                break;
            }
        }
    }
    // SAFETY: stops the started client.
    let _stopped = unsafe { stream.client.Stop() };
}

#[derive(Debug)]
struct Wasapi;

impl AudioBackend for Wasapi {
    fn availability(&self) -> Availability {
        let probe = std::thread::Builder::new()
            .name("istmo-screen-capture-wasapi-probe".to_owned())
            .spawn(|| {
                let _com = Com::init();
                activate_system().map(|_| ())
            });
        match probe.map(JoinHandle::join) {
            Ok(Ok(Ok(()))) => Availability::Ready,
            Ok(Ok(Err(err))) => Availability::NotOnThisSession(err.to_string()),
            _ => Availability::NotOnThisSession("the WASAPI probe did not run".to_owned()),
        }
    }

    fn caps(&self) -> AudioCaps {
        let mut sources = vec![AudioSourceKind::System];
        if process_loopback() {
            sources.extend([AudioSourceKind::Application, AudioSourceKind::ExcludeSelf]);
        }
        AudioCaps {
            sources,
            formats: vec![SampleFormat::S16, SampleFormat::F32],
            may_be_silent: false,
        }
    }

    fn open(
        &self,
        config: &AudioConfig,
        out: AudioOut,
    ) -> Result<(Box<dyn BackendSession>, AudioNegotiated), CaptureError> {
        let target = target_of(&config.source)?;
        let config = config.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let (ready_tx, ready) = flume::bounded(1);
        let thread = std::thread::Builder::new()
            .name("istmo-screen-capture-wasapi".to_owned())
            .spawn(move || {
                let _com = Com::init();
                let stream = match open_stream(&config, target) {
                    Ok(stream) => stream,
                    Err(err) => {
                        let _told = ready_tx.send(Err(err));
                        return;
                    }
                };
                if ready_tx.send(Ok(stream.negotiated.clone())).is_err() {
                    // The opener gave up waiting.
                    return;
                }
                run(stream, &config, target, &out, &flag);
            })
            .map_err(|e| error("spawn capture thread", e))?;
        let negotiated = match ready.recv_timeout(SETUP_WAIT) {
            Ok(Ok(negotiated)) => negotiated,
            Ok(Err(err)) => {
                let _joined = thread.join();
                return Err(err);
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                return Err(error("open", "the audio stream did not start in time"));
            }
        };
        Ok((
            Box::new(Session {
                stop,
                thread: Some(thread),
            }),
            negotiated,
        ))
    }
}

/// A running capture; dropping it stops the thread.
#[derive(Debug)]
struct Session {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl BackendSession for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + JOIN_WAIT;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        if thread.is_finished() {
            let _joined = thread.join();
        } else {
            tracing::warn!("wasapi capture thread did not stop in time; detached");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_describe_their_frames() {
        let f32 = wave_format(48_000, 2, SampleFormat::F32);
        let header = f32.Format;
        assert_eq!({ header.nBlockAlign }, 8);
        assert_eq!({ header.nAvgBytesPerSec }, 384_000);
        assert_eq!({ f32.SubFormat }, SUBTYPE_FLOAT);
        let s16 = wave_format(44_100, 1, SampleFormat::S16);
        assert_eq!({ s16.Format.nBlockAlign }, 2);
        assert_eq!({ s16.SubFormat }, SUBTYPE_PCM);
    }

    #[test]
    fn gaps_owe_silence_only_past_the_threshold() {
        let timeline = Timeline {
            clock: SampleClock::default(),
            started: Instant::now(),
            frames: 0,
            rate: 48_000,
            last_data: Instant::now(),
        };
        assert_eq!(timeline.owed(), 0, "nothing owed at the start");
        let past = Instant::now()
            .checked_sub(Duration::from_millis(500))
            .expect("past");
        let flowing = Timeline {
            started: past,
            ..timeline
        };
        assert_eq!(flowing.owed(), 0, "packets keep coming: nothing owed");
        let silent = Timeline {
            last_data: past,
            ..flowing
        };
        let owed = silent.owed();
        assert!((23_000..=26_000).contains(&owed), "{owed}");
    }

    #[test]
    fn system_loopback_follows_the_default_endpoint() {
        assert!(endpoint_moved(Some("a"), Some("b")));
        assert!(!endpoint_moved(Some("a"), Some("a")));
        // An unknown id (a failed query) is no reason to move.
        assert!(!endpoint_moved(Some("a"), None));
        assert!(!endpoint_moved(None, Some("b")));
        assert!(invalidated(AUDCLNT_E_DEVICE_INVALIDATED));
        assert!(!invalidated(windows::Win32::Foundation::E_FAIL));
    }

    #[test]
    fn pids_are_taken_as_they_are() {
        assert_eq!(process_named("1234"), Some(1234));
    }
}
