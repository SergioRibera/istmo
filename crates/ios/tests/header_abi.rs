//! Keeps `runtime/ios/Sources/IstmoFFI/include/istmo_ffi.h` (what Swift
//! compiles against) in sync with `src/cdecl_exports.rs` (what the
//! staticlib exports). Plain text checks: the callback table's field
//! names, order and per-callback arity, and every exported function's
//! name and arity.

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn header_path() -> PathBuf {
    manifest_dir().join("../../runtime/ios/Sources/IstmoFFI/include/istmo_ffi.h")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Drop `//` and `/* */` comments so prose never matches as code.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |end| &after[end + 2..]);
        } else if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |end| &after[end..]);
        } else {
            let ch = rest.chars().next().expect("non-empty");
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

/// Number of parameters in the list starting right after an opening
/// parenthesis. `void` alone counts as zero.
fn arity(after_open: &str) -> usize {
    let mut depth = 0usize;
    let mut end = after_open.len();
    for (i, ch) in after_open.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => {
                end = i;
                break;
            }
            ')' => depth -= 1,
            _ => {}
        }
    }
    after_open[..end]
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "void")
        .count()
}

fn block<'a>(src: &'a str, opener: &str) -> &'a str {
    let start = src
        .find(opener)
        .unwrap_or_else(|| panic!("`{opener}` not found"))
        + opener.len();
    let len = src[start..].find('}').expect("closing brace");
    &src[start..start + len]
}

/// `(field, callback arity)` in declaration order; `ctx` has no arity.
fn header_fields(header: &str) -> Vec<(String, Option<usize>)> {
    block(header, "typedef struct IstmoIosCallbacks {")
        .split(';')
        .map(str::trim)
        .filter(|decl| !decl.is_empty())
        .map(|decl| {
            let (ty, name) = decl
                .rsplit_once(char::is_whitespace)
                .expect("type and name");
            let name = name.trim_start_matches('*').to_owned();
            let ty = ty.trim();
            let arity = (ty != "void").then(|| {
                let typedef = format!("(*{ty})(");
                let at = header
                    .find(&typedef)
                    .unwrap_or_else(|| panic!("typedef {ty} not found"));
                arity(&header[at + typedef.len()..])
            });
            (name, arity)
        })
        .collect()
}

fn rust_fields(rust: &str) -> Vec<(String, Option<usize>)> {
    block(rust, "pub struct IstmoIosCallbacks {")
        .split(',')
        .map(str::trim)
        .filter(|decl| !decl.is_empty())
        .map(|decl| {
            let decl = decl.strip_prefix("pub ").expect("public field");
            let (name, ty) = decl.split_once(':').expect("name: type");
            let ty = ty.trim();
            let arity = ty
                .strip_prefix("Option<")
                .and_then(|t| t.strip_suffix('>'))
                .map(|alias| {
                    let def = format!("pub type {alias} =");
                    let at = rust
                        .find(&def)
                        .unwrap_or_else(|| panic!("alias {alias} not found"));
                    let open = rust[at..].find("fn(").expect("fn pointer alias") + at + 3;
                    arity(&rust[open..])
                });
            (name.trim().to_owned(), arity)
        })
        .collect()
}

/// `(name, arity)` of every `#[unsafe(no_mangle)]` function.
fn rust_exports(rust: &str) -> Vec<(String, usize)> {
    rust.split("#[unsafe(no_mangle)]")
        .skip(1)
        .map(|item| {
            let fn_at = item.find("fn ").expect("exported fn") + 3;
            let open = item[fn_at..].find('(').expect("parameter list") + fn_at;
            (
                item[fn_at..open].trim().to_owned(),
                arity(&item[open + 1..]),
            )
        })
        .collect()
}

fn header_function(header: &str, name: &str) -> Option<usize> {
    // Declarations sit at the start of a line: `<ret> name(`.
    header.lines().enumerate().find_map(|(i, line)| {
        let open = format!(" {name}(");
        line.find(&open).map(|at| {
            let offset: usize = header.lines().take(i).map(|l| l.len() + 1).sum();
            arity(&header[offset + at + open.len()..])
        })
    })
}

#[test]
fn header_matches_rust_exports() {
    let header_path = header_path();
    if !header_path.exists() {
        // Published crate: the Swift package is not part of the tarball.
        return;
    }
    let header = strip_comments(&read(&header_path));
    let rust = strip_comments(&read(&manifest_dir().join("src/cdecl_exports.rs")));

    assert_eq!(
        header_fields(&header),
        rust_fields(&rust),
        "IstmoIosCallbacks fields (name, callback arity) differ between istmo_ffi.h and cdecl_exports.rs",
    );

    let exports = rust_exports(&rust);
    assert!(!exports.is_empty(), "no #[unsafe(no_mangle)] exports found");
    for (name, rust_arity) in &exports {
        let header_arity = header_function(&header, name)
            .unwrap_or_else(|| panic!("`{name}` is exported but not declared in istmo_ffi.h"));
        assert_eq!(
            header_arity, *rust_arity,
            "`{name}` parameter count differs"
        );
    }

    let declared = header
        .lines()
        .filter_map(|line| {
            let start = line.find("istmo_ios_")?;
            let len = line[start..].find('(')?;
            Some(line[start..start + len].to_owned())
        })
        .collect::<Vec<_>>();
    for name in declared {
        assert!(
            exports.iter().any(|(n, _)| *n == name),
            "`{name}` is declared in istmo_ffi.h but not exported by cdecl_exports.rs",
        );
    }
}
