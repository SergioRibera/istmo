use std::fmt::Write as _;

use crate::codegen::{
    KotlinWire, NamedAccess, ReadSource, kt_ident, kt_string, local, write_header,
};
use crate::contract::{Contract, Method, MethodKind};

const WIRE: KotlinWire = KotlinWire::new(NamedAccess::Codecs);

/// Names the generated `handleCall` / `handleCreateInstance` bodies
/// already bind; decoded arguments with the same name get renamed.
const DISPATCH_LOCALS: &[&str] = &[
    "backend",
    "codecs",
    "config",
    "cursor",
    "e",
    "err",
    "errOut",
    "factory",
    "id",
    "instanceId",
    "instances",
    "method",
    "nextInstanceId",
    "out",
    "payload",
    "result",
];

#[must_use]
pub fn generate_kotlin_host(contract: &Contract) -> String {
    let mut out = String::new();
    write_file_header(&mut out, contract);
    write_backend_interface(&mut out, contract);
    let _ = writeln!(out);
    write_codecs_interface(&mut out, contract);
    let _ = writeln!(out);
    write_dispatcher_class(&mut out, contract);
    out
}

#[must_use]
pub fn generate_kotlin_codecs_interface(contract: &Contract) -> String {
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out);
    let _ = writeln!(out, "import java.io.ByteArrayOutputStream");
    let _ = writeln!(out);
    write_codecs_interface(&mut out, contract);
    out
}

fn write_file_header(out: &mut String, contract: &Contract) {
    write_header(out, contract);
    let _ = writeln!(out);
    let _ = writeln!(out, "import java.io.ByteArrayOutputStream");
    if contract.has_stream() {
        if contract.has_failing_stream() {
            let _ = writeln!(out, "import kotlinx.coroutines.flow.catch");
        }
        let _ = writeln!(out, "import kotlinx.coroutines.flow.map");
    }
    let _ = writeln!(out);
}

fn write_backend_interface(out: &mut String, contract: &Contract) {
    let _ = writeln!(
        out,
        "/// Platform-facing surface for the `{}` plugin. Hand-write an",
        contract.type_name,
    );
    let _ = writeln!(
        out,
        "/// impl with the SDK-specific logic; the dispatcher below handles",
    );
    let _ = writeln!(out, "/// wire encode/decode.");
    let _ = writeln!(out, "interface {}Backend {{", contract.type_name);
    for (i, method) in contract.methods.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        write_backend_method(out, method);
    }
    let _ = writeln!(out, "}}");
}

fn write_backend_method(out: &mut String, method: &Method) {
    if let Some(err) = &method.error {
        let how = match method.kind {
            MethodKind::Unary => "for domain errors",
            MethodKind::Stream => "from the flow to end it with a domain error",
        };
        let _ = writeln!(
            out,
            "    // Throw BackendException({}) {how}.",
            err.to_kotlin()
        );
    }
    let _ = writeln!(out, "    {}", backend_method_decl(method));
}

/// Declaration of `method` on the `<T>Backend` interface, shared with
/// the seeded implementation so the two cannot drift apart.
pub(crate) fn backend_method_decl(method: &Method) -> String {
    let args = format_args(method);
    let name = kt_ident(&method.name);
    match method.kind {
        MethodKind::Unary if method.returns.is_unit() => format!("suspend fun {name}({args})"),
        MethodKind::Unary => format!("suspend fun {name}({args}): {}", method.returns.to_kotlin()),
        MethodKind::Stream => format!(
            "fun {name}({args}): kotlinx.coroutines.flow.Flow<{}>",
            method.returns.to_kotlin()
        ),
    }
}

