//! Directive registry + attribute parser.
//!
//! A directive is a `fn(&DirectiveCall)`; the walker calls it once per
//! matching attribute after resolving the enclosing scope.

use once_cell::sync::OnceCell;
use std::collections::HashMap;
use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::reactive::ScopeId;

pub mod anchor;
pub mod bind;
pub mod for_;
pub mod html;
pub mod if_;
pub mod init;
pub mod interp;
pub mod intersect;
pub mod model;
pub mod on;
pub mod ref_;
pub mod resize;
pub mod roving;
pub mod route;
pub mod show;
pub mod teleport;
pub mod text;
pub mod transition;

pub struct DirectiveCall<'a> {
    pub el: &'a Element,
    pub proxy: &'a JsValue,
    pub scope_id: ScopeId,
    /// Argument after the first `:` in the attribute name
    /// (e.g. `class` for `pp-bind:class`).
    pub arg: Option<String>,
    /// Modifiers after `.` (e.g. `prevent` in `pp-on:click.prevent`).
    pub modifiers: Vec<String>,
    /// Attribute value verbatim (the right-hand side after `=`).
    pub value: String,
}

pub type DirectiveFn = fn(&DirectiveCall);

static REGISTRY: OnceCell<HashMap<&'static str, DirectiveFn>> = OnceCell::new();

fn registry() -> &'static HashMap<&'static str, DirectiveFn> {
    REGISTRY.get_or_init(|| {
        let mut m: HashMap<&'static str, DirectiveFn> = HashMap::new();
        m.insert("text", text::run);
        m.insert("html", html::run);
        m.insert("bind", bind::run);
        m.insert("on", on::run);
        m.insert("show", show::run);
        m.insert("model", model::run);
        m.insert("init", init::run);
        m.insert("route", route::run);
        m.insert("for", for_::run);
        m.insert("if", if_::run);
        m.insert("teleport", teleport::run);
        m.insert("ref", ref_::run);
        m.insert("resize", resize::run);
        m.insert("intersect", intersect::run);
        m.insert("anchor", anchor::run);
        m.insert("roving", roving::run);
        m
    })
}

pub fn lookup(name: &str) -> Option<DirectiveFn> {
    registry().get(name).copied()
}

/// Parse an attribute name like `pp-bind:class.camel` into
/// `(directive, arg, modifiers)`.
pub fn parse_attr(attr: &str) -> Option<(String, Option<String>, Vec<String>)> {
    let body = attr.strip_prefix("pp-")?;
    let (head, rest) = match body.split_once(':') {
        Some((h, r)) => (h, Some(r)),
        None => (body, None),
    };
    let mut head_parts = head.split('.');
    let name = head_parts.next()?.to_string();
    let head_mods: Vec<String> = head_parts.map(str::to_string).collect();
    let (arg, tail_mods) = if let Some(rest) = rest {
        let mut it = rest.split('.');
        let a = it.next().map(str::to_string);
        (a, it.map(str::to_string).collect::<Vec<_>>())
    } else {
        (None, Vec::new())
    };
    let mut mods = head_mods;
    mods.extend(tail_mods);
    Some((name, arg, mods))
}
