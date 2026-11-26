//! `[[app.launcher_action]]` — the app's static launcher actions.
//!
//! One declaration feeds every platform; the `istmo-launcher-actions`
//! plugin turns an activated action into a deep link.
//!
//! - **Android** — `res/xml/istmo_launcher_actions.xml` (`<shortcuts>`)
//!   plus its string resources and icons, written into the generated
//!   `android/.istmo/res` ([`LauncherActions::write_android`]), and the
//!   `android.app.shortcuts` `<meta-data>` patched onto the launcher
//!   activity next to the `[[app.url_scheme]]` filters
//!   ([`LauncherActions::render_android_meta_data`]).
//! - **Apple** — `UIApplicationShortcutItems`
//!   ([`LauncherActions::render_info_plist`]).
//! - **Desktop** — packaging tools read the table straight from
//!   `istmo.toml` (`.desktop` `Actions=`); Windows and macOS have no
//!   static form, the app sets them as dynamic actions at start.
//!
//! ```toml
//! [[app.launcher_action]]
//! id = "compose"
//! title = "New note"
//! subtitle = "Start an empty note"
//! url = "notes://compose"
//! icon = "assets/compose.png"        # .png or an Android vector .xml
//! ios_symbol = "square.and.pencil"
//! ```
//!
//! The rules match `LauncherAction::validate` in the plugin crate; keep
//! them in step.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{Item, TableLike};

use crate::app_icon::IconError;
use crate::manifest::{ManifestError, expect_string, expect_table};

const KNOWN_KEYS: &[&str] = &["id", "title", "subtitle", "url", "icon", "ios_symbol"];

/// Class every shortcut targets (the plugin's trampoline).
pub const ANDROID_TRAMPOLINE: &str =
    "dev.istmo.plugins.launcher_actions.IstmoLauncherActionActivity";

/// Intent extra carrying the action id (`IstmoLauncherActionActivity.EXTRA_ACTION_ID`).
pub const ANDROID_ID_EXTRA: &str = "dev.istmo.launcher_actions.ID";

/// `userInfo` key of the URL (`IstmoLauncherActions.urlKey`).
pub const IOS_URL_KEY: &str = "url";

/// Plugin id whose crate ships the trampoline.
pub const PLUGIN_ID: &str = "istmo.launcher_actions";

/// Prefix of every generated resource.
const RESOURCE_PREFIX: &str = "istmo_launcher_action";
/// `res/xml/<name>.xml` and `res/values/<name>.xml`.
const RESOURCE_FILE: &str = "istmo_launcher_actions";

const MAX_ID_LEN: usize = 64;
const MAX_TITLE_CHARS: usize = 128;
const MAX_SUBTITLE_CHARS: usize = 256;
const MAX_URL_LEN: usize = 2048;

/// The icon of a static action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionIconFile {
    /// A PNG, copied to `drawable-nodpi/`.
    Png(PathBuf),
    /// An Android vector drawable, copied to `drawable/`.
    Vector(PathBuf),
}

impl ActionIconFile {
    fn path(&self) -> &Path {
        match self {
            Self::Png(path) | Self::Vector(path) => path,
        }
    }

    fn rooted(&self, root: &Path) -> Self {
        match self {
            Self::Png(path) => Self::Png(root.join(path)),
            Self::Vector(path) => Self::Vector(root.join(path)),
        }
    }

    /// `(directory, file name)` of the copy for the action at `index`.
    fn destination(&self, index: usize) -> (&'static str, String) {
        match self {
            Self::Png(_) => ("drawable-nodpi", format!("{RESOURCE_PREFIX}_{index}.png")),
            Self::Vector(_) => ("drawable", format!("{RESOURCE_PREFIX}_{index}.xml")),
        }
    }
}

/// One `[[app.launcher_action]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherActionSpec {
    /// `1..=64` bytes of `[A-Za-z0-9._-]`.
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub url: String,
    /// Android icon (`.png` or vector `.xml`), relative to the crate.
    pub icon: Option<ActionIconFile>,
    /// SF Symbol name for iOS.
    pub ios_symbol: Option<String>,
}

