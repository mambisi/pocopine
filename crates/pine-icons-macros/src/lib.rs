//! Proc-macros for the `pine-icons` crate.
//!
//! Two function-like macros:
//!
//! - [`icon!`] — resolve a Tabler icon at compile time and embed
//!   its SVG as a `&'static str`. `icon!("user")` defaults to the
//!   outline set; `icon!(filled / "chevron-down")` picks the
//!   filled variant.
//! - [`register_icons!`] — list icon entries that should be
//!   compiled into the app's runtime registry so the template
//!   primitive `<pine-icon name="…">` can find them at render
//!   time. Only names enumerated here land in the binary — the
//!   other ~5000 icons stay out.
//!
//! Assets are vendored under `crates/pine-icons/assets/tabler/`
//! (see `scripts/sync-tabler-icons.sh`). The macro reads the dir
//! relative to its own `CARGO_MANIFEST_DIR` — no build.rs wiring
//! needed, since `include_str!` already tracks the embedded file
//! as a rebuild dependency of the caller crate.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use syn::parse::{Parse, ParseStream, Parser};
use syn::{parse_macro_input, Ident, LitStr, Token};

/// Absolute path to the vendored Tabler asset tree.
fn icons_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("pine-icons-macros crate has no parent")
            .join("pine-icons")
            .join("assets")
            .join("tabler")
    })
}

/// Cached directory listing per variant — used both for existence
/// checks and for did-you-mean suggestions on typo'd names.
fn names_for(variant: &str) -> &'static BTreeSet<String> {
    static OUTLINE: OnceLock<BTreeSet<String>> = OnceLock::new();
    static FILLED: OnceLock<BTreeSet<String>> = OnceLock::new();
    let slot = match variant {
        "outline" => &OUTLINE,
        "filled" => &FILLED,
        _ => unreachable!("names_for called with unknown variant `{variant}`"),
    };
    slot.get_or_init(|| {
        let dir = icons_root().join(variant);
        let mut names = BTreeSet::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("svg") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.insert(stem.to_string());
                }
            }
        }
        names
    })
}

/// FNV-1a 64-bit over the SVG bytes. Cheap, deterministic, no
/// dependencies. The value is embedded as a `const u64` literal
/// in the generated `IconEntry`, so the primitive can skip
/// redundant re-renders with an O(1) compare instead of a full
/// string equality check.
fn fnv1a_u64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn best_suggestion<'a>(
    needle: &str,
    haystack: impl IntoIterator<Item = &'a String>,
) -> Option<&'a str> {
    haystack
        .into_iter()
        .map(|c| (strsim::jaro_winkler(needle, c), c.as_str()))
        .filter(|(score, _)| *score > 0.75)
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, c)| c)
}

// ── icon! ────────────────────────────────────────────────────────

struct IconSpec {
    variant: String,
    variant_span: Span,
    name: String,
    name_span: Span,
}

impl Parse for IconSpec {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        // `"name"` — outline default.
        if input.peek(LitStr) && !input.peek2(Token![/]) {
            let lit: LitStr = input.parse()?;
            if !input.is_empty() {
                return Err(input.error("unexpected tokens after icon name"));
            }
            return Ok(IconSpec {
                variant: "outline".into(),
                variant_span: lit.span(),
                name: lit.value(),
                name_span: lit.span(),
            });
        }
        // `variant / "name"`.
        let variant_ident: Ident = input.parse()?;
        let variant = variant_ident.to_string();
        if variant != "outline" && variant != "filled" {
            return Err(syn::Error::new(
                variant_ident.span(),
                format!("unknown icon variant `{variant}` — expected `outline` or `filled`"),
            ));
        }
        input.parse::<Token![/]>()?;
        let lit: LitStr = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after icon name"));
        }
        Ok(IconSpec {
            variant,
            variant_span: variant_ident.span(),
            name: lit.value(),
            name_span: lit.span(),
        })
    }
}

