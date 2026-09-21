#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
)))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use istmo_core::Runtime;
    use istmo_pen::{
        PenClient, PenConfig, PenHost, backend::PenPublisherFactory, publisher::PenPublisher,
    };
    use pen_demo::{WINDOW_ID, pump_events_stream, pump_hover_stream};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::window::{Window, WindowId};

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let init = Runtime::mock();
    let runtime = init.runtime.clone();
    runtime.declare_plugin(istmo_pen::PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&runtime);
    #[cfg(target_os = "linux")]
    {
        match publisher.install_libinput() {
            Ok(()) => log::info!("libinput backend installed"),
            Err(err) => log::warn!(
                "libinput backend unavailable: {err} — falling back to push_event hatch",
            ),
        }
    }

    runtime.register_host(PenHost::new(PenPublisherFactory::new(Arc::clone(&publisher))));

    struct App {
        runtime: Arc<Runtime>,
        publisher: Arc<PenPublisher>,
        window: Option<Window>,
        client_ready: bool,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("istmo-pen demo — draw with your stylus, watch logs")
                .with_inner_size(winit::dpi::LogicalSize::new(720.0, 520.0));
            let window = event_loop
                .create_window(attrs)
                .expect("create winit window");

            if let Err(err) = self.publisher.register_window(WINDOW_ID, &window) {
                log::error!("register_window failed: {err}");
            } else {
                log::info!("window registered with PenPublisher");
            }

            if !self.client_ready {
                let runtime = Arc::clone(&self.runtime);
                std::thread::spawn(move || {
                    let client =
                        match pollster::block_on(PenClient::from_runtime_with(&runtime, PenConfig::new(WINDOW_ID))) {
                            Ok(c) => c,
                            Err(err) => {
                                log::error!("PenClient::from_runtime_with failed: {err:?}");
                                return;
                            }
                        };
                    let events = match client.events() {
                        Ok(s) => s,
                        Err(err) => {
                            log::error!("events stream failed: {err:?}");
                            return;
                        }
                    };
                    let hover = match client.hover() {
                        Ok(s) => s,
                        Err(err) => {
                            log::error!("hover stream failed: {err:?}");
                            return;
                        }
                    };
                    std::thread::spawn(move || pump_hover_stream(hover));
                    pump_events_stream(events);
                });
                self.client_ready = true;
            }

            self.window = Some(window);
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            _id: WindowId,
            event: WindowEvent,
        ) {
            if let WindowEvent::CloseRequested = event {
                event_loop.exit();
            }
        }
    }

    let event_loop = EventLoop::new()?;
    let mut app = App {
        runtime,
        publisher,
        window: None,
        client_ready: false,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
fn main() {}