impl LauncherActionSpec {
    fn parse(table: &dyn TableLike) -> Result<Self, ManifestError> {
        for (name, _) in table.iter() {
            if !KNOWN_KEYS.contains(&name) {
                return Err(ManifestError::UnknownKey {
                    key: format!("app.launcher_action.{name}"),
                });
            }
        }
        let required = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .ok_or(ManifestError::Missing { key: full })
                .and_then(|item| expect_string(item, full).map(str::to_owned))
        };
        let optional = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .map(|item| expect_string(item, full).map(str::to_owned))
                .transpose()
        };
        let spec = Self {
            id: required("id", "app.launcher_action.id")?,
            title: required("title", "app.launcher_action.title")?,
            subtitle: optional("subtitle", "app.launcher_action.subtitle")?,
            url: required("url", "app.launcher_action.url")?,
            icon: optional("icon", "app.launcher_action.icon")?
                .map(|icon| parse_icon(&icon))
                .transpose()?,
            ios_symbol: optional("ios_symbol", "app.launcher_action.ios_symbol")?,
        };
        spec.validate()?;
        Ok(spec)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let invalid =
            |key: &str, value: &str, expected: &'static str| ManifestError::InvalidValue {
                key: format!("app.launcher_action.{key}"),
                value: value.to_owned(),
                expected,
            };
        if !is_valid_id(&self.id) {
            return Err(invalid("id", &self.id, "1..=64 bytes of [A-Za-z0-9._-]"));
        }
        if !is_valid_label(&self.title, MAX_TITLE_CHARS) {
            return Err(invalid(
                "title",
                &self.title,
                "a non-blank label of at most 128 characters, no control characters",
            ));
        }
        if let Some(subtitle) = &self.subtitle
            && !is_valid_label(subtitle, MAX_SUBTITLE_CHARS)
        {
            return Err(invalid(
                "subtitle",
                subtitle,
                "a non-blank label of at most 256 characters, no control characters",
            ));
        }
        if let Some(problem) = url_problem(&self.url) {
            return Err(invalid("url", &self.url, problem));
        }
        if let Some(symbol) = &self.ios_symbol
            && !is_valid_symbol(symbol)
        {
            return Err(invalid(
                "ios_symbol",
                symbol,
                "an SF Symbol name of [A-Za-z0-9._-]",
            ));
        }
        Ok(())
    }
}

fn parse_icon(raw: &str) -> Result<ActionIconFile, ManifestError> {
    let path = PathBuf::from(raw);
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Ok(ActionIconFile::Png(path)),
        Some("xml") => Ok(ActionIconFile::Vector(path)),
        _ => Err(ManifestError::InvalidValue {
            key: "app.launcher_action.icon".to_owned(),
            value: raw.to_owned(),
            expected: "a .png or an Android vector drawable .xml",
        }),
    }
}

fn is_valid_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn is_valid_label(label: &str, max_chars: usize) -> bool {
    !label.trim().is_empty()
        && label.chars().count() <= max_chars
        && !label.chars().any(char::is_control)
}

fn is_valid_symbol(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() || url.len() > MAX_URL_LEN || url.starts_with('-') {
        return Some("a URL of 1..=2048 bytes not starting with `-`");
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c.is_control() || is_format_char(c))
    {
        return Some("a URL without whitespace, `\"`, control or format characters");
    }
    let scheme = url.split_once(':').map_or("", |(scheme, _)| scheme);
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid {
        return Some("a URL with an RFC 3986 scheme, such as `notes://compose`");
    }
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        .then_some("a URL of the app's own scheme; http(s) links belong to the opener")
}

/// Same list as the plugin crate (Unicode `Cf`).
const fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// Escape `raw` for an Android string resource: the resource compiler's
/// own escapes (`\`, quotes, a leading `@` / `?`) and then XML.
fn android_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (index, ch) in raw.chars().enumerate() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '@' | '?' if index == 0 => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    crate::xml::escape(&out)
}

