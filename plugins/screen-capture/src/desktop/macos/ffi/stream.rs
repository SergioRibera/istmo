//! `SCStream` with the plugin's output / delegate object, its
//! configuration, and one-shot captures (`SCScreenshotManager`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCCaptureDynamicRange, SCScreenshotManager, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamOutput, SCStreamOutputType,
};

use crate::types::{CaptureError, OsVersion};

use super::super::super::sck_map::{self, ID};
use super::{Filter, Sample, Shared, error_of, wait};

/// How long starting, stopping or reconfiguring a stream may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Which output a sample came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Screen,
    Audio,
}

/// What a running stream reports to.
pub(crate) trait Sink: Send + Sync + 'static {
    /// One sample, on the stream's serial queue.
    fn sample(&self, kind: Kind, sample: Sample);

    /// The stream stopped by itself (`stream:didStopWithError:`).
    fn stopped(&self, error: CaptureError);
}

/// An `SCStreamConfiguration`, as plain data.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one switch per SCStreamConfiguration property"
)]
pub(crate) struct Settings {
    /// The OS, so only properties it has are set.
    pub(crate) os: OsVersion,
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// `pixelFormat`.
    pub(crate) fourcc: u32,
    /// `minimumFrameInterval` as `(value, timescale)`.
    pub(crate) interval: (i64, i32),
    pub(crate) queue_depth: u8,
    pub(crate) shows_cursor: bool,
    pub(crate) scales_to_fit: bool,
    /// `colorSpaceName = kCGColorSpaceSRGB` (SDR RGB formats).
    pub(crate) srgb: bool,
    /// `captureDynamicRange = .hdrLocalDisplay` (15+).
    pub(crate) hdr: bool,
    /// `includeChildWindows` (14.2+).
    pub(crate) child_windows: bool,
    /// `capturesAudio` with its rate and channel count (13+).
    pub(crate) audio: Option<AudioSettings>,
}

/// The audio half of a configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioSettings {
    pub(crate) rate: u32,
    pub(crate) channels: u8,
    pub(crate) exclude_self: bool,
}

