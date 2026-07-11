//! `pocopine-macros` — `#[component]` and `#[handlers]` attribute macros.
//!
//! `#[component]` annotates a plain struct and emits:
//!   * `impl ComponentState` (proxy `get`/`set`/`keys`/`invoke` over public
//!     fields via `serde_wasm_bindgen`)
//!   * `impl Self { pub fn register() { ... } }` that wires component, its
//!     template, and optional stylesheet into the runtime.
//!
//! `#[handlers]` annotates `impl MyStruct { ... }` and emits:
//!   * the user's impl block unchanged
//!   * an `impl HandlerDispatch` whose match-arms dispatch to each method.
//!
//! Defaults when the user passes no arguments to `#[component]`:
//!   * `name = <kebab-case of the struct ident>`
//!   * `template = "<StructIdent>.poco"` (relative to the calling `.rs`)
//!   * `style` is omitted unless explicit
//!
//! A struct ident whose kebab-case matches a known HTML element is rejected
//! at compile time, since a collision would mask a real HTML element in
//! parent templates.
//!
//! RFC 050 §4.5 — the `#[component]` macro now runs the `.poco`
//! through `template_parser::parse_strict` at macro expansion
//! time and emits `syn::Error`-shaped diagnostics rendered via
//! `annotate-snippets` (one error block per offending construct).
//! Reading the template off disk at expansion time requires
//! `proc_macro_span` on nightly — the pocopine workspace is
//! already pinned to nightly rustc.

#![feature(proc_macro_span)]

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Expr, ExprClosure, ExprLit, Fields, FnArg, ImplItem, ItemFn, ItemImpl,
    ItemMod, ItemStruct, Lit, LitStr, Meta, MetaNameValue, Pat, PatType, Path, Token, Type,
    parse::{Parse, ParseStream, Parser},
    parse_macro_input,
    punctuated::Punctuated,
};

// RFC 050 — compile-time `.poco` template parser + diagnostic
// renderer. Both host-only (proc-macro crate), invisible to wasm.
mod diagnostics;
// RFC 050 parser now lives in the shared `pocopine-template-parser`
// crate so non-macro build tooling (pocopine-stylekit) can reuse it.
// Re-export under the historical path so in-crate
// `crate::template_parser::…` references keep resolving unchanged.
mod template_parser {
    pub use pocopine_template_parser::*;
}
// RFC 049 — `#[slot]` helper-attribute parsing + marker-trait
// emission. Inert helper consumed by `#[component]`.
mod slot;
// RFC 049 — `uses = [...]` list parsing for consumer
// `#[component]` macros. Builds the local tag → TypePath table
// the consumer-side scan resolves child tags against.
mod uses;
// RFC 049 — consumer-side template scan that emits the trait-
// bound assertions enforcing slot contracts. Consumes the
// TemplateAst parsed by `template_parser` and the UsesTable
// built by `uses`.
mod slot_assertions;
// RFC 054 — compile-time row-plan emitter for keyed `pp-for`.
// Walks the AST, validates expressions via `pocopine-expr`,
// and emits a `&[StaticRowPlan]` literal alongside template
// stamps the caller injects into the source.
mod for_plan;
mod forbidden_directives;
mod pp_for_diagnostics;
mod template_plan;
// Compile-time validation of template expression roots — marker
// references resolved by rustc against `#[component]`-emitted
// field markers and `#[handlers]`-emitted computed/handler
// markers.
mod template_paths;
// RFC-100 — `asset!` content-addressed asset references.
mod assets;

/// RFC 045 + RFC 050 §4.5 — read the `.poco` off disk, parse
/// it with `template_parser::parse_strict`, and enforce the
/// single-root rule.
///
/// In **strict mode** (default), a failure returns `Err` with
/// a `compile_error!` `TokenStream` whose message is a pre-
/// rendered `annotate-snippets` block pointing at the offending
/// line. The caller replaces its expansion output with this
/// `TokenStream`, terminating compilation for the component.
///
/// In **lenient mode** (`POCOPINE_TEMPLATES_LENIENT=1`), any
/// issue returns `Ok(Some(warning_tokens))`. The caller
/// appends the warnings to its normal expansion so the
/// component still registers; rustc surfaces the warning via
/// a `#[deprecated]`-attribute trick (RFC 045 §9.4).
///
/// Returns `Ok(None)` when the template is clean.
/// Source for `validate_template_or_emit_errors` — either a path
/// to read at expansion time (the `template = "Foo.poco"` shape)
/// or a pre-loaded HTML string (the `template_inline = "..."`
/// shape, intended for test fixtures).
enum TemplateSource<'a> {
    File(&'a LitStr),
    Inline { source: String, anchor: &'a LitStr },
}

fn validate_template_or_emit_errors(
    template_source: TemplateSource<'_>,
    component_name: &str,
) -> Result<
    (
        Option<proc_macro2::TokenStream>,
        Option<template_parser::TemplateAst>,
    ),
    TokenStream,
> {
    let lenient = is_lenient_mode();
    match template_source {
        TemplateSource::Inline { source, anchor } => {
            validate_inline_source(&source, anchor, component_name, lenient)
        }
        TemplateSource::File(path) => validate_file_template(path, component_name, lenient),
    }
}

fn validate_inline_source(
    source: &str,
    anchor: &LitStr,
    component_name: &str,
    lenient: bool,
) -> Result<
    (
        Option<proc_macro2::TokenStream>,
        Option<template_parser::TemplateAst>,
    ),
    TokenStream,
> {
    let display_path = format!("<inline template for {component_name}>");
    let ast = match template_parser::parse_strict(source, &display_path) {
        Ok(ast) => ast,
        Err(parse_errors) => {
            let mut rendered_blocks: Vec<String> = Vec::new();
            for err in parse_errors {
                if err.byte_range.end > err.byte_range.start && err.byte_range.end <= source.len() {
                    rendered_blocks.push(diagnostics::render_template_error(
                        source,
                        &display_path,
                        err.byte_range,
                        &format!(
                            "pocopine: invalid inline template for component `{component_name}` — {}",
                            err.message
                        ),
                        &err.message,
                    ));
                } else {
                    rendered_blocks.push(diagnostics::render_fileless_error(
                        &display_path,
                        &format!(
                            "pocopine: invalid inline template for component `{component_name}`"
                        ),
                        &err.message,
                    ));
                }
            }
            return surface_diagnostic(anchor, &rendered_blocks.join("\n\n"), lenient, None);
        }
    };
    let roots: Vec<_> = ast.element_roots().collect();
    match roots.len() {
        1 => Ok((None, Some(ast))),
        0 => {
            let msg = diagnostics::render_fileless_error(
                &display_path,
                &format!(
                    "pocopine: inline template for component `{component_name}` has no root element"
                ),
                "pocopine templates require exactly one root",
            );
            drop(roots);
            surface_diagnostic(anchor, &msg, lenient, Some(ast))
        }
        _ => {
            let second_range = roots[1].opening_tag_range.clone();
            let msg = diagnostics::render_template_error(
                source,
                &display_path,
                second_range,
                &format!(
                    "pocopine: inline template for component `{component_name}` has more than one root element"
                ),
                "additional root — drops at runtime",
            );
            drop(roots);
            surface_diagnostic(anchor, &msg, lenient, Some(ast))
        }
    }
}

fn validate_file_template(
    template_path: &LitStr,
    component_name: &str,
    lenient: bool,
) -> Result<
    (
        Option<proc_macro2::TokenStream>,
        Option<template_parser::TemplateAst>,
    ),
    TokenStream,
> {
    // Resolve the `.poco` file via a two-tier strategy.
    //
    // Tier 1: `Span::local_file()` on nightly's
    // `proc_macro_span`. The primary path — works in cargo
    // builds and rust-analyzer's file-backed evaluations.
    //
    // Tier 2: walk the manifest dir looking for the template
    // filename. rust-analyzer runs speculative proc-macro
    // expansions (hover, completion, inlay hints) where
    // `local_file()` returns `None`; the filesystem walk lets
    // validation still run against the authored file.
    //
    // Tier 3: keep the component buildable, but warn. The generated
    // `include_str!` may still resolve the file even when proc-macro span
    // provenance was unavailable; in that case raw HTML would otherwise be
    // registered with an empty mount function and every directive would be
    // inert without explanation.
    let resolved_path = resolve_template_path(template_path);
    let resolved_path = match resolved_path {
        Some(p) => p,
        None => {
            return Ok((
                Some(unresolved_template_warning_tokens(
                    &template_path.value(),
                    component_name,
                )),
                None,
            ));
        }
    };
    let file_path_str = resolved_path.to_string_lossy().to_string();

    // Display path: relative to CARGO_MANIFEST_DIR if we can
    // compute it, else the raw resolved path. This is what
    // shows up in the `--> path:line:col` line of the
    // rendered error — nicer than an absolute /home/... path.
    let display_path = manifest_relative(&resolved_path);

    let source = match std::fs::read_to_string(&resolved_path) {
        Ok(s) => s,
        Err(e) => {
            let msg = diagnostics::render_fileless_error(
                &display_path,
                &format!("pocopine: cannot read template for component `{component_name}`"),
                &format!("{} ({})", e, file_path_str),
            );
            return surface_diagnostic(template_path, &msg, lenient, None);
        }
    };

    let ast = match template_parser::parse_strict(&source, &display_path) {
        Ok(ast) => ast,
        Err(parse_errors) => {
            let mut rendered_blocks: Vec<String> = Vec::new();
            for err in parse_errors {
                if err.byte_range.end > err.byte_range.start && err.byte_range.end <= source.len() {
                    rendered_blocks.push(diagnostics::render_template_error(
                        &source,
                        &display_path,
                        err.byte_range,
                        &format!(
                            "pocopine: invalid template for component `{component_name}` — {}",
                            err.message
                        ),
                        &err.message,
                    ));
                } else {
                    rendered_blocks.push(diagnostics::render_fileless_error(
                        &display_path,
                        &format!("pocopine: invalid template for component `{component_name}`"),
                        &err.message,
                    ));
                }
            }
            let combined = rendered_blocks.join("\n\n");
            return surface_diagnostic(template_path, &combined, lenient, None);
        }
    };

    // Single-root rule — canonical form uses element_roots()
    // per RFC 045 §8 migration note, which filters out text /
    // comments / synthetic elements / foster-parented content.
    let roots: Vec<_> = ast.element_roots().collect();
    match roots.len() {
        1 => Ok((None, Some(ast))),
        0 => {
            let msg = diagnostics::render_fileless_error(
                &display_path,
                &format!("pocopine: template for component `{component_name}` has no root element"),
                "pocopine templates require exactly one root",
            );
            drop(roots);
            surface_diagnostic(template_path, &msg, lenient, Some(ast))
        }
        _ => {
            // Anchor the error at the SECOND root — that's the
            // offending one from the author's perspective.
            let second_range = roots[1].opening_tag_range.clone();
            let msg = diagnostics::render_template_error(
                &source,
                &display_path,
                second_range,
                &format!(
                    "pocopine: template for component `{component_name}` has more than one root element"
                ),
                "additional root — drops at runtime",
            );
            drop(roots);
            surface_diagnostic(template_path, &msg, lenient, Some(ast))
        }
    }
}

/// Package a rendered diagnostic. Strict → `Err(fatal tokens)`
/// replaces the expansion output. Lenient → `Ok(Some(warning
/// tokens))` gets appended to the normal output so the
/// component still registers.
///
/// `ast` carries the parsed template (if any) through for
/// downstream RFC 049 slot-contract scanning — a file-read or
/// pre-parse failure sets this to `None`.
fn surface_diagnostic(
    anchor: &LitStr,
    rendered: &str,
    lenient: bool,
    ast: Option<template_parser::TemplateAst>,
) -> Result<
    (
        Option<proc_macro2::TokenStream>,
        Option<template_parser::TemplateAst>,
    ),
    TokenStream,
> {
    if lenient {
        Ok((Some(build_warning_tokens(rendered)), ast))
    } else {
        Err(syn::Error::new(anchor.span(), rendered)
            .to_compile_error()
            .into())
    }
}

/// Build a `#[deprecated]`-decorated `const _` item that
/// surfaces `rendered` as a rustc warning. Per RFC 045 §9.4.
/// rustc emits `warning: use of deprecated const '<ident>':
/// <rendered>` when the emitted `let _ = <ident>;` is
/// evaluated.
fn build_warning_tokens(rendered: &str) -> proc_macro2::TokenStream {
    let ident = format_ident!("__pocopine_template_warning_{:x}", fxhash(rendered));
    quote! {
        const _: () = {
            #[deprecated(note = #rendered)]
            #[allow(non_upper_case_globals)]
            const #ident: () = ();
            // Use the const so rustc's deprecated-use lint fires.
            let _ = #ident;
        };
    }
}

fn unresolved_template_warning_tokens(
    template_path: &str,
    component_name: &str,
) -> proc_macro2::TokenStream {
    build_warning_tokens(&format!(
        "pocopine: template plan compilation was skipped for component \
         `{component_name}` because `{template_path}` could not be resolved uniquely during \
         macro expansion; directives will be inert. Use a template path relative to the \
         component module and ensure it resolves uniquely so \
         the planner can validate and compile it."
    ))
}

fn vtable_template_html_tokens(
    compiled_html: &proc_macro2::Literal,
    planner_saw_template: bool,
) -> proc_macro2::TokenStream {
    if planner_saw_template {
        quote! { Some(#compiled_html) }
    } else {
        // `register()` still stores the include_str!-backed template in the
        // thread-local registry. Leaving the PHF payload absent lets runtime
        // lookup fall through to that real source instead of shadowing it
        // with the empty string produced when macro-time resolution failed.
        quote! { None }
    }
}

/// RFC 045 §9 — the `POCOPINE_TEMPLATES_LENIENT` env-var
/// escape hatch. Truthy values: `1`, `true`, `yes` (case-
/// insensitive). Anything else is strict mode.
fn is_lenient_mode() -> bool {
    match std::env::var("POCOPINE_TEMPLATES_LENIENT") {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => false,
    }
}

/// Compute a display path relative to `CARGO_MANIFEST_DIR`
/// when we can; fall back to the raw absolute path otherwise.
fn manifest_relative(path: &std::path::Path) -> String {
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR")
        && let Ok(rel) = path.strip_prefix(&manifest_dir)
    {
        return rel.to_string_lossy().to_string();
    }
    path.to_string_lossy().to_string()
}

/// Two-tier template-path resolution.
///
/// * **Tier 1** — `Span::local_file()`. When the calling
///   `.rs` file is known to the compiler (real cargo build,
///   rust-analyzer's file-backed evaluations), join that
///   `.rs` parent directory with the authored `template = "…"`
///   path. This is the canonical resolution — matches
///   `include_str!`'s own semantics.
///
/// * **Tier 2** — manifest-dir filesystem walk. When the span
///   is synthetic (rust-analyzer speculative expansion,
///   deeply-nested macros) `local_file()` returns None. We
///   fall back by walking `CARGO_MANIFEST_DIR` looking for
///   the template filename. An unambiguous match is used as
///   the resolved path; zero or >1 matches → give up.
///
/// Returns `None` when neither tier finds an existing file. The caller keeps
/// the component buildable, but emits a warning that its directives are inert.
fn resolve_template_path(template_path: &LitStr) -> Option<std::path::PathBuf> {
    // Tier 1 — span-based (cargo + rust-analyzer file-backed).
    if let Some(caller_rs) = template_path.span().unwrap().local_file() {
        let caller_dir = caller_rs
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let candidate = caller_dir.join(template_path.value());
        if candidate.is_file() {
            return Some(candidate);
        }
        // Fall through to tier 2 if the span-derived path
        // doesn't exist — handles edge cases like symlinked
        // workspaces or IDE caching artefacts.
    }

    // Tier 2 — manifest-dir walk. Invoked when the span
    // didn't yield a usable path (rust-analyzer speculative
    // expansion is the common case).
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let template_value = template_path.value();

    // Fast path: if the template path is itself
    // manifest-relative (starts with a known prefix or is
    // directly findable under the manifest), join and check.
    let direct = std::path::Path::new(&manifest_dir).join(&template_value);
    if direct.is_file() {
        return Some(direct);
    }

    // Slower path: recursive search for the basename. Only
    // bite this cost when the direct join missed. Limits the
    // walk to conservative depth and skips obviously-unrelated
    // directories (target, node_modules, .git) so we don't
    // scan the whole user's disk.
    let basename = std::path::Path::new(&template_value).file_name()?;
    let mut matches: Vec<std::path::PathBuf> = Vec::new();
    find_template_in(
        std::path::Path::new(&manifest_dir),
        basename,
        0,
        &mut matches,
    );

    if matches.len() == 1 {
        Some(matches.into_iter().next().unwrap())
    } else {
        // Zero matches → file doesn't exist. More than one match →
        // ambiguous; refuse to guess. The caller warns in either case.
        None
    }
}

/// Recursive filename search, depth-limited and
/// skip-listed so it terminates quickly on large workspaces.
fn find_template_in(
    dir: &std::path::Path,
    basename: &std::ffi::OsStr,
    depth: usize,
    out: &mut Vec<std::path::PathBuf>,
) {
    // Depth cap: pocopine templates live within a few layers
    // of `src/`. Eight is generous.
    if depth > 8 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip build-output and dependency directories.
        if matches!(
            name_str.as_ref(),
            "target" | "node_modules" | ".git" | "pkg" | "dist" | ".idea" | ".vscode"
        ) {
            continue;
        }
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {
                find_template_in(&path, basename, depth + 1, out);
                if out.len() > 1 {
                    return; // short-circuit on ambiguity
                }
            }
            Ok(ft) if ft.is_file() => {
                if entry.file_name() == basename {
                    out.push(path);
                    if out.len() > 1 {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Tiny FNV-1a over the rendered message — just enough
/// variability so two warnings don't emit colliding idents.
/// Not cryptographic; not stable across platforms.
fn fxhash(s: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Parsed `#[observe(KEY [, field = "name"])]` attribute —
/// RFC-036. Each entry emits a `watch_scope_field` install that
/// writes back into `field_ident` whenever the parent's
/// `field_name_on_root` changes, plus a seed read during setup.
struct ObserveEntry {
    field_ident: syn::Ident,
    field_ty: Type,
    /// Name of the field on the injected root. Defaults to
    /// `field_ident.to_string()` when `field = "..."` was absent.
    field_name_on_root: String,
    /// Path to the `InjectKey` used to resolve the root — matches
    /// what the author passed as `via = …`.
    key_path: Path,
}

struct ComputedMethod {
    method_ident: syn::Ident,
    field_name: String,
    ret_ty: Type,
    params: Vec<ComputedParam>,
}

struct ComputedParam {
    ident: syn::Ident,
    ty: Type,
    is_computed_dep: bool,
}

/// HTML Living Standard element names. A struct whose kebab-case ident
/// matches one of these is rejected — its custom-element tag would
/// collide with real HTML markup in parent templates.
///
/// `pub(crate)` so RFC-058 Phase 2's `template_plan` classifier can
/// consult the same list when deciding "is this tag native?" — the
/// canonical eligibility gate (see RFC-057 §6 / RFC-058 §6.2).
pub(crate) const HTML5_ELEMENTS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "area",
    "article",
    "aside",
    "audio",
    "b",
    "base",
    "bdi",
    "bdo",
    "blockquote",
    "body",
    "br",
    "button",
    "canvas",
    "caption",
    "cite",
    "code",
    "col",
    "colgroup",
    "data",
    "datalist",
    "dd",
    "del",
    "details",
    "dfn",
    "dialog",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "i",
    "iframe",
    "img",
    "input",
    "ins",
    "kbd",
    "label",
    "legend",
    "li",
    "link",
    "main",
    "map",
    "mark",
    "math",
    "menu",
    "meta",
    "meter",
    "nav",
    "noscript",
    "object",
    "ol",
    "optgroup",
    "option",
    "output",
    "p",
    "picture",
    "pre",
    "progress",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "script",
    "search",
    "section",
    "select",
    "slot",
    "small",
    "source",
    "span",
    "strong",
    "style",
    "sub",
    "summary",
    "sup",
    "svg",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "time",
    "title",
    "tr",
    "track",
    "u",
    "ul",
    "var",
    "video",
    "wbr",
];

#[derive(Default)]
struct ComponentArgs {
    name: Option<LitStr>,
    template: Option<LitStr>,
    /// Inline-template alternative to `template = "Foo.poco"`.
    /// When set, the literal is used directly as the template
    /// HTML source — no file resolution, no `include_str!`
    /// rebuild dependency. Mutually exclusive with `template`.
    /// Intended for test-only fixtures where authoring a
    /// per-test `.html` file is overhead the assertion doesn't
    /// justify; production components should still use the file
    /// path so the template lives in its own source-of-truth.
    template_inline: Option<LitStr>,
    style: Option<LitStr>,
    role: Option<LitStr>,
    /// Force a specific CSS `display` value on the OUTER custom
    /// tag. Custom elements default to `display: inline`, which
    /// wraps a block-level rendered root in an inline line-box
    /// and breaks flex / grid parent-child layout whenever
    /// compound primitives nest. `display = "contents"` elides
    /// the custom tag's layout box so the inner root participates
    /// in its parent's layout directly. Any valid CSS `display`
    /// value works — `"block"`, `"inline-block"`, `"grid"`, etc.
    /// — the macro emits `<custom-tag> { display: <value> }` at
    /// registration time. Events, scope, and a11y are unaffected
    /// by the display choice.
    display: Option<LitStr>,
    /// RFC-038 — symmetric enter/leave preset name. Default preset
    /// the primitive animates with; authors override per-instance via
    /// the `transition` HTML attribute.
    transition: Option<LitStr>,
    /// RFC-038 — asymmetric enter-only preset. Wins over `transition`
    /// for the enter phase when both are set.
    transition_in: Option<LitStr>,
    /// RFC-038 — asymmetric leave-only preset. Wins over `transition`
    /// for the leave phase when both are set.
    transition_out: Option<LitStr>,
    /// RFC-038 — keyed-pp-for layout animation. Currently only
    /// `"flip"` is recognised; any other value is a no-op so the arg
    /// is forwards-compatible with future modes (slide, stagger, …).
    animate: Option<LitStr>,
    /// RFC 049 — resolved `tag → TypePath` table built from
    /// `uses = [...]`. `None` when the consumer didn't opt in;
    /// the consumer-side scan skips validation entirely in that
    /// case. Empty table means "declared but zero entries" —
    /// still opted-in for future use but no tags resolve.
    uses: Option<uses::UsesTable>,
    /// RFC 060 Tier 3 — `extends = [...]` list. Marks this
    /// component as a bundle: a tag-less type marker that
    /// re-exports the registration of every type in the list
    /// via its own `register()`. Mutually exclusive with
    /// `template` / `template_inline`.
    extends: Option<Vec<syn::Path>>,
    /// Escape hatch for compile-time template-path validation
    /// (`unchecked_paths = "true"`). Skips the marker-reference
    /// emission entirely; template root typos fall back to the
    /// runtime warn path.
    unchecked_paths: bool,
}

impl Parse for ComponentArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pairs: Punctuated<MetaNameValue, Token![,]> = Punctuated::parse_terminated(input)?;
        let mut args = ComponentArgs::default();
        for kv in pairs {
            // `uses = [...]` is the one non-string-valued key;
            // handle it first so the string-lit extraction below
            // doesn't reject it.
            if kv.path.is_ident("uses") {
                let entries = uses::parse_uses_array(kv.value)?;
                let table = uses::resolve_uses(entries)?;
                args.uses = Some(table);
                continue;
            }
            if kv.path.is_ident("extends") {
                args.extends = Some(uses::parse_extends_array(kv.value)?);
                continue;
            }

            let lit = match kv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) => s,
                other => {
                    return Err(syn::Error::new_spanned(other, "expected a string literal"));
                }
            };
            if kv.path.is_ident("name") {
                args.name = Some(lit);
            } else if kv.path.is_ident("template") {
                args.template = Some(lit);
            } else if kv.path.is_ident("template_inline") {
                args.template_inline = Some(lit);
            } else if kv.path.is_ident("style") {
                args.style = Some(lit);
            } else if kv.path.is_ident("role") {
                args.role = Some(lit);
            } else if kv.path.is_ident("display") {
                args.display = Some(lit);
            } else if kv.path.is_ident("transition") {
                args.transition = Some(lit);
            } else if kv.path.is_ident("transition_in") {
                args.transition_in = Some(lit);
            } else if kv.path.is_ident("transition_out") {
                args.transition_out = Some(lit);
            } else if kv.path.is_ident("animate") {
                args.animate = Some(lit);
            } else if kv.path.is_ident("unchecked_paths") {
                args.unchecked_paths = lit.value() == "true";
            } else {
                return Err(syn::Error::new_spanned(
                    kv.path,
                    "unknown key — expected one of: name, template, template_inline, \
                     style, role, display, transition, transition_in, transition_out, \
                     animate, uses, extends, unchecked_paths",
                ));
            }
        }
        Ok(args)
    }
}

/// RFC-033 — canonical primitive-role → default-element map.
/// A role picks the semantically-correct root tag for a primitive
/// template (mirrors Reka UI's Primitive convention) and emits a
/// `data-pine-role="<role>"` CSS hook on the root. Templates with
/// a role must use the placeholder `<root>...</root>` pair for
/// their root element; the registrar rewrites it at compile time.
fn role_to_tag(role: &str) -> Option<&'static str> {
    match role {
        "visual" => Some("span"),
        "interactive" => Some("button"),
        "link" => Some("a"),
        "media" => Some("img"),
        "panel" => Some("div"),
        "scope" => Some("div"),
        "surface" => Some("div"),
        "heading" => Some("h2"),
        "text" => Some("p"),
        "list" => Some("ul"),
        "item" => Some("li"),
        "label" => Some("label"),
        _ => None,
    }
}

/// Default `display` value for a role when the author didn't set
/// `display = "..."` explicitly. Layout-bearing roles (`panel`,
/// `scope`) default to `contents` so the custom-element wrapper
/// elides its own layout box — without this, the inner `<root>`'s
/// flex / grid / sticky positioning binds to a `display: inline`
/// custom element and silently breaks. Other roles keep the
/// browser's custom-element default (`inline`) until the author
/// overrides.
fn role_default_display(role: &str) -> Option<&'static str> {
    match role {
        "panel" | "scope" => Some("contents"),
        _ => None,
    }
}

/// Apply RFC 054 row-plan stamps AND RFC 084 slot template edits
/// to the original template source **in a single sorted pass**.
///
/// Both edit sources record `insert_at` byte offsets relative to
/// the un-mutated `ast.source`. Applying them sequentially (stamps
/// first, then slot edits) would let stamps shift the byte positions
/// the slot edits assume, breaking the slot's auto-publish
/// injection silently when the template contains both
/// `<template pp-for>` row plans AND iterated-mode typed slots.
/// Combining the edits and sorting by descending `insert_at` keeps
/// every offset valid throughout the pass.
fn apply_combined_template_edits(
    source: &str,
    stamps: &[for_plan::TemplateStamp],
    slot_edits: &[slot::TemplateEdit],
) -> String {
    if stamps.is_empty() && slot_edits.is_empty() {
        return source.to_string();
    }
    let mut all: Vec<slot::TemplateEdit> = stamps
        .iter()
        .map(|stamp| slot::TemplateEdit {
            insert_at: stamp.insert_at,
            text: format!(" data-pp-row-plan=\"{}\"", stamp.plan_id),
        })
        .collect();
    all.extend(slot_edits.iter().cloned());
    slot::apply_template_edits(source, &all)
}

fn compile_template_static(raw: &str, name: &str, role: Option<(&str, &str)>) -> String {
    let Some((tag, role_name)) = role else {
        return inject_pp_data_static(raw, name);
    };
    let mut prefix = format!(r#"data-pine-role="{role_name}""#);
    if tag == "button" && !root_placeholder_has_attr_static(raw, "type") {
        prefix.push_str(r#" type="button""#);
    }
    let renamed = rewrite_root_placeholder_static(raw, tag, &prefix);
    inject_pp_data_static(&renamed, name)
}

fn rewrite_root_placeholder_static(raw: &str, tag: &str, prefix_attrs: &str) -> String {
    let attrs = prefix_attrs.trim();
    let step1 = raw.replace("<root>", &format!("<{tag} {attrs}>"));
    let step2 = step1.replace("<root ", &format!("<{tag} {attrs} "));
    let step3 = step2.replace("<root/>", &format!("<{tag} {attrs}/>"));
    step3.replace("</root>", &format!("</{tag}>"))
}

fn root_placeholder_has_attr_static(raw: &str, needle: &str) -> bool {
    let Some(pos) = raw.find("<root") else {
        return false;
    };
    let after = pos + "<root".len();
    let boundary = raw.as_bytes().get(after).copied();
    if !matches!(
        boundary,
        Some(b' ') | Some(b'>') | Some(b'/') | Some(b'\n') | Some(b'\t') | Some(b'\r')
    ) {
        return false;
    }
    let bytes = raw.as_bytes();
    let Some(close) = find_tag_end_static(bytes, pos) else {
        return false;
    };
    let tag_slice = &raw[pos + 1..close];
    for chunk in tag_slice.split_ascii_whitespace().skip(1) {
        let name_end = chunk.find('=').unwrap_or(chunk.len());
        if chunk[..name_end].eq_ignore_ascii_case(needle) {
            return true;
        }
    }
    false
}

fn inject_pp_data_static(raw: &str, name: &str) -> String {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        if bytes[i] != b'<' {
            return raw.to_owned();
        }
        if i + 4 <= len && &bytes[i..i + 4] == b"<!--" {
            if let Some(end) = find_seq_static(bytes, i + 4, b"-->") {
                i = end + 3;
                continue;
            }
            return raw.to_owned();
        }
        if i + 2 <= len && bytes[i + 1] == b'!' {
            if let Some(end) = find_byte_static(bytes, i, b'>') {
                i = end + 1;
                continue;
            }
            return raw.to_owned();
        }
        if i + 2 <= len && bytes[i + 1] == b'?' {
            if let Some(end) = find_seq_static(bytes, i + 2, b"?>") {
                i = end + 2;
                continue;
            }
            return raw.to_owned();
        }
        let Some(close) = find_tag_end_static(bytes, i) else {
            return raw.to_owned();
        };
        let self_closing = close > 0 && bytes[close - 1] == b'/';
        let insert_at = if self_closing { close - 1 } else { close };
        let attr = format!(" data-pp-scope-id=\"{name}\"");
        let mut out = String::with_capacity(raw.len() + attr.len());
        out.push_str(&raw[..insert_at]);
        if !out.ends_with(char::is_whitespace) {
            out.push(' ');
        }
        out.push_str(attr.trim_start());
        out.push_str(&raw[insert_at..]);
        return out;
    }
    raw.to_owned()
}

fn find_byte_static(bytes: &[u8], start: usize, needle: u8) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|&b| b == needle)
        .map(|p| start + p)
}

