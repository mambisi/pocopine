//! Pine Stylekit — a Pocopine-native utility-CSS compiler.
//!
//! Stylekit accepts a Tailwind-*shaped* class grammar
//! (`variant:variant:utility-scale`, arbitrary `utility-[value]`) and
//! compiles it against a typed [`registry`] of utility families and a
//! CSS-first [`tokens`] model. It emits deterministic static CSS with
//! **no browser-side runtime** — see RFC 092.
//!
//! Pipeline (`crates/pocopine-stylekit`):
//!
//! ```text
//! .poco AST ──extract──▶ used class set + spans
//!                              │
//!   @theme CSS ──tokens──▶ token model
//!                              │
//!                          parse + registry  ──▶  emit  ──▶  styles.css
//!                              │
//!                          diagnostics (fail loud)
//! ```
//!
//! This is the v1 skeleton: the class-string [`parse`]r, [`emit`]ter
//! escaping, and [`diagnostics`] suggestions are real; the [`registry`]
//! coverage and `.poco` [`extract`]ion are seams to be filled under the
//! phased plan in RFC 092 §10.

pub mod catalog;
pub mod diagnostics;
pub mod emit;
pub mod extract;
pub mod palette;
pub mod parse;
pub mod project;
pub mod registry;
pub mod render;
pub mod tokens;

pub use diagnostics::{Diagnostic, Severity, Span};
pub use project::{compile_project, ProjectCss, SourceFile};
pub use registry::{CssType, Registry};
pub use render::render;
pub use tokens::ThemeTokens;

/// Behavior knobs that vary build vs. dev and the porting experiment.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Downgrade unknown-utility errors to warnings (RFC 092 D5,
    /// `--stylekit-compat=warn`). Default `false` — errors are hard.
    pub compat_warn: bool,
    /// Prepend the base reset (Preflight) to the project stylesheet
    /// (M2-B). Default `true`; disable for pages that bring their own
    /// reset.
    pub preflight: bool,
    /// Class names defined as real rules in the author's CSS (the
    /// non-`@theme` passthrough). These are valid component classes, not
    /// utilities — the compiler skips them instead of erroring.
    pub known_classes: std::collections::BTreeSet<String>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            compat_warn: false,
            preflight: true,
            known_classes: std::collections::BTreeSet::new(),
        }
    }
}

/// The result of compiling a set of used classes against the registry
/// and token model.
#[derive(Debug, Default)]
pub struct Compilation {
    /// Deterministic CSS output for the used utilities.
    pub css: String,
    /// Diagnostics gathered during compilation (errors + warnings).
    pub diagnostics: Vec<Diagnostic>,
}

impl Compilation {
    /// Whether any diagnostic is error-severity. `pocopine build` and
    /// `pocopine dev` must fail loud when this is true (RFC 092 D6).
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

/// The Stylekit compiler facade: holds the resolved registry + token
/// model and turns an extracted class set into [`Compilation`] output.
#[derive(Debug)]
pub struct Compiler {
    registry: Registry,
    tokens: ThemeTokens,
    options: CompileOptions,
}

impl Compiler {
    /// Construct a compiler from a token model, using the default
    /// built-in utility registry.
    pub fn new(tokens: ThemeTokens, options: CompileOptions) -> Self {
        Self {
            registry: Registry::builtin(),
            tokens,
            options,
        }
    }

    /// Compile a set of used classes (with spans) into CSS + diagnostics.
    ///
    /// This is the seam every front-end (`build`, `dev`, the hidden
    /// `stylekit` debug verb) funnels through.
    pub fn compile(&self, used: &[extract::UsedClass]) -> Compilation {
        tracing::debug!(
            target: "pocopine.log",
            count = used.len(),
            "stylekit: compiling used classes"
        );

        let mut out = Compilation::default();

        // Parse first, then emit in cascade order. Utility rules all carry
        // ~equal specificity, so when two variants of the same property both
        // match an element the *source order* decides the winner — a plain
        // alphabetical sort (the order `used` arrives in) gets this wrong two
        // ways, both of which this re-ordering fixes (matching Tailwind's
        // layering):
        //
        //   1. Responsive `@media` rules sort by their prefix letter
        //      (`lg`→"l", `md`→"m", `sm`→"s"), so a bare `lg:` block lands
        //      *before* a `sm:` block — breakpoints cascade in reverse and a
        //      smaller breakpoint clobbers a larger one. They must emit after
        //      the non-responsive rules, ascending (sm < md < lg < xl < 2xl).
        //   2. `data-[…]:`/`aria-[…]:` (d) sort before `hover:` (h), so
        //      hovering an active element lets `hover:` clobber its
        //      `data-[state=on]:` styling. State variants must emit *after*
        //      pseudo-class variants.
        let mut parsed: Vec<(&extract::UsedClass, parse::ParsedClass)> =
            Vec::with_capacity(used.len());
        for used in used {
            match parse::parse_class(&used.value) {
                Ok(p) => parsed.push((used, p)),
                Err(diag) => out.diagnostics.push(diag.at(used.span)),
            }
        }
        // Stable sort keeps the incoming literal order within each layer, so
        // output stays deterministic.
        parsed.sort_by_key(|(_, p)| cascade_sort_key(p));

        for (used, parsed) in &parsed {
            self.registry.emit_into(
                &used.value,
                parsed,
                &self.tokens,
                used.span,
                &self.options,
                &mut out,
            );
        }
        out
    }
}

/// Emission-order key. Lower keys emit first (and so lose to later keys on
/// equal specificity). Ordered by `(breakpoint, state)`:
///
/// * `breakpoint` — `0` for non-responsive rules, then `sm`/`md`/`lg`/`xl`/
///   `2xl` ascending (`1..=5`). Responsive `@media` rules thus emit after the
///   base layer and in min-width order, so a larger breakpoint wins.
/// * `state` — `0` for base/pseudo-class variants, `1` for component-state
///   attribute variants (`data-[…]`, `aria-[…]`), so an active/checked element
///   keeps its styling when hovered or focused.
fn cascade_sort_key(parsed: &parse::ParsedClass) -> (u8, u8) {
    let breakpoint = parsed
        .variants
        .iter()
        .filter_map(|v| match v.0.as_str() {
            "sm" => Some(1u8),
            "md" => Some(2),
            "lg" => Some(3),
            "xl" => Some(4),
            "2xl" => Some(5),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let state = u8::from(
        parsed
            .variants
            .iter()
            .any(|v| v.0.starts_with("data-[") || v.0.starts_with("aria-[")),
    );
    (breakpoint, state)
}
