//! `[[app.document_type]]` — the file types an app opens.
//!
//! One declaration feeds every platform:
//!
//! - **Android** — `<intent-filter>` children for the launcher
//!   activity ([`DocumentTypes::render_android_intent_filters`]):
//!   `VIEW` + `SEND` by MIME type, plus a path-pattern `VIEW` filter for
//!   the extensions of exported types (custom types usually arrive as
//!   `application/octet-stream`).
//! - **Apple** — `CFBundleDocumentTypes`, `UTExportedTypeDeclarations`,
//!   `UTImportedTypeDeclarations` and `LSSupportsOpeningDocumentsInPlace`
//!   ([`DocumentTypes::render_info_plist`]).
//! - **Desktop** — packaging tools (`.desktop` `MimeType=`, Windows
//!   `ProgIDs`, the macOS `.app` plist) read the same table straight from
//!   `istmo.toml`; delivery at run time goes through the
//!   `istmo.documents` plugin.
//!
//! ```toml
//! [[app.document_type]]
//! name = "PDF document"
//! id = "com.adobe.pdf"
//! extensions = ["pdf"]
//! mime_types = ["application/pdf"]
//!
//! [[app.document_type]]
//! name = "Notes document"
//! id = "com.example.notes.document"
//! extensions = ["note"]
//! mime_types = ["application/x-note"]
//! role = "editor"
//! rank = "owner"
//! exported = true
//! ```

use std::fmt::Write as _;

use toml_edit::{Item, TableLike, Value};

use crate::manifest::{ManifestError, expect_bool, expect_string, expect_table};

const KNOWN_KEYS: &[&str] = &[
    "id",
    "name",
    "extensions",
    "mime_types",
    "role",
    "rank",
    "exported",
    "conforms_to",
];

/// Uniform type identifiers the system already declares; importing
/// them again is redundant.
const SYSTEM_UTIS: &[&str] = &[
    "com.adobe.pdf",
    "public.data",
    "public.content",
    "public.text",
    "public.plain-text",
    "public.utf8-plain-text",
    "public.image",
    "public.png",
    "public.jpeg",
    "public.html",
    "public.json",
    "public.xml",
];

const DEFAULT_CONFORMS_TO: &str = "public.data";

/// What the app does with a document type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DocumentRole {
    #[default]
    Viewer,
    Editor,
}

impl DocumentRole {
    fn parse(literal: &str) -> Option<Self> {
        match literal {
            "viewer" => Some(Self::Viewer),
            "editor" => Some(Self::Editor),
            _ => None,
        }
    }

    const fn apple(self) -> &'static str {
        match self {
            Self::Viewer => "Viewer",
            Self::Editor => "Editor",
        }
    }
}

/// How strongly the app claims a document type among the installed
/// apps that open it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DocumentRank {
    /// The app created the type (only for `exported` types).
    Owner,
    /// The app wants to be the default handler.
    Default,
    /// The app opens the type but should not become its default.
    #[default]
    Alternate,
}

impl DocumentRank {
    fn parse(literal: &str) -> Option<Self> {
        match literal {
            "owner" => Some(Self::Owner),
            "default" => Some(Self::Default),
            "alternate" => Some(Self::Alternate),
            _ => None,
        }
    }

    const fn apple(self) -> &'static str {
        match self {
            Self::Owner => "Owner",
            Self::Default => "Default",
            Self::Alternate => "Alternate",
        }
    }
}

/// One `[[app.document_type]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentType {
    /// Uniform type identifier (Apple) and `ProgID` base (Windows).
    pub id: Option<String>,
    /// Human-readable name shown by the system.
    pub name: String,
    /// File extensions without the leading dot.
    pub extensions: Vec<String>,
    pub mime_types: Vec<String>,
    pub role: DocumentRole,
    pub rank: DocumentRank,
    /// The app defines this type (it is not a system or third-party type).
    pub exported: bool,
    /// Apple parent types of an exported or imported declaration.
    pub conforms_to: Vec<String>,
}

