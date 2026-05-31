//! Typed utility registry (RFC 092 §architecture, §6 catalog).
//!
//! Each utility family declares the value type it expects, so the
//! compiler can reject `w-[red]` as a type error and `bg-surafce` as an
//! unknown-token error — both with spans. Coverage is inventory-driven:
//! the families and scales here are exactly what the `examples/`
//! actually use. Colours fall back to the built-in Tailwind palette
//! ([`crate::palette`], M2) when no `@theme` token matches; an unknown
//! class is still a *diagnostic*, not a silent miss.

use crate::diagnostics::{suggest, Diagnostic, Span};
use crate::emit::{escape_selector, Rule};
use crate::parse::ParsedClass;
use crate::tokens::ThemeTokens;
use crate::{Compilation, CompileOptions};

type Decls = Vec<(String, String)>;
/// `Some(Ok)` = this family handled it; `Some(Err)` = handled but
/// invalid (typed diagnostic); `None` = not this family, keep looking.
type Resolved = Option<Result<Decls, Diagnostic>>;

/// The value type an arbitrary `[…]` is validated against (RFC 092 D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CssType {
    Length,
    Color,
    Number,
}

impl CssType {
    /// Coarse, deterministic acceptance used for diagnostics.
    fn accepts(self, value: &str) -> bool {
        match self {
            CssType::Length => {
                value == "0"
                    || value.ends_with("px")
                    || value.ends_with("rem")
                    || value.ends_with("em")
                    || value.ends_with('%')
                    || value.ends_with("vh")
                    || value.ends_with("vw")
                    || value.ends_with("fr")
                    || value.starts_with("calc(")
                    || value.starts_with("min(")
                    || value.starts_with("max(")
                    || value.starts_with("clamp(")
                    || value.starts_with("minmax(")
                    || value.starts_with("var(")
            }
            CssType::Color => {
                value.starts_with('#')
                    || value.starts_with("rgb")
                    || value.starts_with("oklch")
                    || value.starts_with("hsl")
                    || value.starts_with("var(")
                    || matches!(value, "transparent" | "currentColor" | "white" | "black")
            }
            CssType::Number => value.parse::<f32>().is_ok(),
        }
    }
}

/// The utility registry. `builtin()` returns the framework-owned set.
#[derive(Debug)]
pub struct Registry {
    _private: (),
}

impl Registry {
    pub fn builtin() -> Self {
        Self { _private: () }
    }

    /// Resolve + emit one parsed class into `out`, recording any
    /// diagnostic. `literal` is the verbatim class text (authoritative
    /// for the selector).
    pub fn emit_into(
        &self,
        literal: &str,
        parsed: &ParsedClass,
        tokens: &ThemeTokens,
        span: Span,
        options: &CompileOptions,
        out: &mut Compilation,
    ) {
        // Author-defined component class (a real rule in the passthrough
        // CSS) — not a utility. Skip silently; its CSS is already emitted.
        if parsed.variants.is_empty()
            && parsed.arbitrary.is_none()
            && options.known_classes.contains(&parsed.base)
        {
            return;
        }

        let declarations = match self.resolve(parsed, tokens) {
            Ok(decls) => decls,
            Err(diag) => {
                // RFC 092 D5: --stylekit-compat=warn downgrades unknown
                // utilities so a half-ported example still builds.
                let diag = if options.compat_warn {
                    Diagnostic {
                        severity: crate::Severity::Warning,
                        ..diag
                    }
                } else {
                    diag
                };
                out.diagnostics.push(diag.at(span));
                return;
            }
        };

        // The selector carries its own leading `.` — `Rule::render` emits
        // it verbatim, so conditioner prefixes don't collide with the dot.
        let mut selector = format!(".{}", escape_selector(literal));
        let mut at_rule = None;
        // Ancestor (`group-*`) / sibling (`peer-*`) conditioners, in source
        // order. `true` = sibling (`~`), `false` = ancestor (descendant).
        let mut conditioners: Vec<(bool, String)> = Vec::new();
        for variant in &parsed.variants {
            match resolve_variant(&variant.0) {
                VariantResolution::Pseudo(suffix) => selector.push_str(&suffix),
                VariantResolution::Parent(sel) => conditioners.push((false, sel)),
                VariantResolution::Sibling(sel) => conditioners.push((true, sel)),
                VariantResolution::AtRule(at) => at_rule = Some(at),
                VariantResolution::Unknown => {
                    out.diagnostics.push(
                        Diagnostic::error(format!("unknown variant `{}`", variant.0)).at(span),
                    );
                    return;
                }
            }
        }
        // `space-{x,y}` and `divide-*` apply *between* children, not to the
        // element itself — append the child-combinator tail. (A negative
        // `-space-x-4` keeps the same selector shape.)
        let sd = parsed.base.strip_prefix('-').unwrap_or(&parsed.base);
        if sd.starts_with("space-y-")
            || sd.starts_with("space-x-")
            || sd.starts_with("divide-")
            || sd == "divide-x"
            || sd == "divide-y"
            || sd == "divide"
        {
            selector.push_str(" > :not([hidden]) ~ :not([hidden])");
        }
        // Prepend each `group-*`/`peer-*` conditioner, innermost (last in
        // source) closest to the element, so stacked variants chain
        // (`group-hover:group-focus:` → `.group:hover .group:focus .foo`).
        for (is_sibling, sel) in conditioners.iter().rev() {
            selector = if *is_sibling {
                format!("{sel} ~ {selector}")
            } else {
                format!("{sel} {selector}")
            };
        }

        out.css.push_str(
            &Rule {
                selector,
                declarations,
                at_rule,
            }
            .render(),
        );
    }

    /// Map a parsed base (+ optional arbitrary value) to CSS
    /// declarations, or a diagnostic.
    fn resolve(&self, parsed: &ParsedClass, tokens: &ThemeTokens) -> Result<Decls, Diagnostic> {
        let base = parsed.base.as_str();

        // Negative utilities (`-m-4`, `-inset-2`, `-translate-x-1/2`):
        // resolve the positive form, then negate the negatable
        // declarations. A `-` on a non-negatable utility (`-flex`) is an
        // error, matching Tailwind (which emits nothing).
        if let Some(pos) = base.strip_prefix('-') {
            let decls = self.resolve_base(pos, &parsed.arbitrary, tokens)?;
            return negate_decls(pos, decls);
        }
        self.resolve_base(base, &parsed.arbitrary, tokens)
    }

    /// Resolve a (non-negated) base + optional arbitrary value.
    fn resolve_base(
        &self,
        base: &str,
        arbitrary: &Option<String>,
        tokens: &ThemeTokens,
    ) -> Result<Decls, Diagnostic> {
        if let Some(value) = arbitrary {
            return resolve_arbitrary(base, value);
        }

        // 1. Exact, value-less utilities.
        if let Some(decls) = static_utility(base) {
            return Ok(decls);
        }

        // 2. Prefix families, in priority order. The first family that
        //    claims the base wins; `None` means "not mine, keep trying".
        let families: &[fn(&str, &ThemeTokens) -> Resolved] = &[
            try_spacing,
            try_space,
            try_sizing,
            try_logical_size,
            try_inset,
            try_grid,
            try_transform,
            try_border,
            try_rounded,
            try_shadow,
            try_ring,
            try_text,
            try_font,
            try_leading,
            try_tracking,
            try_underline_offset,
            try_decoration,
            try_backdrop,
            try_numeric,
            try_outline,
            try_container,
            try_misc,
            try_blend,
            try_cursor,
            try_mask,
            try_scroll,
            try_filter,
            try_gradient,
            try_divide,
            try_color,
        ];
        for family in families {
            if let Some(result) = family(base, tokens) {
                return result;
            }
        }

        // 3. Unknown utility — suggest the nearest known name.
        let mut diag = Diagnostic::error(format!("unknown utility `{base}`"));
        if let Some(hint) = suggest(base, known_bases()) {
            diag = diag.with_help(format!("did you mean `{hint}`?"));
        }
        Err(diag)
    }
}

// ── negation ────────────────────────────────────────────────────────

/// Negate the negatable declarations of an already-resolved positive
/// utility. Returns `Err` if none are negatable (so `-flex`, `-bg-red`,
/// `-z-10` are rejected, matching Tailwind's "emits nothing").
fn negate_decls(pos: &str, decls: Decls) -> Result<Decls, Diagnostic> {
    let mut negated = false;
    let out: Decls = decls
        .into_iter()
        .map(|(p, v)| {
            if negatable_prop(&p) {
                if let Some(nv) = negate_value(&v) {
                    negated = true;
                    return (p, nv);
                }
            }
            (p, v)
        })
        .collect();
    if negated {
        Ok(out)
    } else {
        Err(Diagnostic::error(format!(
            "`-{pos}` is not a negatable utility"
        )))
    }
}

/// Properties that accept a negative value (margin, position/inset,
/// scroll-margin, text-indent, order, and the transform layer vars).
fn negatable_prop(p: &str) -> bool {
    p.starts_with("margin")
        || p.starts_with("inset")
        || p.starts_with("scroll-margin")
        || matches!(
            p,
            "top" | "right" | "bottom" | "left" | "text-indent" | "order"
        )
        || matches!(
            p,
            "--pp-tx"
                | "--pp-ty"
                | "--pp-tz"
                | "--pp-rotate"
                | "--pp-rotate-x"
                | "--pp-rotate-y"
                | "--pp-rotate-z"
                | "--pp-skew-x"
                | "--pp-skew-y"
                | "--pp-scale-x"
                | "--pp-scale-y"
                | "--pp-scale-z"
        )
}

/// Negate a CSS length/number value. Simple dimensions get a `-` prefix
/// (`50%` → `-50%`); a double negative cancels; `calc()`/`var()`/min-max
/// wrap in `calc(… * -1)`; non-numeric keywords (`auto`) aren't
/// negatable (`None`).
fn negate_value(v: &str) -> Option<String> {
    match v {
        "0" | "0px" => Some("0px".into()),
        "auto" | "none" | "" => None,
        _ if v.starts_with('-') => Some(v[1..].to_string()),
        _ if v.starts_with(|c: char| c.is_ascii_digit() || c == '.') => Some(format!("-{v}")),
        _ if v.starts_with("calc(")
            || v.starts_with("var(")
            || v.starts_with("min(")
            || v.starts_with("max(")
            || v.starts_with("clamp(") =>
        {
            Some(format!("calc({v} * -1)"))
        }
        _ => None,
    }
}

// ── value helpers ───────────────────────────────────────────────────

fn decl(p: &str, v: &str) -> Decls {
    vec![(p.to_string(), v.to_string())]
}

fn trim_num(n: f64) -> String {
    format!("{n}")
}

/// Spacing scale value: `0` → `0px`, `px` → `1px`, `auto`/`full`, else
/// `calc(var(--spacing, 0.25rem) * N)` (token-driven, RFC 092 D4).
fn spacing(value: &str) -> Option<String> {
    // Fractions (`1/2` → `50%`) for inset / translate / width.
    if let Some((num, den)) = value.split_once('/') {
        let n = num.parse::<f64>().ok()?;
        let d = den.parse::<f64>().ok()?;
        if d != 0.0 {
            return Some(format!("{}%", trim_num(n / d * 100.0)));
        }
    }
    match value {
        "0" => Some("0px".into()),
        "px" => Some("1px".into()),
        "auto" => Some("auto".into()),
        "full" => Some("100%".into()),
        _ => value
            .parse::<f64>()
            .ok()
            .map(|n| format!("calc(var(--spacing, 0.25rem) * {})", trim_num(n))),
    }
}

/// Sizing value: spacing scale plus keyword sizes and the named
/// max-width container scale.
fn size(value: &str) -> Option<String> {
    match value {
        "screen" => Some("100vh".into()),
        "svh" => Some("100svh".into()),
        "lvh" => Some("100lvh".into()),
        "dvh" => Some("100dvh".into()),
        "svw" => Some("100svw".into()),
        "lvw" => Some("100lvw".into()),
        "dvw" => Some("100dvw".into()),
        "min" => Some("min-content".into()),
        "max" => Some("max-content".into()),
        "fit" => Some("fit-content".into()),
        "none" => Some("none".into()),
        "xs" => Some("20rem".into()),
        "sm" => Some("24rem".into()),
        "md" => Some("28rem".into()),
        "lg" => Some("32rem".into()),
        "xl" => Some("36rem".into()),
        "2xl" => Some("42rem".into()),
        "3xl" => Some("48rem".into()),
        _ => spacing(value),
    }
}

// ── exact utilities ─────────────────────────────────────────────────

