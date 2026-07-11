//! Single source of truth for the pocopine `pp-*` directive surface.
//!
//! Shared by the `#[component]` macro (RFC-063 forbidden-directive scan) and the
//! `pocopine lsp` language server, so editor diagnostics and compiler errors
//! describe the same surface and never drift. Host-only, dependency-free.
//!
//! Three things live here:
//! - [`REMOVED`] — directives RFC-063 §4.1 deleted, with their migration text.
//! - [`DIRECTIVES`] — the live directive specs (arg + host constraints).
//! - [`parse_directive_attr`] — splits a raw attribute name into head / arg /
//!   modifiers, resolving the `:attr` and `@event` shorthands.

#![forbid(unsafe_code)]

/// Directives RFC-063 §4.1 removed from the author-facing surface, keyed by
/// directive head (no `pp-` prefix). The macro turns a match into a
/// `compile_error!`; the language server into an editor diagnostic. The strings
/// are the canonical migration text — macro tests assert substrings of them
/// (`on_setup`, `handlers`, `synchronous`, `auto-stamps`, `RFC 063`), so keep
/// those stable.
pub const REMOVED: &[(&str, &str)] = &[
    (
        "cloak",
        "`pp-cloak` was removed in v2 (RFC 063 §4.1.2). Mount is synchronous \
         post-RFC-061; the FOUC the directive guarded against no longer happens. \
         Drop the attribute.",
    ),
    (
        "init",
        "`pp-init` was removed in v2 (RFC 063 §4.1.1). Use the `on_setup` \
         lifecycle hook instead:\n\n  #[handlers]\n  impl MyComponent {\n      \
         fn on_setup(&mut self) {\n          // your init code here\n      }\n  }",
    ),
    (
        "data",
        "`pp-data` is no longer an author-facing attribute (RFC 063 §4.1.3). The \
         macro auto-stamps the scope marker on every component root; you never \
         needed to type it. Drop the attribute. If you were using it as an \
         unliftable-subtree signal, that authoring pattern was walker-era and no \
         longer exists.",
    ),
];

/// Migration message for a removed directive head, or `None` if it is not a
/// removed directive.
pub fn removed_message(head: &str) -> Option<&'static str> {
    REMOVED.iter().find(|(n, _)| *n == head).map(|(_, m)| *m)
}

/// Whether a directive requires, allows, or forbids an `:arg` after its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgReq {
    Required,
    Optional,
    Forbidden,
}

/// Host-element constraint for a directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// Must sit on a `<template>` host.
    TemplateOnly,
    /// Must sit on a component tag (kebab-case with a hyphen) or the `<root>`
    /// placeholder.
    ComponentOnly,
    /// Any element.
    Any,
}

/// One live directive's contract.
#[derive(Debug, Clone, Copy)]
pub struct DirectiveSpec {
    /// Head name without the `pp-` prefix (e.g. `"on"`).
    pub name: &'static str,
    /// Shorthand prefix, if any (`:` for bind, `@` for on).
    pub shorthand: Option<char>,
    pub takes_arg: ArgReq,
    pub host: Host,
    /// Markdown hover documentation (shown by the language server).
    pub hover_doc: &'static str,
    /// Governing RFC slug, if any.
    pub rfc: Option<&'static str>,
}

const fn d(
    name: &'static str,
    shorthand: Option<char>,
    takes_arg: ArgReq,
    host: Host,
    hover_doc: &'static str,
    rfc: Option<&'static str>,
) -> DirectiveSpec {
    DirectiveSpec {
        name,
        shorthand,
        takes_arg,
        host,
        hover_doc,
        rfc,
    }
}

