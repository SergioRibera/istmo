use std::fmt::Write as _;

use crate::codegen::{
    NamedAccess, SwiftNames, SwiftWire, fresh, swift_ident, swift_string, write_header,
};
use crate::contract::{Contract, Method, MethodKind, TypeRef};
use crate::swift_host::{format_args, write_codecs_protocol};

/// Clients reach the readers and writers through their `codecs`.
const fn wire(names: SwiftNames<'_>) -> SwiftWire<'_> {
    SwiftWire::new(NamedAccess::Codecs, names)
}

#[must_use]
pub fn generate_swift(contract: &Contract) -> String {
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out);
    write_protocol(&mut out, contract);
    if let Some(init) = &contract.init {
        write_factory(&mut out, contract, init);
    }
    out
}

/// Swift consumer of a Rust-hosted plugin: encodes each call with the
/// same per-type writers the dispatcher uses, decodes the response and
/// surfaces domain errors as a typed `<T>Exception`.
///
/// The file also declares `<T>Codecs`, which `<T>CodecsImpl` implements:
/// a client-role app gets no dispatcher file to carry it.
#[must_use]
pub fn generate_swift_client(contract: &Contract) -> String {
    let name = &contract.type_name;
    let uses_codecs = contract.signatures_use_named();
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out);
    let _ = writeln!(out, "import Foundation");
    let _ = writeln!(out, "import IstmoRuntime");
    let _ = writeln!(out);
    write_codecs_protocol(&mut out, contract);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "/// Rust-hosted `{name}` plugin. Ships each call across the frame",
    );
    let _ = writeln!(out, "/// protocol via `IstmoRuntime.shared.call(...)`.");
    let _ = writeln!(out, "public final class {name}Client {{");
    let _ = writeln!(
        out,
        "    public static let PLUGIN_ID: String = {}",
        swift_string(&contract.plugin_id)
    );
    let _ = writeln!(out);
    if uses_codecs {
        let _ = writeln!(out, "    private let codecs: {name}Codecs");
        let _ = writeln!(out);
        let _ = writeln!(out, "    public init(codecs: {name}Codecs) {{");
        let _ = writeln!(out, "        self.codecs = codecs");
        let _ = writeln!(out, "    }}");
    } else {
        let _ = writeln!(out, "    public init() {{}}");
    }
    // A frame sink is opened by a Rust consumer: methods taking one have
    // no Swift caller.
    for method in contract.methods.iter().filter(|m| !m.takes_frame_sink()) {
        let _ = writeln!(out);
        match method.kind {
            MethodKind::Unary => write_unary(&mut out, contract, method),
            MethodKind::Stream => write_stream(&mut out, contract, method),
        }
    }
    let _ = writeln!(out, "}}");
    for err in contract.error_types() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "/// Swift-side view of a domain error decoded from `Frame::Respond {{ Err(bytes) }}`."
        );
        let _ = writeln!(
            out,
            "public struct {}: Error {{",
            contract.exception_name(err)
        );
        let _ = writeln!(out, "    public let payload: Data");
        let _ = writeln!(
            out,
            "    public let decoded: {}?",
            SwiftNames::of(contract).ty(err)
        );
        let _ = writeln!(out, "}}");
    }
    out
}

/// Emits the payload encoding and returns the payload variable's name.
fn write_payload(out: &mut String, swift: SwiftNames<'_>, method: &Method) -> String {
    let payload = fresh("payload", &arg_names(method));
    if method.args.is_empty() {
        let _ = writeln!(out, "        let {payload} = Data()");
        return payload;
    }
    let _ = writeln!(out, "        var {payload} = Data()");
    for arg in &method.args {
        wire(swift).write_stmt(out, "        ", &arg.ty, &payload, &swift_ident(&arg.name));
    }
    payload
}

fn write_unary(out: &mut String, contract: &Contract, method: &Method) {
    let swift = SwiftNames::of(contract);
    let wire = wire(swift);
    let args = format_args(swift, method);
    let ret = swift.ty(&method.returns);
    let _ = writeln!(
        out,
        "    public func {}({args}) async throws -> {ret} {{",
        swift_ident(&method.name),
    );
    let payload = write_payload(out, swift, method);
    let names = arg_names(method);
    let bytes = fresh("bytes", &names);
    let cursor = fresh("c", &names);
    let call = format!(
        "try await IstmoRuntime.shared.call(pluginId: Self.PLUGIN_ID, method: {}, payload: {payload})",
        swift_string(&method.name),
    );
    let returns_unit = method.returns.is_unit();
    match (&method.error, returns_unit) {
        (None, true) => {
            let _ = writeln!(out, "        _ = {call}");
        }
        (None, false) => {
            let _ = writeln!(out, "        let {bytes} = {call}");
        }
        (Some(err), _) => {
            let target = if returns_unit {
                "_"
            } else {
                let _ = writeln!(out, "        let {bytes}: Data");
                bytes.as_str()
            };
            let _ = writeln!(out, "        do {{");
            let _ = writeln!(out, "            {target} = {call}");
            let _ = writeln!(out, "        }} catch let e as PluginException {{");
            let _ = writeln!(out, "            var errCursor = Bincode.Cursor(e.payload)");
            let read = wire.read(err, "errCursor");
            let read = read.strip_prefix("try ").unwrap_or(&read);
            let _ = writeln!(
                out,
                "            let decoded: {}? = try? {read}",
                swift.ty(err)
            );
            let _ = writeln!(
                out,
                "            throw {}(payload: e.payload, decoded: decoded)",
                contract.exception_name(err),
            );
            let _ = writeln!(out, "        }}");
        }
    }
    if !returns_unit {
        let _ = writeln!(out, "        var {cursor} = Bincode.Cursor({bytes})");
        let _ = writeln!(
            out,
            "        return {}",
            wire.read(&method.returns, &cursor)
        );
    }
    let _ = writeln!(out, "    }}");
}

