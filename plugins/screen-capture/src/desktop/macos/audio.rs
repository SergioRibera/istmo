//! System / application audio through ScreenCaptureKit (macOS 13+): an
//! `SCStream` of the main display with `capturesAudio`, its video kept
//! to a 2×2 frame a second and dropped. Chunks arrive as planar `f32` on
//! the host time clock; they are interleaved into the asked format.
//! `wall_us` follows the sample count (`audio_clock`, the clock every
//! desktop audio backend shares), anchored at the wall time of the
//! chunk's own timestamp; `pts_ns` stays on host time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::types::{
    AudioCaps, AudioConfig, AudioMeta, AudioNegotiated, AudioSource, Availability, CaptureError,
    OsVersion, PtsClock, Requirement,
};

use super::super::audio_clock::SampleClock;
use super::super::backend::{AudioBackend, BackendSession};
use super::super::clock;
use super::super::output::AudioOut;
use super::super::sck_map::{self, AudioTimeline, FourCc, ID};
use super::ffi::{
    self, AudioChunk, AudioSettings, Content, Exclusions, Kind, Sample, Settings, Sink, Stream,
};

pub(crate) fn backend() -> Arc<dyn AudioBackend> {
    Arc::new(SckAudio {
        os: ffi::os_version(),
    })
}

/// The rate asked for when the caller leaves it open.
const DEFAULT_RATE: u32 = 48_000;
/// The channel count asked for when the caller leaves it open (SCK
/// delivers mono or stereo).
const DEFAULT_CHANNELS: u8 = 2;

#[derive(Debug)]
struct SckAudio {
    os: OsVersion,
}

impl AudioBackend for SckAudio {
    fn availability(&self) -> Availability {
        if !sck_map::at_least(self.os, sck_map::AUDIO) {
            Availability::OsTooOld(sck_map::AUDIO)
        } else if !ffi::screen_recording_granted() {
            Availability::NeedsAccess(crate::types::AccessScope::ScreenRecording)
        } else {
            Availability::Ready
        }
    }

    fn caps(&self) -> AudioCaps {
        sck_map::audio_caps(self.os)
    }

    fn open(
        &self,
        config: &AudioConfig,
        out: AudioOut,
    ) -> Result<(Box<dyn BackendSession>, AudioNegotiated), CaptureError> {
        ffi::pooled(|| {
            if !sck_map::at_least(self.os, sck_map::AUDIO) {
                return Err(CaptureError::OsTooOld(sck_map::AUDIO));
            }
            if !ffi::screen_recording_granted() {
                return Err(CaptureError::ConsentRequired);
            }
            let channels = config.channels.unwrap_or(DEFAULT_CHANNELS);
            if !(1..=2).contains(&channels) {
                return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Audio));
            }
            let rate = config.rate.unwrap_or(DEFAULT_RATE);
            let content = Content::fetch()?;
            let displays = content.displays();
            let main = displays
                .iter()
                .find(|d| d.main)
                .or_else(|| displays.first())
                .ok_or(CaptureError::SourceLost)?;
            let (filter, exclude_self) = match &config.source {
                AudioSource::System => (
                    content.display_filter(main.id, &Exclusions::default()),
                    false,
                ),
                AudioSource::ExcludeSelf => (
                    content.display_filter(main.id, &Exclusions::default()),
                    true,
                ),
                AudioSource::Application(name) => {
                    // A bundle id, or the application's name.
                    let app = content
                        .applications()
                        .into_iter()
                        .find(|a| a.bundle_id == *name || a.name == *name)
                        .ok_or_else(|| {
                            CaptureError::SourceNotFound(crate::types::SourceId::new(
                                ID,
                                name.clone(),
                            ))
                        })?;
                    let bundle = (!app.bundle_id.is_empty()).then_some(app.bundle_id.as_str());
                    (
                        content.application_filter(
                            bundle,
                            Some(app.pid),
                            main.id,
                            &Exclusions::default(),
                        ),
                        false,
                    )
                }
                AudioSource::Node(_) => {
                    return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Audio));
                }
            };
            let filter = filter.ok_or(CaptureError::SourceLost)?;
            let settings = Settings {
                os: self.os,
                width: 2,
                height: 2,
                fourcc: FourCc::BGRA.0,
                interval: (1, 1),
                queue_depth: sck_map::POOL.min_depth,
                shows_cursor: false,
                scales_to_fit: false,
                srgb: false,
                hdr: false,
                child_windows: false,
                audio: Some(AudioSettings {
                    rate,
                    channels,
                    exclude_self,
                }),
            };
            let negotiated = AudioNegotiated {
                source: config.source.clone(),
                rate,
                channels,
                format: config.format,
            };
            let sink = Arc::new(AudioSink {
                out,
                negotiated: negotiated.clone(),
                timeline: Mutex::default(),
                warned: AtomicBool::new(false),
            });
            // The screen output is added too: without one SCK logs every
            // dropped video frame.
            let stream = Stream::start(&filter, &settings, true, true, sink)?;
            Ok((
                Box::new(AudioSession { _stream: stream }) as Box<dyn BackendSession>,
                negotiated,
            ))
        })
    }
}

