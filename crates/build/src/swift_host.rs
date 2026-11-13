use std::fmt::Write as _;

use crate::codegen::{NamedAccess, SwiftWire, local, swift_ident, swift_string, write_header};
use crate::contract::{Contract, Method, MethodKind, TypeRef};

const WIRE: SwiftWire = SwiftWire::new(NamedAccess::Codecs);

/// Names the generated `handleCall` / `handleCreateInstance` bodies
/// already bind; decoded arguments with the same name get renamed.
const DISPATCH_LOCALS: &[&str] = &[
    "backend",
    "backendOpt",
    "c",
    "codecs",
    "config",
    "err",
    "errOut",
    "factory",
    "id",
    "instanceId",
    "instances",
    "lock",
    "method",
    "nextInstanceId",
    "out",
    "payload",
    "result",
];

#[must_use]
pub fn generate_swift_host(contract: &Contract) -> String {
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out);
    let _ = writeln!(out, "import Foundation");
    let _ = writeln!(out, "import IstmoRuntime");
    let _ = writeln!(out);
    write_backend_protocol(&mut out, contract);
    let _ = writeln!(out);
    write_codecs_protocol(&mut out, contract);
    let _ = writeln!(out);
    write_dispatcher_class(&mut out, contract);
    out
}

fn write_backend_protocol(out: &mut String, contract: &Contract) {
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
    let _ = writeln!(
        out,
        "public protocol {}Backend: AnyObject {{",
        contract.type_name
    );
    for method in &contract.methods {
        write_backend_method(out, method);
    }
    let _ = writeln!(out, "}}");
}

fn write_backend_method(out: &mut String, method: &Method) {
    if let (MethodKind::Unary, Some(err)) = (method.kind, &method.error) {
        let _ = writeln!(
            out,
            "    /// Throw `{}` to surface a typed domain error.",
            err.to_swift()
        );
    }
    let _ = writeln!(out, "    {}", backend_method_decl(method));
}

/// Requirement of `method` on the `<T>Backend` protocol, shared with the
/// seeded implementation so the two cannot drift apart.
pub(crate) fn backend_method_decl(method: &Method) -> String {
    let args = format_args(method);
    let ret = method.returns.to_swift();
    let name = swift_ident(&method.name);
    match method.kind {
        MethodKind::Unary => format!("func {name}({args}) async throws -> {ret}"),
        MethodKind::Stream => format!("func {name}({args}) -> AsyncThrowingStream<{ret}, Error>"),
    }
}

/// `label: Type` parameters, labels backticked when they are keywords.
pub(crate) fn format_args(method: &Method) -> String {
    method
        .args
        .iter()
        .map(|a| format!("{}: {}", swift_ident(&a.name), a.ty.to_swift()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `public protocol <T>Codecs`, shared by the dispatcher and the client.
pub(crate) fn write_codecs_protocol(out: &mut String, contract: &Contract) {
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
    let _ = writeln!(out, "/// helper on the type itself.");
    let _ = writeln!(out, "public protocol {}Codecs {{", contract.type_name);
    if types.is_empty() {
        let _ = writeln!(out, "    // No named types in this contract.");
    }
    for ty in &types {
        let _ = writeln!(
            out,
            "    func read{ty}(_ c: inout Bincode.Cursor) throws -> {ty}"
        );
        let _ = writeln!(out, "    func write{ty}(_ out: inout Data, _ value: {ty})");
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
        "/// Register with `IstmoRuntime.shared.registerHandler(PLUGIN_ID, dispatcher)`."
    );

    if contract.init.is_some() {
        write_stateful_dispatcher(out, contract);
    } else {
        write_stateless_dispatcher(out, contract);
    }
}

fn releaser(contract: &Contract) -> &'static str {
    if contract.references_native_handle() {
        ", HandleReleaser"
    } else {
        ""
    }
}

fn write_stateless_dispatcher(out: &mut String, contract: &Contract) {
    let name = &contract.type_name;
    let _ = writeln!(
        out,
        "public final class {name}Dispatcher: PluginHandler{} {{",
        releaser(contract),
    );
    let _ = writeln!(
        out,
        "    public static let PLUGIN_ID: String = {}",
        swift_string(&contract.plugin_id),
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "    private let backend: {name}Backend");
    let _ = writeln!(out, "    private let codecs: {name}Codecs");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "    public init(backend: {name}Backend, codecs: {name}Codecs) {{",
    );
    let _ = writeln!(out, "        self.backend = backend");
    let _ = writeln!(out, "        self.codecs = codecs");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    if contract.references_native_handle() {
        let _ = writeln!(
            out,
            "    public func releaseNativeHandle(_ handleId: UInt64) {{",
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
    let _ = writeln!(out, "public protocol {name}Factory {{");
    let _ = writeln!(
        out,
        "    func create(config: {}) async throws -> {name}Backend",
        init.to_swift(),
    );
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "public final class {name}Dispatcher: PluginHandler{} {{",
        releaser(contract),
    );
    let _ = writeln!(
        out,
        "    public static let PLUGIN_ID: String = {}",
        swift_string(&contract.plugin_id),
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "    private let factory: {name}Factory");
    let _ = writeln!(out, "    private let codecs: {name}Codecs");
    let _ = writeln!(
        out,
        "    private var instances: [UInt64: {name}Backend] = [:]"
    );
    let _ = writeln!(out, "    private var nextInstanceId: UInt64 = 1");
    let _ = writeln!(out, "    private let lock = NSLock()");
    let _ = writeln!(out);
    if contract.references_native_handle() {
        let _ = writeln!(
            out,
            "    public func releaseNativeHandle(_ handleId: UInt64) {{",
        );
        let _ = writeln!(out, "        lock.lock()");
        let _ = writeln!(out, "        let snapshot = Array(instances.values)");
        let _ = writeln!(out, "        lock.unlock()");
        let _ = writeln!(out, "        for backend in snapshot {{");
        let _ = writeln!(
            out,
            "            (backend as? HandleReleaser)?.releaseNativeHandle(handleId)",
        );
        let _ = writeln!(out, "        }}");
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out);
    }
    let _ = writeln!(
        out,
        "    public init(factory: {name}Factory, codecs: {name}Codecs) {{",
    );
    let _ = writeln!(out, "        self.factory = factory");
    let _ = writeln!(out, "        self.codecs = codecs");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "    public func handleCreateInstance(payload: Data) async throws -> Data {{",
    );
    let _ = writeln!(out, "        var c = Bincode.Cursor(payload)");
    let _ = writeln!(out, "        let config = {}", WIRE.read(init, "c"));
    let _ = writeln!(
        out,
        "        let backend = try await factory.create(config: config)"
    );
    let _ = writeln!(out, "        lock.lock()");
    let _ = writeln!(out, "        let id = nextInstanceId");
    let _ = writeln!(out, "        nextInstanceId += 1");
    let _ = writeln!(out, "        instances[id] = backend");
    let _ = writeln!(out, "        lock.unlock()");
    let _ = writeln!(out, "        var out = Data()");
    let _ = writeln!(out, "        Bincode.writeVarintU64(&out, id)");
    let _ = writeln!(out, "        return out");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    write_handle_call(out, contract, true);
    let _ = writeln!(out, "}}");
}

