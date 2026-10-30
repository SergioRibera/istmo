use istmo_core::{Runtime, codec};
use istmo_plugins::{
    Color, ColorScheme, THEME_PREFERENCE_CHANNEL, ThemePreference, ThemePreferenceClient,
};

fn sample() -> ThemePreference {
    ThemePreference {
        color_scheme: ColorScheme::Dark,
        accent_color: Some(Color::rgb(0x4C, 0x8B, 0xF5)),
        high_contrast: false,
        font_scale: 1.15,
        reduce_motion: false,
        reduce_transparency: false,
        bold_text: false,
        invert_colors: false,
        screen_reader_active: false,
    }
}

fn publish(rt: &std::sync::Arc<Runtime>, pref: &ThemePreference) {
    let bytes = codec::encode(pref).expect("encode");
    rt.publish_early_latest(THEME_PREFERENCE_CHANNEL, bytes);
}

#[test]
fn current_is_none_before_publish() {
    let init = Runtime::mock();
    let plugin = ThemePreferenceClient::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.current().unwrap().is_none());
}

#[test]
fn late_subscriber_sees_retained() {
    let init = Runtime::mock();
    let rt = init.runtime;
    publish(&rt, &sample());
    let plugin = ThemePreferenceClient::from_runtime(&rt).expect("declared");
    assert_eq!(plugin.current().unwrap(), Some(sample()));
    assert_eq!(plugin.updates().recv().unwrap(), sample());
}

#[test]
fn updates_stream_delivers_subsequent_changes() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = ThemePreferenceClient::from_runtime(&rt).expect("declared");
    let stream = plugin.updates();

    publish(&rt, &sample());
    assert_eq!(stream.recv().unwrap(), sample());

    let mut next = sample();
    next.color_scheme = ColorScheme::Light;
    next.font_scale = 2.0;
    next.reduce_motion = true;
    publish(&rt, &next);
    assert_eq!(stream.recv().unwrap(), next);
}

#[test]
fn color_helpers_fill_alpha() {
    assert_eq!(Color::rgb(10, 20, 30).a, 255);
    assert_eq!(Color::rgba(10, 20, 30, 128).a, 128);
}