/// The live directive registry. Mirrors the directives the `#[component]` macro
/// accepts; the removed three (see [`REMOVED`]) are intentionally absent.
pub const DIRECTIVES: &[DirectiveSpec] = &[
    d(
        "if",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-if** — conditional rendering. Host: `<template>` only; body: exactly one element. Compiles to a mount/unmount plan (RFC-058 — no runtime walker).",
        Some("rfc-001-components"),
    ),
    d(
        "else-if",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-else-if** — continues a `pp-if` chain. Host: `<template>` only, and it must be the immediate (whitespace/comment-tolerant) sibling of a `<template pp-if>` or `<template pp-else-if>`. Exactly one branch of the chain renders. (RFC-094)",
        Some("rfc-094-conditional-chains-and-enum-matching"),
    ),
    d(
        "else",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-else** — final branch of a `pp-if` chain; takes NO expression. Host: `<template>` only, immediately after a `pp-if`/`pp-else-if` sibling. (RFC-094)",
        Some("rfc-094-conditional-chains-and-enum-matching"),
    ),
    d(
        "match",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-match** — enum/value dispatch. Host: `<template>` whose direct children are `<template pp-case>` arms. Matches serde's externally-tagged encoding: unit variants by name, payload variants by tag with `pp-let` payload binding. First match wins; nothing renders without a match or `_`. (RFC-094)",
        Some("rfc-094-conditional-chains-and-enum-matching"),
    ),
    d(
        "case",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-case** — one arm of a `pp-match`. Value is a LITERAL arm, not an expression: `pp-case=\"Ready\"`, multi-variant `pp-case=\"Idle | Loading\"`, or the final wildcard `pp-case=\"_\"`. Bind the payload with `pp-let=\"name\"`. (RFC-094)",
        Some("rfc-094-conditional-chains-and-enum-matching"),
    ),
    d(
        "for",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-for** — list iteration. Syntax: `pp-for=\"item in items\"`. Host: `<template>` only. Exposes `$index`, `$first`, `$last`; pair with `pp-key` for keyed diffing.",
        Some("rfc-004-pp-for"),
    ),
    d(
        "show",
        None,
        ArgReq::Forbidden,
        Host::Any,
        "**pp-show** — toggles `display:none` vs the element's default. Element stays in the DOM. Component hosts are supported: false hides the whole rendered subtree; true restores the host's generated `display: contents` rule. Use `pp-if` + `pp-transition` for enter/leave.",
        None,
    ),
    d(
        "text",
        None,
        ArgReq::Forbidden,
        Host::Any,
        "**pp-text** — sets `textContent` to the stringified expression. Prefer inline `{{expr}}` when mixing static and dynamic text.",
        Some("rfc-025-text-interpolation"),
    ),
    d(
        "html",
        None,
        ArgReq::Forbidden,
        Host::Any,
        "**pp-html** — sets `innerHTML` to the expression value. Use with caution (XSS surface).",
        None,
    ),
    d(
        "bind",
        Some(':'),
        ArgReq::Required,
        Host::Any,
        "**pp-bind:attr** (shorthand `:attr`) — reactive attribute binding. E.g. `:class=\"variant\"`, `:disabled=\"busy\"`.",
        Some("rfc-020-shorthand-prefixes"),
    ),
    d(
        "on",
        Some('@'),
        ArgReq::Required,
        Host::Any,
        "**pp-on:event** (shorthand `@event`) — event handler. Modifiers: `.prevent` `.stop` `.self` `.once` `.window` `.document` `.capture` `.outside` `.debounce[.ms]`; key modifiers on keyboard events.",
        Some("rfc-013-pp-on-key-modifiers"),
    ),
    d(
        "model",
        None,
        ArgReq::Optional,
        Host::Any,
        "**pp-model** — two-way binding. Native inputs: `pp-model=\"field\"` (`.number`/`.trim`/`.lazy`). Components: `pp-model:<prop>=\"field\"`.",
        Some("rfc-009-pp-model-components"),
    ),
    d(
        "ref",
        None,
        ArgReq::Forbidden,
        Host::Any,
        "**pp-ref** — stores the element in component refs. Access via `refs::get::<T>(\"name\")` from Rust.",
        None,
    ),
    d(
        "route",
        None,
        ArgReq::Forbidden,
        Host::Any,
        "**pp-route** — marks `<a>` for SPA router interception (prevents default navigation, updates the outlet).",
        Some("rfc-003-router"),
    ),
    d(
        "teleport",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-teleport** — portal rendering. Host: `<template>` only. Body inserted at a CSS-selector target (resolved once).",
        Some("rfc-006-pp-teleport"),
    ),
    d(
        "transition",
        None,
        ArgReq::Optional,
        Host::Any,
        "**pp-transition** — enter/leave animations. Preset `pp-transition=\"fade\"`, asymmetric `:in`/`:out`, or explicit class phases `:enter` `:enter-start` `:enter-end` `:leave`…",
        Some("rfc-038-animation"),
    ),
    d(
        "anchor",
        None,
        ArgReq::Required,
        Host::Any,
        "**pp-anchor:placement** — floating-element positioning. Placements `top/bottom/left/right` × `start/center/end`; modifiers `.offset.N` `.flip` `.same-width`.",
        Some("rfc-015-pp-anchor"),
    ),
    d(
        "roving",
        None,
        ArgReq::Optional,
        Host::Any,
        "**pp-roving** — roving-tabindex keyboard navigation. `.vertical` (default) `.horizontal` `.both` `.nowrap`; `.virtual` for aria-activedescendant combobox.",
        Some("rfc-034-pp-roving-activedescendant"),
    ),
    d(
        "resize",
        None,
        ArgReq::Forbidden,
        Host::Any,
        "**pp-resize** — ResizeObserver integration. Handler gets `(width: f64, height: f64)`. Modifiers `.content-box` `.border-box` `.document`.",
        Some("rfc-016-pp-resize-pp-intersect"),
    ),
    d(
        "intersect",
        None,
        ArgReq::Optional,
        Host::Any,
        "**pp-intersect** — IntersectionObserver integration. `:enter`/`:leave` fire on visibility; `.threshold.N` `.margin.<value>`.",
        Some("rfc-016-pp-resize-pp-intersect"),
    ),
    d(
        "flip",
        None,
        ArgReq::Forbidden,
        Host::Any,
        "**pp-flip** — FLIP layout animation: surrounding DOM mutations animate this element from its old to new position. Honours `prefers-reduced-motion`.",
        Some("rfc-039-animation-v2"),
    ),
    d(
        "as",
        None,
        ArgReq::Forbidden,
        Host::ComponentOnly,
        "**pp-as** — polymorphic rendering: hoists the user's child as the component's rendered root. Requires a single `<slot>` root in the component template.",
        Some("rfc-019-pp-as"),
    ),
    d(
        "key",
        None,
        ArgReq::Forbidden,
        Host::Any,
        "**pp-key** — keyed-diffing identifier for `pp-for` (typically `item.id`). _RFC-063 (deferred) converges this to `:key` on the row root._",
        Some("rfc-007-pp-for-keys"),
    ),
    d(
        "slot",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-slot** — consumer side of a named slot; value is the slot name. Pair with `pp-let` for scoped slots. _RFC-063 (deferred) converges to `pp-slot:name=\"binding\"`._",
        Some("rfc-011-scoped-slots"),
    ),
    d(
        "let",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-let** — scope variable for a scoped slot's props, e.g. `<template pp-slot=\"item\" pp-let=\"ctx\">`. _RFC-063 (deferred) folds this into `pp-slot:name`._",
        Some("rfc-011-scoped-slots"),
    ),
    d(
        "stagger",
        None,
        ArgReq::Forbidden,
        Host::TemplateOnly,
        "**pp-stagger** — per-item stagger delay (ms) for `pp-for` enter animations. _RFC-063 (deferred) converges this into a `pp-for` modifier._",
        Some("rfc-039-animation-v2"),
    ),
];