impl DocumentType {
    fn parse(table: &dyn TableLike) -> Result<Self, ManifestError> {
        for (name, _) in table.iter() {
            if !KNOWN_KEYS.contains(&name) {
                return Err(ManifestError::UnknownKey {
                    key: format!("app.document_type.{name}"),
                });
            }
        }
        let name = table
            .get("name")
            .ok_or(ManifestError::Missing {
                key: "app.document_type.name",
            })
            .and_then(|item| expect_string(item, "app.document_type.name"))?
            .to_owned();
        let id = table
            .get("id")
            .map(|item| expect_string(item, "app.document_type.id").map(str::to_owned))
            .transpose()?;
        let extensions = string_list(table.get("extensions"), "app.document_type.extensions")?;
        if let Some(bad) = extensions
            .iter()
            .find(|ext| ext.is_empty() || ext.starts_with('.') || ext.contains(['/', '\\', '*']))
        {
            return Err(invalid(
                "app.document_type.extensions",
                bad,
                "an extension without the leading dot, such as `pdf`",
            ));
        }
        let mime_types = string_list(table.get("mime_types"), "app.document_type.mime_types")?;
        if let Some(bad) = mime_types.iter().find(|mime| !is_mime_type(mime)) {
            return Err(invalid(
                "app.document_type.mime_types",
                bad,
                "a `type/subtype` MIME type",
            ));
        }
        if extensions.is_empty() && mime_types.is_empty() {
            return Err(ManifestError::Missing {
                key: "app.document_type.extensions",
            });
        }
        let role = enum_key(
            table,
            "role",
            "app.document_type.role",
            DocumentRole::parse,
            "`viewer` or `editor`",
        )?;
        let rank = enum_key(
            table,
            "rank",
            "app.document_type.rank",
            DocumentRank::parse,
            "`owner`, `default` or `alternate`",
        )?;
        let exported = table
            .get("exported")
            .map(|item| expect_bool(item, "app.document_type.exported"))
            .transpose()?
            .unwrap_or(false);
        let conforms_to = match table.get("conforms_to") {
            Some(item) => string_list(Some(item), "app.document_type.conforms_to")?,
            None => vec![DEFAULT_CONFORMS_TO.to_owned()],
        };
        if exported {
            if id.is_none() {
                return Err(ManifestError::Missing {
                    key: "app.document_type.id",
                });
            }
            if extensions.is_empty() {
                return Err(ManifestError::Missing {
                    key: "app.document_type.extensions",
                });
            }
        } else if rank == DocumentRank::Owner {
            return Err(invalid(
                "app.document_type.rank",
                "owner",
                "`owner` only on `exported = true` types",
            ));
        }
        Ok(Self {
            id,
            name,
            extensions,
            mime_types,
            role,
            rank,
            exported,
            conforms_to,
        })
    }

    /// Whether the Apple side must declare the type itself.
    fn needs_apple_declaration(&self) -> bool {
        self.id
            .as_deref()
            .is_some_and(|id| self.exported || !SYSTEM_UTIS.contains(&id))
    }
}

/// Every `[[app.document_type]]` of an app, in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentTypes(Vec<DocumentType>);