fn static_utility(base: &str) -> Option<Decls> {
    let d = |p: &str, v: &str| Some(decl(p, v));
    let multi = |pairs: &[(&str, &str)]| {
        Some(
            pairs
                .iter()
                .map(|(p, v)| (p.to_string(), v.to_string()))
                .collect::<Decls>(),
        )
    };
    match base {
        // display
        "block" => d("display", "block"),
        "inline" => d("display", "inline"),
        "inline-block" => d("display", "inline-block"),
        "flex" => d("display", "flex"),
        "inline-flex" => d("display", "inline-flex"),
        "grid" => d("display", "grid"),
        "inline-grid" => d("display", "inline-grid"),
        "contents" => d("display", "contents"),
        "hidden" => d("display", "none"),
        // `group` is a marker class for `group-*` variants — no own style.
        "group" => Some(Vec::new()),
        // position
        "relative" => d("position", "relative"),
        "absolute" => d("position", "absolute"),
        "fixed" => d("position", "fixed"),
        "sticky" => d("position", "sticky"),
        "static" => d("position", "static"),
        // flexbox
        "flex-row" => d("flex-direction", "row"),
        "flex-col" => d("flex-direction", "column"),
        "flex-wrap" => d("flex-wrap", "wrap"),
        "flex-wrap-reverse" => d("flex-wrap", "wrap-reverse"),
        "flex-nowrap" => d("flex-wrap", "nowrap"),
        "flex-none" => d("flex", "none"),
        "flex-1" => d("flex", "1 1 0%"),
        "flex-auto" => d("flex", "1 1 auto"),
        "flex-initial" => d("flex", "0 1 auto"),
        "grow" => d("flex-grow", "1"),
        "grow-0" => d("flex-grow", "0"),
        "shrink" => d("flex-shrink", "1"),
        "shrink-0" => d("flex-shrink", "0"),
        "items-center" => d("align-items", "center"),
        "items-start" => d("align-items", "flex-start"),
        "items-end" => d("align-items", "flex-end"),
        "items-baseline" => d("align-items", "baseline"),
        "items-stretch" => d("align-items", "stretch"),
        "justify-center" => d("justify-content", "center"),
        "justify-start" => d("justify-content", "flex-start"),
        "justify-end" => d("justify-content", "flex-end"),
        "justify-between" => d("justify-content", "space-between"),
        "justify-around" => d("justify-content", "space-around"),
        "justify-evenly" => d("justify-content", "space-evenly"),
        "justify-normal" => d("justify-content", "normal"),
        "justify-stretch" => d("justify-content", "stretch"),
        "grid-cols-none" => d("grid-template-columns", "none"),
        "grid-cols-subgrid" => d("grid-template-columns", "subgrid"),
        "grid-rows-none" => d("grid-template-rows", "none"),
        "grid-rows-subgrid" => d("grid-template-rows", "subgrid"),
        // text align + transform + decoration line
        "text-left" => d("text-align", "left"),
        "text-center" => d("text-align", "center"),
        "text-right" => d("text-align", "right"),
        "text-justify" => d("text-align", "justify"),
        "uppercase" => d("text-transform", "uppercase"),
        "lowercase" => d("text-transform", "lowercase"),
        "tabular-nums" => d("font-variant-numeric", "tabular-nums"),
        "normal-nums" => d("font-variant-numeric", "normal"),
        "capitalize" => d("text-transform", "capitalize"),
        "underline" => d("text-decoration-line", "underline"),
        "line-through" => d("text-decoration-line", "line-through"),
        "no-underline" => d("text-decoration-line", "none"),
        "truncate" => multi(&[
            ("overflow", "hidden"),
            ("text-overflow", "ellipsis"),
            ("white-space", "nowrap"),
        ]),
        "antialiased" => multi(&[
            ("-webkit-font-smoothing", "antialiased"),
            ("-moz-osx-font-smoothing", "grayscale"),
        ]),
        "subpixel-antialiased" => multi(&[
            ("-webkit-font-smoothing", "auto"),
            ("-moz-osx-font-smoothing", "auto"),
        ]),
        "sr-only" => multi(&[
            ("position", "absolute"),
            ("width", "1px"),
            ("height", "1px"),
            ("padding", "0"),
            ("margin", "-1px"),
            ("overflow", "hidden"),
            ("clip", "rect(0, 0, 0, 0)"),
            ("white-space", "nowrap"),
            ("border-width", "0"),
        ]),
        "not-sr-only" => multi(&[
            ("position", "static"),
            ("width", "auto"),
            ("height", "auto"),
            ("padding", "0"),
            ("margin", "0"),
            ("overflow", "visible"),
            ("clip", "auto"),
            ("white-space", "normal"),
        ]),
        // overflow-wrap (Tailwind v4 `wrap-*`)
        "wrap-normal" => d("overflow-wrap", "normal"),
        "wrap-break-word" => d("overflow-wrap", "break-word"),
        "wrap-anywhere" => d("overflow-wrap", "anywhere"),
        // cursor / pointer
        "cursor-pointer" => d("cursor", "pointer"),
        "cursor-not-allowed" => d("cursor", "not-allowed"),
        "cursor-wait" => d("cursor", "wait"),
        "cursor-default" => d("cursor", "default"),
        "pointer-events-none" => d("pointer-events", "none"),
        "pointer-events-auto" => d("pointer-events", "auto"),
        // overflow
        "overflow-hidden" => d("overflow", "hidden"),
        "overflow-visible" => d("overflow", "visible"),
        "overflow-auto" => d("overflow", "auto"),
        "overflow-scroll" => d("overflow", "scroll"),
        "overflow-x-auto" => d("overflow-x", "auto"),
        "overflow-y-auto" => d("overflow-y", "auto"),
        "overflow-x-hidden" => d("overflow-x", "hidden"),
        "overflow-y-hidden" => d("overflow-y", "hidden"),
        // outline (width/colour live in `try_outline`)
        "outline-none" => multi(&[
            ("outline", "2px solid transparent"),
            ("outline-offset", "2px"),
        ]),
        // For forced-colors a11y, `outline-hidden` keeps a transparent
        // outline rather than removing it (matches Tailwind v4).
        "outline-hidden" => multi(&[
            ("outline", "2px solid transparent"),
            ("outline-offset", "2px"),
        ]),
        "outline" => multi(&[("outline-style", "solid"), ("outline-width", "1px")]),
        "outline-solid" => d("outline-style", "solid"),
        "outline-dashed" => d("outline-style", "dashed"),
        "outline-dotted" => d("outline-style", "dotted"),
        "outline-double" => d("outline-style", "double"),
        // borders (style / bare width)
        "border" => d("border-width", "1px"),
        "border-0" => d("border-width", "0px"),
        "border-solid" => d("border-style", "solid"),
        "border-dashed" => d("border-style", "dashed"),
        "border-dotted" => d("border-style", "dotted"),
        "border-double" => d("border-style", "double"),
        "border-hidden" => d("border-style", "hidden"),
        "border-none" => d("border-style", "none"),
        "border-t" => d("border-top-width", "1px"),
        "border-r" => d("border-right-width", "1px"),
        "border-b" => d("border-bottom-width", "1px"),
        "border-l" => d("border-left-width", "1px"),
        "border-t-0" => d("border-top-width", "0px"),
        "border-b-0" => d("border-bottom-width", "0px"),
        // line-height keyword
        "leading-none" => d("line-height", "1"),
        // transitions
        "transition" => multi(&[
            (
                "transition-property",
                "color, background-color, border-color, text-decoration-color, fill, stroke, \
                 opacity, box-shadow, transform, filter, backdrop-filter",
            ),
            ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"),
            ("transition-duration", "150ms"),
        ]),
        "transition-colors" => multi(&[
            (
                "transition-property",
                "color, background-color, border-color, text-decoration-color, fill, stroke",
            ),
            ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"),
            ("transition-duration", "150ms"),
        ]),
        "transition-none" => d("transition-property", "none"),
        "transition-all" => multi(&[
            ("transition-property", "all"),
            ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"),
            ("transition-duration", "150ms"),
        ]),
        "transition-transform" => multi(&[
            ("transition-property", "transform, translate, scale, rotate"),
            ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"),
            ("transition-duration", "150ms"),
        ]),
        "transition-opacity" => multi(&[
            ("transition-property", "opacity"),
            ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"),
            ("transition-duration", "150ms"),
        ]),
        "transition-shadow" => multi(&[
            ("transition-property", "box-shadow"),
            ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"),
            ("transition-duration", "150ms"),
        ]),
        // transition timing functions
        "ease-linear" => d("transition-timing-function", "linear"),
        "ease-in" => d("transition-timing-function", "cubic-bezier(0.4, 0, 1, 1)"),
        "ease-out" => d("transition-timing-function", "cubic-bezier(0, 0, 0.2, 1)"),
        "ease-in-out" => d("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"),
        // backdrop-filter (composable chain — see `try_backdrop`)
        "backdrop-filter" => Some(backdrop_decls("--pp-bd-blur", "var(--pp-bd-blur, )")),
        "backdrop-filter-none" => d("backdrop-filter", "none"),
        "backdrop-blur" => Some(backdrop_decls("--pp-bd-blur", "blur(8px)")),
        "backdrop-grayscale" => Some(backdrop_decls("--pp-bd-grayscale", "grayscale(100%)")),
        "backdrop-grayscale-0" => Some(backdrop_decls("--pp-bd-grayscale", "grayscale(0)")),
        "backdrop-invert" => Some(backdrop_decls("--pp-bd-invert", "invert(100%)")),
        "backdrop-invert-0" => Some(backdrop_decls("--pp-bd-invert", "invert(0)")),
        "backdrop-sepia" => Some(backdrop_decls("--pp-bd-sepia", "sepia(100%)")),
        "backdrop-sepia-0" => Some(backdrop_decls("--pp-bd-sepia", "sepia(0)")),
        // object-fit
        "object-contain" => d("object-fit", "contain"),
        "object-cover" => d("object-fit", "cover"),
        "object-fill" => d("object-fit", "fill"),
        "object-none" => d("object-fit", "none"),
        "object-scale-down" => d("object-fit", "scale-down"),
        // word-break / wrapping
        "break-all" => d("word-break", "break-all"),
        "break-keep" => d("word-break", "keep-all"),
        "break-words" => d("overflow-wrap", "break-word"),
        "break-normal" => multi(&[("overflow-wrap", "normal"), ("word-break", "normal")]),
        "whitespace-nowrap" => d("white-space", "nowrap"),
        "whitespace-normal" => d("white-space", "normal"),
        "whitespace-pre" => d("white-space", "pre"),
        // resize
        "resize" => d("resize", "both"),
        "resize-x" => d("resize", "horizontal"),
        "resize-y" => d("resize", "vertical"),
        "resize-none" => d("resize", "none"),
        // animation (keyframes for these live in Preflight)
        "animate-spin" => d("animation", "spin 1s linear infinite"),
        "animate-none" => d("animation", "none"),
        "animate-ping" => d("animation", "ping 1s cubic-bezier(0, 0, 0.2, 1) infinite"),
        "animate-pulse" => d(
            "animation",
            "pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite",
        ),
        "animate-bounce" => d("animation", "bounce 1s infinite"),
        // box-sizing
        "box-border" => d("box-sizing", "border-box"),
        "box-content" => d("box-sizing", "content-box"),
        // float / clear
        "float-left" => d("float", "left"),
        "float-right" => d("float", "right"),
        "float-none" => d("float", "none"),
        "float-start" => d("float", "inline-start"),
        "float-end" => d("float", "inline-end"),
        "clear-left" => d("clear", "left"),
        "clear-right" => d("clear", "right"),
        "clear-both" => d("clear", "both"),
        "clear-none" => d("clear", "none"),
        // isolation / visibility
        "isolate" => d("isolation", "isolate"),
        "isolation-auto" => d("isolation", "auto"),
        "visible" => d("visibility", "visible"),
        "invisible" => d("visibility", "hidden"),
        "collapse" => d("visibility", "collapse"),
        // grid flow + auto placement keywords
        "grid-flow-row" => d("grid-auto-flow", "row"),
        "grid-flow-col" => d("grid-auto-flow", "column"),
        "grid-flow-dense" => d("grid-auto-flow", "dense"),
        "grid-flow-row-dense" => d("grid-auto-flow", "row dense"),
        "grid-flow-col-dense" => d("grid-auto-flow", "column dense"),
        "auto-cols-auto" => d("grid-auto-columns", "auto"),
        "auto-cols-min" => d("grid-auto-columns", "min-content"),
        "auto-cols-max" => d("grid-auto-columns", "max-content"),
        "auto-cols-fr" => d("grid-auto-columns", "minmax(0, 1fr)"),
        "auto-rows-auto" => d("grid-auto-rows", "auto"),
        "auto-rows-min" => d("grid-auto-rows", "min-content"),
        "auto-rows-max" => d("grid-auto-rows", "max-content"),
        "auto-rows-fr" => d("grid-auto-rows", "minmax(0, 1fr)"),
        // align-content / place-* / *-items / *-self
        "content-normal" => d("align-content", "normal"),
        "content-baseline" => d("align-content", "baseline"),
        "content-stretch" => d("align-content", "stretch"),
        "content-center" => d("align-content", "center"),
        "content-start" => d("align-content", "flex-start"),
        "content-end" => d("align-content", "flex-end"),
        "content-between" => d("align-content", "space-between"),
        "content-around" => d("align-content", "space-around"),
        "content-evenly" => d("align-content", "space-evenly"),
        "self-auto" => d("align-self", "auto"),
        "self-center" => d("align-self", "center"),
        "self-start" => d("align-self", "flex-start"),
        "self-end" => d("align-self", "flex-end"),
        "self-stretch" => d("align-self", "stretch"),
        "self-baseline" => d("align-self", "baseline"),
        "justify-items-center" => d("justify-items", "center"),
        "justify-items-start" => d("justify-items", "start"),
        "justify-items-end" => d("justify-items", "end"),
        "justify-items-stretch" => d("justify-items", "stretch"),
        "justify-self-auto" => d("justify-self", "auto"),
        "justify-self-center" => d("justify-self", "center"),
        "justify-self-start" => d("justify-self", "start"),
        "justify-self-end" => d("justify-self", "end"),
        "justify-self-stretch" => d("justify-self", "stretch"),
        "place-content-center" => d("place-content", "center"),
        "place-content-start" => d("place-content", "start"),
        "place-content-end" => d("place-content", "end"),
        "place-content-between" => d("place-content", "space-between"),
        "place-content-around" => d("place-content", "space-around"),
        "place-content-evenly" => d("place-content", "space-evenly"),
        "place-content-baseline" => d("place-content", "baseline"),
        "place-content-stretch" => d("place-content", "stretch"),
        "place-items-center" => d("place-items", "center"),
        "place-items-start" => d("place-items", "start"),
        "place-items-end" => d("place-items", "end"),
        "place-items-stretch" => d("place-items", "stretch"),
        "place-self-auto" => d("place-self", "auto"),
        "place-self-center" => d("place-self", "center"),
        "place-self-start" => d("place-self", "start"),
        "place-self-end" => d("place-self", "end"),
        "place-self-stretch" => d("place-self", "stretch"),
        // object-position
        "object-top" => d("object-position", "top"),
        "object-bottom" => d("object-position", "bottom"),
        "object-center" => d("object-position", "center"),
        "object-left" => d("object-position", "left"),
        "object-right" => d("object-position", "right"),
        // transform reset
        "transform-none" => d("transform", "none"),
        // typography
        "italic" => d("font-style", "italic"),
        "not-italic" => d("font-style", "normal"),
        "overline" => d("text-decoration-line", "overline"),
        "text-wrap" => d("text-wrap", "wrap"),
        "text-nowrap" => d("text-wrap", "nowrap"),
        "text-balance" => d("text-wrap", "balance"),
        "text-pretty" => d("text-wrap", "pretty"),
        "text-ellipsis" => d("text-overflow", "ellipsis"),
        "text-clip" => d("text-overflow", "clip"),
        "align-baseline" => d("vertical-align", "baseline"),
        "align-top" => d("vertical-align", "top"),
        "align-middle" => d("vertical-align", "middle"),
        "align-bottom" => d("vertical-align", "bottom"),
        "align-text-top" => d("vertical-align", "text-top"),
        "align-text-bottom" => d("vertical-align", "text-bottom"),
        "align-sub" => d("vertical-align", "sub"),
        "align-super" => d("vertical-align", "super"),
        "ordinal" => d("font-variant-numeric", "ordinal"),
        "slashed-zero" => d("font-variant-numeric", "slashed-zero"),
        "content-none" => d("content", "none"),
        // lists
        "list-disc" => d("list-style-type", "disc"),
        "list-decimal" => d("list-style-type", "decimal"),
        "list-none" => d("list-style-type", "none"),
        "list-inside" => d("list-style-position", "inside"),
        "list-outside" => d("list-style-position", "outside"),
        // tables
        "border-collapse" => d("border-collapse", "collapse"),
        "border-separate" => d("border-collapse", "separate"),
        "table-auto" => d("table-layout", "auto"),
        "table-fixed" => d("table-layout", "fixed"),
        "caption-top" => d("caption-side", "top"),
        "caption-bottom" => d("caption-side", "bottom"),
        // forms / interaction
        "appearance-none" => d("appearance", "none"),
        "appearance-auto" => d("appearance", "auto"),
        "select-none" => d("user-select", "none"),
        "select-text" => d("user-select", "text"),
        "select-all" => d("user-select", "all"),
        "select-auto" => d("user-select", "auto"),
        "will-change-auto" => d("will-change", "auto"),
        "will-change-scroll" => d("will-change", "scroll-position"),
        "will-change-contents" => d("will-change", "contents"),
        "will-change-transform" => d("will-change", "transform"),
        // scroll behaviour / snap
        "scroll-auto" => d("scroll-behavior", "auto"),
        "scroll-smooth" => d("scroll-behavior", "smooth"),
        "snap-none" => d("scroll-snap-type", "none"),
        "snap-x" => d("scroll-snap-type", "x var(--pp-snap-strictness, proximity)"),
        "snap-y" => d("scroll-snap-type", "y var(--pp-snap-strictness, proximity)"),
        "snap-both" => d(
            "scroll-snap-type",
            "both var(--pp-snap-strictness, proximity)",
        ),
        "snap-mandatory" => d("--pp-snap-strictness", "mandatory"),
        "snap-proximity" => d("--pp-snap-strictness", "proximity"),
        "snap-start" => d("scroll-snap-align", "start"),
        "snap-end" => d("scroll-snap-align", "end"),
        "snap-center" => d("scroll-snap-align", "center"),
        "snap-align-none" => d("scroll-snap-align", "none"),
        "snap-normal" => d("scroll-snap-stop", "normal"),
        "snap-always" => d("scroll-snap-stop", "always"),
        // blend modes (common values)
        "mix-blend-normal" => d("mix-blend-mode", "normal"),
        "mix-blend-multiply" => d("mix-blend-mode", "multiply"),
        "mix-blend-screen" => d("mix-blend-mode", "screen"),
        "mix-blend-overlay" => d("mix-blend-mode", "overlay"),
        "mix-blend-darken" => d("mix-blend-mode", "darken"),
        "mix-blend-lighten" => d("mix-blend-mode", "lighten"),
        "mix-blend-difference" => d("mix-blend-mode", "difference"),
        // overscroll
        "overscroll-auto" => d("overscroll-behavior", "auto"),
        "overscroll-contain" => d("overscroll-behavior", "contain"),
        "overscroll-none" => d("overscroll-behavior", "none"),
        "overscroll-y-auto" => d("overscroll-behavior-y", "auto"),
        "overscroll-y-contain" => d("overscroll-behavior-y", "contain"),
        "overscroll-y-none" => d("overscroll-behavior-y", "none"),
        "overscroll-x-auto" => d("overscroll-behavior-x", "auto"),
        "overscroll-x-contain" => d("overscroll-behavior-x", "contain"),
        "overscroll-x-none" => d("overscroll-behavior-x", "none"),
        // filter chain (composable, like transforms)
        "filter" => Some(filter_decls("--pp-blur", "var(--pp-blur, )")),
        "filter-none" => d("filter", "none"),
        "blur" => Some(filter_decls("--pp-blur", "blur(8px)")),
        "blur-none" => Some(filter_decls("--pp-blur", " ")),
        "blur-xs" => Some(filter_decls("--pp-blur", "blur(2px)")),
        "blur-sm" => Some(filter_decls("--pp-blur", "blur(4px)")),
        "blur-md" => Some(filter_decls("--pp-blur", "blur(12px)")),
        "blur-lg" => Some(filter_decls("--pp-blur", "blur(16px)")),
        "blur-xl" => Some(filter_decls("--pp-blur", "blur(24px)")),
        "blur-2xl" => Some(filter_decls("--pp-blur", "blur(40px)")),
        "blur-3xl" => Some(filter_decls("--pp-blur", "blur(64px)")),
        "grayscale" => Some(filter_decls("--pp-grayscale", "grayscale(100%)")),
        "grayscale-0" => Some(filter_decls("--pp-grayscale", "grayscale(0)")),
        "invert" => Some(filter_decls("--pp-invert", "invert(100%)")),
        "invert-0" => Some(filter_decls("--pp-invert", "invert(0)")),
        "sepia" => Some(filter_decls("--pp-sepia", "sepia(100%)")),
        "sepia-0" => Some(filter_decls("--pp-sepia", "sepia(0)")),
        // transform-origin
        "origin-center" => d("transform-origin", "center"),
        "origin-top" => d("transform-origin", "top"),
        "origin-bottom" => d("transform-origin", "bottom"),
        "origin-left" => d("transform-origin", "left"),
        "origin-right" => d("transform-origin", "right"),
        "origin-top-left" => d("transform-origin", "top left"),
        "origin-top-right" => d("transform-origin", "top right"),
        "origin-bottom-left" => d("transform-origin", "bottom left"),
        "origin-bottom-right" => d("transform-origin", "bottom right"),
        // linear-gradient direction (stops set by from-/via-/to-)
        "bg-linear-to-t" => gradient_dir("to top"),
        "bg-linear-to-b" => gradient_dir("to bottom"),
        "bg-linear-to-l" => gradient_dir("to left"),
        "bg-linear-to-r" => gradient_dir("to right"),
        "bg-linear-to-tl" => gradient_dir("to top left"),
        "bg-linear-to-tr" => gradient_dir("to top right"),
        "bg-linear-to-bl" => gradient_dir("to bottom left"),
        "bg-linear-to-br" => gradient_dir("to bottom right"),
        "bg-conic" => d(
            "background-image",
            "conic-gradient(from 0deg, var(--pp-grad-stops))",
        ),
        "bg-radial" => d("background-image", "radial-gradient(var(--pp-grad-stops))"),
        "bg-none" => d("background-image", "none"),
        // divide width (bare) + styles, applied to children via emit_into tail
        "divide-x" => d("border-left-width", "1px"),
        "divide-y" => d("border-top-width", "1px"),
        "divide-solid" => d("border-style", "solid"),
        "divide-dashed" => d("border-style", "dashed"),
        "divide-dotted" => d("border-style", "dotted"),
        "divide-none" => d("border-style", "none"),
        "divide-x-reverse" => d("--pp-divide-x-reverse", "1"),
        "divide-y-reverse" => d("--pp-divide-y-reverse", "1"),
        // background-clip / origin
        "bg-clip-border" => d("background-clip", "border-box"),
        "bg-clip-padding" => d("background-clip", "padding-box"),
        "bg-clip-content" => d("background-clip", "content-box"),
        "bg-clip-text" => multi(&[
            ("background-clip", "text"),
            ("-webkit-background-clip", "text"),
        ]),
        "bg-origin-border" => d("background-origin", "border-box"),
        "bg-origin-padding" => d("background-origin", "padding-box"),
        "bg-origin-content" => d("background-origin", "content-box"),
        // background-size / position / repeat
        "bg-auto" => d("background-size", "auto"),
        "bg-cover" => d("background-size", "cover"),
        "bg-contain" => d("background-size", "contain"),
        "bg-center" => d("background-position", "center"),
        "bg-top" => d("background-position", "top"),
        "bg-bottom" => d("background-position", "bottom"),
        "bg-left" => d("background-position", "left"),
        "bg-right" => d("background-position", "right"),
        "bg-top-left" => d("background-position", "left top"),
        "bg-top-right" => d("background-position", "right top"),
        "bg-bottom-left" => d("background-position", "left bottom"),
        "bg-bottom-right" => d("background-position", "right bottom"),
        "bg-left-top" => d("background-position", "left top"),
        "bg-right-top" => d("background-position", "right top"),
        "bg-left-bottom" => d("background-position", "left bottom"),
        "bg-right-bottom" => d("background-position", "right bottom"),
        "bg-repeat" => d("background-repeat", "repeat"),
        "bg-repeat-space" => d("background-repeat", "space"),
        "bg-repeat-round" => d("background-repeat", "round"),
        "bg-no-repeat" => d("background-repeat", "no-repeat"),
        "bg-repeat-x" => d("background-repeat", "repeat-x"),
        "bg-repeat-y" => d("background-repeat", "repeat-y"),
        "bg-fixed" => d("background-attachment", "fixed"),
        "bg-local" => d("background-attachment", "local"),
        "bg-scroll" => d("background-attachment", "scroll"),
        // background-blend-mode
        "bg-blend-normal" => d("background-blend-mode", "normal"),
        "bg-blend-multiply" => d("background-blend-mode", "multiply"),
        "bg-blend-screen" => d("background-blend-mode", "screen"),
        "bg-blend-overlay" => d("background-blend-mode", "overlay"),
        "bg-blend-darken" => d("background-blend-mode", "darken"),
        "bg-blend-lighten" => d("background-blend-mode", "lighten"),
        // break-before / -after / -inside (columns + pages)
        "break-before-auto" => d("break-before", "auto"),
        "break-before-avoid" => d("break-before", "avoid"),
        "break-before-page" => d("break-before", "page"),
        "break-before-column" => d("break-before", "column"),
        "break-before-all" => d("break-before", "all"),
        "break-before-left" => d("break-before", "left"),
        "break-before-right" => d("break-before", "right"),
        "break-before-avoid-page" => d("break-before", "avoid-page"),
        "break-before-avoid-column" => d("break-before", "avoid-column"),
        "break-after-auto" => d("break-after", "auto"),
        "break-after-avoid" => d("break-after", "avoid"),
        "break-after-page" => d("break-after", "page"),
        "break-after-column" => d("break-after", "column"),
        "break-after-all" => d("break-after", "all"),
        "break-after-left" => d("break-after", "left"),
        "break-after-right" => d("break-after", "right"),
        "break-after-avoid-page" => d("break-after", "avoid-page"),
        "break-after-avoid-column" => d("break-after", "avoid-column"),
        "break-inside-auto" => d("break-inside", "auto"),
        "break-inside-avoid" => d("break-inside", "avoid"),
        "break-inside-avoid-page" => d("break-inside", "avoid-page"),
        "break-inside-avoid-column" => d("break-inside", "avoid-column"),
        // text-shadow scale
        "text-shadow-none" => d("text-shadow", "none"),
        "text-shadow-2xs" => d("text-shadow", "0 1px 0 rgb(0 0 0 / 0.15)"),
        "text-shadow-xs" => d("text-shadow", "0 1px 1px rgb(0 0 0 / 0.2)"),
        "text-shadow-sm" => d(
            "text-shadow",
            "0 1px 2px rgb(0 0 0 / 0.15), 0 1px 3px rgb(0 0 0 / 0.1)",
        ),
        "text-shadow-md" => d(
            "text-shadow",
            "0 1px 2px rgb(0 0 0 / 0.15), 0 2px 4px rgb(0 0 0 / 0.15)",
        ),
        "text-shadow-lg" => d(
            "text-shadow",
            "0 1px 3px rgb(0 0 0 / 0.2), 0 4px 8px rgb(0 0 0 / 0.2)",
        ),
        // 3D context
        "transform-flat" => d("transform-style", "flat"),
        "transform-3d" => d("transform-style", "preserve-3d"),
        // `scale-3d`/`translate-3d` enable the composed 3D transform; the
        // per-axis values come from `scale-z-*`/`translate-z-*` etc.
        "scale-3d" => d("transform", TRANSFORM_CHAIN),
        "translate-3d" => d("transform", TRANSFORM_CHAIN),
        "backface-visible" => d("backface-visibility", "visible"),
        "backface-hidden" => d("backface-visibility", "hidden"),
        "perspective-origin-center" => d("perspective-origin", "center"),
        "perspective-origin-top" => d("perspective-origin", "top"),
        "perspective-origin-bottom" => d("perspective-origin", "bottom"),
        "perspective-origin-left" => d("perspective-origin", "left"),
        "perspective-origin-right" => d("perspective-origin", "right"),
        "perspective-none" => d("perspective", "none"),
        // touch-action
        "touch-auto" => d("touch-action", "auto"),
        "touch-none" => d("touch-action", "none"),
        "touch-pan-x" => d("touch-action", "pan-x"),
        "touch-pan-y" => d("touch-action", "pan-y"),
        "touch-pan-left" => d("touch-action", "pan-left"),
        "touch-pan-right" => d("touch-action", "pan-right"),
        "touch-pan-up" => d("touch-action", "pan-up"),
        "touch-pan-down" => d("touch-action", "pan-down"),
        "touch-pinch-zoom" => d("touch-action", "pinch-zoom"),
        "touch-manipulation" => d("touch-action", "manipulation"),
        // scrollbar-gutter
        "scrollbar-gutter-auto" => d("scrollbar-gutter", "auto"),
        "scrollbar-gutter-stable" => d("scrollbar-gutter", "stable"),
        "scrollbar-gutter-both-edges" => d("scrollbar-gutter", "stable both-edges"),
        // hyphens
        "hyphens-none" => d("hyphens", "none"),
        "hyphens-manual" => d("hyphens", "manual"),
        "hyphens-auto" => d("hyphens", "auto"),
        // contain
        "contain-none" => d("contain", "none"),
        "contain-content" => d("contain", "content"),
        "contain-strict" => d("contain", "strict"),
        "contain-layout" => d("contain", "layout"),
        "contain-paint" => d("contain", "paint"),
        "contain-size" => d("contain", "size"),
        "contain-style" => d("contain", "style"),
        "contain-inline-size" => d("contain", "inline-size"),
        // field-sizing
        "field-sizing-content" => d("field-sizing", "content"),
        "field-sizing-fixed" => d("field-sizing", "fixed"),
        // box-decoration
        "box-decoration-clone" => d("box-decoration-break", "clone"),
        "box-decoration-slice" => d("box-decoration-break", "slice"),
        // forced-color-adjust
        "forced-color-adjust-auto" => d("forced-color-adjust", "auto"),
        "forced-color-adjust-none" => d("forced-color-adjust", "none"),
        // transition-behavior
        "transition-normal" => d("transition-behavior", "normal"),
        "transition-discrete" => d("transition-behavior", "allow-discrete"),
        // color-scheme
        "scheme-normal" => d("color-scheme", "normal"),
        "scheme-light" => d("color-scheme", "light"),
        "scheme-dark" => d("color-scheme", "dark"),
        "scheme-light-dark" => d("color-scheme", "light dark"),
        // lists / reverse
        "list-image-none" => d("list-style-image", "none"),
        "space-x-reverse" => d("--pp-space-x-reverse", "1"),
        "space-y-reverse" => d("--pp-space-y-reverse", "1"),
        // ── masks (box properties) ──
        "mask-none" => multi(&[("-webkit-mask-image", "none"), ("mask-image", "none")]),
        "mask-clip-border" => multi(&[
            ("-webkit-mask-clip", "border-box"),
            ("mask-clip", "border-box"),
        ]),
        "mask-clip-padding" => multi(&[
            ("-webkit-mask-clip", "padding-box"),
            ("mask-clip", "padding-box"),
        ]),
        "mask-clip-content" => multi(&[
            ("-webkit-mask-clip", "content-box"),
            ("mask-clip", "content-box"),
        ]),
        "mask-clip-fill" => multi(&[("-webkit-mask-clip", "fill-box"), ("mask-clip", "fill-box")]),
        "mask-clip-stroke" => multi(&[
            ("-webkit-mask-clip", "stroke-box"),
            ("mask-clip", "stroke-box"),
        ]),
        "mask-clip-view" => multi(&[("-webkit-mask-clip", "view-box"), ("mask-clip", "view-box")]),
        "mask-no-clip" => multi(&[("-webkit-mask-clip", "no-clip"), ("mask-clip", "no-clip")]),
        "mask-origin-border" => multi(&[
            ("-webkit-mask-origin", "border-box"),
            ("mask-origin", "border-box"),
        ]),
        "mask-origin-padding" => multi(&[
            ("-webkit-mask-origin", "padding-box"),
            ("mask-origin", "padding-box"),
        ]),
        "mask-origin-content" => multi(&[
            ("-webkit-mask-origin", "content-box"),
            ("mask-origin", "content-box"),
        ]),
        "mask-type-luminance" => d("mask-type", "luminance"),
        "mask-type-alpha" => d("mask-type", "alpha"),
        "mask-add" => multi(&[
            ("-webkit-mask-composite", "source-over"),
            ("mask-composite", "add"),
        ]),
        "mask-subtract" => multi(&[
            ("-webkit-mask-composite", "source-out"),
            ("mask-composite", "subtract"),
        ]),
        "mask-intersect" => multi(&[
            ("-webkit-mask-composite", "source-in"),
            ("mask-composite", "intersect"),
        ]),
        "mask-exclude" => multi(&[
            ("-webkit-mask-composite", "xor"),
            ("mask-composite", "exclude"),
        ]),
        "mask-repeat" => multi(&[("-webkit-mask-repeat", "repeat"), ("mask-repeat", "repeat")]),
        "mask-no-repeat" => multi(&[
            ("-webkit-mask-repeat", "no-repeat"),
            ("mask-repeat", "no-repeat"),
        ]),
        "mask-repeat-x" => multi(&[
            ("-webkit-mask-repeat", "repeat-x"),
            ("mask-repeat", "repeat-x"),
        ]),
        "mask-repeat-y" => multi(&[
            ("-webkit-mask-repeat", "repeat-y"),
            ("mask-repeat", "repeat-y"),
        ]),
        "mask-repeat-round" => multi(&[("-webkit-mask-repeat", "round"), ("mask-repeat", "round")]),
        "mask-repeat-space" => multi(&[("-webkit-mask-repeat", "space"), ("mask-repeat", "space")]),
        _ => None,
    }
}

