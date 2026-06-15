//! Shared parsing helpers for the attribute macros.

use syn::{Expr, Lit, ReturnType, Type};

/// `lookup_term` → `LookupTerm`.
pub fn pascal_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = true;
    for ch in snake.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Pull the `String` out of a `"..."` literal expression.
pub fn lit_string(expr: &Expr) -> syn::Result<String> {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(s) => Ok(s.value()),
            other => Err(syn::Error::new_spanned(other, "expected a string literal")),
        },
        other => Err(syn::Error::new_spanned(other, "expected a string literal")),
    }
}

/// The `T` in `-> Result<T, E>` or `-> AgenkitResult<T>`.
pub fn result_ok_type(ret: &ReturnType) -> syn::Result<Type> {
    let ReturnType::Type(_, ty) = ret else {
        return Err(syn::Error::new_spanned(
            ret,
            "expected a return type of `AgenkitResult<T>`",
        ));
    };
    let Type::Path(path) = ty.as_ref() else {
        return Err(syn::Error::new_spanned(
            ty,
            "expected `AgenkitResult<T>` / `Result<T, _>`",
        ));
    };
    let segment = path.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(ty, "expected `AgenkitResult<T>` / `Result<T, _>`")
    })?;
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "expected `AgenkitResult<T>` / `Result<T, _>` with a type argument",
        ));
    };
    for arg in &args.args {
        if let syn::GenericArgument::Type(t) = arg {
            return Ok(t.clone());
        }
    }
    Err(syn::Error::new_spanned(
        ty,
        "could not find the `Ok` type argument",
    ))
}

/// Join `#[doc = "..."]` lines into a trimmed description.
pub fn doc_string(attrs: &[syn::Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta
            && let Ok(text) = lit_string(&nv.value)
        {
            lines.push(text.trim().to_string());
        }
    }
    lines.join("\n").trim().to_string()
}
