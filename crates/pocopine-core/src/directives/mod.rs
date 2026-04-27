//! Directive registry + attribute parser.
//!
//! A directive is a `fn(&DirectiveCall)`; the walker calls it once per
//! matching attribute after resolving the enclosing scope.

use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::reactive::ScopeId;

pub mod anchor;
pub mod bind;
pub mod flip;
pub mod for_;
pub mod for_plan;
pub mod html;
pub mod if_;
pub mod init;
pub mod interp;
pub mod intersect;
pub mod model;
pub mod on;
pub mod ref_;
pub mod resize;
pub mod route;
pub mod roving;
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

// RFC-058 Phase 6.5 size pass — registry was a
// `OnceCell<HashMap<&'static str, DirectiveFn>>` for ~5 entries
// without `legacy-dom` and ~17 with. Twiggy showed the
// HashMap's `reserve_rehash` instantiation alone cost ~2.2 KB.
// A `&'static [(&str, DirectiveFn)]` + linear scan is smaller,
// faster (no hash, no allocation, no OnceCell), and the lookup
// is only called from `apply_static_plan` for opaque directives
// + `walker::dispatch` for legacy-dom — neither is a hot loop.
const REGISTRY: &[(&str, DirectiveFn)] = &[
    // RFC-058 Phase 6.5 opaque-directive lift allowlist. The
    // compiled path's `apply_static_plan` looks these up by name
    // for `StaticOpaqueDirective` entries; they're the only
    // directives compiled-only apps need at runtime.
    ("resize", resize::run),
    ("intersect", intersect::run),
    ("anchor", anchor::run),
    ("roving", roving::run),
    ("flip", flip::run),
    // Every other `run()` is a walker-only entry: the runtime
    // walker's `dispatch` loop calls them when scanning `pp-*`
    // attributes on user-authored DOM. Compiled apps install
    // these via the typed `install_*` entries on
    // `apply_static_plan` instead, so the registry stays bare
    // without `legacy-dom`. With the feature off, the linker
    // drops every `run()` body listed below (and their
    // transitive `DirectiveCall` extraction helpers) — that's
    // the bulk of the Phase 6.5 size win.
    #[cfg(feature = "legacy-dom")]
    ("text", text::run),
    #[cfg(feature = "legacy-dom")]
    ("html", html::run),
    #[cfg(feature = "legacy-dom")]
    ("bind", bind::run),
    #[cfg(feature = "legacy-dom")]
    ("on", on::run),
    #[cfg(feature = "legacy-dom")]
    ("show", show::run),
    #[cfg(feature = "legacy-dom")]
    ("model", model::run),
    #[cfg(feature = "legacy-dom")]
    ("init", init::run),
    #[cfg(feature = "legacy-dom")]
    ("route", route::run),
    #[cfg(feature = "legacy-dom")]
    ("for", for_::run),
    #[cfg(feature = "legacy-dom")]
    ("if", if_::run),
    #[cfg(feature = "legacy-dom")]
    ("teleport", teleport::run),
    #[cfg(feature = "legacy-dom")]
    ("ref", ref_::run),
];

pub fn lookup(name: &str) -> Option<DirectiveFn> {
    REGISTRY.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
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

/// Normalize a template-facing prop/field name into the Rust field
/// key used by component state.
///
/// Template syntax is HTML-shaped (`fixed-weeks`, `min-value`);
/// component fields are Rust-shaped (`fixed_weeks`, `min_value`).
/// Directive parsing intentionally preserves the authored name, so
/// callers that cross a component boundary must normalize before
/// consulting `is_prop()` or writing through the child's proxy.
pub fn normalize_prop_name(name: &str) -> String {
    name.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::{normalize_prop_name, parse_attr};

    #[test]
    fn parse_attr_preserves_kebab_case_arg() {
        let parsed = parse_attr("pp-bind:fixed-weeks.camel").expect("parsed");
        assert_eq!(parsed.0, "bind");
        assert_eq!(parsed.1.as_deref(), Some("fixed-weeks"));
        assert_eq!(parsed.2, vec!["camel"]);
    }

    #[test]
    fn parse_attr_keeps_data_and_aria_names_intact() {
        let bind = parse_attr("pp-bind:aria-labelledby.once").expect("parsed bind");
        assert_eq!(bind.0, "bind");
        assert_eq!(bind.1.as_deref(), Some("aria-labelledby"));
        assert_eq!(bind.2, vec!["once"]);

        let model = parse_attr("pp-model:min-value.lazy").expect("parsed model");
        assert_eq!(model.0, "model");
        assert_eq!(model.1.as_deref(), Some("min-value"));
        assert_eq!(model.2, vec!["lazy"]);
    }

    #[test]
    fn parse_attr_collects_head_and_tail_modifiers_in_order() {
        let parsed = parse_attr("pp-on.window.capture:keydown.enter.prevent.once").expect("parsed");
        assert_eq!(parsed.0, "on");
        assert_eq!(parsed.1.as_deref(), Some("keydown"));
        assert_eq!(
            parsed.2,
            vec!["window", "capture", "enter", "prevent", "once"]
        );
    }

    #[test]
    fn parse_attr_supports_modifier_only_directives() {
        let parsed = parse_attr("pp-show.important.transition").expect("parsed");
        assert_eq!(parsed.0, "show");
        assert_eq!(parsed.1, None);
        assert_eq!(parsed.2, vec!["important", "transition"]);
    }

    #[test]
    fn parse_attr_rejects_non_directive_attributes() {
        assert!(parse_attr("class").is_none());
        assert!(parse_attr(":fixed-weeks").is_none());
        assert!(parse_attr("@click").is_none());
    }

    #[test]
    fn parse_attr_preserves_empty_arg_when_authored() {
        let parsed = parse_attr("pp-bind:").expect("parsed");
        assert_eq!(parsed.0, "bind");
        assert_eq!(parsed.1.as_deref(), Some(""));
        assert!(parsed.2.is_empty());
    }

    #[test]
    fn normalize_prop_name_maps_kebab_to_snake() {
        assert_eq!(normalize_prop_name("fixed-weeks"), "fixed_weeks");
        assert_eq!(normalize_prop_name("min-value"), "min_value");
        assert_eq!(normalize_prop_name("disabled"), "disabled");
    }

    #[test]
    fn normalize_prop_name_leaves_existing_snake_case_unchanged() {
        assert_eq!(normalize_prop_name("week_starts_on"), "week_starts_on");
        assert_eq!(normalize_prop_name("model"), "model");
    }

    #[test]
    fn normalize_prop_name_handles_repeated_and_edge_hyphens() {
        assert_eq!(normalize_prop_name("data-test-id"), "data_test_id");
        assert_eq!(normalize_prop_name("fixed--weeks"), "fixed__weeks");
        assert_eq!(normalize_prop_name("-leading"), "_leading");
        assert_eq!(normalize_prop_name("trailing-"), "trailing_");
    }

    #[test]
    fn normalize_prop_name_only_rewrites_hyphens() {
        assert_eq!(normalize_prop_name("aria-labelledby"), "aria_labelledby");
        assert_eq!(normalize_prop_name("x.y-z"), "x.y_z");
        assert_eq!(
            normalize_prop_name("already:mixed-name"),
            "already:mixed_name"
        );
    }
}