fn format_args(method: &Method) -> String {
    method
        .args
        .iter()
        .map(|a| format!("{}: {}", kt_ident(&a.name), a.ty.to_kotlin()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_codecs_interface(out: &mut String, contract: &Contract) {
    let types = contract.codec_type_names();
    let _ = writeln!(
        out,
        "/// Reader / writer per `Named` type referenced by the `{}` contract.",
        contract.type_name,
    );
    let _ = writeln!(
        out,
        "/// User provides one impl; each entry typically delegates to a",
    );
    let _ = writeln!(out, "/// companion helper on the type itself.");
    let _ = writeln!(out, "interface {}Codecs {{", contract.type_name);
    if types.is_empty() {
        let _ = writeln!(
            out,
            "    // No named types in this contract; nothing to codegen.",
        );
    }
    for (i, ty) in types.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        let _ = writeln!(
            out,
            "    fun read{ty}(bytes: ByteArray, offset: Int): Bincode.Decoded<{ty}>",
        );
        let _ = writeln!(
            out,
            "    fun write{ty}(out: ByteArrayOutputStream, value: {ty})"
        );
    }
    let _ = writeln!(out, "}}");
}

fn write_dispatcher_class(out: &mut String, contract: &Contract) {
    let name = &contract.type_name;
    let _ = writeln!(
        out,
        "/// Decodes wire calls, invokes `{name}Backend`, encodes the response.",
    );
    let _ = writeln!(
        out,
        "/// Register with `IstmoRuntime.registerHandler(PLUGIN_ID, dispatcher)`."
    );

    if contract.init.is_some() {
        write_stateful_dispatcher(out, contract);
    } else {
        write_stateless_dispatcher(out, contract);
    }
}

fn dispatcher_bases(contract: &Contract) -> &'static str {
    if contract.references_native_handle() {
        "PluginHandler, HandleReleaser"
    } else {
        "PluginHandler"
    }
}

fn write_companion(out: &mut String, contract: &Contract) {
    let _ = writeln!(out, "    companion object {{");
    let _ = writeln!(
        out,
        "        const val PLUGIN_ID = {}",
        kt_string(&contract.plugin_id),
    );
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
}

fn write_stateless_dispatcher(out: &mut String, contract: &Contract) {
    let name = &contract.type_name;
    let _ = writeln!(out, "class {name}Dispatcher(");
    let _ = writeln!(out, "    private val backend: {name}Backend,");
    let _ = writeln!(out, "    private val codecs: {name}Codecs,");
    let _ = writeln!(out, ") : {} {{", dispatcher_bases(contract));
    let _ = writeln!(out);
    write_companion(out, contract);
    if contract.references_native_handle() {
        let _ = writeln!(
            out,
            "    override fun releaseNativeHandle(handleId: Long) {{"
        );
        let _ = writeln!(
            out,
            "        (backend as? HandleReleaser)?.releaseNativeHandle(handleId)",
        );
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out);
    }
    write_handle_call(out, contract, false);
    let _ = writeln!(out, "}}");
}

fn write_stateful_dispatcher(out: &mut String, contract: &Contract) {
    let name = &contract.type_name;
    let Some(init) = &contract.init else {
        return;
    };
    let _ = writeln!(out, "interface {name}Factory {{");
    let _ = writeln!(
        out,
        "    suspend fun create(config: {}): {name}Backend",
        init.to_kotlin(),
    );
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
    let _ = writeln!(out, "class {name}Dispatcher(");
    let _ = writeln!(out, "    private val factory: {name}Factory,");
    let _ = writeln!(out, "    private val codecs: {name}Codecs,");
    let _ = writeln!(out, ") : {} {{", dispatcher_bases(contract));
    let _ = writeln!(out);
    write_companion(out, contract);
    let _ = writeln!(
        out,
        "    private val instances = java.util.concurrent.ConcurrentHashMap<Long, {name}Backend>()",
    );
    let _ = writeln!(
        out,
        "    private val nextInstanceId = java.util.concurrent.atomic.AtomicLong(1)"
    );
    if contract.references_native_handle() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "    override fun releaseNativeHandle(handleId: Long) {{"
        );
        let _ = writeln!(out, "        for (backend in instances.values) {{");
        let _ = writeln!(
            out,
            "            (backend as? HandleReleaser)?.releaseNativeHandle(handleId)",
        );
        let _ = writeln!(out, "        }}");
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "    override suspend fun handleCreateInstance(payload: ByteArray): ByteArray {{",
    );
    WIRE.read_last(
        out,
        "        ",
        init,
        "config",
        ReadSource {
            bytes: "payload",
            cursor: "0",
        },
    );
    let _ = writeln!(out, "        val backend = factory.create(config)");
    let _ = writeln!(out, "        val id = nextInstanceId.getAndIncrement()");
    let _ = writeln!(out, "        instances[id] = backend");
    let _ = writeln!(out, "        val out = ByteArrayOutputStream()");
    let _ = writeln!(out, "        Bincode.writeVarintU64(out, id)");
    let _ = writeln!(out, "        return out.toByteArray()");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    write_handle_call(out, contract, true);
    let _ = writeln!(out, "}}");
}

