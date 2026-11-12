use std::fmt::Write as _;

use crate::codegen::{KotlinWire, NamedAccess, kt_ident, local, write_header};
use crate::contract::{Contract, EnumDef, StructDef, TypeDef};

/// Codecs implementations call their own `read<T>` / `write<T>`.
const WIRE: KotlinWire = KotlinWire::new(NamedAccess::SelfImpl);

/// Locals a struct reader declares before decoding its fields.
const READER_LOCALS: &[&str] = &["bytes", "offset", "cursor"];

#[must_use]
pub fn generate_kotlin_types(contract: &Contract) -> String {
    let mut out = String::new();
    write_header(&mut out, contract);
    for ty in &contract.types {
        let _ = writeln!(out);
        match ty {
            TypeDef::Struct(s) => write_struct(&mut out, s),
            TypeDef::Enum(e) => write_enum(&mut out, e),
        }
    }
    out
}

#[must_use]
pub fn generate_kotlin_codecs(contract: &Contract) -> String {
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out, "import java.io.ByteArrayOutputStream");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "class {}CodecsImpl : {}Codecs {{",
        contract.type_name, contract.type_name,
    );
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

fn write_struct(out: &mut String, s: &StructDef) {
    let _ = writeln!(out, "data class {}(", s.name);
    for (i, f) in s.fields.iter().enumerate() {
        let comma = if i + 1 == s.fields.len() { "" } else { "," };
        let _ = writeln!(
            out,
            "    val {}: {}{}",
            kt_ident(&f.name),
            f.ty.to_kotlin(),
            comma
        );
    }
    let _ = writeln!(out, ")");
}

