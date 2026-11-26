//! One live X11 session: a capture thread on its own connection,
//! driven by XDamage (or polling at the session rate without it).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::xproto;

use crate::pool::{HoldError, PoolAccounting};
use crate::select;
use crate::types::{
    CaptureError, CaptureEvent, CopyStage, CursorMode, Damage, Exhaustion, FrameRate,
    NegotiatedStream, Rect, SessionUpdate, Size, Source, SourceKind,
};

use super::super::super::backend::{BackendSession, StreamPlan};
use super::super::super::clock;
use super::super::super::output::{Delivery, FrameOut};
use super::conn::{ID, Target, X11, x_err};
use super::grab::Grabber;
use super::{CursorPlan, capture, cursor_modes, meta_for, negotiated};

/// Longest a capture thread sleeps before it looks at its stop flag.
const IDLE_WAIT: Duration = Duration::from_millis(100);
/// How often a window's visibility is re-read (one round trip).
const VISIBILITY_CHECK: Duration = Duration::from_millis(250);
/// How often monitors are re-read to notice hot-plug and resizes.
const MONITOR_CHECK: Duration = Duration::from_secs(1);
/// More damage rectangles than this become a full-frame damage.
const MAX_RECTS: usize = Damage::MAX_RECTS;
/// How long dropping a session waits for its thread.
const JOIN_WAIT: Duration = Duration::from_secs(2);

/// Damage collected since the last delivered frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Pending {
    full: bool,
    rects: Vec<Rect>,
}

impl Pending {
    const fn full() -> Self {
        Self {
            full: true,
            rects: Vec::new(),
        }
    }

    const fn is_empty(&self) -> bool {
        !self.full && self.rects.is_empty()
    }

    /// Add a rectangle given in drawable coordinates, keeping what lies
    /// inside `area` (made relative to it).
    fn add(&mut self, rect: Rect, area: Rect) {
        if self.full {
            return;
        }
        if let Some(inside) = rect.intersect(area) {
            self.rects.push(inside.relative_to(area.x, area.y));
            if self.rects.len() > MAX_RECTS {
                *self = Self::full();
            }
        }
    }

    fn take(&mut self) -> Damage {
        let pending = std::mem::take(self);
        if pending.full {
            Damage::Full
        } else {
            Damage::Rects(pending.rects)
        }
    }
}

/// What the capture thread shares with the session handle.
#[derive(Debug)]
struct Shared {
    stop: AtomicBool,
    cursor: Mutex<CursorMode>,
    negotiated: Mutex<NegotiatedStream>,
}

pub(super) struct Session {
    shared: Arc<Shared>,
    available_cursor: Vec<CursorMode>,
    done: flume::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

/// Open a session on `plan.source`: the first read happens here so the
/// negotiated stream knows its size and format.
pub(super) fn start(
    x: X11,
    plan: &StreamPlan,
    out: FrameOut,
    tier: u8,
    cursor: CursorMode,
) -> Result<(Session, NegotiatedStream), CaptureError> {
    let mut grabber = Grabber::default();
    let source = plan.source.clone();
    let (probe, _) = capture(&x, &mut grabber, &source, CursorPlan::Hidden, None)?;
    let depth = usize::from(plan.pool_depth.max(2));
    let negotiated = negotiated(
        &x,
        &source,
        &probe,
        cursor,
        tier,
        plan.pool_depth.max(2),
        plan.exhaustion,
    );
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        cursor: Mutex::new(cursor),
        negotiated: Mutex::new(negotiated.clone()),
    });
    let available_cursor = cursor_modes(&x);
    let (done_tx, done) = flume::bounded(1);
    let worker = Worker {
        x,
        grabber,
        source,
        out,
        shared: Arc::clone(&shared),
        pool: PoolAccounting::new(depth),
        pool_depth: plan.pool_depth.max(2),
        exhaustion: plan.exhaustion,
        tier,
        size: probe.size,
    };
    let thread = std::thread::Builder::new()
        .name("istmo-screen-capture-x11".to_owned())
        .spawn(move || {
            worker.run();
            let _finished = done_tx.send(());
        })
        .map_err(|e| x_err("spawn capture thread", e))?;
    Ok((
        Session {
            shared,
            available_cursor,
            done,
            thread: Some(thread),
        },
        negotiated,
    ))
}

