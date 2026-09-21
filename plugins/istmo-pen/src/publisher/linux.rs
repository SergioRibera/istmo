//! Linux integration surface.
//!
//! Unlike Windows (`WM_POINTER*`) and macOS (`NSEvent` local monitor),
//! Linux has no first-party per-window stylus event stream a plugin can
//! subscribe to. Two paths are viable, with different trade-offs:
//!
//! - **Compositor protocol (recommended for standard apps).** Wayland
//!   apps hook `zwp_tablet_manager_v2` on the display; X11 apps use
//!   `XInput2` tablet devices. Either way the app decodes tool axes,
//!   builds a [`crate::PenEvent`], and forwards it via
//!   [`super::PenPublisher::push_event`]. This is the correct path for
//!   any Wayland or Xorg client (Freya, egui via winit, GTK, Qt, …).
//!
//! - **Raw `libinput` seat (kiosks, compositors).** Apps that run with
//!   a `systemd-logind` seat or direct `/dev/input/*` access can call
//!   [`super::PenPublisher::install_libinput`], which spawns a
//!   background thread polling a process-wide `libinput` context on
//!   `seat0` and broadcasts every tablet-tool sample to registered
//!   [`super::WindowState`]s. Not viable for regular Wayland/X11
//!   client apps because they lack the necessary capabilities and do
//!   not receive routed events from the compositor.
//!
//! The [`input`](https://docs.rs/input) + [`libc`](https://docs.rs/libc)
//! crates are linked unconditionally on Linux — the same pattern as
//! `windows-sys` on Windows and `objc2` on macOS. Consumers who don't
//! call `install_libinput` still get the escape hatch below without
//! paying anything at runtime; only the seat setup itself costs a
//! thread + open FDs, and that only happens on explicit opt-in.
//!
//! In both cases the sample decode logic is the same as the built-in
//! Windows and macOS publishers — pressure/tilt/rotation normalized,
//! timestamps monotonic microseconds since attach. See
//! [`super::PenPublisher::push_event`] and
//! [`super::PenPublisher::push_hover`] for the escape hatch used by
//! compositor integrations.

pub(super) use libinput_backend::install_libinput;

mod libinput_backend {
    use std::fs::OpenOptions;
    use std::os::fd::{IntoRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock, Weak};
    use std::time::{Duration, Instant};

    use flume::{Receiver, Sender};
    use input::event::Event as LibinputEvent;
    use input::event::tablet_tool::{
        ProximityState, TabletToolEvent, TabletToolEventTrait, TabletToolType, TipState,
    };
    use input::{Libinput, LibinputInterface};

    use crate::publisher::PenPublisher;
    use crate::{PenEvent, PenHoverEvent, PenMove, PenSample, PenToolKind};