fn find_seq_static(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || start + needle.len() > bytes.len() {
        return None;
    }
    (start..=bytes.len() - needle.len()).find(|&i| &bytes[i..i + needle.len()] == needle)
}

fn find_tag_end_static(bytes: &[u8], tag_start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = tag_start + 1;
    let mut quote: Option<u8> = None;
    while i < len {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(i),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Kebab-case an ident: `TodoItem` → `todo-item`.
pub(crate) fn kebab_case(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    for (i, c) in ident.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Build `#name => #call(&self.#id),` match arms — one per direct
/// field. Shared by `#[component]` and `#[store]` for the per-field
/// projection lanes (fingerprint, quick-len, text) that each wrap a
/// single `::pocopine::__private` call around the field reference, so
/// the two macros can't drift on the arm shape.
fn field_call_arms<'a>(
    field_idents: &'a [syn::Ident],
    field_names: &'a [String],
    call: TokenStream2,
) -> impl Iterator<Item = TokenStream2> + 'a {
    field_idents
        .iter()
        .zip(field_names.iter())
        .map(move |(id, name)| {
            quote! {
                #name => #call(&self.#id),
            }
        })
}

/// RFC 081 — convert a `pp-ref` name to a valid Rust method
/// identifier for the generated `<ComponentName>Refs` struct.
/// Kebab/dot/colon to underscore, leading non-alpha to a
/// prefixed underscore. Names that already look like snake_case
/// (e.g. `form_root`, `title_input`) pass through unchanged.
pub(crate) fn normalize_ref_method_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 1);
    for (i, c) in name.chars().enumerate() {
        let mapped = match c {
            'a'..='z' | 'A'..='Z' | '_' => c,
            '0'..='9' if i > 0 => c,
            _ => '_',
        };
        out.push(mapped);
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

struct ClientModuleArgs {
    file: Option<LitStr>,
    name: Option<LitStr>,
}

impl Parse for ClientModuleArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self {
                file: None,
                name: None,
            });
        }

        if input.peek(LitStr) {
            let file = input.parse()?;
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
                if !input.is_empty() {
                    return Err(input.error(
                        "expected only one string literal, or use `file = \"...\"` / `name = \"...\"`",
                    ));
                }
            }
            return Ok(Self {
                file: Some(file),
                name: None,
            });
        }

        let entries = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        let mut args = Self {
            file: None,
            name: None,
        };
        for entry in entries {
            let Some(ident) = entry.path.get_ident() else {
                return Err(syn::Error::new_spanned(
                    entry.path,
                    "expected `file = \"...\"` or `name = \"...\"`",
                ));
            };
            let Expr::Lit(ExprLit {
                lit: Lit::Str(value),
                ..
            }) = entry.value
            else {
                return Err(syn::Error::new_spanned(
                    entry.value,
                    "client module arguments must be string literals",
                ));
            };
            match ident.to_string().as_str() {
                "file" => {
                    if args.file.replace(value).is_some() {
                        return Err(syn::Error::new_spanned(ident, "duplicate `file` argument"));
                    }
                }
                "name" => {
                    if args.name.replace(value).is_some() {
                        return Err(syn::Error::new_spanned(ident, "duplicate `name` argument"));
                    }
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        ident,
                        "expected `file = \"...\"` or `name = \"...\"`",
                    ));
                }
            }
        }
        Ok(args)
    }
}

fn client_module_name_from_file(file: &LitStr) -> syn::Result<String> {
    let value = file.value();
    let file_name = value.rsplit('/').next().unwrap_or(&value);
    if !file_name.ends_with(".client.ts") {
        return Err(syn::Error::new_spanned(
            file,
            "`#[client_module(file = ...)]` expects a `.client.ts` file",
        ));
    }
    pocopine_client_codegen::client_module_name(file_name)
        .map_err(|err| syn::Error::new_spanned(file, err.to_string()))
}

fn client_module_methods_from_file(
    file: Option<&LitStr>,
) -> syn::Result<Vec<pocopine_client_codegen::ClientFacadeMethod>> {
    let Some(file) = file else {
        return Ok(Vec::new());
    };
    let Some(path) = resolve_template_path(file) else {
        return Err(syn::Error::new_spanned(
            file,
            format!(
                "client module file `{}` could not be found relative to this module or CARGO_MANIFEST_DIR",
                file.value()
            ),
        ));
    };
    let source = std::fs::read_to_string(&path).map_err(|err| {
        syn::Error::new_spanned(
            file,
            format!(
                "failed to read client module `{}`: {err}",
                manifest_relative(&path)
            ),
        )
    })?;
    pocopine_client_codegen::extract_client_facade_methods(&source, &path).map_err(|err| {
        syn::Error::new_spanned(
            file,
            format!(
                "failed to parse client module `{}`: {err}",
                manifest_relative(&path)
            ),
        )
    })
}

fn client_module_rebuild_pin_from_file(file: Option<&LitStr>) -> TokenStream2 {
    let Some(file) = file else {
        return quote! {};
    };
    let Some(path) = resolve_template_path(file) else {
        return quote! {};
    };
    let path = path.canonicalize().unwrap_or(path);
    let path = LitStr::new(&path.to_string_lossy(), Span::call_site());
    quote! {
        const _: &str = include_str!(#path);
    }
}

fn client_module_method_tokens(
    methods: &[pocopine_client_codegen::ClientFacadeMethod],
) -> syn::Result<TokenStream2> {
    let mut tokens = Vec::new();
    for method in methods {
        let rust_name = format_ident!("{}", method.rust_name);
        let js_name = LitStr::new(&method.name, Span::call_site());
        let output: Type = syn::parse_str(&method.output).map_err(|err| {
            let label = match method.kind {
                pocopine_client_codegen::ClientFacadeMethodKind::Async => "return",
                pocopine_client_codegen::ClientFacadeMethodKind::Subscription => "callback",
            };
            syn::Error::new(
                Span::call_site(),
                format!(
                    "unsupported client module {label} type `{}`: {err}",
                    method.output
                ),
            )
        })?;
        match method.kind {
            pocopine_client_codegen::ClientFacadeMethodKind::Async => {
                tokens.push(quote! {
                    pub async fn #rust_name(&self) -> Result<#output, Error> {
                        self.call_async(#js_name).await
                    }
                });
            }
            pocopine_client_codegen::ClientFacadeMethodKind::Subscription => {
                tokens.push(quote! {
                    pub fn #rust_name(
                        &self,
                        scope: ::pocopine::ScopeId,
                        handler: impl FnMut(Result<#output, Error>) + 'static,
                    ) -> Result<(), Error> {
                        self.subscribe(scope, #js_name, handler)
                    }
                });
            }
        }
    }
    Ok(quote! { #(#tokens)* })
}

#[cfg(test)]
mod client_module_tests {
    use super::*;

    #[test]
    fn generates_facade_tokens_from_codegen_methods() {
        let methods = vec![
            pocopine_client_codegen::ClientFacadeMethod {
                name: "signIn".to_string(),
                rust_name: "sign_in".to_string(),
                kind: pocopine_client_codegen::ClientFacadeMethodKind::Async,
                output: "Option<FirebaseAuthUser>".to_string(),
            },
            pocopine_client_codegen::ClientFacadeMethod {
                name: "onAuthStateChanged".to_string(),
                rust_name: "on_auth_state_changed".to_string(),
                kind: pocopine_client_codegen::ClientFacadeMethodKind::Subscription,
                output: "Option<FirebaseAuthUser>".to_string(),
            },
        ];

        let tokens = client_module_method_tokens(&methods).unwrap().to_string();
        assert!(tokens.contains("pub async fn sign_in"));
        assert!(tokens.contains("self . call_async (\"signIn\")"));
        assert!(tokens.contains("pub fn on_auth_state_changed"));
        assert!(tokens.contains("self . subscribe (scope , \"onAuthStateChanged\""));
    }

    #[test]
    fn client_module_names_use_codegen_rules() {
        let file = LitStr::new("OAuth.client.ts", Span::call_site());
        assert_eq!(client_module_name_from_file(&file).unwrap(), "oauth");

        let file = LitStr::new("my_thing.client.ts", Span::call_site());
        assert_eq!(client_module_name_from_file(&file).unwrap(), "my-thing");
    }
}

#[cfg(test)]
mod template_validation_tests {
    use super::*;

    #[test]
    fn unresolved_template_warns_that_plan_compilation_was_skipped() {
        let rendered = unresolved_template_warning_tokens(
            "__pocopine_test_missing_template_7f30b0.poco",
            "missing-template-fixture",
        )
        .to_string();
        assert!(rendered.contains("missing-template-fixture"), "{rendered}");
        assert!(
            rendered.contains("plan compilation was skipped"),
            "{rendered}"
        );
        assert!(rendered.contains("directives will be inert"), "{rendered}");
    }

    #[test]
    fn unavailable_planner_source_does_not_shadow_runtime_template_with_empty_phf_html() {
        let empty = proc_macro2::Literal::string("");
        assert_eq!(
            vtable_template_html_tokens(&empty, false).to_string(),
            "None"
        );

        let compiled = proc_macro2::Literal::string("<div>ready</div>");
        let tokens = vtable_template_html_tokens(&compiled, true).to_string();
        assert!(tokens.contains("Some"), "{tokens}");
        assert!(tokens.contains("ready"), "{tokens}");
    }
}

/// `#[client_module]` — declare a Rust facade for a typed `.client.ts`
/// module bundled by the Pocopine CLI.
///
/// ```ignore
/// #[pocopine::client_module("Firebase.client.ts")]
/// pub mod firebase {
///     use crate::User;
/// }
/// ```
#[proc_macro_attribute]
pub fn client_module(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match ClientModuleArgs::parse.parse(attr) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemMod);

    let attrs = input.attrs;
    let vis = input.vis;
    let ident = input.ident;
    let user_items = input.content.map(|(_, items)| items).unwrap_or_default();
    let file = args.file.clone();
    let client_module_rebuild_pin = client_module_rebuild_pin_from_file(file.as_ref());
    let generated_methods = match client_module_methods_from_file(file.as_ref()) {
        Ok(methods) => match client_module_method_tokens(&methods) {
            Ok(tokens) => tokens,
            Err(err) => return err.to_compile_error().into(),
        },
        Err(err) => return err.to_compile_error().into(),
    };
    let module_name = match args.name {
        Some(name) => name.value(),
        None => match args.file {
            Some(file) => match client_module_name_from_file(&file) {
                Ok(name) => name,
                Err(err) => return err.to_compile_error().into(),
            },
            None => kebab_case(&ident.to_string()),
        },
    };
    if module_name.trim().is_empty() {
        return syn::Error::new_spanned(
            &ident,
            "`#[client_module]` resolved to an empty module name",
        )
        .to_compile_error()
        .into();
    }

    let module_name_lit = LitStr::new(&module_name, proc_macro2::Span::call_site());
    quote! {
        #(#attrs)*
        #vis mod #ident {
            #client_module_rebuild_pin

            pub const NAME: &str = #module_name_lit;
            pub type Error = ::pocopine::__private::ClientModuleError;

            #[derive(Clone, Debug)]
            pub struct Module {
                inner: ::pocopine::__private::ClientModule,
            }

            pub fn required() -> Result<Module, Error> {
                ::pocopine::__private::ClientModule::required(NAME)
                    .map(|inner| Module { inner })
            }

            pub fn optional() -> Result<Option<Module>, Error> {
                ::pocopine::__private::ClientModule::optional(NAME)
                    .map(|module| module.map(|inner| Module { inner }))
            }

            impl Module {
                pub fn name(&self) -> &str {
                    self.inner.name()
                }

                #[doc(hidden)]
                pub fn raw(&self) -> &::pocopine::__private::ClientModule {
                    &self.inner
                }

                pub async fn call_async<T>(&self, method: impl AsRef<str>) -> Result<T, Error>
                where
                    T: ::serde::de::DeserializeOwned,
                {
                    self.inner.call_async(method).await
                }

                pub fn subscribe<T>(
                    &self,
                    scope: ::pocopine::ScopeId,
                    method: impl AsRef<str>,
                    handler: impl FnMut(Result<T, Error>) + 'static,
                ) -> Result<(), Error>
                where
                    T: ::serde::de::DeserializeOwned + 'static,
                {
                    self.inner.subscribe(scope, method, handler)
                }

                #generated_methods
            }

            #(#user_items)*
        }
    }
    .into()
}

#[derive(Clone, Copy)]
enum StaticPropKindCode {
    Auto,
    String,
    Bool,
    Number,
}

fn static_prop_kind_for_type(ty: &Type) -> proc_macro2::TokenStream {
    match classify_static_prop_type(ty) {
        StaticPropKindCode::Auto => quote! { ::pocopine::__private::StaticPropKind::Auto },
        StaticPropKindCode::String => quote! { ::pocopine::__private::StaticPropKind::String },
        StaticPropKindCode::Bool => quote! { ::pocopine::__private::StaticPropKind::Bool },
        StaticPropKindCode::Number => quote! { ::pocopine::__private::StaticPropKind::Number },
    }
}

fn classify_static_prop_type(ty: &Type) -> StaticPropKindCode {
    let Some(segment) = type_path_last_segment(ty) else {
        return StaticPropKindCode::Auto;
    };
    let ident = segment.ident.to_string();
    if ident == "Option" {
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && let Some(inner) = args.args.iter().find_map(|arg| match arg {
                syn::GenericArgument::Type(inner) => Some(inner),
                _ => None,
            })
        {
            return classify_static_prop_type(inner);
        }
        return StaticPropKindCode::Auto;
    }

    match ident.as_str() {
        "String" => StaticPropKindCode::String,
        "bool" => StaticPropKindCode::Bool,
        "f32" | "f64" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32"
        | "u64" | "u128" | "usize" => StaticPropKindCode::Number,
        _ => StaticPropKindCode::Auto,
    }
}

fn type_path_last_segment(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(path) if path.qself.is_none() => path.path.segments.last(),
        _ => None,
    }
}

/// RFC-097 §3.3 — type constructors that grant interior mutability. A
/// `#[component]`/`#[store]` field that *reaches* one of these (anywhere
/// in its type tree) can be mutated through `&self`, which would make
/// the `&self`-handlers-skip-the-sweep optimisation unsound, so the
/// macros reject them. (A type *alias* can still smuggle one past this
/// syntactic check — documented as forfeiting the guarantee, same
/// standing as `#[serde(skip)]`.)
const INTERIOR_MUT_TYPES: &[&str] = &["Cell", "RefCell", "Mutex", "RwLock", "UnsafeCell"];

/// Recursively search a type tree for an interior-mutability
/// constructor, returning its name. Walks generic arguments
/// (`Vec<Cell<_>>`, `Option<RefCell<_>>`, `Rc<RefCell<_>>`, …) and
/// tuple/array/slice/reference element types — NOT just the outermost
/// constructor — so a cell nested under any wrapper is still caught.
fn find_interior_mut(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(tp) if tp.qself.is_none() => {
            let seg = tp.path.segments.last()?;
            let name = seg.ident.to_string();
            if INTERIOR_MUT_TYPES.contains(&name.as_str()) {
                return Some(name);
            }
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                for arg in &args.args {
                    if let syn::GenericArgument::Type(inner) = arg
                        && let Some(found) = find_interior_mut(inner)
                    {
                        return Some(found);
                    }
                }
            }
            None
        }
        Type::Tuple(t) => t.elems.iter().find_map(find_interior_mut),
        Type::Array(a) => find_interior_mut(&a.elem),
        Type::Slice(s) => find_interior_mut(&s.elem),
        Type::Reference(r) => find_interior_mut(&r.elem),
        Type::Group(g) => find_interior_mut(&g.elem),
        Type::Paren(p) => find_interior_mut(&p.elem),
        _ => None,
    }
}

/// RFC-097 §3.3 — reject any field whose type reaches an interior-
/// mutability constructor. Returns `Some(compile_error tokens)` on the
/// first offending field. Checks EVERY field (a non-path field type
/// such as a tuple is skipped, not treated as end-of-scan).
fn interior_mut_rejection(
    field_idents: &[syn::Ident],
    field_types: &[Type],
) -> Option<TokenStream2> {
    for (ident, ty) in field_idents.iter().zip(field_types.iter()) {
        if let Some(name) = find_interior_mut(ty) {
            let msg = format!(
                "RFC-097: field `{ident}` reaches interior-mutability type `{name}`. Component/store \
                 fields must be plain value-semantics data — interior mutability would let a \
                 `&self` handler mutate state, breaking the no-sweep optimisation. Move the cell \
                 out of component state (e.g. a `thread_local!` keyed by `ScopeId`)."
            );
            return Some(syn::Error::new_spanned(ty, msg).to_compile_error());
        }
    }
    None
}

/// RFC-097 §3.2 — emit the field-projection surface for a component/
/// store:
///
/// * a `<Name>Fields` struct whose **public fields are the typed
///   [`FieldHandle`]s**, one per serde-visible field;
/// * a `<Name>FieldsExt` trait with a single `fn fields(&self) ->
///   <Name>Fields`, impl'd for `Handle<Name>`.
///
/// Reached as `this::<Name>().fields().<field>.set(v)`. Because the
/// handles live as fields on a *dedicated* struct (not methods on
/// `Handle`), accessor names share no namespace with `Handle`'s
/// inherent methods — a field named `update`/`with`/`scope_id` is
/// fine, so there is **no reserved-name list to maintain** (a proc
/// macro can't introspect `Handle`'s methods, so any such list would
/// have to be hand-maintained — the very thing we avoid). The only
/// name we add to `Handle` is `fields`, which we own and which can't
/// collide with a field accessor (different receiver type). Shared by
/// `#[component]` and `#[store]`.
fn field_handles_tokens(
    struct_ident: &syn::Ident,
    ident_str: &str,
    field_idents: &[syn::Ident],
    field_names: &[String],
    field_types: &[Type],
    field_is_serde_skip: &[bool],
) -> TokenStream2 {
    let fields_struct = proc_macro2::Ident::new(&format!("{ident_str}Fields"), struct_ident.span());
    let ext_trait = proc_macro2::Ident::new(&format!("{ident_str}FieldsExt"), struct_ident.span());
    let mut struct_fields: Vec<TokenStream2> = Vec::new();
    let mut inits: Vec<TokenStream2> = Vec::new();
    for (((ident, name), ty), skip) in field_idents
        .iter()
        .zip(field_names.iter())
        .zip(field_types.iter())
        .zip(field_is_serde_skip.iter())
    {
        if *skip {
            continue;
        }
        let name_lit = proc_macro2::Literal::string(name);
        let doc =
            format!("Handle to the `{name}` field — single-field reactive writes (no sweep).");
        struct_fields.push(quote! {
            #[doc = #doc]
            pub #ident: ::pocopine::__private::FieldHandle<#ty>,
        });
        inits.push(quote! {
            #ident: ::pocopine::__private::FieldHandle::__new(__poc_sid, #name_lit),
        });
    }
    let struct_doc = format!(
        "RFC-097 field handles for [`{ident_str}`]. Obtain via \
         `this::<{ident_str}>().fields()` (or `store::<{ident_str}>().fields()`); each public \
         field is a `FieldHandle<_>` for single-field reactive writes from async tasks."
    );
    quote! {
        #[doc = #struct_doc]
        #[derive(Clone, Copy)]
        pub struct #fields_struct {
            #(#struct_fields)*
        }

        /// RFC-097 — brings `Handle::fields()` into scope for this type.
        pub trait #ext_trait {
            /// Typed single-field handles for this scope's fields.
            fn fields(&self) -> #fields_struct;
        }
        impl #ext_trait for ::pocopine::Handle<#struct_ident> {
            fn fields(&self) -> #fields_struct {
                let __poc_sid = ::pocopine::Handle::scope_id(self);
                #fields_struct {
                    #(#inits)*
                }
            }
        }
    }
}