/// Look up a live directive by head name (no `pp-`).
pub fn lookup(head: &str) -> Option<&'static DirectiveSpec> {
    DIRECTIVES.iter().find(|spec| spec.name == head)
}

/// RFC-038 transition presets — valid values for `pp-transition="…"`.
pub const TRANSITION_PRESETS: &[&str] = &[
    "fade",
    "scale",
    "fade-scale",
    "zoom",
    "slide-up",
    "slide-down",
    "slide-left",
    "slide-right",
    "collapse",
    "none",
];

/// Enumerable `:arg` values for a directive head, for completion. Empty means
/// the arg is free-form (e.g. `pp-bind:<any-attr>`) or the directive takes none.
pub fn valid_args(head: &str) -> &'static [&'static str] {
    match head {
        "transition" => &[
            "enter",
            "enter-start",
            "enter-end",
            "leave",
            "leave-start",
            "leave-end",
            "in",
            "out",
        ],
        "anchor" => &[
            "top-start",
            "top-center",
            "top-end",
            "bottom-start",
            "bottom-center",
            "bottom-end",
            "left-start",
            "left-center",
            "left-end",
            "right-start",
            "right-center",
            "right-end",
        ],
        "intersect" => &["enter", "leave"],
        _ => &[],
    }
}

/// `.modifier` names for a directive head, for completion. Empty means none.
pub fn modifiers(head: &str) -> &'static [&'static str] {
    match head {
        "on" => &[
            // event-control modifiers (RFC-057)
            "prevent",
            "stop",
            "self",
            "once",
            "window",
            "document",
            "capture",
            "passive",
            "debounce",
            "outside", //
            // key modifiers (RFC-013)
            "escape",
            "esc",
            "enter",
            "tab",
            "space",
            "backspace",
            "delete",
            "del",
            "arrow-up",
            "up",
            "arrow-down",
            "down",
            "arrow-left",
            "left",
            "arrow-right",
            "right",
            "home",
            "end",
            "page-up",
            "page-down", //
            // system-key modifiers
            "ctrl",
            "shift",
            "alt",
            "meta",
        ],
        "model" => &["number", "trim", "lazy"],
        "anchor" => &["offset", "flip", "same-width"],
        "roving" => &[
            "vertical",
            "horizontal",
            "both",
            "nowrap",
            "virtual",
            "activedescendant",
            "items",
        ],
        "resize" => &["content-box", "border-box", "document"],
        "intersect" => &["threshold", "margin"],
        _ => &[],
    }
}

