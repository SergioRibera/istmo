//! Keeps `runtime/ios/Sources/IstmoFFI/include/istmo_ffi.h` (what Swift
//! compiles against) in sync with `src/cdecl_exports.rs` (what the
//! staticlib exports). Plain text checks: the callback table's field
//! names, order and types (callback parameter and return types
//! included), and every exported function's name, parameter types and
//! return type. C types are mapped onto their Rust spelling; a type
//! neither side knows fails the test instead of slipping through.

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

/// Parameters of the list starting right after an opening parenthesis,
/// plus the text after its closing one. `void` alone is an empty list.
fn split_params(after_open: &str) -> (Vec<&str>, &str) {
    let mut depth = 0usize;
    let mut end = after_open.len();
    for (i, ch) in after_open.char_indices() {
        match ch {
            '(' | '<' => depth += 1,
            ')' if depth == 0 => {
                end = i;
                break;
            }
            ')' | '>' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let params = after_open[..end]
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "void")
        .collect();
    (params, after_open.get(end + 1..).unwrap_or(""))
}

/// Rust spelling of a C type written without a parameter name.
fn c_to_rust(ty: &str) -> String {
    let spaced = ty.replace('*', " * ");
    let ty = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    match ty.as_str() {
        "void" => "()",
        "void *" => "*mut c_void",
        "const uint8_t *" => "*const u8",
        "bool" => "bool",
        "uint32_t" => "u32",
        "uint64_t" => "u64",
        "size_t" => "usize",
        "IstmoIosCallbacks" => "IstmoIosCallbacks",
        other => panic!("C type `{other}` has no Rust mapping in this test"),
    }
    .to_owned()
}

/// Rust spelling of a named C parameter such as `const uint8_t *payload`.
fn c_param(param: &str) -> String {
    let spaced = param.replace('*', " * ");
    let mut tokens = spaced.split_whitespace().collect::<Vec<_>>();
    assert!(tokens.len() > 1, "C parameter `{param}` has no name");
    tokens.pop();
    c_to_rust(&tokens.join(" "))
}

/// Type of a Rust parameter `name: ty`, whitespace normalised.
fn rust_param(param: &str) -> String {
    let (_, ty) = param
        .split_once(':')
        .unwrap_or_else(|| panic!("Rust parameter `{param}` is not `name: type`"));
    normalise(ty)
}

fn normalise(ty: &str) -> String {
    ty.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Return type following a Rust parameter list, `()` when absent.
fn rust_return(after_close: &str) -> String {
    let sig = after_close
        .split(['{', ';'])
        .next()
        .expect("split yields one item");
    sig.split_once("->")
        .map_or_else(|| "()".to_owned(), |(_, ret)| normalise(ret))
}

/// A function signature as `(parameter types, return type)`.
type Sig = (Vec<String>, String);

#[derive(Debug, PartialEq, Eq)]
enum Field {
    Plain(String),
    Callback(Sig),
}

fn block<'a>(src: &'a str, opener: &str) -> &'a str {
    let start = src
        .find(opener)
        .unwrap_or_else(|| panic!("`{opener}` not found"))
        + opener.len();
    let len = src[start..].find('}').expect("closing brace");
    &src[start..start + len]
}

fn header_typedef(header: &str, name: &str) -> Sig {
    let marker = format!("(*{name})(");
    let at = header
        .find(&marker)
        .unwrap_or_else(|| panic!("typedef {name} not found"));
    let typedef = header[..at].rfind("typedef").expect("typedef keyword") + "typedef".len();
    let ret = c_to_rust(&header[typedef..at]);
    let (params, _) = split_params(&header[at + marker.len()..]);
    (params.into_iter().map(c_param).collect(), ret)
}

