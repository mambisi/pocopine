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

        let mut selector = escape_selector(literal);
        let mut at_rule = None;
        for variant in &parsed.variants {
            match resolve_variant(&variant.0) {
                VariantResolution::Pseudo(suffix) => selector.push_str(&suffix),
                VariantResolution::AtRule(at) => at_rule = Some(at),
                VariantResolution::Unknown => {
                    out.diagnostics.push(
                        Diagnostic::error(format!("unknown variant `{}`", variant.0)).at(span),
                    );
                    return;
                }
            }
        }
        // `space-{x,y}` apply margins *between* children, not to the
        // element itself — append the child-combinator tail.
        if parsed.base.starts_with("space-y-") || parsed.base.starts_with("space-x-") {
            selector.push_str(" > :not([hidden]) ~ :not([hidden])");
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

        if let Some(value) = &parsed.arbitrary {
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
            try_inset,
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
            try_backdrop_blur,
            try_numeric,
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
        "hidden" => d("display", "none"),
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
        "flex-nowrap" => d("flex-wrap", "nowrap"),
        "flex-none" => d("flex", "none"),
        "flex-1" => d("flex", "1 1 0%"),
        "flex-auto" => d("flex", "1 1 auto"),
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
        // outline
        "outline-none" => multi(&[
            ("outline", "2px solid transparent"),
            ("outline-offset", "2px"),
        ]),
        // borders (style / bare width)
        "border" => d("border-width", "1px"),
        "border-0" => d("border-width", "0px"),
        "border-solid" => d("border-style", "solid"),
        "border-dashed" => d("border-style", "dashed"),
        "border-dotted" => d("border-style", "dotted"),
        "border-double" => d("border-style", "double"),
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
        // backdrop
        "backdrop-blur" => d("backdrop-filter", "blur(8px)"),
        _ => None,
    }
}

// ── prefix families ─────────────────────────────────────────────────