/// A directive attribute split into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAttr {
    /// Head name without `pp-` (e.g. `"on"` from `pp-on:click.prevent`).
    pub head: String,
    /// The `:arg` after the head, if present.
    pub arg: Option<String>,
    /// `.modifier` segments, in source order.
    pub modifiers: Vec<String>,
}

/// Parse a raw attribute name into a [`ParsedAttr`], or `None` if it is not a
/// pocopine directive (`pp-*`, `:attr` bind shorthand, or `@event` on shorthand).
/// Plain HTML attributes return `None`.
pub fn parse_directive_attr(raw: &str) -> Option<ParsedAttr> {
    let body = if let Some(rest) = raw.strip_prefix("pp-") {
        rest.to_string()
    } else if raw.len() > 1 && raw.starts_with(':') {
        format!("bind:{}", &raw[1..])
    } else if raw.len() > 1 && raw.starts_with('@') {
        format!("on:{}", &raw[1..])
    } else {
        return None;
    };

    let (head_section, rest_after_colon) = match body.find(':') {
        Some(idx) => (&body[..idx], Some(&body[idx + 1..])),
        None => (body.as_str(), None),
    };

    let mut head_parts = head_section.split('.');
    let head = head_parts.next().unwrap_or("").to_string();
    let mut modifiers: Vec<String> = head_parts.map(|s| s.to_string()).collect();

    let mut arg = None;
    if let Some(rest) = rest_after_colon {
        let mut tail = rest.split('.');
        arg = tail.next().map(|s| s.to_string());
        modifiers.extend(tail.map(|s| s.to_string()));
    }

    Some(ParsedAttr {
        head,
        arg,
        modifiers,
    })
}

/// Whether a tag is a component tag (kebab-case with a hyphen) or the `<root>`
/// placeholder — the hosts a `component-only` directive accepts.
pub fn is_component_host(tag: &str) -> bool {
    tag == "root" || tag.contains('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_lookup() {
        assert!(removed_message("init").unwrap().contains("on_setup"));
        assert!(removed_message("cloak").unwrap().contains("synchronous"));
        assert!(removed_message("data").unwrap().contains("auto-stamps"));
        assert!(removed_message("html").is_none()); // kept by RFC-063
        assert!(lookup("init").is_none()); // removed → not live
    }

    #[test]
    fn parses_long_form() {
        let p = parse_directive_attr("pp-on:click.prevent.stop").unwrap();
        assert_eq!(p.head, "on");
        assert_eq!(p.arg.as_deref(), Some("click"));
        assert_eq!(p.modifiers, vec!["prevent", "stop"]);
    }

    #[test]
    fn parses_shorthands() {
        let bind = parse_directive_attr(":class").unwrap();
        assert_eq!(bind.head, "bind");
        assert_eq!(bind.arg.as_deref(), Some("class"));

        let on = parse_directive_attr("@keydown.enter").unwrap();
        assert_eq!(on.head, "on");
        assert_eq!(on.arg.as_deref(), Some("keydown"));
        assert_eq!(on.modifiers, vec!["enter"]);
    }

    #[test]
    fn ignores_plain_attrs() {
        assert!(parse_directive_attr("class").is_none());
        assert!(parse_directive_attr("data-id").is_none());
        assert!(parse_directive_attr(":").is_none());
        assert!(parse_directive_attr("@").is_none());
    }
}
