//! `[[app.url_scheme]]` — the custom URL schemes an app answers.
//!
//! One declaration feeds every platform:
//!
//! - **Android** — a `VIEW` + `BROWSABLE` `<intent-filter>` per scheme
//!   for the launcher activity ([`UrlSchemes::render_android_intent_filters`]),
//!   patched next to the `[[app.document_type]]` filters.
//! - **Apple** — `CFBundleURLTypes` ([`UrlSchemes::render_info_plist`]).
//! - **Desktop** — packaging tools (`.desktop` `x-scheme-handler/…`,
//!   Windows `URL Protocol` keys, the macOS `.app` plist) read the table
//!   straight from `istmo.toml`; delivery at run time goes through the
//!   `istmo.deep_links` plugin.
//!
//! ```toml
//! [[app.url_scheme]]
//! scheme = "com.example.notes"
//! name = "Notes sign-in callback"
//! ```

use std::fmt::Write as _;

use toml_edit::{Item, TableLike};

use crate::manifest::{ManifestError, expect_string, expect_table};

const KNOWN_KEYS: &[&str] = &["scheme", "name"];

/// Schemes the platforms own; registering them would hijack (or fail
/// to hijack) every link of that kind.
const RESERVED: &[&str] = &[
    "http",
    "https",
    "file",
    "content",
    "data",
    "javascript",
    "about",
    "blob",
    "intent",
    "mailto",
    "tel",
    "sms",
    "ftp",
    "ws",
    "wss",
];

/// One `[[app.url_scheme]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlScheme {
    /// Lowercase RFC 3986 scheme, without `:`.
    pub scheme: String,
    /// Human-readable name (`CFBundleURLName`). Defaults to the scheme.
    pub name: Option<String>,
}

impl UrlScheme {
    fn parse(table: &dyn TableLike) -> Result<Self, ManifestError> {
        for (name, _) in table.iter() {
            if !KNOWN_KEYS.contains(&name) {
                return Err(ManifestError::UnknownKey {
                    key: format!("app.url_scheme.{name}"),
                });
            }
        }
        let scheme = table
            .get("scheme")
            .ok_or(ManifestError::Missing {
                key: "app.url_scheme.scheme",
            })
            .and_then(|item| expect_string(item, "app.url_scheme.scheme"))?;
        if !is_valid_scheme(scheme) {
            return Err(ManifestError::InvalidValue {
                key: "app.url_scheme.scheme".to_owned(),
                value: scheme.to_owned(),
                expected: "a lowercase RFC 3986 scheme such as `com.example.app`, \
                           not a scheme the platform owns",
            });
        }
        let name = table
            .get("name")
            .map(|item| expect_string(item, "app.url_scheme.name").map(str::to_owned))
            .transpose()?;
        Ok(Self {
            scheme: scheme.to_owned(),
            name,
        })
    }
}

/// `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`, lowercase only (Android
/// matches schemes case-sensitively and lowercases incoming ones).
fn is_valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
        && !RESERVED.contains(&scheme)
}

/// Every `[[app.url_scheme]]` of an app, in declaration order, without
/// duplicates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlSchemes(Vec<UrlScheme>);

