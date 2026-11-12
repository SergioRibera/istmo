use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::codegen::{NamedAccess, SwiftWire, local, swift_ident, write_header};
use crate::contract::{Contract, EnumDef, StructDef, TypeDef, TypeRef};

/// Codecs implementations call their own `read<T>` / `write<T>`.
const WIRE: SwiftWire = SwiftWire::new(NamedAccess::SelfImpl);

/// Locals a struct reader declares before decoding its fields.
const READER_LOCALS: &[&str] = &["c"];

#[must_use]
pub fn generate_swift_types(contract: &Contract) -> String {
    let errors = error_type_names(contract);
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out);
    let _ = writeln!(out, "import Foundation");
    if contract.references_native_handle() {
        // `NativeHandleId` is declared by the runtime module.
        let _ = writeln!(out, "import IstmoRuntime");
    }
    for ty in &contract.types {
        let _ = writeln!(out);
        let is_error = is_error_shape(ty.name()) || errors.contains(ty.name());
        match ty {
            TypeDef::Struct(s) => write_struct(&mut out, s, is_error),
            TypeDef::Enum(e) => write_enum(&mut out, e, is_error),
        }
    }
    out
}

#[must_use]
pub fn generate_swift_codecs(contract: &Contract) -> String {
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out);
    let _ = writeln!(out, "import Foundation");
    let _ = writeln!(out, "import IstmoRuntime");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "public final class {}CodecsImpl: {}Codecs {{",
        contract.type_name, contract.type_name,
    );
    let _ = writeln!(out, "    public init() {{}}");
    for ty in &contract.types {
        let _ = writeln!(out);
        match ty {
            TypeDef::Struct(s) => write_struct_codec(&mut out, s),
            TypeDef::Enum(e) => write_enum_codec(&mut out, e),
        }
    }
    if contract.references_native_handle() {
        let _ = writeln!(out);
        write_native_handle_codec(&mut out);
    }
    let _ = writeln!(out, "}}");
    out
}