/// A composable-chain emission: set one layer var, then (re)emit the
/// `property: chain` that references every layer's var. Shared by the
/// filter / transform / shadow families so `blur-sm grayscale`,
/// `translate-x-2 rotate-45`, `shadow-xl ring-1` each stack.
fn chain_decls(layer: &str, value: &str, property: &str, chain: &str) -> Decls {
    vec![
        (layer.to_string(), value.to_string()),
        (property.to_string(), chain.to_string()),
    ]
}

/// Composable filter chain — each filter utility sets its own var and
/// emits this identical `filter`, so `blur-sm grayscale` apply together.
const FILTER_CHAIN: &str = "var(--pp-blur, ) var(--pp-brightness, ) var(--pp-contrast, ) var(--pp-grayscale, ) var(--pp-saturate, ) var(--pp-invert, ) var(--pp-sepia, )";

fn filter_decls(layer: &str, value: &str) -> Decls {
    chain_decls(layer, value, "filter", FILTER_CHAIN)
}

/// Composable backdrop-filter chain — mirrors [`FILTER_CHAIN`] for the
/// `backdrop-*` family so `backdrop-blur-sm backdrop-grayscale` apply
/// together. Emits the `-webkit-` prefix for Safari.
const BACKDROP_CHAIN: &str = "var(--pp-bd-blur, ) var(--pp-bd-brightness, ) var(--pp-bd-contrast, ) var(--pp-bd-grayscale, ) var(--pp-bd-saturate, ) var(--pp-bd-invert, ) var(--pp-bd-sepia, ) var(--pp-bd-opacity, )";

fn backdrop_decls(layer: &str, value: &str) -> Decls {
    vec![
        (layer.to_string(), value.to_string()),
        (
            "-webkit-backdrop-filter".to_string(),
            BACKDROP_CHAIN.to_string(),
        ),
        ("backdrop-filter".to_string(), BACKDROP_CHAIN.to_string()),
    ]
}

fn gradient_dir(dir: &str) -> Option<Decls> {
    Some(decl(
        "background-image",
        &format!("linear-gradient({dir}, var(--pp-grad-stops))"),
    ))
}

// ── prefix families ─────────────────────────────────────────────────

/// Margin/padding/gap edge properties for a spacing prefix. Shared by
/// `try_spacing` (scale values) and `resolve_arbitrary` (`m-[4px]`).
/// `gap-x`/`gap-y` map to column-/row-gap and are handled by the caller.
fn spacing_props(prefix: &str) -> Option<&'static [&'static str]> {
    Some(match prefix {
        "p" => &["padding"],
        "px" => &["padding-left", "padding-right"],
        "py" => &["padding-top", "padding-bottom"],
        "pt" => &["padding-top"],
        "pb" => &["padding-bottom"],
        "pl" => &["padding-left"],
        "pr" => &["padding-right"],
        "m" => &["margin"],
        "mx" => &["margin-left", "margin-right"],
        "my" => &["margin-top", "margin-bottom"],
        "mt" => &["margin-top"],
        "mb" => &["margin-bottom"],
        "ml" => &["margin-left"],
        "mr" => &["margin-right"],
        // logical (RTL-aware) inline/block padding + margin
        "ps" => &["padding-inline-start"],
        "pe" => &["padding-inline-end"],
        "pbs" => &["padding-block-start"],
        "pbe" => &["padding-block-end"],
        "ms" => &["margin-inline-start"],
        "me" => &["margin-inline-end"],
        "mbs" => &["margin-block-start"],
        "mbe" => &["margin-block-end"],
        "gap" => &["gap"],
        _ => return None,
    })
}

/// Inset/position edge properties for an arbitrary `inset*`/positional
/// base (`inset-x-[4px]`, `start-[2rem]`). `top`/`right`/`bottom`/`left`
/// are single-property and handled directly by `resolve_arbitrary`.
fn inset_props(base: &str) -> Option<&'static [&'static str]> {
    Some(match base {
        "inset" => &["top", "right", "bottom", "left"],
        "inset-x" => &["left", "right"],
        "inset-y" => &["top", "bottom"],
        "inset-s" => &["inset-inline-start"],
        "inset-e" => &["inset-inline-end"],
        "inset-bs" => &["inset-block-start"],
        "inset-be" => &["inset-block-end"],
        "start" => &["inset-inline-start"],
        "end" => &["inset-inline-end"],
        _ => return None,
    })
}

/// scroll-margin / scroll-padding longhands for an arbitrary `scroll-m*`
/// / `scroll-p*` base (`scroll-mt-[4px]`). Mirrors `try_scroll`.
fn scroll_arb_props(base: &str) -> Option<Vec<String>> {
    let (prop, seg) = if let Some(r) = base.strip_prefix("scroll-m") {
        ("scroll-margin", r)
    } else if let Some(r) = base.strip_prefix("scroll-p") {
        ("scroll-padding", r)
    } else {
        return None;
    };
    let suffixes: &[&str] = match seg {
        "" => &[""],
        "x" => &["-left", "-right"],
        "y" => &["-top", "-bottom"],
        "t" => &["-top"],
        "b" => &["-bottom"],
        "l" => &["-left"],
        "r" => &["-right"],
        "s" => &["-inline-start"],
        "e" => &["-inline-end"],
        "bs" => &["-block-start"],
        "be" => &["-block-end"],
        _ => return None,
    };
    Some(suffixes.iter().map(|s| format!("{prop}{s}")).collect())
}

/// `p|px|py|pt|pb|pl|pr|m|mx|my|mt|mb|ml|mr|gap|gap-x|gap-y` + value.
fn try_spacing(base: &str, _t: &ThemeTokens) -> Resolved {
    // Directional gaps split awkwardly under the generic prefix rule, so
    // claim them first: `gap-x-*` → column-gap, `gap-y-*` → row-gap.
    if let Some(v) = base.strip_prefix("gap-x-") {
        return Some(match spacing(v) {
            Some(s) => Ok(decl("column-gap", &s)),
            None => Err(Diagnostic::error(format!(
                "`gap-x` expects a spacing value, got `{v}`"
            ))),
        });
    }
    if let Some(v) = base.strip_prefix("gap-y-") {
        return Some(match spacing(v) {
            Some(s) => Ok(decl("row-gap", &s)),
            None => Err(Diagnostic::error(format!(
                "`gap-y` expects a spacing value, got `{v}`"
            ))),
        });
    }
    let (prefix, value) = base.split_once('-')?;
    let props = spacing_props(prefix)?;
    Some(match spacing(value) {
        Some(v) => Ok(props.iter().map(|p| (p.to_string(), v.clone())).collect()),
        None => Err(Diagnostic::error(format!(
            "`{prefix}` expects a spacing value, got `{value}`"
        ))),
    })
}

/// `space-y-N` / `space-x-N` — margin between children. The child
/// combinator selector is appended by `emit_into`.
fn try_space(base: &str, _t: &ThemeTokens) -> Resolved {
    let (prop, value) = if let Some(v) = base.strip_prefix("space-y-") {
        ("margin-top", v)
    } else if let Some(v) = base.strip_prefix("space-x-") {
        ("margin-left", v)
    } else {
        return None;
    };
    Some(match spacing(value) {
        Some(v) => Ok(decl(prop, &v)),
        None => Err(Diagnostic::error(format!(
            "`space` expects a spacing value, got `{value}`"
        ))),
    })
}

/// `w|h|size|min-w|min-h|max-w|max-h` + value.
fn try_sizing(base: &str, _t: &ThemeTokens) -> Resolved {
    let (prefix, value) = base.split_once('-')?;
    let props: &[&str] = match prefix {
        "w" => &["width"],
        "h" => &["height"],
        "size" => &["width", "height"],
        "min" => match value.split_once('-') {
            Some(("w", _)) => &["min-width"],
            Some(("h", _)) => &["min-height"],
            _ => return None,
        },
        "max" => match value.split_once('-') {
            Some(("w", _)) => &["max-width"],
            Some(("h", _)) => &["max-height"],
            _ => return None,
        },
        _ => return None,
    };
    // For min/max the real value is after the second segment.
    let value = match prefix {
        "min" | "max" => value.split_once('-').map(|(_, v)| v).unwrap_or(value),
        _ => value,
    };
    Some(match size(value) {
        Some(v) => Ok(props.iter().map(|p| (p.to_string(), v.clone())).collect()),
        None => Err(Diagnostic::error(format!(
            "`{prefix}` expects a size value, got `{value}`"
        ))),
    })
}

/// Logical sizing (writing-mode aware): `block-{v}` → `block-size`,
/// `inline-{v}` → `inline-size`, plus their `min-`/`max-` forms. Static
/// `inline-flex`/`inline-block`/`inline-grid` are matched earlier, so
/// only value-bearing `inline-*` reach here.
fn try_logical_size(base: &str, _t: &ThemeTokens) -> Resolved {
    let (prop, value) = if let Some(v) = base.strip_prefix("min-block-") {
        ("min-block-size", v)
    } else if let Some(v) = base.strip_prefix("max-block-") {
        ("max-block-size", v)
    } else if let Some(v) = base.strip_prefix("min-inline-") {
        ("min-inline-size", v)
    } else if let Some(v) = base.strip_prefix("max-inline-") {
        ("max-inline-size", v)
    } else if let Some(v) = base.strip_prefix("block-") {
        ("block-size", v)
    } else if let Some(v) = base.strip_prefix("inline-") {
        ("inline-size", v)
    } else {
        return None;
    };
    Some(match size(value) {
        Some(v) => Ok(decl(prop, &v)),
        None => Err(Diagnostic::error(format!(
            "`{prop}` expects a size value, got `{value}`"
        ))),
    })
}

/// `inset|inset-x|inset-y|top|right|bottom|left` + value.
fn try_inset(base: &str, _t: &ThemeTokens) -> Resolved {
    // `inset-ring*` / `inset-shadow*` are box-shadow effects (handled by
    // `try_ring`/`try_shadow`), not positional inset utilities.
    if base.starts_with("inset-ring") || base.starts_with("inset-shadow") {
        return None;
    }
    let (prefix, value) = base.split_once('-')?;
    let props: &[&str] = match prefix {
        "top" => &["top"],
        "right" => &["right"],
        "bottom" => &["bottom"],
        "left" => &["left"],
        "start" => &["inset-inline-start"],
        "end" => &["inset-inline-end"],
        "inset" => match value.split_once('-') {
            Some(("x", _)) => &["left", "right"],
            Some(("y", _)) => &["top", "bottom"],
            Some(("s", _)) => &["inset-inline-start"],
            Some(("e", _)) => &["inset-inline-end"],
            Some(("bs", _)) => &["inset-block-start"],
            Some(("be", _)) => &["inset-block-end"],
            _ => &["top", "right", "bottom", "left"],
        },
        _ => return None,
    };
    let value = match prefix {
        "inset" => {
            let v = value;
            ["x-", "y-", "s-", "e-", "bs-", "be-"]
                .iter()
                .find_map(|p| v.strip_prefix(p))
                .unwrap_or(v)
        }
        _ => value,
    };
    Some(match spacing(value) {
        Some(v) => Ok(props.iter().map(|p| (p.to_string(), v.clone())).collect()),
        None => Err(Diagnostic::error(format!(
            "`{prefix}` expects a position value, got `{value}`"
        ))),
    })
}

/// `border-{color}` (width/style handled by `static_utility`).
fn try_border(base: &str, tokens: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("border-")?;
    // `border-spacing-*` is a table family handled in `try_numeric`.
    if name.starts_with("spacing") {
        return None;
    }
    // Numeric width like border-2.
    if let Ok(n) = name.parse::<u32>() {
        return Some(Ok(decl("border-width", &format!("{n}px"))));
    }
    // Per-side colour: `border-t-<color>` → border-top-color, etc.
    for (pfx, prop) in [
        ("t-", "border-top-color"),
        ("r-", "border-right-color"),
        ("b-", "border-bottom-color"),
        ("l-", "border-left-color"),
    ] {
        if let Some(color) = name.strip_prefix(pfx) {
            return Some(resolve_color_value(prop, color, tokens, base));
        }
    }
    Some(resolve_color_value("border-color", name, tokens, base))
}

/// `rounded` / `rounded-{scale}`. Token-driven like the spacing base
/// and `text-*`: the named scale references the user-definable
/// `--radius-{scale}` theme token, with Tailwind's default value as the
/// literal fallback. `none`/`full` are non-token specials.
fn radius_value(scale: &str) -> Result<String, Diagnostic> {
    Ok(match scale {
        "" => "var(--radius, 0.25rem)".to_string(),
        "none" => "0px".to_string(),
        "full" => "9999px".to_string(),
        "xs" | "sm" | "md" | "lg" | "xl" | "2xl" | "3xl" | "4xl" => {
            let fallback = match scale {
                "xs" => "0.125rem",
                "sm" => "0.25rem",
                "md" => "0.375rem",
                "lg" => "0.5rem",
                "xl" => "0.75rem",
                "2xl" => "1rem",
                "3xl" => "1.5rem",
                "4xl" => "2rem",
                _ => unreachable!(),
            };
            format!("var(--radius-{scale}, {fallback})")
        }
        _ => return Err(Diagnostic::error(format!("unknown radius `{scale}`"))),
    })
}

/// `rounded` + optional side/corner (`-t`/`-tl`/…) + optional scale.
fn try_rounded(base: &str, _t: &ThemeTokens) -> Resolved {
    let rest = base.strip_prefix("rounded")?;
    let rest = match rest {
        "" => "",
        r => r.strip_prefix('-')?,
    };
    // Longest side/corner tokens first so `tl`/`ss` win over `t`/`s`.
    let sides: &[(&str, &[&str])] = &[
        ("tl", &["border-top-left-radius"]),
        ("tr", &["border-top-right-radius"]),
        ("bl", &["border-bottom-left-radius"]),
        ("br", &["border-bottom-right-radius"]),
        // logical (writing-mode aware) corners + sides
        ("ss", &["border-start-start-radius"]),
        ("se", &["border-start-end-radius"]),
        ("es", &["border-end-start-radius"]),
        ("ee", &["border-end-end-radius"]),
        (
            "s",
            &["border-start-start-radius", "border-end-start-radius"],
        ),
        ("e", &["border-start-end-radius", "border-end-end-radius"]),
        ("t", &["border-top-left-radius", "border-top-right-radius"]),
        (
            "b",
            &["border-bottom-left-radius", "border-bottom-right-radius"],
        ),
        (
            "l",
            &["border-top-left-radius", "border-bottom-left-radius"],
        ),
        (
            "r",
            &["border-top-right-radius", "border-bottom-right-radius"],
        ),
    ];
    let (props, scale): (&[&str], &str) = sides
        .iter()
        .find_map(|(tok, props)| {
            if rest == *tok {
                Some((*props, ""))
            } else {
                rest.strip_prefix(&format!("{tok}-")).map(|s| (*props, s))
            }
        })
        .unwrap_or((&["border-radius"], rest));
    Some(match radius_value(scale) {
        Ok(v) => Ok(props.iter().map(|p| (p.to_string(), v.clone())).collect()),
        Err(e) => Err(e),
    })
}

/// Composable `box-shadow` chain (RFC 092 M2). Every shadow/ring
/// utility sets *only* its own layer var and emits this identical
/// `box-shadow`, so `shadow-xl ring-1` on one element render *both*
/// (mirrors Tailwind's `--tw-shadow`/`--tw-ring-shadow` layering).
/// Unset layers fall back to a transparent shadow.
const SHADOW_CHAIN: &str = "var(--pp-inset-shadow, 0 0 #0000), var(--pp-inset-ring-shadow, 0 0 #0000), var(--pp-ring-offset-shadow, 0 0 #0000), var(--pp-ring-shadow, 0 0 #0000), var(--pp-shadow, 0 0 #0000)";

fn shadow_decls(layer_var: &str, value: &str) -> Decls {
    chain_decls(layer_var, value, "box-shadow", SHADOW_CHAIN)
}

/// Default value of a named shadow scale (Tailwind v4). `None` = not a
/// preset name.
fn shadow_scale(name: &str) -> Option<&'static str> {
    // The shadow colour is `var(--pp-shadow-color, <default>)` so
    // `shadow-{color}` can recolour the preset (Tailwind's
    // `--tw-shadow-color` mechanism).
    Some(match name {
        "2xs" => "0 1px var(--pp-shadow-color, rgb(0 0 0 / 0.05))",
        "xs" => "0 1px 2px 0 var(--pp-shadow-color, rgb(0 0 0 / 0.05))",
        "sm" => "0 1px 3px 0 var(--pp-shadow-color, rgb(0 0 0 / 0.1)), 0 1px 2px -1px var(--pp-shadow-color, rgb(0 0 0 / 0.1))",
        "md" => "0 4px 6px -1px var(--pp-shadow-color, rgb(0 0 0 / 0.1)), 0 2px 4px -2px var(--pp-shadow-color, rgb(0 0 0 / 0.1))",
        "lg" => "0 10px 15px -3px var(--pp-shadow-color, rgb(0 0 0 / 0.1)), 0 4px 6px -4px var(--pp-shadow-color, rgb(0 0 0 / 0.1))",
        "xl" => "0 20px 25px -5px var(--pp-shadow-color, rgb(0 0 0 / 0.1)), 0 8px 10px -6px var(--pp-shadow-color, rgb(0 0 0 / 0.1))",
        "2xl" => "0 25px 50px -12px var(--pp-shadow-color, rgb(0 0 0 / 0.25))",
        "inner" => "inset 0 2px 4px 0 var(--pp-shadow-color, rgb(0 0 0 / 0.05))",
        _ => return None,
    })
}