impl UrlSchemes {
    /// Parse `app.url_scheme`, written as one table or an array of
    /// tables.
    pub fn parse(item: &Item) -> Result<Self, ManifestError> {
        let entries: Vec<UrlScheme> = if let Some(array) = item.as_array_of_tables() {
            array
                .iter()
                .map(|table| UrlScheme::parse(table))
                .collect::<Result<_, _>>()?
        } else {
            vec![UrlScheme::parse(expect_table(item, "app.url_scheme")?)?]
        };
        for (index, entry) in entries.iter().enumerate() {
            if entries[..index].iter().any(|e| e.scheme == entry.scheme) {
                return Err(ManifestError::InvalidValue {
                    key: "app.url_scheme.scheme".to_owned(),
                    value: entry.scheme.clone(),
                    expected: "each scheme declared once",
                });
            }
        }
        Ok(Self(entries))
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &UrlScheme> {
        self.0.iter()
    }

    /// `<intent-filter>` elements for the activity that receives links,
    /// or an empty string when no scheme is declared.
    #[must_use]
    pub fn render_android_intent_filters(&self) -> String {
        let mut out = String::new();
        for entry in self.iter() {
            out.push_str("<intent-filter>\n");
            out.push_str("    <action android:name=\"android.intent.action.VIEW\"/>\n");
            out.push_str("    <category android:name=\"android.intent.category.DEFAULT\"/>\n");
            out.push_str("    <category android:name=\"android.intent.category.BROWSABLE\"/>\n");
            let _ = writeln!(
                out,
                "    <data android:scheme=\"{}\"/>",
                crate::xml::escape(&entry.scheme)
            );
            out.push_str("</intent-filter>\n");
        }
        out
    }

    /// `CFBundleURLTypes`, or an empty string when no scheme is declared.
    #[must_use]
    pub fn render_info_plist(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::from("<key>CFBundleURLTypes</key>\n<array>\n");
        for entry in self.iter() {
            let name = entry.name.as_deref().unwrap_or(&entry.scheme);
            let _ = write!(
                out,
                "    <dict>\n        <key>CFBundleURLName</key>\n        <string>{}</string>\n\
                 \x20       <key>CFBundleTypeRole</key>\n        <string>Viewer</string>\n\
                 \x20       <key>CFBundleURLSchemes</key>\n        <array>\n\
                 \x20           <string>{}</string>\n        </array>\n    </dict>\n",
                crate::xml::escape(name),
                crate::xml::escape(&entry.scheme),
            );
        }
        out.push_str("</array>\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use toml_edit::DocumentMut;

    use super::*;

    fn parse(source: &str) -> Result<UrlSchemes, ManifestError> {
        let doc: DocumentMut = source.parse().unwrap();
        UrlSchemes::parse(&doc["app"]["url_scheme"])
    }

    #[test]
    fn renders_android_filters_and_plist() {
        let schemes = parse(
            "[[app.url_scheme]]\nscheme = \"com.example.app\"\nname = \"Sign-in\"\n\
             [[app.url_scheme]]\nscheme = \"notes+x\"\n",
        )
        .unwrap();
        let android = schemes.render_android_intent_filters();
        assert_eq!(android.matches("<intent-filter>").count(), 2);
        assert!(android.contains("android.intent.category.BROWSABLE"));
        assert!(android.contains("android:scheme=\"com.example.app\""));
        let plist = schemes.render_info_plist();
        assert!(plist.contains("<string>Sign-in</string>"));
        assert!(plist.contains("<string>notes+x</string>"));
        assert_eq!(plist.matches("<key>CFBundleURLName</key>").count(), 2);
    }

    #[test]
    fn single_table_form_is_accepted() {
        let schemes = parse("[app.url_scheme]\nscheme = \"myapp\"\n").unwrap();
        assert_eq!(schemes.iter().count(), 1);
    }

    #[test]
    fn rejects_invalid_reserved_and_duplicate_schemes() {
        for bad in ["MyApp", "1app", "my_app", "https", "javascript", ""] {
            let err = parse(&format!("[[app.url_scheme]]\nscheme = \"{bad}\"\n")).unwrap_err();
            assert!(
                matches!(err, ManifestError::InvalidValue { .. }),
                "{bad}: {err}"
            );
        }
        let dup = parse("[[app.url_scheme]]\nscheme = \"a\"\n[[app.url_scheme]]\nscheme = \"a\"\n");
        assert!(dup.is_err());
        let unknown = parse("[[app.url_scheme]]\nscheme = \"a\"\nhost = \"b\"\n");
        assert!(matches!(unknown, Err(ManifestError::UnknownKey { .. })));
    }

    #[test]
    fn empty_renders_nothing() {
        let schemes = UrlSchemes::default();
        assert!(schemes.render_android_intent_filters().is_empty());
        assert!(schemes.render_info_plist().is_empty());
    }
}