    /// Attempt to install a `libinput`-backed sample source on the
    /// publisher. Spawns one background thread per process; repeated
    /// calls return the previously recorded outcome — the seat setup
    /// happens exactly once.
    ///
    /// # Errors
    /// Returns an error string when the `libinput` context could not
    /// be constructed or the seat assignment failed (missing logind,
    /// insufficient permissions, no udev, …).
    pub(crate) fn install_libinput(publisher: &Arc<PenPublisher>) -> Result<(), String> {
        static INSTALLED: OnceLock<Mutex<Result<(), String>>> = OnceLock::new();
        let slot = INSTALLED.get_or_init(|| Mutex::new(spawn(Arc::downgrade(publisher))));
        match slot.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    // `Libinput` holds an `Rc<dyn LibinputInterface>` and therefore is
    // not `Send`. The pump thread constructs the context on entry so
    // ownership never leaves the thread.
    fn spawn(publisher: Weak<PenPublisher>) -> Result<(), String> {
        let (ready_tx, ready_rx) = flume::bounded::<Result<(), String>>(1);
        std::thread::Builder::new()
            .name("istmo-pen-libinput".to_owned())
            .spawn(move || run_pump(publisher, ready_tx))
            .map_err(|err| format!("failed to spawn libinput pump thread: {err}"))?;
        ready_rx
            .recv()
            .map_err(|_| "libinput pump thread exited before initialisation".to_owned())?
    }

    fn run_pump(publisher: Weak<PenPublisher>, ready: Sender<Result<(), String>>) {
        let mut context = Libinput::new_with_udev(SeatInterface);
        if let Err(err) = context.udev_assign_seat("seat0") {
            let msg = format!("libinput: udev_assign_seat(\"seat0\") failed: {err:?}");
            let _ = ready.send(Err(msg));
            return;
        }
        let _ = ready.send(Ok(()));
        drop(ready);

        let clock = Arc::new(Clock::new());
        drain_pump(&mut context, &publisher, &clock);
    }

    fn drain_pump(context: &mut Libinput, publisher: &Weak<PenPublisher>, clock: &Arc<Clock>) {
        loop {
            if context.dispatch().is_err() {
                std::thread::sleep(Duration::from_millis(8));
                continue;
            }
            while let Some(event) = context.next() {
                let Some(publisher) = publisher.upgrade() else {
                    return;
                };
                if let LibinputEvent::Tablet(tablet) = event {
                    handle_tablet(tablet, &publisher, clock);
                }
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    fn handle_tablet(event: TabletToolEvent, publisher: &Arc<PenPublisher>, clock: &Arc<Clock>) {
        if !is_stylus(&event) {
            return;
        }
        match event {
            TabletToolEvent::Axis(axis) => {
                let sample = decode_axis(&axis, clock);
                broadcast_event(publisher, PenEvent::Move(PenMove {
                    sample,
                    coalesced: Vec::new(),
                    predicted: Vec::new(),
                }));
            }
            TabletToolEvent::Proximity(prox) => {
                let sample = decode_axis(&prox, clock);
                match prox.proximity_state() {
                    ProximityState::In => {
                        broadcast_hover(publisher, PenHoverEvent::ProximityEnter(sample));
                    }
                    ProximityState::Out => {
                        broadcast_hover(publisher, PenHoverEvent::ProximityLeave);
                    }
                }
            }
            TabletToolEvent::Tip(tip) => {
                let sample = decode_axis(&tip, clock);
                let ev = match tip.tip_state() {
                    TipState::Down => PenEvent::Down(sample),
                    TipState::Up => PenEvent::Up(sample),
                };
                broadcast_event(publisher, ev);
            }
            _ => {
                // Button events are folded into the sample's `buttons`
                // bitmap by the follow-up Axis event; nothing to emit
                // on their own.
            }
        }
    }

    fn is_stylus(event: &TabletToolEvent) -> bool {
        let tool = match event {
            TabletToolEvent::Axis(e) => e.tool(),
            TabletToolEvent::Proximity(e) => e.tool(),
            TabletToolEvent::Tip(e) => e.tool(),
            TabletToolEvent::Button(e) => e.tool(),
            _ => return false,
        };
        matches!(
            tool.tool_type(),
            Some(TabletToolType::Pen | TabletToolType::Pencil | TabletToolType::Eraser)
        )
    }

    fn broadcast_event(publisher: &PenPublisher, event: PenEvent) {
        for id in publisher.window_ids() {
            publisher.push_event(id, event.clone());
        }
    }

    fn broadcast_hover(publisher: &PenPublisher, event: PenHoverEvent) {
        for id in publisher.window_ids() {
            publisher.push_hover(id, event.clone());
        }
    }

    fn decode_axis<T: TabletToolEventTrait>(event: &T, clock: &Arc<Clock>) -> PenSample {
        let tool = event.tool();
        let tool_kind = match tool.tool_type() {
            Some(TabletToolType::Eraser) => PenToolKind::Eraser,
            Some(TabletToolType::Pen | TabletToolType::Pencil) => PenToolKind::Tip,
            _ => PenToolKind::Unknown,
        };
        // libinput reports absolute tablet coordinates in millimetres;
        // consumers scale into client space. `x` / `y` may fall outside
        // the window on multi-monitor setups — that's a routing problem
        // consumers solve, not a decoding problem here.
        let x = event.x() as f32;
        let y = event.y() as f32;
        let pressure = if tool.has_pressure() {
            event.pressure() as f32
        } else {
            0.0
        };
        let tilt_x = if tool.has_tilt() {
            (event.tilt_x() as f32).to_radians()
        } else {
            0.0
        };
        let tilt_y = if tool.has_tilt() {
            (event.tilt_y() as f32).to_radians()
        } else {
            0.0
        };
        let twist = if tool.has_rotation() {
            (event.rotation() as f32).to_radians()
        } else {
            0.0
        };
        let z_offset = if tool.has_distance() {
            event.distance() as f32
        } else {
            0.0
        };
        let time_us = event.time_usec();
        let attach_us = clock.attach_us();
        let timestamp_us = time_us.saturating_sub(attach_us);
        let tool_id = (tool.serial() & 0xFFFF_FFFF) as u32;
        PenSample {
            x,
            y,
            pressure,
            tilt_x,
            tilt_y,
            azimuth: 0.0,
            altitude: 0.0,
            twist,
            tangential_pressure: 0.0,
            z_offset,
            timestamp_us,
            sequence: clock.next_sequence(),
            tool_id,
            tool_kind,
            buttons: 0,
        }
    }

    #[derive(Debug)]
    struct Clock {
        sequence: AtomicU32,
        start: Instant,
        attach_us: AtomicU64,
    }

    impl Clock {
        fn new() -> Self {
            Self {
                sequence: AtomicU32::new(0),
                start: Instant::now(),
                attach_us: AtomicU64::new(0),
            }
        }

        fn next_sequence(&self) -> u32 {
            self.sequence.fetch_add(1, Ordering::Relaxed)
        }

        fn attach_us(&self) -> u64 {
            let cached = self.attach_us.load(Ordering::Relaxed);
            if cached != 0 {
                return cached;
            }
            let now = self.start.elapsed().as_micros() as u64;
            self.attach_us.store(now, Ordering::Relaxed);
            now
        }
    }

    struct SeatInterface;

    impl LibinputInterface for SeatInterface {
        fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
            OpenOptions::new()
                .read((flags & libc::O_ACCMODE) == libc::O_RDONLY
                    || (flags & libc::O_ACCMODE) == libc::O_RDWR)
                .write((flags & libc::O_ACCMODE) == libc::O_WRONLY
                    || (flags & libc::O_ACCMODE) == libc::O_RDWR)
                .custom_flags(flags)
                .open(path)
                .map(OwnedFd::from)
                .map_err(|err| err.raw_os_error().unwrap_or(libc::EIO))
        }

        fn close_restricted(&mut self, fd: OwnedFd) {
            // Drop the descriptor deliberately — libinput no longer
            // wants it. `into_raw_fd` releases ownership so the OS
            // returns the fd on drop of the underlying number.
            let _ = fd.into_raw_fd();
        }
    }

    // The `Receiver` alias silences an otherwise-unused import warning
    // when the pump path optimises to zero events.
    #[allow(dead_code)]
    type _EnsureReceiver = Receiver<()>;
}
