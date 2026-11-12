use std::fmt::Write as _;

use crate::codegen::{
    KotlinWire, NamedAccess, ReadSource, fresh, kt_ident, kt_string, write_header,
};
use crate::contract::{Contract, Method, MethodKind, TypeRef};

const WIRE: KotlinWire = KotlinWire::new(NamedAccess::Codecs);

#[must_use]
pub fn generate_kotlin_client(contract: &Contract) -> String {
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out);
    let _ = writeln!(out, "import java.io.ByteArrayOutputStream");
    if contract.has_stream() {
        let _ = writeln!(out, "import kotlinx.coroutines.flow.map");
    }
    let _ = writeln!(out);
    let name = &contract.type_name;
    let _ = writeln!(
        out,
        "/// Rust-hosted `{name}` plugin. Ships each call across the frame",
    );
    let _ = writeln!(out, "/// protocol via `IstmoRuntime.call(...)`.");
    if contract.signatures_use_named() {
        let _ = writeln!(out, "class {name}Client(");
        let _ = writeln!(out, "    private val codecs: {name}Codecs,");
        let _ = writeln!(out, ") {{");
    } else {
        let _ = writeln!(out, "class {name}Client {{");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "    companion object {{");
    let _ = writeln!(
        out,
        "        const val PLUGIN_ID = {}",
        kt_string(&contract.plugin_id),
    );
    let _ = writeln!(out, "    }}");
    for method in &contract.methods {
        let _ = writeln!(out);
        write_client_method(&mut out, contract, method);
    }
    let _ = writeln!(out, "}}");
    for err in contract.error_types() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "/// Kotlin-side view of a domain error decoded from `Frame::Respond {{ Err(bytes) }}`.",
        );
        let _ = writeln!(out, "class {}(", contract.exception_name(err));
        let _ = writeln!(out, "    val payload: ByteArray,");
        let _ = writeln!(out, "    val decoded: {}?,", err.to_kotlin());
        let _ = writeln!(out, ") : RuntimeException()");
    }
    out
}

/// Locals a client method declares. Each avoids the method's parameter
/// names so a contract argument called `payload` or `cursor` is never
/// shadowed or misread.
struct Locals {
    payload_buf: String,
    payload: String,
    bytes: String,
    cursor: String,
    result: String,
}

impl Locals {
    fn for_method(method: &Method) -> Self {
        let names: Vec<&str> = method.args.iter().map(|a| a.name.as_str()).collect();
        Self {
            payload_buf: fresh("payloadBuf", &names),
            payload: fresh("payload", &names),
            bytes: fresh("bytes", &names),
            cursor: fresh("cursor", &names),
            result: fresh("result", &names),
        }
    }

    fn source(&self) -> ReadSource<'_> {
        ReadSource {
            bytes: &self.bytes,
            cursor: &self.cursor,
        }
    }
}

fn write_client_method(out: &mut String, contract: &Contract, method: &Method) {
    let arg_sig = format_args(method);
    let ret_kt = method.returns.to_kotlin();
    let fn_name = kt_ident(&method.name);
    let wire_name = kt_string(&method.name);
    let locals = Locals::for_method(method);
    let Locals {
        payload,
        bytes,
        cursor,
        result,
        ..
    } = &locals;
    match method.kind {
        MethodKind::Unary => {
            let returns_unit = method.returns.is_unit();
            let sig_ret = if returns_unit {
                String::new()
            } else {
                format!(": {ret_kt}")
            };
            let _ = writeln!(out, "    suspend fun {fn_name}({arg_sig}){sig_ret} {{");
            write_payload_build(out, method, &locals, "        ");
            let assign = if returns_unit {
                String::new()
            } else {
                format!("val {bytes} = ")
            };
            let call = format!("IstmoRuntime.call(PLUGIN_ID, {wire_name}, {payload})");
            if let Some(err) = &method.error {
                let decode = WIRE.read(err, "e.payload", "0");
                let _ = writeln!(out, "        {assign}try {{");
                let _ = writeln!(out, "            {call}");
                let _ = writeln!(out, "        }} catch (e: PluginException) {{");
                let _ = writeln!(
                    out,
                    "            val decoded = runCatching {{ {decode}.value }}.getOrNull()",
                );
                let _ = writeln!(
                    out,
                    "            throw {}(e.payload, decoded)",
                    contract.exception_name(err),
                );
                let _ = writeln!(out, "        }}");
            } else {
                let _ = writeln!(out, "        {assign}{call}");
            }
            if !returns_unit {
                let _ = writeln!(out, "        var {cursor} = 0");
                WIRE.read_into(out, "        ", &method.returns, result, locals.source());
                let _ = writeln!(out, "        return {result}");
            }
            let _ = writeln!(out, "    }}");
        }
        MethodKind::Stream => {
            let _ = writeln!(
                out,
                "    fun {fn_name}({arg_sig}): kotlinx.coroutines.flow.Flow<{ret_kt}> {{",
            );
            write_payload_build(out, method, &locals, "        ");
            let _ = writeln!(
                out,
                "        return IstmoRuntime.stream(PLUGIN_ID, {wire_name}, {payload}).map {{ {bytes} ->",
            );
            write_stream_map_body(out, &method.returns, &locals, "            ");
            let _ = writeln!(out, "        }}");
            let _ = writeln!(out, "    }}");
        }
    }
}

/// Emits the `payload` local holding the encoded arguments.
fn write_payload_build(out: &mut String, method: &Method, locals: &Locals, indent: &str) {
    let Locals {
        payload_buf,
        payload,
        ..
    } = locals;
    if method.args.is_empty() {
        let _ = writeln!(out, "{indent}val {payload} = ByteArray(0)");
        return;
    }
    let _ = writeln!(out, "{indent}val {payload_buf} = ByteArrayOutputStream()");
    for arg in &method.args {
        WIRE.write_stmt(out, indent, &arg.ty, payload_buf, &kt_ident(&arg.name));
    }
    let _ = writeln!(out, "{indent}val {payload} = {payload_buf}.toByteArray()");
}

fn write_stream_map_body(out: &mut String, ret: &TypeRef, locals: &Locals, indent: &str) {
    if ret.is_unit() {
        let _ = writeln!(out, "{indent}Unit");
        return;
    }
    let _ = writeln!(out, "{indent}var {} = 0", locals.cursor);
    WIRE.read_into(out, indent, ret, &locals.result, locals.source());
    let _ = writeln!(out, "{indent}{}", locals.result);
}

fn format_args(method: &Method) -> String {
    method
        .args
        .iter()
        .map(|a| format!("{}: {}", kt_ident(&a.name), a.ty.to_kotlin()))
        .collect::<Vec<_>>()
        .join(", ")
}