impl BackendSession for Session {
    fn update(&mut self, update: &SessionUpdate) -> Result<NegotiatedStream, CaptureError> {
        let mut negotiated = self
            .shared
            .negotiated
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match update {
            SessionUpdate::Cursor(request) => {
                let mode = select::cursor_mode(*request, &self.available_cursor);
                if request.strength == crate::types::Strength::Require && mode != request.mode {
                    return Err(CaptureError::unsatisfiable(
                        Some(ID),
                        crate::types::Requirement::Cursor,
                    ));
                }
                *self
                    .shared
                    .cursor
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = mode;
                negotiated.cursor = mode;
            }
            SessionUpdate::Formats(formats) => {
                let offer = select::Offer {
                    format: negotiated.format,
                    memory: negotiated.memory,
                    modifiers: Vec::new(),
                    copies: vec![CopyStage::ShmTransport],
                };
                let chosen = select::negotiate(formats, &[offer], 0).ok_or_else(|| {
                    CaptureError::unsatisfiable(Some(ID), crate::types::Requirement::Formats)
                })?;
                negotiated.tier = chosen.tier;
            }
            SessionUpdate::Fps(_)
            | SessionUpdate::Region(_)
            | SessionUpdate::Pause
            | SessionUpdate::Resume => {
                return Err(CaptureError::unsatisfiable(
                    Some(ID),
                    crate::types::Requirement::Operation,
                ));
            }
        }
        Ok(negotiated.clone())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if self.done.recv_timeout(JOIN_WAIT).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _joined = thread.join();
            }
        } else {
            tracing::warn!("x11 capture thread did not stop in time; detached");
        }
    }
}

struct Worker {
    x: X11,
    grabber: Grabber,
    source: Source,
    out: FrameOut,
    shared: Arc<Shared>,
    pool: PoolAccounting,
    pool_depth: u8,
    exhaustion: Exhaustion,
    tier: u8,
    size: Size,
}

/// How the source looks right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Visible,
    Hidden,
}

impl Worker {
    fn period(&self) -> Duration {
        match self.out.fps() {
            // The monitor's refresh (60 Hz when unknown).
            FrameRate::Native => {
                Duration::from_secs(1000)
                    / self.source.refresh_mhz.filter(|r| *r > 0).unwrap_or(60_000)
            }
            FrameRate::Max(n) | FrameRate::Exact(n) | FrameRate::OnDamage(n) => {
                Duration::from_secs(1) / n.max(1)
            }
        }
    }

    fn drawable(&self) -> Result<xproto::Drawable, CaptureError> {
        Ok(match X11::target(&self.source)? {
            Target::Monitor(_) => self.x.root,
            Target::Window(window) => window,
        })
    }

    /// The rectangle of the damage drawable the source covers.
    fn damage_area(&self) -> Rect {
        match X11::target(&self.source) {
            Ok(Target::Monitor(rect)) => rect,
            _ => Rect::from_size(self.size),
        }
    }

    fn create_damage(&self) -> Option<damage::Damage> {
        if !self.x.ext.damage {
            return None;
        }
        let drawable = self.drawable().ok()?;
        let id = self.x.conn.generate_id().ok()?;
        self.x
            .conn
            .damage_create(id, drawable, damage::ReportLevel::RAW_RECTANGLES)
            .ok()?
            .check()
            .ok()?;
        Some(id)
    }

    fn drain_events(&self, pending: &mut Pending) {
        let area = self.damage_area();
        while let Ok(Some(event)) = self.x.conn.poll_for_event() {
            if let Event::DamageNotify(notify) = event {
                let rect = Rect::new(
                    i32::from(notify.area.x),
                    i32::from(notify.area.y),
                    u32::from(notify.area.width),
                    u32::from(notify.area.height),
                );
                pending.add(rect, area);
            }
        }
    }