/// `shadow` / `shadow-{scale}` / `shadow-{token}`. Token-driven, and
/// emitted through the composable chain so it coexists with `ring-*`.
fn try_shadow(base: &str, tokens: &ThemeTokens) -> Resolved {
    // `inset-shadow` / `inset-shadow-{scale}` / `-none` / `-{token}`,
    // layered first in the composable chain.
    if base == "inset-shadow" || base.starts_with("inset-shadow-") {
        let value = if base == "inset-shadow" {
            "var(--inset-shadow, inset 0 2px 4px 0 rgb(0 0 0 / 0.05))".to_string()
        } else {
            let name = base.strip_prefix("inset-shadow-").unwrap();
            match name {
                "none" => "0 0 #0000".to_string(),
                "2xs" => "inset 0 1px rgb(0 0 0 / 0.05)".to_string(),
                "xs" => "inset 0 1px 1px rgb(0 0 0 / 0.05)".to_string(),
                "sm" => "inset 0 2px 4px rgb(0 0 0 / 0.05)".to_string(),
                _ if tokens.var_for("inset-shadow", name).is_some() => {
                    format!("var(--inset-shadow-{name})")
                }
                _ => {
                    return Some(Err(Diagnostic::error(format!(
                        "`--inset-shadow-{name}` is not a defined token"
                    ))))
                }
            }
        };
        return Some(Ok(shadow_decls("--pp-inset-shadow", &value)));
    }
    let value = if base == "shadow" {
        "var(--shadow, 0 1px 3px 0 var(--pp-shadow-color, rgb(0 0 0 / 0.1)), \
         0 1px 2px -1px var(--pp-shadow-color, rgb(0 0 0 / 0.1)))"
            .to_string()
    } else {
        let name = base.strip_prefix("shadow-")?;
        if name == "none" {
            "0 0 #0000".to_string()
        } else if let Some(fallback) = shadow_scale(name) {
            format!("var(--shadow-{name}, {fallback})")
        } else if tokens.var_for("shadow", name).is_some() {
            format!("var(--shadow-{name})")
        } else if let Ok(color) = resolve_color_string(name, tokens, base) {
            // `shadow-{color}` recolours the preset via `--pp-shadow-color`.
            return Some(Ok(decl("--pp-shadow-color", &color)));
        } else {
            return Some(Err(Diagnostic::error(format!(
                "`--shadow-{name}` is not a defined token"
            ))));
        }
    };
    Some(Ok(shadow_decls("--pp-shadow", &value)))
}

/// `ring` / `ring-{n}` widths and `ring-{color}`. Widths feed the
/// composable chain (so a ring layers over a shadow); the colour form
/// just sets the ring-colour variable.
fn try_ring(base: &str, tokens: &ThemeTokens) -> Resolved {
    // `inset-ring` / `inset-ring-{n}` / `inset-ring-{color}` — an inset
    // ring layered ahead of the others in the composable chain.
    if base == "inset-ring" {
        return Some(Ok(shadow_decls(
            "--pp-inset-ring-shadow",
            "inset 0 0 0 1px var(--pp-inset-ring-color, currentcolor)",
        )));
    }
    if let Some(name) = base.strip_prefix("inset-ring-") {
        if let Ok(n) = name.parse::<u32>() {
            return Some(Ok(shadow_decls(
                "--pp-inset-ring-shadow",
                &format!("inset 0 0 0 {n}px var(--pp-inset-ring-color, currentcolor)"),
            )));
        }
        return Some(resolve_color_value(
            "--pp-inset-ring-color",
            name,
            tokens,
            base,
        ));
    }
    // `ring-offset-{n}` width / `ring-offset-{color}` — an offset ring
    // (approximate: a solid layer between the element and the main ring).
    if let Some(name) = base.strip_prefix("ring-offset-") {
        if let Ok(n) = name.parse::<u32>() {
            return Some(Ok(shadow_decls(
                "--pp-ring-offset-shadow",
                &format!("0 0 0 {n}px var(--pp-ring-offset-color, #fff)"),
            )));
        }
        return Some(resolve_color_value(
            "--pp-ring-offset-color",
            name,
            tokens,
            base,
        ));
    }
    if base == "ring" {
        return Some(Ok(shadow_decls(
            "--pp-ring-shadow",
            "0 0 0 3px var(--pp-ring-color, currentcolor)",
        )));
    }
    let name = base.strip_prefix("ring-")?;
    if let Ok(n) = name.parse::<u32>() {
        return Some(Ok(shadow_decls(
            "--pp-ring-shadow",
            &format!("0 0 0 {n}px var(--pp-ring-color, currentcolor)"),
        )));
    }
    // ring-{color} sets the ring colour variable.
    Some(resolve_color_value("--pp-ring-color", name, tokens, base))
}

/// `text-{color}` and named font sizes (align handled statically).
///
/// Named sizes are token-driven like the spacing base (RFC 092 D4): the
/// utility references the user-definable `--text-{size}` /
/// `--text-{size}--line-height` theme tokens, with the default scale as
/// the literal fallback. This matches Tailwind's mechanism and lets a
/// `@theme` rescale typography without touching the registry.
fn try_text(base: &str, tokens: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("text-")?;
    // `text-{size}/{leading}` — font-size + an explicit line-height
    // (`text-sm/6`, `text-[12px]/[1.5]`). Only intercept when the prefix
    // is clearly a size, so `text-red-500/50` stays a colour+alpha.
    if let Some((sz, lead)) = name.split_once('/') {
        let is_arb = sz.starts_with('[') && sz.ends_with(']');
        if font_size(sz).is_some() || is_arb {
            let font = if let Some((fs, _)) = font_size(sz) {
                format!("var(--text-{sz}, {fs})")
            } else {
                sz[1..sz.len() - 1].replace('_', " ")
            };
            let lh = if let Some(inner) = lead.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                inner.replace('_', " ")
            } else if let Ok(n) = lead.parse::<f64>() {
                format!("{}rem", trim_num(n * 0.25))
            } else {
                lead.to_string()
            };
            return Some(Ok(vec![
                ("font-size".into(), font),
                ("line-height".into(), lh),
            ]));
        }
    }
    if let Some((fs, lh)) = font_size(name) {
        return Some(Ok(vec![
            ("font-size".into(), format!("var(--text-{name}, {fs})")),
            (
                "line-height".into(),
                format!("var(--text-{name}--line-height, {lh})"),
            ),
        ]));
    }
    Some(resolve_color_value("color", name, tokens, base))
}

fn try_font(base: &str, _t: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("font-")?;
    // `font-stretch-{keyword}` / `font-stretch-{N}%`.
    if let Some(s) = name.strip_prefix("stretch-") {
        let v = match s {
            "ultra-condensed" | "extra-condensed" | "condensed" | "semi-condensed" | "normal"
            | "semi-expanded" | "expanded" | "extra-expanded" | "ultra-expanded" => s.to_string(),
            _ if s.ends_with('%') && s[..s.len() - 1].parse::<f64>().is_ok() => s.to_string(),
            _ => {
                return Some(Err(Diagnostic::error(format!(
                    "`font-stretch` expects a keyword or percentage, got `{s}`"
                ))))
            }
        };
        return Some(Ok(decl("font-stretch", &v)));
    }
    let pair = match name {
        "thin" => Some(("font-weight", "100")),
        "light" => Some(("font-weight", "300")),
        "normal" => Some(("font-weight", "400")),
        "medium" => Some(("font-weight", "500")),
        "semibold" => Some(("font-weight", "600")),
        "bold" => Some(("font-weight", "700")),
        "extrabold" => Some(("font-weight", "800")),
        "sans" => Some((
            "font-family",
            "var(--font-sans, ui-sans-serif, system-ui, sans-serif)",
        )),
        "serif" => Some(("font-family", "var(--font-serif, ui-serif, serif)")),
        "mono" => Some(("font-family", "var(--font-mono, ui-monospace, monospace)")),
        _ => None,
    }?;
    Some(Ok(decl(pair.0, pair.1)))
}

/// Token-driven like `text-*`: references `--leading-{name}` with the
/// default scale as fallback.
fn try_leading(base: &str, _t: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("leading-")?;
    // Numeric leading (`leading-5` → 1.25rem) — the spacing scale.
    if let Ok(n) = name.parse::<f64>() {
        return Some(Ok(decl(
            "line-height",
            &format!("{}rem", trim_num(n * 0.25)),
        )));
    }
    let fallback = match name {
        "tight" => "1.25",
        "snug" => "1.375",
        "normal" => "1.5",
        "relaxed" => "1.625",
        "loose" => "2",
        _ => {
            return Some(Err(Diagnostic::error(format!(
                "unknown line-height `{name}`"
            ))))
        }
    };
    Some(Ok(decl(
        "line-height",
        &format!("var(--leading-{name}, {fallback})"),
    )))
}

/// Token-driven like `text-*`: references `--tracking-{name}` with the
/// default scale as fallback.
fn try_tracking(base: &str, _t: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("tracking-")?;
    let fallback = match name {
        "tighter" => "-0.05em",
        "tight" => "-0.025em",
        "normal" => "0em",
        "wide" => "0.025em",
        "wider" => "0.05em",
        "widest" => "0.1em",
        _ => {
            return Some(Err(Diagnostic::error(format!(
                "unknown letter-spacing `{name}`"
            ))))
        }
    };
    Some(Ok(decl(
        "letter-spacing",
        &format!("var(--tracking-{name}, {fallback})"),
    )))
}

fn try_underline_offset(base: &str, _t: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("underline-offset-")?;
    match name.parse::<u32>() {
        Ok(n) => Some(Ok(decl("text-underline-offset", &format!("{n}px")))),
        Err(_) if name == "auto" => Some(Ok(decl("text-underline-offset", "auto"))),
        Err(_) => Some(Err(Diagnostic::error(format!(
            "`underline-offset` expects a length, got `{name}`"
        )))),
    }
}

/// `decoration-{n}` thickness, `decoration-{style}`, `decoration-{color}`.
fn try_decoration(base: &str, tokens: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("decoration-")?;
    if let Ok(n) = name.parse::<u32>() {
        return Some(Ok(decl("text-decoration-thickness", &format!("{n}px"))));
    }
    match name {
        "solid" | "double" | "dotted" | "dashed" | "wavy" => {
            return Some(Ok(decl("text-decoration-style", name)))
        }
        "auto" | "from-font" => return Some(Ok(decl("text-decoration-thickness", name))),
        _ => {}
    }
    Some(resolve_color_value(
        "text-decoration-color",
        name,
        tokens,
        base,
    ))
}

/// `mix-blend-*` / `bg-blend-*` — the full CSS `<blend-mode>` set (a few
/// common ones are also statics, which short-circuit first).
fn try_blend(base: &str, _t: &ThemeTokens) -> Resolved {
    let (prop, mode) = if let Some(m) = base.strip_prefix("mix-blend-") {
        ("mix-blend-mode", m)
    } else if let Some(m) = base.strip_prefix("bg-blend-") {
        ("background-blend-mode", m)
    } else {
        return None;
    };
    let ok = matches!(
        mode,
        "normal"
            | "multiply"
            | "screen"
            | "overlay"
            | "darken"
            | "lighten"
            | "color-dodge"
            | "color-burn"
            | "hard-light"
            | "soft-light"
            | "difference"
            | "exclusion"
            | "hue"
            | "saturation"
            | "color"
            | "luminosity"
            | "plus-darker"
            | "plus-lighter"
    );
    ok.then(|| Ok(decl(prop, mode)))
}

/// `cursor-{keyword}` — the full CSS `cursor` keyword set (common ones
/// are also statics, which short-circuit first).
fn try_cursor(base: &str, _t: &ThemeTokens) -> Resolved {
    let kw = base.strip_prefix("cursor-")?;
    let ok = matches!(
        kw,
        "auto"
            | "default"
            | "pointer"
            | "wait"
            | "text"
            | "move"
            | "help"
            | "not-allowed"
            | "none"
            | "context-menu"
            | "progress"
            | "cell"
            | "crosshair"
            | "vertical-text"
            | "alias"
            | "copy"
            | "no-drop"
            | "grab"
            | "grabbing"
            | "all-scroll"
            | "col-resize"
            | "row-resize"
            | "n-resize"
            | "e-resize"
            | "s-resize"
            | "w-resize"
            | "ne-resize"
            | "nw-resize"
            | "se-resize"
            | "sw-resize"
            | "ew-resize"
            | "ns-resize"
            | "nesw-resize"
            | "nwse-resize"
            | "zoom-in"
            | "zoom-out"
    );
    ok.then(|| Ok(decl("cursor", kw)))
}

/// `backdrop-*` filter family, composed through [`BACKDROP_CHAIN`] so
/// multiple backdrop filters stack (mirrors `try_filter` + the filter
/// statics for the non-backdrop forms).
fn try_backdrop(base: &str, _t: &ThemeTokens) -> Resolved {
    if let Some(name) = base.strip_prefix("backdrop-blur-") {
        let v = match name {
            "none" => "0",
            "xs" => "2px",
            "sm" => "4px",
            "md" => "12px",
            "lg" => "16px",
            "xl" => "24px",
            "2xl" => "40px",
            "3xl" => "64px",
            _ => return Some(Err(Diagnostic::error(format!("unknown blur `{name}`")))),
        };
        return Some(Ok(backdrop_decls("--pp-bd-blur", &format!("blur({v})"))));
    }
    for (pfx, var, func) in [
        ("backdrop-brightness-", "--pp-bd-brightness", "brightness"),
        ("backdrop-contrast-", "--pp-bd-contrast", "contrast"),
        ("backdrop-saturate-", "--pp-bd-saturate", "saturate"),
        ("backdrop-opacity-", "--pp-bd-opacity", "opacity"),
    ] {
        if let Some(v) = base.strip_prefix(pfx) {
            return Some(
                parse_pct(v).map(|p| backdrop_decls(var, &format!("{func}({})", trim_num(p)))),
            );
        }
    }
    None
}

/// `outline-{width}` / `outline-{color}`. Bare `outline`, the styles, and
/// `outline-none`/`-hidden` are statics; `outline-offset-*` is numeric.
fn try_outline(base: &str, tokens: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("outline-")?;
    // `outline-offset-*` belongs to `try_numeric` (registered earlier).
    if name.starts_with("offset") {
        return None;
    }
    // Integer width sets a solid outline of that thickness.
    if let Ok(n) = name.parse::<u32>() {
        return Some(Ok(vec![
            ("outline-style".into(), "solid".into()),
            ("outline-width".into(), format!("{n}px")),
        ]));
    }
    // Otherwise a colour.
    Some(resolve_color_value("outline-color", name, tokens, base))
}

/// Container-context utility (`@container`, `@container-normal`,
/// `@container-size`, optionally `/name`). The `@{size}:` query
/// *variants* are handled in `resolve_variant`.
fn try_container(base: &str, _t: &ThemeTokens) -> Resolved {
    let rest = base.strip_prefix("@container")?;
    // Optional `/name` suffix names the container.
    let (kind, name) = match rest.split_once('/') {
        Some((k, n)) => (k, Some(n)),
        None => (rest, None),
    };
    let ty = match kind {
        "" => "inline-size",
        "-normal" => "normal",
        "-size" => "size",
        _ => return None,
    };
    Some(Ok(match name {
        // Named: shorthand `container: <name> [/ <type>]`.
        Some(n) if ty == "normal" => decl("container", n),
        Some(n) => decl("container", &format!("{n} / {ty}")),
        None => decl("container-type", ty),
    }))
}

/// Numeric families: `opacity-N`, `z-N`, `duration-N`, `scale-N`.
fn try_numeric(base: &str, _t: &ThemeTokens) -> Resolved {
    // Multi-segment families that the generic split would mangle.
    if let Some(v) = base.strip_prefix("outline-offset-") {
        return Some(int(v).map(|n| decl("outline-offset", &format!("{n}px"))));
    }
    if let Some(v) = base.strip_prefix("tab-") {
        return Some(int(v).map(|n| decl("tab-size", &n.to_string())));
    }
    if let Some(v) = base.strip_prefix("border-spacing-x-") {
        return Some(
            spacing(v)
                .map(|s| {
                    vec![
                        ("--pp-bs-x".into(), s),
                        (
                            "border-spacing".into(),
                            "var(--pp-bs-x, 0) var(--pp-bs-y, 0)".into(),
                        ),
                    ]
                })
                .ok_or_else(|| {
                    Diagnostic::error(format!("`border-spacing-x` expects spacing, got `{v}`"))
                }),
        );
    }
    if let Some(v) = base.strip_prefix("border-spacing-y-") {
        return Some(
            spacing(v)
                .map(|s| {
                    vec![
                        ("--pp-bs-y".into(), s),
                        (
                            "border-spacing".into(),
                            "var(--pp-bs-x, 0) var(--pp-bs-y, 0)".into(),
                        ),
                    ]
                })
                .ok_or_else(|| {
                    Diagnostic::error(format!("`border-spacing-y` expects spacing, got `{v}`"))
                }),
        );
    }
    if let Some(v) = base.strip_prefix("border-spacing-") {
        return Some(
            spacing(v)
                .map(|s| decl("border-spacing", &s))
                .ok_or_else(|| {
                    Diagnostic::error(format!("`border-spacing` expects spacing, got `{v}`"))
                }),
        );
    }
    let (prefix, value) = base.split_once('-')?;
    match prefix {
        "opacity" => Some(parse_pct(value).map(|p| decl("opacity", &trim_num(p)))),
        "z" => Some(int(value).map(|n| decl("z-index", &n.to_string()))),
        "duration" => Some(int(value).map(|n| decl("transition-duration", &format!("{n}ms")))),
        "delay" => Some(int(value).map(|n| decl("transition-delay", &format!("{n}ms")))),
        "order" => Some(match value {
            "first" => Ok(decl("order", "-9999")),
            "last" => Ok(decl("order", "9999")),
            "none" => Ok(decl("order", "0")),
            _ => int(value).map(|n| decl("order", &n.to_string())),
        }),
        "columns" => Some(int(value).map(|n| decl("column-count", &n.to_string()))),
        "perspective" => Some(int(value).map(|n| decl("perspective", &format!("{n}px")))),
        "zoom" => Some(parse_pct(value).map(|p| decl("zoom", &trim_num(p)))),
        "basis" => Some(match size(value) {
            Some(v) => Ok(decl("flex-basis", &v)),
            None => Err(Diagnostic::error(format!(
                "`basis` expects a size value, got `{value}`"
            ))),
        }),
        "indent" => Some(match spacing(value) {
            Some(v) => Ok(decl("text-indent", &v)),
            None => Err(Diagnostic::error(format!(
                "`indent` expects a spacing value, got `{value}`"
            ))),
        }),
        _ => None,
    }
}

/// `grid-cols-N`, `grid-rows-N`, `col-span-N`, `row-span-N` (+ `-full`).
fn try_grid(base: &str, _t: &ThemeTokens) -> Resolved {
    if let Some(v) = base.strip_prefix("grid-cols-") {
        return Some(int(v).map(|n| {
            decl(
                "grid-template-columns",
                &format!("repeat({n}, minmax(0, 1fr))"),
            )
        }));
    }
    if let Some(v) = base.strip_prefix("grid-rows-") {
        return Some(int(v).map(|n| {
            decl(
                "grid-template-rows",
                &format!("repeat({n}, minmax(0, 1fr))"),
            )
        }));
    }
    if let Some(v) = base.strip_prefix("col-span-") {
        let value = if v == "full" {
            "1 / -1".to_string()
        } else {
            match int(v) {
                Ok(n) => format!("span {n} / span {n}"),
                Err(e) => return Some(Err(e)),
            }
        };
        return Some(Ok(decl("grid-column", &value)));
    }
    if let Some(v) = base.strip_prefix("row-span-") {
        let value = if v == "full" {
            "1 / -1".to_string()
        } else {
            match int(v) {
                Ok(n) => format!("span {n} / span {n}"),
                Err(e) => return Some(Err(e)),
            }
        };
        return Some(Ok(decl("grid-row", &value)));
    }
    // line placement: col-start/-end, row-start/-end (+ `auto`)
    for (pfx, prop) in [
        ("col-start-", "grid-column-start"),
        ("col-end-", "grid-column-end"),
        ("row-start-", "grid-row-start"),
        ("row-end-", "grid-row-end"),
    ] {
        if let Some(v) = base.strip_prefix(pfx) {
            let value = if v == "auto" {
                "auto".to_string()
            } else {
                match int(v) {
                    Ok(n) => n.to_string(),
                    Err(e) => return Some(Err(e)),
                }
            };
            return Some(Ok(decl(prop, &value)));
        }
    }
    None
}

/// Composable transform chain (like the shadow chain): each transform
/// utility sets just its own var + emits this identical `transform`, so
/// `translate-x-2 rotate-45 scale-110` all apply together.
const TRANSFORM_CHAIN: &str = "translate(var(--pp-tx, 0), var(--pp-ty, 0)) translateZ(var(--pp-tz, 0)) rotate(var(--pp-rotate, 0)) rotateX(var(--pp-rotate-x, 0)) rotateY(var(--pp-rotate-y, 0)) rotateZ(var(--pp-rotate-z, 0)) skewX(var(--pp-skew-x, 0)) skewY(var(--pp-skew-y, 0)) scaleX(var(--pp-scale-x, 1)) scaleY(var(--pp-scale-y, 1)) scaleZ(var(--pp-scale-z, 1))";

fn transform_decls(layer: &str, value: &str) -> Decls {
    chain_decls(layer, value, "transform", TRANSFORM_CHAIN)
}

fn num_deg(v: &str) -> Result<String, Diagnostic> {
    v.parse::<f64>()
        .map(|n| format!("{}deg", trim_num(n)))
        .map_err(|_| Diagnostic::error(format!("expected a number, got `{v}`")))
}

