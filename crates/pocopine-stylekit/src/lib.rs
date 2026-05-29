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
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Downgrade unknown-utility errors to warnings (RFC 092 D5,
    /// `--stylekit-compat=warn`). Default `false` — errors are hard.
    pub compat_warn: bool,
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
        for used in used {
            match parse::parse_class(&used.value) {
                Ok(parsed) => self.registry.emit_into(
                    &used.value,
                    &parsed,
                    &self.tokens,
                    used.span,
                    &self.options,
                    &mut out,
                ),
                Err(diag) => out.diagnostics.push(diag.at(used.span)),
            }
        }
        out
    }
}