    /// Monitors only: notice a monitor that went away or changed size.
    fn check_monitor(&mut self) -> Result<(), CaptureError> {
        if self.source.kind != SourceKind::Monitor {
            return Ok(());
        }
        let monitors = self.x.monitors()?;
        let Some(now) = monitors.into_iter().find(|m| m.id == self.source.id) else {
            return Err(CaptureError::SourceLost);
        };
        if now.geometry != self.source.geometry {
            self.source = now;
        }
        Ok(())
    }

    /// [`window_visibility`](Self::window_visibility) when `check`, else
    /// the last known one (no round trip).
    fn window_visibility_if(
        &self,
        check: bool,
        last: Visibility,
    ) -> Result<Visibility, CaptureError> {
        if check {
            self.window_visibility()
        } else {
            Ok(last)
        }
    }

    /// Sleep until the server sends something (damage), at most
    /// `timeout`.
    fn wait_for_events(&self, timeout: Duration) {
        use std::os::fd::AsFd as _;
        let fd = self.x.conn.stream().as_fd();
        let mut fds = [rustix::event::PollFd::new(
            &fd,
            rustix::event::PollFlags::IN,
        )];
        let timeout = rustix::event::Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        // An interrupted or failed wait only means looking again sooner.
        let _ready = rustix::event::poll(&mut fds, Some(&timeout));
    }

    /// Windows only: unmapped windows give no frames; destroyed ones end
    /// the session.
    fn window_visibility(&self) -> Result<Visibility, CaptureError> {
        match X11::target(&self.source)? {
            Target::Monitor(_) => Ok(Visibility::Visible),
            Target::Window(window) => Ok(if self.x.viewable(window)? {
                Visibility::Visible
            } else {
                Visibility::Hidden
            }),
        }
    }

