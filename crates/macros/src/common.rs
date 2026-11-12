//! Parsing and validation shared by the trait-shaped attribute macros
//! (`#[plugin]`, `#[service]`, `#[worker]`).

use quote::quote;
use syn::{
    Expr, ExprLit, FnArg, GenericArgument, ItemTrait, Lit, LitStr, Path, PathArguments, ReturnType,
    Signature, Token, TraitItem, Type, parse_quote, parse_str,
};

/// Which macro is validating a trait; only used to phrase errors.
#[derive(Clone, Copy)]
pub(crate) enum TraitKind {
    Plugin,
    Service,
    Worker,
}

impl TraitKind {
    const fn noun(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Service => "service",
            Self::Worker => "worker",
        }
    }
}

pub(crate) fn expect_lit_str(expr: &Expr) -> syn::Result<LitStr> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.clone()),
        _ => Err(syn::Error::new_spanned(expr, "expected string literal")),
    }
}

/// Parse a `key = "::some::path"` argument value into a [`Path`].
pub(crate) fn expect_path_lit(expr: &Expr, key: &str) -> syn::Result<Path> {
    let raw = expect_lit_str(expr)?;
    parse_str::<Path>(&raw.value())
        .map_err(|e| syn::Error::new_spanned(expr, format!("invalid `{key}` path: {e}")))
}

/// Reject trait and method shapes the generated client / host code
/// cannot model, pointing the error at the offending tokens.
pub(crate) fn reject_unsupported_trait_shape(t: &ItemTrait, kind: TraitKind) -> syn::Result<()> {
    let noun = kind.noun();
    if !t.generics.params.is_empty() || t.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &t.generics,
            format!("generic {noun} traits are not supported"),
        ));
    }
    if !t.supertraits.is_empty() {
        return Err(syn::Error::new_spanned(
            &t.supertraits,
            format!("supertraits on a {noun} trait are not supported"),
        ));
    }
    if let Some(unsafety) = &t.unsafety {
        return Err(syn::Error::new_spanned(
            unsafety,
            format!("`unsafe` {noun} traits are not supported"),
        ));
    }
    for item in &t.items {
        let TraitItem::Fn(f) = item else {
            return Err(syn::Error::new_spanned(
                item,
                format!("a {noun} trait may only declare methods"),
            ));
        };
        let sig = &f.sig;
        if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
            return Err(syn::Error::new_spanned(
                &sig.generics,
                format!(
                    "generic methods are not supported on a {noun} trait; \
                     every argument and return type must be concrete"
                ),
            ));
        }
        if let Some(body) = &f.default {
            return Err(syn::Error::new_spanned(
                body,
                format!(
                    "default method bodies are not supported on a {noun} trait; \
                     the macro rewrites each signature, so implement the method on the type instead"
                ),
            ));
        }
        if let Some(unsafety) = &sig.unsafety {
            return Err(syn::Error::new_spanned(
                unsafety,
                format!("`unsafe fn` is not supported on a {noun} trait"),
            ));
        }
        if let Some(variadic) = &sig.variadic {
            return Err(syn::Error::new_spanned(
                variadic,
                "variadic arguments are not supported",
            ));
        }
        require_ref_self(sig)?;
    }
    Ok(())
}

/// The receiver must be exactly `&self`: the dispatcher shares one
/// implementation across concurrent calls, so `&mut self`, `self` and
/// typed receivers (`self: Arc<Self>`) cannot be served.
fn require_ref_self(sig: &Signature) -> syn::Result<()> {
    let Some(FnArg::Receiver(receiver)) = sig.inputs.first() else {
        return Err(syn::Error::new_spanned(
            sig,
            format!("`{}` must take `&self` as its first argument", sig.ident),
        ));
    };
    let plain_ref = receiver.colon_token.is_none()
        && receiver.mutability.is_none()
        && matches!(&receiver.reference, Some((_, None)));
    if plain_ref {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            receiver,
            "the receiver must be `&self`; `&mut self`, `self`, explicit lifetimes and typed receivers are not supported",
        ))
    }
}

/// Rewrite every `async fn` into a plain `fn` returning
/// `impl Future<Output = T> + Send + '_`, so implementations stay
/// `async fn` while the dispatcher can rely on `Send` futures.
pub(crate) fn desugar_async_methods(trait_def: &mut ItemTrait) {
    for item in &mut trait_def.items {
        let TraitItem::Fn(f) = item else { continue };
        if f.sig.asyncness.is_none() {
            continue;
        }
        f.sig.asyncness = None;
        let return_ty = match &f.sig.output {
            ReturnType::Default => quote! { () },
            ReturnType::Type(_, t) => quote! { #t },
        };
        let new_output: Type = parse_quote! {
            impl ::core::future::Future<Output = #return_ty> + ::core::marker::Send + '_
        };
        f.sig.output = ReturnType::Type(<Token![->]>::default(), Box::new(new_output));
    }
}

/// How a method's declared return type maps onto the wire.
///
/// Client and host both read this one classification, so they always
/// agree on whether the error half travels as a domain error or as part
/// of the success payload.
pub(crate) enum ReturnShape {
    /// The whole value is the success payload.
    Plain(Type),
    /// `Result<T, E>`: `T` is the success payload, `E` the domain error.
    Result { ok: Type, err: Box<Type> },
}

impl ReturnShape {
    /// Type that travels in a success response.
    pub(crate) const fn ok_ty(&self) -> &Type {
        match self {
            Self::Plain(ty) | Self::Result { ok: ty, .. } => ty,
        }
    }

    pub(crate) fn err_ty(&self) -> Option<&Type> {
        match self {
            Self::Plain(_) => None,
            Self::Result { err, .. } => Some(err.as_ref()),
        }
    }
}

/// Classify a return type.
///
/// Any path whose last segment is `Result` must spell out both type
/// parameters. A one-parameter alias such as `io::Result<T>` hides the
/// error type, and treating it as either shape would let the two sides
/// of the wire disagree, so it is refused. Aliases under another name
/// (`type Reply<T> = Result<T, MyError>`) are opaque to the macro and
/// travel whole as the success payload.
pub(crate) fn classify_return(output: &ReturnType) -> syn::Result<ReturnShape> {
    let ty = match output {
        ReturnType::Default => return Ok(ReturnShape::Plain(parse_quote! { () })),
        ReturnType::Type(_, t) => (**t).clone(),
    };
    let Type::Path(tp) = &ty else {
        return Ok(ReturnShape::Plain(ty));
    };
    let Some(last) = tp.path.segments.last() else {
        return Ok(ReturnShape::Plain(ty));
    };
    if last.ident != "Result" {
        return Ok(ReturnShape::Plain(ty));
    }
    let types: Vec<&Type> = match &last.arguments {
        PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|a| match a {
                GenericArgument::Type(t) => Some(t),
                _ => None,
            })
            .collect(),
        PathArguments::None | PathArguments::Parenthesized(_) => Vec::new(),
    };
    match types.as_slice() {
        [ok, err] => Ok(ReturnShape::Result {
            ok: (*ok).clone(),
            err: Box::new((*err).clone()),
        }),
        _ => Err(syn::Error::new_spanned(
            &ty,
            "spell out the full `Result<T, E>`: an alias with a hidden error type \
             (such as `io::Result<T>`) cannot be split into a success payload and a domain error",
        )),
    }
}
