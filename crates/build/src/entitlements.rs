use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntitlementValue {
    Bool(bool),
    String(String),
    StringArray(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IosEntitlements {
    entries: BTreeMap<String, EntitlementValue>,
}

impl IosEntitlements {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_bool(&mut self, key: impl Into<String>, value: bool) -> &mut Self {
        self.entries
            .insert(key.into(), EntitlementValue::Bool(value));
        self
    }

    pub fn add_string(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.entries
            .insert(key.into(), EntitlementValue::String(value.into()));
        self
    }

    pub fn add_string_array(
        &mut self,
        key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> &mut Self {
        self.entries.insert(
            key.into(),
            EntitlementValue::StringArray(values.into_iter().map(Into::into).collect()),
        );
        self
    }

    pub fn merge(&mut self, other: Self) -> &mut Self {
        self.entries.extend(other.entries);
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        let _ = writeln!(
            out,
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">"
        );
        let _ = writeln!(out, "<plist version=\"1.0\">");
        let _ = writeln!(out, "<dict>");
        for (key, value) in &self.entries {
            let _ = writeln!(out, "    <key>{}</key>", escape_xml(key));
            match value {
                EntitlementValue::Bool(true) => {
                    let _ = writeln!(out, "    <true/>");
                }
                EntitlementValue::Bool(false) => {
                    let _ = writeln!(out, "    <false/>");
                }
                EntitlementValue::String(s) => {
                    let _ = writeln!(out, "    <string>{}</string>", escape_xml(s));
                }
                EntitlementValue::StringArray(values) => {
                    let _ = writeln!(out, "    <array>");
                    for v in values {
                        let _ = writeln!(out, "        <string>{}</string>", escape_xml(v));
                    }
                    let _ = writeln!(out, "    </array>");
                }
            }
        }
        let _ = writeln!(out, "</dict>");
        let _ = writeln!(out, "</plist>");
        out
    }
}

fn escape_xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