/// `translate-x/y`, `rotate`, `skew-x/y`, `scale`/`scale-x/y`. Composed
/// via [`TRANSFORM_CHAIN`] so they stack. Negation (`-rotate-45`) is
/// applied centrally in `resolve` against the layer vars.
fn try_transform(base: &str, _t: &ThemeTokens) -> Resolved {
    let rest = base;

    if let Some(v) = rest.strip_prefix("translate-x-") {
        return Some(
            spacing(v)
                .map(|r| transform_decls("--pp-tx", &r))
                .ok_or_else(|| {
                    Diagnostic::error(format!("`translate-x` expects a spacing value, got `{v}`"))
                }),
        );
    }
    if let Some(v) = rest.strip_prefix("translate-y-") {
        return Some(
            spacing(v)
                .map(|r| transform_decls("--pp-ty", &r))
                .ok_or_else(|| {
                    Diagnostic::error(format!("`translate-y` expects a spacing value, got `{v}`"))
                }),
        );
    }
    if let Some(v) = rest.strip_prefix("translate-z-") {
        return Some(
            spacing(v)
                .map(|r| transform_decls("--pp-tz", &r))
                .ok_or_else(|| {
                    Diagnostic::error(format!("`translate-z` expects a spacing value, got `{v}`"))
                }),
        );
    }
    if let Some(v) = rest.strip_prefix("rotate-x-") {
        return Some(num_deg(v).map(|d| transform_decls("--pp-rotate-x", &d)));
    }
    if let Some(v) = rest.strip_prefix("rotate-y-") {
        return Some(num_deg(v).map(|d| transform_decls("--pp-rotate-y", &d)));
    }
    if let Some(v) = rest.strip_prefix("rotate-z-") {
        return Some(num_deg(v).map(|d| transform_decls("--pp-rotate-z", &d)));
    }
    if let Some(v) = rest.strip_prefix("scale-z-") {
        return Some(parse_pct(v).map(|p| transform_decls("--pp-scale-z", &trim_num(p))));
    }
    if let Some(v) = rest.strip_prefix("rotate-") {
        return Some(num_deg(v).map(|d| transform_decls("--pp-rotate", &d)));
    }
    if let Some(v) = rest.strip_prefix("skew-x-") {
        return Some(num_deg(v).map(|d| transform_decls("--pp-skew-x", &d)));
    }
    if let Some(v) = rest.strip_prefix("skew-y-") {
        return Some(num_deg(v).map(|d| transform_decls("--pp-skew-y", &d)));
    }
    if let Some(v) = rest.strip_prefix("scale-x-") {
        return Some(parse_pct(v).map(|p| transform_decls("--pp-scale-x", &trim_num(p))));
    }
    if let Some(v) = rest.strip_prefix("scale-y-") {
        return Some(parse_pct(v).map(|p| transform_decls("--pp-scale-y", &trim_num(p))));
    }
    if let Some(v) = rest.strip_prefix("scale-") {
        return Some(parse_pct(v).map(|p| {
            let mut d = transform_decls("--pp-scale-x", &trim_num(p));
            d.insert(0, ("--pp-scale-y".to_string(), trim_num(p)));
            d
        }));
    }
    // Bare `translate-*` / `skew-*` drive both axes at once.
    if let Some(v) = rest.strip_prefix("translate-") {
        return Some(
            spacing(v)
                .map(|r| {
                    let mut d = transform_decls("--pp-tx", &r);
                    d.insert(0, ("--pp-ty".to_string(), r.clone()));
                    d
                })
                .ok_or_else(|| {
                    Diagnostic::error(format!("`translate` expects a spacing value, got `{v}`"))
                }),
        );
    }
    if let Some(v) = rest.strip_prefix("skew-") {
        return Some(num_deg(v).map(|deg| {
            let mut d = transform_decls("--pp-skew-x", &deg);
            d.insert(0, ("--pp-skew-y".to_string(), deg.clone()));
            d
        }));
    }
    None
}

/// `aspect-*`, `line-clamp-*`, and SVG/form colour families
/// (`fill`/`stroke`/`accent`/`caret`).
fn try_misc(base: &str, tokens: &ThemeTokens) -> Resolved {
    match base {
        "aspect-square" => return Some(Ok(decl("aspect-ratio", "1 / 1"))),
        "aspect-video" => return Some(Ok(decl("aspect-ratio", "16 / 9"))),
        "aspect-auto" => return Some(Ok(decl("aspect-ratio", "auto"))),
        "line-clamp-none" => {
            return Some(Ok(vec![
                ("-webkit-line-clamp".into(), "unset".into()),
                ("display".into(), "block".into()),
                ("overflow".into(), "visible".into()),
            ]))
        }
        _ => {}
    }
    if let Some(v) = base.strip_prefix("line-clamp-") {
        return Some(int(v).map(|n| {
            vec![
                ("display".into(), "-webkit-box".into()),
                ("-webkit-box-orient".into(), "vertical".into()),
                ("-webkit-line-clamp".into(), n.to_string()),
                ("overflow".into(), "hidden".into()),
            ]
        }));
    }
    for (pfx, prop) in [
        ("fill-", "fill"),
        ("stroke-", "stroke"),
        ("accent-", "accent-color"),
        ("caret-", "caret-color"),
    ] {
        if let Some(name) = base.strip_prefix(pfx) {
            if name == "none" {
                return Some(Ok(decl(prop, "none")));
            }
            if prop == "stroke" {
                if let Ok(n) = name.parse::<u32>() {
                    return Some(Ok(decl("stroke-width", &n.to_string())));
                }
            }
            if prop == "accent-color" && name == "auto" {
                return Some(Ok(decl(prop, "auto")));
            }
            return Some(resolve_color_value(prop, name, tokens, base));
        }
    }
    None
}

/// An opaque (no-op) mask layer — the default every unset `--pp-mask-*`
/// layer falls back to (Tailwind ships these as `@property` initial
/// values; Stylekit has no `@property` layer so the defaults live in the
/// `var(--x, …)` fallbacks here).
const MASK_OPAQUE: &str = "linear-gradient(#fff, #fff)";

/// `mask-image` composition + `mask-composite: intersect` — emitted by
/// every mask *gradient* utility so the three layers (linear, radial,
/// conic) combine. Box statics (clip/origin/type) live in
/// `static_utility`.
fn mask_scaffold() -> Decls {
    let chain = format!(
        "var(--pp-mask-linear, {MASK_OPAQUE}), var(--pp-mask-radial, {MASK_OPAQUE}), \
         var(--pp-mask-conic, {MASK_OPAQUE})"
    );
    vec![
        ("-webkit-mask-image".into(), chain.clone()),
        ("mask-image".into(), chain),
        ("-webkit-mask-composite".into(), "source-in".into()),
        ("mask-composite".into(), "intersect".into()),
    ]
}

/// Edge keys → the physical edges (+ gradient direction) they drive.
/// `x`/`y` span two edges so `mask-x-from-*` fades both sides.
fn mask_edges(key: &str) -> Option<&'static [(&'static str, &'static str)]> {
    Some(match key {
        "t" => &[("top", "to top")],
        "b" => &[("bottom", "to bottom")],
        "l" => &[("left", "to left")],
        "r" => &[("right", "to right")],
        "x" => &[("left", "to left"), ("right", "to right")],
        "y" => &[("top", "to top"), ("bottom", "to bottom")],
        _ => return None,
    })
}

/// Build a linear-edge mask layer: scaffold + `--pp-mask-linear`
/// composition + each edge's gradient + the one stop var (`from`/`to`,
/// `position`/`color`). Composes with sibling edges via the shared vars.
fn mask_edge_decls(edges: &[(&str, &str)], kind: &str, slot: &str, val: &str) -> Decls {
    let mut d = mask_scaffold();
    d.push((
        "--pp-mask-linear".into(),
        format!(
            "var(--pp-mask-left, {MASK_OPAQUE}), var(--pp-mask-right, {MASK_OPAQUE}), \
             var(--pp-mask-bottom, {MASK_OPAQUE}), var(--pp-mask-top, {MASK_OPAQUE})"
        ),
    ));
    for (edge, dir) in edges {
        d.push((
            format!("--pp-mask-{edge}"),
            format!(
                "linear-gradient({dir}, \
                 var(--pp-mask-{edge}-from-color, black) var(--pp-mask-{edge}-from-position, 0%), \
                 var(--pp-mask-{edge}-to-color, transparent) var(--pp-mask-{edge}-to-position, 100%))"
            ),
        ));
        d.push((format!("--pp-mask-{edge}-{kind}-{slot}"), val.to_string()));
    }
    d
}

/// Build a linear-angle / radial / conic mask layer: scaffold + the
/// layer's `-stops` definition + gradient wrapper + the one var set.
fn mask_layer_decls(layer: &str, set: (String, String)) -> Decls {
    let (stops, gradient) = match layer {
        "linear" => (
            "var(--pp-mask-linear-position, 180deg), \
             var(--pp-mask-linear-from-color, black) var(--pp-mask-linear-from-position, 0%), \
             var(--pp-mask-linear-to-color, transparent) var(--pp-mask-linear-to-position, 100%)",
            "linear-gradient(var(--pp-mask-linear-stops))",
        ),
        "radial" => (
            "var(--pp-mask-radial-shape, ellipse) var(--pp-mask-radial-size, farthest-corner) \
             at var(--pp-mask-radial-position, center), \
             var(--pp-mask-radial-from-color, black) var(--pp-mask-radial-from-position, 0%), \
             var(--pp-mask-radial-to-color, transparent) var(--pp-mask-radial-to-position, 100%)",
            "radial-gradient(var(--pp-mask-radial-stops))",
        ),
        _ => (
            "from var(--pp-mask-conic-position, 0deg), \
             var(--pp-mask-conic-from-color, black) var(--pp-mask-conic-from-position, 0%), \
             var(--pp-mask-conic-to-color, transparent) var(--pp-mask-conic-to-position, 100%)",
            "conic-gradient(var(--pp-mask-conic-stops))",
        ),
    };
    let mut d = mask_scaffold();
    d.push((format!("--pp-mask-{layer}-stops"), stops.into()));
    d.push((format!("--pp-mask-{layer}"), gradient.into()));
    d.push(set);
    d
}

/// The masks family — composable linear-edge / linear-angle / radial /
/// conic gradient layers (RFC 092 D3 practical subset). Each utility
/// emits the shared scaffold + its layer's `*-stops` definition + the one
/// variable it sets, so siblings (`mask-t-from-0 mask-b-from-0`,
/// `mask-radial-* + mask-conic-*`) compose. Defaults are baked into the
/// `var()` fallbacks in lieu of Tailwind's `@property` layer. Arbitrary /
/// `(--var)` stops (`mask-t-from-[0px]`) route here via `mask_arbitrary`.
fn try_mask(base: &str, tokens: &ThemeTokens) -> Resolved {
    let rest = base.strip_prefix("mask-")?;

    // Radial shape / size keywords.
    match rest {
        "circle" => {
            return Some(Ok(mask_layer_decls(
                "radial",
                ("--pp-mask-radial-shape".into(), "circle".into()),
            )))
        }
        "ellipse" => {
            return Some(Ok(mask_layer_decls(
                "radial",
                ("--pp-mask-radial-shape".into(), "ellipse".into()),
            )))
        }
        "radial-closest-side"
        | "radial-farthest-side"
        | "radial-closest-corner"
        | "radial-farthest-corner" => {
            let size = rest.strip_prefix("radial-").unwrap().replace('-', " ");
            return Some(Ok(mask_layer_decls(
                "radial",
                ("--pp-mask-radial-size".into(), size),
            )));
        }
        _ => {}
    }

    // Linear edges: mask-{t,b,l,r,x,y}-{from,to}-<value>.
    for key in ["t", "b", "l", "r", "x", "y"] {
        if let Some(tail) = rest.strip_prefix(key).and_then(|r| r.strip_prefix('-')) {
            if let Some((kind, raw)) = split_from_to(tail) {
                let edges = mask_edges(key).unwrap();
                return Some(match mask_value(raw, tokens) {
                    Some((slot, val)) => Ok(mask_edge_decls(edges, kind, slot, &val)),
                    None => Err(mask_err(base, raw)),
                });
            }
        }
    }

    // Linear-angle / radial / conic layers.
    for layer in ["linear", "radial", "conic"] {
        if let Some(tail) = rest.strip_prefix(layer).and_then(|r| r.strip_prefix('-')) {
            if layer == "radial" {
                if let Some(pos) = tail.strip_prefix("at-") {
                    let set = (
                        "--pp-mask-radial-position".to_string(),
                        pos.replace('-', " "),
                    );
                    return Some(Ok(mask_layer_decls("radial", set)));
                }
            }
            return Some(match mask_gradient_var(layer, tail, tokens) {
                Some(Ok(set)) => Ok(mask_layer_decls(layer, set)),
                Some(Err(e)) => Err(e),
                None => Err(mask_err(base, tail)),
            });
        }
    }

    None
}

/// Arbitrary / `(--var)` mask stops: `mask-t-from-[0px]`,
/// `mask-radial-to-(--x)`, `mask-radial-at-[10%_20%]`. The value is an
/// arbitrary CSS length used verbatim as the stop *position* (typed
/// `[color:…]` hints are dropped upstream, so colour stops fall back to
/// position — an accepted subset limitation).
fn mask_arbitrary(base: &str, value: &str) -> Option<Decls> {
    let rest = base.strip_prefix("mask-")?;
    // `mask-radial-[<size>]` sets the radial size directly.
    if rest == "radial" {
        return Some(mask_layer_decls(
            "radial",
            ("--pp-mask-radial-size".to_string(), value.to_string()),
        ));
    }
    for key in ["t", "b", "l", "r", "x", "y"] {
        if let Some(kind) = rest.strip_prefix(key).and_then(|r| r.strip_prefix('-')) {
            if kind == "from" || kind == "to" {
                return Some(mask_edge_decls(
                    mask_edges(key).unwrap(),
                    kind,
                    "position",
                    value,
                ));
            }
        }
    }
    for layer in ["linear", "radial", "conic"] {
        if let Some(tail) = rest.strip_prefix(layer).and_then(|r| r.strip_prefix('-')) {
            if layer == "radial" && tail == "at" {
                let set = ("--pp-mask-radial-position".to_string(), value.to_string());
                return Some(mask_layer_decls("radial", set));
            }
            if tail == "from" || tail == "to" {
                let set = (
                    format!("--pp-mask-{layer}-{tail}-position"),
                    value.to_string(),
                );
                return Some(mask_layer_decls(layer, set));
            }
        }
    }
    None
}

/// `from-`/`to-` prefix split, returning `(kind, value)`.
fn split_from_to(tail: &str) -> Option<(&'static str, &str)> {
    if let Some(v) = tail.strip_prefix("from-") {
        Some(("from", v))
    } else {
        tail.strip_prefix("to-").map(|v| ("to", v))
    }
}

/// Resolve the single variable a linear-angle / radial / conic mask
/// utility sets: `from-/to-<value>` → a stop position/color, or a bare
/// angle (`mask-conic-45`, `mask-linear-90`) → the layer's `-position`.
/// `None` = not a gradient-var utility for this layer.
#[allow(clippy::type_complexity)]
fn mask_gradient_var(
    layer: &str,
    tail: &str,
    tokens: &ThemeTokens,
) -> Option<Result<(String, String), Diagnostic>> {
    if let Some((kind, raw)) = split_from_to(tail) {
        return Some(match mask_value(raw, tokens) {
            Some((slot, val)) => Ok((format!("--pp-mask-{layer}-{kind}-{slot}"), val)),
            None => Err(mask_err(&format!("mask-{layer}-{kind}"), raw)),
        });
    }
    // Bare angle for linear/conic → the layer position.
    if layer != "radial" {
        if let Ok(deg) = tail.parse::<f64>() {
            return Some(Ok((
                format!("--pp-mask-{layer}-position"),
                format!("{}deg", trim_num(deg)),
            )));
        }
    }
    None
}

/// A mask stop value → `(slot, value)` where `slot` is `position` (a
/// length/percentage/spacing) or `color`. Bare numbers use the spacing
/// scale (Tailwind's `mask-b-from-2`); `var(--x)` and lengths/percentages
/// are positions; anything else is resolved as a colour.
fn mask_value(value: &str, tokens: &ThemeTokens) -> Option<(&'static str, String)> {
    if value.ends_with('%') && value[..value.len() - 1].parse::<f64>().is_ok() {
        return Some(("position", value.to_string()));
    }
    if value.starts_with("var(") || value.starts_with("calc(") {
        return Some(("position", value.to_string()));
    }
    if value.parse::<f64>().is_ok() || value == "px" {
        return spacing(value).map(|v| ("position", v));
    }
    resolve_color_string(value, tokens, value)
        .ok()
        .map(|c| ("color", c))
}

fn mask_err(base: &str, value: &str) -> Diagnostic {
    Diagnostic::error(format!(
        "`{base}` expects a stop position or colour, got `{value}`"
    ))
}

/// `scroll-m*` / `scroll-p*` — scroll-margin / scroll-padding with the
/// usual direction + logical segments.
fn try_scroll(base: &str, _t: &ThemeTokens) -> Resolved {
    let (prop_base, seg) = if let Some(r) = base.strip_prefix("scroll-m") {
        ("scroll-margin", r)
    } else if let Some(r) = base.strip_prefix("scroll-p") {
        ("scroll-padding", r)
    } else {
        return None;
    };
    let (dir, val) = match seg.strip_prefix('-') {
        Some(v) => ("", v),
        None => seg.split_once('-')?,
    };
    let suffixes: &[&str] = match dir {
        "" => &[""],
        "x" => &["-left", "-right"],
        "y" => &["-top", "-bottom"],
        "t" => &["-top"],
        "b" => &["-bottom"],
        "l" => &["-left"],
        "r" => &["-right"],
        "s" => &["-inline-start"],
        "e" => &["-inline-end"],
        "bs" => &["-block-start"],
        "be" => &["-block-end"],
        _ => return None,
    };
    Some(match spacing(val) {
        Some(v) => Ok(suffixes
            .iter()
            .map(|s| (format!("{prop_base}{s}"), v.clone()))
            .collect()),
        None => Err(Diagnostic::error(format!(
            "`scroll` expects a spacing value, got `{val}`"
        ))),
    })
}

/// Gradient stops: `from-{c}` / `via-{c}` / `to-{c}` set the composable
/// `--pp-grad-*` vars consumed by `bg-linear-to-*`.
fn try_gradient(base: &str, tokens: &ThemeTokens) -> Resolved {
    // Angled gradients: `bg-linear-45`, `bg-conic-180`.
    if let Some(a) = base.strip_prefix("bg-linear-") {
        if let Ok(n) = a.parse::<f64>() {
            return Some(Ok(decl(
                "background-image",
                &format!("linear-gradient({}deg, var(--pp-grad-stops))", trim_num(n)),
            )));
        }
    }
    if let Some(a) = base.strip_prefix("bg-conic-") {
        if let Ok(n) = a.parse::<f64>() {
            return Some(Ok(decl(
                "background-image",
                &format!(
                    "conic-gradient(from {}deg, var(--pp-grad-stops))",
                    trim_num(n)
                ),
            )));
        }
    }
    // `from-50%` / `via-30%` / `to-90%` set a gradient-stop *position*.
    for (pfx, var) in [
        ("from-", "--pp-grad-from-position"),
        ("via-", "--pp-grad-via-position"),
        ("to-", "--pp-grad-to-position"),
    ] {
        if let Some(p) = base.strip_prefix(pfx) {
            if p.ends_with('%') && p[..p.len() - 1].parse::<f64>().is_ok() {
                return Some(Ok(decl(var, p)));
            }
        }
    }
    if let Some(c) = base.strip_prefix("from-") {
        return Some(resolve_color_string(c, tokens, base).map(|v| {
            vec![
                ("--pp-grad-from".into(), v),
                (
                    "--pp-grad-stops".into(),
                    "var(--pp-grad-from), var(--pp-grad-to, transparent)".into(),
                ),
            ]
        }));
    }
    if let Some(c) = base.strip_prefix("via-") {
        return Some(resolve_color_string(c, tokens, base).map(|v| {
            vec![
                ("--pp-grad-via".into(), v),
                (
                    "--pp-grad-stops".into(),
                    "var(--pp-grad-from), var(--pp-grad-via), var(--pp-grad-to, transparent)"
                        .into(),
                ),
            ]
        }));
    }
    if let Some(c) = base.strip_prefix("to-") {
        return Some(resolve_color_string(c, tokens, base).map(|v| decl("--pp-grad-to", &v)));
    }
    None
}

/// Numeric filter functions feeding the composable [`FILTER_CHAIN`].
fn try_filter(base: &str, _t: &ThemeTokens) -> Resolved {
    for (pfx, var, func) in [
        ("brightness-", "--pp-brightness", "brightness"),
        ("contrast-", "--pp-contrast", "contrast"),
        ("saturate-", "--pp-saturate", "saturate"),
    ] {
        if let Some(v) = base.strip_prefix(pfx) {
            return Some(
                parse_pct(v).map(|p| filter_decls(var, &format!("{func}({})", trim_num(p)))),
            );
        }
    }
    None
}

/// `divide-x-N` / `divide-y-N` widths and `divide-{color}` (applied to
/// children via the `emit_into` child-combinator tail).
fn try_divide(base: &str, tokens: &ThemeTokens) -> Resolved {
    if let Some(v) = base.strip_prefix("divide-x-") {
        if let Ok(n) = v.parse::<u32>() {
            return Some(Ok(decl("border-left-width", &format!("{n}px"))));
        }
    }
    if let Some(v) = base.strip_prefix("divide-y-") {
        if let Ok(n) = v.parse::<u32>() {
            return Some(Ok(decl("border-top-width", &format!("{n}px"))));
        }
    }
    if let Some(c) = base.strip_prefix("divide-") {
        return Some(resolve_color_value("border-color", c, tokens, base));
    }
    None
}

/// `bg-{color}` (the broadest colour family; tried last).
fn try_color(base: &str, tokens: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("bg-")?;
    Some(resolve_color_value("background-color", name, tokens, base))
}

// ── shared colour + numeric resolution ──────────────────────────────

fn parse_pct(value: &str) -> Result<f64, Diagnostic> {
    value
        .parse::<f64>()
        .map(|n| n / 100.0)
        .map_err(|_| Diagnostic::error(format!("expected a number, got `{value}`")))
}

fn int(value: &str) -> Result<i64, Diagnostic> {
    value
        .parse::<i64>()
        .map_err(|_| Diagnostic::error(format!("expected an integer, got `{value}`")))
}