impl DocumentTypes {
    /// Parse `app.document_type`, written as one table or an array of
    /// tables.
    pub fn parse(item: &Item) -> Result<Self, ManifestError> {
        if let Some(array) = item.as_array_of_tables() {
            return array
                .iter()
                .map(|table| DocumentType::parse(table))
                .collect::<Result<_, _>>()
                .map(Self);
        }
        let table = expect_table(item, "app.document_type")?;
        DocumentType::parse(table).map(|entry| Self(vec![entry]))
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &DocumentType> {
        self.0.iter()
    }

    /// `<intent-filter>` elements for the activity that receives
    /// documents, or an empty string when no type is declared.
    #[must_use]
    pub fn render_android_intent_filters(&self) -> String {
        let mut mimes: Vec<&str> = self
            .iter()
            .flat_map(|entry| entry.mime_types.iter().map(String::as_str))
            .collect();
        mimes.sort_unstable();
        mimes.dedup();
        let mut exported_exts: Vec<&str> = self
            .iter()
            .filter(|entry| entry.exported)
            .flat_map(|entry| entry.extensions.iter().map(String::as_str))
            .collect();
        exported_exts.sort_unstable();
        exported_exts.dedup();

        let mut out = String::new();
        if !mimes.is_empty() {
            for action in ["VIEW", "SEND"] {
                out.push_str("<intent-filter>\n");
                let _ = writeln!(
                    out,
                    "    <action android:name=\"android.intent.action.{action}\"/>"
                );
                out.push_str("    <category android:name=\"android.intent.category.DEFAULT\"/>\n");
                if action == "VIEW" {
                    out.push_str("    <data android:scheme=\"content\"/>\n");
                    out.push_str("    <data android:scheme=\"file\"/>\n");
                }
                for mime in &mimes {
                    let _ = writeln!(
                        out,
                        "    <data android:mimeType=\"{}\"/>",
                        crate::xml::escape(mime)
                    );
                }
                out.push_str("</intent-filter>\n");
            }
        }
        if !exported_exts.is_empty() {
            // Files with a custom extension usually come in as
            // `application/octet-stream` or without a MIME type, so the
            // path is the only thing to match. `pathPattern` is not
            // greedy: one pattern per extra dot in the file name.
            out.push_str("<intent-filter>\n");
            out.push_str("    <action android:name=\"android.intent.action.VIEW\"/>\n");
            out.push_str("    <category android:name=\"android.intent.category.DEFAULT\"/>\n");
            out.push_str("    <data android:scheme=\"content\"/>\n");
            out.push_str("    <data android:scheme=\"file\"/>\n");
            out.push_str("    <data android:host=\"*\"/>\n");
            out.push_str("    <data android:mimeType=\"*/*\"/>\n");
            for ext in &exported_exts {
                let ext = crate::xml::escape(ext);
                for dots in 0..4 {
                    let prefix = ".*\\\\.".repeat(dots);
                    let _ = writeln!(
                        out,
                        "    <data android:pathPattern=\"{prefix}.*\\\\.{ext}\"/>"
                    );
                }
            }
            out.push_str("</intent-filter>\n");
        }
        out
    }

    /// `Info.plist` `<key>`/value pairs (no enclosing `<dict>`), or an
    /// empty string when no type is declared.
    #[must_use]
    pub fn render_info_plist(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::from("<key>CFBundleDocumentTypes</key>\n<array>\n");
        for entry in self.iter() {
            out.push_str("    <dict>\n");
            plist_string(&mut out, 2, "CFBundleTypeName", &entry.name);
            plist_string(&mut out, 2, "CFBundleTypeRole", entry.role.apple());
            plist_string(&mut out, 2, "LSHandlerRank", entry.rank.apple());
            if let Some(id) = &entry.id {
                plist_array(&mut out, 2, "LSItemContentTypes", std::slice::from_ref(id));
            } else {
                plist_array(&mut out, 2, "CFBundleTypeExtensions", &entry.extensions);
                plist_array(&mut out, 2, "CFBundleTypeMIMETypes", &entry.mime_types);
            }
            out.push_str("    </dict>\n");
        }
        out.push_str("</array>\n");
        for (key, exported) in [
            ("UTExportedTypeDeclarations", true),
            ("UTImportedTypeDeclarations", false),
        ] {
            let declared: Vec<&DocumentType> = self
                .iter()
                .filter(|entry| entry.exported == exported && entry.needs_apple_declaration())
                .collect();
            if declared.is_empty() {
                continue;
            }
            let _ = writeln!(out, "<key>{key}</key>\n<array>");
            for entry in declared {
                out.push_str("    <dict>\n");
                if let Some(id) = &entry.id {
                    plist_string(&mut out, 2, "UTTypeIdentifier", id);
                }
                plist_string(&mut out, 2, "UTTypeDescription", &entry.name);
                plist_array(&mut out, 2, "UTTypeConformsTo", &entry.conforms_to);
                out.push_str("        <key>UTTypeTagSpecification</key>\n        <dict>\n");
                plist_array(&mut out, 3, "public.filename-extension", &entry.extensions);
                plist_array(&mut out, 3, "public.mime-type", &entry.mime_types);
                out.push_str("        </dict>\n    </dict>\n");
            }
            out.push_str("</array>\n");
        }
        if self.iter().any(|entry| entry.role == DocumentRole::Editor) {
            out.push_str("<key>LSSupportsOpeningDocumentsInPlace</key>\n<true/>\n");
        }
        out
    }
}

fn plist_string(out: &mut String, depth: usize, key: &str, value: &str) {
    let indent = "    ".repeat(depth);
    let _ = writeln!(
        out,
        "{indent}<key>{key}</key>\n{indent}<string>{}</string>",
        crate::xml::escape(value)
    );
}

fn plist_array(out: &mut String, depth: usize, key: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    let indent = "    ".repeat(depth);
    let _ = writeln!(out, "{indent}<key>{key}</key>\n{indent}<array>");
    for value in values {
        let _ = writeln!(
            out,
            "{indent}    <string>{}</string>",
            crate::xml::escape(value)
        );
    }
    let _ = writeln!(out, "{indent}</array>");
}

fn string_list(item: Option<&Item>, key: &'static str) -> Result<Vec<String>, ManifestError> {
    let Some(item) = item else {
        return Ok(Vec::new());
    };
    let mismatch = || ManifestError::TypeMismatch {
        key: key.to_owned(),
        expected: "array of strings",
    };
    item.as_array()
        .ok_or_else(mismatch)?
        .iter()
        .map(|value| match value {
            Value::String(s) => Ok(s.value().clone()),
            _ => Err(mismatch()),
        })
        .collect()
}

/// Optional string key restricted to the literals `parse` accepts.
fn enum_key<T: Default>(
    table: &dyn TableLike,
    name: &str,
    key: &'static str,
    parse: fn(&str) -> Option<T>,
    expected: &'static str,
) -> Result<T, ManifestError> {
    let Some(item) = table.get(name) else {
        return Ok(T::default());
    };
    let literal = expect_string(item, key)?;
    parse(literal).ok_or_else(|| invalid(key, literal, expected))
}

fn is_mime_type(raw: &str) -> bool {
    raw.split_once('/').is_some_and(|(kind, sub)| {
        !kind.is_empty() && !sub.is_empty() && !raw.contains(char::is_whitespace)
    })
}

fn invalid(key: &str, value: &str, expected: &'static str) -> ManifestError {
    ManifestError::InvalidValue {
        key: key.to_owned(),
        value: value.to_owned(),
        expected,
    }
}

#[cfg(test)]
mod tests {
    use toml_edit::DocumentMut;

