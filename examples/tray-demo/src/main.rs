//! A tray-only desktop app: a menu with every item kind, a clock item
//! updated every second, a title toggled from the menu, and three icon
//! modes (static, animated, icons swapped by handle).
//!
//! Run with `cargo run -p tray-demo`; quit from the menu.

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]
mod demo;

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use istmo_core::{Runtime, RuntimeConfig, RuntimeInit};
    use istmo_tray::{DesktopTray, TrayClient, TrayHost};

    let RuntimeInit { runtime, outbound } = Runtime::init(RuntimeConfig::inline())?;
    // Every call is served in this process; nothing is meant for a peer.
    std::thread::Builder::new()
        .name("outbound-drain".to_owned())
        .spawn(move || while outbound.recv().is_ok() {})?;
    runtime.register_host(TrayHost::new(DesktopTray::default()));
    let tray = TrayClient::from_runtime(&runtime)?;

    // AppKit needs the main thread's run loop; the demo runs beside it.
    #[cfg(target_os = "macos")]
    {
        std::thread::Builder::new()
            .name("tray-demo".to_owned())
            .spawn(move || {
                if let Err(err) = demo::run(&tray) {
                    eprintln!("tray demo: {err}");
                }
                std::process::exit(0);
            })?;
        istmo_tray::run_event_loop(false)
    }
    #[cfg(not(target_os = "macos"))]
    demo::run(&tray)
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn main() {
    eprintln!("the tray demo runs on Linux, Windows and macOS");
}