/// Every `[[app.launcher_action]]` of an app, in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LauncherActions(Vec<LauncherActionSpec>);

impl LauncherActions {
    /// Parse `app.launcher_action`, written as one table or an array of
    /// tables.
    pub fn parse(item: &Item) -> Result<Self, ManifestError> {
        let entries: Vec<LauncherActionSpec> = if let Some(array) = item.as_array_of_tables() {
            array
                .iter()
                .map(|table| LauncherActionSpec::parse(table))
                .collect::<Result<_, _>>()?
        } else {
            vec![LauncherActionSpec::parse(expect_table(
                item,
                "app.launcher_action",
            )?)?]
        };
        for (index, entry) in entries.iter().enumerate() {
            if entries[..index].iter().any(|e| e.id == entry.id) {
                return Err(ManifestError::InvalidValue {
                    key: "app.launcher_action.id".to_owned(),
                    value: entry.id.clone(),
                    expected: "each id declared once",
                });
            }
        }
        Ok(Self(entries))
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &LauncherActionSpec> {
        self.0.iter()
    }

    /// These actions with icon paths anchored at `root` (the crate).
    #[must_use]
    pub fn rooted(&self, root: &Path) -> Self {
        Self(
            self.0
                .iter()
                .map(|spec| LauncherActionSpec {
                    icon: spec.icon.as_ref().map(|icon| icon.rooted(root)),
                    ..spec.clone()
                })
                .collect(),
        )
    }

    /// Icon files read by [`Self::write_android`].
    pub fn source_files(&self) -> impl Iterator<Item = &Path> {
        self.0
            .iter()
            .filter_map(|spec| spec.icon.as_ref().map(ActionIconFile::path))
    }

    /// The `<meta-data>` for the launcher activity, or an empty string.
    #[must_use]
    pub fn render_android_meta_data(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        format!(
            "<meta-data android:name=\"android.app.shortcuts\" \
             android:resource=\"@xml/{RESOURCE_FILE}\"/>\n"
        )
    }

    /// `res/xml/istmo_launcher_actions.xml`. `target_package` is the
    /// application id (`[app] id`): a build with `applicationIdSuffix`
    /// needs its own value, which static shortcuts cannot express.
    #[must_use]
    pub fn render_android_shortcuts(&self, target_package: &str) -> String {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <!-- GENERATED by istmo-build from [[app.launcher_action]] - DO NOT EDIT. -->\n\
             <shortcuts xmlns:android=\"http://schemas.android.com/apk/res/android\">\n",
        );
        for (index, spec) in self.iter().enumerate() {
            let _ = writeln!(
                out,
                "    <shortcut\n        android:shortcutId=\"{}\"\n        android:enabled=\"true\"",
                crate::xml::escape(&spec.id)
            );
            // `drawable/` and `drawable-nodpi/` both resolve as `@drawable`.
            if spec.icon.is_some() {
                let _ = writeln!(
                    out,
                    "        android:icon=\"@drawable/{RESOURCE_PREFIX}_{index}\""
                );
            }
            let _ = writeln!(
                out,
                "        android:shortcutShortLabel=\"@string/{RESOURCE_PREFIX}_{index}_title\""
            );
            let long = if spec.subtitle.is_some() {
                "subtitle"
            } else {
                "title"
            };
            let _ = write!(
                out,
                "        android:shortcutLongLabel=\"@string/{RESOURCE_PREFIX}_{index}_{long}\">\n\
                 \x20       <intent\n\
                 \x20           android:action=\"android.intent.action.VIEW\"\n\
                 \x20           android:data=\"{url}\"\n\
                 \x20           android:targetPackage=\"{package}\"\n\
                 \x20           android:targetClass=\"{ANDROID_TRAMPOLINE}\">\n\
                 \x20           <extra android:name=\"{ANDROID_ID_EXTRA}\" android:value=\"{id}\"/>\n\
                 \x20       </intent>\n\
                 \x20   </shortcut>\n",
                url = crate::xml::escape(&spec.url),
                package = crate::xml::escape(target_package),
                id = crate::xml::escape(&spec.id),
            );
        }
        out.push_str("</shortcuts>\n");
        out
    }

