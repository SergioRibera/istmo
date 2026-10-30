//! Extract the plugin types an app actually wires through
//! `istmo::runtime!(plugins: [...])`.
//!
//! The permission pipeline uses the extracted list to drop permission
//! and `Info.plist` entries from plugins the app does not reference —
//! so only genuinely used plugin surfaces leave a trace in the final
//! `AndroidManifest.xml` and `Info.plist`.
//!
//! Covers the common invocation shapes:
//!
//! - `istmo::runtime!(plugins: [FooClient, bar::BazClient])` — the
//!   top-level item form.
//! - `runtime!(plugins: [...])` after a `use istmo::runtime;`.
//! - Invocations inside `fn main()` or any statement block.
//!
//! Macro invocations gated behind `#[cfg(...)]` are treated as wired
//! (the strict "pay for what you use" semantics would need to pin a
//! single target to deduce; the pipeline runs per-target at
//! `build.rs` time, which prunes already).

use std::fs;
use std::io;
use std::path::Path;

use proc_macro2::{Delimiter, Spacing, TokenTree};
use syn::{Block, Expr, File, Item, Macro, Stmt};

use crate::builtins;
use crate::manifest::{Manifest, PluginEntry};

#[derive(Debug)]
pub enum WiredError {
    Io(io::Error),
    Parse(syn::Error),
}

impl std::fmt::Display for WiredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "reading app source: {e}"),
            Self::Parse(e) => write!(f, "parsing app source: {e}"),
        }
    }
}

impl std::error::Error for WiredError {}

impl From<io::Error> for WiredError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<syn::Error> for WiredError {
    fn from(e: syn::Error) -> Self {
        Self::Parse(e)
    }
}

/// Read `source_path`, parse it with `syn`, and return the
/// de-duplicated list of plugin paths the file wires through
/// `istmo::runtime!`.
pub fn extract_wired_plugin_ids(source_path: impl AsRef<Path>) -> Result<Vec<String>, WiredError> {
    let source = fs::read_to_string(source_path.as_ref())?;
    let file: File = syn::parse_file(&source)?;
    Ok(extract_from_file(&file))
}

/// Variant that accepts an already-parsed file (useful when the
/// caller already walked the source for other extractors).
#[must_use]
pub fn extract_from_file(file: &File) -> Vec<String> {
    let mut out = Vec::new();
    for item in &file.items {
        collect_from_item(item, &mut out);
    }
    dedup_preserve_order(out)
}

fn collect_from_item(item: &Item, out: &mut Vec<String>) {
    match item {
        Item::Macro(m) => visit_macro(&m.mac, out),
        Item::Fn(f) => collect_from_block(&f.block, out),
        Item::Mod(m) => {
            if let Some((_, items)) = &m.content {
                for inner in items {
                    collect_from_item(inner, out);
                }
            }
        }
        Item::Const(c) => collect_from_expr(&c.expr, out),
        Item::Static(s) => collect_from_expr(&s.expr, out),
        _ => {}
    }
}

fn collect_from_block(block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Macro(m) => visit_macro(&m.mac, out),
            Stmt::Expr(expr, _) => collect_from_expr(expr, out),
            Stmt::Local(l) => {
                if let Some(init) = &l.init {
                    collect_from_expr(&init.expr, out);
                }
            }
            Stmt::Item(inner) => collect_from_item(inner, out),
        }
    }
}

fn collect_from_expr(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Macro(m) => visit_macro(&m.mac, out),
        Expr::Block(b) => collect_from_block(&b.block, out),
        Expr::If(e) => {
            collect_from_expr(&e.cond, out);
            collect_from_block(&e.then_branch, out);
            if let Some((_, els)) = &e.else_branch {
                collect_from_expr(els, out);
            }
        }
        Expr::Loop(e) => collect_from_block(&e.body, out),
        Expr::ForLoop(e) => collect_from_block(&e.body, out),
        Expr::While(e) => collect_from_block(&e.body, out),
        Expr::Unsafe(e) => collect_from_block(&e.block, out),
        _ => {}
    }
}

fn visit_macro(mac: &Macro, out: &mut Vec<String>) {
    if !is_runtime_macro(mac) {
        return;
    }
    out.extend(paths_from_runtime_tokens(mac.tokens.clone()));
}