    fn run(mut self) {
        let damage = self.create_damage();
        let mut pending = Pending::full();
        let mut visibility = Visibility::Visible;
        let mut last_capture: Option<Instant> = None;
        let mut last_monitor_check = Instant::now();
        let mut last_visibility_check: Option<Instant> = None;
        let mut skipped: u32 = 0;
        loop {
            if self.shared.stop.load(Ordering::SeqCst) || self.out.is_stopped() {
                break;
            }
            self.drain_events(&mut pending);
            if last_monitor_check.elapsed() >= MONITOR_CHECK {
                last_monitor_check = Instant::now();
                if let Err(err) = self.check_monitor() {
                    self.out.fail(err);
                    break;
                }
            }
            let check_visibility =
                last_visibility_check.is_none_or(|at| at.elapsed() >= VISIBILITY_CHECK);
            if check_visibility {
                last_visibility_check = Some(Instant::now());
            }
            match self.window_visibility_if(check_visibility, visibility) {
                Ok(now) if now != visibility => {
                    visibility = now;
                    self.out.event(match now {
                        Visibility::Visible => CaptureEvent::SourceVisible,
                        Visibility::Hidden => CaptureEvent::SourceHidden,
                    });
                    if now == Visibility::Visible {
                        pending = Pending::full();
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    self.out.fail(err);
                    break;
                }
            }
            let until_due = last_capture.map_or(Duration::ZERO, |at| {
                self.period().saturating_sub(at.elapsed())
            });
            let wanted = damage.is_none() || !pending.is_empty();
            if !self.out.wants_frames() {
                self.out.wait_while_paused(IDLE_WAIT);
                continue;
            }
            if visibility == Visibility::Hidden {
                self.wait_for_events(VISIBILITY_CHECK);
                continue;
            }
            if !until_due.is_zero() {
                std::thread::sleep(until_due.min(IDLE_WAIT));
                continue;
            }
            if !wanted {
                // Nothing changed: sleep until the server reports damage.
                self.wait_for_events(IDLE_WAIT);
                continue;
            }
            last_capture = Some(Instant::now());
            match self.capture_one(&mut pending, &mut skipped) {
                Ok(Delivery::Closed) => break,
                Ok(_) => {}
                Err(CaptureError::SourceLost) if self.source.kind == SourceKind::Window => {
                    // Unmapped between the check and the read: next tick
                    // tells hidden from destroyed.
                    last_visibility_check = None;
                }
                Err(err) => {
                    self.out.fail(err);
                    break;
                }
            }
        }
        if let Some(damage) = damage {
            let _destroyed = self.x.conn.damage_destroy(damage);
        }
        self.grabber.release(&self.x);
        let _flushed = self.x.conn.flush();
    }

    fn capture_one(
        &mut self,
        pending: &mut Pending,
        skipped: &mut u32,
    ) -> Result<Delivery, CaptureError> {
        let slot = (0..u32::from(self.pool_depth))
            .find(|s| !self.pool.is_held(*s))
            .unwrap_or(0);
        let lease = match self.pool.hold(slot) {
            Ok(lease) => Some(lease),
            Err(err @ HoldError::SlotOutOfRange { .. }) => {
                return Err(CaptureError::protocol(ID, "pool", err));
            }
            Err(HoldError::Exhausted(exhausted)) => {
                self.out
                    .exhausted(exhausted, self.pool_depth, self.exhaustion);
                *skipped = skipped.saturating_add(1);
                match self.exhaustion {
                    Exhaustion::Stall => {
                        // Keep the damage; pull again once a buffer returns.
                        let _woken = self.pool.released().recv_timeout(self.period());
                        return Ok(Delivery::Dropped);
                    }
                    Exhaustion::DropNewest => {
                        pending.take();
                        return Ok(Delivery::Dropped);
                    }
                    Exhaustion::CopyToCpu => None,
                }
            }
        };
        let cursor = *self
            .shared
            .cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let pts = clock::monotonic_ns();
        // A lease-free read-back (CopyToCpu) reads over the socket: owned
        // bytes that never pin a segment.
        let slot = lease.as_ref().map(|l| l.identity().slot);
        let (grab, state) = capture(
            &self.x,
            &mut self.grabber,
            &self.source,
            cursor.into(),
            slot,
        )?;
        if grab.size != self.size {
            self.size = grab.size;
            let generation = self.pool.reallocate(usize::from(self.pool_depth));
            tracing::debug!(generation, "x11 source resized");
            self.grabber.reset_segments(&self.x);
            self.out.event(CaptureEvent::SourceResized(grab.size));
            *pending = Pending::full();
            // The lease belonged to the old generation; take a fresh one.
            return Ok(Delivery::Dropped);
        }
        let mut meta = meta_for(&self.source, &grab, pts, pending.take(), state);
        meta.tier = self.tier;
        meta.skipped_before = std::mem::take(skipped);
        let buffer = self
            .grabber
            .buffer(grab)
            .ok_or_else(|| x_err("frame", "the segment went away"))?;
        let delivery = if let Some(lease) = lease {
            meta.buffer = lease.identity();
            let identity = lease.identity().into();
            self.out.deliver(
                meta,
                buffer
                    .with_identity(identity)
                    .with_release(move || drop(lease)),
            )
        } else {
            meta.copied = Some(CopyStage::ExhaustionReadback);
            self.out.deliver(meta, buffer)
        };
        Ok(delivery)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_is_clipped_to_the_source_and_made_relative() {
        let mut pending = Pending::default();
        let monitor = Rect::new(100, 0, 50, 50);
        pending.add(Rect::new(90, 10, 20, 5), monitor);
        pending.add(Rect::new(0, 0, 10, 10), monitor);
        assert_eq!(pending.take(), Damage::Rects(vec![Rect::new(0, 10, 10, 5)]));
        assert!(pending.is_empty());
    }

    #[test]
    fn too_many_rects_become_full_damage() {
        let mut pending = Pending::default();
        for i in 0..=MAX_RECTS {
            pending.add(
                Rect::new(i32::try_from(i).expect("small"), 0, 1, 1),
                Rect::new(0, 0, 100, 100),
            );
        }
        assert_eq!(pending.take(), Damage::Full);
    }
}
