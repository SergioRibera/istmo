//! Open a window and print what is dragged onto it.
//!
//! `cargo run -p istmo-drop --example wayland_drop` in a Wayland session
//! (or inside Flatpak), then drag files from a file manager onto the
//! window. On X11 the publisher has nothing to do: winit gets the drops.

use std::sync::Arc;

use istmo_core::Runtime;
use istmo_drop::{DropInbox, DropOptions, FileDropClient};
use istmo_window::{WindowId, WindowRegistry};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::Window;

struct App {
    window: Option<Window>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(Window::default_attributes().with_title("Drop files here"))
            .expect("window");
        WindowRegistry::global()
            .register(WindowId(1), &window)
            .expect("register window");
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                WindowRegistry::global().unregister(WindowId(1));
                event_loop.exit();
            }
            WindowEvent::DroppedFile(path) => println!("winit dropped {}", path.display()),
            _ => {}
        }
    }
}

fn main() {
    let runtime: Arc<Runtime> = Runtime::mock().expects::<FileDropClient>().finish().runtime;
    let drops = istmo_drop::register_desktop(&runtime);
    drops
        .follow_registry(WindowRegistry::global())
        .expect("follow windows");
    let client = FileDropClient::from_runtime(&runtime).expect("declared");
    pollster::block_on(client.listen(DropOptions::any().with_folders())).expect("listen");

    let inbox = DropInbox::from_runtime(&runtime).expect("declared");
    let changes = inbox.drag_changes();
    std::thread::spawn(move || {
        while let Ok(state) = changes.recv() {
            println!("drag: {state:?}");
        }
    });
    let stream = inbox.stream();
    std::thread::spawn(move || {
        while let Ok(dropped) = stream.recv() {
            println!("dropped: {dropped:#?}");
        }
    });

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.run_app(&mut App { window: None }).expect("run");
    drop(drops);
}