/// Resolve a colour token name (possibly with a `/alpha` modifier) into
/// a single declaration. Named CSS colours and `transparent` pass
/// through; everything else must be a defined `--color-*` token.
/// Resolve a colour name (+ optional `/alpha`) to a CSS colour string.
fn resolve_color_string(
    name: &str,
    tokens: &ThemeTokens,
    base: &str,
) -> Result<String, Diagnostic> {
    let (name, alpha) = match name.split_once('/') {
        Some((n, a)) => (n, Some(a)),
        None => (name, None),
    };
    let color = match name {
        "transparent" => "transparent".to_string(),
        "current" => "currentColor".to_string(),
        "inherit" => "inherit".to_string(),
        "white" => "#ffffff".to_string(),
        "black" => "#000000".to_string(),
        // User `@theme` tokens win (override); the built-in Tailwind
        // palette is the fallback before erroring.
        _ => match tokens.var_for("color", name) {
            Some(var) => var,
            None => match crate::palette::lookup(name) {
                Some(value) => value.to_string(),
                None => return Err(unknown_token(base, "color", name, tokens)),
            },
        },
    };
    Ok(match alpha {
        Some(a) => format!("color-mix(in oklab, {color} {}, transparent)", alpha_pct(a)),
        None => color,
    })
}

/// Normalise an opacity-modifier value to a percentage. Bare integers
/// are already 0–100 (`/50` → `50%`); a bracketed/decimal fraction is
/// 0–1 (`/[0.5]` → `50%`); an explicit `%` passes through; `var()`/`calc()`
/// pass through unchanged.
fn alpha_pct(a: &str) -> String {
    if a.ends_with('%') {
        a.to_string()
    } else if let Ok(f) = a.parse::<f64>() {
        if a.contains('.') {
            format!("{}%", trim_num(f * 100.0))
        } else {
            format!("{a}%")
        }
    } else {
        a.to_string()
    }
}

fn resolve_color_value(
    prop: &str,
    name: &str,
    tokens: &ThemeTokens,
    base: &str,
) -> Result<Decls, Diagnostic> {
    resolve_color_string(name, tokens, base).map(|value| decl(prop, &value))
}

fn unknown_token(base: &str, family: &str, name: &str, tokens: &ThemeTokens) -> Diagnostic {
    let prefix = &base[..base.len() - name.len()];
    let candidates: Vec<String> = tokens
        .names_in_family(family)
        .map(|n| format!("{prefix}{n}"))
        .collect();
    let refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
    let mut diag = Diagnostic::error(format!("`--{family}-{name}` is not a defined token"));
    if let Some(hint) = suggest(base, refs) {
        diag = diag.with_help(format!("did you mean `{hint}`?"));
    }
    diag
}

/// Named font-size scale → (font-size, line-height).
fn font_size(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "xs" => ("0.75rem", "1rem"),
        "sm" => ("0.875rem", "1.25rem"),
        "base" => ("1rem", "1.5rem"),
        "lg" => ("1.125rem", "1.75rem"),
        "xl" => ("1.25rem", "1.75rem"),
        "2xl" => ("1.5rem", "2rem"),
        "3xl" => ("1.875rem", "2.25rem"),
        _ => return None,
    })
}

// ── arbitrary values ────────────────────────────────────────────────

/// Insert the whitespace CSS requires around `+` `-` `*` `/` operators
/// inside `calc()` / `min()` / `max()` / `clamp()`:
/// `calc(100vh-3.5rem)` → `calc(100vh - 3.5rem)`.
///
/// Authors can't type the spaces inside a class attribute (a space would
/// split the class into two), and CSS *rejects* an unspaced `+`/`-` in
/// `calc()` outright — the whole declaration is dropped — so without this
/// pass `h-[calc(100vh-3.5rem)]` silently produces no height at all. Mirrors
/// Tailwind's math-operator normalisation.
///
/// Operators inside a `var(--custom-prop)` name (or any non-math function
/// such as `url()`) are left untouched: a paren is classified by the
/// identifier in front of it, and only math-function descendants get the
/// operator spacing.
fn normalize_math(value: &str) -> String {
    // Fast path — only the four math functions need operator spacing.
    if !["calc(", "min(", "max(", "clamp("]
        .iter()
        .any(|f| value.contains(f))
    {
        return value.to_string();
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Ctx {
        Math,
        /// Inside `var()` / `env()` / `url()` / … — leave operators alone.
        Opaque,
    }

    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len() + 8);
    let mut stack: Vec<Ctx> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '(' {
            // Classify by the identifier immediately preceding the paren.
            let ident_end = out.len();
            let ident_start = out
                .as_bytes()
                .iter()
                .rposition(|b| !(b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_'))
                .map(|p| p + 1)
                .unwrap_or(0);
            let ctx = match &out[ident_start..ident_end] {
                "calc" | "min" | "max" | "clamp" => Ctx::Math,
                // Bare grouping paren inherits its parent's context.
                "" => stack.last().copied().unwrap_or(Ctx::Opaque),
                // Named non-math function (var, env, url, theme, …).
                _ => Ctx::Opaque,
            };
            stack.push(ctx);
            out.push('(');
            i += 1;
            continue;
        }
        if c == ')' {
            stack.pop();
            out.push(')');
            i += 1;
            continue;
        }
        if stack.last().copied() == Some(Ctx::Math) && matches!(c, '+' | '-' | '*' | '/') {
            let prev = out.trim_end().bytes().last();
            // `*` / `/` are always binary; `+` / `-` only when they follow a
            // completed value (digit, unit letter, `%`, `)` or `.`).
            let value_end = matches!(prev, Some(b) if b.is_ascii_alphanumeric() || matches!(b, b')' | b'%' | b'.'));
            // Guard scientific notation: a `-`/`+` right after `e`/`E` that
            // itself follows a digit is an exponent sign, not an operator.
            let exponent = matches!(c, '+' | '-') && matches!(prev, Some(b'e' | b'E')) && {
                let t = out.trim_end();
                let tb = t.as_bytes();
                tb.len() >= 2 && tb[tb.len() - 2].is_ascii_digit()
            };
            if (matches!(c, '*' | '/') || value_end) && !exponent {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push(' ');
                out.push(c);
                out.push(' ');
                i += 1;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Resolve `base-[value]`. The `_`-for-space convention is normalised, and
/// `calc()`/`min()`/`max()`/`clamp()` operator spacing is repaired (see
/// [`normalize_math`]).
fn resolve_arbitrary(base: &str, value: &str) -> Result<Decls, Diagnostic> {
    let v = normalize_math(&value.replace('_', " "));
    let typed = |ty: CssType, prop: &str| {
        if ty.accepts(value) {
            Ok(decl(prop, &v))
        } else {
            Err(Diagnostic::error(format!(
                "`{base}` expects a {ty:?} value, got `{value}`"
            )))
        }
    };
    let spread = |props: &[&str]| Ok(props.iter().map(|p| (p.to_string(), v.clone())).collect());
    // Box-edge families that expand to one-or-more properties:
    // `m-[4px]`, `px-[2rem]`, `inset-x-[10px]`, `scroll-mt-[2px]`. The
    // value is an arbitrary CSS length, used verbatim.
    if base == "gap-x" {
        return Ok(decl("column-gap", &v));
    }
    if base == "gap-y" {
        return Ok(decl("row-gap", &v));
    }
    if let Some(props) = spacing_props(base).or_else(|| inset_props(base)) {
        return spread(props);
    }
    if let Some(props) = scroll_arb_props(base) {
        let refs: Vec<&str> = props.iter().map(String::as_str).collect();
        return spread(&refs);
    }
    // Mask gradient stops with arbitrary / (--var) values:
    // `mask-t-from-[0px]`, `mask-radial-to-(--x)`, `mask-radial-at-[…]`.
    if let Some(d) = mask_arbitrary(base, &v) {
        return Ok(d);
    }
    // Strip a leading Tailwind type hint (`[color:…]`, `[length:…]`, …) so
    // the value is usable CSS, and remember whether it named a colour.
    let (hint, vv) = match v.split_once(':') {
        Some((t, rest))
            if matches!(
                t,
                "color"
                    | "length"
                    | "angle"
                    | "number"
                    | "percentage"
                    | "url"
                    | "image"
                    | "position"
                    | "family-name"
                    | "generic-name"
                    | "shadow"
            ) =>
        {
            (Some(t), rest.to_string())
        }
        _ => (None, v.clone()),
    };
    let is_color = hint == Some("color") || (hint.is_none() && CssType::Color.accepts(&vv));
    // Border width / colour by side (`border-[2px]`, `border-[#abc]`,
    // `border-x-[1px]`, `border-t-[#fff]`, …). Width vs colour is chosen by
    // `is_color`, the same way `ring`/`outline` disambiguate.
    {
        let kind = if is_color { "color" } else { "width" };
        let sides: &[&str] = match base {
            "border" => &["border"],
            "border-x" => &["border-left", "border-right"],
            "border-y" => &["border-top", "border-bottom"],
            "border-t" => &["border-top"],
            "border-r" => &["border-right"],
            "border-b" => &["border-bottom"],
            "border-l" => &["border-left"],
            "border-s" => &["border-inline-start"],
            "border-e" => &["border-inline-end"],
            _ => &[],
        };
        if !sides.is_empty() {
            return Ok(sides
                .iter()
                .map(|s| (format!("{s}-{kind}"), vv.clone()))
                .collect());
        }
    }
    // `divide-x-[2px]` / `divide-y-[3px]` widths and `divide-[#abc]` colour —
    // applied *between* children by the `emit_into` child-combinator tail
    // (keyed on the `divide-*` base), exactly like the scale-value forms.
    match base {
        "divide-x" => return Ok(decl("border-left-width", &vv)),
        "divide-y" => return Ok(decl("border-top-width", &vv)),
        "divide" if is_color => return Ok(decl("border-color", &vv)),
        _ => {}
    }
    // Families that take either a length or a colour (`ring-[3px]` vs
    // `ring-[#fff]`, `outline-[2px]` vs `outline-[red]`).
    match base {
        "outline" => {
            return Ok(if is_color {
                decl("outline-color", &vv)
            } else {
                vec![
                    ("outline-style".into(), "solid".into()),
                    ("outline-width".into(), vv),
                ]
            })
        }
        "ring" => {
            return Ok(if is_color {
                decl("--pp-ring-color", &vv)
            } else {
                shadow_decls(
                    "--pp-ring-shadow",
                    &format!("0 0 0 {vv} var(--pp-ring-color, currentcolor)"),
                )
            })
        }
        "ring-offset" => {
            return Ok(if is_color {
                decl("--pp-ring-offset-color", &vv)
            } else {
                shadow_decls(
                    "--pp-ring-offset-shadow",
                    &format!("0 0 0 {vv} var(--pp-ring-offset-color, #fff)"),
                )
            })
        }
        "inset-ring" => {
            return Ok(if is_color {
                decl("--pp-inset-ring-color", &vv)
            } else {
                shadow_decls(
                    "--pp-inset-ring-shadow",
                    &format!("inset 0 0 0 {vv} var(--pp-inset-ring-color, currentcolor)"),
                )
            })
        }
        "inset-shadow" => return Ok(shadow_decls("--pp-inset-shadow", &format!("inset {vv}"))),
        "text-shadow" => return Ok(decl("text-shadow", &vv)),
        "drop-shadow" => return Ok(decl("filter", &format!("drop-shadow({vv})"))),
        // gradient stops with arbitrary colours
        "from" => {
            return Ok(vec![
                ("--pp-grad-from".into(), vv),
                (
                    "--pp-grad-stops".into(),
                    "var(--pp-grad-from), var(--pp-grad-to, transparent)".into(),
                ),
            ])
        }
        "via" => {
            return Ok(vec![
                ("--pp-grad-via".into(), vv),
                (
                    "--pp-grad-stops".into(),
                    "var(--pp-grad-from), var(--pp-grad-via), var(--pp-grad-to, transparent)"
                        .into(),
                ),
            ])
        }
        "to" => return Ok(decl("--pp-grad-to", &vv)),
        "font" => {
            return Ok(if vv.chars().all(|c| c.is_ascii_digit()) {
                decl("font-weight", &vv)
            } else {
                decl("font-family", &vv)
            })
        }
        "decoration" => {
            return Ok(if is_color {
                decl("text-decoration-color", &vv)
            } else {
                decl("text-decoration-thickness", &vv)
            })
        }
        "stroke" if !is_color => return Ok(decl("stroke-width", &vv)),
        _ => {}
    }
    match base {
        "w" => typed(CssType::Length, "width"),
        "h" => typed(CssType::Length, "height"),
        "size" => Ok(vec![("width".into(), v.clone()), ("height".into(), v)]),
        "min-w" => typed(CssType::Length, "min-width"),
        "min-h" => typed(CssType::Length, "min-height"),
        "max-w" => typed(CssType::Length, "max-width"),
        "max-h" => typed(CssType::Length, "max-height"),
        "text" => Ok(decl("font-size", &v)),
        "tracking" => Ok(decl("letter-spacing", &v)),
        "leading" => Ok(decl("line-height", &v)),
        "rounded" => Ok(decl("border-radius", &v)),
        "shadow" => Ok(decl("box-shadow", &v)),
        "underline-offset" => Ok(decl("text-underline-offset", &v)),
        "grid-cols" => Ok(decl("grid-template-columns", &v)),
        "grid-rows" => Ok(decl("grid-template-rows", &v)),
        "transition" => Ok(decl("transition-property", &v)),
        // transition timing — value used verbatim (already carries its unit /
        // function, e.g. `300ms`, `cubic-bezier(…)`).
        "duration" => Ok(decl("transition-duration", &v)),
        "delay" => Ok(decl("transition-delay", &v)),
        "ease" => Ok(decl("transition-timing-function", &v)),
        // plain numeric / keyword families.
        "z" => Ok(decl("z-index", &v)),
        "order" => Ok(decl("order", &v)),
        "opacity" => Ok(decl("opacity", &v)),
        "columns" => Ok(decl("columns", &v)),
        "outline-offset" => Ok(decl("outline-offset", &v)),
        // transforms compose through the `--pp-*` chain so they stack with
        // sibling transforms, exactly like `translate-x` (the arbitrary value
        // is used verbatim — `1.05`, `3deg`, …).
        "scale" => Ok({
            let mut d = transform_decls("--pp-scale-x", &v);
            d.insert(0, ("--pp-scale-y".to_string(), v.clone()));
            d
        }),
        "scale-x" => Ok(transform_decls("--pp-scale-x", &v)),
        "scale-y" => Ok(transform_decls("--pp-scale-y", &v)),
        "scale-z" => Ok(transform_decls("--pp-scale-z", &v)),
        "rotate" => Ok(transform_decls("--pp-rotate", &v)),
        "rotate-x" => Ok(transform_decls("--pp-rotate-x", &v)),
        "rotate-y" => Ok(transform_decls("--pp-rotate-y", &v)),
        "rotate-z" => Ok(transform_decls("--pp-rotate-z", &v)),
        "skew-x" => Ok(transform_decls("--pp-skew-x", &v)),
        "skew-y" => Ok(transform_decls("--pp-skew-y", &v)),
        "top" => Ok(decl("top", &v)),
        "left" => Ok(decl("left", &v)),
        "right" => Ok(decl("right", &v)),
        "bottom" => Ok(decl("bottom", &v)),
        "indent" => Ok(decl("text-indent", &v)),
        "basis" => Ok(decl("flex-basis", &v)),
        // transform translates compose through the chain (negatable).
        "translate-x" => Ok(transform_decls("--pp-tx", &v)),
        "translate-y" => Ok(transform_decls("--pp-ty", &v)),
        "translate-z" => Ok(transform_decls("--pp-tz", &v)),
        "content" => Ok(decl("content", &v)),
        "aspect" => Ok(decl("aspect-ratio", &v)),
        "font-features" => Ok(decl("font-feature-settings", &v)),
        "bg-position" => Ok(decl("background-position", &v)),
        "bg-size" => Ok(decl("background-size", &v)),
        "tab" => Ok(decl("tab-size", &v)),
        "fill" => Ok(decl("fill", &vv)),
        "stroke" => Ok(decl("stroke", &vv)),
        "bg" if is_color => Ok(decl("background-color", &vv)),
        // non-colour `bg-[…]` is an image/gradient (url(), linear-gradient…)
        "bg" => Ok(decl("background-image", &vv)),
        "text-color" => Ok(decl("color", &vv)),
        "mask" => Ok(vec![
            ("-webkit-mask-image".into(), v.clone()),
            ("mask-image".into(), v),
        ]),
        "mask-position" => Ok(vec![
            ("-webkit-mask-position".into(), v.clone()),
            ("mask-position".into(), v),
        ]),
        "mask-size" => Ok(vec![
            ("-webkit-mask-size".into(), v.clone()),
            ("mask-size".into(), v),
        ]),
        _ => Err(Diagnostic::error(format!(
            "`{base}` does not support arbitrary `[…]` values"
        ))),
    }
}

// ── variants ────────────────────────────────────────────────────────

enum VariantResolution {
    Pseudo(String),
    /// Ancestor-conditioned: prepend `<sel> ` to the rule selector
    /// (e.g. `group-hover` → `.group:hover &`).
    Parent(String),
    /// Sibling-conditioned: prepend `<sel> ~ ` (e.g. `peer-checked`).
    Sibling(String),
    AtRule(String),
    Unknown,
}

/// Map a state name to its pseudo-class — shared by the plain, `group-*`,
/// and `peer-*` forms.
fn state_pseudo(name: &str) -> Option<&'static str> {
    Some(match name {
        "hover" => ":hover",
        "focus" => ":focus",
        "focus-visible" => ":focus-visible",
        "focus-within" => ":focus-within",
        "active" => ":active",
        "disabled" => ":disabled",
        "enabled" => ":enabled",
        "checked" => ":checked",
        "required" => ":required",
        "valid" => ":valid",
        "invalid" => ":invalid",
        "optional" => ":optional",
        "read-only" => ":read-only",
        "indeterminate" => ":indeterminate",
        "placeholder-shown" => ":placeholder-shown",
        "autofill" => ":autofill",
        "user-valid" => ":user-valid",
        "user-invalid" => ":user-invalid",
        "in-range" => ":in-range",
        "out-of-range" => ":out-of-range",
        "default" => ":default",
        "target" => ":target",
        "visited" => ":visited",
        "empty" => ":empty",
        "first" => ":first-child",
        "last" => ":last-child",
        "only" => ":only-child",
        "odd" => ":nth-child(odd)",
        "even" => ":nth-child(even)",
        "first-of-type" => ":first-of-type",
        "last-of-type" => ":last-of-type",
        "only-of-type" => ":only-of-type",
        _ => return None,
    })
}

/// The `@media` feature query for a named breakpoint / feature variant —
/// shared by the plain form and `not-{variant}` negation.
fn media_query(variant: &str) -> Option<&'static str> {
    Some(match variant {
        "sm" => "(min-width: 640px)",
        "md" => "(min-width: 768px)",
        "lg" => "(min-width: 1024px)",
        "xl" => "(min-width: 1280px)",
        "2xl" => "(min-width: 1536px)",
        "max-sm" => "(max-width: 639.98px)",
        "max-md" => "(max-width: 767.98px)",
        "max-lg" => "(max-width: 1023.98px)",
        "max-xl" => "(max-width: 1279.98px)",
        "dark" => "(prefers-color-scheme: dark)",
        "motion-safe" => "(prefers-reduced-motion: no-preference)",
        "motion-reduce" => "(prefers-reduced-motion: reduce)",
        "contrast-more" => "(prefers-contrast: more)",
        "contrast-less" => "(prefers-contrast: less)",
        "portrait" => "(orientation: portrait)",
        "landscape" => "(orientation: landscape)",
        "forced-colors" => "(forced-colors: active)",
        "pointer-fine" => "(pointer: fine)",
        "pointer-coarse" => "(pointer: coarse)",
        _ => return None,
    })
}

/// Bare `aria-{state}` → `[aria-{state}="true"]`, shared by the plain,
/// `group-*`, and `peer-*` forms.
fn aria_pseudo(s: &str) -> Option<String> {
    matches!(
        s,
        "checked"
            | "disabled"
            | "expanded"
            | "hidden"
            | "pressed"
            | "readonly"
            | "required"
            | "selected"
            | "busy"
            | "current"
            | "invalid"
            | "modal"
    )
    .then(|| format!("[aria-{s}=\"true\"]"))
}

fn resolve_variant(variant: &str) -> VariantResolution {
    use VariantResolution::*;
    // Pseudo-elements (authors supply `content-['']` for before/after).
    match variant {
        "before" => return Pseudo("::before".into()),
        "after" => return Pseudo("::after".into()),
        "marker" => return Pseudo("::marker".into()),
        "selection" => return Pseudo("::selection".into()),
        "first-letter" => return Pseudo("::first-letter".into()),
        "first-line" => return Pseudo("::first-line".into()),
        "file" => return Pseudo("::file-selector-button".into()),
        "backdrop" => return Pseudo("::backdrop".into()),
        "placeholder" => return Pseudo("::placeholder".into()),
        "details-content" => return Pseudo("::details-content".into()),
        // child / descendant variants
        "*" => return Pseudo(" > *".into()),
        "**" => return Pseudo(" *".into()),
        _ => {}
    }
    // Container-query variants: `@{size}`, `@min-{size}`, `@max-{size}`,
    // `@[arb]`, each optionally `/name`. Emit a `@container` at-rule.
    if let Some(rest) = variant.strip_prefix('@') {
        let (spec, name) = match rest.split_once('/') {
            Some((s, n)) => (s, Some(n)),
            None => (rest, None),
        };
        let (is_max, size_spec) = if let Some(s) = spec.strip_prefix("max-") {
            (true, s)
        } else if let Some(s) = spec.strip_prefix("min-") {
            (false, s)
        } else {
            (false, spec)
        };
        if let Some(width) = container_size(size_spec) {
            let cond = if is_max {
                format!("not (min-width: {width})")
            } else {
                format!("(min-width: {width})")
            };
            let q = match name {
                Some(n) => format!("@container {n} {cond}"),
                None => format!("@container {cond}"),
            };
            return AtRule(q);
        }
        return Unknown;
    }
    // Direction / state attribute variants.
    match variant {
        "ltr" => return Parent("[dir=\"ltr\"]".into()),
        "rtl" => return Parent("[dir=\"rtl\"]".into()),
        "open" => return Pseudo("[open]".into()),
        "inert" => return Pseudo("[inert]".into()),
        "starting" => return AtRule("@starting-style".into()),
        _ => {}
    }
    // Media / feature queries.
    if let Some(q) = media_query(variant) {
        return AtRule(format!("@media {q}"));
    }
    if variant == "print" {
        return AtRule("@media print".into());
    }
    // `not-{variant}` — negate a named state (`not-hover` → `:not(:hover)`)
    // or media query (`not-dark` → `@media not (…)`). Bracketed `not-[…]`
    // is handled below.
    if let Some(inner) = variant.strip_prefix("not-").filter(|i| !i.starts_with('[')) {
        if let Some(p) = state_pseudo(inner) {
            return Pseudo(format!(":not({p})"));
        }
        if let Some(q) = media_query(inner) {
            return AtRule(format!("@media not {q}"));
        }
    }
    // group-* / peer-* (named states).
    if let Some(s) = variant.strip_prefix("group-") {
        if let Some(p) = state_pseudo(s)
            .map(str::to_string)
            .or_else(|| aria_pseudo(s))
        {
            return Parent(format!(".group{p}"));
        }
        // `group-has-{state}` → `.group:has(:state) &`.
        if let Some(hs) = s.strip_prefix("has-").and_then(state_pseudo) {
            return Parent(format!(".group:has({hs})"));
        }
        // Arbitrary ancestor selector: `group-[&:hover]`, `group-[.foo]`.
        if let Some(inner) = bracketed(variant, "group-") {
            let sel = inner.replace('_', " ").replace('&', ".group");
            return Parent(if sel.contains(".group") {
                sel
            } else {
                format!(".group {sel}")
            });
        }
    }
    if let Some(s) = variant.strip_prefix("peer-") {
        if let Some(p) = state_pseudo(s)
            .map(str::to_string)
            .or_else(|| aria_pseudo(s))
        {
            return Sibling(format!(".peer{p}"));
        }
        if let Some(hs) = s.strip_prefix("has-").and_then(state_pseudo) {
            return Sibling(format!(".peer:has({hs})"));
        }
        if let Some(inner) = bracketed(variant, "peer-") {
            let sel = inner.replace('_', " ").replace('&', ".peer");
            return Sibling(if sel.contains(".peer") {
                sel
            } else {
                format!(".peer {sel}")
            });
        }
    }
    // Plain state pseudo-classes + bare aria-state.
    if let Some(p) = state_pseudo(variant) {
        return Pseudo(p.into());
    }
    if let Some(s) = variant
        .strip_prefix("aria-")
        .filter(|s| !s.starts_with('['))
    {
        if let Some(a) = aria_pseudo(s) {
            return Pseudo(a);
        }
    }
    // `has-{state}` → `:has(:state)` (bare; `has-[…]` is below).
    if let Some(hs) = variant.strip_prefix("has-").and_then(state_pseudo) {
        return Pseudo(format!(":has({hs})"));
    }
    // Functional / arbitrary variants.
    if let Some(inner) = bracketed(variant, "has-") {
        return Pseudo(format!(":has({})", inner.replace('_', " ")));
    }
    if let Some(inner) = bracketed(variant, "not-") {
        return Pseudo(format!(":not({})", inner.replace('_', " ")));
    }
    if let Some(inner) = bracketed(variant, "supports-") {
        return AtRule(format!("@supports ({})", inner.replace('_', " ")));
    }
    if let Some(inner) = bracketed(variant, "min-") {
        return AtRule(format!("@media (min-width: {inner})"));
    }
    if let Some(inner) = bracketed(variant, "max-") {
        return AtRule(format!("@media (max-width: {inner})"));
    }
    if variant.starts_with("data-[") && variant.ends_with(']') {
        return attr("data", &variant["data-[".len()..variant.len() - 1]);
    }
    if variant.starts_with("aria-[") && variant.ends_with(']') {
        return attr("aria", &variant["aria-[".len()..variant.len() - 1]);
    }
    Unknown
}

/// A container-query breakpoint width — the named `@{size}` scale
/// (Tailwind v4 `--container-*`) or an arbitrary `[480px]`. `None` if the
/// spec names no known size (so the variant is reported unknown).
fn container_size(spec: &str) -> Option<String> {
    if let Some(inner) = spec.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return Some(inner.replace('_', " "));
    }
    Some(
        match spec {
            "3xs" => "16rem",
            "2xs" => "18rem",
            "xs" => "20rem",
            "sm" => "24rem",
            "md" => "28rem",
            "lg" => "32rem",
            "xl" => "36rem",
            "2xl" => "42rem",
            "3xl" => "48rem",
            "4xl" => "56rem",
            "5xl" => "64rem",
            "6xl" => "72rem",
            "7xl" => "80rem",
            _ => return None,
        }
        .to_string(),
    )
}

/// Inner text of a `prefix[...]` functional variant, else `None`.
fn bracketed<'a>(variant: &'a str, prefix: &str) -> Option<&'a str> {
    variant
        .strip_prefix(prefix)?
        .strip_prefix('[')?
        .strip_suffix(']')
}

