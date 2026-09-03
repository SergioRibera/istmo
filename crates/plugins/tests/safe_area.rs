//! `SafeArea` plugin: verifies inset publishing, replay for late subscribers,
//! and the Flutter-style `padding` / `view_padding` combinators.

use istmo_core::{Runtime, codec};
use istmo_plugins::{EdgeInsets, SAFE_AREA_CHANNEL, SafeArea, SafeAreaInsets};

fn publish(rt: &std::sync::Arc<Runtime>, insets: SafeAreaInsets) {
    let bytes = codec::encode(&insets).expect("encode");
    rt.publish_early_latest(SAFE_AREA_CHANNEL, bytes);
}

fn insets_with_status_bar(top: f32) -> SafeAreaInsets {
    SafeAreaInsets {
        system_bars: EdgeInsets {
            top,
            ..EdgeInsets::ZERO
        },
        ..SafeAreaInsets::default()
    }
}

#[test]
fn current_is_none_before_any_publish() {
    let init = Runtime::mock();
    let plugin = SafeArea::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.current().unwrap().is_none());
    assert_eq!(plugin.current_or_zero(), SafeAreaInsets::default());
}

#[test]
fn late_subscriber_receives_retained_insets_first() {
    let init = Runtime::mock();
    let rt = init.runtime;

    publish(&rt, insets_with_status_bar(24.0));
    publish(&rt, insets_with_status_bar(28.0));

    let plugin = SafeArea::from_runtime(&rt).expect("declared");
    let current = plugin.current().unwrap().unwrap();
    assert!((current.system_bars.top - 28.0).abs() < f32::EPSILON);

    let stream = plugin.stream();
    let first = stream.recv().unwrap();
    assert!((first.system_bars.top - 28.0).abs() < f32::EPSILON);

    publish(&rt, insets_with_status_bar(0.0));
    let second = stream.recv().unwrap();
    assert!(second.system_bars.top.abs() < f32::EPSILON);
}

#[test]
fn padding_is_element_wise_max_of_system_bars_and_cutout() {
    let insets = SafeAreaInsets {
        system_bars: EdgeInsets {
            top: 24.0,
            right: 0.0,
            bottom: 48.0,
            left: 0.0,
        },
        ime: EdgeInsets::ZERO,
        display_cutout: EdgeInsets {
            top: 40.0,
            right: 8.0,
            bottom: 0.0,
            left: 8.0,
        },
    };
    let padding = insets.padding();
    // Cutout wins on top + horizontal edges (notch), system-bar wins
    // on bottom (nav-bar taller than cutout there).
    assert!((padding.top - 40.0).abs() < f32::EPSILON);
    assert!((padding.right - 8.0).abs() < f32::EPSILON);
    assert!((padding.bottom - 48.0).abs() < f32::EPSILON);
    assert!((padding.left - 8.0).abs() < f32::EPSILON);
}

#[test]
fn view_padding_expands_bottom_by_ime_height() {
    let insets = SafeAreaInsets {
        system_bars: EdgeInsets {
            top: 24.0,
            bottom: 16.0,
            ..EdgeInsets::ZERO
        },
        ime: EdgeInsets {
            bottom: 320.0,
            ..EdgeInsets::ZERO
        },
        display_cutout: EdgeInsets::ZERO,
    };
    let view_padding = insets.view_padding();
    // Nav bar contributes 16 on bottom; keyboard shoves that to 320.
    assert!((view_padding.bottom - 320.0).abs() < f32::EPSILON);
    // Top unchanged — IME never reserves top space in single-window mode.
    assert!((view_padding.top - 24.0).abs() < f32::EPSILON);
}

#[test]
fn edge_insets_helpers_report_horizontal_and_vertical_totals() {
    let e = EdgeInsets {
        top: 10.0,
        right: 20.0,
        bottom: 30.0,
        left: 40.0,
    };
    assert!((e.horizontal() - 60.0).abs() < f32::EPSILON);
    assert!((e.vertical() - 40.0).abs() < f32::EPSILON);
}

#[test]
fn multiple_subscribers_all_observe_each_update() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = SafeArea::from_runtime(&rt).expect("declared");
    let s1 = plugin.stream();
    let s2 = plugin.stream();
    publish(&rt, insets_with_status_bar(30.0));
    assert!((s1.recv().unwrap().system_bars.top - 30.0).abs() < f32::EPSILON);
    assert!((s2.recv().unwrap().system_bars.top - 30.0).abs() < f32::EPSILON);
}

#[test]
fn plugin_id_matches_channel_key() {
    // Contract: a native backend that publishes on the wire channel needs
    // to use exactly this key, and clients declaring the plugin use the
    // same id. Drift between them silently breaks the subscription.
    use istmo_core::Plugin;
    assert_eq!(SafeArea::PLUGIN_ID, SAFE_AREA_CHANNEL);
}
