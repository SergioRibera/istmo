//! Audio-only sessions against the live PipeWire session daemon, through
//! the public API (no dialog, no video backend involved). Runs only with
//! `ISTMO_SC_TEST_PIPEWIRE` set.

#![cfg(all(target_os = "linux", feature = "linux-audio"))]

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use istmo_core::{Overflow, SinkPolicy, StreamItem};
use istmo_screen_capture::{
    AudioConfig, AudioEvent, AudioSource, AudioUpdate, Availability, CapabilityQuery, SampleFormat,
    ScreenCaptureClientExt, StopReason,
};

const GATE: &str = "ISTMO_SC_TEST_PIPEWIRE";

fn now_us() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_micros(),
    )
    .expect("fits")
}

/// Three seconds of the default sink's monitor at `rate`: the requested
/// shape, a sample-count clock on the wall clock, contiguous chunks.
fn capture_at(rate: u32) {
    let live = common::live();
    let config = AudioConfig::of(AudioSource::System)
        .rate(rate)
        .channels(2)
        .format(SampleFormat::S16);
    let session = common::block(live.client.open_audio_session(
        &live.runtime,
        config,
        SinkPolicy::queue(512).with_overflow(Overflow::DropNewest),
    ))
    .expect("open an audio-only session");
    let negotiated = session.negotiated().clone();
    eprintln!("{rate} Hz: negotiated {negotiated:?}");
    assert_eq!(
        negotiated.rate, rate,
        "PipeWire resamples to the asked rate"
    );
    assert_eq!(negotiated.channels, 2);
    assert_eq!(negotiated.format, SampleFormat::S16);

    let started = Instant::now();
    let mut chunks = Vec::new();
    while started.elapsed() < Duration::from_secs(3) {
        match session.audio().try_recv() {
            Some(chunk) => chunks.push(chunk),
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let frames: u64 = chunks.iter().map(|c| u64::from(c.meta.frames)).sum();
    eprintln!("{rate} Hz: {} chunks, {frames} frames", chunks.len());
    assert!(
        !chunks.is_empty(),
        "the monitor delivers (silence included)"
    );
    let rate64 = u64::from(rate);
    assert!(
        frames > rate64 * 2 && frames < rate64 * 4,
        "about three seconds of samples: {frames}"
    );
    let last = chunks.last().expect("chunks");
    assert!(
        now_us().abs_diff(last.meta.wall_us) < 500_000,
        "wall_us is UNIX-epoch µs, near now"
    );
    for pair in chunks.windows(2) {
        let (a, b) = (&pair[0].meta, &pair[1].meta);
        assert_eq!(b.seq, a.seq + 1, "no chunk dropped");
        assert_eq!(b.rate, rate);
        assert!(b.wall_us >= a.wall_us);
        if !b.discontinuity {
            // The sample count, not the arrival time, spaces the chunks.
            let expected = u64::from(a.frames) * 1_000_000 / rate64;
            assert!(
                (b.wall_us - a.wall_us).abs_diff(expected) <= 1,
                "chunk spacing {} vs {expected} µs",
                b.wall_us - a.wall_us
            );
        }
        let bytes = pair[1].buffer.as_bytes().map_or(0, <[u8]>::len);
        assert_eq!(bytes, b.frames as usize * 2 * 2, "interleaved S16 stereo");
    }

    // Mute keeps the timing and zeroes the samples.
    let events = session.events().expect("events");
    common::block(session.update(AudioUpdate::Mute)).expect("mute");
    std::thread::sleep(Duration::from_millis(100));
    while session.audio().try_recv().is_some() {}
    let muted = common::wait_until("a muted chunk", Duration::from_secs(2), || {
        session.audio().try_recv().is_some_and(|c| {
            c.buffer
                .as_bytes()
                .is_some_and(|b| b.iter().all(|v| *v == 0))
        })
    });
    assert!(muted);
    common::block(session.close()).expect("close");
    let mut seen = Vec::new();
    while let Ok(StreamItem::Event(event)) = events.recv() {
        seen.push(event);
    }
    assert!(
        matches!(seen.first(), Some(AudioEvent::Started(_))),
        "{seen:?}"
    );
    assert!(seen.contains(&AudioEvent::Muted));
    assert_eq!(
        seen.last(),
        Some(&AudioEvent::Stopped(StopReason::Closed)),
        "{seen:?}"
    );
}

#[test]
fn audio_only_at_44100_for_aac_eld() {
    if !common::enabled(GATE) {
        return;
    }
    capture_at(44_100);
}

#[test]
fn audio_only_at_48000() {
    if !common::enabled(GATE) {
        return;
    }
    capture_at(48_000);
}

#[test]
fn the_report_says_audio_runs_here() {
    if !common::enabled(GATE) {
        return;
    }
    let live = common::live();
    let report =
        common::block(live.client.capabilities(CapabilityQuery::default())).expect("capabilities");
    let audio = report
        .audio
        .expect("the PipeWire audio backend is compiled");
    assert_eq!(audio.availability, Availability::Ready);
}
