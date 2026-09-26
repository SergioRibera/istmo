//! Per-plugin `Info.plist` / entitlements fragments for Apple targets.
//!
//! A plugin that needs keys in its host app's `Info.plist` (usage
//! descriptions, URL schemes, document types, …) or entitlements (App
//! Groups, keychain sharing, …) ships them as ordinary property lists
//! next to its Swift sources:
//!
//! ```text
//! plugins/share/native/ios/Info.plist.fragment
//! plugins/share/native/ios/App.entitlements.fragment
//! ```
//!
//! The `.fragment` suffix keeps Xcode from treating them as bundle
//! resources; the generated xcodegen include also excludes them from
//! the app target explicitly.
//!
//! [`emit_app`](crate::emit_app()) merges every fragment reachable
//! through the `DEP_*_ISTMO_NATIVE_IOS` handover with [`PlistFragments`]
//! and writes the result into the app between marker comments:
//!
//! ```xml
//! <dict>
//!     …app keys…
//!     <!-- istmo:plugins:start -->
//!     <!-- istmo:plugins:end -->
//! </dict>
//! ```
//!
//! A sidecar (`Info.plist.plugins.xml` / `App.entitlements.plugins.xml`)
//! is always written so apps without the markers can copy the keys by
//! hand.
//!
//! # Merge rules
//!
//! * dictionaries merge recursively;
//! * arrays concatenate, skipping values already present;
//! * conflicting scalars keep the first plugin's value (plugins are
//!   visited in crate-name order) and emit a Cargo warning;
//! * keys the app already defines outside the managed block are
//!   skipped with a warning — the app always wins.

use std::fmt;
use std::path::{Path, PathBuf};

use plist::{Dictionary, Value};

/// File name of the `Info.plist` fragment inside `native/ios/`.
pub const INFO_PLIST_FRAGMENT: &str = "Info.plist.fragment";

/// File name of the entitlements fragment inside `native/ios/`.
pub const ENTITLEMENTS_FRAGMENT: &str = "App.entitlements.fragment";

/// Marker comment opening the istmo-managed block.
pub const PLUGINS_MARK_START: &str = "<!-- istmo:plugins:start -->";

/// Marker comment closing the istmo-managed block.
pub const PLUGINS_MARK_END: &str = "<!-- istmo:plugins:end -->";

/// Failure while reading or merging a fragment.
#[derive(Debug)]
pub enum PlistFragmentError {
    Read { path: PathBuf, error: plist::Error },
    NotADictionary { path: PathBuf },
    Render(plist::Error),
}

impl fmt::Display for PlistFragmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, error } => write!(f, "reading {}: {error}", path.display()),
            Self::NotADictionary { path } => {
                write!(
                    f,
                    "{}: top-level plist value must be a <dict>",
                    path.display()
                )
            }
            Self::Render(error) => write!(f, "rendering merged plist: {error}"),
        }
    }
}

impl std::error::Error for PlistFragmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { error, .. } | Self::Render(error) => Some(error),
            Self::NotADictionary { .. } => None,
        }
    }
}

/// Non-fatal merge diagnostic, surfaced as a `cargo::warning`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeWarning {
    /// Two plugins set the same scalar key to different values.
    Conflict {
        key_path: String,
        kept_from: String,
        ignored_from: String,
    },
    /// The app already defines `key` outside the managed block.
    ShadowedByApp { key: String },
}

impl fmt::Display for MergeWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict {
                key_path,
                kept_from,
                ignored_from,
            } => write!(
                f,
                "plist key `{key_path}` set by both `{kept_from}` and `{ignored_from}`; \
                 keeping `{kept_from}`'s value"
            ),
            Self::ShadowedByApp { key } => write!(
                f,
                "plist key `{key}` is already defined by the app outside the istmo block; \
                 merge the plugin's value by hand"
            ),
        }
    }
}

/// Accumulates fragments from several plugins into one dictionary.
#[derive(Debug, Default)]
pub struct PlistFragments {
    merged: Dictionary,
    /// Which plugin set each top-level key first — for conflict
    /// messages.
    owners: Vec<(String, String)>,
    warnings: Vec<MergeWarning>,
}

impl PlistFragments {
    /// Parse `path` and merge it under the name `plugin`.
    pub fn add_file(&mut self, plugin: &str, path: &Path) -> Result<(), PlistFragmentError> {
        let value = Value::from_file(path).map_err(|error| PlistFragmentError::Read {
            path: path.to_path_buf(),
            error,
        })?;
        let Value::Dictionary(dict) = value else {
            return Err(PlistFragmentError::NotADictionary {
                path: path.to_path_buf(),
            });
        };
        self.add(plugin, dict);
        Ok(())
    }

    /// Merge an already-parsed fragment under the name `plugin`.
    pub fn add(&mut self, plugin: &str, fragment: Dictionary) {
        for (key, value) in fragment {
            let owner = self.owner_of(&key).unwrap_or(plugin).to_owned();
            if let Some(existing) = self.merged.get_mut(&key) {
                merge_value(existing, value, &key, &owner, plugin, &mut self.warnings);
            } else {
                self.owners.push((key.clone(), plugin.to_owned()));
                self.merged.insert(key, value);
            }
        }
    }

