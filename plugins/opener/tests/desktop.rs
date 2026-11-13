use istmo_opener::{DesktopOpener, OpenError, Opener, SchemePolicy};

#[test]
fn empty_url_is_invalid() {
    let result = pollster::block_on(DesktopOpener::default().open(String::new()));
    assert_eq!(result, Err(OpenError::Invalid));
}

#[test]
fn disallowed_scheme_is_refused_before_spawning() {
    let result = pollster::block_on(DesktopOpener::default().open("file:///etc/passwd".into()));
    assert_eq!(result, Err(OpenError::SchemeNotAllowed("file".into())));
}

#[test]
fn default_policy_allows_common_schemes() {
    let policy = SchemePolicy::default();
    for url in [
        "https://example.com/?a=1&b=2",
        "HTTP://example.com",
        "mailto:a@example.com",
        "tel:+15551234",
        "sms:+15551234",
        "geo:0,0",
    ] {
        assert_eq!(policy.check(url), Ok(()), "{url}");
    }
}

#[test]
fn policy_rejects_malformed_urls() {
    let policy = SchemePolicy::default();
    for url in [
        "",
        "-https://x",
        "example.com",
        "1http://x",
        "https://a\nb",
        ":x",
    ] {
        assert_eq!(policy.check(url), Err(OpenError::Invalid), "{url:?}");
    }
}

#[test]
fn policy_rejects_invisible_format_characters() {
    let policy = SchemePolicy::default();
    for url in [
        "https://exa\u{200B}mple.com",
        "https://example.com/\u{200F}",
        "https://\u{202E}moc.elpmaxe",
        "https://example.com/\u{2066}x\u{2069}",
        "\u{FEFF}https://example.com",
        "https://example.com/\u{00AD}",
        "https://example.com/\u{E0041}",
    ] {
        assert_eq!(policy.check(url), Err(OpenError::Invalid), "{url:?}");
    }
    // Visible non-ASCII text stays allowed.
    assert_eq!(policy.check("https://例え.jp/ñ"), Ok(()));
}

#[test]
fn policy_can_be_widened_and_narrowed() {
    let widened = SchemePolicy::default().allow("FILE");
    assert_eq!(widened.check("file:///tmp"), Ok(()));

    let narrowed = SchemePolicy::only(["https"]);
    assert_eq!(
        narrowed.check("http://example.com"),
        Err(OpenError::SchemeNotAllowed("http".into()))
    );
}

#[test]
fn plugin_error_payload_decodes_to_typed_error() {
    let bytes = istmo_core::codec::encode(&OpenError::NoHandler).expect("encode");
    let err = OpenError::from(istmo_core::IstmoError::PluginError { bytes });
    assert_eq!(err, OpenError::NoHandler);
}