#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match ComponentArgs::parse.parse(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let mut input = parse_macro_input!(item as ItemStruct);

    // Bake a host-side `allow(dead_code)` onto the component struct.
    // Fields that only feed `#[cfg(target_arch = "wasm32")]` code
    // paths look unread on host; CI's `-D warnings` then fails for
    // patterns the framework actively encourages (Pine primitives
    // wrap `web_sys`). Pair this with the inner attribute baked into
    // `#[handlers]` blocks — together they cover the struct and the
    // helper fns in its impl.
    input.attrs.push(syn::parse_quote!(
        #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    ));

    let struct_ident = input.ident.clone();
    let ident_str = struct_ident.to_string();
    let default_name = kebab_case(&ident_str);
    let name_str = args
        .name
        .as_ref()
        .map(|s| s.value())
        .unwrap_or_else(|| default_name.clone());

    if matches!(name_str.as_str(), "pp-component" | "pp-outlet") {
        return syn::Error::new_spanned(
            &struct_ident,
            format!(
                "component tag `<{name_str}>` is reserved by the Pocopine runtime. \
                 Rename the struct or pass an explicit `name = \"...\"` override."
            ),
        )
        .to_compile_error()
        .into();
    }

    if HTML5_ELEMENTS.binary_search(&name_str.as_str()).is_ok() {
        return syn::Error::new_spanned(
            &struct_ident,
            format!(
                "component tag `<{name_str}>` would collide with a real HTML element. \
                 Rename the struct or pass an explicit `name = \"...\"` override."
            ),
        )
        .to_compile_error()
        .into();
    }

    if args.template.is_some() && args.template_inline.is_some() {
        return syn::Error::new_spanned(
            &struct_ident,
            "`template = \"...\"` and `template_inline = \"...\"` are mutually \
             exclusive — pick one source-of-truth for the template.",
        )
        .to_compile_error()
        .into();
    }

    // RFC 060 Tier 3 — bundle mode (`extends = [...]`) is a
    // tagless re-export marker. It owns no template, no style,
    // no constructor — all it does is forward `register()` to
    // each member. An empty `extends = []` is a degenerate
    // bundle (registers nothing) and is rejected upfront so the
    // author isn't routed into the non-bundle path with a
    // missing-template diagnostic.
    if let Some(paths) = args.extends.as_ref()
        && paths.is_empty()
    {
        return syn::Error::new_spanned(
            &struct_ident,
            "`extends = []` is empty — bundle markers must list at least one member, \
                 otherwise the type is a no-op. Drop the attribute or list the components.",
        )
        .to_compile_error()
        .into();
    }
    let is_bundle = args.extends.is_some();
    if is_bundle {
        // Reject every component-body-only option in bundle
        // mode. Silently dropping `style` / `role` / `display`
        // / `transition*` / `animate` would mislead the author
        // into thinking the bundle carries them.
        let bundle_disallowed: &[(&str, bool)] = &[
            ("template", args.template.is_some()),
            ("template_inline", args.template_inline.is_some()),
            ("style", args.style.is_some()),
            ("role", args.role.is_some()),
            ("display", args.display.is_some()),
            ("transition", args.transition.is_some()),
            ("transition_in", args.transition_in.is_some()),
            ("transition_out", args.transition_out.is_some()),
            ("animate", args.animate.is_some()),
        ];
        for (name, set) in bundle_disallowed {
            if *set {
                return syn::Error::new_spanned(
                    &struct_ident,
                    format!(
                        "`{name} = ...` is not valid on a bundle marker \
                         (`extends = [...]`) — bundles are tagless re-export \
                         markers, they own no template, style, role, display, \
                         transition, or animate metadata."
                    ),
                )
                .to_compile_error()
                .into();
            }
        }
    }

    if is_bundle {
        // RFC 060 Tier 3 — minimal bundle expansion. Skip every
        // template / state / handler emission; the bundle is
        // a tagless type marker whose `register()` just forwards
        // to each `extends` entry. Cycle / dedupe protection
        // comes from the Tier 1 `mark_registered` guard.
        let extends_paths = args
            .extends
            .as_ref()
            .expect("is_bundle implies extends.is_some()");
        let extends_calls = extends_paths.iter().map(|path| {
            quote! {
                <#path as ::pocopine::__private::Component>::register();
            }
        });
        let out = quote! {
            #input

            impl #struct_ident {
                /// Bundle marker — registers every `extends` entry
                /// transitively. Idempotent via the runtime
                /// `mark_registered` guard.
                pub fn register() {
                    if !::pocopine::__private::mark_registered::<#struct_ident>() {
                        return;
                    }
                    #(#extends_calls)*
                }

                // RFC 060 Tier 4 — bundle vtables exist for
                // symmetry with non-bundle components (so
                // `app!{}`'s phf map can hold any type
                // uniformly). The `name` is informational only —
                // bundles aren't tag-resolvable.
                #[doc(hidden)]
                #[allow(non_upper_case_globals)]
                pub const __POCO_VTABLE: &'static ::pocopine::__private::ComponentVTable =
                    &::pocopine::__private::ComponentVTable {
                        name: #name_str,
                        register: <#struct_ident>::register,
                        template_html: None,
                        plan: None,
                        mount_template: None,
                    };
            }

            impl ::pocopine::__private::Component for #struct_ident {
                const NAME: &'static str = #name_str;
                fn register() {
                    <#struct_ident>::register();
                }
            }
        };
        return out.into();
    }

    let template_path: Option<LitStr> = if args.template_inline.is_some() {
        None
    } else {
        Some(match &args.template {
            Some(s) => s.clone(),
            None => LitStr::new(&format!("{ident_str}.poco"), struct_ident.span()),
        })
    };

    // RFC 049 — `#[slot(...)]` helper attributes on the struct.
    // Parse them off `input.attrs` (strips them from the
    // emitted struct so rustc doesn't see an unknown attribute)
    // and hold onto the declarations for trait emission below.
    let slot_decls = match slot::parse_and_strip_slots(&mut input.attrs) {
        Ok(s) => s,
        Err(e) => return e.to_compile_error().into(),
    };
    let slot_traits_tokens = slot::emit_slot_traits(&struct_ident, &name_str, &slot_decls);

    // RFC-031 — `#[prop]` is the explicit "parent contract"
    // marker; everything else defaults to state (internal,
    // parents can't write it). Mirrors `pub` vs private in
    // Rust — annotate what leaks outward, not what stays
    // internal. The macro strips the `#[prop]` attribute from
    // the emitted struct so rustc doesn't see an unknown
    // attribute.
    let mut field_idents: Vec<syn::Ident> = Vec::new();
    let mut field_names: Vec<String> = Vec::new();
    let mut field_types: Vec<Type> = Vec::new();
    let mut field_is_prop: Vec<bool> = Vec::new();
    let mut field_is_model: Vec<bool> = Vec::new();
    // RFC-097 — fields hidden from serde get no `FieldHandle` accessor
    // (the projection read/write round-trips through serde).
    let mut field_is_serde_skip: Vec<bool> = Vec::new();
    let mut field_model_names: Vec<Option<String>> = Vec::new();
    let mut observes: Vec<ObserveEntry> = Vec::new();
    // RFC-044 §5.10 — `flatten` fields (role-agnostic). Each entry
    // is `(container_ident, leaf_names, is_model)`. The container
    // itself is a normal state field in `field_*` — not prop, not
    // model — and each leaf is synthesised as an independent public
    // key that routes get/set through the container's serde impl.
    //
    // `is_model = true`  → `#[model(flatten)]`: leaves are
    //   parent-writable props AND child-emittable models.
    // `is_model = false` → `#[prop(flatten)]`: leaves are
    //   parent-writable props only — inbound, no `pp:update:` channel.
    let mut flatten_fields: Vec<(syn::Ident, Vec<String>, bool)> = Vec::new();
    // Bare-flatten fields — `#[prop(flatten)]` / `#[model(flatten)]`
    // with no explicit list. The field's type must implement
    // `::pocopine::__private::Props` (typically via
    // `#[derive(Props)]`); the runtime fallthrough in get / set /
    // keys / is_prop / is_model / static_prop_kind /
    // flatten_container_of / model_name routes through that trait.
    // Tuple is `(container_ident, container_type, is_model)`.
    let mut bare_flatten_fields: Vec<(syn::Ident, syn::Type, bool)> = Vec::new();
    for field in input.fields.iter_mut() {
        let Some(ident) = field.ident.clone() else {
            continue;
        };
        let field_ty = field.ty.clone();
        let mut is_prop = false;
        let mut is_model = false;
        let mut model_name: Option<String> = None;
        let mut observe_spec: Option<(Path, Option<String>)> = None;
        let mut observe_err: Option<syn::Error> = None;
        let mut is_serde_skip = false;
        field.attrs.retain(|a| {
            if a.path().is_ident("prop") {
                // Shapes accepted:
                //   #[prop]                                  bare
                //   #[prop(flatten = ["leaf1", "leaf2"])]    explicit-list flatten
                //   #[prop(flatten)]                         bare flatten — field type
                //                                            must impl `Props`
                match &a.meta {
                    Meta::Path(_) => {
                        is_prop = true;
                    }
                    Meta::List(_) => {
                        let parsed: syn::Result<Option<Vec<String>>> =
                            a.parse_args_with(|input: syn::parse::ParseStream| {
                                let mut flatten_leaves: Option<Vec<String>> = None;
                                let mut bare_flatten = false;
                                while !input.is_empty() {
                                    let key: syn::Ident = input.parse()?;
                                    if key != "flatten" {
                                        return Err(syn::Error::new_spanned(
                                            key,
                                            "unknown #[prop] key — expected: flatten",
                                        ));
                                    }
                                    if input.peek(Token![=]) {
                                        input.parse::<Token![=]>()?;
                                        let arr: syn::ExprArray = input.parse()?;
                                        let mut leaves = Vec::with_capacity(arr.elems.len());
                                        for e in arr.elems.iter() {
                                            match e {
                                                Expr::Lit(ExprLit {
                                                    lit: Lit::Str(s), ..
                                                }) => leaves.push(s.value()),
                                                other => {
                                                    return Err(syn::Error::new_spanned(
                                                        other,
                                                        "flatten leaves must be string literals",
                                                    ));
                                                }
                                            }
                                        }
                                        flatten_leaves = Some(leaves);
                                    } else {
                                        bare_flatten = true;
                                    }
                                    if input.peek(Token![,]) {
                                        input.parse::<Token![,]>()?;
                                    }
                                }
                                match (flatten_leaves, bare_flatten) {
                                    // explicit leaf list
                                    (Some(leaves), _) => Ok(Some(leaves)),
                                    // bare `#[prop(flatten)]` — field
                                    // type must impl `Props`; leaves
                                    // resolve through the trait at
                                    // runtime.
                                    (None, true) => Ok(None),
                                    (None, false) => Err(syn::Error::new_spanned(
                                        a,
                                        "#[prop] list form expects `flatten` or \
                                         `flatten = [\"leaf1\", \"leaf2\"]`",
                                    )),
                                }
                            });
                        match parsed {
                            Ok(Some(leaves)) => {
                                // Explicit-list flatten — container is
                                // internal, leaves take the prop role.
                                is_prop = false;
                                flatten_fields.push((ident.clone(), leaves, false));
                            }
                            Ok(None) => {
                                // Bare flatten — container is
                                // internal, leaves resolve through
                                // `<FieldTy as Props>` at runtime.
                                is_prop = false;
                                bare_flatten_fields.push((ident.clone(), field_ty.clone(), false));
                            }
                            Err(e) => observe_err = Some(e),
                        }
                    }
                    Meta::NameValue(_) => {
                        observe_err = Some(syn::Error::new_spanned(
                            a,
                            "#[prop] accepts either bare form or \
                             #[prop(flatten = [\"leaf1\", \"leaf2\"])]",
                        ));
                    }
                }
                return false;
            }
            if a.path().is_ident("model") {
                // Shapes accepted:
                //   #[model]                                  bare
                //   #[model(name = "…")]                      wire-name rename
                //   #[model(flatten = ["leaf1", "leaf2"])]    explicit-list flatten
                //   #[model(flatten)]                         bare flatten — field type
                //                                              must impl `Props`
                let parsed: syn::Result<(Option<String>, Option<Vec<String>>, bool)> = match &a.meta
                {
                    Meta::Path(_) => Ok((None, None, false)),
                    Meta::List(_) => a.parse_args_with(|input: syn::parse::ParseStream| {
                        let mut wire_name: Option<String> = None;
                        let mut flatten_leaves: Option<Vec<String>> = None;
                        let mut bare_flatten = false;
                        while !input.is_empty() {
                            let key: syn::Ident = input.parse()?;
                            if input.peek(Token![=]) {
                                input.parse::<Token![=]>()?;
                                if key == "name" {
                                    let s: LitStr = input.parse()?;
                                    wire_name = Some(s.value());
                                } else if key == "flatten" {
                                    let arr: syn::ExprArray = input.parse()?;
                                    let mut leaves = Vec::with_capacity(arr.elems.len());
                                    for e in arr.elems.iter() {
                                        match e {
                                            Expr::Lit(ExprLit {
                                                lit: Lit::Str(s), ..
                                            }) => leaves.push(s.value()),
                                            other => {
                                                return Err(syn::Error::new_spanned(
                                                    other,
                                                    "flatten leaves must be string literals",
                                                ));
                                            }
                                        }
                                    }
                                    flatten_leaves = Some(leaves);
                                } else {
                                    return Err(syn::Error::new_spanned(
                                        key,
                                        "unknown #[model] key — expected: name, flatten",
                                    ));
                                }
                            } else if key == "flatten" {
                                // Bare `#[model(flatten)]` — the
                                // field type must implement
                                // `Props` (typically via
                                // `#[derive(Props)]`). Leaves
                                // resolve through that trait at
                                // runtime; outer match below
                                // pushes the container into
                                // `bare_flatten_fields`.
                                bare_flatten = true;
                            } else {
                                return Err(syn::Error::new_spanned(
                                    key,
                                    "expected `=` after #[model] key",
                                ));
                            }
                            if input.peek(Token![,]) {
                                input.parse::<Token![,]>()?;
                            }
                        }
                        Ok((wire_name, flatten_leaves, bare_flatten))
                    }),
                    Meta::NameValue(_) => Err(syn::Error::new_spanned(
                        a,
                        "#[model] accepts either bare form, \
                             #[model(name = \"...\")], or \
                             #[model(flatten = [\"leaf1\", \"leaf2\"])]",
                    )),
                };
                match parsed {
                    Ok((name, flatten, bare)) => {
                        // RFC-044 §5.10 — `flatten` is incompatible with
                        // `name`: there is no single wire name to rename
                        // when the field is exploded into per-leaf keys.
                        // Catch the combination instead of silently
                        // dropping `name` on the floor.
                        if name.is_some() && (flatten.is_some() || bare) {
                            observe_err = Some(syn::Error::new_spanned(
                                a,
                                "#[model(name = \"...\", flatten…)] is not allowed — \
                                 `flatten` explodes the field into per-leaf wire keys, \
                                 so a single rename has no target. Drop one of the two.",
                            ));
                        } else if let Some(leaves) = flatten {
                            // Explicit-list flatten — container is
                            // internal, leaves take the prop + model
                            // role (added to `flatten_fields`).
                            is_prop = false;
                            is_model = false;
                            model_name = None;
                            flatten_fields.push((ident.clone(), leaves, true));
                        } else if bare {
                            // Bare `#[model(flatten)]` — container is
                            // internal, leaves resolve through
                            // `<FieldTy as Props>` at runtime and
                            // additionally carry the `#[model]` role
                            // (`is_model = true` -> per-leaf
                            // `pp:update:<leaf>` channel).
                            is_prop = false;
                            is_model = false;
                            model_name = None;
                            bare_flatten_fields.push((ident.clone(), field_ty.clone(), true));
                        } else {
                            is_prop = true;
                            is_model = true;
                            model_name = name;
                        }
                    }
                    Err(e) => observe_err = Some(e),
                }
                return false;
            }
            if a.path().is_ident("observe") {
                // Shape: `#[observe(KEY)]` or
                // `#[observe(KEY, field = "name")]`. First positional
                // arg is the `InjectKey<Handle<T>>` path; the
                // optional `field = "…"` overrides the default
                // parent-field name (which would otherwise match
                // `field_ident`).
                let parsed = a.parse_args_with(|input: syn::parse::ParseStream| {
                    let key: Path = input.parse()?;
                    let mut rename: Option<LitStr> = None;
                    while !input.is_empty() {
                        input.parse::<Token![,]>()?;
                        if input.is_empty() {
                            break;
                        }
                        let kv: MetaNameValue = input.parse()?;
                        if kv.path.is_ident("field") {
                            match kv.value {
                                Expr::Lit(ExprLit {
                                    lit: Lit::Str(s), ..
                                }) => rename = Some(s),
                                other => {
                                    return Err(syn::Error::new_spanned(
                                        other,
                                        "`field` must be a string literal",
                                    ));
                                }
                            }
                        } else {
                            return Err(syn::Error::new_spanned(
                                kv.path,
                                "unknown #[observe] key — expected: field",
                            ));
                        }
                    }
                    Ok((key, rename.map(|s| s.value())))
                });
                match parsed {
                    Ok(spec) => observe_spec = Some(spec),
                    Err(e) => observe_err = Some(e),
                }
                return false;
            }
            // RFC-097 — note (don't strip) serde-skip so the field-handle
            // emission can exclude fields that won't round-trip through
            // the projection serde path. `serde` derive still needs the
            // attribute, so keep it (`return true`).
            if a.path().is_ident("serde") {
                let _ = a.parse_nested_meta(|meta| {
                    if meta.path.is_ident("skip")
                        || meta.path.is_ident("skip_serializing")
                        || meta.path.is_ident("skip_deserializing")
                    {
                        is_serde_skip = true;
                    }
                    // Consume any `= value` (rename/with/…) OR a `(…)`
                    // group (`rename(serialize=…, deserialize=…)`) so
                    // parsing later serde keys in the same attr doesn't
                    // error and silently drop a trailing `skip`.
                    if meta.input.peek(Token![=]) {
                        let _ = meta.value().and_then(|v| v.parse::<Expr>());
                    } else if meta.input.peek(syn::token::Paren) {
                        let _ = meta.parse_nested_meta(|_| Ok(()));
                    }
                    Ok(())
                });
                return true;
            }
            true
        });
        if let Some(err) = observe_err {
            return err.to_compile_error().into();
        }
        if let Some((key_path, rename)) = observe_spec {
            let name_on_root =
                rename.unwrap_or_else(|| ident.to_string().trim_start_matches("r#").to_string());
            observes.push(ObserveEntry {
                field_ident: ident.clone(),
                field_ty: field_ty.clone(),
                field_name_on_root: name_on_root,
                key_path,
            });
        }
        let rust_name = ident.to_string().trim_start_matches("r#").to_string();
        field_types.push(field_ty);
        field_names.push(rust_name.clone());
        field_idents.push(ident);
        field_is_prop.push(is_prop);
        field_is_model.push(is_model);
        field_is_serde_skip.push(is_serde_skip);
        field_model_names.push(model_name.or_else(|| is_model.then_some(rust_name)));
    }

    // RFC-097 §3.3 — reject interior-mutability field types up front so
    // the `&self`-skips-the-sweep optimisation stays sound.
    if let Some(err) = interior_mut_rejection(&field_idents, &field_types) {
        return quote! { #input #err }.into();
    }

    // RFC-044 §5.10 — collision rule. A flatten leaf must not shadow
    // a real struct field or another flatten leaf; both would race
    // for the same scope-state key with divergent get/set arms.
    {
        let mut seen: std::collections::HashSet<&str> =
            field_names.iter().map(String::as_str).collect();
        for (_, leaves, _) in &flatten_fields {
            for leaf in leaves {
                if !seen.insert(leaf.as_str()) {
                    return syn::Error::new_spanned(
                        &struct_ident,
                        format!(
                            "flatten leaf `{leaf}` collides with an existing field \
                             or another flatten leaf — leaf names must be unique"
                        ),
                    )
                    .to_compile_error()
                    .into();
                }
            }
        }
    }

    // RFC-044 §5.10 — bare-flatten codegen precompute. For each
    // `#[prop(flatten)]` / `#[model(flatten)]` field with NO explicit
    // leaf list, the field's type must implement
    // `::pocopine::__private::Props` (typically via
    // `#[derive(Props)]`); the runtime fallthrough below routes leaf
    // metadata through that trait at runtime. Explicit-list flatten
    // (still supported, RFC-044 §5.10.2) emits static match arms; the
    // two paths coexist and the explicit arms run first.
    let bare_flatten_is_prop_terms: Vec<proc_macro2::TokenStream> = bare_flatten_fields
        .iter()
        .map(|(_, ty, _)| {
            quote! {
                <#ty as ::pocopine::__private::Props>::prop_leaves().contains(&key)
            }
        })
        .collect();
    let bare_flatten_is_model_terms: Vec<proc_macro2::TokenStream> = bare_flatten_fields
        .iter()
        .filter(|(_, _, is_model)| *is_model)
        .map(|(_, ty, _)| {
            quote! {
                <#ty as ::pocopine::__private::Props>::prop_leaves().contains(&key)
            }
        })
        .collect();
    let bare_flatten_keys_extend: Vec<proc_macro2::TokenStream> = bare_flatten_fields
        .iter()
        .map(|(_, ty, _)| {
            quote! {
                __keys.extend_from_slice(
                    <#ty as ::pocopine::__private::Props>::prop_leaves(),
                );
            }
        })
        .collect();
    let bare_flatten_get_lookups: Vec<proc_macro2::TokenStream> = bare_flatten_fields
        .iter()
        .map(|(ident, ty, _)| {
            quote! {
                if <#ty as ::pocopine::__private::Props>::prop_leaves().contains(&key) {
                    return ::pocopine::__private::Props::prop_get(&self.#ident, key);
                }
            }
        })
        .collect();
    let bare_flatten_set_lookups: Vec<proc_macro2::TokenStream> = bare_flatten_fields
        .iter()
        .map(|(ident, ty, _)| {
            quote! {
                if <#ty as ::pocopine::__private::Props>::prop_leaves().contains(&key) {
                    return ::pocopine::__private::Props::prop_set(
                        &mut self.#ident,
                        key,
                        value,
                    );
                }
            }
        })
        .collect();
    let bare_flatten_static_kind_lookups: Vec<proc_macro2::TokenStream> = bare_flatten_fields
        .iter()
        .map(|(_, ty, _)| {
            quote! {
                if <#ty as ::pocopine::__private::Props>::prop_leaves().contains(&key) {
                    return <#ty as ::pocopine::__private::Props>::prop_static_kind(key);
                }
            }
        })
        .collect();
    let bare_flatten_container_lookups: Vec<proc_macro2::TokenStream> = bare_flatten_fields
        .iter()
        .map(|(ident, ty, _)| {
            let container_name = ident.to_string().trim_start_matches("r#").to_string();
            quote! {
                if <#ty as ::pocopine::__private::Props>::prop_leaves().contains(&key) {
                    return ::core::option::Option::Some(#container_name);
                }
            }
        })
        .collect();
    let bare_flatten_model_name_lookups: Vec<proc_macro2::TokenStream> = bare_flatten_fields
        .iter()
        .filter(|(_, _, is_model)| *is_model)
        .map(|(_, ty, _)| {
            quote! {
                for __leaf in
                    <#ty as ::pocopine::__private::Props>::prop_leaves().iter()
                {
                    if *__leaf == key {
                        return ::core::option::Option::Some(*__leaf);
                    }
                }
            }
        })
        .collect();

    // RFC-044 §5.10 flatten-leaf codegen. For each `flatten = ["a",
    // "b"]` container field, each leaf becomes a synthetic public
    // key that:
    //
    //   - `get(leaf)`: serialise the container once, read the leaf
    //     key off the resulting JS object.
    //   - `set(leaf, value)`: serialise the container, splice the
    //     leaf, deserialise back into the container field.
    //   - `keys()` includes the leaf (without it, `snapshot_models`
    //     in the model runtime wouldn't iterate it and emits would
    //     never fire).
    //   - `is_prop(leaf)`: true for both roles (parent mirror-in via
    //     pp-model:leaf / static attr / pp-bind).
    //   - `is_model(leaf)` / `model_name(leaf)`: true only for
    //     `#[model(flatten)]` leaves — `#[prop(flatten)]` is inbound
    //     only, no `pp:update:` channel.
    //
    // One serde round-trip per inbound mirror-in write; same order
    // as the pre-landing `Option<T>` empty-string shim. Outbound
    // emission (model leaves only) uses the same path the non-flatten
    // struct form already walks — the snapshot-diff machinery in
    // `model_runtime.rs` iterates leaves just like any other
    // `is_model` key.
    let flatten_leaf_get_arms = flatten_fields.iter().flat_map(|(container, leaves, _)| {
        leaves.iter().map(move |leaf| {
            quote! {
                #leaf => {
                    let __obj = ::pocopine::__private::serde_wasm_bindgen::to_value(
                        &self.#container,
                    )
                    .unwrap_or(::pocopine::__private::JsValue::UNDEFINED);
                    ::pocopine::__private::js_sys::Reflect::get(
                        &__obj,
                        &::pocopine::__private::JsValue::from_str(#leaf),
                    )
                    .unwrap_or(::pocopine::__private::JsValue::UNDEFINED)
                },
            }
        })
    });

    let flatten_leaf_set_arms = flatten_fields.iter().flat_map(|(container, leaves, _)| {
        leaves.iter().map(move |leaf| {
            quote! {
                #leaf => {
                    let __value = value;
                    let __obj = ::pocopine::__private::serde_wasm_bindgen::to_value(
                        &self.#container,
                    )
                    .unwrap_or(::pocopine::__private::JsValue::UNDEFINED);
                    if __obj.is_object() {
                        let __normalised =
                            if __value.as_string().as_deref() == Some("") {
                                ::pocopine::__private::JsValue::NULL
                            } else {
                                __value
                            };
                        let _ = ::pocopine::__private::js_sys::Reflect::set(
                            &__obj,
                            &::pocopine::__private::JsValue::from_str(#leaf),
                            &__normalised,
                        );
                        if let Ok(v) =
                            ::pocopine::__private::serde_wasm_bindgen::from_value(__obj)
                        {
                            self.#container = v;
                        }
                    }
                }
            }
        })
    });

    // `serde_wasm_bindgen::to_value(&None::<T>)` returns `undefined`,
    // but RFC-044 §5.4 promises `None` serialises as `null`. Canonicalise
    // at the boundary so the emit detail, parent mirror-in round-trips,
    // and `get()` reads are all consistent.
    let get_arms = field_idents
        .iter()
        .zip(field_names.iter())
        .map(|(id, name)| {
            quote! {
                #name => {
                    let __v = ::pocopine::__private::serde_wasm_bindgen::to_value(&self.#id)
                        .unwrap_or(::pocopine::__private::JsValue::NULL);
                    if __v.is_undefined() {
                        ::pocopine::__private::JsValue::NULL
                    } else {
                        __v
                    }
                },
            }
        })
        .chain(flatten_leaf_get_arms);

    // RFC-095 W2 — per-field dirty-check fingerprints. One arm per
    // direct field; flatten leaves / computed keys fall through to
    // `None` (= "unknown", treated as changed by the dirty sweep —
    // the conservative direction). Same `Serialize` bound as
    // `get_arms`, so this compiles wherever `get` does.
    let fingerprint_arms = field_call_arms(
        &field_idents,
        &field_names,
        quote! { ::pocopine::__private::fingerprint_value },
    );
    // RFC-095 W2b — O(1) length probes for the dirty sweep's
    // collection short-circuit.
    let quick_len_arms = field_call_arms(
        &field_idents,
        &field_names,
        quote! { ::pocopine::__private::quick_len_value },
    );

    // RFC-096 S3 — typed text lane. Same Serialize bound as
    // get(); the projector itself decides scalar-vs-compound.
    let text_arms = field_call_arms(
        &field_idents,
        &field_names,
        quote! { ::pocopine::__private::text_projection },
    );

    let set_arms = field_idents.iter().zip(field_names.iter()).map(|(id, name)| {
        quote! {
            #name => {
                let __value = value;
                if let Ok(v) = ::pocopine::__private::serde_wasm_bindgen::from_value(__value.clone()) {
                    self.#id = v;
                } else if __value.as_string().as_deref() == Some("") {
                    if let Ok(v) = ::pocopine::__private::serde_wasm_bindgen::from_value(
                        ::pocopine::__private::JsValue::NULL,
                    ) {
                        self.#id = v;
                    }
                }
            }
        }
    }).chain(flatten_leaf_set_arms);

    // All flatten leaves — parent-writable props regardless of role.
    let flatten_leaf_names: Vec<&String> = flatten_fields
        .iter()
        .flat_map(|(_, leaves, _)| leaves.iter())
        .collect();
    // Model-flatten leaves only — these additionally carry the
    // `#[model]` role (emittable via `pp:update:<leaf>`).
    let flatten_model_leaf_names: Vec<&String> = flatten_fields
        .iter()
        .filter(|(_, _, is_model)| *is_model)
        .flat_map(|(_, leaves, _)| leaves.iter())
        .collect();

    // RFC-044 §5.10.4 — collision shield for the bare-flatten
    // fallthrough. A bare leaf is only known at runtime; without
    // guarding the `_` arms a key that ALSO names a real struct field
    // (or an explicit-flatten leaf) would be misclassified as
    // belonging to a bare container — `is_prop("x")` would flip true
    // via the bare OR-term even when `x` is unmarked state, and
    // `flatten_container_of("x")` would point at the wrong container,
    // dual-triggering a `#[watch(<container>)]` that wasn't actually
    // mutated. Gate the fallthrough on "key isn't already a static
    // known key".
    //
    // `static_known_field_names_check` matches every real struct
    // field (used by `flatten_container_of` / `model_name`, whose
    // static arms cover only explicit-flatten leaves).
    // `static_known_keys_check` matches every real field PLUS every
    // explicit-flatten leaf (used by `is_prop` / `is_model`, whose
    // matches-cover only the *role-marked* subset).
    let static_known_field_names_check = if field_names.is_empty() {
        quote! { false }
    } else {
        quote! { matches!(key, #(#field_names)|*) }
    };
    let static_known_keys_check_idents: Vec<&String> = field_names
        .iter()
        .chain(flatten_leaf_names.iter().copied())
        .collect();
    let static_known_keys_check = if static_known_keys_check_idents.is_empty() {
        quote! { false }
    } else {
        quote! { matches!(key, #(#static_known_keys_check_idents)|*) }
    };
    // Runtime collision audit emitted into `keys()` initialisation —
    // panics on first call (mount time) if any bare-flatten leaf
    // collides with a real field, an explicit-flatten leaf, or
    // another bare leaf. Compile-time const-eval is future work
    // (needs an inherent `const` on the `Props` impl); this fails
    // loud at mount, matching RFC §5.10.4's "hard error" promise in
    // practice without blocking the v1 derive shape.
    let static_known_keys_audit_arr = static_known_keys_check_idents.iter().map(|n| quote! { #n });
    let bare_flatten_audit_extends = bare_flatten_fields.iter().map(|(_, ty, _)| {
        quote! {
            __bare_leaves.extend_from_slice(
                <#ty as ::pocopine::__private::Props>::prop_leaves(),
            );
        }
    });
    let bare_flatten_collision_audit = if bare_flatten_fields.is_empty() {
        quote! {}
    } else {
        quote! {
            // Gathered once on first `keys()` call; panics on any
            // leaf-name collision (RFC-044 §5.10.4).
            let mut __bare_leaves: ::std::vec::Vec<&'static str> = ::std::vec::Vec::new();
            #(#bare_flatten_audit_extends)*
            let __static_known: &[&'static str] = &[#(#static_known_keys_audit_arr),*];
            for __i in 0..__bare_leaves.len() {
                let __leaf = __bare_leaves[__i];
                if __static_known.contains(&__leaf) {
                    panic!(
                        "RFC-044 §5.10.4 — bare-flatten leaf `{}` collides with \
                         an existing field or explicit-flatten leaf on this \
                         component; rename the leaf",
                        __leaf,
                    );
                }
                if __bare_leaves[..__i].contains(&__leaf) {
                    panic!(
                        "RFC-044 §5.10.4 — bare-flatten leaf `{}` is exposed by \
                         more than one container on this component",
                        __leaf,
                    );
                }
            }
        }
    };

    // RFC-044 §5.10 — leaf → container map for `flatten_container_of`.
    // A write to any leaf mutates the whole container field, so the
    // proxy `set` trap triggers the container key too, letting one
    // `#[watch(<container>)]` fire on any leaf change. Role-agnostic:
    // both `#[prop(flatten)]` and `#[model(flatten)]` leaves map here.
    let mut flatten_container_arms: Vec<proc_macro2::TokenStream> = Vec::new();
    for (container, leaves, _) in &flatten_fields {
        let container_name = container.to_string();
        let container_name = container_name.trim_start_matches("r#");
        for leaf in leaves {
            flatten_container_arms.push(quote! {
                #leaf => ::core::option::Option::Some(#container_name),
            });
        }
    }

    let keys_arr = field_names
        .iter()
        .chain(flatten_leaf_names.iter().copied())
        .map(|n| quote! { #n });

    // RFC-031 — `is_prop(key)` returns true only for fields
    // annotated `#[prop]`. Everything else is state — parents
    // stay out. Runtime consults this in `apply_static_props`,
    // `pp-bind` child-prop write, and `pp-model` mirror-in.
    //
    // Flatten leaves always count as props (parent-writable via
    // static attr / `pp-bind` / `pp-model:<leaf>`), regardless of
    // whether the container is `#[prop(flatten)]` or
    // `#[model(flatten)]`.
    let prop_field_names: Vec<&String> = field_names
        .iter()
        .zip(field_is_prop.iter())
        .filter_map(|(n, is_prop)| is_prop.then_some(n))
        .chain(flatten_leaf_names.iter().copied())
        .collect();
    // `matches!(key, a | b | c)` needs at least one pattern —
    // fall back to a `false` literal when no field is a prop. Each
    // bare-flatten container contributes an extra OR term resolved
    // through `<Ty as Props>::prop_leaves()` at runtime.
    let is_prop_body = match (
        prop_field_names.is_empty(),
        bare_flatten_is_prop_terms.is_empty(),
    ) {
        (true, true) => quote! { let _ = key; false },
        (false, true) => quote! { matches!(key, #(#prop_field_names)|*) },
        // Bare-flatten OR-terms are gated by `!static_known_keys_check`
        // so a real field that shadows a bare leaf doesn't flip
        // `is_prop` to true via the bare path (RFC-044 §5.10.4).
        (true, false) => quote! {
            !(#static_known_keys_check) && (#(#bare_flatten_is_prop_terms)||*)
        },
        (false, false) => quote! {
            matches!(key, #(#prop_field_names)|*)
                || (!(#static_known_keys_check)
                    && (#(#bare_flatten_is_prop_terms)||*))
        },
    };

    // Flatten leaves have no statically-known Rust type at the
    // container's leaf granularity (the attribute carries names, not
    // types). Probe the *current* serialised container value to
    // classify the leaf: a `String` leaf must report `String` so a
    // numeric-looking value (`label="2024"`) is not coerced to a
    // number and dropped by the leaf's serde round-trip. Probe runs
    // against the default-constructed component during
    // `apply_static_props`, before any parent write — accurate for
    // every non-`Option` leaf (an `Option::None` leaf serialises to
    // `null` and falls back to `Auto`, matching the non-flatten
    // `Option<T>` story).
    let flatten_leaf_kind_arms = flatten_fields.iter().flat_map(|(container, leaves, _)| {
        leaves.iter().map(move |leaf| {
            quote! {
                #leaf => {
                    let __obj = ::pocopine::__private::serde_wasm_bindgen::to_value(
                        &self.#container,
                    )
                    .unwrap_or(::pocopine::__private::JsValue::UNDEFINED);
                    let __leaf = ::pocopine::__private::js_sys::Reflect::get(
                        &__obj,
                        &::pocopine::__private::JsValue::from_str(#leaf),
                    )
                    .unwrap_or(::pocopine::__private::JsValue::UNDEFINED);
                    if __leaf.as_bool().is_some() {
                        ::pocopine::__private::StaticPropKind::Bool
                    } else if __leaf.as_f64().is_some() {
                        ::pocopine::__private::StaticPropKind::Number
                    } else if __leaf.is_string() {
                        ::pocopine::__private::StaticPropKind::String
                    } else {
                        ::pocopine::__private::StaticPropKind::Auto
                    }
                },
            }
        })
    });
    let static_prop_kind_arms = field_names
        .iter()
        .zip(field_is_prop.iter())
        .zip(field_types.iter())
        .filter_map(|((name, is_prop), ty)| {
            if !*is_prop {
                return None;
            }
            let kind = static_prop_kind_for_type(ty);
            Some(quote! { #name => #kind, })
        })
        .chain(flatten_leaf_kind_arms);
    let static_prop_kind_body = quote! {
        match key {
            #(#static_prop_kind_arms)*
            _ => {
                #(#bare_flatten_static_kind_lookups)*
                ::pocopine::__private::StaticPropKind::Auto
            }
        }
    };

    // Only `#[model(flatten)]` leaves are models — `#[prop(flatten)]`
    // leaves are inbound-only and never emit.
    let model_field_names: Vec<&String> = field_names
        .iter()
        .zip(field_is_model.iter())
        .filter_map(|(n, is_model)| is_model.then_some(n))
        .chain(flatten_model_leaf_names.iter().copied())
        .collect();
    // Same shape as `is_prop_body` (above) — bare model-flatten
    // containers contribute OR terms resolved through
    // `<Ty as Props>::prop_leaves()` at runtime, gated by
    // `!static_known_keys_check` so a real field that happens to
    // share a bare leaf's name doesn't bleed into the model role.
    let is_model_body = match (
        model_field_names.is_empty(),
        bare_flatten_is_model_terms.is_empty(),
    ) {
        (true, true) => quote! { let _ = key; false },
        (false, true) => quote! { matches!(key, #(#model_field_names)|*) },
        (true, false) => quote! {
            !(#static_known_keys_check) && (#(#bare_flatten_is_model_terms)||*)
        },
        (false, false) => quote! {
            matches!(key, #(#model_field_names)|*)
                || (!(#static_known_keys_check)
                    && (#(#bare_flatten_is_model_terms)||*))
        },
    };
    let model_name_arms = field_names
        .iter()
        .zip(field_is_model.iter())
        .zip(field_model_names.iter())
        .filter_map(|((field_name, is_model), wire_name)| {
            if !is_model {
                return None;
            }
            let wire_name = wire_name.as_ref()?;
            Some(quote! {
                #field_name => ::core::option::Option::Some(#wire_name),
            })
        })
        .chain(flatten_model_leaf_names.iter().copied().map(|leaf| {
            // Model-flatten leaves have no separate wire-name rename
            // — the leaf literal IS the wire name. `#[prop(flatten)]`
            // leaves are not models and contribute no arm here.
            quote! {
                #leaf => ::core::option::Option::Some(#leaf),
            }
        }));
    // `#[model]` emission sends the field's value as the CustomEvent
    // detail — no parent struct, no key-value context. So we serialize
    // the field directly and let any `#[serde(serialize_with = ...)]`
    // / `#[serde(with = ...)]` on the field take effect through its
    // natural serde impl. Key-affecting attrs (`rename`,
    // `skip_serializing_if`) are semantically inapplicable here —
    // there's no enclosing struct whose key they could rename or
    // whose presence they could control. A previous revision wrapped
    // the field in a `__PocoModelField { value: &T }` struct + read
    // back via `Reflect::get("value")`, but that broke silently under
    // `rename` (wrapper serialised to `{"foo": …}`, hard-coded lookup
    // returned UNDEFINED) and under `skip_serializing_if` on
    // `Option<T>::None` (wrapper serialised to `{}`, lookup returned
    // UNDEFINED — contradicting RFC-044 §5.4's "None canonicalises to
    // null" promise). Direct serialisation gets the canonical serde
    // shape (including `null` for `None`) for free.
    let model_value_arms = field_idents
        .iter()
        .zip(field_names.iter())
        .zip(field_is_model.iter())
        .filter_map(|((field_ident, field_name), is_model)| {
            if !is_model {
                return None;
            }
            Some(quote! {
                #field_name => {
                    let __v = ::pocopine::__private::serde_wasm_bindgen::to_value(
                        &self.#field_ident,
                    )
                    .unwrap_or(::pocopine::__private::JsValue::NULL);
                    if __v.is_undefined() {
                        ::pocopine::__private::JsValue::NULL
                    } else {
                        __v
                    }
                },
            })
        });

    // Resolve `role = "..."` → canonical HTML tag name. An unknown
    // role is a compile-time error; an explicitly-omitted role
    // keeps the classic `inject_pp_data`-only path so non-primitive
    // components don't need a placeholder root.
    let mut role_for_template_plan: Option<(String, String)> = None;
    let role_arg: proc_macro2::TokenStream = match args.role.as_ref() {
        Some(lit) => {
            let role_name = lit.value();
            let Some(tag) = role_to_tag(&role_name) else {
                return syn::Error::new_spanned(
                    lit,
                    format!(
                        "unknown primitive role `{role_name}` — expected one of: \
                         visual, interactive, link, media, panel, scope, surface, \
                         heading, text, list, item, label"
                    ),
                )
                .to_compile_error()
                .into();
            };
            // RFC-058 Phase 6.5 — body / slot fragments need the
            // same `<root>` substitution `compile_template`
            // applies at runtime. The runtime version stamps
            // `data-pine-role="<role>"`; mirror that exact attr
            // shape here so DOM / CSS selectors agree across the
            // two paths.
            role_for_template_plan =
                Some((tag.to_string(), format!("data-pine-role=\"{}\"", role_name)));
            quote! { Some((#tag, #role_name)) }
        }
        None => quote! { None },
    };
    let role_for_static_template: Option<(&'static str, String)> =
        args.role.as_ref().and_then(|lit| {
            let role_name = lit.value();
            role_to_tag(&role_name).map(|tag| (tag, role_name))
        });

    // RFC-038 transition / animate args → string literals the
    // generated `ComponentState` impl returns. Precedence:
    //   transition_in wins for enter, transition_out wins for leave,
    //   `transition` provides the fallback for whichever isn't
    //   explicitly set. Missing entirely → "". `animate = "flip"`
    //   sets the animate-kind literal; anything else falls back to
    //   the raw string for forwards compatibility.
    let transition_sym = args
        .transition
        .as_ref()
        .map(|l| l.value())
        .unwrap_or_default();
    let transition_in = args
        .transition_in
        .as_ref()
        .map(|l| l.value())
        .unwrap_or_else(|| transition_sym.clone());
    let transition_out = args
        .transition_out
        .as_ref()
        .map(|l| l.value())
        .unwrap_or_else(|| transition_sym.clone());
    let animate_kind = args.animate.as_ref().map(|l| l.value()).unwrap_or_default();
    let transition_in_literal = proc_macro2::Literal::string(&transition_in);
    let transition_out_literal = proc_macro2::Literal::string(&transition_out);
    let animate_literal = proc_macro2::Literal::string(&animate_kind);

    // RFC 045 + RFC 050 §4.5 — validate the `.poco` at macro
    // expansion time using the html5ever-backed parser. Strict
    // mode: errors replace the expansion output via
    // `syn::Error` with a pre-rendered `annotate-snippets`
    // block pointing at the exact `.poco` line. Lenient mode
    // (POCOPINE_TEMPLATES_LENIENT=1, RFC 045 §9): warnings
    // ride alongside the normal expansion so the component
    // still registers.
    //
    // Also keeps the parsed AST in scope for RFC 049's
    // consumer-side slot-contract scan, run below when the
    // consumer declared a `uses = [...]` list.
    let template_source = match (&args.template_inline, &template_path) {
        (Some(lit), _) => TemplateSource::Inline {
            source: lit.value(),
            anchor: lit,
        },
        (None, Some(path)) => TemplateSource::File(path),
        // Unreachable — `template_path` is `None` only when
        // `template_inline` is `Some`, but we still want a
        // safe fallback rather than `unreachable!` so the
        // expansion is robust to future refactors.
        (None, None) => TemplateSource::File(template_path.as_ref().unwrap()),
    };
    let (template_warnings, template_ast) =
        match validate_template_or_emit_errors(template_source, &name_str) {
            Ok((warning, ast)) => (warning.unwrap_or_else(proc_macro2::TokenStream::new), ast),
            Err(token_stream) => return token_stream,
        };

    // RFC 049 consumer-side scan — emit trait-bound assertions
    // for each (parent, child) pair where both tags resolve
    // through the consumer's `uses` list. Empty when `uses` is
    // absent or the template has no recognised typed parents.
    let slot_assertions_tokens = match (&args.uses, &template_ast) {
        (Some(uses_table), Some(ast)) => slot_assertions::emit_slot_assertions(ast, uses_table),
        _ => proc_macro2::TokenStream::new(),
    };

    // RFC 060 Tier 2 — hard error on any custom-element tag in
    // the template that isn't covered by the consumer's `uses`
    // list. Opt-in: validation only runs when `uses = [...]` is
    // declared.
    let unknown_tag_diagnostics_tokens = match (&args.uses, &template_ast) {
        (Some(uses_table), Some(ast)) => {
            slot_assertions::emit_unknown_tag_diagnostics(ast, uses_table)
        }
        _ => proc_macro2::TokenStream::new(),
    };

    // RFC 063 — hard error on any directive RFC 063 §4.1
    // deletes. Always-on (no opt-in). See
    // `forbidden_directives::FORBIDDEN` for the current entries.
    let forbidden_directive_diagnostics_tokens = match &template_ast {
        Some(ast) => forbidden_directives::emit_diagnostics(ast),
        None => proc_macro2::TokenStream::new(),
    };

    // Issue #76 — reject `<template pp-for>` rows whose direct
    // element child is another `<template>`. Always-on.
    let pp_for_template_child_diagnostics_tokens = match &template_ast {
        Some(ast) => pp_for_diagnostics::emit_diagnostics(ast),
        None => proc_macro2::TokenStream::new(),
    };

    // RFC 084 — typed slot props validation. Emits `const _: ()`
    // blocks that compare each `<slot :LHS=...>` publication
    // against the declared `props = T`'s `#[prop]` field set at
    // `cargo check` time. Returns both validation tokens AND
    // byte-level template edits (Phase 2 iterated-mode
    // auto-publishes the pp-for iter var by inserting
    // `:VAR="VAR"` into the slot's opening tag). Untyped slots
    // (no `props = T`) emit nothing — backwards-compatible with
    // every existing `#[slot]` site.
    let (slot_props_validation_tokens, slot_template_edits) = match &template_ast {
        Some(ast) => {
            let emit = slot::emit_slot_props_validation(ast, &slot_decls, &struct_ident);
            (emit.tokens, emit.template_edits)
        }
        None => (proc_macro2::TokenStream::new(), Vec::new()),
    };

    // RFC-111 — compile-time template-path validation: one
    // const-eval check per expression root the template can
    // evaluate, joined by rustc's const machinery against the
    // `__POC_TEMPLATE_FIELDS` const emitted below and the
    // `__POC_COMPUTED_KEYS` / `__POC_HANDLER_KEYS` consts
    // `#[handlers]` emits. A typo'd or renamed root becomes a
    // const panic whose message (formatted here, at expansion
    // time) names the root, the offending directive, the
    // template, and the nearest field — anchored on the
    // `template` argument's span. Bare `#[prop(flatten)]` leaves
    // resolve at runtime through the `Props` trait, so those
    // components keep handler checks but skip bindable ones.
    // `unchecked_paths = "true"` opts out.
    let own_bindable_names: Vec<String> = field_names
        .iter()
        .cloned()
        .chain(
            flatten_fields
                .iter()
                .flat_map(|(_, leaves, _)| leaves.iter().cloned()),
        )
        .collect();
    let template_path_assertions_tokens = match &template_ast {
        Some(ast) if !args.unchecked_paths => {
            let skip_bindable = !bare_flatten_fields.is_empty();
            let span = args
                .template
                .as_ref()
                .or(args.template_inline.as_ref())
                .map(|l| l.span())
                .unwrap_or_else(proc_macro2::Span::call_site);
            template_paths::emit_path_assertions(
                ast,
                &struct_ident,
                skip_bindable,
                &own_bindable_names,
                span,
            )
        }
        _ => proc_macro2::TokenStream::new(),
    };
    let template_path_marker_tokens = {
        let fields_const = template_paths::field_keys_const(own_bindable_names.iter().cloned());
        quote! {
            #[doc(hidden)]
            impl #struct_ident {
                #fields_const
            }
        }
    };
    // RFC-112 hardening — local `<pp-component :is="field">` selections
    // must be `ComponentRef<Host>` / `Option<ComponentRef<Host>>`. Bare computed names
    // resolve through the return-type markers emitted by `#[handlers]`.
    // `$store` and other dotted dynamic-scope paths are enforced by the
    // runtime's strict tagged `ComponentRef` wire shape.
    let dynamic_component_selection_assertions_tokens = match &template_ast {
        Some(ast) => {
            let span = args
                .template
                .as_ref()
                .or(args.template_inline.as_ref())
                .map(|literal| literal.span())
                .unwrap_or_else(proc_macro2::Span::call_site);
            template_paths::emit_dynamic_component_selection_assertions(
                ast,
                &struct_ident,
                &field_idents,
                span,
            )
        }
        None => proc_macro2::TokenStream::new(),
    };

    // RFC 054 — compile row plans for eligible keyed `pp-for`
    // templates and stamp the source with `data-pp-row-plan`
    // anchors so the runtime can match the directive call back
    // to its plan. When no plan is emitted the runtime takes
    // the existing generic mount path.
    let row_plans = template_ast
        .as_ref()
        .map(for_plan::analyze_row_plans)
        .unwrap_or_else(|| for_plan::EmittedRowPlans {
            array_tokens: quote! { &[] },
            has_plans: false,
            stamps: Vec::new(),
            assignments: Vec::new(),
        });

    // RFC-058 §6.2 layering (resolved) — template-plan + row-plan
    // compilers coexist on one template. The row-plan analyser
    // hands its `(template_node_path, plan_id)` assignments to
    // the template-plan classifier, which bakes
    // `data-pp-row-plan="<id>"` directly into the cleaned HTML
    // during serialization. The byte-position stamps in
    // `row_plans.stamps` then become redundant — used only as
    // the fallback when the template-plan classifier didn't run
    // (no eligible directive anywhere AND no row plans either).
    let template_plan = template_ast
        .as_ref()
        .map(|ast| {
            template_plan::analyze_template_plan(
                ast,
                &row_plans.assignments,
                role_for_template_plan.clone(),
                &name_str,
            )
        })
        .unwrap_or_else(|| template_plan::EmittedTemplatePlan {
            plan_tokens: None,
            cleaned_html: None,
            slot_fragment_fns: proc_macro2::TokenStream::new(),
            if_body_fns: proc_macro2::TokenStream::new(),
            specialized_mount_body: None,
            diagnostics: proc_macro2::TokenStream::new(),
            ref_names: Vec::new(),
        });

    // Build the literal to feed into `compile_template`:
    //
    //   * Template plan present  → cleaned HTML with classified
    //     attributes stripped.
    //   * Else row stamps present → original source with
    //     `data-pp-row-plan="<id>"` spliced into each
    //     `<template pp-for>` opening tag.
    //   * Else raw `include_str!` (the existing pre-RFC-058
    //     path).
    //
    // We keep the `const _: &str = include_str!(...)` dependency
    // pin in `register_template_stmt` so cargo still rebuilds
    // when the `.poco` changes.
    let template_source_for_compile = if let Some(cleaned) = template_plan.cleaned_html.as_deref() {
        cleaned.to_string()
    } else if let Some(inline) = args.template_inline.as_ref() {
        let source = template_ast
            .as_ref()
            .map(|ast| ast.source.as_str())
            .unwrap_or("");
        if !source.is_empty() {
            apply_combined_template_edits(source, &row_plans.stamps, &slot_template_edits)
        } else {
            // No AST source — fall back to the raw inline. Row-plan
            // stamps and slot edits are AST-derived, so they can't
            // be applied to a string we never parsed.
            inline.value()
        }
    } else {
        let source = template_ast
            .as_ref()
            .map(|ast| ast.source.as_str())
            .unwrap_or("");
        apply_combined_template_edits(source, &row_plans.stamps, &slot_template_edits)
    };

    let compiled_template_html = compile_template_static(
        &template_source_for_compile,
        &name_str,
        role_for_static_template
            .as_ref()
            .map(|(tag, role_name)| (*tag, role_name.as_str())),
    );
    let compiled_template_html_lit = proc_macro2::Literal::string(&compiled_template_html);
    let vtable_template_html_tokens =
        vtable_template_html_tokens(&compiled_template_html_lit, template_ast.is_some());

    let template_literal_tokens = if let Some(cleaned) = template_plan.cleaned_html.as_deref() {
        let lit = proc_macro2::Literal::string(cleaned);
        quote! { #lit }
    } else if let Some(inline) = args.template_inline.as_ref() {
        // `template_inline` always carries the source verbatim;
        // the row-plan stamps and cleaned-HTML serialiser handle
        // the file-source path above, so reaching this branch
        // means the macro had nothing to rewrite.
        let source = template_ast
            .as_ref()
            .map(|ast| ast.source.as_str())
            .unwrap_or_else(|| {
                // template_ast is empty only when validation
                // failed in lenient mode — fall back to the raw
                // string so the registration still emits.
                ""
            });
        let value = if !source.is_empty() {
            source.to_string()
        } else {
            inline.value()
        };
        let lit = proc_macro2::Literal::string(&value);
        quote! { #lit }
    } else if row_plans.stamps.is_empty() {
        let path = template_path
            .as_ref()
            .expect("file-template path resolves when template_inline is absent");
        quote! { include_str!(#path) }
    } else {
        let source = template_ast
            .as_ref()
            .map(|ast| ast.source.as_str())
            .unwrap_or("");
        let stamped = for_plan::apply_stamps(source, &row_plans.stamps);
        let lit = proc_macro2::Literal::string(&stamped);
        quote! { #lit }
    };

    let row_plans_array_tokens = row_plans.array_tokens;
    let register_row_plans_stmt = if row_plans.has_plans {
        quote! {
            ::pocopine::__private::register_row_plans(
                #name_str,
                #row_plans_array_tokens,
            );
        }
    } else {
        quote! {}
    };

    let template_plan_module_ident = format_ident!("__poc_plan_{}", struct_ident);
    let template_plan_tokens_for_vtable = template_plan.plan_tokens.clone();
    let specialized_mount_body = template_plan.specialized_mount_body.clone();
    let template_plan_diagnostics_tokens = template_plan.diagnostics.clone();
    let template_plan_item_tokens = match template_plan.plan_tokens.clone() {
        Some(plan_tokens) => {
            let slot_fns = template_plan.slot_fragment_fns;
            let if_body_fns = template_plan.if_body_fns;
            quote! {
                #[doc(hidden)]
                #[allow(non_snake_case)]
                mod #template_plan_module_ident {
                    use super::*;

                    #slot_fns
                    #if_body_fns

                    pub const PLAN: ::pocopine::__private::StaticTemplatePlan = #plan_tokens;
                }
            }
        }
        None => quote! {},
    };

    let register_template_plan_stmt = match template_plan.plan_tokens {
        Some(_) => quote! {
            ::pocopine::__private::register_template_plan(
                #name_str,
                &#template_plan_module_ident::PLAN,
            );
        },
        None => quote! {},
    };

    // The `const _: &str = include_str!(...)` pin tells cargo to
    // re-run the macro when the `.poco` file changes. Inline
    // templates have no file to watch, so the pin is omitted —
    // the literal IS the source-of-truth for those.
    let template_rebuild_pin_tokens = match (&args.template_inline, &template_path) {
        (Some(_), _) => quote! {},
        (None, Some(path)) => quote! { const _: &str = include_str!(#path); },
        (None, None) => quote! {},
    };
    let register_template_stmt = quote! {
        #template_warnings
        #template_rebuild_pin_tokens
        ::pocopine::__private::register_template(
            #name_str,
            ::pocopine::__private::compile_template(
                #template_literal_tokens,
                #name_str,
                #role_arg,
            ),
        );
        #register_row_plans_stmt
        #register_template_plan_stmt
    };

    let register_style_stmt = match args.style.as_ref() {
        Some(style_path) => quote! {
            const _: &str = include_str!(#style_path);
            ::pocopine::__private::inject_style(
                #name_str,
                include_str!(#style_path),
            );
        },
        None => quote! {},
    };

    // `display = "<value>"` → inject `<custom-tag> { display: <value>; }`
    // at registration time. Lets primitives declare the outer
    // custom tag's layout display without authors repeating
    // `pine-foo { display: contents; }` across every demo
    // stylesheet. Any valid CSS display value works.
    //
    // When `display` is unset, layout-bearing roles (`panel`,
    // `scope`) default to `contents` so the wrapper elides its
    // layout box — see `role_default_display`. Other roles keep
    // the browser's custom-element default (`inline`).
    let effective_display: Option<String> =
        args.display.as_ref().map(|lit| lit.value()).or_else(|| {
            args.role
                .as_ref()
                .and_then(|lit| role_default_display(&lit.value()).map(|s| s.to_string()))
        });
    // Layout-class lint — if the root element carries a layout-
    // bearing utility class (`sticky`, `h-screen`, `min-h-*`,
    // `inset-*`) and the component didn't set `display` explicitly
    // *and* the role didn't default one, fail compilation with a
    // directive message. The custom element defaults to
    // `display: inline`, which silently breaks the offending
    // utility — see RFC docs/guides/poco/04-expressions.md companion.
    let layout_lint_tokens: proc_macro2::TokenStream = if effective_display.is_none() {
        template_ast
            .as_ref()
            .and_then(|ast| ast.element_roots().next())
            .and_then(|root| {
                root.attrs
                    .iter()
                    .find(|(k, _)| k == "class")
                    .map(|(_, v)| v.clone())
            })
            .and_then(|classes| {
                classes
                    .split_whitespace()
                    .find(|cls| {
                        *cls == "sticky"
                            || *cls == "h-screen"
                            || cls.starts_with("min-h-")
                            || cls.starts_with("inset-")
                    })
                    .map(|cls| cls.to_string())
            })
            .map(|trigger| {
                let msg = format!(
                    "pocopine: component `{name_str}` has root class `{trigger}` but no \
                         `display` set. A bare custom element defaults to `display: inline`, \
                         which silently breaks `{trigger}` (the containing block becomes a \
                         tiny inline box). Set `display = \"contents\"` to elide the \
                         custom-element layout box, or pick a role that defaults it \
                         (role = \"panel\" / \"scope\")."
                );
                quote! { ::core::compile_error!(#msg); }
            })
            .unwrap_or_default()
    } else {
        proc_macro2::TokenStream::new()
    };

    let register_display_stmt: proc_macro2::TokenStream = match effective_display {
        Some(value) => {
            let css = format!("{name_str} {{ display: {value}; }}");
            let sentinel = format!("{name_str}-display");
            quote! {
                ::pocopine::__private::inject_style(#sentinel, #css);
            }
        }
        None => quote! {},
    };

    let has_template_plan = template_plan_tokens_for_vtable.is_some();
    let (template_plan_assoc_const_tokens, vtable_plan_tokens) =
        match template_plan_tokens_for_vtable {
            Some(_) => (
                quote! {
                    #[doc(hidden)]
                    pub const __POC_TEMPLATE_PLAN: &'static ::pocopine::__private::StaticTemplatePlan =
                        &#template_plan_module_ident::PLAN;
                },
                quote! { Some(<#struct_ident>::__POC_TEMPLATE_PLAN) },
            ),
            None => (quote! {}, quote! { None }),
        };
    let mount_template_body_tokens = if has_template_plan {
        match specialized_mount_body {
            Some(specialized) => {
                quote! {
                    let __poc_template_name = #name_str;
                    #specialized
                }
            }
            None => quote! {
                ::core::compile_error!(
                    "pocopine internal error: template plan was emitted without a generated component mount body"
                );
            },
        }
    } else {
        quote! {}
    };

    // RFC 060 Tier 1 — emit a transitive `T::register()` call per
    // `uses = [...]` entry so the consumer's registration brings
    // every reachable component along with it. The cycle/dedupe
    // guard at the top of `register()` (see `mark_registered`)
    // makes this safe for cyclic graphs.
    let register_uses_stmts: proc_macro2::TokenStream = match args.uses.as_ref() {
        Some(table) => {
            let calls = table.entries.iter().map(|(_tag, type_path)| {
                quote! {
                    <#type_path as ::pocopine::__private::Component>::register();
                }
            });
            quote! { #(#calls)* }
        }
        None => quote! {},
    };

    // Dynamic-component selections are scoped to the host's typed `uses`
    // contract. `ComponentRef::of::<Child>()` infers the host from its typed
    // context and relies on the marker impl,
    // while the runtime-name escape hatch consults the associated allowlist.
    // Deduplicate by type path so an explicit alias cannot emit overlapping
    // marker impls for the same child type.
    let dynamic_component_contract_tokens = {
        let mut seen = ::std::collections::BTreeSet::new();
        let child_types: Vec<&Path> = args
            .uses
            .as_ref()
            .into_iter()
            .flat_map(|table| table.entries.iter().map(|(_tag, path)| path))
            .filter(|path| seen.insert(quote! { #path }.to_string()))
            .collect();
        quote! {
            impl ::pocopine::__private::DynamicComponentHost for #struct_ident {
                const DYNAMIC_COMPONENTS: &'static [&'static str] = &[
                    #(<#child_types as ::pocopine::__private::Component>::NAME,)*
                ];
            }

            #(
                impl ::pocopine::__private::ComponentUses<#child_types> for #struct_ident {}
            )*
        }
    };

    // Give each registration function a distinct name so multiple components
    // in one module don't trip the `pub fn register()` duplicate.
    let _register_fn = format_ident!("__pocopine_register_{}", struct_ident);

    // RFC-036 — `#[observe(KEY)]`. Emit two inherent
    // methods on the struct that #[handlers] calls from its
    // generated `setup()`. Bodies are empty when no field is
    // observed, so the call sites are cheap but unconditional.
    let observe_seed_stmts = observes.iter().map(|m| {
        let field_ident = &m.field_ident;
        let root_field_ident = syn::Ident::new(&m.field_name_on_root, field_ident.span());
        let key_path = &m.key_path;
        quote! {
            if let ::core::option::Option::Some(__root) =
                ::pocopine::inject(&#key_path)
            {
                __root.with(|__r| {
                    self.#field_ident = ::core::clone::Clone::clone(&__r.#root_field_ident);
                });
            }
        }
    });
    let observe_install_stmts = observes.iter().map(|m| {
        let field_ident = &m.field_ident;
        let field_ty = &m.field_ty;
        let field_name_on_root = &m.field_name_on_root;
        let key_path = &m.key_path;
        quote! {
            if let ::core::option::Option::Some(__root) =
                ::pocopine::inject(&#key_path)
            {
                let __scope = __root.scope_id();
                let __h = ::core::clone::Clone::clone(&__handle);
                ::pocopine::watch_scope_field_scoped::<#field_ty, _>(
                    __scope,
                    #field_name_on_root,
                    move |__v, _| {
                        let __v: #field_ty = ::core::clone::Clone::clone(__v);
                        ::pocopine::__private::with_write_origin(
                            ::pocopine::__private::WriteOrigin::ObserveMirror,
                            || __h.update(|__s| __s.#field_ident = __v),
                        );
                    },
                );
            }
        }
    });
    let observe_impl = quote! {
        impl #struct_ident {
            #[doc(hidden)]
            pub fn __pocopine_observe_seed(&mut self) {
                #(#observe_seed_stmts)*
            }
            #[doc(hidden)]
            pub fn __pocopine_observe_install(__handle: ::pocopine::Handle<Self>) {
                let _ = &__handle;
                #(#observe_install_stmts)*
            }
        }
    };

    // RFC 081 — generated `<ComponentName>Refs<'a>` struct.
    // One method per `pp-ref="name"` in the template. Each
    // returns a typed `RefAccessor` carrying (scope_id, name)
    // so callers reach `.element()` / `.as_::<T>()` /
    // `.component::<T>()` with the name fixed at codegen time
    // (typo → unknown method, fails to build).
    //
    // Wired as a `From<LifecycleContext<'a>>` extractor so
    // handlers take the typed struct directly:
    //
    //     fn on_ready(&self, refs: KeepNoteFormRefs) {
    //         let body = refs.body().component::<KeepNoteBody>()?;
    //     }
    //
    // Always emitted (even for components with zero pp-refs)
    // for two reasons: keeps the From extractor extractor-name
    // discoverable, and the empty struct is dead-code-eliminable
    // when no handler consumes it.
    let refs_struct_ident =
        proc_macro2::Ident::new(&format!("{ident_str}Refs"), struct_ident.span());
    // RFC 081 Phase 2 Codex-P2 fix — `ref_names` is dedup'd
    // by RAW `pp-ref` string in the template-plan analyser,
    // but `normalize_ref_method_ident` maps `body-main`,
    // `body.main`, and `body_main` all to the same Rust
    // ident `body_main`. Emitting duplicate inherent methods
    // would fail to compile with a confusing E0592. Instead,
    // group raw names by their normalized form and emit a
    // typed `compile_error!` per collision so the author
    // sees exactly which template names clashed.
    let mut method_name_groups: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for name in &template_plan.ref_names {
        method_name_groups
            .entry(normalize_ref_method_ident(name))
            .or_default()
            .push(name.clone());
    }
    let ref_collision_errors: Vec<TokenStream2> = method_name_groups
        .iter()
        .filter(|(_, raw)| raw.len() > 1)
        .map(|(method_name, raw_names)| {
            let msg = format!(
                "RFC 081: `pp-ref` names {raw:?} all normalize to the Rust method \
                 identifier `{method_name}` for the generated `{ident_str}Refs` accessor. \
                 Rename one of the templates' `pp-ref=\"...\"` attributes so each \
                 normalizes to a distinct method name (kebab/dot/colon → underscore).",
                raw = raw_names,
            );
            let lit = proc_macro2::Literal::string(&msg);
            quote! {
                ::core::compile_error!(#lit);
            }
        })
        .collect();
    let ref_accessor_methods = method_name_groups.iter().map(|(method_name, raw_names)| {
        let method_ident = proc_macro2::Ident::new(method_name, struct_ident.span());
        // Use the first raw name as the runtime lookup key
        // — when the collision diagnostic above fires, the
        // build won't reach mount anyway; when there's no
        // collision the group has exactly one entry.
        let name_lit = proc_macro2::Literal::string(&raw_names[0]);
        let doc_lit = proc_macro2::Literal::string(&raw_names[0]);
        quote! {
            /// Typed accessor for the
            #[doc = #doc_lit]
            /// `pp-ref`. Returns a [`::pocopine::__private::RefAccessor`]
            /// carrying the scope id + ref name baked in; call
            /// `.element()`, `.as_::<T>()`, or `.component::<T>()`
            /// to resolve.
            pub fn #method_ident(&self) -> ::pocopine::__private::RefAccessor {
                ::pocopine::__private::RefAccessor::__new(self.scope_id, #name_lit)
            }
        }
    });
    let refs_struct_tokens = quote! {
        /// Macro-generated typed refs struct (RFC 081 Phase 2).
        /// One method per `pp-ref="name"` in the component's
        /// template. Reach via the `From<LifecycleContext>`
        /// extractor: write
        /// `fn on_ready(&self, refs: #refs_struct_ident)` and
        /// call `refs.<name>().component::<T>()` etc.
        #[doc(hidden)]
        #[derive(Clone, Copy)]
        pub struct #refs_struct_ident<'__poc_refs> {
            scope_id: ::pocopine::__private::ScopeId,
            _m: ::core::marker::PhantomData<&'__poc_refs ()>,
        }

        // RFC 081 Phase 2 Codex-P2 — collision diagnostics
        // emitted at module scope so each clashing pair of
        // `pp-ref` names produces one clear error.
        #(#ref_collision_errors)*

        impl<'__poc_refs> #refs_struct_ident<'__poc_refs> {
            #(#ref_accessor_methods)*
        }

        impl<'__poc_refs> ::core::convert::From<
            ::pocopine::__private::LifecycleContext<'__poc_refs>,
        > for #refs_struct_ident<'__poc_refs> {
            fn from(
                ctx: ::pocopine::__private::LifecycleContext<'__poc_refs>,
            ) -> Self {
                Self {
                    scope_id: ctx.scope_id,
                    _m: ::core::marker::PhantomData,
                }
            }
        }
    };

    // RFC-097 §3.2 — the `<Name>Fields` extension trait of typed
    // single-field handles, impl'd for `Handle<Name>`.
    let field_handles_tokens = field_handles_tokens(
        &struct_ident,
        &ident_str,
        &field_idents,
        &field_names,
        &field_types,
        &field_is_serde_skip,
    );

    let out = quote! {
        #input
        #template_plan_item_tokens
        #refs_struct_tokens
        #field_handles_tokens

        // RFC 049 — marker traits + blanket impls for each
        // #[slot(accepts=...)] / #[slot(only=...)] declared on
        // this struct. Empty when no #[slot] declared or all
        // slots use the bare `#[slot(default)]` form.
        #slot_traits_tokens

        // RFC 049 — consumer-side slot-contract assertions:
        // one `const _: fn() = || assert_child::<...>();` per
        // (parent-tag, child-tag) pair resolved against the
        // consumer's `uses = [...]` list. Empty when the
        // consumer didn't opt in or the template has no
        // recognised typed parents.
        #slot_assertions_tokens

        // RFC 060 Tier 2 — hard `compile_error!` for every
        // custom-element tag in the template that the consumer's
        // `uses = [...]` doesn't cover. Opt-in: only fires when
        // `uses` is declared.
        #unknown_tag_diagnostics_tokens

        // Template-plan diagnostics are emitted independently from the plan
        // module: an invalid template may collect only diagnostics and emit no
        // runtime plan at all.
        #template_plan_diagnostics_tokens

        // RFC 063 — hard `compile_error!` for every directive
        // RFC 063 §4.1 deletes (`pp-cloak` today; more in
        // follow-up commits).
        #forbidden_directive_diagnostics_tokens

        // Issue #76 — hard `compile_error!` for `<template pp-for>`
        // rows whose direct element child is another `<template>`.
        #pp_for_template_child_diagnostics_tokens

        // RFC 084 — typed slot props validation. `const _: ()`
        // blocks enforce that `#[slot(name = "...", props = T)]`'s
        // publication keys match `T`'s `#[prop]` field set.
        #slot_props_validation_tokens

        // Template-path validation — field markers + one
        // `const _: fn() = <T>::__poc_bindable_<root>;` per
        // template expression root (handler roots resolve
        // against `#[handlers]`-emitted markers).
        #template_path_marker_tokens
        #template_path_assertions_tokens
        #dynamic_component_selection_assertions_tokens

        // Layout-class lint — hard `compile_error!` when the root
        // carries `sticky` / `h-screen` / `min-h-*` / `inset-*` but
        // the component doesn't declare a `display` (and the role
        // didn't default one). The custom element's default
        // `display: inline` silently breaks those utilities.
        #layout_lint_tokens

        #observe_impl

        impl ::pocopine::__private::ComponentState for #struct_ident {
            fn get(&self, key: &str) -> ::pocopine::__private::JsValue {
                match key {
                    #(#get_arms)*
                    _ => {
                        #(#bare_flatten_get_lookups)*
                        <Self as ::pocopine::__private::HandlerDispatch>::computed_get(self, key)
                            .unwrap_or(::pocopine::__private::JsValue::UNDEFINED)
                    }
                }
            }
            fn set(&mut self, key: &str, value: ::pocopine::__private::JsValue) {
                match key {
                    #(#set_arms)*
                    _ => {
                        #(#bare_flatten_set_lookups)*
                        let _ = value;
                    }
                }
            }
            fn is_computed_field(&self, key: &str) -> bool {
                <Self as ::pocopine::__private::HandlerDispatch>::computed_keys()
                    .contains(&key)
            }
            fn keys(&self) -> &'static [&'static str] {
                static __POCOPINE_KEYS: ::std::sync::OnceLock<&'static [&'static str]> =
                    ::std::sync::OnceLock::new();
                *__POCOPINE_KEYS.get_or_init(|| {
                    let mut __keys = vec![#(#keys_arr),*];
                    __keys.extend_from_slice(
                        <Self as ::pocopine::__private::HandlerDispatch>::computed_keys(),
                    );
                    #(#bare_flatten_keys_extend)*
                    // RFC-044 §5.10.4 — one-time bare-flatten leaf
                    // collision audit. No-op when the component has
                    // no bare flatten fields.
                    #bare_flatten_collision_audit
                    let __boxed: ::std::boxed::Box<[&'static str]> = __keys.into_boxed_slice();
                    ::std::boxed::Box::leak(__boxed)
                })
            }
            fn field_fingerprint(&self, key: &str) -> ::core::option::Option<u64> {
                match key {
                    #(#fingerprint_arms)*
                    _ => ::core::option::Option::None,
                }
            }
            fn field_quick_len(&self, key: &str) -> ::core::option::Option<u64> {
                match key {
                    #(#quick_len_arms)*
                    _ => ::core::option::Option::None,
                }
            }
            fn field_as_text(&self, key: &str) -> ::core::option::Option<::std::string::String> {
                match key {
                    #(#text_arms)*
                    _ => ::core::option::Option::None,
                }
            }
            fn is_prop(&self, key: &str) -> bool {
                #is_prop_body
            }
            fn static_prop_kind(&self, key: &str) -> ::pocopine::__private::StaticPropKind {
                #static_prop_kind_body
            }
            fn flatten_container_of(
                &self,
                key: &str,
            ) -> ::core::option::Option<&'static str> {
                match key {
                    #(#flatten_container_arms)*
                    _ => {
                        // RFC-044 §5.10.4 — a real struct field that
                        // happens to share a bare-flatten leaf's name
                        // must not falsely report a container (which
                        // would trigger `#[watch(<container>)]` on a
                        // write that only touched the field).
                        if #static_known_field_names_check {
                            return ::core::option::Option::None;
                        }
                        #(#bare_flatten_container_lookups)*
                        ::core::option::Option::None
                    }
                }
            }
            fn is_model(&self, key: &str) -> bool {
                #is_model_body
            }
            fn model_name(&self, key: &str) -> ::core::option::Option<&'static str> {
                match key {
                    #(#model_name_arms)*
                    _ => {
                        // RFC-044 §5.10.4 — gate the bare-model-flatten
                        // fallthrough so a real field's name can't
                        // accidentally be reported as a wire-emitted
                        // model leaf.
                        if #static_known_field_names_check {
                            return ::core::option::Option::None;
                        }
                        #(#bare_flatten_model_name_lookups)*
                        ::core::option::Option::None
                    }
                }
            }
            fn get_model_value(&self, key: &str) -> ::pocopine::__private::JsValue {
                match key {
                    #(#model_value_arms)*
                    _ => self.get(key),
                }
            }
            fn invoke(
                &mut self,
                key: &str,
                args: &::pocopine::__private::js_sys::Array,
            ) -> ::pocopine::__private::JsValue {
                <Self as ::pocopine::__private::HandlerDispatch>::invoke_handler(self, key, args)
            }
            // RFC-097 §3.3 — surface the `#[handlers]` receiver partition
            // to the runtime so `Scope::invoke` can skip the sweep for
            // `&self` handlers.
            fn is_readonly_handler(&self, key: &str) -> bool {
                <Self as ::pocopine::__private::HandlerDispatch>::is_readonly_handler(self, key)
            }
            fn setup(
                &mut self,
                ctx: ::pocopine::__private::LifecycleContext<'_>,
            ) {
                <Self as ::pocopine::__private::HandlerDispatch>::setup(self, ctx);
            }
            fn mount(
                &mut self,
                ctx: ::pocopine::__private::LifecycleContext<'_>,
            ) {
                <Self as ::pocopine::__private::HandlerDispatch>::mount(self, ctx);
            }
            fn on_ready(
                &self,
                ctx: ::pocopine::__private::LifecycleContext<'_>,
            ) {
                <Self as ::pocopine::__private::HandlerDispatch>::on_ready(self, ctx);
            }
            fn unmount(
                &mut self,
                ctx: ::pocopine::__private::LifecycleContext<'_>,
            ) {
                <Self as ::pocopine::__private::HandlerDispatch>::unmount(self, ctx);
            }
            fn has_setup(&self) -> bool {
                <Self as ::pocopine::__private::HandlerDispatch>::has_setup(self)
            }
            fn has_on_mount(&self) -> bool {
                <Self as ::pocopine::__private::HandlerDispatch>::has_on_mount(self)
            }
            fn has_on_ready(&self) -> bool {
                <Self as ::pocopine::__private::HandlerDispatch>::has_on_ready(self)
            }
            fn transition_in_preset(&self) -> &'static str {
                #transition_in_literal
            }
            fn transition_out_preset(&self) -> &'static str {
                #transition_out_literal
            }
            fn animate_kind(&self) -> &'static str {
                #animate_literal
            }
            fn type_name(&self) -> &'static str {
                #name_str
            }
        }

        impl #struct_ident {
            #template_plan_assoc_const_tokens

            /// RFC 062 Phase 1 — generated mount-specialization
            /// entry. This first-phase body is the generic plan
            /// applier shim; later phases replace it with unrolled
            /// per-component DOM operations.
            #[doc(hidden)]
            pub fn __pocopine_mount_template(
                root: &::pocopine::__private::web_sys::Element,
                scope_id: ::pocopine::ScopeId,
                proxy: &::pocopine::__private::JsValue,
            ) {
                #mount_template_body_tokens
            }

            /// Register this component (template, stylesheet, constructor)
            /// with the pocopine runtime. Idempotent. Call directly or via
            /// [`pocopine::App::register`].
            pub fn register() {
                if !::pocopine::__private::mark_registered::<#struct_ident>() {
                    return;
                }
                ::pocopine::__private::register_component_with_mount(
                    #name_str,
                    concat!(module_path!(), "::", stringify!(#struct_ident)),
                    || {
                        let instance: ::std::rc::Rc<::std::cell::RefCell<#struct_ident>> =
                            ::std::rc::Rc::new(::std::cell::RefCell::new(
                                <#struct_ident as ::core::default::Default>::default()
                            ));
                        ::pocopine::__private::Scope::new(instance)
                    },
                    ::core::option::Option::Some(
                        <#struct_ident as ::pocopine::__private::Component>::mount_template,
                    ),
                );
                #register_template_stmt
                #register_style_stmt
                #register_display_stmt
                #register_uses_stmts
            }

            // RFC 060 Tier 4 — `&'static ComponentVTable` in
            // `.rodata`. Consumed by the `app!{}` macro's
            // `phf::Map` literal (vtable per component, keyed
            // by NAME).
            #[doc(hidden)]
            #[allow(non_upper_case_globals)]
            pub const __POCO_VTABLE: &'static ::pocopine::__private::ComponentVTable =
                &::pocopine::__private::ComponentVTable {
                    name: #name_str,
                    register: <#struct_ident>::register,
                    template_html: #vtable_template_html_tokens,
                    plan: #vtable_plan_tokens,
                    mount_template: ::core::option::Option::Some(
                        <#struct_ident as ::pocopine::__private::Component>::mount_template,
                    ),
                };
        }

        impl ::pocopine::__private::Component for #struct_ident {
            const NAME: &'static str = #name_str;
            fn register() {
                <#struct_ident>::register();
            }
            fn mount_template(
                root: &::pocopine::__private::web_sys::Element,
                scope_id: ::pocopine::ScopeId,
                proxy: &::pocopine::__private::JsValue,
            ) {
                <#struct_ident>::__pocopine_mount_template(root, scope_id, proxy);
            }
        }

        #dynamic_component_contract_tokens
    };

    out.into()
}

#[proc_macro_attribute]
pub fn handlers(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemImpl);
    let ty = input.self_ty.clone();
    let handlers_type_ident = match ty.as_ref() {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.clone()),
        _ => None,
    };

    // Bake a host-side `allow(dead_code)` into the user's impl block.
    // Pine primitives that wrap `web_sys` routinely keep helper fns
    // (validate, mint_id, accept_allows, …) that are only reached
    // from `#[cfg(target_arch = "wasm32")]` handlers; on the host
    // build those helpers look unreferenced and trip CI's
    // `-D warnings`. The inner attribute applied here covers every
    // item the user puts in the same `#[handlers] impl` block.
    input.attrs.push(syn::parse_quote!(
        #![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    ));

    let mut arms = Vec::new();
    // Template-path validation — every name that gets an invoke
    // arm also gets a `__poc_handler_<name>` marker so the
    // `#[component]`-emitted references resolve. BTreeSet: cfg-
    // split methods with the same name must not emit duplicate
    // marker items.
    let mut handler_marker_names: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    // RFC-097 §3.3 — names of `&self` (read-only) handlers; the runtime
    // skips the dirty sweep for these.
    let mut readonly_names: Vec<String> = Vec::new();
    let mut has_on_setup = false;
    let mut has_on_mount = false;
    let mut has_on_ready = false;
    let mut has_on_unmount = false;
    // RFC-032 — number of extractor params past &self / &mut self.
    // Drives how many `__ctx.into()` calls the generated forwarder
    // passes to the user's `on_mount` / `on_ready`.
    let mut on_setup_extractor_count: usize = 0;
    let mut on_mount_extractor_count: usize = 0;
    let mut on_ready_extractor_count: usize = 0;
    let mut on_unmount_extractor_count: usize = 0;
    // (method_ident, field_ident, value_type) for each `#[watch(f)]`
    // method. The macro auto-generates an `on_ready` that wires a
    // `watch_field` per entry.
    let mut watches: Vec<(syn::Ident, syn::Ident, syn::Type)> = Vec::new();
    let mut computed_methods: Vec<ComputedMethod> = Vec::new();

    // First pass: collect watch metadata while the `#[watch(...)]`
    // attribute is still on each method. Strip the attribute in the
    // same loop so the compiler doesn't see an unknown attr on the
    // rewritten output.
    let mut methods_to_skip_in_arms: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for impl_item in input.items.iter_mut() {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let mut watch_field: Option<syn::Ident> = None;
        let mut is_computed = false;
        method.attrs.retain(|attr| {
            if attr.path().is_ident("watch") {
                if let Ok(ident) = attr.parse_args::<syn::Ident>() {
                    watch_field = Some(ident);
                }
                false // strip
            } else if attr.path().is_ident("computed") {
                is_computed = true;
                false
            } else {
                true
            }
        });
        if let Some(field_ident) = watch_field {
            // Extract V from the method's first typed arg.
            let v_ty = method.sig.inputs.iter().find_map(|arg| match arg {
                FnArg::Typed(PatType { ty, .. }) => Some((**ty).clone()),
                _ => None,
            });
            let Some(v_ty) = v_ty else { continue };
            methods_to_skip_in_arms.insert(method.sig.ident.to_string());
            watches.push((method.sig.ident.clone(), field_ident, v_ty));
        }
        if is_computed {
            if method.sig.receiver().is_some() {
                return syn::Error::new_spanned(
                    &method.sig.ident,
                    "#[computed] methods must not take self; declare dependencies as parameters",
                )
                .to_compile_error()
                .into();
            }
            let ret_ty = match &method.sig.output {
                syn::ReturnType::Type(_, ty) => (**ty).clone(),
                syn::ReturnType::Default => {
                    return syn::Error::new_spanned(
                        &method.sig.ident,
                        "#[computed] methods must declare a return type",
                    )
                    .to_compile_error()
                    .into();
                }
            };
            methods_to_skip_in_arms.insert(method.sig.ident.to_string());
            computed_methods.push(ComputedMethod {
                method_ident: method.sig.ident.clone(),
                field_name: method
                    .sig
                    .ident
                    .to_string()
                    .trim_start_matches("r#")
                    .to_string(),
                ret_ty,
                params: Vec::new(),
            });
        }
    }

    let computed_names: std::collections::HashSet<String> = computed_methods
        .iter()
        .map(|entry| entry.field_name.clone())
        .collect();

    for entry in computed_methods.iter_mut() {
        let method = input.items.iter().find_map(|item| match item {
            ImplItem::Fn(method) if method.sig.ident == entry.method_ident => Some(method),
            _ => None,
        });
        let Some(method) = method else {
            continue;
        };
        for arg in method.sig.inputs.iter() {
            let FnArg::Typed(PatType { pat, ty, .. }) = arg else {
                continue;
            };
            let Pat::Ident(pat_ident) = pat.as_ref() else {
                return syn::Error::new_spanned(
                    pat,
                    "#[computed] parameters must be simple identifiers",
                )
                .to_compile_error()
                .into();
            };
            let dep_name = pat_ident
                .ident
                .to_string()
                .trim_start_matches("r#")
                .to_string();
            entry.params.push(ComputedParam {
                ident: pat_ident.ident.clone(),
                ty: (**ty).clone(),
                is_computed_dep: computed_names.contains(&dep_name),
            });
        }
    }
    for entry in &computed_methods {
        for param in entry.params.iter().filter(|param| param.is_computed_dep) {
            if matches!(param.ty, Type::Reference(_)) {
                return syn::Error::new_spanned(
                    &param.ident,
                    "#[computed] cannot borrow another computed value by reference in v1",
                )
                .to_compile_error()
                .into();
            }
        }
    }

    for item in &input.items {
        let ImplItem::Fn(method) = item else { continue };
        let Some(receiver) = method.sig.receiver() else {
            continue;
        };
        let ident = method.sig.ident.clone();
        let name = ident.to_string();
        let extractor_count = || {
            method
                .sig
                .inputs
                .iter()
                .filter(|a| matches!(a, FnArg::Typed(_)))
                .count()
        };
        match name.as_str() {
            "on_setup" => {
                has_on_setup = true;
                on_setup_extractor_count = extractor_count();
                continue; // lifecycle; don't emit an invoke arm
            }
            "on_mount" => {
                has_on_mount = true;
                on_mount_extractor_count = extractor_count();
                continue; // lifecycle; don't emit an invoke arm
            }
            "on_ready" => {
                has_on_ready = true;
                on_ready_extractor_count = extractor_count();
                continue; // lifecycle; don't emit an invoke arm
            }
            "on_unmount" => {
                has_on_unmount = true;
                on_unmount_extractor_count = extractor_count();
                continue; // lifecycle; don't emit an invoke arm
            }
            _ => {}
        }

        // `#[watch(field)]`-decorated methods are called by the
        // auto-generated on_ready, never as a named handler.
        if methods_to_skip_in_arms.contains(&name) {
            continue;
        }

        // Collect typed arg positions after `&mut self`. Per RFC-008,
        // each arg's type must implement `FromHandlerArg`; the macro
        // emits the per-arg conversion call.
        let typed_args: Vec<(syn::Ident, syn::Type)> = method
            .sig
            .inputs
            .iter()
            .enumerate()
            .filter_map(|(i, arg)| match arg {
                FnArg::Typed(PatType { ty, .. }) => {
                    let ident = format_ident!("_arg{}", i);
                    Some((ident, (**ty).clone()))
                }
                _ => None,
            })
            .collect();
        let conversions = typed_args.iter().enumerate().map(|(i, (bind, ty))| {
            let idx = i as u32;
            quote! {
                let #bind: #ty = match <#ty as ::pocopine::__private::FromHandlerArg>::from_handler_arg(
                    _args.get(#idx),
                ) {
                    Some(v) => v,
                    None => return ::pocopine::__private::JsValue::UNDEFINED,
                };
            }
        });
        let bindings = typed_args.iter().map(|(bind, _)| quote!(#bind));
        // Forward conditional-compilation attributes from the method
        // onto the emitted dispatch arm. Without this, a method
        // annotated `#[cfg(target_arch = "wasm32")]` would compile
        // away on host while the arm — which still references the
        // method and its arg types — would not, breaking the host
        // build. Restricted to `cfg`/`cfg_attr` so unrelated attrs
        // (e.g. `#[doc]`, `#[allow]`) don't leak onto the arm.
        let cfg_attrs: Vec<&syn::Attribute> = method
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr"))
            .collect();
        arms.push(quote! {
            #(#cfg_attrs)*
            #name => {
                #(#conversions)*
                Self::#ident(self #(, #bindings)*);
                ::pocopine::__private::JsValue::UNDEFINED
            }
        });
        // Markers are emitted cfg-unconditionally on purpose:
        // the component-side reference must resolve on host AND
        // wasm even when the method itself is cfg-gated.
        handler_marker_names.insert(name.clone());
        // RFC-097 §3.3 — `&self` (immutable reference, no `mut`) is a
        // read-only handler. By-value `self` and `&mut self` stay swept.
        if receiver.reference.is_some() && receiver.mutability.is_none() {
            readonly_names.push(name.clone());
        }
    }

    // Always wrap setup with the observe seed + install calls the
    // `#[component]` macro emitted as inherent methods. The bodies
    // are no-ops when the struct has no `#[observe(KEY)]`
    // fields, so the overhead is one call + a `this::<Self>()`
    // lookup — negligible compared to the setup invocation itself.
    // User's `on_setup` (when declared) runs after observes so
    // author code sees observed fields already populated.
    let setup_extractor_args = (0..on_setup_extractor_count).map(|_| {
        quote! { __ctx.into() }
    });
    let user_on_setup_call = has_on_setup.then(|| {
        quote! { Self::on_setup(self #(, #setup_extractor_args)*); }
    });
    let setup_impl = Some(quote! {
        fn setup(
            &mut self,
            __ctx: ::pocopine::__private::LifecycleContext<'_>,
        ) {
            let _ = &__ctx;
            <Self>::__pocopine_observe_seed(self);
            let __state_ptr = ::pocopine::__private::component_computed::state_ptr(self);
            let __me = ::pocopine::this::<Self>();
            #user_on_setup_call
            <Self>::__pocopine_computed_install(__me.scope_id(), __state_ptr, __me.clone());
            <Self>::__pocopine_observe_install(__me);
        }
        fn has_setup(&self) -> bool { true }
    });
    // RFC-032: forward `__ctx.into()` for each extractor the user
    // declared on `on_mount`. Zero-arg signature just calls
    // through and ignores the ctx.
    let mount_extractor_args = (0..on_mount_extractor_count).map(|_| {
        quote! { __ctx.into() }
    });
    let mount_impl = has_on_mount.then(|| {
        quote! {
            fn mount(
                &mut self,
                __ctx: ::pocopine::__private::LifecycleContext<'_>,
            ) {
                let _ = &__ctx;
                Self::on_mount(self #(, #mount_extractor_args)*);
            }
            fn has_on_mount(&self) -> bool { true }
        }
    });
    // Build the list of watch_field registration statements for the
    // auto-generated on_ready. Each `#[watch(field)]` method
    // becomes:
    //
    //   let __scope = current_scope_id().expect(…);
    //   pocopine::watch_field::<V, _>("field", move |new, prev| {
    //       let new_v = new.clone();
    //       let prev_v = prev.cloned();
    //       if let Some(scope) = pocopine::Scope::find(__scope) {
    //           if let Some(inner) = scope.typed::<Self>() {
    //               pocopine::Handle::new(inner, __scope)
    //                   .update(|s| s.<method>(new_v, prev_v));
    //           }
    //       }
    //   });
    //
    // `Handle::new` + `update` acquires a fresh mutable borrow via
    // the captured scope id. This sidesteps two things at once:
    // (1) the &self / &mut self mismatch between on_ready and the
    // decorated method, and (2) the fact that `this::<Self>()`
    // depends on the thread-local `CURRENT_SCOPE_ID`, which isn't
    // set during most watch callback re-runs (triggers come from
    // the parent's effect chain, not a fresh `Scope::invoke`).
    let watch_installs = watches.iter().map(|(method_ident, field_ident, v_ty)| {
        let field_name = field_ident.to_string();
        let ty = ty.clone();
        quote! {
            {
                let __scope = ::pocopine::current_scope_id()
                    .expect("watch_field installed outside a lifecycle context");
                let __watch_initial_pending =
                    ::std::rc::Rc::new(::std::cell::Cell::new(true));
                let __watch_initial_ticket =
                    ::std::rc::Rc::new(::std::cell::Cell::new(0_u64));
                let __watch_effect = ::pocopine::watch_scope_field_now::<#v_ty, _>(__scope, #field_name, move |new, prev| {
                    let new_v: #v_ty = new.clone();
                    let prev_v: ::core::option::Option<#v_ty> = prev.cloned();
                    if __watch_initial_pending.get() {
                        let __ticket = __watch_initial_ticket.get() + 1;
                        __watch_initial_ticket.set(__ticket);
                        let __pending = __watch_initial_pending.clone();
                        let __tickets = __watch_initial_ticket.clone();
                        ::pocopine::tick::next(move || {
                            if !__pending.get() || __tickets.get() != __ticket {
                                return;
                            }
                            __pending.set(false);
                            if let Some(scope) = ::pocopine::Scope::find(__scope) {
                                if let Some(inner) = scope.typed::<#ty>() {
                                    ::pocopine::Handle::new(inner, __scope)
                                        .update(|s| {
                                            s.#method_ident(new_v, ::core::option::Option::None);
                                        });
                                }
                            }
                        });
                        return;
                    }
                    if let Some(scope) = ::pocopine::Scope::find(__scope) {
                        if let Some(inner) = scope.typed::<#ty>() {
                            ::pocopine::Handle::new(inner, __scope)
                                .update(|s| {
                                    s.#method_ident(new_v, prev_v);
                                });
                        }
                    }
                });
                ::pocopine::on_scope_unmount_for(__scope, move || {
                    ::pocopine::release(__watch_effect);
                });
            }
        }
    });
    let has_watches = !watches.is_empty();
    let has_computed = !computed_methods.is_empty();

    let mut computed_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (idx, entry) in computed_methods.iter().enumerate() {
        computed_index.insert(entry.field_name.clone(), idx);
    }
    let mut indegree = vec![0_usize; computed_methods.len()];
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); computed_methods.len()];
    for (idx, entry) in computed_methods.iter().enumerate() {
        for param in entry.params.iter().filter(|param| param.is_computed_dep) {
            let dep_name = param.ident.to_string().trim_start_matches("r#").to_string();
            if let Some(dep_idx) = computed_index.get(&dep_name).copied() {
                indegree[idx] += 1;
                edges[dep_idx].push(idx);
            }
        }
    }
    let mut ready: Vec<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(idx, degree)| (*degree == 0).then_some(idx))
        .collect();
    let mut topo = Vec::with_capacity(computed_methods.len());
    while let Some(idx) = ready.pop() {
        topo.push(idx);
        for next in &edges[idx] {
            indegree[*next] -= 1;
            if indegree[*next] == 0 {
                ready.push(*next);
            }
        }
    }
    if topo.len() != computed_methods.len() {
        let offender = computed_methods
            .iter()
            .enumerate()
            .find_map(|(idx, entry)| (indegree[idx] > 0).then_some(entry.method_ident.clone()))
            .unwrap_or_else(|| format_ident!("computed"));
        return syn::Error::new_spanned(offender, "#[computed] dependency graph contains a cycle")
            .to_compile_error()
            .into();
    }

    let computed_install_stmts = topo.iter().map(|idx| {
        let entry = &computed_methods[*idx];
        let method_ident = &entry.method_ident;
        let field_name = &entry.field_name;
        let ret_ty = &entry.ret_ty;
        let arg_bindings: Vec<_> = entry
            .params
            .iter()
            .map(|param| {
                let ident = &param.ident;
                let ty = &param.ty;
                let dep_name = ident.to_string().trim_start_matches("r#").to_string();
                if param.is_computed_dep {
                    quote! {
                        let #ident: #ty = ::pocopine::__private::serde_wasm_bindgen::from_value(
                            ::pocopine::__private::component_computed::get(__scope, #dep_name)
                                .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
                        )
                        .expect("failed to deserialize a computed dependency");
                    }
                } else if matches!(ty, Type::Reference(_)) {
                    let dep_ident = ident;
                    quote! {
                        let #ident: #ty = &__s.#dep_ident;
                    }
                } else {
                    let dep_ident = ident;
                    quote! {
                        let #ident: #ty = ::core::clone::Clone::clone(&__s.#dep_ident);
                    }
                }
            })
            .collect();
        let track_fields = entry
            .params
            .iter()
            .filter(|param| !param.is_computed_dep)
            .map(|param| {
                let dep_name = param.ident.to_string().trim_start_matches("r#").to_string();
                quote! {
                    ::pocopine::__private::track(__scope, #dep_name);
                }
            });
        let args = entry.params.iter().map(|param| {
            let ident = &param.ident;
            quote! { #ident }
        });
        quote! {
            {
                let __computed_handle = __handle.clone();
                __entries.push((
                    #field_name,
                    ::std::rc::Rc::new(::pocopine::__private::runtime_computed(move || {
                        #(#track_fields)*
                        __computed_handle.with(|__s| {
                            #(#arg_bindings)*
                            let __out: #ret_ty = Self::#method_ident(#(#args),*);
                            let __js = ::pocopine::__private::serde_wasm_bindgen::to_value(&__out)
                                .unwrap_or(::pocopine::__private::JsValue::NULL);
                            if __js.is_undefined() {
                                ::pocopine::__private::JsValue::NULL
                            } else {
                                __js
                            }
                        })
                    })),
                ));
            }
        }
    });
    let computed_keys = computed_methods.iter().map(|entry| {
        let field_name = &entry.field_name;
        quote! { #field_name }
    });
    let computed_dispatch_impl = if has_computed {
        quote! {
            fn computed_keys() -> &'static [&'static str]
            where
                Self: Sized,
            {
                &[#(#computed_keys),*]
            }

            fn computed_get(
                &self,
                key: &str,
            ) -> ::core::option::Option<::pocopine::__private::JsValue> {
                let __ptr = ::pocopine::__private::component_computed::state_ptr(self);
                ::pocopine::__private::component_computed::get_for_state_ptr(__ptr, key)
            }
        }
    } else {
        quote! {}
    };
    let computed_type_markers: Vec<_> = handlers_type_ident
        .as_ref()
        .into_iter()
        .flat_map(|type_ident| {
            computed_methods.iter().map(move |entry| {
                let marker =
                    template_paths::computed_type_marker_ident(type_ident, &entry.field_name);
                // Every valid dynamic selection type is a path
                // (`ComponentRef<Host>` or `Option<ComponentRef<Host>>`). Use `()` for
                // other return shapes so unrelated reference/impl-trait
                // computed methods keep compiling, while selecting one still
                // fails the marker assertion.
                let output_ty = match &entry.ret_ty {
                    Type::Path(_) => entry.ret_ty.clone(),
                    _ => syn::parse_quote!(()),
                };
                quote! {
                    #[doc(hidden)]
                    #[allow(non_camel_case_types)]
                    struct #marker;

                    impl ::pocopine::__private::ComputedTypeWitness for #marker {
                        type Output = #output_ty;
                    }
                }
            })
        })
        .collect();
    let computed_impl = if has_computed {
        quote! {
            impl #ty {
                #[doc(hidden)]
                pub fn __pocopine_computed_install(
                    __scope: ::pocopine::ScopeId,
                    __state_ptr: usize,
                    __handle: ::pocopine::Handle<Self>,
                ) {
                    let mut __entries = ::std::vec::Vec::new();
                    #(#computed_install_stmts)*
                    ::pocopine::__private::component_computed::install(
                        __scope,
                        __state_ptr,
                        __entries,
                    );
                }
            }
        }
    } else {
        quote! {
            impl #ty {
                #[doc(hidden)]
                pub fn __pocopine_computed_install(
                    _scope: ::pocopine::ScopeId,
                    _state_ptr: usize,
                    _handle: ::pocopine::Handle<Self>,
                ) {
                }
            }
        }
    };

    // RFC-032 — same extractor-forwarding logic as mount. Zero-arg
    // user signature stays zero-arg; any extractor params become
    // `__ctx.into()` in the generated forwarder.
    let on_ready_extractor_args: Vec<_> = (0..on_ready_extractor_count)
        .map(|_| quote! { __ctx.into() })
        .collect();

    // User wrote their own `on_ready` explicitly: use it. If they
    // didn't but there's at least one `#[watch]`, generate an
    // on_ready that wires up every watch. If they wrote BOTH,
    // merge — user's body runs first, then watch setup.
    let on_ready_impl = if has_on_ready {
        if has_watches {
            quote! {
                fn on_ready(
                    &self,
                    __ctx: ::pocopine::__private::LifecycleContext<'_>,
                ) {
                    let _ = &__ctx;
                    Self::on_ready(self #(, #on_ready_extractor_args)*);
                    #(#watch_installs)*
                }
                fn has_on_ready(&self) -> bool { true }
            }
        } else {
            quote! {
                fn on_ready(
                    &self,
                    __ctx: ::pocopine::__private::LifecycleContext<'_>,
                ) {
                    let _ = &__ctx;
                    Self::on_ready(self #(, #on_ready_extractor_args)*);
                }
                fn has_on_ready(&self) -> bool { true }
            }
        }
    } else if has_watches {
        quote! {
            fn on_ready(
                &self,
                __ctx: ::pocopine::__private::LifecycleContext<'_>,
            ) {
                let _ = &__ctx;
                #(#watch_installs)*
            }
            fn has_on_ready(&self) -> bool { true }
        }
    } else {
        quote! {}
    };
    let unmount_extractor_args: Vec<_> = (0..on_unmount_extractor_count)
        .map(|_| quote! { __ctx.into() })
        .collect();
    let unmount_impl = has_on_unmount.then(|| {
        quote! {
            fn unmount(
                &mut self,
                __ctx: ::pocopine::__private::LifecycleContext<'_>,
            ) {
                let _ = &__ctx;
                Self::on_unmount(self #(, #unmount_extractor_args)*);
            }
        }
    });

    // RFC-097 §3.3 — override the read-only classifier only when there
    // is at least one `&self` handler; otherwise the default (`false`)
    // stands and every handler stays swept.
    let readonly_impl = if readonly_names.is_empty() {
        quote! {}
    } else {
        quote! {
            fn is_readonly_handler(&self, key: &str) -> bool {
                matches!(key, #(#readonly_names)|*)
            }
        }
    };

    // RFC-111 — template-path validation name lists: the two
    // consts the `#[component]`-emitted const-eval checks read
    // that this macro alone can see — `#[computed]` field names
    // and dispatchable handler names.
    let template_path_markers = {
        let computed_names: std::collections::BTreeSet<String> = computed_methods
            .iter()
            .map(|c| c.field_name.clone())
            .collect();
        let consts = template_paths::handlers_keys_consts(
            computed_names.into_iter(),
            handler_marker_names.into_iter(),
        );
        quote! {
            #[doc(hidden)]
            impl #ty {
                #consts
            }
        }
    };

    let out = quote! {
        #input

        #(#computed_type_markers)*

        #computed_impl

        #template_path_markers

        impl ::pocopine::__private::HandlerDispatch for #ty {
            fn invoke_handler(
                &mut self,
                key: &str,
                _args: &::pocopine::__private::js_sys::Array,
            ) -> ::pocopine::__private::JsValue {
                match key {
                    #(#arms)*
                    _ => ::pocopine::__private::JsValue::UNDEFINED,
                }
            }
            #readonly_impl
            #setup_impl
            #mount_impl
            #on_ready_impl
            #unmount_impl
            #computed_dispatch_impl
        }
    };

    out.into()
}

#[derive(Default)]
struct StoreArgs {
    name: Option<LitStr>,
}

impl Parse for StoreArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pairs: Punctuated<MetaNameValue, Token![,]> = Punctuated::parse_terminated(input)?;
        let mut args = StoreArgs::default();
        for kv in pairs {
            let lit = match kv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) => s,
                other => {
                    return Err(syn::Error::new_spanned(other, "expected a string literal"));
                }
            };
            if kv.path.is_ident("name") {
                args.name = Some(lit);
            } else {
                return Err(syn::Error::new_spanned(
                    kv.path,
                    "unknown key — expected: name",
                ));
            }
        }
        Ok(args)
    }
}

/// `#[store]` — declare a singleton store. Same shape as `#[component]`
/// (emits `ComponentState` + `HandlerDispatch` bridge), plus a per-type
/// `thread_local` holding the singleton, plus an `impl Store`. Unlike
/// `#[component]`, stores are not tied to a DOM element — they're
/// registered once via `App::store::<T>()` and accessed from templates
/// via `$store.<name>` and from Rust via `pocopine::store::<T>()`.
#[proc_macro_attribute]
pub fn store(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match StoreArgs::parse.parse(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemStruct);

    let struct_ident = input.ident.clone();
    let ident_str = struct_ident.to_string();
    let default_name = kebab_case(&ident_str);
    let name_str = args
        .name
        .as_ref()
        .map(|s| s.value())
        .unwrap_or(default_name);

    let field_idents: Vec<_> = input
        .fields
        .iter()
        .filter_map(|f| f.ident.clone())
        .collect();
    // `ident.to_string()` on a raw identifier (`r#type`) returns
    // the `r#` prefix. Callers never see it in HTML attributes,
    // so strip it so attribute-to-prop mapping matches the bare
    // name (`type`) as authors expect.
    let field_names: Vec<String> = field_idents
        .iter()
        .map(|i| i.to_string().trim_start_matches("r#").to_string())
        .collect();
    // RFC-097 — stores get the same `<Name>Fields` extension trait, so
    // collect field types + serde-skip (the store loop didn't need
    // either before).
    let field_types: Vec<Type> = input
        .fields
        .iter()
        .filter(|f| f.ident.is_some())
        .map(|f| f.ty.clone())
        .collect();
    let field_is_serde_skip: Vec<bool> = input
        .fields
        .iter()
        .filter(|f| f.ident.is_some())
        .map(|f| {
            let mut skip = false;
            for a in &f.attrs {
                if a.path().is_ident("serde") {
                    let _ = a.parse_nested_meta(|meta| {
                        if meta.path.is_ident("skip")
                            || meta.path.is_ident("skip_serializing")
                            || meta.path.is_ident("skip_deserializing")
                        {
                            skip = true;
                        }
                        if meta.input.peek(Token![=]) {
                            let _ = meta.value().and_then(|v| v.parse::<Expr>());
                        } else if meta.input.peek(syn::token::Paren) {
                            let _ = meta.parse_nested_meta(|_| Ok(()));
                        }
                        Ok(())
                    });
                }
            }
            skip
        })
        .collect();
    if let Some(err) = interior_mut_rejection(&field_idents, &field_types) {
        return quote! { #input #err }.into();
    }

    let get_arms = field_idents
        .iter()
        .zip(field_names.iter())
        .map(|(id, name)| {
            quote! {
                #name => ::pocopine::__private::serde_wasm_bindgen::to_value(&self.#id)
                    .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
            }
        });
    // RFC-095 W2 — stores are the heaviest `Handle::update` users;
    // per-field fingerprints let the dirty sweep trigger only the
    // fields an `update` closure actually moved.
    let fingerprint_arms = field_call_arms(
        &field_idents,
        &field_names,
        quote! { ::pocopine::__private::fingerprint_value },
    );
    // RFC-095 W2b — O(1) length probes for the dirty sweep's
    // collection short-circuit.
    let quick_len_arms = field_call_arms(
        &field_idents,
        &field_names,
        quote! { ::pocopine::__private::quick_len_value },
    );
    let set_arms = field_idents.iter().zip(field_names.iter()).map(|(id, name)| {
        quote! {
            #name => {
                let __value = value;
                if let Ok(v) = ::pocopine::__private::serde_wasm_bindgen::from_value(__value.clone()) {
                    self.#id = v;
                } else if __value.as_string().as_deref() == Some("") {
                    if let Ok(v) = ::pocopine::__private::serde_wasm_bindgen::from_value(
                        ::pocopine::__private::JsValue::NULL,
                    ) {
                        self.#id = v;
                    }
                }
            }
        }
    });
    let keys_arr = field_names.iter().map(|n| quote! { #n });

    // RFC-097 §3.2 — store field handles (`store::<Name>().field()`).
    let field_handles_tokens = field_handles_tokens(
        &struct_ident,
        &ident_str,
        &field_idents,
        &field_names,
        &field_types,
        &field_is_serde_skip,
    );

    let out = quote! {
        #input
        #field_handles_tokens

        impl #struct_ident {
            // RFC-036 — stores don't support `#[observe(KEY)]`
            // (there's no parent context / inject chain), but
            // `#[handlers]` unconditionally calls these from its
            // generated setup. Emit no-op shims so the call
            // compiles for `#[store]` targets.
            #[doc(hidden)]
            pub fn __pocopine_observe_seed(&mut self) {}
            #[doc(hidden)]
            pub fn __pocopine_observe_install(
                __handle: ::pocopine::Handle<Self>,
            ) {
                let _ = __handle;
            }
        }

        impl ::pocopine::__private::ComponentState for #struct_ident {
            fn get(&self, key: &str) -> ::pocopine::__private::JsValue {
                match key {
                    #(#get_arms)*
                    // Not a real field — fall through to a `#[computed]`
                    // synthetic field. Same resolution order as
                    // `#[component]`: real fields first, then computed.
                    _ => <Self as ::pocopine::__private::HandlerDispatch>::computed_get(self, key)
                        .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
                }
            }
            fn set(&mut self, key: &str, value: ::pocopine::__private::JsValue) {
                match key {
                    #(#set_arms)*
                    // Computed keys never match a real field arm, so
                    // they land here and stay read-only: a proxy write
                    // to `$store.<name>.<computed>` is a silent no-op.
                    _ => { let _ = value; }
                }
            }
            fn keys(&self) -> &'static [&'static str] {
                // Real field keys plus `#[computed]` synthetic keys.
                // `computed_keys()` is empty for stores with no
                // computed methods, so this degrades to the raw list.
                static __POCOPINE_STORE_KEYS: ::std::sync::OnceLock<&'static [&'static str]> =
                    ::std::sync::OnceLock::new();
                *__POCOPINE_STORE_KEYS.get_or_init(|| {
                    let mut __keys = ::std::vec![#(#keys_arr),*];
                    __keys.extend_from_slice(
                        <Self as ::pocopine::__private::HandlerDispatch>::computed_keys(),
                    );
                    let __boxed: ::std::boxed::Box<[&'static str]> = __keys.into_boxed_slice();
                    ::std::boxed::Box::leak(__boxed)
                })
            }
            fn is_computed_field(&self, key: &str) -> bool {
                // Drives the projection-cache skip in `read_field_tracked`:
                // a computed carries its own `Computed<JsValue>` memo, so
                // it must not also be frozen in the per-field projection
                // cache (which only invalidates on a direct write).
                <Self as ::pocopine::__private::HandlerDispatch>::computed_keys()
                    .contains(&key)
            }
            fn field_fingerprint(&self, key: &str) -> ::core::option::Option<u64> {
                match key {
                    #(#fingerprint_arms)*
                    _ => ::core::option::Option::None,
                }
            }
            fn field_quick_len(&self, key: &str) -> ::core::option::Option<u64> {
                match key {
                    #(#quick_len_arms)*
                    _ => ::core::option::Option::None,
                }
            }
            fn invoke(
                &mut self,
                key: &str,
                args: &::pocopine::__private::js_sys::Array,
            ) -> ::pocopine::__private::JsValue {
                <Self as ::pocopine::__private::HandlerDispatch>::invoke_handler(self, key, args)
            }
            // RFC-097 §3.3 — surface the `#[handlers]` receiver partition
            // to the runtime so `Scope::invoke` can skip the sweep for
            // `&self` handlers.
            fn is_readonly_handler(&self, key: &str) -> bool {
                <Self as ::pocopine::__private::HandlerDispatch>::is_readonly_handler(self, key)
            }
            fn type_name(&self) -> &'static str {
                #name_str
            }
        }

        impl ::pocopine::__private::Store for #struct_ident {
            const STORE_NAME: &'static str = #name_str;

            fn __register_store() {
                // First-registration wins; subsequent calls are no-ops.
                if ::pocopine::__private::store_scope(#name_str).is_some() {
                    return;
                }
                let instance: ::std::rc::Rc<::std::cell::RefCell<#struct_ident>> =
                    ::std::rc::Rc::new(::std::cell::RefCell::new(
                        <#struct_ident as ::core::default::Default>::default()
                    ));
                // Pointer used to key this store's `#[computed]` entries.
                // It must equal the pointer `computed_get` derives on a
                // read — both borrow the same `RefCell`, whose contents
                // never move — so compute it here and drop the borrow
                // before the `Rc` is shared. No-op for stores without
                // computed methods.
                let __state_ptr = ::pocopine::__private::component_computed::state_ptr(
                    &*instance.borrow(),
                );
                let scope = ::pocopine::__private::Scope::new(instance.clone());
                let __scope_id = scope.id;
                // Stores have no DOM lifecycle, so `setup` never runs.
                // Install computed entries here instead, before the scope
                // is registered — so the first `$store.<name>.<computed>`
                // read resolves. `Handle` keeps the singleton alive for
                // the computed closures.
                <#struct_ident>::__pocopine_computed_install(
                    __scope_id,
                    __state_ptr,
                    ::pocopine::__private::Handle::new(instance, __scope_id),
                );
                ::pocopine::__private::register_store_scope(#name_str, scope);
            }

            fn __handle() -> ::pocopine::__private::Handle<Self> {
                let scope = ::pocopine::__private::store_scope(#name_str)
                    .expect(concat!(
                        "store ", #name_str,
                        " not registered — call App::store::<_>() first"
                    ));
                let inner = scope.typed::<#struct_ident>().expect(
                    "store scope's typed state has a mismatched type",
                );
                ::pocopine::__private::Handle::new(inner, scope.id)
            }
        }
    };

    out.into()
}

/// `#[server]` — declare a function that runs on the server and is
/// callable from the client.
///
/// Expands to two `cfg`-gated definitions:
///
/// * **wasm32** — a client stub that POSTs the arguments as JSON to
///   a generated endpoint path and deserializes the JSON response as
///   `Result<R, ServerError>`. The user-supplied body is discarded on
///   this target.
/// * **non-wasm32** — the original user body, plus a helper
///   `__<name>_route(router) -> axum::Router` that registers the POST
///   route and an inventory entry that lets `pocopine_server::Server`
///   mount the route automatically. `#[server(guard = path)]` runs the
///   guard before the user body.
///
/// Access policy:
///
/// * `#[server(public)]` explicitly declares an open endpoint.
/// * `#[server(guard = require_user)]` protects the endpoint. The guard
///   must be an async function that accepts
///   `pocopine_server::RequestContext` and returns a `Result`.
/// * `#[server(idempotent)]` marks the generated client request as
///   replay-safe for middleware. Auth middleware may retry these at
///   most once after a successful refresh; unmarked calls remain
///   fail-closed.
/// * `#[server(path = "/api/stable-name")]` opts into a public, stable
///   route path. Without `path`, the macro uses a generated internal
///   path scoped by module path and function name.
/// * `#[server]` without either marker still compiles, but emits a
///   warning so authors do not accidentally ship unreviewed endpoints.
///
/// The signature shape this milestone supports:
///
/// * `async fn name(arg1: T1, ..., argN: TN) -> Result<R, ServerError>`
/// * `RequestContext`, `Extension<T>`, and `Option<Extension<T>>` parameters
///   are server-supplied and omitted from the
///   generated client stub / JSON payload.
/// * Every arg must be owned (`T`, not `&T` / `&mut T`). Args must
///   `Serialize + Deserialize`.
/// * Return type must round-trip through `serde_json`; guarded routes
///   expect the canonical `ServerError` for auth/body-decode failures.
#[derive(Default)]
struct ServerPolicy {
    public: bool,
    guard: Option<Path>,
    idempotent: bool,
    path: Option<LitStr>,
}

impl ServerPolicy {
    fn has_access_policy(&self) -> bool {
        self.public || self.guard.is_some()
    }
}

fn parse_server_policy(attr: TokenStream) -> syn::Result<ServerPolicy> {
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse(attr)?;
    let mut policy = ServerPolicy::default();

    for meta in metas {
        match meta {
            Meta::Path(path) if path.is_ident("public") => {
                if policy.public {
                    return Err(syn::Error::new_spanned(path, "duplicate `public` policy"));
                }
                policy.public = true;
            }
            Meta::Path(path) if path.is_ident("idempotent") => {
                if policy.idempotent {
                    return Err(syn::Error::new_spanned(
                        path,
                        "duplicate `idempotent` marker",
                    ));
                }
                policy.idempotent = true;
            }
            Meta::NameValue(MetaNameValue { path, value, .. }) if path.is_ident("guard") => {
                if policy.guard.is_some() {
                    return Err(syn::Error::new_spanned(path, "duplicate auth guard"));
                }
                policy.guard = Some(parse_server_guard_value(value)?);
            }
            Meta::NameValue(MetaNameValue { path, value, .. }) if path.is_ident("path") => {
                if policy.path.is_some() {
                    return Err(syn::Error::new_spanned(path, "duplicate route path"));
                }
                policy.path = Some(parse_server_path_value(value)?);
            }
            Meta::List(list) if list.path.is_ident("guard") => {
                if policy.guard.is_some() {
                    return Err(syn::Error::new_spanned(list.path, "duplicate auth guard"));
                }
                policy.guard = Some(list.parse_args::<Path>()?);
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "unsupported `#[server]` option; expected `public`, `idempotent`, `guard = path`, or `path = \"/api/name\"`",
                ));
            }
        }
    }

    if policy.public {
        let Some(guard) = policy.guard.as_ref() else {
            return Ok(policy);
        };
        return Err(syn::Error::new_spanned(
            guard,
            "`public` server functions cannot also declare an auth guard",
        ));
    }

    Ok(policy)
}

fn parse_server_guard_value(value: Expr) -> syn::Result<Path> {
    match value {
        Expr::Path(expr_path) => Ok(expr_path.path),
        Expr::Lit(ExprLit {
            lit: Lit::Str(lit), ..
        }) => lit.parse::<Path>(),
        other => Err(syn::Error::new_spanned(
            other,
            "`guard` must be a Rust path, for example `guard = require_user`",
        )),
    }
}

fn parse_server_path_value(value: Expr) -> syn::Result<LitStr> {
    let Expr::Lit(ExprLit {
        lit: Lit::Str(lit), ..
    }) = value
    else {
        return Err(syn::Error::new_spanned(
            value,
            "`path` must be a string literal, for example `path = \"/api/posts\"`",
        ));
    };

    let value = lit.value();
    if !value.starts_with('/') {
        return Err(syn::Error::new_spanned(lit, "`path` must start with `/`"));
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(syn::Error::new_spanned(
            lit,
            "`path` must not contain surrounding whitespace or control characters",
        ));
    }

    Ok(lit)
}

/// True when a `#[server]` fn returns a streaming result
/// (`StreamServerResult<T>` / `ServerStream<T>`) — detected syntactically from
/// the return type's head ident (RFC-107).
fn returns_stream(sig: &syn::Signature) -> bool {
    if let syn::ReturnType::Type(_, ty) = &sig.output
        && let syn::Type::Path(type_path) = ty.as_ref()
        && let Some(seg) = type_path.path.segments.last()
    {
        let ident = seg.ident.to_string();
        return ident == "StreamServerResult" || ident == "ServerStream";
    }
    false
}

fn is_request_context_type(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    type_path
        .path
        .segments
        .last()
        .is_some_and(|seg| seg.ident == "RequestContext")
}

fn type_path_last_ident(ty: &Type) -> Option<&syn::Ident> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    type_path.path.segments.last().map(|seg| &seg.ident)
}

fn is_extension_type(ty: &Type) -> bool {
    type_path_last_ident(ty).is_some_and(|ident| ident == "Extension")
}

fn is_option_extension_type(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    let Some(last) = type_path.path.segments.last() else {
        return false;
    };
    if last.ident != "Option" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return false;
    };
    args.args
        .iter()
        .any(|arg| matches!(arg, syn::GenericArgument::Type(inner) if is_extension_type(inner)))
}

fn is_server_extractor_type(ty: &Type) -> bool {
    is_request_context_type(ty) || is_extension_type(ty) || is_option_extension_type(ty)
}

#[proc_macro_attribute]
pub fn server(attr: TokenStream, item: TokenStream) -> TokenStream {
    let policy = match parse_server_policy(attr) {
        Ok(policy) => policy,
        Err(err) => return err.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemFn);
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;
    let streaming = returns_stream(sig);

    let fn_ident = sig.ident.clone();
    let fn_name_str = fn_ident.to_string();
    let route_ident = format_ident!("__{fn_name_str}_route");
    let path_ident = format_ident!("__{fn_name_str}_path");
    let function_path_ident = format_ident!("__{fn_name_str}_function_path");
    let path_fn = match policy.path.as_ref() {
        Some(path) => quote! {
            #[doc(hidden)]
            pub fn #path_ident() -> &'static str {
                #path
            }
        },
        None => quote! {
            #[doc(hidden)]
            pub fn #path_ident() -> &'static str {
                static PATH: ::std::sync::OnceLock<::std::string::String> =
                    ::std::sync::OnceLock::new();
                PATH.get_or_init(|| {
                    ::pocopine::__private::server_function_default_path(
                        module_path!(),
                        #fn_name_str,
                    )
                }).as_str()
            }
        },
    };
    let function_path_fn = quote! {
        #[doc(hidden)]
        pub fn #function_path_ident() -> &'static str {
            concat!(module_path!(), "::", #fn_name_str)
        }
    };

    // Collect wire (pat_ident, type) pairs, rejecting self / ref args.
    // Server extractors are supplied from request parts/extensions on the host,
    // while the wasm client stub omits them.
    let mut arg_idents = Vec::new();
    let mut arg_types = Vec::new();
    let mut has_request_context_arg = false;
    let mut server_extractor_params = Vec::new();
    let mut server_call_args = Vec::new();
    let mut client_inputs = Punctuated::<FnArg, Token![,]>::new();
    for input_arg in &sig.inputs {
        match input_arg {
            FnArg::Receiver(r) => {
                return syn::Error::new_spanned(
                    r,
                    "`#[server]` functions cannot take `self` — they are free functions",
                )
                .to_compile_error()
                .into();
            }
            FnArg::Typed(PatType { pat, ty, .. }) => {
                // Reject &T / &mut T.
                if matches!(**ty, syn::Type::Reference(_)) {
                    return syn::Error::new_spanned(
                        ty,
                        "`#[server]` args must be owned — reference types are not supported \
                         (clone-into or take an owned type instead)",
                    )
                    .to_compile_error()
                    .into();
                }
                let Pat::Ident(pat_ident) = &**pat else {
                    return syn::Error::new_spanned(
                        pat,
                        "`#[server]` args must be simple identifiers",
                    )
                    .to_compile_error()
                    .into();
                };
                if is_server_extractor_type(ty) {
                    if is_request_context_type(ty) && has_request_context_arg {
                        return syn::Error::new_spanned(
                            ty,
                            "`#[server]` functions may accept at most one `RequestContext` parameter",
                        )
                        .to_compile_error()
                        .into();
                    }
                    if is_request_context_type(ty) {
                        has_request_context_arg = true;
                    }
                    let ident = &pat_ident.ident;
                    server_extractor_params.push((pat_ident.ident.clone(), (**ty).clone()));
                    server_call_args.push(quote! { #ident });
                    continue;
                }
                arg_idents.push(pat_ident.ident.clone());
                arg_types.push((**ty).clone());
                let ident = &pat_ident.ident;
                server_call_args.push(quote! { #ident });
                client_inputs.push(input_arg.clone());
            }
        }
    }
    let mut client_sig = sig.clone();
    client_sig.inputs = client_inputs;

    // `(arg1, arg2, ...)` — tuple of idents, with trailing comma for
    // single-element tuples so the macro output is grammatical.
    let args_tuple_value = if arg_idents.is_empty() {
        quote! { () }
    } else if arg_idents.len() == 1 {
        let only = &arg_idents[0];
        quote! { (#only,) }
    } else {
        quote! { ( #(#arg_idents),* ) }
    };

    // `(T1, T2, ...)` — matching tuple type.
    let args_tuple_type = if arg_types.is_empty() {
        quote! { () }
    } else if arg_types.len() == 1 {
        let only = &arg_types[0];
        quote! { (#only,) }
    } else {
        quote! { ( #(#arg_types),* ) }
    };

    // `(arg1, arg2, ...)` destructuring pattern for the axum handler's
    // `Json<(T1, T2, ...)>` body extractor.
    let destructure = if arg_idents.is_empty() {
        quote! { _args }
    } else if arg_idents.len() == 1 {
        let only = &arg_idents[0];
        quote! { (#only,) }
    } else {
        quote! { ( #(#arg_idents),* ) }
    };

    let client_sig_without_body = quote! { #vis #client_sig };

    let client_body = if streaming {
        quote! {
            ::pocopine::fetch::call_stream::<#args_tuple_type, _>(
                #path_ident(),
                &#args_tuple_value,
            ).await
        }
    } else if policy.idempotent {
        quote! {
            ::pocopine::fetch::call_replay_safe::<#args_tuple_type, _>(
                #path_ident(),
                &#args_tuple_value,
            ).await
        }
    } else {
        quote! {
            ::pocopine::fetch::call::<#args_tuple_type, _>(
                #path_ident(),
                &#args_tuple_value,
            ).await
        }
    };

    let client = quote! {
        #[cfg(target_arch = "wasm32")]
        #client_sig_without_body {
            #client_body
        }
    };

    let policy_warning = if policy.has_access_policy() {
        proc_macro2::TokenStream::new()
    } else {
        build_warning_tokens(&format!(
            "pocopine #[server] function `{fn_name_str}` has no access policy. Write #[server(public)] for an intentional public endpoint or #[server(guard = path::to_guard)] to enforce auth. Missing server-function access policy is currently a warning and may become a hard error."
        ))
    };

    // RFC-107 — a streaming `#[server]` frames its result (and any pre-body
    // error) as SSE; a unary one stays JSON. Branch the response sites once.
    let error_return = if streaming {
        quote! { return ::pocopine_server::sse_error_response(__pocopine_error); }
    } else {
        quote! {
            let result = ::core::result::Result::Err(__pocopine_error);
            return ::pocopine_server::axum::Json(result);
        }
    };
    let final_return = if streaming {
        quote! { ::pocopine_server::sse_stream_response(result) }
    } else {
        quote! { ::pocopine_server::axum::Json(result) }
    };
    let server_extractor_bindings: Vec<_> = server_extractor_params
        .iter()
        .map(|(ident, ty)| {
            quote! {
                let #ident: #ty =
                    match <#ty as ::pocopine_server::FromRequestContext>
                        ::from_request_context(&__pocopine_request_context)
                    {
                        ::core::result::Result::Ok(value) => value,
                        ::core::result::Result::Err(err) => {
                            let __pocopine_error: ::pocopine::ServerError =
                                ::core::convert::Into::into(err);
                            let __pocopine_duration_ms =
                                __pocopine_started.elapsed().as_millis() as u64;
                            let __pocopine_error_kind = match &__pocopine_error {
                                ::pocopine::ServerError::App(_) => "app",
                                ::pocopine::ServerError::Unauthorized(_) => "unauthorized",
                                ::pocopine::ServerError::Forbidden(_) => "forbidden",
                                ::pocopine::ServerError::BadRequest(_) => "bad_request",
                                ::pocopine::ServerError::Network(_) => "network",
                            };
                            let __pocopine_status = match &__pocopine_error {
                                ::pocopine::ServerError::Unauthorized(_) => 401,
                                ::pocopine::ServerError::Forbidden(_) => 403,
                                ::pocopine::ServerError::BadRequest(_) => 400,
                                _ => 500,
                            };
                            ::pocopine_server::tracing::warn!(
                                target: "pocopine.log",
                                function = #fn_name_str,
                                function_path = #function_path_ident(),
                                route = #path_ident(),
                                duration_ms = __pocopine_duration_ms,
                                error_kind = __pocopine_error_kind,
                                error = %__pocopine_error,
                                "server function request context extraction failed"
                            );
                            if ::pocopine_server::has_server_function_rejected_hooks() {
                                ::pocopine_server::emit(
                                    ::pocopine_server::ServerFunctionRejected {
                                        function: #fn_name_str,
                                        function_path: #function_path_ident(),
                                        request_id: __pocopine_request_id,
                                        status: __pocopine_status,
                                        reason: __pocopine_error_kind,
                                    },
                                );
                            }
                            #error_return
                        }
                    };
            }
        })
        .collect();

    // Always destructure the request into parts so the
    // server-function plugin event can read the framework-stamped
    // RequestId (set by `request_event_layer`).
    let parts_binding = quote! { let (parts, body) = request.into_parts(); };
    let needs_request_context = policy.guard.is_some() || !server_extractor_params.is_empty();
    let request_context_setup = if needs_request_context {
        quote! {
            let __pocopine_request_context =
                ::pocopine_server::RequestContext::from_parts(
                    parts.method.clone(),
                    parts.uri.clone(),
                    parts.headers.clone(),
                    parts.extensions.clone(),
                );
        }
    } else {
        proc_macro2::TokenStream::new()
    };
    let guard_prologue = match policy.guard.as_ref() {
        Some(guard_path) => quote! {
            if let Err(err) = #guard_path(__pocopine_request_context.clone()).await {
                let __pocopine_error: ::pocopine::ServerError =
                    ::core::convert::Into::into(err);
                let __pocopine_duration_ms =
                    __pocopine_started.elapsed().as_millis() as u64;
                let __pocopine_error_kind = match &__pocopine_error {
                    ::pocopine::ServerError::App(_) => "app",
                    ::pocopine::ServerError::Unauthorized(_) => "unauthorized",
                    ::pocopine::ServerError::Forbidden(_) => "forbidden",
                    ::pocopine::ServerError::BadRequest(_) => "bad_request",
                    ::pocopine::ServerError::Network(_) => "network",
                };
                let __pocopine_status = match &__pocopine_error {
                    ::pocopine::ServerError::Unauthorized(_) => 401,
                    ::pocopine::ServerError::Forbidden(_) => 403,
                    ::pocopine::ServerError::BadRequest(_) => 400,
                    _ => 500,
                };
                ::pocopine_server::tracing::warn!(
                    target: "pocopine.log",
                    function = #fn_name_str,
                    function_path = #function_path_ident(),
                    route = #path_ident(),
                    duration_ms = __pocopine_duration_ms,
                    error_kind = __pocopine_error_kind,
                    error = %__pocopine_error,
                    "server function guard rejected request"
                );
                if ::pocopine_server::has_server_function_rejected_hooks() {
                    ::pocopine_server::emit(
                        ::pocopine_server::ServerFunctionRejected {
                            function: #fn_name_str,
                            function_path: #function_path_ident(),
                            request_id: __pocopine_request_id,
                            status: __pocopine_status,
                            reason: __pocopine_error_kind,
                        },
                    );
                }
                #error_return
            }
        },
        None => proc_macro2::TokenStream::new(),
    };

    // Generated routes take the raw request so the same explicit body
    // limit applies to public and guarded endpoints; guarded routes run
    // auth before the body is read.
    let route_handler = quote! {
        ::pocopine_server::axum::routing::post(
            |request: ::pocopine_server::axum::extract::Request| async move {
                use ::pocopine_server::tracing::Instrument as _;

                async move {
                    let __pocopine_started = ::std::time::Instant::now();
                    #parts_binding
                    let __pocopine_request_id: u64 = parts
                        .extensions
                        .get::<::pocopine_server::RequestId>()
                        .map(|id| id.0)
                        .unwrap_or_else(|| ::pocopine_server::next_request_id());
                    if ::pocopine_server::has_server_function_started_hooks() {
                        ::pocopine_server::emit(
                            ::pocopine_server::ServerFunctionStarted {
                                function: #fn_name_str,
                                function_path: #function_path_ident(),
                                request_id: __pocopine_request_id,
                            },
                        );
                    }
                    #request_context_setup
                    #guard_prologue
                    let body_limit = ::pocopine_server::server_function_body_limit();
                    let body_bytes = match ::pocopine_server::axum::body::to_bytes(
                        body,
                        body_limit,
                    ).await {
                        Ok(bytes) => bytes,
                        Err(err) => {
                            let __pocopine_error = ::pocopine::ServerError::BadRequest(
                                format!("read server-function request body: {err}"),
                            );
                            let __pocopine_duration_ms =
                                __pocopine_started.elapsed().as_millis() as u64;
                            ::pocopine_server::tracing::warn!(
                                target: "pocopine.log",
                                function = #fn_name_str,
                                function_path = #function_path_ident(),
                                route = #path_ident(),
                                duration_ms = __pocopine_duration_ms,
                                error_kind = "bad_request",
                                error = %__pocopine_error,
                                "server function request body read failed"
                            );
                            if ::pocopine_server::has_server_function_rejected_hooks() {
                                ::pocopine_server::emit(
                                    ::pocopine_server::ServerFunctionRejected {
                                        function: #fn_name_str,
                                        function_path: #function_path_ident(),
                                        request_id: __pocopine_request_id,
                                        status: 400,
                                        reason: "bad_request",
                                    },
                                );
                            }
                            #error_return
                        }
                    };
                    let __pocopine_body_bytes = body_bytes.len() as u64;
                    let #destructure: #args_tuple_type =
                        match ::pocopine_server::serde_json::from_slice(&body_bytes) {
                            Ok(args) => args,
                            Err(err) => {
                                let __pocopine_error = ::pocopine::ServerError::BadRequest(
                                    format!("parse server-function request body: {err}"),
                                );
                                let __pocopine_duration_ms =
                                    __pocopine_started.elapsed().as_millis() as u64;
                                ::pocopine_server::tracing::warn!(
                                    target: "pocopine.log",
                                    function = #fn_name_str,
                                    function_path = #function_path_ident(),
                                    route = #path_ident(),
                                    duration_ms = __pocopine_duration_ms,
                                    body_bytes = __pocopine_body_bytes,
                                    error_kind = "bad_request",
                                    error = "request body parse failed",
                                    "server function request body parse failed"
                                );
                                if ::pocopine_server::has_server_function_rejected_hooks() {
                                    ::pocopine_server::emit(
                                        ::pocopine_server::ServerFunctionRejected {
                                            function: #fn_name_str,
                                            function_path: #function_path_ident(),
                                            request_id: __pocopine_request_id,
                                            status: 400,
                                            reason: "bad_request",
                                        },
                                    );
                                }
                                #error_return
                            }
                        };
                    #(#server_extractor_bindings)*
                    let result = #fn_ident( #(#server_call_args),* ).await;
                    let __pocopine_duration_ms =
                        __pocopine_started.elapsed().as_millis() as u64;
                    let __pocopine_duration_ms_f64 =
                        __pocopine_started.elapsed().as_secs_f64() * 1_000.0;
                    match &result {
                        ::core::result::Result::Ok(_) => {
                            ::pocopine_server::tracing::info!(
                                target: "pocopine.trace",
                                function = #fn_name_str,
                                function_path = #function_path_ident(),
                                route = #path_ident(),
                                duration_ms = __pocopine_duration_ms,
                                body_bytes = __pocopine_body_bytes,
                                "server function completed"
                            );
                            if ::pocopine_server::has_server_function_completed_hooks() {
                                ::pocopine_server::emit(
                                    ::pocopine_server::ServerFunctionCompleted {
                                        function: #fn_name_str,
                                        function_path: #function_path_ident(),
                                        request_id: __pocopine_request_id,
                                        duration_ms: __pocopine_duration_ms_f64,
                                    },
                                );
                            }
                        }
                        ::core::result::Result::Err(err) => {
                            let __pocopine_error_kind = match err {
                                ::pocopine::ServerError::App(_) => "app",
                                ::pocopine::ServerError::Unauthorized(_) => "unauthorized",
                                ::pocopine::ServerError::Forbidden(_) => "forbidden",
                                ::pocopine::ServerError::BadRequest(_) => "bad_request",
                                ::pocopine::ServerError::Network(_) => "network",
                            };
                            ::pocopine_server::tracing::warn!(
                                target: "pocopine.log",
                                function = #fn_name_str,
                                function_path = #function_path_ident(),
                                route = #path_ident(),
                                duration_ms = __pocopine_duration_ms,
                                body_bytes = __pocopine_body_bytes,
                                error_kind = __pocopine_error_kind,
                                error = %err,
                                "server function failed"
                            );
                            if ::pocopine_server::has_server_function_failed_hooks() {
                                ::pocopine_server::emit(
                                    ::pocopine_server::ServerFunctionFailed {
                                        function: #fn_name_str,
                                        function_path: #function_path_ident(),
                                        request_id: __pocopine_request_id,
                                        error_class: __pocopine_error_kind,
                                        duration_ms: __pocopine_duration_ms_f64,
                                    },
                                );
                            }
                        }
                    }
                    #final_return
                }
                .instrument(::pocopine_server::tracing::info_span!(
                    target: "pocopine.trace",
                    "pocopine.server_function",
                    function = #fn_name_str,
                    function_path = #function_path_ident(),
                    route = #path_ident(),
                ))
                .await
            },
        )
    };

    let server = quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #vis #sig #body

        #[cfg(not(target_arch = "wasm32"))]
        #[doc(hidden)]
        pub fn #route_ident(
            router: ::pocopine_server::axum::Router,
        ) -> ::pocopine_server::axum::Router {
            router.route(#path_ident(), #route_handler)
        }

        #[cfg(not(target_arch = "wasm32"))]
        ::pocopine_server::inventory::submit! {
            ::pocopine_server::ServerFunctionRoute {
                module_path: module_path!(),
                function: #fn_name_str,
                path: #path_ident,
                install: #route_ident,
            }
        }
    };

    let out = quote! {
        #policy_warning
        #path_fn
        #function_path_fn
        #client
        #server
    };
    out.into()
}

mod protected_kw {
    syn::custom_keyword!(require);
}

struct ProtectedInput {
    check: ExprClosure,
    function: ItemFn,
}

impl Parse for ProtectedInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        input.parse::<protected_kw::require>()?;
        let check = input.parse::<ExprClosure>()?;

        if input.peek(Token![;]) {
            input.parse::<Token![;]>()?;
        } else if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        } else {
            return Err(input.error("expected `;` after `require |ctx| ...`"));
        }

        let function = input.parse::<ItemFn>()?;
        Ok(Self { check, function })
    }
}

/// `protected!` — declare a guarded server function with inline policy.
///
/// ```ignore
/// pocopine::protected! {
///     require |ctx| ctx.principal().has_role(&Role::admin());
///
///     pub async fn create_post(title: String) -> ServerResult<Post> {
///         /* protected body */
///     }
/// }
/// ```
///
/// The require closure must be a synchronous boolean check. The macro
/// emits a private guard function and feeds it through
/// `#[pocopine::server(guard = ...)]`, so auth still runs before the
/// JSON body is decoded.
#[proc_macro]
pub fn protected(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ProtectedInput);
    let check = input.check;
    let function = input.function;
    let fn_ident = function.sig.ident.clone();
    let guard_ident = format_ident!("__pocopine_guard_{fn_ident}");
    let mut check_inputs = check.inputs.into_iter();
    let Some(ctx_pat) = check_inputs.next() else {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "`protected!` requires `require |ctx| condition;`",
        )
        .to_compile_error()
        .into();
    };
    if let Some(extra) = check_inputs.next() {
        return syn::Error::new_spanned(
            extra,
            "`protected!` require closures must accept exactly one request context argument",
        )
        .to_compile_error()
        .into();
    }
    let check_body = check.body;

    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        async fn #guard_ident(
            ctx: ::pocopine::server::RequestContext,
        ) -> ::pocopine::ServerResult<()> {
            use ::pocopine::auth::RequestAuthExt as _;
            let __pocopine_guard_allows: bool = {
                let #ctx_pat = ctx;
                #check_body
            };
            if !__pocopine_guard_allows {
                return ::core::result::Result::Err(
                    ::pocopine::ServerError::forbidden("server function access denied"),
                );
            }
            ::core::result::Result::Ok(())
        }

        #[pocopine::server(guard = #guard_ident)]
        #function
    }
    .into()
}

#[derive(Clone)]
struct JobConfig {
    queue: String,
    retries: u32,
    periodic: Option<JobPeriodicConfig>,
}

#[derive(Clone)]
enum JobPeriodicConfig {
    Every { interval_ms: u64 },
    Cron { expr: String },
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            queue: "default".to_string(),
            retries: 3,
            periodic: None,
        }
    }
}

fn parse_job_config(attr: TokenStream) -> syn::Result<JobConfig> {
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse(attr)?;
    let mut config = JobConfig::default();

    for meta in metas {
        match meta {
            Meta::NameValue(MetaNameValue { path, value, .. }) if path.is_ident("queue") => {
                let Expr::Lit(ExprLit {
                    lit: Lit::Str(lit), ..
                }) = value
                else {
                    return Err(syn::Error::new_spanned(
                        value,
                        "`queue` must be a string literal",
                    ));
                };
                config.queue = lit.value();
            }
            Meta::NameValue(MetaNameValue { path, value, .. })
                if path.is_ident("retries") || path.is_ident("max_retries") =>
            {
                let Expr::Lit(ExprLit {
                    lit: Lit::Int(lit), ..
                }) = value
                else {
                    return Err(syn::Error::new_spanned(
                        value,
                        "`retries` must be an integer literal",
                    ));
                };
                config.retries = lit.base10_parse::<u32>()?;
            }
            Meta::NameValue(MetaNameValue { path, value, .. }) if path.is_ident("every") => {
                let Expr::Lit(ExprLit {
                    lit: Lit::Str(lit), ..
                }) = value
                else {
                    return Err(syn::Error::new_spanned(
                        value,
                        "`every` must be a string literal like \"15m\"",
                    ));
                };
                if config.periodic.is_some() {
                    return Err(syn::Error::new_spanned(
                        path,
                        "`every` and `cron` are mutually exclusive",
                    ));
                }
                let interval_ms = parse_job_duration_ms(&lit)?;
                config.periodic = Some(JobPeriodicConfig::Every { interval_ms });
            }
            Meta::NameValue(MetaNameValue { path, value, .. }) if path.is_ident("cron") => {
                let Expr::Lit(ExprLit {
                    lit: Lit::Str(lit), ..
                }) = value
                else {
                    return Err(syn::Error::new_spanned(
                        value,
                        "`cron` must be a string literal",
                    ));
                };
                if config.periodic.is_some() {
                    return Err(syn::Error::new_spanned(
                        path,
                        "`every` and `cron` are mutually exclusive",
                    ));
                }
                let expr = lit.value();
                expr.parse::<cron::Schedule>().map_err(|err| {
                    syn::Error::new_spanned(
                        &lit,
                        format!("invalid `cron` expression for #[job]: {err}"),
                    )
                })?;
                config.periodic = Some(JobPeriodicConfig::Cron { expr });
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "unsupported `#[job]` option; expected `queue = \"name\"`, `retries = N`, `every = \"15m\"`, or `cron = \"...\"`",
                ));
            }
        }
    }

    Ok(config)
}

fn parse_job_duration_ms(lit: &LitStr) -> syn::Result<u64> {
    let value = lit.value();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(syn::Error::new_spanned(lit, "`every` cannot be empty"));
    }

    let split_at = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (num, unit) = trimmed.split_at(split_at);
    if num.is_empty() || unit.is_empty() {
        return Err(syn::Error::new_spanned(
            lit,
            "`every` must include a number and unit, for example \"30s\", \"15m\", or \"2h\"",
        ));
    }
    let value = num.parse::<u64>().map_err(|err| {
        syn::Error::new_spanned(lit, format!("invalid `every` duration number: {err}"))
    })?;
    let multiplier = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 60 * 60_000,
        "d" => 24 * 60 * 60_000,
        _ => {
            return Err(syn::Error::new_spanned(
                lit,
                "`every` supports ms, s, m, h, and d units",
            ));
        }
    };
    let interval_ms = value.checked_mul(multiplier).ok_or_else(|| {
        syn::Error::new_spanned(lit, "`every` duration is too large to represent")
    })?;
    if interval_ms == 0 {
        return Err(syn::Error::new_spanned(
            lit,
            "`every` duration must be greater than zero",
        ));
    }
    Ok(interval_ms)
}

/// `#[job]` — declare a host-side background job.
///
/// The function body compiles only for host targets. On wasm32 the macro
/// emits a stub with the same signature that returns
/// `JobError::Unsupported`, keeping client bundles free of Redis and worker
/// runtime dependencies.
#[proc_macro_attribute]
pub fn job(attr: TokenStream, item: TokenStream) -> TokenStream {
    let config = match parse_job_config(attr) {
        Ok(config) => config,
        Err(err) => return err.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemFn);
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;

    if sig.asyncness.is_none() {
        return syn::Error::new_spanned(sig.fn_token, "`#[job]` functions must be async")
            .to_compile_error()
            .into();
    }

    let fn_ident = sig.ident.clone();
    let fn_name_str = fn_ident.to_string();
    let queue = config.queue;
    let retries = config.retries;
    let periodic = config.periodic.clone();
    let job_mod = format_ident!("{fn_name_str}_job");
    let job_name_const = format_ident!("__pocopine_job_name_{fn_name_str}");
    let dispatch_ident = format_ident!("__pocopine_dispatch_{fn_name_str}");

    let mut inputs = sig.inputs.iter();
    let payload = match inputs.next() {
        Some(input_arg) => {
            if inputs.next().is_some() {
                return syn::Error::new_spanned(
                    sig,
                    "`#[job]` functions must take zero args for periodic jobs or one owned payload argument for manually enqueued jobs",
                )
                .to_compile_error()
                .into();
            }
            if periodic.is_some() {
                return syn::Error::new_spanned(
                    sig,
                    "`#[job(every = ...)]` and `#[job(cron = ...)]` jobs must not take a payload argument",
                )
                .to_compile_error()
                .into();
            }
            match input_arg {
                FnArg::Receiver(r) => {
                    return syn::Error::new_spanned(
                        r,
                        "`#[job]` functions cannot take `self` — they are free functions",
                    )
                    .to_compile_error()
                    .into();
                }
                FnArg::Typed(PatType { pat, ty, .. }) => {
                    if matches!(**ty, syn::Type::Reference(_)) {
                        return syn::Error::new_spanned(
                            ty,
                            "`#[job]` payloads must be owned — reference types are not supported",
                        )
                        .to_compile_error()
                        .into();
                    }
                    let Pat::Ident(pat_ident) = &**pat else {
                        return syn::Error::new_spanned(
                            pat,
                            "`#[job]` payload argument must be a simple identifier",
                        )
                        .to_compile_error()
                        .into();
                    };
                    JobPayload::Typed {
                        ident: pat_ident.ident.clone(),
                        ty: ty.clone(),
                    }
                }
            }
        }
        None if periodic.is_some() => JobPayload::Unit,
        None => {
            return syn::Error::new_spanned(
                sig,
                "`#[job]` functions without `every` or `cron` must take one owned payload argument",
            )
            .to_compile_error()
            .into();
        }
    };

    if matches!(sig.output, syn::ReturnType::Default) {
        return syn::Error::new_spanned(sig, "`#[job]` functions must return `JobResult<()>`")
            .to_compile_error()
            .into();
    }

    let sig_without_body = quote! { #vis #sig };
    let helper_arg_def = payload.helper_arg_def();
    let helper_arg_call = payload.helper_arg_call();
    let payload_ref_binding = payload.payload_ref_binding();
    let payload_ref = payload.payload_ref();
    let dispatch_body = payload.dispatch_body(&fn_ident);
    let descriptor_ctor = match periodic {
        Some(JobPeriodicConfig::Every { interval_ms }) => quote! {
            ::pocopine::__private::jobs::JobDescriptor::periodic(
                #job_name_const,
                #queue,
                #retries,
                ::pocopine::__private::jobs::PeriodicSchedule::every_millis(#interval_ms),
                #job_mod::#dispatch_ident,
            )
        },
        Some(JobPeriodicConfig::Cron { expr }) => quote! {
            ::pocopine::__private::jobs::JobDescriptor::periodic(
                #job_name_const,
                #queue,
                #retries,
                ::pocopine::__private::jobs::PeriodicSchedule::cron(#expr),
                #job_mod::#dispatch_ident,
            )
        },
        None => quote! {
            ::pocopine::__private::jobs::JobDescriptor::new(
                #job_name_const,
                #queue,
                #retries,
                #job_mod::#dispatch_ident,
            )
        },
    };

    let host = quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #vis #sig #body

        #[cfg(not(target_arch = "wasm32"))]
        #[allow(non_upper_case_globals)]
        const #job_name_const: &'static str =
            concat!(module_path!(), "::", stringify!(#fn_ident));

        #[cfg(not(target_arch = "wasm32"))]
        #vis mod #job_mod {
            use super::*;

            pub async fn enqueue(
                #helper_arg_def
            ) -> ::pocopine::JobResult<::pocopine::JobId> {
                let client = ::pocopine::JobClient::from_env()?;
                enqueue_with(&client, #helper_arg_call).await
            }

            pub async fn enqueue_with(
                client: &::pocopine::JobClient,
                #helper_arg_def
            ) -> ::pocopine::JobResult<::pocopine::JobId> {
                #payload_ref_binding
                client
                    .enqueue_json(
                        super::#job_name_const,
                        #queue,
                        ::pocopine::__private::jobs::RetryPolicy::from_retries(#retries).max_attempts,
                        #payload_ref,
                    )
                    .await
            }

            pub async fn schedule_in(
                #helper_arg_def
                delay: ::std::time::Duration,
            ) -> ::pocopine::JobResult<::pocopine::JobId> {
                let client = ::pocopine::JobClient::from_env()?;
                schedule_in_with(&client, #helper_arg_call delay).await
            }

            pub async fn schedule_in_with(
                client: &::pocopine::JobClient,
                #helper_arg_def
                delay: ::std::time::Duration,
            ) -> ::pocopine::JobResult<::pocopine::JobId> {
                #payload_ref_binding
                client
                    .schedule_json_in(
                        super::#job_name_const,
                        #queue,
                        ::pocopine::__private::jobs::RetryPolicy::from_retries(#retries).max_attempts,
                        #payload_ref,
                        delay,
                    )
                    .await
            }

            pub async fn schedule_at(
                #helper_arg_def
                when: ::std::time::SystemTime,
            ) -> ::pocopine::JobResult<::pocopine::JobId> {
                let client = ::pocopine::JobClient::from_env()?;
                schedule_at_with(&client, #helper_arg_call when).await
            }

            pub async fn schedule_at_with(
                client: &::pocopine::JobClient,
                #helper_arg_def
                when: ::std::time::SystemTime,
            ) -> ::pocopine::JobResult<::pocopine::JobId> {
                #payload_ref_binding
                client
                    .schedule_json_at(
                        super::#job_name_const,
                        #queue,
                        ::pocopine::__private::jobs::RetryPolicy::from_retries(#retries).max_attempts,
                        #payload_ref,
                        when,
                    )
                    .await
            }

            pub fn #dispatch_ident(
                payload: ::std::vec::Vec<u8>,
            ) -> ::pocopine::JobFuture {
                ::std::boxed::Box::pin(async move {
                    #dispatch_body
                })
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        ::pocopine::__private::jobs::inventory::submit! {
            #descriptor_ctor
        }
    };

    let wasm = quote! {
        #[cfg(target_arch = "wasm32")]
        #[allow(unused_variables)]
        #sig_without_body {
            ::core::result::Result::Err(::pocopine::JobError::unsupported(
                "background jobs run on the server worker, not in wasm",
            ))
        }
    };

    quote! {
        #host
        #wasm
    }
    .into()
}

enum JobPayload {
    Unit,
    Typed { ident: syn::Ident, ty: Box<Type> },
}

impl JobPayload {
    fn helper_arg_def(&self) -> proc_macro2::TokenStream {
        match self {
            JobPayload::Unit => quote! {},
            JobPayload::Typed { ident, ty } => quote! { #ident: #ty, },
        }
    }

    fn helper_arg_call(&self) -> proc_macro2::TokenStream {
        match self {
            JobPayload::Unit => quote! {},
            JobPayload::Typed { ident, .. } => quote! { #ident, },
        }
    }

    fn payload_ref_binding(&self) -> proc_macro2::TokenStream {
        match self {
            JobPayload::Unit => quote! { let __pocopine_payload = (); },
            JobPayload::Typed { .. } => quote! {},
        }
    }

    fn payload_ref(&self) -> proc_macro2::TokenStream {
        match self {
            JobPayload::Unit => quote! { &__pocopine_payload },
            JobPayload::Typed { ident, .. } => quote! { &#ident },
        }
    }

    fn dispatch_body(&self, fn_ident: &syn::Ident) -> proc_macro2::TokenStream {
        match self {
            JobPayload::Unit => quote! {
                let _: () = ::pocopine::__private::jobs::serde_json::from_slice(&payload)?;
                super::#fn_ident().await
            },
            JobPayload::Typed { ident, ty } => quote! {
                let #ident: #ty =
                    ::pocopine::__private::jobs::serde_json::from_slice(&payload)?;
                super::#fn_ident(#ident).await
            },
        }
    }
}

/// `#[derive(Emit)]` — RFC 056 §6.8 typed event emission.
///
/// Implements [`pocopine::Emit`] for an enum where each variant maps
/// to one DOM `CustomEvent`:
///
/// * variant ident → kebab-case event name
///   (`Confirm` → `"confirm"`, `OpenChange` → `"open-change"`)
/// * unit variants → empty `detail`
/// * struct variants → fields serialized as a flat object payload
/// * tuple variants → fields serialized as a positional array
///
/// ```ignore
/// #[derive(Emit)]
/// pub enum DialogEvent {
///     Close,
///     Confirm { value: String },
/// }
///
/// emit_event(DialogEvent::Close);
/// emit_event(DialogEvent::Confirm { value: "ok".into() });
/// ```
#[proc_macro_derive(Emit)]
pub fn derive_emit(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let enum_ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let Data::Enum(data) = &input.data else {
        return syn::Error::new_spanned(
            &input.ident,
            "#[derive(Emit)] can only be applied to enums",
        )
        .to_compile_error()
        .into();
    };

    let mut name_arms = Vec::new();
    let mut detail_arms = Vec::new();
    let mut from_event_arms = Vec::new();
    let mut name_literals = Vec::new();
    for variant in &data.variants {
        let var_ident = &variant.ident;
        let kebab = kebab_case(&var_ident.to_string());
        name_literals.push(kebab.clone());
        match &variant.fields {
            Fields::Unit => {
                name_arms.push(quote! { Self::#var_ident => #kebab, });
                detail_arms.push(quote! {
                    Self::#var_ident => {
                        ::pocopine::__private::serde_wasm_bindgen::to_value(
                            &::core::option::Option::<()>::None,
                        )
                    }
                });
                from_event_arms.push(quote! {
                    #kebab => ::core::option::Option::Some(Self::#var_ident),
                });
            }
            Fields::Named(named) => {
                let field_idents: Vec<_> = named
                    .named
                    .iter()
                    .map(|f| f.ident.clone().unwrap())
                    .collect();
                let field_tys: Vec<_> = named.named.iter().map(|f| f.ty.clone()).collect();
                name_arms.push(quote! {
                    Self::#var_ident { .. } => #kebab,
                });
                detail_arms.push(quote! {
                    Self::#var_ident { #(#field_idents),* } => {
                        #[derive(::pocopine::__private::serde::Serialize)]
                        #[serde(crate = "::pocopine::__private::serde")]
                        struct __PocoEmitPayload<'__a> {
                            #(#field_idents: &'__a #field_tys,)*
                        }
                        let __payload = __PocoEmitPayload {
                            #(#field_idents,)*
                        };
                        ::pocopine::__private::serde_wasm_bindgen::to_value(&__payload)
                    }
                });
                from_event_arms.push(quote! {
                    #kebab => {
                        #[derive(::pocopine::__private::serde::Deserialize)]
                        #[serde(crate = "::pocopine::__private::serde")]
                        struct __PocoEmitPayload {
                            #(#field_idents: #field_tys,)*
                        }
                        let __payload: __PocoEmitPayload =
                            ::pocopine::__private::serde_wasm_bindgen::from_value(detail).ok()?;
                        ::core::option::Option::Some(Self::#var_ident {
                            #(#field_idents: __payload.#field_idents,)*
                        })
                    }
                });
            }
            Fields::Unnamed(unnamed) => {
                let bindings: Vec<_> = (0..unnamed.unnamed.len())
                    .map(|i| format_ident!("__f{}", i))
                    .collect();
                let field_tys: Vec<_> = unnamed.unnamed.iter().map(|f| f.ty.clone()).collect();
                let positional: Vec<_> = (0..unnamed.unnamed.len()).map(syn::Index::from).collect();
                name_arms.push(quote! {
                    Self::#var_ident(..) => #kebab,
                });
                detail_arms.push(quote! {
                    Self::#var_ident(#(#bindings),*) => {
                        ::pocopine::__private::serde_wasm_bindgen::to_value(
                            &(#(#bindings,)*),
                        )
                    }
                });
                from_event_arms.push(quote! {
                    #kebab => {
                        let __tuple: ( #(#field_tys,)* ) =
                            ::pocopine::__private::serde_wasm_bindgen::from_value(detail).ok()?;
                        ::core::option::Option::Some(Self::#var_ident(
                            #(__tuple.#positional,)*
                        ))
                    }
                });
            }
        }
    }

    let out = quote! {
        impl #impl_generics ::pocopine::Emit for #enum_ident #ty_generics #where_clause {
            fn event_name(&self) -> &'static str {
                match self {
                    #(#name_arms)*
                }
            }
            fn to_detail(
                &self,
            ) -> ::core::result::Result<
                ::pocopine::__private::wasm_bindgen::JsValue,
                ::pocopine::__private::serde_wasm_bindgen::Error,
            > {
                match self {
                    #(#detail_arms)*
                }
            }
            fn event_names() -> &'static [&'static str] {
                &[ #(#name_literals,)* ]
            }
            fn from_event(
                name: &str,
                detail: ::pocopine::__private::wasm_bindgen::JsValue,
            ) -> ::core::option::Option<Self> {
                let _ = detail;
                match name {
                    #(#from_event_arms)*
                    _ => ::core::option::Option::None,
                }
            }
        }
    };
    out.into()
}

/// `#[derive(RouteComponent)]` — opt a component into client routing.
///
/// Expands to the empty impl —
/// `impl ::pocopine::RouteComponent for MyComponent {}` — so the
/// component satisfies the `App::route::<C>()` bound and inherits the
/// default (guard-free, loader-free) `RouteConfig`.
///
/// Derive when the route needs no route-local configuration. Hand-impl
/// `RouteComponent` instead when the component overrides `config()` —
/// guards, loaders, page metadata, prefetch policy:
///
/// ```ignore
/// // No guards/loaders — derive.
/// #[derive(Default, Serialize, Deserialize, RouteComponent)]
/// #[component]
/// pub struct About {}
///
/// // Route-local config — hand-impl.
/// impl RouteComponent for Dashboard {
///     fn config() -> RouteConfig<Self> {
///         RouteConfig::new().guard(predicate_guard(members_only))
///     }
/// }
/// ```
///
/// A blanket `impl<T: Component> RouteComponent for T` was considered
/// and rejected: it would conflict with every hand-written `config()`
/// override and erase the opt-in bound that keeps guarded components
/// from being mounted unguarded.
#[proc_macro_derive(RouteComponent)]
pub fn derive_route_component(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let out = quote! {
        impl #impl_generics ::pocopine::RouteComponent
            for #ident #ty_generics #where_clause {}
    };
    out.into()
}

// ── RFC-044 §5.10 — `#[derive(Props)]` ────────────────────────────

/// Generates a `Props` impl for a flatten-container struct.
///
/// Fields marked `#[prop]` become wire leaves (in declaration order);
/// unmarked fields are ignored. Leaf field types must implement
/// `PropValue` (impls ship for `String`, `bool`, the int/float
/// numerics, and `Option<T: PropValue>`).
///
/// ```ignore
/// #[derive(Props, Default, Clone, PartialEq, Serialize, Deserialize)]
/// pub struct SeriesCommon {
///     #[prop] pub key: String,
///     #[prop] pub label: String,
///     #[prop] pub color: String,
///     #[prop] pub visible: bool,
/// }
/// ```
#[proc_macro_derive(Props, attributes(prop))]
pub fn derive_props(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(
            &input.ident,
            "#[derive(Props)] can only be applied to structs",
        )
        .to_compile_error()
        .into();
    };

    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(
            &input.ident,
            "#[derive(Props)] requires a struct with named fields",
        )
        .to_compile_error()
        .into();
    };

    // Collect (ident, type) for every field marked `#[prop]`. Order
    // follows declaration order, which is what `prop_leaves()`
    // returns. Unmarked fields are internal — same role split as
    // `#[component]` (RFC-031): `#[prop]` = exposed, default = state.
    let mut leaves: Vec<(syn::Ident, syn::Type)> = Vec::new();
    for field in fields.named.iter() {
        if !field.attrs.iter().any(|a| a.path().is_ident("prop")) {
            continue;
        }
        let Some(ident) = field.ident.clone() else {
            continue;
        };
        leaves.push((ident, field.ty.clone()));
    }

    if leaves.is_empty() {
        return syn::Error::new_spanned(
            &input.ident,
            "#[derive(Props)] requires at least one field marked `#[prop]`",
        )
        .to_compile_error()
        .into();
    }

    let leaf_names: Vec<String> = leaves
        .iter()
        .map(|(i, _)| i.to_string().trim_start_matches("r#").to_string())
        .collect();

    let get_arms = leaves.iter().zip(leaf_names.iter()).map(|((id, _), name)| {
        quote! {
            #name => ::pocopine::__private::PropValue::to_prop_js(&self.#id),
        }
    });

    let set_arms = leaves
        .iter()
        .zip(leaf_names.iter())
        .map(|((id, ty), name)| {
            quote! {
                #name => {
                    if let ::core::option::Option::Some(__v) =
                        <#ty as ::pocopine::__private::PropValue>::from_prop_js(value)
                    {
                        self.#id = __v;
                    }
                }
            }
        });

    let kind_arms = leaves.iter().zip(leaf_names.iter()).map(|((_, ty), name)| {
        quote! {
            #name => <#ty as ::pocopine::__private::PropValue>::prop_static_kind(),
        }
    });

    let out = quote! {
        impl #impl_generics ::pocopine::__private::Props for #struct_ident #ty_generics
            #where_clause
        {
            fn prop_leaves() -> &'static [&'static str] {
                &[#(#leaf_names),*]
            }
            fn prop_get(
                &self,
                leaf: &str,
            ) -> ::pocopine::__private::JsValue {
                match leaf {
                    #(#get_arms)*
                    _ => ::pocopine::__private::JsValue::UNDEFINED,
                }
            }
            fn prop_set(
                &mut self,
                leaf: &str,
                value: ::pocopine::__private::JsValue,
            ) {
                match leaf {
                    #(#set_arms)*
                    _ => {
                        let _ = value;
                    }
                }
            }
            fn prop_static_kind(
                leaf: &str,
            ) -> ::pocopine::__private::StaticPropKind {
                match leaf {
                    #(#kind_arms)*
                    _ => ::pocopine::__private::StaticPropKind::Auto,
                }
            }
        }

        // RFC 084 — inherent `const` mirror of `prop_leaves()` for
        // const-eval consumers (typed-slot validation in particular).
        // The trait fn `prop_leaves()` returns the same slice but
        // isn't const, so const blocks can't call it. Exposing the
        // leaves as an inherent const lets `#[slot(props = T)]`
        // validation compare publication keys against this slice at
        // `cargo check` time.
        impl #impl_generics #struct_ident #ty_generics #where_clause {
            #[doc(hidden)]
            pub const __POC_PROP_LEAVES: &'static [&'static str] = &[#(#leaf_names),*];
        }
    };
    out.into()
}

// ── RFC 060 Tier 4 — `app!{}` macro ───────────────────────────────

/// One entry in the `components: [...]` list.
enum AppComponentEntry {
    /// `Home` — kebab-cased ident is the phf key.
    Bare(syn::Path),
    /// `(Home, "custom-tag")` — explicit override when the
    /// component declared `#[component(name = "...")]`.
    Explicit(syn::Path, LitStr),
}

/// One entry in the `routes: [...]` list — `("/path", Home)`.
struct AppRouteEntry {
    pattern: LitStr,
    component: syn::Path,
}

/// Parsed body of the `app!{}` macro: explicit component list,
/// optional plugin list, and route list. Per RFC 060 §8 Q1's
/// chosen mechanism (Option b1):
/// users list every reachable component explicitly so the macro
/// can emit a `phf::phf_map!` literal at expansion time.
struct AppMacroInput {
    plugins: Vec<Expr>,
    components: Vec<AppComponentEntry>,
    routes: Vec<AppRouteEntry>,
}

impl Parse for AppMacroInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut plugins: Option<Vec<Expr>> = None;
        let mut components: Option<Vec<AppComponentEntry>> = None;
        let mut routes: Option<Vec<AppRouteEntry>> = None;

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            let _: Token![:] = input.parse()?;
            let value: Expr = input.parse()?;
            // optional trailing comma between sections
            let _ = input.parse::<Token![,]>();

            match key.to_string().as_str() {
                "plugins" => {
                    if plugins.is_some() {
                        return Err(syn::Error::new(key.span(), "duplicate `plugins:`"));
                    }
                    plugins = Some(parse_plugins_array(value)?);
                }
                "components" => {
                    if components.is_some() {
                        return Err(syn::Error::new(key.span(), "duplicate `components:`"));
                    }
                    components = Some(parse_components_array(value)?);
                }
                "routes" => {
                    if routes.is_some() {
                        return Err(syn::Error::new(key.span(), "duplicate `routes:`"));
                    }
                    routes = Some(parse_routes_array(value)?);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown `app!{{}}` section `{other}` — expected `plugins:`, `components:`, or `routes:`"
                        ),
                    ));
                }
            }
        }

        let components = components.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`app!{}` requires a `components: [...]` section",
            )
        })?;
        let routes = routes.unwrap_or_default();
        let plugins = plugins.unwrap_or_default();

        Ok(AppMacroInput {
            plugins,
            components,
            routes,
        })
    }
}

fn parse_plugins_array(value: Expr) -> syn::Result<Vec<Expr>> {
    let Expr::Array(arr) = value else {
        return Err(syn::Error::new_spanned(
            &value,
            "`plugins` expects an array literal — `plugins: [observability_plugin()]`",
        ));
    };
    Ok(arr.elems.into_iter().collect())
}

fn parse_components_array(value: Expr) -> syn::Result<Vec<AppComponentEntry>> {
    let Expr::Array(arr) = value else {
        return Err(syn::Error::new_spanned(
            &value,
            "`components` expects an array literal — \
             `components: [Home, About, (CustomNamed, \"custom-tag\")]`",
        ));
    };
    let mut out = Vec::with_capacity(arr.elems.len());
    for elem in arr.elems {
        match elem {
            Expr::Path(syn::ExprPath { path, .. }) => out.push(AppComponentEntry::Bare(path)),
            Expr::Tuple(tup) => {
                if tup.elems.len() != 2 {
                    return Err(syn::Error::new_spanned(
                        &tup,
                        "tuple `components` entries must be `(TypePath, \"tag\")` — exactly two elements",
                    ));
                }
                let mut iter = tup.elems.into_iter();
                let path = match iter.next().unwrap() {
                    Expr::Path(p) => p.path,
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "first tuple element must be a type path",
                        ));
                    }
                };
                let lit = match iter.next().unwrap() {
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }) => s,
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "second tuple element must be a string literal tag",
                        ));
                    }
                };
                out.push(AppComponentEntry::Explicit(path, lit));
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "components entries are either bare type paths or `(TypePath, \"tag\")` tuples",
                ));
            }
        }
    }
    Ok(out)
}