impl Settings {
    // Every setter below takes plain values on a configuration this
    // function owns; the version-gated ones are only called where the OS
    // has them (an unknown selector would raise).
    fn build(&self) -> Retained<SCStreamConfiguration> {
        // SAFETY: makes a default configuration.
        let config = unsafe { SCStreamConfiguration::new() };
        // SAFETY: a plain setter, see above.
        unsafe { config.setWidth(self.width.max(1)) };
        // SAFETY: a plain setter, see above.
        unsafe { config.setHeight(self.height.max(1)) };
        // SAFETY: a plain setter, see above.
        unsafe { config.setPixelFormat(self.fourcc) };
        let interval = CMTime {
            value: self.interval.0,
            timescale: self.interval.1,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        // SAFETY: a plain setter, see above.
        unsafe { config.setMinimumFrameInterval(interval) };
        // SAFETY: a plain setter, see above.
        unsafe { config.setQueueDepth(isize::from(self.queue_depth)) };
        // SAFETY: a plain setter, see above.
        unsafe { config.setShowsCursor(self.shows_cursor) };
        // SAFETY: a plain setter, see above.
        unsafe { config.setScalesToFit(self.scales_to_fit) };
        if self.srgb {
            // SAFETY: an immutable framework constant.
            let srgb = unsafe { objc2_core_graphics::kCGColorSpaceSRGB };
            // SAFETY: a plain setter, see above.
            unsafe { config.setColorSpaceName(srgb) };
        }
        if self.hdr && sck_map::at_least(self.os, sck_map::HDR) {
            // SAFETY: a setter that exists from 15.0, checked above.
            unsafe { config.setCaptureDynamicRange(SCCaptureDynamicRange::HDRLocalDisplay) };
        }
        if self.child_windows && sck_map::at_least(self.os, sck_map::CHILD_WINDOWS) {
            // SAFETY: a setter that exists from 14.2, checked above.
            unsafe { config.setIncludeChildWindows(true) };
        }
        if let Some(audio) = self.audio
            && sck_map::at_least(self.os, sck_map::AUDIO)
        {
            // SAFETY: a setter that exists from 13.0, checked above.
            unsafe { config.setCapturesAudio(true) };
            // SAFETY: as above.
            unsafe { config.setSampleRate(isize::try_from(audio.rate).unwrap_or(48_000)) };
            // SAFETY: as above.
            unsafe { config.setChannelCount(isize::from(audio.channels)) };
            // SAFETY: as above.
            unsafe { config.setExcludesCurrentProcessAudio(audio.exclude_self) };
        }
        config
    }
}

/// The output and delegate object's state.
struct OutputIvars {
    sink: Arc<dyn Sink>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class has no
    // `Drop` of its own (its ivars drop normally).
    #[unsafe(super(NSObject))]
    #[name = "IstmoScreenCaptureStreamOutput"]
    #[ivars = OutputIvars]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            let kind = match kind {
                SCStreamOutputType::Screen => Kind::Screen,
                SCStreamOutputType::Audio => Kind::Audio,
                // Microphone samples are not asked for.
                _ => return,
            };
            let sample = Sample::retain(sample);
            // Never unwind into the framework's queue.
            let delivered =
                catch_unwind(AssertUnwindSafe(|| self.ivars().sink.sample(kind, sample)));
            if delivered.is_err() {
                tracing::error!("screen-capture: a ScreenCaptureKit sample handler panicked");
            }
        }
    }

    unsafe impl SCStreamDelegate for StreamOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            let error = error_of(error, "stream");
            let reported = catch_unwind(AssertUnwindSafe(|| self.ivars().sink.stopped(error)));
            if reported.is_err() {
                tracing::error!("screen-capture: a ScreenCaptureKit stop handler panicked");
            }
        }
    }
);

impl StreamOutput {
    fn new(sink: Arc<dyn Sink>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { sink });
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// A completion handler that reports an optional `NSError`.
type DoneBlock = RcBlock<dyn Fn(*mut NSError)>;

/// A completion handler, and where its answer arrives.
fn done_block(operation: &'static str) -> (DoneBlock, flume::Receiver<Result<(), CaptureError>>) {
    let (tx, rx) = flume::bounded(1);
    let block = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: the framework passes null or a valid error for the
        // duration of the call.
        let error = unsafe { error.as_ref() };
        let answer = error.map_or(Ok(()), |error| Err(error_of(error, operation)));
        let _sent = tx.try_send(answer);
    });
    (block, rx)
}

/// A running capture. Dropping it stops the stream (waiting a bounded
/// time) and drops the output object; samples already handed out stay
/// valid.
pub(crate) struct Stream {
    capture: Shared<SCStream>,
    output: Shared<StreamOutput>,
    screen: bool,
    audio: bool,
    _queue: DispatchRetained<DispatchQueue>,
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("screen", &self.screen)
            .field("audio", &self.audio)
            .finish_non_exhaustive()
    }
}