    use super::*;

    fn parse(source: &str) -> Result<DocumentTypes, ManifestError> {
        let doc: DocumentMut = source.parse().unwrap();
        DocumentTypes::parse(&doc["app"]["document_type"])
    }

    const SAMPLE: &str = r#"
[[app.document_type]]
name = "PDF document"
id = "com.adobe.pdf"
extensions = ["pdf"]
mime_types = ["application/pdf"]

[[app.document_type]]
name = "Markdown"
id = "net.daringfireball.markdown"
extensions = ["md", "markdown"]
mime_types = ["text/markdown"]
role = "editor"

[[app.document_type]]
name = "Notes document"
id = "com.example.notes.document"
extensions = ["note"]
mime_types = ["application/x-note"]
role = "editor"
rank = "owner"
exported = true
"#;

    #[test]
    fn parses_array_of_tables() {
        let types = parse(SAMPLE).unwrap();
        let entries: Vec<_> = types.iter().collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].role, DocumentRole::Viewer);
        assert_eq!(entries[0].rank, DocumentRank::Alternate);
        assert_eq!(entries[0].conforms_to, ["public.data"]);
        assert_eq!(entries[2].rank, DocumentRank::Owner);
        assert!(entries[2].exported);
    }

    #[test]
    fn parses_single_table() {
        let types = parse("[app.document_type]\nname = \"PDF\"\nextensions = [\"pdf\"]\n").unwrap();
        assert_eq!(types.iter().count(), 1);
    }

    #[test]
    fn rejects_unknown_key() {
        let err = parse("[app.document_type]\nname = \"x\"\nextensions = [\"x\"]\nicon = \"a\"\n")
            .unwrap_err();
        assert!(
            matches!(err, ManifestError::UnknownKey { key } if key == "app.document_type.icon")
        );
    }

    #[test]
    fn rejects_dotted_extension() {
        let err =
            parse("[app.document_type]\nname = \"x\"\nextensions = [\".pdf\"]\n").unwrap_err();
        assert!(matches!(err, ManifestError::InvalidValue { .. }));
    }

    #[test]
    fn rejects_bad_mime() {
        let err = parse("[app.document_type]\nname = \"x\"\nmime_types = [\"pdf\"]\n").unwrap_err();
        assert!(matches!(err, ManifestError::InvalidValue { .. }));
    }

    #[test]
    fn requires_extension_or_mime() {
        let err = parse("[app.document_type]\nname = \"x\"\n").unwrap_err();
        assert!(matches!(err, ManifestError::Missing { .. }));
    }

    #[test]
    fn exported_requires_id() {
        let err =
            parse("[app.document_type]\nname = \"x\"\nextensions = [\"x\"]\nexported = true\n")
                .unwrap_err();
        assert!(matches!(err, ManifestError::Missing { key } if key == "app.document_type.id"));
    }

    #[test]
    fn owner_requires_exported() {
        let err =
            parse("[app.document_type]\nname = \"x\"\nextensions = [\"x\"]\nrank = \"owner\"\n")
                .unwrap_err();
        assert!(matches!(err, ManifestError::InvalidValue { .. }));
    }

    #[test]
    fn android_filters_cover_mimes_and_exported_extensions() {
        let xml = parse(SAMPLE).unwrap().render_android_intent_filters();
        assert!(xml.contains("android.intent.action.VIEW"));
        assert!(xml.contains("android.intent.action.SEND"));
        assert!(xml.contains("android:mimeType=\"application/pdf\""));
        assert!(xml.contains("android:mimeType=\"text/markdown\""));
        assert!(xml.contains("android:pathPattern=\".*\\\\.note\""));
        assert!(xml.contains("android:pathPattern=\".*\\\\..*\\\\.note\""));
        assert!(
            !xml.contains("\\\\.pdf\""),
            "only exported types match by path"
        );
    }

    #[test]
    fn info_plist_declares_documents_and_types() {
        let plist = parse(SAMPLE).unwrap().render_info_plist();
        assert!(plist.contains("<key>CFBundleDocumentTypes</key>"));
        assert!(plist.contains("<string>com.adobe.pdf</string>"));
        assert!(plist.contains("<key>UTExportedTypeDeclarations</key>"));
        assert!(plist.contains("<key>UTImportedTypeDeclarations</key>"));
        assert!(plist.contains("<string>net.daringfireball.markdown</string>"));
        assert!(plist.contains("<key>LSSupportsOpeningDocumentsInPlace</key>"));
        let imported = plist.split("UTImportedTypeDeclarations").nth(1).unwrap();
        assert!(
            !imported.contains("com.adobe.pdf"),
            "system types are not re-imported"
        );
    }

    #[test]
    fn empty_set_renders_nothing() {
        let types = DocumentTypes::default();
        assert_eq!(types.render_android_intent_filters(), "");
        assert_eq!(types.render_info_plist(), "");
    }
}
