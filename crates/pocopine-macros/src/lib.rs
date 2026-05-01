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
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream, Parser},
    parse_macro_input,
    punctuated::Punctuated,
    Data, DeriveInput, Expr, ExprLit, Fields, FnArg, ImplItem, ItemFn, ItemImpl, ItemStruct, Lit,
    LitStr, Meta, MetaNameValue, Pat, PatType, Path, Token, Type,
};

// RFC 050 — compile-time `.poco` template parser + diagnostic
// renderer. Both host-only (proc-macro crate), invisible to wasm.
mod diagnostics;
mod template_parser;
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
mod template_plan;

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
    // Tier 3: give up silently (not an error). If neither
    // lookup found a file, we return Ok((None, None)) and the
    // caller emits the normal component registration with no
    // validation. The real cargo build retries with a
    // file-backed span so any real template bug still gets
    // caught at build time.
    let resolved_path = resolve_template_path(template_path);
    let resolved_path = match resolved_path {
        Some(p) => p,
        None => return Ok((None, None)),
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
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        if let Ok(rel) = path.strip_prefix(&manifest_dir) {
            return rel.to_string_lossy().to_string();
        }
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
/// Returns `None` when neither tier finds an existing file
/// — the caller treats that as "silent skip," not an error.
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
        // Zero matches → file doesn't exist; nothing to
        // validate. More than one match → ambiguous; refuse
        // to guess. Either way, silent skip.
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
            } else {
                return Err(syn::Error::new_spanned(
                    kv.path,
                    "unknown key — expected one of: name, template, template_inline, \
                     style, role, display, transition, transition_in, transition_out, \
                     animate, uses, extends",
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

#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match ComponentArgs::parse.parse(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let mut input = parse_macro_input!(item as ItemStruct);

    let struct_ident = input.ident.clone();
    let ident_str = struct_ident.to_string();
    let default_name = kebab_case(&ident_str);
    let name_str = args
        .name
        .as_ref()
        .map(|s| s.value())
        .unwrap_or_else(|| default_name.clone());

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
    if let Some(paths) = args.extends.as_ref() {
        if paths.is_empty() {
            return syn::Error::new_spanned(
                &struct_ident,
                "`extends = []` is empty — bundle markers must list at least one member, \
                 otherwise the type is a no-op. Drop the attribute or list the components.",
            )
            .to_compile_error()
            .into();
        }
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
        let extends_vtables = extends_paths.iter().map(|path| {
            quote! { #path::__POCO_VTABLE }
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

                #[doc(hidden)]
                pub fn __poco_uses() -> &'static [&'static ::pocopine::__private::ComponentVTable] {
                    static USES: &[&'static ::pocopine::__private::ComponentVTable] = &[
                        #(#extends_vtables),*
                    ];
                    USES
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
                        uses: <#struct_ident>::__poco_uses,
                        is_bundle: true,
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
    let mut field_is_prop: Vec<bool> = Vec::new();
    let mut field_is_model: Vec<bool> = Vec::new();
    let mut field_model_names: Vec<Option<String>> = Vec::new();
    let mut observes: Vec<ObserveEntry> = Vec::new();
    // RFC-044 §5.10 — `#[model(flatten = [...])]` fields. Each entry
    // is `(container_ident, leaf_names)`. The container itself is a
    // normal state field in `field_*` — not prop, not model — and
    // each leaf is synthesised as an independent prop+model public
    // key that routes get/set through the container's serde impl.
    let mut flatten_fields: Vec<(syn::Ident, Vec<String>)> = Vec::new();
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
        field.attrs.retain(|a| {
            if a.path().is_ident("prop") {
                is_prop = true;
                return false;
            }
            if a.path().is_ident("model") {
                // Shapes accepted:
                //   #[model]                                  bare
                //   #[model(name = "…")]                      wire-name rename
                //   #[model(flatten = ["leaf1", "leaf2"])]    per-leaf wire shape
                //   #[model(flatten)]                         (reserved — see below)
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
                                // Bare `#[model(flatten)]` —
                                // auto-discovery form per RFC-044
                                // §5.10. Reserved for a follow-up
                                // PR that adds the runtime leaves
                                // side-table; today's macro emits
                                // static match arms and can't
                                // produce those without knowing
                                // the leaf list.
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
                        if bare && flatten.is_none() {
                            observe_err = Some(syn::Error::new_spanned(
                                a,
                                "bare #[model(flatten)] auto-discovery is not yet \
                                 implemented — provide an explicit leaf list: \
                                 #[model(flatten = [\"field1\", \"field2\"])]",
                            ));
                        } else if let Some(leaves) = flatten {
                            // Container is internal — not prop, not
                            // model. Leaves take those roles (added
                            // to `flatten_fields` below, spliced into
                            // codegen after the per-field loop).
                            is_prop = false;
                            is_model = false;
                            model_name = None;
                            // Stash for post-loop splice. Can't push
                            // directly yet because `ident` hasn't been
                            // fully cloned out of the enclosing scope.
                            // Using a sentinel: empty Vec means "will
                            // populate after parse".
                            flatten_fields.push((ident.clone(), leaves));
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
        let _ = field_ty;
        field_names.push(rust_name.clone());
        field_idents.push(ident);
        field_is_prop.push(is_prop);
        field_is_model.push(is_model);
        field_model_names.push(model_name.or_else(|| is_model.then_some(rust_name)));
    }

    // RFC-044 §5.10 flatten-leaf codegen. For each
    // `#[model(flatten = ["a", "b"])]` container field, each leaf
    // becomes a synthetic public key that:
    //
    //   - `get(leaf)` / `get_model_value(leaf)`: serialise the
    //     container once, read the leaf key off the resulting
    //     JS object.
    //   - `set(leaf, value)`: serialise the container, splice the
    //     leaf, deserialise back into the container field.
    //   - `keys()` includes the leaf (without it, `snapshot_models`
    //     in the model runtime wouldn't iterate it and emits would
    //     never fire).
    //   - `is_prop(leaf)` / `is_model(leaf)`: true (parent
    //     mirror-in via pp-model:leaf; emit via per-leaf channel).
    //   - `model_name(leaf)`: the leaf itself.
    //
    // One serde round-trip per inbound mirror-in write; same order
    // as the pre-landing `Option<T>` empty-string shim. Outbound
    // emission uses the same path the non-flatten struct form
    // already walks — the snapshot-diff machinery in
    // `model_runtime.rs` iterates leaves just like any other
    // `is_model` key.
    let flatten_leaf_get_arms = flatten_fields.iter().flat_map(|(container, leaves)| {
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

    let flatten_leaf_set_arms = flatten_fields.iter().flat_map(|(container, leaves)| {
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

    let flatten_leaf_names: Vec<&String> = flatten_fields
        .iter()
        .flat_map(|(_, leaves)| leaves.iter())
        .collect();

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
    // `pp-model:<leaf>`) and as models (emit via `pp:update:<leaf>`).
    let prop_field_names: Vec<&String> = field_names
        .iter()
        .zip(field_is_prop.iter())
        .filter_map(|(n, is_prop)| is_prop.then_some(n))
        .chain(flatten_leaf_names.iter().copied())
        .collect();
    // `matches!(key, a | b | c)` needs at least one pattern —
    // fall back to a `false` literal when no field is a prop.
    let is_prop_body = if prop_field_names.is_empty() {
        quote! { let _ = key; false }
    } else {
        quote! { matches!(key, #(#prop_field_names)|*) }
    };

    let model_field_names: Vec<&String> = field_names
        .iter()
        .zip(field_is_model.iter())
        .filter_map(|(n, is_model)| is_model.then_some(n))
        .chain(flatten_leaf_names.iter().copied())
        .collect();
    let is_model_body = if model_field_names.is_empty() {
        quote! { let _ = key; false }
    } else {
        quote! { matches!(key, #(#model_field_names)|*) }
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
        .chain(flatten_leaf_names.iter().copied().map(|leaf| {
            // Leaves have no separate wire-name rename — the leaf
            // literal IS the wire name.
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
            )
        })
        .unwrap_or_else(|| template_plan::EmittedTemplatePlan {
            plan_tokens: None,
            cleaned_html: None,
            slot_fragment_fns: proc_macro2::TokenStream::new(),
            if_body_fns: proc_macro2::TokenStream::new(),
            specialized_mount_body: None,
        });

    // Build the literal to feed into `compile_template`:
    //
    //   * Template plan present  → cleaned HTML with classified
    //     attributes stripped + `data-pp-text-managed` markers.
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
            source.to_string()
        } else {
            inline.value()
        }
    } else if row_plans.stamps.is_empty() {
        template_ast
            .as_ref()
            .map(|ast| ast.source.as_str().to_string())
            .unwrap_or_default()
    } else {
        let source = template_ast
            .as_ref()
            .map(|ast| ast.source.as_str())
            .unwrap_or("");
        for_plan::apply_stamps(source, &row_plans.stamps)
    };

    let compiled_template_html = compile_template_static(
        &template_source_for_compile,
        &name_str,
        role_for_static_template
            .as_ref()
            .map(|(tag, role_name)| (*tag, role_name.as_str())),
    );
    let compiled_template_html_lit = proc_macro2::Literal::string(&compiled_template_html);

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
    let register_display_stmt: proc_macro2::TokenStream = match args.display.as_ref() {
        Some(lit) => {
            let value = lit.value();
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
    let uses_vtables: proc_macro2::TokenStream = match args.uses.as_ref() {
        Some(table) => {
            let vtables = table.entries.iter().map(|(_tag, type_path)| {
                quote! { #type_path::__POCO_VTABLE }
            });
            quote! { &[#(#vtables),*] }
        }
        None => quote! { &[] },
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
                ::pocopine::watch_scope_field::<#field_ty, _>(
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
    let has_observes = !observes.is_empty();
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
            #[doc(hidden)]
            pub const __POCOPINE_HAS_OBSERVES: bool = #has_observes;
        }
    };

    let out = quote! {
        #input
        #template_plan_item_tokens

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

        // RFC 063 — hard `compile_error!` for every directive
        // RFC 063 §4.1 deletes (`pp-cloak` today; more in
        // follow-up commits).
        #forbidden_directive_diagnostics_tokens

        #observe_impl

        impl ::pocopine::__private::ComponentState for #struct_ident {
            fn get(&self, key: &str) -> ::pocopine::__private::JsValue {
                match key {
                    #(#get_arms)*
                    _ => <Self as ::pocopine::__private::HandlerDispatch>::computed_get(self, key)
                        .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
                }
            }
            fn set(&mut self, key: &str, value: ::pocopine::__private::JsValue) {
                match key {
                    #(#set_arms)*
                    _ => {}
                }
            }
            fn keys(&self) -> &'static [&'static str] {
                static __POCOPINE_KEYS: ::std::sync::OnceLock<&'static [&'static str]> =
                    ::std::sync::OnceLock::new();
                *__POCOPINE_KEYS.get_or_init(|| {
                    let mut __keys = vec![#(#keys_arr),*];
                    __keys.extend_from_slice(
                        <Self as ::pocopine::__private::HandlerDispatch>::computed_keys(),
                    );
                    let __boxed: ::std::boxed::Box<[&'static str]> = __keys.into_boxed_slice();
                    ::std::boxed::Box::leak(__boxed)
                })
            }
            fn is_prop(&self, key: &str) -> bool {
                #is_prop_body
            }
            fn is_model(&self, key: &str) -> bool {
                #is_model_body
            }
            fn model_name(&self, key: &str) -> ::core::option::Option<&'static str> {
                match key {
                    #(#model_name_arms)*
                    _ => ::core::option::Option::None,
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
            fn has_on_unmount(&self) -> bool {
                <Self as ::pocopine::__private::HandlerDispatch>::has_on_unmount(self)
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

            #[doc(hidden)]
            pub fn __poco_uses() -> &'static [&'static ::pocopine::__private::ComponentVTable] {
                static USES: &[&'static ::pocopine::__private::ComponentVTable] = #uses_vtables;
                USES
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
                    uses: <#struct_ident>::__poco_uses,
                    is_bundle: false,
                    template_html: Some(#compiled_template_html_lit),
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
    };

    out.into()
}

#[proc_macro_attribute]
pub fn handlers(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemImpl);
    let ty = input.self_ty.clone();

    let mut arms = Vec::new();
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
        let Some(_receiver) = method.sig.receiver() else {
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
        arms.push(quote! {
            #name => {
                #(#conversions)*
                Self::#ident(self #(, #bindings)*);
                ::pocopine::__private::JsValue::UNDEFINED
            }
        });
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
                ::pocopine::watch_scope_field_now::<#v_ty, _>(__scope, #field_name, move |new, prev| {
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
            fn has_on_unmount(&self) -> bool { true }
        }
    });

    let out = quote! {
        #input

        #computed_impl

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

    let get_arms = field_idents
        .iter()
        .zip(field_names.iter())
        .map(|(id, name)| {
            quote! {
                #name => ::pocopine::__private::serde_wasm_bindgen::to_value(&self.#id)
                    .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
            }
        });
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

    let out = quote! {
        #input

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
            #[doc(hidden)]
            pub const __POCOPINE_HAS_OBSERVES: bool = false;
        }

        impl ::pocopine::__private::ComponentState for #struct_ident {
            fn get(&self, key: &str) -> ::pocopine::__private::JsValue {
                match key {
                    #(#get_arms)*
                    _ => ::pocopine::__private::JsValue::UNDEFINED,
                }
            }
            fn set(&mut self, key: &str, value: ::pocopine::__private::JsValue) {
                match key {
                    #(#set_arms)*
                    _ => {}
                }
            }
            fn keys(&self) -> &'static [&'static str] {
                &[#(#keys_arr),*]
            }
            fn invoke(
                &mut self,
                key: &str,
                args: &::pocopine::__private::js_sys::Array,
            ) -> ::pocopine::__private::JsValue {
                <Self as ::pocopine::__private::HandlerDispatch>::invoke_handler(self, key, args)
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
                let scope = ::pocopine::__private::Scope::new(instance);
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
///   `/_pocopine/<name>` and deserializes the JSON response as
///   `Result<R, ServerError>`. The user-supplied body is discarded on
///   this target.
/// * **non-wasm32** — the original user body, plus a helper
///   `__<name>_route(router) -> axum::Router` that registers the POST
///   route so a server binary can mount it.
///
/// The signature shape this milestone supports:
///
/// * `async fn name(arg1: T1, ..., argN: TN) -> Result<R, ServerError>`
/// * Every arg must be owned (`T`, not `&T` / `&mut T`). Args must
///   `Serialize + Deserialize`.
/// * Return type is ignored by this macro; the user is responsible for
///   having it round-trip through `serde_json`.
#[proc_macro_attribute]
pub fn server(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;

    let fn_ident = sig.ident.clone();
    let fn_name_str = fn_ident.to_string();
    let route_path = format!("/_pocopine/{fn_name_str}");
    let route_ident = format_ident!("__{fn_name_str}_route");

    // Collect (pat_ident, type) pairs, rejecting self / ref args.
    let mut arg_idents = Vec::new();
    let mut arg_types = Vec::new();
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
                arg_idents.push(pat_ident.ident.clone());
                arg_types.push((**ty).clone());
            }
        }
    }

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

    let sig_without_body = quote! { #vis #sig };

    let client = quote! {
        #[cfg(target_arch = "wasm32")]
        #sig_without_body {
            ::pocopine::fetch::call::<#args_tuple_type, _>(
                #route_path,
                &#args_tuple_value,
            ).await
        }
    };

    // On the server we preserve the user's body, plus emit a route helper.
    // The extractor destructures the JSON body into our ident tuple, then
    // we call the original function by name — Rust method resolution picks
    // the non-wasm32 definition.
    let server = quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #vis #sig #body

        #[cfg(not(target_arch = "wasm32"))]
        #[doc(hidden)]
        pub fn #route_ident(
            router: ::pocopine_server::axum::Router,
        ) -> ::pocopine_server::axum::Router {
            router.route(
                #route_path,
                ::pocopine_server::axum::routing::post(
                    |::pocopine_server::axum::Json(#destructure):
                        ::pocopine_server::axum::Json<#args_tuple_type>| async move {
                        let result = #fn_ident( #(#arg_idents),* ).await;
                        ::pocopine_server::axum::Json(result)
                    },
                ),
            )
        }
    };

    let out = quote! {
        #client
        #server
    };
    out.into()
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

/// Parsed body of the `app!{}` macro: explicit component list +
/// route list. Per RFC 060 §8 Q1's chosen mechanism (Option b1):
/// users list every reachable component explicitly so the macro
/// can emit a static registry at expansion time.
struct AppMacroInput {
    components: Vec<AppComponentEntry>,
    routes: Vec<AppRouteEntry>,
    devtools: bool,
}

impl Parse for AppMacroInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut components: Option<Vec<AppComponentEntry>> = None;
        let mut routes: Option<Vec<AppRouteEntry>> = None;
        let mut devtools: Option<bool> = None;

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            let _: Token![:] = input.parse()?;
            let value: Expr = input.parse()?;
            // optional trailing comma between sections
            let _ = input.parse::<Token![,]>();

            match key.to_string().as_str() {
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
                "devtools" => {
                    if devtools.is_some() {
                        return Err(syn::Error::new(key.span(), "duplicate `devtools:`"));
                    }
                    devtools = Some(parse_bool_literal(
                        value,
                        "`devtools` expects `true` or `false`",
                    )?);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown `app!{{}}` section `{other}` — expected `components:`, `routes:`, or `devtools:`"),
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

        Ok(AppMacroInput {
            components,
            routes,
            devtools: devtools.unwrap_or(false),
        })
    }
}

fn parse_bool_literal(value: Expr, msg: &'static str) -> syn::Result<bool> {
    match value {
        Expr::Lit(ExprLit {
            lit: Lit::Bool(value),
            ..
        }) => Ok(value.value),
        other => Err(syn::Error::new_spanned(other, msg)),
    }
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

/// `app!{}` — single-site macro that emits a static registry from
/// an explicit `components: [...]` list, then runs an
/// `App::new()...route::<T>(...).run_with_static_registry(REGISTRY)`
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
///         routes: [
///             ("/", Home),
///             ("/about", About),
///         ],
///     };
/// }
/// ```
///
/// Bundles (`#[component(extends = [...])]`) can appear in
/// `components`, but their members must also be listed explicitly
/// so RFC 065 can validate the whole route-cluster surface.
#[proc_macro]
pub fn app(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as AppMacroInput);
    let split_mode = std::env::var("POCOPINE_SPLIT_MODE").ok();
    let split_base =
        std::env::var("POCOPINE_SPLIT_BASE").unwrap_or_else(|_| "pocopine_app".to_string());

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

    if let Some(mode) = split_mode.as_deref() {
        if split_strict_enabled() {
            if let Err(err) = validate_split_convention(&parsed) {
                return err.to_compile_error().into();
            }
        }
        if mode == "shell" {
            if let Ok(path) = std::env::var("POCOPINE_SPLIT_ROUTE_COUNT_OUT") {
                if let Err(err) = std::fs::write(&path, parsed.routes.len().to_string()) {
                    return syn::Error::new(
                        proc_macro2::Span::call_site(),
                        format!("failed to write POCOPINE_SPLIT_ROUTE_COUNT_OUT `{path}`: {err}"),
                    )
                    .to_compile_error()
                    .into();
                }
            }
            if let Ok(path) = std::env::var("POCOPINE_SPLIT_ROUTE_IDS_OUT") {
                let route_ids = split_route_ids(&parsed)
                    .into_iter()
                    .map(|id| id.name)
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Err(err) = std::fs::write(&path, route_ids) {
                    return syn::Error::new(
                        proc_macro2::Span::call_site(),
                        format!("failed to write POCOPINE_SPLIT_ROUTE_IDS_OUT `{path}`: {err}"),
                    )
                    .to_compile_error()
                    .into();
                }
            }
            return emit_split_shell_app(&parsed, &split_base).into();
        }
        if let Some(raw_idx) = mode.strip_prefix("route:") {
            let Ok(route_idx) = raw_idx.parse::<usize>() else {
                return syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "POCOPINE_SPLIT_MODE route value must be `route:<index>`",
                )
                .to_compile_error()
                .into();
            };
            return emit_split_route_app(&parsed, route_idx).into();
        }
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "POCOPINE_SPLIT_MODE must be `shell` or `route:<index>`",
        )
        .to_compile_error()
        .into();
    }

    // Resolve each component entry to (key, vtable_path).
    let entries: Vec<_> = parsed
        .components
        .iter()
        .map(|c| match c {
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
        })
        .collect();

    let registry_entries = entries.iter().map(|(key, vtable)| {
        quote! { (#key, #vtable), }
    });
    let component_vtables = entries.iter().map(|(_key, vtable)| {
        quote! { #vtable }
    });

    // Use `route_static` (record-only) so `C::register()` doesn't
    // fire here — `run_with_registry` is the one and only
    // registration path, keeping the phf map authoritative.
    let route_calls = parsed.routes.iter().map(|r| {
        let pattern = &r.pattern;
        let component = &r.component;
        quote! { .route_static::<#component>(#pattern) }
    });
    let route_roots = parsed.routes.iter().enumerate().map(|(idx, r)| {
        let component = &r.component;
        let id = format!("r{idx}");
        quote! {
            ::pocopine::__private::RouteClusterRoot {
                route_id: #id,
                component: <#component as ::pocopine::__private::Component>::NAME,
            }
        }
    });
    let run_call = if parsed.routes.is_empty() {
        quote! { .run_with_static_registry(REGISTRY); }
    } else {
        quote! { .run_with_cluster_manifest(REGISTRY, &CLUSTER_MANIFEST); }
    };
    let devtools_call = parsed.devtools.then(|| quote! { .with_devtools() });

    let out = quote! {
        {
            static REGISTRY: &[(
                &'static str,
                &'static ::pocopine::__private::ComponentVTable,
            )] = &[
                #(#registry_entries)*
            ];
            static COMPONENTS: &[&'static ::pocopine::__private::ComponentVTable] = &[
                #(#component_vtables),*
            ];
            static ROUTE_ROOTS: &[::pocopine::__private::RouteClusterRoot] = &[
                #(#route_roots),*
            ];
            static CLUSTER_MANIFEST: ::pocopine::__private::ClusterManifest =
                ::pocopine::__private::ClusterManifest {
                    components: COMPONENTS,
                    routes: ROUTE_ROOTS,
                };

            ::pocopine::App::new()
                #(#route_calls)*
                #devtools_call
                #run_call
        }
    };
    out.into()
}

fn split_strict_enabled() -> bool {
    matches!(
        std::env::var("POCOPINE_SPLIT_STRICT").as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
}

fn validate_split_convention(parsed: &AppMacroInput) -> syn::Result<()> {
    for component in &parsed.components {
        let path = app_component_path(component);
        let owner = split_owner(path);
        if owner.is_none() {
            return Err(syn::Error::new_spanned(
                path,
                "split strict mode requires component paths to live under `shell`, `routes`, or `shared`; \
                 move this component to the split-ready layout or pass `--no-strict` for a non-release experiment",
            ));
        }
    }

    let route_targets: std::collections::HashSet<String> = parsed
        .routes
        .iter()
        .map(|route| {
            let component = &route.component;
            quote! { #component }.to_string()
        })
        .collect();
    for route in &parsed.routes {
        if split_owner(&route.component) != Some(SplitOwner::Routes) {
            return Err(syn::Error::new_spanned(
                &route.component,
                "split strict mode requires route targets under `routes::<name>::...`; \
                 shell components are always loaded and shared components are dependencies, not route roots",
            ));
        }
    }
    let mut seen_route_ids = std::collections::HashSet::new();
    for (idx, route) in parsed.routes.iter().enumerate() {
        let id = split_route_id(idx, &route.component);
        if !seen_route_ids.insert(id.clone()) {
            return Err(syn::Error::new_spanned(
                &route.component,
                format!(
                    "split strict mode derived duplicate route id `{id}`; \
                     put route roots under distinct `routes::<id>::...` modules"
                ),
            ));
        }
    }

    for component in parsed.components.iter().take_while(|component| {
        let path = app_component_path(component);
        !route_targets.contains(&quote! { #path }.to_string())
    }) {
        let path = app_component_path(component);
        if split_owner(path) != Some(SplitOwner::Shell) {
            return Err(syn::Error::new_spanned(
                path,
                "split strict mode treats components before the first route root as shell-owned; \
                 shell-owned components must live under `shell::...`. Move shared/route components after the route roots",
            ));
        }
    }

    Ok(())
}

struct SplitRouteId {
    name: String,
}

fn split_route_ids(parsed: &AppMacroInput) -> Vec<SplitRouteId> {
    parsed
        .routes
        .iter()
        .enumerate()
        .map(|(idx, route)| SplitRouteId {
            name: split_route_id(idx, &route.component),
        })
        .collect()
}

fn split_route_id(idx: usize, path: &syn::Path) -> String {
    let segments = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>();
    if let Some(route_pos) = segments.iter().position(|segment| segment == "routes") {
        if let Some(name) = segments.get(route_pos + 1) {
            return sanitize_split_id(name);
        }
    }
    let fallback = path
        .segments
        .last()
        .map(|segment| kebab_case(&segment.ident.to_string()).replace('-', "_"))
        .unwrap_or_else(|| format!("route_{idx}"));
    let fallback = sanitize_split_id(&fallback);
    if fallback.is_empty() {
        format!("route_{idx}")
    } else {
        fallback
    }
}

fn sanitize_split_id(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SplitOwner {
    Shell,
    Routes,
    Shared,
}

fn split_owner(path: &syn::Path) -> Option<SplitOwner> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .find_map(|segment| match segment.as_str() {
            "shell" => Some(SplitOwner::Shell),
            "routes" => Some(SplitOwner::Routes),
            "shared" => Some(SplitOwner::Shared),
            _ => None,
        })
}

fn app_component_path(c: &AppComponentEntry) -> &syn::Path {
    match c {
        AppComponentEntry::Bare(path) => path,
        AppComponentEntry::Explicit(path, _) => path,
    }
}

fn app_component_key(c: &AppComponentEntry) -> String {
    match c {
        AppComponentEntry::Bare(path) => {
            let last = path.segments.last().expect("path has at least one segment");
            kebab_case(&last.ident.to_string())
        }
        AppComponentEntry::Explicit(_, lit) => lit.value(),
    }
}

fn emit_split_shell_app(parsed: &AppMacroInput, split_base: &str) -> proc_macro2::TokenStream {
    let route_path_keys: std::collections::HashSet<String> = parsed
        .routes
        .iter()
        .map(|r| {
            let component = &r.component;
            quote! { #component }.to_string()
        })
        .collect();
    let shell_entries: Vec<_> = parsed
        .components
        .iter()
        .take_while(|component| {
            let path = app_component_path(component);
            !route_path_keys.contains(&quote! { #path }.to_string())
        })
        .filter(|component| {
            let path = app_component_path(component);
            !route_path_keys.contains(&quote! { #path }.to_string())
        })
        .map(|component| {
            let key = app_component_key(component);
            let path = app_component_path(component);
            let vtable_path = quote! { #path::__POCO_VTABLE };
            (key, vtable_path)
        })
        .collect();
    let registry_entries = shell_entries.iter().map(|(key, vtable)| {
        quote! { (#key, #vtable), }
    });
    let shell_registers = shell_entries.iter().map(|(_key, vtable)| {
        quote! { (#vtable.register)(); }
    });
    let route_root_markers = split_route_ids(parsed)
        .into_iter()
        .zip(parsed.routes.iter())
        .map(|(id, route)| {
            let marker = quote::format_ident!("__pocopine_split_route_root_{}", id.name);
            let component = &route.component;
            quote! {
                #[no_mangle]
                pub extern "C" fn #marker() {
                    <#component as ::pocopine::__private::Component>::register();
                    let __pocopine_host = unsafe {
                        &*::core::ptr::NonNull::<::pocopine::__private::web_sys::Element>::dangling().as_ptr()
                    };
                    let __pocopine_handle =
                        ::pocopine::App::mount_subtree::<#component>(__pocopine_host);
                    let _ = ::core::hint::black_box(__pocopine_handle);
                }
            }
        });
    let route_mount_arms = split_route_ids(parsed)
        .into_iter()
        .zip(parsed.routes.iter())
        .map(|(id, route)| {
            let id = id.name;
            let component = &route.component;
            let pattern = route.pattern.value();
            let param_setters = route_param_setters(&pattern);
            quote! {
                #id => {
                    __POCOPINE_ROUTE_HANDLE.with(|handle| {
                        if let ::core::option::Option::Some(handle) = handle.borrow_mut().take() {
                            handle.unmount();
                        }
                    });
                    let Some(document) = ::pocopine::__private::web_sys::window()
                        .and_then(|window| window.document())
                    else {
                        return;
                    };
                    let Ok(host) = document.create_element(
                        <#component as ::pocopine::__private::Component>::NAME
                    ) else {
                        return;
                    };
                    let __pocopine_path_segments: ::std::vec::Vec<&str> = path
                        .trim_matches('/')
                        .split('/')
                        .filter(|segment| !segment.is_empty())
                        .collect();
                    #(#param_setters)*
                    outlet.replace_children_with_node_1(host.as_ref());
                    let handle = ::pocopine::App::mount_subtree::<#component>(&host);
                    __POCOPINE_ROUTE_HANDLE.with(|slot| {
                        *slot.borrow_mut() = ::core::option::Option::Some(handle);
                    });
                }
            }
        });
    let devtools_call = parsed.devtools.then(|| quote! { .with_devtools() });
    let manifest = split_manifest_json(parsed, split_base);

    quote! {
        {
            static REGISTRY: &[(
                &'static str,
                &'static ::pocopine::__private::ComponentVTable,
            )] = &[
                #(#registry_entries)*
            ];

            #[no_mangle]
            pub extern "C" fn __pocopine_split_shell_root() {
                #(#shell_registers)*
            }

            #(#route_root_markers)*

            ::std::thread_local! {
                static __POCOPINE_DESCRIPTOR_ROUTE_HANDLE: ::std::cell::RefCell<::core::option::Option<::pocopine::SubtreeHandle>> =
                    const { ::std::cell::RefCell::new(::core::option::Option::None) };
                static __POCOPINE_ROUTE_HANDLE: ::std::cell::RefCell<::core::option::Option<::pocopine::SubtreeHandle>> =
                    const { ::std::cell::RefCell::new(::core::option::Option::None) };
            }

            #[::pocopine::__private::wasm_bindgen::prelude::wasm_bindgen]
            pub fn pocopine_split_manifest() -> ::std::string::String {
                #manifest.to_string()
            }

            #[::pocopine::__private::wasm_bindgen::prelude::wasm_bindgen]
            pub fn pocopine_split_unmount_route() {
                __POCOPINE_ROUTE_HANDLE.with(|handle| {
                    if let ::core::option::Option::Some(handle) = handle.borrow_mut().take() {
                        handle.unmount();
                    }
                });
            }

            #[::pocopine::__private::wasm_bindgen::prelude::wasm_bindgen]
            pub fn pocopine_split_mount_route(
                route_id: ::std::string::String,
                outlet: ::pocopine::__private::web_sys::Element,
                path: ::std::string::String,
            ) {
                match route_id.as_str() {
                    #(#route_mount_arms)*
                    _ => {}
                }
            }

            #[::pocopine::__private::wasm_bindgen::prelude::wasm_bindgen]
            pub fn pocopine_host_register_static_component(
                tag: ::std::string::String,
                html: ::std::string::String,
            ) {
                ::pocopine::__private::register_descriptor_component(tag, html);
            }

            #[::pocopine::__private::wasm_bindgen::prelude::wasm_bindgen]
            pub fn pocopine_host_mount_static_component(
                outlet: ::pocopine::__private::web_sys::Element,
                tag: ::std::string::String,
            ) {
                __POCOPINE_DESCRIPTOR_ROUTE_HANDLE.with(|handle| {
                    if let ::core::option::Option::Some(handle) = handle.borrow_mut().take() {
                        handle.unmount();
                    }
                });
                let Some(document) = ::pocopine::__private::web_sys::window()
                    .and_then(|window| window.document())
                else {
                    return;
                };
                let Ok(host) = document.create_element(&tag) else {
                    return;
                };
                outlet.replace_children_with_node_1(host.as_ref());
                let handle = ::pocopine::App::mount_registered_subtree(&host, &tag);
                __POCOPINE_DESCRIPTOR_ROUTE_HANDLE.with(|slot| {
                    *slot.borrow_mut() = ::core::option::Option::Some(handle);
                });
            }

            ::pocopine::App::new()
                #devtools_call
                .run_with_static_registry(REGISTRY);
        }
    }
}

fn emit_split_route_app(parsed: &AppMacroInput, route_idx: usize) -> proc_macro2::TokenStream {
    let Some(route) = parsed.routes.get(route_idx) else {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("POCOPINE_SPLIT_MODE route:{route_idx} is out of range"),
        )
        .to_compile_error();
    };
    let component = &route.component;
    let pattern = route.pattern.value();
    let param_setters = route_param_setters(&pattern);

    quote! {
        {
            ::std::thread_local! {
                static __POCOPINE_ROUTE_HANDLE: ::std::cell::RefCell<::core::option::Option<::pocopine::SubtreeHandle>> =
                    const { ::std::cell::RefCell::new(::core::option::Option::None) };
            }

            #[::pocopine::__private::wasm_bindgen::prelude::wasm_bindgen]
            pub fn unmount_pocopine_route() {
                __POCOPINE_ROUTE_HANDLE.with(|handle| {
                    if let ::core::option::Option::Some(handle) = handle.borrow_mut().take() {
                        handle.unmount();
                    }
                });
            }

            #[::pocopine::__private::wasm_bindgen::prelude::wasm_bindgen]
            pub fn mount_pocopine_route(
                outlet: ::pocopine::__private::web_sys::Element,
                path: ::std::string::String,
            ) {
                unmount_pocopine_route();
                let Some(document) = ::pocopine::__private::web_sys::window()
                    .and_then(|window| window.document())
                else {
                    return;
                };
                let Ok(host) = document.create_element(
                    <#component as ::pocopine::__private::Component>::NAME
                ) else {
                    return;
                };
                let __pocopine_path_segments: ::std::vec::Vec<&str> = path
                    .trim_matches('/')
                    .split('/')
                    .filter(|segment| !segment.is_empty())
                    .collect();
                #(#param_setters)*
                outlet.replace_children_with_node_1(host.as_ref());
                let handle = ::pocopine::App::mount_subtree::<#component>(&host);
                __POCOPINE_ROUTE_HANDLE.with(|slot| {
                    *slot.borrow_mut() = ::core::option::Option::Some(handle);
                });
            }
        }
    }
}

fn route_param_setters(pattern: &str) -> Vec<proc_macro2::TokenStream> {
    pattern
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .enumerate()
        .filter_map(|(idx, segment)| {
            let name = segment.strip_prefix(':')?;
            Some(quote! {
                if let ::core::option::Option::Some(value) = __pocopine_path_segments.get(#idx) {
                    let _ = host.set_attribute(#name, value);
                }
            })
        })
        .collect()
}

fn split_manifest_json(parsed: &AppMacroInput, split_base: &str) -> String {
    let routes = parsed
        .routes
        .iter()
        .enumerate()
        .map(|(idx, route)| {
            let id = split_route_id(idx, &route.component);
            format!(
                "{{\"id\":\"{}\",\"pattern\":\"{}\",\"module\":\"/pkg/{}_route_{}.js\"}}",
                json_escape(&id),
                json_escape(&route.pattern.value()),
                json_escape(split_base),
                json_escape(&id),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"routes\":[{routes}]}}")
}

fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}