impl Stream {
    /// Start capturing `filter` with `settings`, delivering screen and / or
    /// audio samples into `sink`. Returns once the capture runs.
    ///
    /// # Errors
    /// The framework refused the configuration or the start (no grant:
    /// `ConsentDenied`).
    pub(crate) fn start(
        filter: &Filter,
        settings: &Settings,
        screen: bool,
        audio: bool,
        sink: Arc<dyn Sink>,
    ) -> Result<Self, CaptureError> {
        let config = settings.build();
        let output = StreamOutput::new(sink);
        let delegate: &ProtocolObject<dyn SCStreamDelegate> = ProtocolObject::from_ref(&*output);
        // SAFETY: a designated initialiser with a valid filter,
        // configuration and delegate (held by `Self` for the stream's
        // life: SCStream keeps its delegate weakly).
        let capture = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                filter.raw(),
                &config,
                Some(delegate),
            )
        };
        let queue = DispatchQueue::new("dev.istmo.screen-capture.stream", None);
        let as_output: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
        let outputs = [
            (screen, SCStreamOutputType::Screen),
            (audio, SCStreamOutputType::Audio),
        ];
        for (wanted, kind) in outputs {
            if wanted {
                // SAFETY: the output object and the queue outlive the
                // stream (both are held by `Self`).
                unsafe {
                    capture.addStreamOutput_type_sampleHandlerQueue_error(
                        as_output,
                        kind,
                        Some(&queue),
                    )
                }
                .map_err(|error| error_of(&error, "add stream output"))?;
            }
        }
        let (block, rx) = done_block("start");
        // SAFETY: a valid completion handler; the framework copies it.
        unsafe { capture.startCaptureWithCompletionHandler(Some(&block)) };
        wait(&rx, CALL_TIMEOUT, "start")?;
        Ok(Self {
            capture: Shared(capture),
            output: Shared(output),
            screen,
            audio,
            _queue: queue,
        })
    }

    /// Apply new settings to the running stream.
    ///
    /// # Errors
    /// The framework refused them; the stream keeps the old ones.
    pub(crate) fn update(&self, settings: &Settings) -> Result<(), CaptureError> {
        let config = settings.build();
        let (block, rx) = done_block("update");
        // SAFETY: a valid configuration and completion handler.
        unsafe {
            self.capture
                .updateConfiguration_completionHandler(&config, Some(&block));
        }
        wait(&rx, CALL_TIMEOUT, "update")
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let (block, rx) = done_block("stop");
        // SAFETY: a valid completion handler on a started stream.
        unsafe { self.capture.stopCaptureWithCompletionHandler(Some(&block)) };
        if let Err(err) = wait(&rx, CALL_TIMEOUT, "stop") {
            // Stopping a stream the system already stopped reports an error.
            tracing::debug!(%err, "screen-capture: stopping the stream");
        }
        let output: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*self.output.0);
        let outputs = [
            (self.screen, SCStreamOutputType::Screen),
            (self.audio, SCStreamOutputType::Audio),
        ];
        for (added, kind) in outputs {
            if added {
                // SAFETY: removes an output this value added.
                let _removed = unsafe { self.capture.removeStreamOutput_type_error(output, kind) };
            }
        }
    }
}

/// The answer of a one-shot capture's completion handler.
fn shot_answer(sample: *mut CMSampleBuffer, error: *mut NSError) -> Result<Sample, CaptureError> {
    if let Some(sample) = NonNull::new(sample) {
        // SAFETY: a valid sample buffer for the call's duration;
        // `Sample::retain` takes its own reference.
        return Ok(Sample::retain(unsafe { sample.as_ref() }));
    }
    // SAFETY: null or a valid error for the call's duration.
    let error = unsafe { error.as_ref() };
    Err(error.map_or_else(
        || CaptureError::protocol(ID, "screenshot", "no sample and no error"),
        |error| error_of(error, "screenshot"),
    ))
}

/// One frame of `filter` through `SCScreenshotManager` (macOS 14+).
///
/// # Errors
/// The framework refused or did not answer.
pub(crate) fn screenshot(filter: &Filter, settings: &Settings) -> Result<Sample, CaptureError> {
    let config = settings.build();
    let (tx, rx) = flume::bounded(1);
    let block = RcBlock::new(move |sample: *mut CMSampleBuffer, error: *mut NSError| {
        let _sent = tx.try_send(shot_answer(sample, error));
    });
    // SAFETY: a valid filter, configuration and completion handler; the
    // caller checked the OS has `SCScreenshotManager` (14.0).
    unsafe {
        SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
            filter.raw(),
            &config,
            Some(&block),
        );
    }
    wait(&rx, CALL_TIMEOUT, "screenshot")
}