#[derive(Debug)]
struct AudioSession {
    _stream: Stream,
}

impl BackendSession for AudioSession {}

#[derive(Debug)]
struct AudioSink {
    out: AudioOut,
    negotiated: AudioNegotiated,
    timeline: Mutex<Timeline>,
    /// A rate / channel mismatch was logged once.
    warned: AtomicBool,
}

/// The stream's audio timing: gaps in the chunk timestamps, and the
/// shared sample-count clock for `wall_us`.
#[derive(Debug, Default)]
struct Timeline {
    gaps: AudioTimeline,
    clock: SampleClock,
}

impl AudioSink {
    fn chunk(&self, sample: &Sample) -> Result<(), CaptureError> {
        let chunk: AudioChunk = sample.audio()?;
        let rate = rate_of(chunk.rate).unwrap_or(self.negotiated.rate);
        let channels = u8::try_from(chunk.channels).unwrap_or(self.negotiated.channels);
        if (rate != self.negotiated.rate || channels != self.negotiated.channels)
            && !self.warned.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                asked_rate = self.negotiated.rate,
                rate,
                asked_channels = self.negotiated.channels,
                channels,
                "screen-capture: ScreenCaptureKit delivers audio at another rate or channel count; chunks say what they carry"
            );
        }
        let (pcm, frames) = if chunk.planar {
            let planes: Vec<&[f32]> = chunk.samples.iter().map(Vec::as_slice).collect();
            sck_map::interleave(&planes, self.negotiated.format)
        } else {
            let samples = chunk.samples.first().map_or(&[][..], Vec::as_slice);
            let frames = samples.len() / usize::from(channels.max(1));
            (
                sck_map::convert_interleaved(samples, self.negotiated.format),
                frames,
            )
        };
        if frames == 0 {
            return Ok(());
        }
        let pts_ns = sample
            .pts_ns()
            .or_else(ffi::host_time_ns)
            .unwrap_or_default();
        let frames = u32::try_from(frames).unwrap_or(u32::MAX);
        let (gap, stamp) = {
            let mut timeline = self.timeline.lock().unwrap_or_else(PoisonError::into_inner);
            let gap = timeline.gaps.chunk(pts_ns, u64::from(frames), rate);
            let start_us = clock::wall_us_for(pts_ns, PtsClock::HostTime);
            (gap, timeline.clock.stamp(u64::from(frames), rate, start_us))
        };
        if let Some(drift_us) = stamp.reanchored_by_us {
            tracing::debug!(
                drift_us,
                "screen-capture audio: re-anchored the chunk clock"
            );
        }
        let meta = AudioMeta {
            seq: 0,
            pts_ns,
            wall_us: stamp.wall_us,
            rate,
            channels,
            format: self.negotiated.format,
            frames,
            discontinuity: gap || stamp.reanchored_by_us.is_some(),
        };
        // `false` once the sink is gone: the host ends the session.
        let _open = self.out.deliver(meta, pcm);
        Ok(())
    }
}

fn rate_of(rate: f64) -> Option<u32> {
    if !rate.is_finite() || rate < 1.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "positive, finite and clamped to u32"
    )]
    let rate = rate.round().min(f64::from(u32::MAX)) as u32;
    Some(rate)
}

impl Sink for AudioSink {
    fn sample(&self, kind: Kind, sample: Sample) {
        if kind != Kind::Audio {
            return;
        }
        if let Err(err) = self.chunk(&sample) {
            tracing::debug!(%err, "screen-capture: an audio chunk was dropped");
        }
    }

    fn stopped(&self, error: CaptureError) {
        self.out.fail(error);
    }
}