fn header_fields(header: &str) -> Vec<(String, Field)> {
    block(header, "typedef struct IstmoIosCallbacks {")
        .split(';')
        .map(str::trim)
        .filter(|decl| !decl.is_empty())
        .map(|decl| {
            let (ty, name) = decl
                .rsplit_once(char::is_whitespace)
                .expect("type and name");
            let stars = name.len() - name.trim_start_matches('*').len();
            let name = name.trim_start_matches('*').to_owned();
            let ty = format!("{}{}", ty.trim(), " *".repeat(stars));
            let field = if ty.starts_with("Istmo") && ty.ends_with("Fn") {
                Field::Callback(header_typedef(header, &ty))
            } else {
                Field::Plain(c_to_rust(&ty))
            };
            (name, field)
        })
        .collect()
}

fn rust_alias(rust: &str, alias: &str) -> Sig {
    let def = format!("pub type {alias} =");
    let at = rust
        .find(&def)
        .unwrap_or_else(|| panic!("alias {alias} not found"));
    let open = rust[at..].find("fn(").expect("fn pointer alias") + at + 3;
    let (params, tail) = split_params(&rust[open..]);
    (
        params.into_iter().map(rust_param).collect(),
        rust_return(tail),
    )
}

fn rust_fields(rust: &str) -> Vec<(String, Field)> {
    let body = block(rust, "pub struct IstmoIosCallbacks {");
    let (fields, _) = split_params(body);
    fields
        .into_iter()
        .map(|decl| {
            let decl = decl.strip_prefix("pub ").expect("public field");
            let (name, ty) = decl.split_once(':').expect("name: type");
            let ty = normalise(ty);
            let field = ty
                .strip_prefix("Option<")
                .and_then(|t| t.strip_suffix('>'))
                .map_or_else(
                    || Field::Plain(ty.clone()),
                    |alias| Field::Callback(rust_alias(rust, alias)),
                );
            (name.trim().to_owned(), field)
        })
        .collect()
}

/// `(name, signature)` of every `#[unsafe(no_mangle)]` function.
fn rust_exports(rust: &str) -> Vec<(String, Sig)> {
    rust.split("#[unsafe(no_mangle)]")
        .skip(1)
        .map(|item| {
            let fn_at = item.find("fn ").expect("exported fn") + 3;
            let open = item[fn_at..].find('(').expect("parameter list") + fn_at;
            let (params, tail) = split_params(&item[open + 1..]);
            (
                item[fn_at..open].trim().to_owned(),
                (
                    params.into_iter().map(rust_param).collect(),
                    rust_return(tail),
                ),
            )
        })
        .collect()
}

fn header_function(header: &str, name: &str) -> Option<Sig> {
    // Declarations sit at the start of a line: `<ret> name(`.
    let open = format!(" {name}(");
    let mut offset = 0;
    for line in header.lines() {
        if let Some(at) = line.find(&open) {
            let ret = c_to_rust(&line[..at]);
            let (params, _) = split_params(&header[offset + at + open.len()..]);
            return Some((params.into_iter().map(c_param).collect(), ret));
        }
        offset += line.len() + 1;
    }
    None
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
        "IstmoIosCallbacks fields (name, type) differ between istmo_ffi.h and cdecl_exports.rs",
    );

    let exports = rust_exports(&rust);
    assert!(!exports.is_empty(), "no #[unsafe(no_mangle)] exports found");
    for (name, rust_sig) in &exports {
        let header_sig = header_function(&header, name)
            .unwrap_or_else(|| panic!("`{name}` is exported but not declared in istmo_ffi.h"));
        assert_eq!(&header_sig, rust_sig, "`{name}` signature differs");
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

#[test]
fn c_types_map_onto_rust_spelling() {
    assert_eq!(c_param("const uint8_t *payload"), "*const u8");
    assert_eq!(c_param("void *ctx"), "*mut c_void");
    assert_eq!(c_param("uint64_t call_id"), "u64");
    assert_eq!(rust_param("payload_len: usize"), "usize");
    assert_eq!(rust_return(" -> bool {"), "bool");
    assert_eq!(rust_return(" {"), "()");
}