/// `p|px|py|pt|pb|pl|pr|m|mx|my|mt|mb|ml|mr|gap|gap-x|gap-y` + value.
fn try_spacing(base: &str, _t: &ThemeTokens) -> Resolved {
    let (prefix, value) = base.split_once('-')?;
    let props: &[&str] = match prefix {
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
        "gap" => &["gap"],
        _ => return None,
    };
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

/// `inset|inset-x|inset-y|top|right|bottom|left` + value.
fn try_inset(base: &str, _t: &ThemeTokens) -> Resolved {
    let (prefix, value) = base.split_once('-')?;
    let props: &[&str] = match prefix {
        "top" => &["top"],
        "right" => &["right"],
        "bottom" => &["bottom"],
        "left" => &["left"],
        "inset" => match value.split_once('-') {
            Some(("x", _)) => &["left", "right"],
            Some(("y", _)) => &["top", "bottom"],
            _ => &["top", "right", "bottom", "left"],
        },
        _ => return None,
    };
    let value = match prefix {
        "inset" => value
            .strip_prefix("x-")
            .or_else(|| value.strip_prefix("y-"))
            .unwrap_or(value),
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
    // Numeric width like border-2.
    if let Ok(n) = name.parse::<u32>() {
        return Some(Ok(decl("border-width", &format!("{n}px"))));
    }
    Some(resolve_color_value("border-color", name, tokens, base))
}

fn try_rounded(base: &str, _t: &ThemeTokens) -> Resolved {
    if base == "rounded" {
        return Some(Ok(decl("border-radius", "0.25rem")));
    }
    let name = base.strip_prefix("rounded-")?;
    let v = match name {
        "none" => "0px",
        "sm" => "0.125rem",
        "md" => "0.375rem",
        "lg" => "0.5rem",
        "xl" => "0.75rem",
        "2xl" => "1rem",
        "3xl" => "1.5rem",
        "full" => "9999px",
        _ => return Some(Err(Diagnostic::error(format!("unknown radius `{name}`")))),
    };
    Some(Ok(decl("border-radius", v)))
}

fn try_shadow(base: &str, tokens: &ThemeTokens) -> Resolved {
    if base == "shadow" {
        return Some(Ok(decl(
            "box-shadow",
            "0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1)",
        )));
    }
    let name = base.strip_prefix("shadow-")?;
    let preset = match name {
        "none" => Some("none"),
        "sm" => Some("0 1px 2px 0 rgb(0 0 0 / 0.05)"),
        "md" => Some("0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1)"),
        "lg" => Some("0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1)"),
        "xl" => Some("0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1)"),
        "2xl" => Some("0 25px 50px -12px rgb(0 0 0 / 0.25)"),
        _ => None,
    };
    if let Some(v) = preset {
        return Some(Ok(decl("box-shadow", v)));
    }
    // Token-backed: shadow-card → var(--shadow-card).
    if tokens.var_for("shadow", name).is_some() {
        return Some(Ok(decl("box-shadow", &format!("var(--shadow-{name})"))));
    }
    Some(Err(Diagnostic::error(format!(
        "`--shadow-{name}` is not a defined token"
    ))))
}

/// `ring` / `ring-{n}` widths and `ring-{color}`.
fn try_ring(base: &str, tokens: &ThemeTokens) -> Resolved {
    if base == "ring" {
        return Some(Ok(decl(
            "box-shadow",
            "0 0 0 3px var(--pp-ring-color, currentcolor)",
        )));
    }
    let name = base.strip_prefix("ring-")?;
    if let Ok(n) = name.parse::<u32>() {
        return Some(Ok(decl(
            "box-shadow",
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

fn try_leading(base: &str, _t: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("leading-")?;
    let v = match name {
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
    Some(Ok(decl("line-height", v)))
}

fn try_tracking(base: &str, _t: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("tracking-")?;
    let v = match name {
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
    Some(Ok(decl("letter-spacing", v)))
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

/// `decoration-{n}` thickness and `decoration-{color}`.
fn try_decoration(base: &str, tokens: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("decoration-")?;
    if let Ok(n) = name.parse::<u32>() {
        return Some(Ok(decl("text-decoration-thickness", &format!("{n}px"))));
    }
    Some(resolve_color_value(
        "text-decoration-color",
        name,
        tokens,
        base,
    ))
}

fn try_backdrop_blur(base: &str, _t: &ThemeTokens) -> Resolved {
    let name = base.strip_prefix("backdrop-blur-")?;
    let v = match name {
        "none" => "0",
        "sm" => "4px",
        "md" => "12px",
        "lg" => "16px",
        "xl" => "24px",
        "2xl" => "40px",
        "3xl" => "64px",
        _ => return Some(Err(Diagnostic::error(format!("unknown blur `{name}`")))),
    };
    Some(Ok(decl("backdrop-filter", &format!("blur({v})"))))
}

/// Numeric families: `opacity-N`, `z-N`, `duration-N`, `scale-N`.
fn try_numeric(base: &str, _t: &ThemeTokens) -> Resolved {
    let (prefix, value) = base.split_once('-')?;
    match prefix {
        "opacity" => Some(parse_pct(value).map(|p| decl("opacity", &trim_num(p)))),
        "z" => Some(int(value).map(|n| decl("z-index", &n.to_string()))),
        "duration" => Some(int(value).map(|n| decl("transition-duration", &format!("{n}ms")))),
        "scale" => {
            Some(parse_pct(value).map(|p| decl("transform", &format!("scale({})", trim_num(p)))))
        }
        _ => None,
    }
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
fn resolve_color_value(
    prop: &str,
    name: &str,
    tokens: &ThemeTokens,
    base: &str,
) -> Result<Decls, Diagnostic> {
    let (name, alpha) = match name.split_once('/') {
        Some((n, a)) => (n, Some(a)),
        None => (name, None),
    };

    let color = match name {
        "transparent" => "transparent".to_string(),
        "current" => "currentColor".to_string(),
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

    let value = match alpha {
        Some(a) => format!("color-mix(in oklab, {color} {a}%, transparent)"),
        None => color,
    };
    Ok(decl(prop, &value))
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

/// Resolve `base-[value]`. The `_`-for-space convention is normalised.
fn resolve_arbitrary(base: &str, value: &str) -> Result<Decls, Diagnostic> {
    let v = value.replace('_', " ");
    let typed = |ty: CssType, prop: &str| {
        if ty.accepts(value) {
            Ok(decl(prop, &v))
        } else {
            Err(Diagnostic::error(format!(
                "`{base}` expects a {ty:?} value, got `{value}`"
            )))
        }
    };
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
        "top" => Ok(decl("top", &v)),
        "left" => Ok(decl("left", &v)),
        "bg" => typed(CssType::Color, "background-color"),
        "text-color" => typed(CssType::Color, "color"),
        _ => Err(Diagnostic::error(format!(
            "`{base}` does not support arbitrary `[…]` values"
        ))),
    }
}

// ── variants ────────────────────────────────────────────────────────

enum VariantResolution {
    Pseudo(String),
    AtRule(String),
    Unknown,
}

fn resolve_variant(variant: &str) -> VariantResolution {
    use VariantResolution::*;
    match variant {
        "hover" => Pseudo(":hover".into()),
        "focus" => Pseudo(":focus".into()),
        "focus-visible" => Pseudo(":focus-visible".into()),
        "focus-within" => Pseudo(":focus-within".into()),
        "active" => Pseudo(":active".into()),
        "disabled" => Pseudo(":disabled".into()),
        "checked" => Pseudo(":checked".into()),
        "first" => Pseudo(":first-child".into()),
        "last" => Pseudo(":last-child".into()),
        "placeholder" => Pseudo("::placeholder".into()),
        "sm" => AtRule("@media (min-width: 640px)".into()),
        "md" => AtRule("@media (min-width: 768px)".into()),
        "lg" => AtRule("@media (min-width: 1024px)".into()),
        "xl" => AtRule("@media (min-width: 1280px)".into()),
        "2xl" => AtRule("@media (min-width: 1536px)".into()),
        v if v.starts_with("data-[") && v.ends_with(']') => {
            attr("data", &v["data-[".len()..v.len() - 1])
        }
        v if v.starts_with("aria-[") && v.ends_with(']') => {
            attr("aria", &v["aria-[".len()..v.len() - 1])
        }
        _ => Unknown,
    }
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

    #[test]
    fn static_utilities() {
        assert_eq!(css("flex"), ".flex {\n  display: flex;\n}\n");
        assert!(css("items-center").contains("align-items: center;"));
        assert!(css("justify-between").contains("justify-content: space-between;"));
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
        assert!(css("rounded-md").contains("border-radius: 0.375rem;"));
        assert!(css("rounded-full").contains("border-radius: 9999px;"));
        assert!(css("shadow-card").contains("box-shadow: var(--shadow-card);"));
        assert!(css("ring-2").contains("box-shadow: 0 0 0 2px var(--pp-ring-color, currentcolor);"));
        assert!(css("ring-accent").contains("--pp-ring-color: var(--color-accent);"));
    }

    #[test]
    fn numeric_families() {
        assert!(css("opacity-40").contains("opacity: 0.4;"));
        assert!(css("z-10").contains("z-index: 10;"));
        assert!(css("duration-200").contains("transition-duration: 200ms;"));
        assert!(css("scale-95").contains("transform: scale(0.95);"));
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
        assert!(css("h-[calc(100vh-3.5rem)]").contains("height: calc(100vh-3.5rem);"));
        assert!(css("grid-cols-[minmax(0,1fr)_80px]")
            .contains("grid-template-columns: minmax(0,1fr) 80px;"));
        assert!(css("tracking-[0.04em]").contains("letter-spacing: 0.04em;"));
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