fn attr(kind: &str, inner: &str) -> VariantResolution {
    match inner.split_once('=') {
        Some((k, val)) => VariantResolution::Pseudo(format!("[{kind}-{k}=\"{val}\"]")),
        None => VariantResolution::Pseudo(format!("[{kind}-{inner}]")),
    }
}

/// Flat list of known base names for "did you mean" suggestions. A
/// representative slice — exact families resolve directly above.
fn known_bases() -> impl Iterator<Item = &'static str> {
    [
        "block",
        "inline",
        "inline-flex",
        "flex",
        "grid",
        "hidden",
        "relative",
        "absolute",
        "fixed",
        "sticky",
        "flex-col",
        "flex-row",
        "flex-1",
        "shrink",
        "shrink-0",
        "grow",
        "items-center",
        "items-start",
        "items-end",
        "justify-center",
        "justify-between",
        "truncate",
        "uppercase",
        "underline",
        "antialiased",
        "sr-only",
        "cursor-pointer",
        "overflow-hidden",
        "outline-none",
        "border",
        "rounded",
        "shadow",
        "ring",
        "transition",
        "transition-colors",
        "text-center",
        "text-left",
        "text-right",
        "font-medium",
        "font-semibold",
        "font-mono",
        "leading-relaxed",
        "tracking-tight",
    ]
    .into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_class;

    fn run(literal: &str, tokens: &ThemeTokens) -> Compilation {
        let mut out = Compilation::default();
        let parsed = parse_class(literal).unwrap();
        Registry::builtin().emit_into(
            literal,
            &parsed,
            tokens,
            Span::UNKNOWN,
            &CompileOptions::default(),
            &mut out,
        );
        out
    }

    #[test]
    fn math_spacing_repairs_unspaced_calc_minus() {
        // The bug: CSS drops `calc(100vh-3.5rem)` (unspaced `-` is invalid),
        // so `h-[calc(100vh-3.5rem)]` produced no height at all.
        assert_eq!(normalize_math("calc(100vh-3.5rem)"), "calc(100vh - 3.5rem)");
    }

    #[test]
    fn math_spacing_leaves_var_custom_property_names_intact() {
        // The `-` inside `--color-line` is part of the identifier, not an
        // operator — only the top-level subtraction gets spaced.
        assert_eq!(
            normalize_math("calc(var(--color-line)-2px)"),
            "calc(var(--color-line) - 2px)"
        );
    }

    #[test]
    fn math_spacing_handles_nested_min_calc() {
        assert_eq!(
            normalize_math("min(calc(100vw-2rem),380px)"),
            "min(calc(100vw - 2rem),380px)"
        );
        assert_eq!(
            normalize_math("calc(min(100vh-2rem,720px)-2.75rem)"),
            "calc(min(100vh - 2rem,720px) - 2.75rem)"
        );
    }

    #[test]
    fn math_spacing_is_idempotent_and_collapses_extra_spaces() {
        assert_eq!(
            normalize_math("calc(100vh  -  3.5rem)"),
            "calc(100vh - 3.5rem)"
        );
    }

    #[test]
    fn math_spacing_keeps_unary_and_exponent_signs() {
        // Leading unary minus and `2px - -3px` stay as written.
        assert_eq!(normalize_math("calc(-3px + 2px)"), "calc(-3px + 2px)");
        assert_eq!(normalize_math("calc(2px - -3px)"), "calc(2px - -3px)");
        // `1e-3` exponent is not an operator.
        assert_eq!(normalize_math("calc(1e-3px * 2)"), "calc(1e-3px * 2)");
    }

    #[test]
    fn math_spacing_ignores_values_without_math_functions() {
        // A bare `var()` / token with hyphens must not be touched.
        assert_eq!(normalize_math("var(--a-b)"), "var(--a-b)");
        assert_eq!(normalize_math("200px"), "200px");
        assert_eq!(normalize_math("minmax(0,1fr)"), "minmax(0,1fr)");
    }

    #[test]
    fn height_arbitrary_calc_emits_spaced_operator() {
        // End-to-end: the sidebar's `h-[calc(100vh-3.5rem)]`.
        let out = run("h-[calc(100vh-3.5rem)]", &palette());
        assert!(
            out.css.contains("height: calc(100vh - 3.5rem)"),
            "expected spaced calc, got: {}",
            out.css
        );
    }

    fn palette() -> ThemeTokens {
        let mut t = ThemeTokens::new();
        for name in [
            "surface",
            "ink-50",
            "ink-100",
            "accent",
            "accent-soft",
            "line",
            "danger",
        ] {
            t.insert(format!("color-{name}"), "#abc");
        }
        t.insert("shadow-card", "0 1px 2px rgba(0,0,0,.05)");
        t
    }

    fn css(literal: &str) -> String {
        run(literal, &palette()).css
    }

    fn is_err(literal: &str) -> bool {
        run(literal, &palette()).has_errors()
    }

    #[test]
    fn static_utilities() {
        assert_eq!(css("flex"), ".flex {\n  display: flex;\n}\n");
        assert!(css("items-center").contains("align-items: center;"));
        assert!(css("justify-between").contains("justify-content: space-between;"));
    }

    #[test]
    fn extended_utilities() {
        // display / object-fit / word-break / resize / animation
        assert!(css("contents").contains("display: contents;"));
        assert!(css("inline-grid").contains("display: inline-grid;"));
        assert!(css("object-contain").contains("object-fit: contain;"));
        assert!(css("break-all").contains("word-break: break-all;"));
        assert!(css("resize-y").contains("resize: vertical;"));
        assert!(css("animate-spin").contains("animation: spin 1s linear infinite;"));
        // grid
        assert!(css("grid-cols-2").contains("grid-template-columns: repeat(2, minmax(0, 1fr));"));
        assert!(css("col-span-2").contains("grid-column: span 2 / span 2;"));
        // directional gap + numeric leading + fraction inset
        assert!(css("gap-x-2").contains("column-gap:"));
        assert!(css("gap-y-1").contains("row-gap:"));
        assert!(css("leading-5").contains("line-height: 1.25rem;"));
        assert!(css("top-1/2").contains("top: 50%;"));
        // negative fractional translate composes via the transform chain
        assert!(css("-translate-y-1/2").contains("--pp-ty: -50%;"));
        assert!(css("-translate-y-1/2").contains("transform: translate(var(--pp-tx"));
        // per-side border colour
        assert!(css("border-t-accent").contains("border-top-color:"));
        // arbitrary min()/clamp() lengths now accepted
        assert!(css("w-[min(90%,32rem)]").contains("width: min(90%,32rem);"));
        // group-hover → ancestor-prefixed selector; last-of-type pseudo
        assert!(css("group-hover:opacity-50").contains(".group:hover "));
        assert!(css("last-of-type:border-b-0").contains(":last-of-type"));
    }

    #[test]
    fn known_class_is_not_a_utility_error() {
        let tokens = palette();
        let mut out = Compilation::default();
        let parsed = parse_class("cfe-card").unwrap();
        let options = CompileOptions {
            known_classes: std::iter::once("cfe-card".to_string()).collect(),
            ..Default::default()
        };
        Registry::builtin().emit_into(
            "cfe-card",
            &parsed,
            &tokens,
            Span::UNKNOWN,
            &options,
            &mut out,
        );
        assert!(!out.has_errors(), "{:?}", out.diagnostics);
        assert!(
            out.css.is_empty(),
            "author-defined class emits no utility CSS"
        );
    }

    #[test]
    fn gradient_filter_divide_batch() {
        // gradients
        assert!(css("bg-linear-to-r").contains("linear-gradient(to right, var(--pp-grad-stops))"));
        assert!(css("from-accent").contains("--pp-grad-from:"));
        assert!(css("from-accent").contains("--pp-grad-stops:"));
        assert!(css("to-surface").contains("--pp-grad-to:"));
        assert!(css("via-line").contains("--pp-grad-via:"));
        // filters (composable chain)
        assert!(css("blur-sm").contains("--pp-blur: blur(4px);"));
        assert!(css("blur-sm").contains("filter: var(--pp-blur"));
        assert!(css("brightness-50").contains("--pp-brightness: brightness(0.5);"));
        assert!(css("grayscale").contains("--pp-grayscale: grayscale(100%);"));
        // divide → child-combinator selector
        assert!(css("divide-y").contains("> :not([hidden]) ~ :not([hidden])"));
        assert!(css("divide-y").contains("border-top-width: 1px;"));
        assert!(css("divide-x-2").contains("border-left-width: 2px;"));
        assert!(css("divide-line").contains("border-color:"));
        // grid placement, origin, table, outline
        assert!(css("col-start-2").contains("grid-column-start: 2;"));
        assert!(css("row-end-3").contains("grid-row-end: 3;"));
        assert!(css("origin-top-left").contains("transform-origin: top left;"));
        assert!(css("border-spacing-2").contains("border-spacing:"));
        assert!(css("outline-offset-2").contains("outline-offset: 2px;"));
    }

    #[test]
    fn new_variants_batch() {
        assert!(css("dark:bg-surface").contains("@media (prefers-color-scheme: dark)"));
        assert!(css("before:block").contains("::before"));
        assert!(css("peer-checked:flex").contains(".peer:checked ~ "));
        assert!(css("group-disabled:opacity-50").contains(".group:disabled "));
        assert!(css("has-[a]:flex").contains(":has(a)"));
        assert!(css("max-md:hidden").contains("@media (max-width: 767.98px)"));
        assert!(css("print:hidden").contains("@media print"));
    }

    #[test]
    fn conditioned_variant_selectors_are_well_formed() {
        // Exact selectors — guards against the `..group` double-dot and the
        // dropped class dot (a substring check `.contains(".group:hover ")`
        // passes on the malformed `..group:hover group-hover\:…`).
        let g = css("group-hover:opacity-50");
        assert!(
            g.contains(".group:hover .group-hover\\:opacity-50 {"),
            "{g}"
        );
        let p = css("peer-checked:flex");
        assert!(p.contains(".peer:checked ~ .peer-checked\\:flex {"), "{p}");
        // No rule selector may start with a double dot.
        for cls in [
            "group-hover:opacity-50",
            "peer-checked:flex",
            "flex",
            "hover:bg-surface",
        ] {
            assert!(!css(cls).contains(".."), "double dot in {cls}");
        }
        // Stacked conditioners chain (both ancestors kept, in order).
        assert!(css("group-hover:group-focus:flex")
            .contains(".group:hover .group:focus .group-hover\\:group-focus\\:flex {"));
        // group-has-{state} composes too.
        assert!(css("group-has-checked:flex")
            .contains(".group:has(:checked) .group-has-checked\\:flex {"));
    }

    #[test]
    fn logical_3d_bg_batch() {
        // logical (RTL) spacing + inset
        assert!(css("ps-4").contains("padding-inline-start:"));
        assert!(css("me-2").contains("margin-inline-end:"));
        assert!(css("inset-s-0").contains("inset-inline-start: 0px;"));
        assert!(css("inset-be-2").contains("inset-block-end:"));
        // 3D transforms compose into the chain
        assert!(css("rotate-x-45").contains("--pp-rotate-x: 45deg;"));
        assert!(css("rotate-x-45").contains("rotateX(var(--pp-rotate-x"));
        assert!(css("translate-z-2").contains("--pp-tz:"));
        assert!(css("scale-z-110").contains("--pp-scale-z: 1.1;"));
        assert!(css("perspective-500").contains("perspective: 500px;"));
        assert!(css("transform-3d").contains("transform-style: preserve-3d;"));
        // background extras
        assert!(css("bg-cover").contains("background-size: cover;"));
        assert!(css("bg-center").contains("background-position: center;"));
        assert!(css("bg-clip-text").contains("-webkit-background-clip: text;"));
        assert!(css("bg-blend-multiply").contains("background-blend-mode: multiply;"));
        // text-shadow + break + columns
        assert!(css("text-shadow-sm").contains("text-shadow:"));
        assert!(css("break-inside-avoid").contains("break-inside: avoid;"));
        // scroll-margin/padding + touch + misc statics
        assert!(css("scroll-mt-4").contains("scroll-margin-top:"));
        assert!(css("scroll-p-2").contains("scroll-padding:"));
        assert!(css("scroll-px-3").contains("scroll-padding-left:"));
        assert!(css("touch-none").contains("touch-action: none;"));
        assert!(css("hyphens-auto").contains("hyphens: auto;"));
        assert!(css("contain-content").contains("contain: content;"));
        assert!(css("scheme-dark").contains("color-scheme: dark;"));
        assert!(css("transition-discrete").contains("transition-behavior: allow-discrete;"));
    }

    #[test]
    fn logical_sizing_and_mask() {
        // logical sizing → block-size / inline-size + min/max forms
        assert!(css("block-4").contains("block-size: calc(var(--spacing, 0.25rem) * 4);"));
        assert!(css("inline-full").contains("inline-size: 100%;"));
        assert!(css("inline-min").contains("inline-size: min-content;"));
        assert!(css("min-block-0").contains("min-block-size: 0px;"));
        assert!(css("max-inline-md").contains("max-inline-size: 28rem;"));
        // static `inline-flex`/`inline-block` still win over the family.
        assert!(css("inline-flex").contains("display: inline-flex;"));
        assert!(css("inline-block").contains("display: inline-block;"));

        // mask box statics (with -webkit- fallbacks)
        assert!(css("mask-none").contains("mask-image: none;"));
        assert!(css("mask-clip-border").contains("mask-clip: border-box;"));
        assert!(css("mask-clip-border").contains("-webkit-mask-clip: border-box;"));
        assert!(css("mask-origin-padding").contains("mask-origin: padding-box;"));
        assert!(css("mask-type-alpha").contains("mask-type: alpha;"));
        assert!(css("mask-add").contains("mask-composite: add;"));
        assert!(css("mask-subtract").contains("-webkit-mask-composite: source-out;"));
        assert!(css("mask-no-repeat").contains("mask-repeat: no-repeat;"));

        // edge-fade masks → see the `mask_gradients` test for the
        // composable layer model.
        assert!(css("mask-b-from-20%").contains("--pp-mask-bottom-from-position: 20%;"));
        assert!(css("mask-t-to-4")
            .contains("--pp-mask-top-to-position: calc(var(--spacing, 0.25rem) * 4);"));

        // arbitrary mask image / position / size (full-image override)
        assert!(css("mask-[url(/m.png)]").contains("mask-image: url(/m.png);"));
        assert!(css("mask-position-[center]").contains("mask-position: center;"));
        assert!(css("mask-size-[cover]").contains("mask-size: cover;"));
    }

    #[test]
    fn outline_animate_backdrop_batch() {
        // outline family
        assert!(css("outline").contains("outline-width: 1px;"));
        assert!(css("outline").contains("outline-style: solid;"));
        assert!(css("outline-2").contains("outline-width: 2px;"));
        assert!(css("outline-dashed").contains("outline-style: dashed;"));
        assert!(css("outline-line").contains("outline-color: var(--color-line);"));
        assert!(css("outline-hidden").contains("outline: 2px solid transparent;"));
        // outline-offset still owned by the numeric family
        assert!(css("outline-offset-2").contains("outline-offset: 2px;"));

        // animations (keyframes shipped in Preflight)
        assert!(css("animate-pulse").contains("animation: pulse 2s"));
        assert!(css("animate-ping").contains("animation: ping 1s"));
        assert!(css("animate-bounce").contains("animation: bounce 1s infinite;"));

        // backdrop-filter composes through its own chain (+ -webkit-)
        assert!(css("backdrop-blur-sm").contains("--pp-bd-blur: blur(4px);"));
        assert!(css("backdrop-blur-sm").contains("backdrop-filter: var(--pp-bd-blur"));
        assert!(css("backdrop-blur-sm").contains("-webkit-backdrop-filter:"));
        assert!(css("backdrop-brightness-50").contains("--pp-bd-brightness: brightness(0.5);"));
        assert!(css("backdrop-grayscale").contains("--pp-bd-grayscale: grayscale(100%);"));
        assert!(css("backdrop-opacity-75").contains("--pp-bd-opacity: opacity(0.75);"));

        // font-stretch + font-smoothing
        assert!(css("font-stretch-expanded").contains("font-stretch: expanded;"));
        assert!(css("font-stretch-75%").contains("font-stretch: 75%;"));
        assert!(css("subpixel-antialiased").contains("-webkit-font-smoothing: auto;"));

        // 3D transform markers
        assert!(css("scale-3d").contains("transform: translate("));
        assert!(css("translate-3d").contains("scaleZ(var(--pp-scale-z"));
    }

    #[test]
    fn ring_shadow_inset_batch() {
        // ring-offset feeds its own layer in the extended shadow chain
        assert!(css("ring-offset-2").contains("--pp-ring-offset-shadow: 0 0 0 2px"));
        assert!(css("ring-offset-line").contains("--pp-ring-offset-color: var(--color-line);"));
        // inset-ring
        assert!(css("inset-ring").contains("--pp-inset-ring-shadow: inset 0 0 0 1px"));
        assert!(css("inset-ring-2").contains("--pp-inset-ring-shadow: inset 0 0 0 2px"));
        assert!(css("inset-ring-accent").contains("--pp-inset-ring-color: var(--color-accent);"));
        // inset-shadow
        assert!(css("inset-shadow-sm").contains("--pp-inset-shadow: inset 0 2px 4px"));
        assert!(css("inset-shadow-none").contains("--pp-inset-shadow: 0 0 #0000;"));
        // the chain now layers all five effects
        assert!(css("shadow").contains("var(--pp-inset-shadow"));
        assert!(css("shadow").contains("var(--pp-ring-offset-shadow"));
        // positional inset utilities are unaffected
        assert!(css("inset-0").contains("top: 0px;"));
    }

    #[test]
    fn container_queries() {
        // container-context utility
        assert!(css("@container").contains("container-type: inline-size;"));
        assert!(css("@container-normal").contains("container-type: normal;"));
        assert!(css("@container-size").contains("container-type: size;"));
        assert!(css("@container/sidebar").contains("container: sidebar / inline-size;"));
        assert!(css("@container-size/main").contains("container: main / size;"));
        // container-query variants → @container at-rules
        assert!(css("@lg:flex").contains("@container (min-width: 32rem)"));
        assert!(css("@md:grid").contains("@container (min-width: 28rem)"));
        assert!(css("@max-lg:hidden").contains("@container not (min-width: 32rem)"));
        assert!(css("@min-[480px]:block").contains("@container (min-width: 480px)"));
        assert!(css("@lg/sidebar:flex").contains("@container sidebar (min-width: 32rem)"));
    }

    #[test]
    fn batch_keywords_and_arbitrary_targets() {
        // full blend-mode set + cursor keywords + decoration styles
        assert!(css("mix-blend-color-dodge").contains("mix-blend-mode: color-dodge;"));
        assert!(css("bg-blend-luminosity").contains("background-blend-mode: luminosity;"));
        assert!(css("cursor-grabbing").contains("cursor: grabbing;"));
        assert!(css("cursor-nw-resize").contains("cursor: nw-resize;"));
        assert!(css("decoration-wavy").contains("text-decoration-style: wavy;"));
        // misc keyword statics + viewport units
        assert!(css("justify-normal").contains("justify-content: normal;"));
        assert!(css("flex-wrap-reverse").contains("flex-wrap: wrap-reverse;"));
        assert!(css("grid-cols-none").contains("grid-template-columns: none;"));
        assert!(css("h-dvh").contains("height: 100dvh;"));
        assert!(css("min-h-svh").contains("min-height: 100svh;"));
        // arbitrary targets (length-or-colour disambiguated)
        assert!(css("outline-[2px]").contains("outline-width: 2px;"));
        assert!(css("outline-[#0088cc]").contains("outline-color: #0088cc;"));
        assert!(css("ring-[3px]").contains("--pp-ring-shadow: 0 0 0 3px"));
        assert!(css("ring-[#fff]").contains("--pp-ring-color: #fff;"));
        assert!(css("inset-shadow-[10px_10px]").contains("--pp-inset-shadow: inset 10px 10px"));
        assert!(css("text-shadow-[0_1px_#000]").contains("text-shadow: 0 1px #000;"));
        assert!(css("stroke-[2px]").contains("stroke-width: 2px;"));
        assert!(css("font-[550]").contains("font-weight: 550;"));
        assert!(css("from-[#fff]").contains("--pp-grad-from: #fff;"));
        // typed colour hint is stripped
        assert!(css("bg-[color:var(--c)]").contains("background-color: var(--c);"));
    }

    #[test]
    fn family_gap_closers() {
        // logical radius corners/sides
        assert!(css("rounded-s-lg").contains("border-start-start-radius:"));
        assert!(css("rounded-ee").contains("border-end-end-radius:"));
        // bare translate / skew drive both axes
        assert!(css("translate-4").contains("--pp-tx:"));
        assert!(css("translate-4").contains("--pp-ty:"));
        assert!(css("skew-6").contains("--pp-skew-x: 6deg;"));
        // inherit colour + gradient stop position + shadow colour
        assert!(css("bg-inherit").contains("background-color: inherit;"));
        assert!(css("from-50%").contains("--pp-grad-from-position: 50%;"));
        assert!(css("shadow-red-500").contains("--pp-shadow-color:"));
        // misc statics + arbitrary targets + viewport unit
        assert!(css("not-sr-only").contains("position: static;"));
        assert!(css("wrap-anywhere").contains("overflow-wrap: anywhere;"));
        assert!(css("tab-4").contains("tab-size: 4;"));
        assert!(css("bg-conic").contains("conic-gradient"));
        assert!(css("bg-linear-45").contains("linear-gradient(45deg,"));
        assert!(css("bg-position-[10px]").contains("background-position: 10px;"));
        // text size + line-height shorthand
        assert!(css("text-sm/6").contains("font-size:"));
        assert!(css("text-sm/6").contains("line-height: 1.5rem;"));
    }

    #[test]
    fn extended_variants() {
        // not-{state} → :not(pseudo); not-{media} → @media not
        assert!(css("not-hover:flex").contains(":not(:hover)"));
        assert!(css("not-first-of-type:flex").contains(":not(:first-of-type)"));
        assert!(css("not-dark:flex").contains("@media not (prefers-color-scheme: dark)"));
        // direction + state-attribute variants
        assert!(css("ltr:flex").contains("[dir=\"ltr\"] "));
        assert!(css("rtl:flex").contains("[dir=\"rtl\"] "));
        assert!(css("open:flex").contains("[open]"));
        assert!(css("inert:flex").contains("[inert]"));
    }

    #[test]
    fn mask_gradients() {
        // edge fade — scaffold + linear composition + the one stop var
        let t = css("mask-t-from-50%");
        assert!(t.contains("mask-image: var(--pp-mask-linear,"));
        assert!(t.contains("mask-composite: intersect;"));
        assert!(t.contains("--pp-mask-linear: var(--pp-mask-left,"));
        assert!(t.contains("--pp-mask-top: linear-gradient(to top,"));
        assert!(t.contains("--pp-mask-top-from-position: 50%;"));
        // to-stop + spacing-scale value
        assert!(css("mask-b-to-2")
            .contains("--pp-mask-bottom-to-position: calc(var(--spacing, 0.25rem) * 2);"));
        // x spans both left + right
        let x = css("mask-x-from-0");
        assert!(x.contains("--pp-mask-left-from-position: 0px;"));
        assert!(x.contains("--pp-mask-right-from-position: 0px;"));
        // radial / conic / linear layers compose independently
        assert!(css("mask-radial-from-20%")
            .contains("--pp-mask-radial: radial-gradient(var(--pp-mask-radial-stops));"));
        assert!(css("mask-radial-at-center").contains("--pp-mask-radial-position: center;"));
        assert!(css("mask-conic-180").contains("--pp-mask-conic-position: 180deg;"));
        assert!(css("mask-linear-45").contains("--pp-mask-linear-position: 45deg;"));
        // arbitrary + (--var) stops route through the same builders
        assert!(css("mask-t-from-[12px]").contains("--pp-mask-top-from-position: 12px;"));
        assert!(css("mask-radial-from-(--p)").contains("--pp-mask-radial-from-position: var(--p);"));
        // -webkit- fallbacks present
        assert!(t.contains("-webkit-mask-image:"));
        assert!(t.contains("-webkit-mask-composite: source-in;"));
    }

    #[test]
    fn arbitrary_box_edges() {
        // margin / padding / gap arbitrary values, used verbatim
        assert!(css("m-[4px]").contains("margin: 4px;"));
        assert!(css("px-[2rem]").contains("padding-left: 2rem;"));
        assert!(css("px-[2rem]").contains("padding-right: 2rem;"));
        assert!(css("gap-[10px]").contains("gap: 10px;"));
        assert!(css("gap-x-[1ch]").contains("column-gap: 1ch;"));
        // inset / position
        assert!(css("inset-x-[5px]").contains("left: 5px;"));
        assert!(css("inset-x-[5px]").contains("right: 5px;"));
        assert!(css("inset-bs-[2rem]").contains("inset-block-start: 2rem;"));
        assert!(css("start-[10%]").contains("inset-inline-start: 10%;"));
        // scroll-margin/padding, indent, basis
        assert!(css("scroll-mt-[8px]").contains("scroll-margin-top: 8px;"));
        assert!(css("scroll-p-[4px]").contains("scroll-padding: 4px;"));
        assert!(css("indent-[1em]").contains("text-indent: 1em;"));
        assert!(css("basis-[20%]").contains("flex-basis: 20%;"));
        // translate arbitrary composes through the transform chain
        assert!(css("translate-x-[10px]").contains("--pp-tx: 10px;"));
        assert!(css("translate-x-[10px]").contains("transform: translate(var(--pp-tx"));
        // the (--var) shorthand routes through the same targets
        assert!(css("m-(--gap)").contains("margin: var(--gap);"));
        assert!(css("inset-x-(--x)").contains("left: var(--x);"));
        // negation composes with arbitrary values (simple dimension → bare `-`)
        assert!(css("-m-[4px]").contains("margin: -4px;"));
        assert!(css("-translate-x-[10px]").contains("--pp-tx: -10px;"));
    }

    #[test]
    fn negative_utilities() {
        // spacing scale → calc(... * -1)
        assert!(css("-m-4").contains("margin: calc(calc(var(--spacing, 0.25rem) * 4) * -1);"));
        assert!(css("-mt-2").contains("margin-top: calc(calc(var(--spacing, 0.25rem) * 2) * -1);"));
        // simple dimensions get a bare `-` prefix
        assert!(css("-inset-px").contains("top: -1px;"));
        assert!(css("-inset-px").contains("left: -1px;"));
        // fractions → -50%
        assert!(css("-translate-y-1/2").contains("--pp-ty: -50%;"));
        assert!(css("-top-1/2").contains("top: -50%;"));
        // logical inset + scroll-margin negate too
        assert!(css("-inset-bs-2")
            .contains("inset-block-start: calc(calc(var(--spacing, 0.25rem) * 2) * -1);"));
        assert!(css("-scroll-mt-4")
            .contains("scroll-margin-top: calc(calc(var(--spacing, 0.25rem) * 4) * -1);"));
        // rotate/skew degrees
        assert!(css("-rotate-45").contains("--pp-rotate: -45deg;"));
        // negative space keeps the child-combinator selector
        assert!(css("-space-x-2").contains("> :not([hidden]) ~ :not([hidden])"));
        assert!(css("-space-x-2")
            .contains("margin-left: calc(calc(var(--spacing, 0.25rem) * 2) * -1);"));
        // non-negatable utilities are rejected (Tailwind emits nothing)
        assert!(is_err("-flex"));
        assert!(is_err("-bg-surface"));
        assert!(is_err("-p-4")); // padding can't be negative
        assert!(is_err("-z-10")); // z-index isn't negatable here
    }

    #[test]
    fn spacing_scale() {
        assert!(css("p-0").contains("padding: 0px;"));
        assert!(css("px-3").contains("padding-left: calc(var(--spacing, 0.25rem) * 3);"));
        assert!(css("px-3").contains("padding-right: calc(var(--spacing, 0.25rem) * 3);"));
        assert!(css("gap-2.5").contains("gap: calc(var(--spacing, 0.25rem) * 2.5);"));
        assert!(css("ml-auto").contains("margin-left: auto;"));
    }

    #[test]
    fn sizing() {
        assert!(css("w-full").contains("width: 100%;"));
        assert!(css("size-9").contains("width: calc(var(--spacing, 0.25rem) * 9);"));
        assert!(css("size-9").contains("height: calc(var(--spacing, 0.25rem) * 9);"));
        assert!(css("min-h-screen").contains("min-height: 100vh;"));
        assert!(css("max-w-md").contains("max-width: 28rem;"));
        assert!(css("w-px").contains("width: 1px;"));
    }

    #[test]
    fn colors_and_alpha() {
        assert!(css("bg-surface").contains("background-color: var(--color-surface);"));
        assert!(css("text-ink-50").contains("color: var(--color-ink-50);"));
        assert!(css("border-line").contains("border-color: var(--color-line);"));
        assert!(css("bg-transparent").contains("background-color: transparent;"));
        assert!(css("bg-surface/95").contains(
            "background-color: color-mix(in oklab, var(--color-surface) 95%, transparent);"
        ));
        // bracketed / fractional opacity modifiers normalise to a %:
        // `/[0.5]` (fraction) → 50%, `/[50%]` and `/50` → 50%.
        assert!(css("bg-surface/[0.5]")
            .contains("color-mix(in oklab, var(--color-surface) 50%, transparent)"));
        assert!(css("bg-surface/[50%]")
            .contains("color-mix(in oklab, var(--color-surface) 50%, transparent)"));
        // works on the builtin palette + other colour utilities too
        assert!(css("fill-red-500/[50%]").contains("color-mix(in oklab,"));
        assert!(css("divide-red-500/[50%]").contains("color-mix(in oklab,"));
    }

    #[test]
    fn space_between_and_numeric_variants() {
        let y = css("space-y-5");
        assert!(
            y.starts_with(".space-y-5 > :not([hidden]) ~ :not([hidden]) {"),
            "{y}"
        );
        assert!(y.contains("margin-top: calc(var(--spacing, 0.25rem) * 5);"));
        assert!(css("space-x-2").contains("margin-left: calc(var(--spacing, 0.25rem) * 2);"));
        assert!(css("tabular-nums").contains("font-variant-numeric: tabular-nums;"));
    }

    #[test]
    fn default_palette_colors() {
        // Built-in Tailwind palette resolves without a @theme entry.
        let slate = css("bg-slate-700");
        assert!(slate.contains("background-color: oklch("), "{slate}");
        assert!(css("text-red-500").contains("color: oklch("));
        assert!(css("ring-sky-500").contains("--pp-ring-color: oklch("));
        // Alpha modifier wraps the palette value too.
        assert!(css("bg-slate-900/50").contains("color-mix(in oklab, oklch("));
        // A @theme token of the same name overrides the palette.
        let mut t = palette();
        t.insert("color-red-500", "#ff0000");
        assert!(run("text-red-500", &t)
            .css
            .contains("color: var(--color-red-500);"));
        // Out-of-range shade is still an error.
        assert!(run("bg-slate-1234", &palette()).has_errors());
    }

    #[test]
    fn borders_widths_and_styles() {
        assert!(css("border").contains("border-width: 1px;"));
        assert!(css("border-0").contains("border-width: 0px;"));
        assert!(css("border-b").contains("border-bottom-width: 1px;"));
        assert!(css("border-dashed").contains("border-style: dashed;"));
        assert!(css("border-accent").contains("border-color: var(--color-accent);"));
    }

    #[test]
    fn rounded_shadow_ring() {
        assert!(css("rounded-md").contains("border-radius: var(--radius-md, 0.375rem);"));
        assert!(css("rounded-full").contains("border-radius: 9999px;"));
        assert!(css("rounded").contains("border-radius: var(--radius, 0.25rem);"));
        // Token-driven shadow scale, emitted through the composable chain.
        assert!(css("shadow-xl").contains("--pp-shadow: var(--shadow-xl, 0 20px 25px"));
        assert!(css("shadow-card").contains("--pp-shadow: var(--shadow-card);"));
        assert!(css("ring-2")
            .contains("--pp-ring-shadow: 0 0 0 2px var(--pp-ring-color, currentcolor);"));
        assert!(css("ring-accent").contains("--pp-ring-color: var(--color-accent);"));
    }

    #[test]
    fn shadow_and_ring_compose() {
        // Both utilities emit the *same* box-shadow chain and set only
        // their own layer var, so on one element both render. The chain
        // also reserves layers for inset-shadow / inset-ring / ring-offset.
        let chain = "box-shadow: var(--pp-inset-shadow, 0 0 #0000), \
                     var(--pp-inset-ring-shadow, 0 0 #0000), \
                     var(--pp-ring-offset-shadow, 0 0 #0000), \
                     var(--pp-ring-shadow, 0 0 #0000), var(--pp-shadow, 0 0 #0000);";
        assert!(css("shadow-xl").contains(chain), "shadow chain");
        assert!(css("ring-1").contains(chain), "ring chain");
    }

    #[test]
    fn leading_tracking_token_driven() {
        assert!(css("leading-relaxed").contains("line-height: var(--leading-relaxed, 1.625);"));
        assert!(css("tracking-widest").contains("letter-spacing: var(--tracking-widest, 0.1em);"));
    }

    #[test]
    fn numeric_families() {
        assert!(css("opacity-40").contains("opacity: 0.4;"));
        assert!(css("z-10").contains("z-index: 10;"));
        assert!(css("duration-200").contains("transition-duration: 200ms;"));
        // scale now composes via the transform chain (stackable).
        assert!(css("scale-95").contains("--pp-scale-x: 0.95;"));
        assert!(css("scale-95").contains("--pp-scale-y: 0.95;"));
        assert!(css("scale-95").contains("transform: translate(var(--pp-tx"));
    }

    #[test]
    fn named_text_size_is_token_driven() {
        // Like the spacing base, named sizes reference user-definable
        // theme tokens with the default scale as the fallback.
        let c = css("text-sm");
        assert!(c.contains("font-size: var(--text-sm, 0.875rem);"), "{c}");
        assert!(
            c.contains("line-height: var(--text-sm--line-height, 1.25rem);"),
            "{c}"
        );
        assert!(css("text-2xl").contains("font-size: var(--text-2xl, 1.5rem);"));
    }

    #[test]
    fn arbitrary_values() {
        assert!(css("text-[13px]").contains("font-size: 13px;"));
        // calc() operators are spaced so the value is valid CSS — an unspaced
        // `-` is rejected by the browser and the whole declaration is dropped.
        assert!(css("h-[calc(100vh-3.5rem)]").contains("height: calc(100vh - 3.5rem);"));
        assert!(css("grid-cols-[minmax(0,1fr)_80px]")
            .contains("grid-template-columns: minmax(0,1fr) 80px;"));
        assert!(css("tracking-[0.04em]").contains("letter-spacing: 0.04em;"));
    }

    #[test]
    fn arbitrary_transition_and_numeric_families() {
        assert!(css("duration-[300ms]").contains("transition-duration: 300ms;"));
        assert!(css("delay-[150ms]").contains("transition-delay: 150ms;"));
        assert!(css("ease-[cubic-bezier(0.4,0,0.2,1)]")
            .contains("transition-timing-function: cubic-bezier(0.4,0,0.2,1);"));
        assert!(css("z-[60]").contains("z-index: 60;"));
        assert!(css("order-[2]").contains("order: 2;"));
        assert!(css("opacity-[0.42]").contains("opacity: 0.42;"));
        assert!(css("columns-[3]").contains("columns: 3;"));
        assert!(css("outline-offset-[3px]").contains("outline-offset: 3px;"));
    }

    #[test]
    fn arbitrary_border_width_and_color() {
        assert!(css("border-[2px]").contains("border-width: 2px;"));
        assert!(css("border-[#abc]").contains("border-color: #abc;"));
        let bx = css("border-x-[1px]");
        assert!(bx.contains("border-left-width: 1px;"));
        assert!(bx.contains("border-right-width: 1px;"));
        assert!(css("border-t-[#fff]").contains("border-top-color: #fff;"));
    }

    #[test]
    fn arbitrary_divide_applies_to_children() {
        let d = css("divide-x-[2px]");
        assert!(d.contains("border-left-width: 2px;"), "{d}");
        // divide-* targets the between-children combinator, not the element.
        assert!(d.contains("> :not([hidden]) ~ :not([hidden])"), "{d}");
    }

    #[test]
    fn arbitrary_transforms_compose_through_chain() {
        let s = css("scale-[1.05]");
        assert!(s.contains("--pp-scale-x: 1.05;"), "{s}");
        assert!(s.contains("--pp-scale-y: 1.05;"), "{s}");
        assert!(s.contains("transform: translate("), "{s}");
        assert!(css("rotate-[3deg]").contains("--pp-rotate: 3deg;"));
        assert!(css("skew-x-[4deg]").contains("--pp-skew-x: 4deg;"));
    }

    #[test]
    fn text_size_with_bracketed_leading_is_not_mangled() {
        // Regression: `text-[16px]/[1.5]` used to splice the `]/[` slug into
        // the value (`font-size: 16px]/[1.5`). Both parts must parse cleanly.
        let c = css("text-[16px]/[1.5]");
        assert!(c.contains("font-size: 16px;"), "{c}");
        assert!(c.contains("line-height: 1.5;"), "{c}");
        assert!(!c.contains("16px]"), "value still mangled: {c}");
    }

    #[test]
    fn variants() {
        assert!(css("hover:bg-surface").starts_with(".hover\\:bg-surface:hover {"));
        assert!(css("md:flex").contains("@media (min-width: 768px) {"));
        assert!(css("data-[state=on]:bg-accent").contains("[data-state=\"on\"]"));
    }

    #[test]
    fn errors() {
        assert!(run("w-[red]", &palette()).has_errors());
        assert!(run("bg-nope", &palette()).has_errors());
        let out = run("flexx", &palette());
        assert!(out.has_errors());
        assert_eq!(
            out.diagnostics[0].help.as_deref(),
            Some("did you mean `flex`?")
        );
        // The example's apostrophe typo must be caught.
        assert!(run("border-b'", &palette()).has_errors());
    }
}
