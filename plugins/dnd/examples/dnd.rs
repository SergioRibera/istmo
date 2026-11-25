//! Open a window, print what is dragged onto it, and drag something out
//! of it: press anywhere and move to drag a text and a file to another
//! app (a text editor, a file manager).
//!
//! `cargo run -p istmo-dnd --example dnd`. Drops on the window come from
//! the publisher on Wayland (Flatpak included) and from winit elsewhere;
//! dragging out works on Wayland, X11, macOS and Windows.

use std::sync::Arc;

use istmo_core::Runtime;
use istmo_dnd::{
    DragAndDropClient, DragFile, DragItem, DragRequest, DropInbox, DropOptions, drag_out_dir,
};
use istmo_window::{WindowId, WindowRegistry};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::Window;

struct App {
    window: Option<Window>,
    client: Arc<DragAndDropClient>,
    /// The left button is down and no drag started from this press yet.
    pressed: bool,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(
                Window::default_attributes().with_title("Drop files here, drag out from here"),
            )
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
            WindowEvent::MouseInput { state, .. } => self.pressed = state == ElementState::Pressed,
            WindowEvent::CursorMoved { .. } if self.pressed => {
                self.pressed = false;
                let client = Arc::clone(&self.client);
                std::thread::spawn(move || drag_out(&client));
            }
            _ => {}
        }
    }
}

fn drag_out(client: &DragAndDropClient) {
    let file = drag_out_dir()
        .and_then(|dir| {
            let path = dir.join("dragged out.txt");
            std::fs::write(&path, "written by the istmo-dnd example\n")?;
            Ok(path)
        })
        .expect("write the file to drag");
    let request = DragRequest::new([
        DragItem::Text("Hello from istmo-dnd".into()),
        DragItem::File(DragFile {
            path: file.display().to_string(),
            mime_type: Some("text/plain".into()),
        }),
    ]);
    println!(
        "drag out: {:?}",
        pollster::block_on(client.start_drag(request))
    );
}

fn main() {
    let runtime: Arc<Runtime> = Runtime::mock()
        .expects::<DragAndDropClient>()
        .finish()
        .runtime;
    let drops = istmo_dnd::register_desktop(&runtime);
    drops
        .follow_registry(WindowRegistry::global())
        .expect("follow windows");
    let client = Arc::new(DragAndDropClient::from_runtime(&runtime).expect("declared"));
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
    event_loop
        .run_app(&mut App {
            window: None,
            client,
            pressed: false,
        })
        .expect("run");
    drop(drops);
}