fn resolve_spec(spec: &IconSpec) -> Result<PathBuf, proc_macro2::TokenStream> {
    let candidates = names_for(&spec.variant);
    if candidates.is_empty() {
        return Err(syn::Error::new(
            spec.variant_span,
            format!(
                "pine-icons: no `{}` icons vendored yet — run `scripts/sync-tabler-icons.sh` first",
                spec.variant
            ),
        )
        .to_compile_error());
    }
    if !candidates.contains(&spec.name) {
        let msg = match best_suggestion(&spec.name, candidates.iter()) {
            Some(s) => format!(
                "unknown tabler {} icon `{}` — did you mean `{}`?",
                spec.variant, spec.name, s
            ),
            None => format!("unknown tabler {} icon `{}`", spec.variant, spec.name),
        };
        return Err(syn::Error::new(spec.name_span, msg).to_compile_error());
    }
    Ok(icons_root()
        .join(&spec.variant)
        .join(format!("{}.svg", spec.name)))
}

/// Embed a Tabler icon's SVG as a `&'static str`.
///
/// ```ignore
/// let outline_user = pine_icons::icon!("user");
/// let filled_star  = pine_icons::icon!(filled / "star");
/// ```
#[proc_macro]
pub fn icon(input: TokenStream) -> TokenStream {
    let spec = parse_macro_input!(input as IconSpec);
    let path = match resolve_spec(&spec) {
        Ok(p) => p,
        Err(err) => return err.into(),
    };
    let path_lit = LitStr::new(
        path.to_str().expect("icon path is not valid UTF-8"),
        Span::call_site(),
    );
    quote! { ::core::include_str!(#path_lit) }.into()
}

// ── register_icons! ──────────────────────────────────────────────

struct RegisterList {
    specs: Vec<IconSpec>,
}

impl Parse for RegisterList {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut specs = Vec::new();
        while !input.is_empty() {
            let fork = input.fork();
            // Parse a single spec; IconSpec's own parser consumes to
            // end-of-stream, which is wrong for comma-separated use.
            // Re-implement the two-shape parse inline.
            let spec = if fork.peek(LitStr) && !fork.peek2(Token![/]) {
                let lit: LitStr = input.parse()?;
                IconSpec {
                    variant: "outline".into(),
                    variant_span: lit.span(),
                    name: lit.value(),
                    name_span: lit.span(),
                }
            } else {
                let variant_ident: Ident = input.parse()?;
                let variant = variant_ident.to_string();
                if variant != "outline" && variant != "filled" {
                    return Err(syn::Error::new(
                        variant_ident.span(),
                        format!(
                            "unknown icon variant `{variant}` — expected `outline` or `filled`"
                        ),
                    ));
                }
                input.parse::<Token![/]>()?;
                let lit: LitStr = input.parse()?;
                IconSpec {
                    variant,
                    variant_span: variant_ident.span(),
                    name: lit.value(),
                    name_span: lit.span(),
                }
            };
            specs.push(spec);
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(RegisterList { specs })
    }
}

/// Compile a set of Tabler icons into the app's runtime registry.
///
/// Only names listed here ship in the WASM binary; the other
/// thousands stay out. `<pine-icon name="…">` at render time
/// looks up by name in this registry.
///
/// ```ignore
/// pine_icons::register_icons![
///     "user",
///     "chevron-down",
///     filled / "star",
/// ];
/// ```
#[proc_macro]
pub fn register_icons(input: TokenStream) -> TokenStream {
    let list = match RegisterList::parse.parse(input) {
        Ok(l) => l,
        Err(e) => return e.to_compile_error().into(),
    };

    let mut entries = proc_macro2::TokenStream::new();
    for spec in list.specs.iter() {
        let path = match resolve_spec(spec) {
            Ok(p) => p,
            Err(err) => return err.into(),
        };
        // Hash the SVG bytes NOW so the primitive doesn't pay the
        // cost at runtime. File read is already covered by the
        // rustc rebuild graph via the emitted `include_str!`, so
        // there's no re-hash-on-no-change concern.
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                return syn::Error::new(
                    spec.name_span,
                    format!("pine-icons: failed to read {}: {e}", path.display()),
                )
                .to_compile_error()
                .into();
            }
        };
        let svg_hash = fnv1a_u64(&bytes);
        let path_lit = LitStr::new(
            path.to_str().expect("icon path is not valid UTF-8"),
            Span::call_site(),
        );
        let name_lit = LitStr::new(&spec.name, Span::call_site());
        let variant_lit = LitStr::new(&spec.variant, Span::call_site());
        entries.extend(quote! {
            ::pine_icons::__register(::pine_icons::IconEntry {
                name: #name_lit,
                variant: #variant_lit,
                svg: ::core::include_str!(#path_lit),
                svg_hash: #svg_hash,
            });
        });
    }

    quote! { { #entries } }.into()
}
