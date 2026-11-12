//! `[plugin.info_plist]` / `[app.info_plist]` entries.

use toml_edit::{Item, Value};
use wincode::{SchemaRead, SchemaWrite};

use super::ManifestError;

/// Scalar value of an [`InfoPlistEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, SchemaWrite, SchemaRead)]
pub enum InfoPlistValue {
    String(String),
    Bool(bool),
}

/// One top-level `<key>` / value pair contributed to an iOS app's
/// `Info.plist`, declared under `[plugin.info_plist]` (plugin crates)
/// or `[app.info_plist]` (apps, where it overrides plugin defaults).
#[derive(Debug, Clone, PartialEq, Eq, Hash, SchemaWrite, SchemaRead)]
pub struct InfoPlistEntry {
    pub key: String,
    pub value: InfoPlistValue,
}

impl InfoPlistEntry {
    /// Render the entry as the `<key>…</key>` + value element pair that
    /// goes inside the root `<dict>` of an `Info.plist`.
    #[must_use]
    pub fn render(&self) -> String {
        let value = match &self.value {
            InfoPlistValue::String(s) => format!("<string>{}</string>", crate::xml::escape(s)),
            InfoPlistValue::Bool(true) => "<true/>".to_owned(),
            InfoPlistValue::Bool(false) => "<false/>".to_owned(),
        };
        format!("<key>{}</key>\n{value}\n", crate::xml::escape(&self.key))
    }

    /// Parse every key of an `info_plist` TOML table. `context` is the
    /// dotted path used in error messages (`plugin.info_plist`,
    /// `app.info_plist`).
    pub fn parse_table(
        table: &dyn toml_edit::TableLike,
        context: &str,
    ) -> Result<Vec<Self>, ManifestError> {
        table
            .iter()
            .map(|(key, item)| {
                let value = match item {
                    Item::Value(Value::String(s)) => InfoPlistValue::String(s.value().clone()),
                    Item::Value(Value::Boolean(b)) => InfoPlistValue::Bool(*b.value()),
                    _ => {
                        return Err(ManifestError::TypeMismatch {
                            key: format!("{context}.{key}"),
                            expected: "string or boolean",
                        });
                    }
                };
                Ok(Self {
                    key: key.to_owned(),
                    value,
                })
            })
            .collect()
    }
}