fn is_runtime_macro(mac: &Macro) -> bool {
    let segments: Vec<_> = mac
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    match segments.len() {
        0 => false,
        1 => segments[0] == "runtime",
        n => segments[n - 1] == "runtime" && segments[n - 2] == "istmo",
    }
}

/// Walk the macro's token stream, find the `plugins :` section, and
/// return every path inside the `[...]` group.
fn paths_from_runtime_tokens(tokens: proc_macro2::TokenStream) -> Vec<String> {
    let mut iter = tokens.into_iter().peekable();
    while let Some(tt) = iter.next() {
        let TokenTree::Ident(id) = &tt else { continue };
        if id != "plugins" {
            continue;
        }
        let Some(TokenTree::Punct(colon)) = iter.peek() else {
            continue;
        };
        if colon.as_char() != ':' || colon.spacing() == Spacing::Joint {
            continue;
        }
        iter.next();
        let Some(TokenTree::Group(group)) = iter.next() else {
            return Vec::new();
        };
        if group.delimiter() != Delimiter::Bracket {
            return Vec::new();
        }
        return parse_path_list(group.stream());
    }
    Vec::new()
}

fn parse_path_list(tokens: proc_macro2::TokenStream) -> Vec<String> {
    let mut paths = Vec::new();
    let mut current = String::new();
    for tt in tokens {
        match tt {
            TokenTree::Punct(p) if p.as_char() == ',' => flush(&mut current, &mut paths),
            TokenTree::Ident(id) => current.push_str(&id.to_string()),
            TokenTree::Punct(p) => current.push(p.as_char()),
            TokenTree::Literal(_) | TokenTree::Group(_) => {
                // Generics / inline expressions — not supported; the
                // path up to this point is still a valid prefix.
            }
        }
    }
    flush(&mut current, &mut paths);
    paths
}

fn flush(current: &mut String, out: &mut Vec<String>) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_owned());
    }
    current.clear();
}

fn dedup_preserve_order(items: Vec<String>) -> Vec<String> {
    let mut seen = Vec::new();
    for item in items {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
}

/// Cross-reference the `wired_paths` with every known plugin set.
///
/// The known set is builtins + every standalone plugin reachable
/// through `DEP_*_ISTMO_MANIFEST`. An entry is kept only if it
/// appears in the app's `istmo::runtime!` invocation OR the
/// auto-wiring env vars (`ISTMO_AUTO_PLUGINS`, `ISTMO_AUTO_REMOTE`)
/// that [`crate::manifest::emit_wiring_env`] populates.
///
/// Facade re-exports (`use istmo::plugins::NetworkStateClient` vs.
/// `use istmo_plugins::NetworkStateClient` vs. the fully-qualified
/// `::istmo_plugins::NetworkStateClient`) all resolve the same way:
/// the last `::`-separated segment is the match key, so the user's
/// `plugins: [NetworkStateClient]` only has to agree on the
/// identifier, not the module path.
///
/// Plugins whose `client_type` is `None` ship unconditionally:
/// they expose no client surface the app can list in `runtime!`,
/// so their presence in a dep manifest is itself the opt-in signal.
///
/// The env-var union is critical: a transitive dep's `build.rs` may
/// register a plugin that the app itself never names in `runtime!`.
/// Trusting only the syn-extracted idents would under-report the
/// wired set and strip a permission the auto-wired plugin needs at
/// runtime.
///
/// Matching is by the client-type's last `::`-separated segment —
/// `FooClient` written in the app's source matches
/// `::istmo_plugins::FooClient` as well as a dep-manifest plugin
/// whose `client_type` ends with `::FooClient`. Collisions on the
/// last segment are rare in istmo plugin surfaces; if two plugins
/// both end in `FooClient`, both will be included, which is the
/// safe default (over-include permissions rather than silently
/// drop one).
#[must_use]
pub fn resolve_wired_entries(
    dep_manifests: &[Manifest],
    wired_paths: &[String],
) -> Vec<PluginEntry> {
    let mut all_paths: Vec<String> = wired_paths.to_vec();
    all_paths.extend(paths_from_auto_env("ISTMO_AUTO_PLUGINS"));
    all_paths.extend(paths_from_auto_env("ISTMO_AUTO_REMOTE"));
    let wanted: Vec<&str> = all_paths.iter().map(|p| last_segment(p)).collect();

    let mut all: Vec<PluginEntry> = builtins::entries();
    for m in dep_manifests {
        all.extend(m.plugins.iter().cloned());
    }

    let mut selected = Vec::new();
    let mut seen_ids = Vec::new();
    for entry in all {
        let keep = match entry.client_type.as_deref() {
            // Plugins with no client type carry no wire-level surface
            // the app can `runtime!`-list — include unconditionally
            // so service-only / background-only plugins ship when
            // present, matching pre-filter behavior.
            None => true,
            Some(ct) => wanted.contains(&last_segment(ct)),
        };
        if !keep {
            continue;
        }
        if seen_ids.contains(&entry.id) {
            continue;
        }
        seen_ids.push(entry.id.clone());
        selected.push(entry);
    }
    selected
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path).trim()
}