fn write_handle_call(out: &mut String, contract: &Contract, stateful: bool) {
    let _ = writeln!(
        out,
        "    public func handleCall(instanceId: UInt64, method: String, payload: Data) async throws -> Data {{",
    );
    if stateful {
        let _ = writeln!(out, "        lock.lock()");
        let _ = writeln!(out, "        let backendOpt = instances[instanceId]");
        let _ = writeln!(out, "        lock.unlock()");
        let _ = writeln!(
            out,
            "        guard let backend = backendOpt else {{ throw PluginRuntimeError.unknownInstance(instanceId) }}",
        );
    }
    let _ = writeln!(out, "        switch method {{");
    for method in &contract.methods {
        write_method_arm(out, contract, method);
    }
    let _ = writeln!(out, "        default:");
    let _ = writeln!(
        out,
        "            throw PluginRuntimeError.unknownMethod(plugin: {}, method: method)",
        swift_string(&contract.type_name),
    );
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
}

fn write_method_arm(out: &mut String, contract: &Contract, method: &Method) {
    let _ = writeln!(out, "        case {}:", swift_string(&method.name));
    if matches!(method.kind, MethodKind::Stream) {
        // `PluginHandler.handleCall` answers with a single `Data`; the
        // iOS runtime has no hosted-stream path to hand the backend's
        // `AsyncThrowingStream` to yet.
        let _ = writeln!(
            out,
            "            throw PluginRuntimeError.unknownMethod(plugin: {}, method: method)",
            swift_string(&contract.type_name),
        );
        return;
    }
    if method.error.is_some() {
        let _ = writeln!(out, "            do {{");
    }
    let indent = if method.error.is_some() {
        "                "
    } else {
        "            "
    };
    if !method.args.is_empty() {
        let _ = writeln!(out, "{indent}var c = Bincode.Cursor(payload)");
    }
    let mut call_args = Vec::with_capacity(method.args.len());
    for arg in &method.args {
        let name = local(&arg.name, DISPATCH_LOCALS, swift_ident);
        let _ = writeln!(out, "{indent}let {name} = {}", WIRE.read(&arg.ty, "c"));
        call_args.push(format!("{}: {name}", swift_ident(&arg.name)));
    }
    let call = format!(
        "backend.{}({})",
        swift_ident(&method.name),
        call_args.join(", ")
    );
    if method.returns.is_unit() {
        let _ = writeln!(out, "{indent}try await {call}");
        let _ = writeln!(out, "{indent}return Data()");
    } else {
        let _ = writeln!(out, "{indent}let result = try await {call}");
        let _ = writeln!(out, "{indent}var out = Data()");
        WIRE.write_stmt(out, indent, &method.returns, "out", "result");
        let _ = writeln!(out, "{indent}return out");
    }
    if let Some(err) = &method.error {
        write_error_catch(out, err);
    }
}

fn write_error_catch(out: &mut String, err: &TypeRef) {
    let _ = writeln!(out, "            }} catch let err as {} {{", err.to_swift());
    let _ = writeln!(out, "                var errOut = Data()");
    WIRE.write_stmt(out, "                ", err, "errOut", "err");
    let _ = writeln!(
        out,
        "                throw PluginException(payload: errOut)"
    );
    let _ = writeln!(out, "            }}");
}
