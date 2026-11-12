use std::fmt::Write as _;

use crate::codegen::{kt_ident, write_header};
use crate::contract::{Contract, Method, MethodKind, TypeRef};

#[must_use]
pub fn generate_kotlin(contract: &Contract) -> String {
    let mut out = String::new();
    write_header(&mut out, contract);
    let _ = writeln!(out);
    write_interface(&mut out, contract);
    if let Some(init) = &contract.init {
        write_factory(&mut out, contract, init);
    }
    out
}

fn write_interface(out: &mut String, contract: &Contract) {
    let _ = writeln!(out, "interface {} {{", contract.type_name);
    for (i, method) in contract.methods.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        write_method(out, method);
    }
    let _ = writeln!(out, "}}");
}

fn write_method(out: &mut String, method: &Method) {
    let args = format_args(method);
    let name = kt_ident(&method.name);
    let ret = method.returns.to_kotlin();
    match method.kind {
        MethodKind::Unary => {
            let _ = writeln!(out, "    suspend fun {name}({args}): {ret}");
        }
        MethodKind::Stream => {
            let _ = writeln!(
                out,
                "    fun {name}({args}): kotlinx.coroutines.flow.Flow<{ret}>"
            );
        }
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

fn write_factory(out: &mut String, contract: &Contract, init: &TypeRef) {
    let _ = writeln!(out);
    let _ = writeln!(out, "// Factory contract for `acquire_with`.");
    let _ = writeln!(out, "interface {}Factory {{", contract.type_name);
    let _ = writeln!(
        out,
        "    suspend fun create(config: {}): {}",
        init.to_kotlin(),
        contract.type_name,
    );
    let _ = writeln!(out, "}}");
}