    /// `res/values/istmo_launcher_actions.xml`: the labels.
    #[must_use]
    pub fn render_android_strings(&self) -> String {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <!-- GENERATED by istmo-build from [[app.launcher_action]] - DO NOT EDIT. -->\n\
             <resources>\n",
        );
        for (index, spec) in self.iter().enumerate() {
            let _ = writeln!(
                out,
                "    <string name=\"{RESOURCE_PREFIX}_{index}_title\">{}</string>",
                android_string(&spec.title)
            );
            if let Some(subtitle) = &spec.subtitle {
                let _ = writeln!(
                    out,
                    "    <string name=\"{RESOURCE_PREFIX}_{index}_subtitle\">{}</string>",
                    android_string(subtitle)
                );
            }
        }
        out.push_str("</resources>\n");
        out
    }

    /// `UIApplicationShortcutItems`, or an empty string.
    #[must_use]
    pub fn render_info_plist(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::from("<key>UIApplicationShortcutItems</key>\n<array>\n");
        for spec in self.iter() {
            let _ = write!(
                out,
                "    <dict>\n\
                 \x20       <key>UIApplicationShortcutItemType</key>\n\
                 \x20       <string>{}</string>\n\
                 \x20       <key>UIApplicationShortcutItemTitle</key>\n\
                 \x20       <string>{}</string>\n",
                crate::xml::escape(&spec.id),
                crate::xml::escape(&spec.title),
            );
            if let Some(subtitle) = &spec.subtitle {
                let _ = write!(
                    out,
                    "        <key>UIApplicationShortcutItemSubtitle</key>\n\
                     \x20       <string>{}</string>\n",
                    crate::xml::escape(subtitle)
                );
            }
            if let Some(symbol) = &spec.ios_symbol {
                let _ = write!(
                    out,
                    "        <key>UIApplicationShortcutItemIconSymbolName</key>\n\
                     \x20       <string>{}</string>\n",
                    crate::xml::escape(symbol)
                );
            }
            let _ = write!(
                out,
                "        <key>UIApplicationShortcutItemUserInfo</key>\n\
                 \x20       <dict>\n\
                 \x20           <key>{IOS_URL_KEY}</key>\n\
                 \x20           <string>{}</string>\n\
                 \x20       </dict>\n\
                 \x20   </dict>\n",
                crate::xml::escape(&spec.url)
            );
        }
        out.push_str("</array>\n");
        out
    }

    /// Write the shortcuts XML, the labels and the icons under `res_dir`
    /// (`android/.istmo/res`), and remove what an earlier configuration
    /// wrote and this one does not.
    pub fn write_android(&self, res_dir: &Path, target_package: &str) -> Result<(), IconError> {
        let shortcuts = res_dir.join("xml").join(format!("{RESOURCE_FILE}.xml"));
        let strings = res_dir.join("values").join(format!("{RESOURCE_FILE}.xml"));
        let mut icons = Vec::new();
        if self.is_empty() {
            remove_if_exists(&shortcuts)?;
            remove_if_exists(&strings)?;
        } else {
            write(
                &shortcuts,
                self.render_android_shortcuts(target_package).as_bytes(),
            )?;
            write(&strings, self.render_android_strings().as_bytes())?;
            for (index, spec) in self.iter().enumerate() {
                let Some(icon) = &spec.icon else { continue };
                let bytes = fs::read(icon.path()).map_err(|source| IconError::ReadFile {
                    path: icon.path().to_path_buf(),
                    source,
                })?;
                let (dir, name) = icon.destination(index);
                let dest = res_dir.join(dir).join(name);
                write(&dest, &bytes)?;
                icons.push(dest);
            }
        }
        for dir in ["drawable", "drawable-nodpi"] {
            let Ok(entries) = fs::read_dir(res_dir.join(dir)) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let ours = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&format!("{RESOURCE_PREFIX}_")));
                if ours && !icons.contains(&path) {
                    remove_if_exists(&path)?;
                }
            }
        }
        Ok(())
    }
}