fn parse_routes_array(value: Expr) -> syn::Result<Vec<AppRouteEntry>> {
    let Expr::Array(arr) = value else {
        return Err(syn::Error::new_spanned(
            &value,
            "`routes` expects an array literal — `routes: [(\"/\", Home), (\"/about\", About)]`",
        ));
    };
    let mut out = Vec::with_capacity(arr.elems.len());
    for elem in arr.elems {
        let Expr::Tuple(tup) = elem else {
            return Err(syn::Error::new_spanned(
                &elem,
                "route entries must be `(\"/path\", TypePath)` tuples",
            ));
        };
        if tup.elems.len() != 2 {
            return Err(syn::Error::new_spanned(
                &tup,
                "route entries must be `(\"/path\", TypePath)` — exactly two elements",
            ));
        }
        let mut iter = tup.elems.into_iter();
        let pattern = match iter.next().unwrap() {
            Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) => s,
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "first route element must be a string literal pattern",
                ));
            }
        };
        let component = match iter.next().unwrap() {
            Expr::Path(p) => p.path,
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "second route element must be a component type path",
                ));
            }
        };
        out.push(AppRouteEntry { pattern, component });
    }
    Ok(out)
}

/// `app!{}` — single-site macro that emits a `&'static
/// phf::Map<&'static str, &'static ComponentVTable>` from an
/// explicit `components: [...]` list, then runs an
/// `App::new()...route::<T>(...).run_with_registry(&REGISTRY)`
/// chain.
///
/// Shape:
///
/// ```ignore
/// fn main() {
///     pocopine::app! {
///         components: [
///             Home,
///             About,
///             pine::PineButton,
///             (CustomNamed, "custom-tag"),  // override for `#[component(name = "...")]`
///         ],
///         plugins: [
///             my_observability_plugin(),
///         ],
///         routes: [
///             ("/", Home),
///             ("/about", About),
///         ],
///     };
/// }
/// ```
///
/// Bundles (`#[component(extends = [...])]`) can appear in
/// `components`, but their members must also be listed
/// explicitly — proc-macros can't read const slices at
/// expansion time, so transitive bundle expansion happens at
/// runtime via `Bundle::register()` rather than statically in
/// the phf map. Future work (RFC 061 / 062) may relax this if a
/// type-level introspection mechanism becomes available.
#[proc_macro]
pub fn app(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as AppMacroInput);

    // RFC 060 — every route target must appear in `components:`
    // for the `&'static phf::Map` to be authoritative. Compare
    // token strings — the user is expected to write the same
    // path form in both lists. A `Home` route matched against a
    // `crate::Home` component is a false negative we accept:
    // both forms resolve to the same vtable at runtime, so the
    // diagnostic just asks the user to align the paths.
    let component_path_keys: std::collections::HashSet<String> = parsed
        .components
        .iter()
        .map(|c| match c {
            AppComponentEntry::Bare(p) => quote! { #p }.to_string(),
            AppComponentEntry::Explicit(p, _) => quote! { #p }.to_string(),
        })
        .collect();
    for r in &parsed.routes {
        let path = &r.component;
        let key = quote! { #path }.to_string();
        if !component_path_keys.contains(&key) {
            return syn::Error::new_spanned(
                path,
                format!(
                    "route target `{key}` is not declared in `components: [...]` \
                     — the static phf registry must be exhaustive (RFC 060 §4.3). \
                     Add `{key}` to the `components:` list, or use the same path \
                     form on both sides if you got an unexpected mismatch."
                ),
            )
            .to_compile_error()
            .into();
        }
    }

    // Resolve each component entry to (key, vtable_path).
    let entries = parsed.components.iter().map(|c| match c {
        AppComponentEntry::Bare(path) => {
            let last = path.segments.last().expect("path has at least one segment");
            let kebab = kebab_case(&last.ident.to_string());
            let vtable_path = quote! { #path::__POCO_VTABLE };
            (kebab, vtable_path)
        }
        AppComponentEntry::Explicit(path, lit) => {
            let kebab = lit.value();
            let vtable_path = quote! { #path::__POCO_VTABLE };
            (kebab, vtable_path)
        }
    });

    let phf_arms = entries.map(|(key, vtable)| {
        quote! { #key => #vtable, }
    });
    let component_names = parsed.components.iter().map(|c| match c {
        AppComponentEntry::Bare(path) => {
            let last = path.segments.last().expect("path has at least one segment");
            kebab_case(&last.ident.to_string())
        }
        AppComponentEntry::Explicit(_, lit) => lit.value(),
    });

    // Use `route_static` (record-only) so `C::register()` doesn't
    // fire here — `run_with_registry` is the one and only
    // registration path, keeping the phf map authoritative.
    let route_calls = parsed.routes.iter().map(|r| {
        let pattern = &r.pattern;
        let component = &r.component;
        quote! { .route_static::<#component>(#pattern) }
    });
    let component_static_calls = component_names.map(|name| {
        quote! { .component_static(#name) }
    });
    let plugin_calls = parsed.plugins.iter().map(|plugin| {
        quote! { .plugin(#plugin) }
    });

    let out = quote! {
        {
            static REGISTRY: ::pocopine::__private::phf::Map<
                &'static str,
                &'static ::pocopine::__private::ComponentVTable,
            > = ::pocopine::__private::phf::phf_map! {
                #(#phf_arms)*
            };

            ::pocopine::App::new()
                #(#component_static_calls)*
                #(#route_calls)*
                #(#plugin_calls)*
                .run_with_registry(&REGISTRY);
        }
    };
    out.into()
}

/// RFC-100 — `asset!("blog/video.webm")` expands to a
/// content-addressed asset URL.
///
/// At expansion time the macro resolves the path against the calling
/// crate's `assets/` directory (`$CARGO_MANIFEST_DIR/assets/<path>`),
/// reads the file, and hashes it (8-lowercase-hex sha256 prefix via
/// `pocopine-crypto`). Absolute paths, `..` segments, and missing
/// files are compile errors. The expansion is:
///
/// ```ignore
/// ::pocopine::__private::asset_url("blog/video.webm", "a3f81c2d")
/// ```
///
/// `asset_url` resolves the base at **runtime**
/// (`window.__POCOPINE_ASSET_BASE` on wasm, the `POCOPINE_ASSET_BASE`
/// env var on the server, `/assets` default), so the same expansion
/// serves dev, SSR, and a CDN-fronted deploy.
///
/// # Rebuild correctness (RFC-100 §6)
///
/// The hash is computed when the calling crate compiles. The macro
/// deliberately does **not** emit `include_bytes!` to register the
/// asset as a rebuild dependency — that would embed arbitrarily large
/// files in the rlib/wasm just for dependency tracking. Instead,
/// `pocopine build`/`run`/`dev`/`deploy` set
/// `POCOPINE_ASSETS_FINGERPRINT` (a combined hash of the app's
/// `assets/` tree) before every cargo invocation, and — when that env
/// is set at expansion time — the macro emits an
/// `option_env!("POCOPINE_ASSETS_FINGERPRINT")` reference that rustc
/// records as an env dependency of the calling crate. Editing any
/// asset changes the fingerprint, which makes cargo recompile the
/// crate and re-expand `asset!` with the fresh hash (mechanism proven
/// end-to-end in `crates/pocopine-cli/tests/fingerprint_tracking.rs`).
///
/// Builds driven by **bare cargo** (or rust-analyzer) don't set the
/// fingerprint env, emit no tracking, and can therefore hold a stale
/// hash after an asset edit; the dev server answers stale hashes with
/// `409` and an explanation as the backstop.
#[proc_macro]
pub fn asset(input: TokenStream) -> TokenStream {
    assets::expand(input)
}