/// Declared types some method throws: they must conform to `Error` for
/// the dispatcher's `catch let err as T` and the client's `throw`.
fn error_type_names(contract: &Contract) -> BTreeSet<&str> {
    contract
        .error_types()
        .into_iter()
        .filter_map(|ty| match ty {
            TypeRef::Named(name) => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

fn write_struct(out: &mut String, s: &StructDef, is_error: bool) {
    let conform = if is_error { ": Error" } else { "" };
    let _ = writeln!(out, "public struct {}{conform} {{", s.name);
    for f in &s.fields {
        let _ = writeln!(
            out,
            "    public let {}: {}",
            swift_ident(&f.name),
            f.ty.to_swift()
        );
    }
    let _ = writeln!(out);
    let init_args = s
        .fields
        .iter()
        .map(|f| format!("{}: {}", swift_ident(&f.name), f.ty.to_swift()))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "    public init({init_args}) {{");
    for f in &s.fields {
        let name = swift_ident(&f.name);
        let _ = writeln!(out, "        self.{name} = {name}");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
}

fn write_enum(out: &mut String, e: &EnumDef, is_error: bool) {
    if e.variants.iter().all(|v| v.payload.is_empty()) {
        let conform = if is_error { ", Error" } else { "" };
        let _ = writeln!(out, "public enum {}: UInt32{conform} {{", e.name);
        for (i, v) in e.variants.iter().enumerate() {
            let _ = writeln!(out, "    case {} = {i}", to_swift_case(&v.name));
        }
    } else {
        let conform = if is_error { ": Error" } else { "" };
        let _ = writeln!(out, "public enum {}{conform} {{", e.name);
        for v in &e.variants {
            let case_name = to_swift_case(&v.name);
            if v.payload.is_empty() {
                let _ = writeln!(out, "    case {case_name}");
            } else {
                let tys = v
                    .payload
                    .iter()
                    .map(TypeRef::to_swift)
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "    case {case_name}({tys})");
            }
        }
    }
    let _ = writeln!(out, "}}");
}

fn is_error_shape(name: &str) -> bool {
    name.ends_with("Error")
}

/// Swift case names are lowerCamelCase.
fn to_swift_case(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let base = first.to_lowercase().collect::<String>() + chars.as_str();
    swift_ident(&base)
}

fn write_struct_codec(out: &mut String, s: &StructDef) {
    let _ = writeln!(
        out,
        "    public func read{}(_ c: inout Bincode.Cursor) throws -> {} {{",
        s.name, s.name,
    );
    let mut init_args = Vec::with_capacity(s.fields.len());
    for f in &s.fields {
        let name = local(&f.name, READER_LOCALS, swift_ident);
        let _ = writeln!(out, "        let {name} = {}", WIRE.read(&f.ty, "c"));
        init_args.push(format!("{}: {name}", swift_ident(&f.name)));
    }
    let _ = writeln!(out, "        return {}({})", s.name, init_args.join(", "));
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "    public func write{}(_ out: inout Data, _ value: {}) {{",
        s.name, s.name,
    );
    for f in &s.fields {
        let value = format!("value.{}", swift_ident(&f.name));
        WIRE.write_stmt(out, "        ", &f.ty, "out", &value);
    }
    let _ = writeln!(out, "    }}");
}

fn write_enum_codec(out: &mut String, e: &EnumDef) {
    let has_payload = e.variants.iter().any(|v| !v.payload.is_empty());

    let _ = writeln!(
        out,
        "    public func read{}(_ c: inout Bincode.Cursor) throws -> {} {{",
        e.name, e.name,
    );
    let _ = writeln!(out, "        let disc = try Bincode.readVarintU32(&c)");
    if has_payload {
        let _ = writeln!(out, "        switch disc {{");
        for (i, v) in e.variants.iter().enumerate() {
            let case_name = to_swift_case(&v.name);
            match v.payload.as_slice() {
                [] => {
                    let _ = writeln!(out, "        case {i}: return .{case_name}");
                }
                [ty] => {
                    let _ = writeln!(
                        out,
                        "        case {i}: return .{case_name}({})",
                        WIRE.read(ty, "c")
                    );
                }
                payload => {
                    let _ = writeln!(out, "        case {i}:");
                    let mut bindings = Vec::with_capacity(payload.len());
                    for (idx, ty) in payload.iter().enumerate() {
                        let name = format!("f{idx}");
                        let _ = writeln!(out, "            let {name} = {}", WIRE.read(ty, "c"));
                        bindings.push(name);
                    }
                    let _ = writeln!(
                        out,
                        "            return .{case_name}({})",
                        bindings.join(", ")
                    );
                }
            }
        }
        let _ = writeln!(
            out,
            "        default: throw Bincode.DecodeError.invalidTag(UInt8(clamping: disc))",
        );
        let _ = writeln!(out, "        }}");
    } else {
        let _ = writeln!(
            out,
            "        guard let value = {}(rawValue: disc) else {{",
            e.name,
        );
        let _ = writeln!(
            out,
            "            throw Bincode.DecodeError.invalidTag(UInt8(clamping: disc))",
        );
        let _ = writeln!(out, "        }}");
        let _ = writeln!(out, "        return value");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "    public func write{}(_ out: inout Data, _ value: {}) {{",
        e.name, e.name,
    );
    if has_payload {
        let _ = writeln!(out, "        switch value {{");
        for (i, v) in e.variants.iter().enumerate() {
            let case_name = to_swift_case(&v.name);
            match v.payload.as_slice() {
                [] => {
                    let _ = writeln!(out, "        case .{case_name}:");
                    let _ = writeln!(out, "            Bincode.writeVarintU32(&out, {i})");
                }
                [ty] => {
                    let _ = writeln!(out, "        case .{case_name}(let payload):");
                    let _ = writeln!(out, "            Bincode.writeVarintU32(&out, {i})");
                    WIRE.write_stmt(out, "            ", ty, "out", "payload");
                }
                payload => {
                    let bindings = (0..payload.len())
                        .map(|idx| format!("let f{idx}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _ = writeln!(out, "        case .{case_name}({bindings}):");
                    let _ = writeln!(out, "            Bincode.writeVarintU32(&out, {i})");
                    for (idx, ty) in payload.iter().enumerate() {
                        WIRE.write_stmt(out, "            ", ty, "out", &format!("f{idx}"));
                    }
                }
            }
        }
        let _ = writeln!(out, "        }}");
    } else {
        let _ = writeln!(out, "        Bincode.writeVarintU32(&out, value.rawValue)");
    }
    let _ = writeln!(out, "    }}");
}

fn write_native_handle_codec(out: &mut String) {
    let _ = writeln!(
        out,
        "    public func readNativeHandleId(_ c: inout Bincode.Cursor) throws -> NativeHandleId {{",
    );
    let _ = writeln!(out, "        return try Bincode.readVarintU64(&c)");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "    public func writeNativeHandleId(_ out: inout Data, _ value: NativeHandleId) {{",
    );
    let _ = writeln!(out, "        Bincode.writeVarintU64(&out, value)");
    let _ = writeln!(out, "    }}");
}
