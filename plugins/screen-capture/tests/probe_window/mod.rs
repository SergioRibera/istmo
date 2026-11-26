//! A Wayland client showing one small xdg toplevel, for tests that list
//! and watch the compositor's windows. It runs on its own thread; the
//! window goes away when the [`ProbeWindow`] drops.

#![allow(
    unreachable_pub,
    reason = "a test-support module included by a test binary"
)]

use std::os::fd::AsFd as _;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

/// Buffer size of the window.
const SIDE: i32 = 64;

enum Command {
    Title(String),
}

/// One mapped toplevel with a known title and app id.
pub struct ProbeWindow {
    commands: Option<mpsc::Sender<Command>>,
    thread: Option<JoinHandle<()>>,
}

impl ProbeWindow {
    /// Map the window; returns once the compositor has its first buffer.
    ///
    /// # Panics
    /// When the compositor lacks `xdg_wm_base` or does not map it in 5 s.
    pub fn open(title: &str, app_id: &str) -> Self {
        let (commands, inbox) = mpsc::channel();
        let (mapped, ready) = mpsc::channel();
        let (title, app_id) = (title.to_owned(), app_id.to_owned());
        let thread = std::thread::Builder::new()
            .name("probe-window".to_owned())
            .spawn(move || run(&title, &app_id, &inbox, &mapped))
            .expect("spawn the probe window");
        ready
            .recv_timeout(Duration::from_secs(5))
            .expect("the probe window is mapped");
        Self {
            commands: Some(commands),
            thread: Some(thread),
        }
    }

    pub fn set_title(&self, title: &str) {
        if let Some(commands) = &self.commands {
            let _sent = commands.send(Command::Title(title.to_owned()));
        }
    }
}

impl Drop for ProbeWindow {
    fn drop(&mut self) {
        // Hanging up ends the thread's loop, which destroys the window.
        drop(self.commands.take());
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

#[derive(Default)]
struct State {
    configured: bool,
}

fn run(title: &str, app_id: &str, inbox: &mpsc::Receiver<Command>, mapped: &mpsc::Sender<()>) {
    let conn = Connection::connect_to_env().expect("connect to the compositor");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm: XdgWmBase = globals.bind(&qh, 1..=1, ()).expect("xdg_wm_base");
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(title.to_owned());
    toplevel.set_app_id(app_id.to_owned());
    surface.commit();
    let mut state = State::default();
    while !state.configured {
        queue.blocking_dispatch(&mut state).expect("configure");
    }
    // The first buffer maps the toplevel.
    let stride = SIDE * 4;
    let size = stride * SIDE;
    let fd =
        rustix::fs::memfd_create("probe-window", rustix::fs::MemfdFlags::CLOEXEC).expect("memfd");
    rustix::fs::ftruncate(&fd, u64::try_from(size).expect("size")).expect("memfd size");
    let pool = shm.create_pool(fd.as_fd(), size, &qh, ());
    let buffer = pool.create_buffer(0, SIDE, SIDE, stride, wl_shm::Format::Xrgb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    queue.roundtrip(&mut state).expect("map");
    let _sent = mapped.send(());
    loop {
        match inbox.recv_timeout(Duration::from_millis(50)) {
            Ok(Command::Title(title)) => {
                toplevel.set_title(title);
                surface.commit();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if queue.roundtrip(&mut state).is_err() {
            return;
        }
    }
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    buffer.destroy();
    pool.destroy();
    let _flushed = queue.roundtrip(&mut state);
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            state.configured = true;
        }
    }
}

/// Objects whose events the probe does not need.
macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {
        $(
            impl Dispatch<$proxy, ()> for State {
                fn event(
                    _: &mut Self,
                    _: &$proxy,
                    _: <$proxy as Proxy>::Event,
                    (): &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
}

ignore_events!(
    WlCompositor,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSurface,
    XdgToplevel
);
