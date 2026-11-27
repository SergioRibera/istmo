//! `LauncherAction::validate` / `validate_set` and the small helpers.

use istmo_launcher_actions::{
    ActionIcon, ActionProblem, LauncherAction, LauncherError, Progress, action_id_from_source,
    action_source,
};

fn action(id: &str, url: &str) -> LauncherAction {
    LauncherAction {
        id: id.into(),
        title: "New note".into(),
        subtitle: Some("Start an empty note".into()),
        url: url.into(),
        icon: Some(ActionIcon::Symbol("square.and.pencil".into())),
    }
}

fn reason(result: Result<(), LauncherError>) -> String {
    match result {
        Err(LauncherError::InvalidAction(ActionProblem { reason, .. })) => reason,
        other => panic!("expected InvalidAction, got {other:?}"),
    }
}

#[test]
fn a_well_formed_action_passes() {
    assert_eq!(action("compose", "notes://compose").validate(), Ok(()));
    assert_eq!(
        action("a.b-c_9", "com.example.notes:/x?y=1").validate(),
        Ok(())
    );
}

#[test]
fn ids_must_be_short_and_plain() {
    for id in ["", "has space", "slash/no", "ünï", &"x".repeat(65)] {
        assert!(
            reason(action(id, "notes://x").validate()).starts_with("id "),
            "{id:?}"
        );
    }
    assert_eq!(action(&"x".repeat(64), "notes://x").validate(), Ok(()));
}

#[test]
fn urls_need_a_non_web_scheme_and_one_token() {
    for url in [
        "",
        "compose",
        "1notes://x",
        "-notes://x",
        "notes://a b",
        "notes://a\"b",
        "notes://a\u{200B}b",
        "notes://a\nb",
        "http://example.com",
        "HTTPS://example.com",
    ] {
        assert!(action("a", url).validate().is_err(), "{url:?}");
    }
    assert!(reason(action("a", "https://x").validate()).contains("opener"));
}

#[test]
fn labels_must_not_be_blank_or_long() {
    let mut blank = action("a", "notes://x");
    blank.title = "  ".into();
    assert_eq!(reason(blank.validate()), "title is blank");
    let mut long = action("a", "notes://x");
    long.subtitle = Some("é".repeat(257));
    assert!(reason(long.validate()).contains("256"));
    let mut control = action("a", "notes://x");
    control.title = "a\u{7}".into();
    assert!(control.validate().is_err());
}

#[test]
fn icons_are_checked() {
    let mut png = action("a", "notes://x");
    png.icon = Some(ActionIcon::Png(b"\x89PNG\r\n\x1a\nrest".to_vec()));
    assert_eq!(png.validate(), Ok(()));
    png.icon = Some(ActionIcon::Png(b"GIF89a".to_vec()));
    assert_eq!(reason(png.validate()), "icon is not a PNG");
    png.icon = Some(ActionIcon::Symbol("bad name".into()));
    assert!(png.validate().is_err());
}

#[test]
fn sets_need_unique_ids() {
    let set = [action("a", "notes://1"), action("b", "notes://2")];
    assert_eq!(LauncherAction::validate_set(&set), Ok(()));
    let dup = [action("a", "notes://1"), action("a", "notes://2")];
    assert_eq!(
        LauncherAction::validate_set(&dup),
        Err(LauncherError::InvalidAction(ActionProblem {
            id: "a".into(),
            reason: "id used by an earlier action".into(),
        }))
    );
}

#[test]
fn sources_round_trip() {
    assert_eq!(action_source("compose"), "launcher-action:compose");
    assert_eq!(
        action_id_from_source("launcher-action:compose"),
        Some("compose")
    );
    assert_eq!(action_id_from_source("launcher-action:"), None);
    assert_eq!(action_id_from_source("launch"), None);
}

#[test]
fn progress_fractions_are_clamped() {
    assert_eq!(Progress::Normal(1.5).fraction(), Some(1.0));
    assert_eq!(Progress::Paused(-1.0).fraction(), Some(0.0));
    assert_eq!(Progress::Error(f32::NAN).fraction(), Some(0.0));
    assert_eq!(Progress::Indeterminate.fraction(), None);
}

#[test]
fn errors_survive_the_plugin_error_payload() {
    let err = LauncherError::TooMany(4);
    let bytes = istmo_core::codec::encode(&err).expect("encode");
    let back = LauncherError::from(istmo_core::IstmoError::PluginError { bytes });
    assert_eq!(back, err);
}
