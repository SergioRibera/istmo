//! Shared entry point for the `istmo-pen` demo — desktop `main.rs`
//! calls into [`run_desktop`] with an already-registered window, and
//! the mobile `#[istmo::mobile_app]` entry drives a headless event
//! logger for Android.

pub const WINDOW_ID: u64 = 1;

use istmo_pen::{PenClient, PenEvent, PenHoverEvent};
#[cfg(target_os = "android")]
use istmo_pen::PenConfig;

istmo::runtime!(
    plugins: [PenClient],
);

pub fn pump_events_stream(stream: istmo_core::TypedStream<PenEvent, ()>) {
    loop {
        match stream.recv() {
            Ok(istmo_core::StreamItem::Event(event)) => {
                log::info!("pen event: {event:?}");
            }
            Ok(istmo_core::StreamItem::Completed) => {
                log::info!("pen events stream completed");
                break;
            }
            Ok(istmo_core::StreamItem::Cancelled) => {
                log::info!("pen events stream cancelled");
                break;
            }
            Ok(istmo_core::StreamItem::Failed(_)) => {
                log::warn!("pen events stream failed");
                break;
            }
            Err(err) => {
                log::error!("pen events recv error: {err:?}");
                break;
            }
        }
    }
}

pub fn pump_hover_stream(stream: istmo_core::TypedStream<PenHoverEvent, ()>) {
    loop {
        match stream.recv() {
            Ok(istmo_core::StreamItem::Event(event)) => {
                log::info!("pen hover: {event:?}");
            }
            Ok(istmo_core::StreamItem::Completed | istmo_core::StreamItem::Cancelled) => break,
            Ok(istmo_core::StreamItem::Failed(_)) => break,
            Err(_) => break,
        }
    }
}

#[cfg(target_os = "android")]
#[istmo::mobile_app]
fn mobile_main() {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("pen-demo mobile_main starting");

    let client = match pollster::block_on(PenClient::acquire_with(PenConfig::new(WINDOW_ID))) {
        Ok(c) => c,
        Err(err) => {
            log::error!("PenClient::acquire_with failed: {err:?}");
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
}
