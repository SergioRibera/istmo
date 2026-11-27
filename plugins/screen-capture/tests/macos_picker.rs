//! The content-sharing picker against a real Mac (14+), with
//! `ISTMO_SC_TEST_MACOS_PICKER` set; someone has to pick. The picker is
//! presented from the main queue, so this test has no libtest harness
//! (`harness = false`): `main` runs `NSApplication` on the main thread
//! while a worker drives the plugin, then exits with the result.

#[cfg(target_os = "macos")]
mod common;

#[cfg(target_os = "macos")]
fn main() {
    use std::panic::catch_unwind;

    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    if !common::enabled("ISTMO_SC_TEST_MACOS_PICKER") {
        return;
    }
    let mtm = MainThreadMarker::new().expect("harness = false runs main on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let _regular = app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    std::thread::Builder::new()
        .name("picker-test".to_owned())
        .spawn(|| {
            let code = i32::from(catch_unwind(the_picker_shows_with_allow_picker).is_err());
            std::process::exit(code);
        })
        .expect("spawn the test thread");
    app.run();
}

#[cfg(target_os = "macos")]
fn the_picker_shows_with_allow_picker() {
    use istmo_screen_capture::{
        BackendId, ConsentConfig, PickerOptions, PixelLayout, ScreenCaptureClientExt,
        ScreenshotRequest, SourceSelector,
    };

    let live = common::live();
    let request = ScreenshotRequest::of(SourceSelector::Picker(PickerOptions::default()))
        .backend(common::fixed(BackendId::MacosSck))
        .consent(ConsentConfig {
            allow_picker: true,
            ..ConsentConfig::default()
        });
    let image = common::block(live.client.screenshot_cpu(request, PixelLayout::default()))
        .expect("pick something in the picker");
    eprintln!("picked: {}x{}", image.width, image.height);
    assert!(image.width > 0);
}

#[cfg(not(target_os = "macos"))]
fn main() {}