fn paths_from_auto_env(key: &str) -> Vec<String> {
    std::env::var(key)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Serialize the `<uses-permission>` entries from the resolved
/// plugin set into an Android manifest fragment ready for Android
/// Gradle Plugin's manifest merger.
///
/// Output shape:
///
/// ```xml
/// <?xml version="1.0" encoding="utf-8"?>
/// <manifest xmlns:android="http://schemas.android.com/apk/res/android">
///   <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE"/>
/// </manifest>
/// ```
///
/// Returns an empty document (zero `<uses-permission>` children) when
/// no entry declares any permission; downstream Gradle integration
/// can skip inclusion in that case to avoid an empty merge
/// contribution.
#[must_use]
pub fn render_android_permissions_manifest(entries: &[PluginEntry]) -> String {
    let mut perms: Vec<&crate::manifest::AndroidPermission> = entries
        .iter()
        .flat_map(|e| e.android_permissions.iter())
        .collect();
    perms.sort();
    perms.dedup();

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\">\n");
    for perm in &perms {
        out.push_str("    <uses-permission android:name=\"");
        out.push_str(&xml_escape(&perm.name));
        out.push('"');
        if let Some(max_sdk) = perm.max_sdk_version {
            out.push_str(" android:maxSdkVersion=\"");
            out.push_str(&max_sdk.to_string());
            out.push('"');
        }
        out.push_str("/>\n");
    }
    out.push_str("</manifest>\n");
    out
}

/// Write [`render_android_permissions_manifest`] to a conventional
/// path under the app's Android project (`<android_app_dir>/src/main/
/// istmo-permissions.xml`). Creates the directory tree if needed.
///
/// `android_app_dir` is the `android/app/` directory of the consuming
/// Gradle project.
///
/// # Errors
///
/// Returns the underlying `io::Error` when directory creation or the
/// write itself fails.
pub fn write_android_permissions_sidecar(
    android_app_dir: impl AsRef<Path>,
    entries: &[PluginEntry],
) -> io::Result<std::path::PathBuf> {
    let dest = android_app_dir
        .as_ref()
        .join("src")
        .join("main")
        .join("istmo-permissions.xml");
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&dest, render_android_permissions_manifest(entries))?;
    Ok(dest)
}

fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(src: &str) -> Vec<String> {
        extract_from_file(&syn::parse_file(src).expect("parse"))
    }

    #[test]
    fn top_level_invocation() {
        let s = "istmo::runtime!(plugins: [FooClient, BarClient]);";
        assert_eq!(extract(s), vec!["FooClient", "BarClient"]);
    }

    #[test]
    fn qualified_paths() {
        let s = "istmo::runtime!(plugins: [a::b::FooClient, crate::bar::BazClient]);";
        assert_eq!(extract(s), vec!["a::b::FooClient", "crate::bar::BazClient"]);
    }

    #[test]
    fn root_rooted_paths() {
        let s = "istmo::runtime!(plugins: [::istmo_plugins::NetworkStateClient]);";
        assert_eq!(extract(s), vec!["::istmo_plugins::NetworkStateClient"]);
    }

    #[test]
    fn mixed_with_other_sections() {
        let s = r#"
            istmo::runtime!(
                plugins: [FooClient],
                remote: [BazClient],
                expects: [some::Trait],
            );
        "#;
        assert_eq!(extract(s), vec!["FooClient"]);
    }

    #[test]
    fn invocation_inside_fn() {
        let s = r#"
            fn main() {
                istmo::runtime!(plugins: [A, B]);
            }
        "#;
        assert_eq!(extract(s), vec!["A", "B"]);
    }

    #[test]
    fn short_form_after_use() {
        let s = r#"
            use istmo::runtime;
            runtime!(plugins: [X]);
        "#;
        assert_eq!(extract(s), vec!["X"]);
    }

    #[test]
    fn merges_duplicates_across_calls() {
        let s = r#"
            #[cfg(target_os = "android")]
            istmo::runtime!(plugins: [FooClient]);
            #[cfg(not(target_os = "android"))]
            istmo::runtime!(plugins: [FooClient, BarClient]);
        "#;
        assert_eq!(extract(s), vec!["FooClient", "BarClient"]);
    }

    #[test]
    fn no_runtime_macro_yields_empty() {
        let s = "fn main() { println!(\"hi\"); }";
        assert!(extract(s).is_empty());
    }

    #[test]
    fn plugins_section_absent_yields_empty() {
        let s = "istmo::runtime!(expects: [X]);";
        assert!(extract(s).is_empty());
    }

    #[test]
    fn non_istmo_runtime_macro_ignored() {
        let s = "tokio::runtime!(plugins: [NotMine]);";
        assert!(extract(s).is_empty());
    }

    #[test]
    fn resolve_matches_builtin_by_last_segment() {
        let selected = resolve_wired_entries(
            &[],
            &vec!["NetworkStateClient".to_owned(), "SystemInfo".to_owned()],
        );
        let ids: Vec<_> = selected.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"istmo.network_state"));
        assert!(ids.contains(&"istmo.system_info"));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn resolve_handles_qualified_path() {
        let selected =
            resolve_wired_entries(&[], &vec!["::istmo_plugins::BatteryStateClient".to_owned()]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "istmo.battery_state");
    }

    #[test]
    fn resolve_excludes_unwired_builtins() {
        let selected = resolve_wired_entries(&[], &vec!["SystemInfo".to_owned()]);
        let ids: Vec<_> = selected.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["istmo.system_info"]);
    }

    #[test]
    fn resolve_empty_wired_list_yields_empty() {
        let selected = resolve_wired_entries(&[], &[]);
        assert!(selected.is_empty());
    }

    #[test]
    fn render_empty_manifest_has_no_permissions() {
        let out = render_android_permissions_manifest(&[]);
        assert!(out.contains("<manifest"));
        assert!(!out.contains("<uses-permission"));
    }

    #[test]
    fn render_sorts_and_dedupes() {
        use crate::manifest::AndroidPermission;
        let a = PluginEntry {
            id: "a".into(),
            android_permissions: vec![AndroidPermission::new("android.permission.INTERNET")],
            ..PluginEntry::default()
        };
        let b = PluginEntry {
            id: "b".into(),
            android_permissions: vec![
                AndroidPermission::new("android.permission.ACCESS_NETWORK_STATE"),
                AndroidPermission::new("android.permission.INTERNET"),
            ],
            ..PluginEntry::default()
        };
        let out = render_android_permissions_manifest(&[a, b]);
        // Alphabetical: ACCESS_NETWORK_STATE before INTERNET
        let network_pos = out.find("ACCESS_NETWORK_STATE").unwrap();
        let internet_pos = out.find("INTERNET").unwrap();
        assert!(network_pos < internet_pos);
        // INTERNET appears exactly once (not duplicated)
        assert_eq!(out.matches("INTERNET").count(), 1);
    }

    #[test]
    fn render_escapes_xml_specials() {
        use crate::manifest::AndroidPermission;
        let entry = PluginEntry {
            id: "x".into(),
            android_permissions: vec![AndroidPermission::new("a&b<c")],
            ..PluginEntry::default()
        };
        let out = render_android_permissions_manifest(&[entry]);
        assert!(out.contains("a&amp;b&lt;c"));
    }

    #[test]
    fn render_includes_max_sdk_version() {
        use crate::manifest::AndroidPermission;
        let entry = PluginEntry {
            id: "x".into(),
            android_permissions: vec![AndroidPermission::with_max_sdk(
                "android.permission.WRITE_EXTERNAL_STORAGE",
                28,
            )],
            ..PluginEntry::default()
        };
        let out = render_android_permissions_manifest(&[entry]);
        assert!(out.contains("android:maxSdkVersion=\"28\""));
        assert!(out.contains("WRITE_EXTERNAL_STORAGE"));
    }
}