/// Swift resolves a name to a local declared later in the same scope, so
/// the client's own locals must not reuse a parameter name.
fn arg_names(method: &Method) -> Vec<&str> {
    method.args.iter().map(|a| a.name.as_str()).collect()
}

fn write_stream(out: &mut String, contract: &Contract, method: &Method) {
    let swift = SwiftNames::of(contract);
    let wire = wire(swift);
    let args = format_args(swift, method);
    let item = swift.ty(&method.returns);
    let _ = writeln!(
        out,
        "    public func {}({args}) -> AsyncThrowingStream<{item}, Error> {{",
        swift_ident(&method.name),
    );
    if method.signature_uses_named() {
        // The unfolding closure escapes; capture the codecs, not `self`.
        // Bound first: the payload writers below already use the name.
        let _ = writeln!(out, "        let codecs = self.codecs");
    }
    let payload = write_payload(out, swift, method);
    let names = arg_names(method);
    let events = fresh("events", &names);
    let bytes = fresh("bytes", &names);
    let cursor = fresh("c", &names);
    let _ = writeln!(
        out,
        "        var {events} = IstmoRuntime.shared.stream(pluginId: Self.PLUGIN_ID, method: {}, payload: {payload}).makeAsyncIterator()",
        swift_string(&method.name),
    );
    let _ = writeln!(out, "        return AsyncThrowingStream(unfolding: {{");
    let next = format!("try await {events}.next()");
    let next = if let Some(err) = &method.error {
        // `StreamEnd::Error` surfaces as `IstmoRuntimeError.streamError`
        // carrying the encoded domain error.
        let next_item = fresh("next", &names);
        let err_payload = fresh("errPayload", &names);
        let _ = writeln!(out, "            let {next_item}: Data?");
        let _ = writeln!(out, "            do {{");
        let _ = writeln!(out, "                {next_item} = {next}");
        let _ = writeln!(
            out,
            "            }} catch IstmoRuntimeError.streamError(let {err_payload}) {{"
        );
        let _ = writeln!(
            out,
            "                var errCursor = Bincode.Cursor({err_payload})"
        );
        let read = wire.read(err, "errCursor");
        let read = read.strip_prefix("try ").unwrap_or(&read);
        let _ = writeln!(
            out,
            "                let decoded: {}? = try? {read}",
            swift.ty(err)
        );
        let _ = writeln!(
            out,
            "                throw {}(payload: {err_payload}, decoded: decoded)",
            contract.exception_name(err),
        );
        let _ = writeln!(out, "            }}");
        next_item
    } else {
        next
    };
    if method.returns.is_unit() {
        let _ = writeln!(out, "            guard {next} != nil else {{ return nil }}");
        let _ = writeln!(out, "            return ()");
    } else {
        let _ = writeln!(
            out,
            "            guard let {bytes} = {next} else {{ return nil }}"
        );
        let _ = writeln!(out, "            var {cursor} = Bincode.Cursor({bytes})");
        let _ = writeln!(
            out,
            "            return {}",
            wire.read(&method.returns, &cursor)
        );
    }
    let _ = writeln!(out, "        }})");
    let _ = writeln!(out, "    }}");
}

fn write_protocol(out: &mut String, contract: &Contract) {
    let _ = writeln!(out, "protocol {}Interface {{", contract.type_name);
    for (i, method) in contract.methods.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        write_method(out, SwiftNames::of(contract), method);
    }
    let _ = writeln!(out, "}}");
}

fn write_method(out: &mut String, swift: SwiftNames<'_>, method: &Method) {
    let args = format_args(swift, method);
    let name = swift_ident(&method.name);
    let ret = swift.ty(&method.returns);
    match method.kind {
        MethodKind::Unary => {
            let _ = writeln!(out, "    func {name}({args}) async throws -> {ret}");
        }
        MethodKind::Stream => {
            let _ = writeln!(
                out,
                "    func {name}({args}) -> AsyncThrowingStream<{ret}, Error>"
            );
        }
    }
}

fn write_factory(out: &mut String, contract: &Contract, init: &TypeRef) {
    let _ = writeln!(out);
    let _ = writeln!(out, "// Factory contract for `acquire_with`.");
    let _ = writeln!(out, "protocol {}InterfaceFactory {{", contract.type_name);
    let _ = writeln!(
        out,
        "    func create(config: {}) async throws -> {}Interface",
        SwiftNames::of(contract).ty(init),
        contract.type_name,
    );
    let _ = writeln!(out, "}}");
}