    fn owner_of(&self, key: &str) -> Option<&str> {
        self.owners
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, owner)| owner.as_str())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.merged.is_empty()
    }

    /// Drop keys the app already defines outside the managed block.
    pub fn remove_app_keys(&mut self, app_keys: &[String]) {
        for key in app_keys {
            if self.merged.remove(key).is_some() {
                self.warnings
                    .push(MergeWarning::ShadowedByApp { key: key.clone() });
            }
        }
    }

    #[must_use]
    pub fn warnings(&self) -> &[MergeWarning] {
        &self.warnings
    }

    #[must_use]
    pub const fn merged(&self) -> &Dictionary {
        &self.merged
    }

    /// Render the merged keys as the `<key>…</key><value/>` sequence
    /// that goes *inside* a `<dict>`, indented one level.
    pub fn render_body(&self) -> Result<String, PlistFragmentError> {
        let mut buf = Vec::new();
        Value::Dictionary(self.merged.clone())
            .to_writer_xml(&mut buf)
            .map_err(PlistFragmentError::Render)?;
        let xml = String::from_utf8_lossy(&buf);
        let body = xml
            .split_once("<dict>")
            .and_then(|(_, rest)| rest.rsplit_once("</dict>"))
            .map_or("", |(inner, _)| inner);
        let mut out = String::with_capacity(body.len());
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            out.push_str(line);
            out.push('\n');
        }
        Ok(out)
    }
}

fn merge_value(
    existing: &mut Value,
    incoming: Value,
    key_path: &str,
    owner: &str,
    plugin: &str,
    warnings: &mut Vec<MergeWarning>,
) {
    match (existing, incoming) {
        (Value::Dictionary(current), Value::Dictionary(next)) => {
            for (key, value) in next {
                let path = format!("{key_path}.{key}");
                match current.get_mut(&key) {
                    Some(slot) => merge_value(slot, value, &path, owner, plugin, warnings),
                    None => {
                        current.insert(key, value);
                    }
                }
            }
        }
        (Value::Array(current), Value::Array(next)) => {
            for value in next {
                if !current.contains(&value) {
                    current.push(value);
                }
            }
        }
        (current, next) => {
            if *current != next {
                warnings.push(MergeWarning::Conflict {
                    key_path: key_path.to_owned(),
                    kept_from: owner.to_owned(),
                    ignored_from: plugin.to_owned(),
                });
            }
        }
    }
}

/// Top-level keys defined in `source` outside the istmo-managed block.
///
/// Returns an empty list when the file cannot be parsed — the merge
/// then proceeds and Xcode reports any duplicate itself.
#[must_use]
pub fn app_keys_outside_block(source: &str) -> Vec<String> {
    let without_block = match (
        source.find(PLUGINS_MARK_START),
        source.find(PLUGINS_MARK_END),
    ) {
        (Some(start), Some(end)) if end > start => {
            format!(
                "{}{}",
                &source[..start],
                &source[end + PLUGINS_MARK_END.len()..]
            )
        }
        _ => source.to_owned(),
    };
    match Value::from_reader_xml(without_block.as_bytes()) {
        Ok(Value::Dictionary(dict)) => dict.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(xml_body: &str) -> Dictionary {
        let doc = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <plist version=\"1.0\"><dict>{xml_body}</dict></plist>"
        );
        match Value::from_reader_xml(doc.as_bytes()).expect("parse") {
            Value::Dictionary(d) => d,
            other => panic!("not a dict: {other:?}"),
        }
    }

    #[test]
    fn merges_dicts_and_arrays_across_plugins() {
        let mut frags = PlistFragments::default();
        frags.add(
            "istmo.share",
            dict(
                "<key>NSPhotoLibraryAddUsageDescription</key><string>Save shared images</string>\
                 <key>LSApplicationQueriesSchemes</key><array><string>mailto</string></array>",
            ),
        );
        frags.add(
            "istmo.other",
            dict(
                "<key>LSApplicationQueriesSchemes</key>\
                 <array><string>mailto</string><string>sms</string></array>",
            ),
        );
        let schemes = frags.merged()["LSApplicationQueriesSchemes"]
            .as_array()
            .expect("array");
        assert_eq!(schemes.len(), 2);
        assert!(frags.warnings().is_empty());
    }

    #[test]
    fn scalar_conflict_keeps_first_and_warns() {
        let mut frags = PlistFragments::default();
        frags.add("a", dict("<key>K</key><string>first</string>"));
        frags.add("b", dict("<key>K</key><string>second</string>"));
        assert_eq!(frags.merged()["K"].as_string(), Some("first"));
        assert_eq!(
            frags.warnings(),
            [MergeWarning::Conflict {
                key_path: "K".to_owned(),
                kept_from: "a".to_owned(),
                ignored_from: "b".to_owned(),
            }]
        );
    }

    #[test]
    fn app_keys_win_over_fragments() {
        let app = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
             <key>K</key><string>app</string>{PLUGINS_MARK_START}\
             <key>Managed</key><string>old</string>{PLUGINS_MARK_END}</dict></plist>"
        );
        let keys = app_keys_outside_block(&app);
        assert_eq!(keys, ["K"]);
        let mut frags = PlistFragments::default();
        frags.add(
            "a",
            dict("<key>K</key><string>plugin</string><key>Managed</key><string>new</string>"),
        );
        frags.remove_app_keys(&keys);
        assert!(frags.merged().get("K").is_none());
        assert_eq!(frags.merged()["Managed"].as_string(), Some("new"));
    }

    #[test]
    fn renders_body_without_outer_dict() {
        let mut frags = PlistFragments::default();
        frags.add("a", dict("<key>K</key><true/>"));
        let body = frags.render_body().expect("render");
        assert!(body.contains("<key>K</key>"), "{body}");
        assert!(!body.contains("<plist"), "{body}");
        assert!(!body.contains("<dict>"), "{body}");
    }
}