fn write_enum(out: &mut String, e: &EnumDef) {
    if e.variants.iter().all(|v| v.payload.is_empty()) {
        let _ = writeln!(out, "enum class {} {{", e.name);
        for (i, v) in e.variants.iter().enumerate() {
            let comma = if i + 1 == e.variants.len() { "" } else { "," };
            let _ = writeln!(out, "    {}{}", kt_ident(&v.name), comma);
        }
    } else {
        let _ = writeln!(out, "sealed class {} {{", e.name);
        for v in &e.variants {
            let variant = kt_ident(&v.name);
            match v.payload.as_slice() {
                [] => {
                    let _ = writeln!(out, "    object {variant} : {}()", e.name);
                }
                [ty] => {
                    let _ = writeln!(
                        out,
                        "    data class {variant}(val value: {}) : {}()",
                        ty.to_kotlin(),
                        e.name,
                    );
                }
                payload => {
                    let fields = payload
                        .iter()
                        .enumerate()
                        .map(|(i, ty)| format!("val f{i}: {}", ty.to_kotlin()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _ = writeln!(out, "    data class {variant}({fields}) : {}()", e.name);
                }
            }
        }
    }
    let _ = writeln!(out, "}}");
}

fn write_struct_codec(out: &mut String, s: &StructDef) {
    let _ = writeln!(
        out,
        "    override fun read{}(bytes: ByteArray, offset: Int): Bincode.Decoded<{}> {{",
        s.name, s.name,
    );
    let _ = writeln!(out, "        var cursor = offset");
    let mut field_list = Vec::with_capacity(s.fields.len());
    for f in &s.fields {
        let name = local(&f.name, READER_LOCALS, kt_ident);
        WIRE.read_into(out, "        ", &f.ty, &name, "bytes");
        field_list.push(format!("{} = {name}", kt_ident(&f.name)));
    }
    let _ = writeln!(
        out,
        "        return Bincode.Decoded({}({}), cursor)",
        s.name,
        field_list.join(", ")
    );
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "    override fun write{}(out: ByteArrayOutputStream, value: {}) {{",
        s.name, s.name,
    );
    for f in &s.fields {
        let value = format!("value.{}", kt_ident(&f.name));
        WIRE.write_stmt(out, "        ", &f.ty, "out", &value);
    }
    let _ = writeln!(out, "    }}");
}

fn write_enum_codec(out: &mut String, e: &EnumDef) {
    let has_payload = e.variants.iter().any(|v| !v.payload.is_empty());

    let _ = writeln!(
        out,
        "    override fun read{}(bytes: ByteArray, offset: Int): Bincode.Decoded<{}> {{",
        e.name, e.name,
    );
    if has_payload {
        let _ = writeln!(out, "        var cursor = offset");
        let _ = writeln!(
            out,
            "        val disc = Bincode.readEnumDiscriminant(bytes, cursor)"
        );
        let _ = writeln!(out, "        cursor = disc.consumed");
        let _ = writeln!(out, "        return when (disc.value) {{");
        for (i, v) in e.variants.iter().enumerate() {
            let variant = format!("{}.{}", e.name, kt_ident(&v.name));
            if v.payload.is_empty() {
                let _ = writeln!(out, "            {i} -> Bincode.Decoded({variant}, cursor)");
                continue;
            }
            let _ = writeln!(out, "            {i} -> {{");
            let bindings: Vec<String> = if v.payload.len() == 1 {
                vec!["inner".to_owned()]
            } else {
                (0..v.payload.len()).map(|idx| format!("f{idx}")).collect()
            };
            for (binding, ty) in bindings.iter().zip(&v.payload) {
                WIRE.read_into(out, "                ", ty, binding, "bytes");
            }
            let _ = writeln!(
                out,
                "                Bincode.Decoded({variant}({}) as {}, cursor)",
                bindings.join(", "),
                e.name,
            );
            let _ = writeln!(out, "            }}");
        }
        let _ = writeln!(
            out,
            "            else -> throw Bincode.DecodeException(\"{}: unknown discriminant ${{disc.value}}\")",
            e.name,
        );
        let _ = writeln!(out, "        }}");
    } else {
        let _ = writeln!(
            out,
            "        val d = Bincode.readEnumDiscriminant(bytes, offset)"
        );
        let _ = writeln!(
            out,
            "        val variant = {}.entries.getOrNull(d.value)",
            e.name,
        );
        let _ = writeln!(
            out,
            "            ?: throw Bincode.DecodeException(\"{}: unknown discriminant ${{d.value}}\")",
            e.name,
        );
        let _ = writeln!(out, "        return Bincode.Decoded(variant, d.consumed)");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "    override fun write{}(out: ByteArrayOutputStream, value: {}) {{",
        e.name, e.name,
    );
    if has_payload {
        let _ = writeln!(out, "        when (value) {{");
        for (i, v) in e.variants.iter().enumerate() {
            let variant = format!("{}.{}", e.name, kt_ident(&v.name));
            if v.payload.is_empty() {
                let _ = writeln!(
                    out,
                    "            {variant} -> Bincode.writeEnumDiscriminant(out, {i})"
                );
                continue;
            }
            let _ = writeln!(out, "            is {variant} -> {{");
            let _ = writeln!(
                out,
                "                Bincode.writeEnumDiscriminant(out, {i})"
            );
            if let [ty] = v.payload.as_slice() {
                WIRE.write_stmt(out, "                ", ty, "out", "value.value");
            } else {
                for (idx, ty) in v.payload.iter().enumerate() {
                    WIRE.write_stmt(out, "                ", ty, "out", &format!("value.f{idx}"));
                }
            }
            let _ = writeln!(out, "            }}");
        }
        let _ = writeln!(out, "        }}");
    } else {
        let _ = writeln!(
            out,
            "        Bincode.writeEnumDiscriminant(out, value.ordinal)",
        );
    }
    let _ = writeln!(out, "    }}");
}

fn write_native_handle_codec(out: &mut String) {
    let _ = writeln!(
        out,
        "    override fun readNativeHandleId(bytes: ByteArray, offset: Int): Bincode.Decoded<NativeHandleId> {{",
    );
    let _ = writeln!(out, "        return Bincode.readVarintU64(bytes, offset)");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "    override fun writeNativeHandleId(out: ByteArrayOutputStream, value: NativeHandleId) {{",
    );
    let _ = writeln!(out, "        Bincode.writeVarintU64(out, value)");
    let _ = writeln!(out, "    }}");
}