fn write_handle_call(out: &mut String, contract: &Contract, stateful: bool) {
    let name = &contract.type_name;
    let _ = writeln!(
        out,
        "    override suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): PluginResult {{",
    );
    if stateful {
        let _ = writeln!(out, "        val backend = instances[instanceId]");
        let _ = writeln!(
            out,
            "            ?: error(\"{name}: unknown instance $instanceId\")"
        );
    }
    let _ = writeln!(out, "        return when (method) {{");
    for method in &contract.methods {
        write_method_arm(out, method);
    }
    let _ = writeln!(
        out,
        "            else -> error(\"unknown {name} method: $method\")",
    );
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
}

fn write_method_arm(out: &mut String, method: &Method) {
    let is_stream = matches!(method.kind, MethodKind::Stream);
    let unary_error = method.error.as_ref().filter(|_| !is_stream);
    let _ = writeln!(out, "            {} -> {{", kt_string(&method.name));
    if unary_error.is_some() {
        let _ = writeln!(out, "                try {{");
    }
    let indent = if unary_error.is_some() {
        "                    "
    } else {
        "                "
    };
    let locals: Vec<String> = method
        .args
        .iter()
        .map(|arg| local(&arg.name, DISPATCH_LOCALS, kt_ident))
        .collect();
    WIRE.read_sequence(
        out,
        indent,
        "payload",
        method
            .args
            .iter()
            .zip(&locals)
            .map(|(arg, name)| (&arg.ty, name.as_str())),
    );
    let call = format!("backend.{}({})", kt_ident(&method.name), locals.join(", "));
    if is_stream {
        let _ = writeln!(out, "{indent}val __istmo_flow = {call}");
        let _ = writeln!(out, "{indent}PluginResult.Stream(");
        if method.returns.is_unit() {
            // `()` items encode to no bytes.
            let _ = writeln!(out, "{indent}    __istmo_flow.map {{");
            let _ = writeln!(out, "{indent}        ByteArray(0)");
        } else {
            let _ = writeln!(out, "{indent}    __istmo_flow.map {{ __istmo_item ->");
            let _ = writeln!(out, "{indent}        val out = ByteArrayOutputStream()");
            WIRE.write_stmt(
                out,
                &format!("{indent}        "),
                &method.returns,
                "out",
                "__istmo_item",
            );
            let _ = writeln!(out, "{indent}        out.toByteArray()");
        }
        if let Some(err) = &method.error {
            // The runtime ends the stream with `StreamEnd::Error` carrying
            // a `PluginException`'s payload.
            let _ = writeln!(out, "{indent}    }}.catch {{ e ->");
            let _ = writeln!(
                out,
                "{indent}        val err = (e as? BackendException)?.error as? {} ?: throw e",
                err.to_kotlin(),
            );
            let _ = writeln!(out, "{indent}        val errOut = ByteArrayOutputStream()");
            WIRE.write_stmt(out, &format!("{indent}        "), err, "errOut", "err");
            let _ = writeln!(
                out,
                "{indent}        throw PluginException(errOut.toByteArray())"
            );
        }
        let _ = writeln!(out, "{indent}    }}");
        let _ = writeln!(out, "{indent})");
    } else if method.returns.is_unit() {
        let _ = writeln!(out, "{indent}{call}");
        let _ = writeln!(out, "{indent}PluginResult.Unary(ByteArray(0))");
    } else {
        let _ = writeln!(out, "{indent}val result = {call}");
        let _ = writeln!(out, "{indent}val out = ByteArrayOutputStream()");
        WIRE.write_stmt(out, indent, &method.returns, "out", "result");
        let _ = writeln!(out, "{indent}PluginResult.Unary(out.toByteArray())");
    }
    if let Some(err) = unary_error {
        let _ = writeln!(out, "                }} catch (e: BackendException) {{");
        let _ = writeln!(
            out,
            "                    val err = e.error as? {} ?: throw e",
            err.to_kotlin(),
        );
        let _ = writeln!(
            out,
            "                    val errOut = ByteArrayOutputStream()"
        );
        WIRE.write_stmt(out, "                    ", err, "errOut", "err");
        let _ = writeln!(
            out,
            "                    throw PluginException(errOut.toByteArray())"
        );
        let _ = writeln!(out, "                }}");
    }
    let _ = writeln!(out, "            }}");
}
