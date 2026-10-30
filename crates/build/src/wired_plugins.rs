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
pub fn extract_wired_plugin_ids(
    source_path: impl AsRef<Path>,
) -> Result<Vec<String>, WiredError> {
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
    let segments: Vec<_> = mac.path.segments.iter().map(|s| s.ident.to_string()).collect();
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
        assert_eq!(
            extract(s),
            vec!["a::b::FooClient", "crate::bar::BazClient"]
        );
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
}