fn write(dest: &Path, contents: &[u8]) -> Result<(), IconError> {
    crate::fsx::write_if_changed(dest, contents)
        .map(drop)
        .map_err(|source| IconError::Write {
            path: dest.to_path_buf(),
            source,
        })
}

fn remove_if_exists(path: &Path) -> Result<(), IconError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(IconError::Write {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use toml_edit::DocumentMut;

    use super::*;

    fn parse(source: &str) -> Result<LauncherActions, ManifestError> {
        let doc: DocumentMut = source.parse().unwrap();
        LauncherActions::parse(&doc["app"]["launcher_action"])
    }

    const TWO: &str = "[[app.launcher_action]]\nid = \"compose\"\ntitle = \"New note\"\n\
         subtitle = \"Start an 'empty' note\"\nurl = \"notes://compose?a=1&b=2\"\n\
         icon = \"assets/compose.png\"\nios_symbol = \"square.and.pencil\"\n\
         [[app.launcher_action]]\nid = \"search\"\ntitle = \"@Search\"\nurl = \"notes://search\"\n";

    #[test]
    fn renders_android_shortcuts_and_strings() {
        let actions = parse(TWO).unwrap();
        let xml = actions.render_android_shortcuts("com.example.notes");
        assert_eq!(xml.matches("<shortcut\n").count(), 2);
        assert!(xml.contains("android:shortcutId=\"compose\""));
        assert!(xml.contains("android:icon=\"@drawable/istmo_launcher_action_0\""));
        assert!(xml.contains("android:data=\"notes://compose?a=1&amp;b=2\""));
        assert!(xml.contains("android:targetPackage=\"com.example.notes\""));
        assert!(xml.contains(&format!("android:targetClass=\"{ANDROID_TRAMPOLINE}\"")));
        assert!(xml.contains(&format!(
            "<extra android:name=\"{ANDROID_ID_EXTRA}\" android:value=\"search\"/>"
        )));
        // Without a subtitle the long label is the title.
        assert!(
            xml.contains("android:shortcutLongLabel=\"@string/istmo_launcher_action_1_title\"")
        );
        let strings = actions.render_android_strings();
        assert!(strings.contains(
            "<string name=\"istmo_launcher_action_0_subtitle\">Start an \\&apos;empty\\&apos; note</string>"
        ));
        assert!(
            strings.contains("<string name=\"istmo_launcher_action_1_title\">\\@Search</string>")
        );
        assert!(!strings.contains("istmo_launcher_action_1_subtitle"));
        assert_eq!(
            actions.render_android_meta_data(),
            "<meta-data android:name=\"android.app.shortcuts\" \
             android:resource=\"@xml/istmo_launcher_actions\"/>\n"
        );
    }

    #[test]
    fn renders_info_plist() {
        let plist = parse(TWO).unwrap().render_info_plist();
        assert!(plist.starts_with("<key>UIApplicationShortcutItems</key>\n<array>\n"));
        assert_eq!(
            plist
                .matches("<key>UIApplicationShortcutItemType</key>")
                .count(),
            2
        );
        assert!(plist.contains("<string>square.and.pencil</string>"));
        assert!(plist.contains("<string>notes://compose?a=1&amp;b=2</string>"));
        assert_eq!(
            plist
                .matches("<key>UIApplicationShortcutItemSubtitle</key>")
                .count(),
            1
        );
        assert_eq!(
            plist.matches(&format!("<key>{IOS_URL_KEY}</key>")).count(),
            2
        );
    }

    #[test]
    fn rejects_what_the_plugin_rejects() {
        for (key, value) in [
            ("id", "has space"),
            ("id", ""),
            ("title", "  "),
            ("url", "https://example.com"),
            ("url", "compose"),
            ("url", "notes://a b"),
            ("url", "-notes://x"),
            ("icon", "icon.svg"),
            ("ios_symbol", "bad name"),
        ] {
            let mut fields = vec![("id", "a"), ("title", "A"), ("url", "notes://a")];
            fields.retain(|(k, _)| *k != key);
            fields.push((key, value));
            let body = fields.iter().fold(String::new(), |mut body, (k, v)| {
                let _ = writeln!(body, "{k} = \"{v}\"");
                body
            });
            let err = parse(&format!("[[app.launcher_action]]\n{body}")).unwrap_err();
            assert!(
                matches!(err, ManifestError::InvalidValue { .. }),
                "{key}={value:?}: {err}"
            );
        }
    }

    #[test]
    fn rejects_unknown_keys_missing_keys_and_duplicates() {
        let unknown = parse(
            "[[app.launcher_action]]\nid = \"a\"\ntitle = \"A\"\nurl = \"n://a\"\nrank = 1\n",
        );
        assert!(matches!(unknown, Err(ManifestError::UnknownKey { .. })));
        let missing = parse("[[app.launcher_action]]\nid = \"a\"\ntitle = \"A\"\n");
        assert!(matches!(missing, Err(ManifestError::Missing { .. })));
        let dup = parse(
            "[[app.launcher_action]]\nid = \"a\"\ntitle = \"A\"\nurl = \"n://a\"\n\
             [[app.launcher_action]]\nid = \"a\"\ntitle = \"B\"\nurl = \"n://b\"\n",
        );
        assert!(matches!(dup, Err(ManifestError::InvalidValue { .. })));
    }

    #[test]
    fn single_table_form_is_accepted() {
        let actions =
            parse("[app.launcher_action]\nid = \"a\"\ntitle = \"A\"\nurl = \"n://a\"\n").unwrap();
        assert_eq!(actions.iter().count(), 1);
    }

    #[test]
    fn empty_renders_nothing() {
        let actions = LauncherActions::default();
        assert_eq!(actions.render_android_meta_data(), "");
        assert_eq!(actions.render_info_plist(), "");
    }

    #[test]
    fn writes_and_cleans_android_resources() {
        let root = std::env::temp_dir().join(format!("istmo-build-la-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("assets/compose.png"), b"\x89PNG\r\n\x1a\nfake").unwrap();
        let res = root.join("res");
        let actions = parse(TWO).unwrap().rooted(&root);
        assert_eq!(actions.source_files().count(), 1);
        actions.write_android(&res, "com.example.notes").unwrap();
        assert!(res.join("xml/istmo_launcher_actions.xml").is_file());
        assert!(res.join("values/istmo_launcher_actions.xml").is_file());
        assert!(
            res.join("drawable-nodpi/istmo_launcher_action_0.png")
                .is_file()
        );
        // A stale icon of an earlier configuration goes away.
        fs::write(
            res.join("drawable-nodpi/istmo_launcher_action_7.png"),
            b"old",
        )
        .unwrap();
        actions.write_android(&res, "com.example.notes").unwrap();
        assert!(
            !res.join("drawable-nodpi/istmo_launcher_action_7.png")
                .exists()
        );
        LauncherActions::default()
            .write_android(&res, "com.example.notes")
            .unwrap();
        assert!(!res.join("xml/istmo_launcher_actions.xml").exists());
        assert!(
            !res.join("drawable-nodpi/istmo_launcher_action_0.png")
                .exists()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_icon_fails() {
        let actions = parse(TWO).unwrap().rooted(Path::new("/nonexistent-istmo"));
        let err = actions
            .write_android(&std::env::temp_dir().join("istmo-la-missing"), "x.y")
            .unwrap_err();
        assert!(matches!(err, IconError::ReadFile { .. }));
    }
}
