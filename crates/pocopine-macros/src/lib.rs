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

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream, Parser},
    parse_macro_input,
    punctuated::Punctuated,
    Expr, ExprLit, FnArg, ImplItem, ItemFn, ItemImpl, ItemStruct, Lit, LitStr,
    Meta, MetaNameValue, Pat, Path, PatType, Token, Type,
};

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

/// HTML Living Standard element names. A struct whose kebab-case ident
/// matches one of these is rejected — its custom-element tag would
/// collide with real HTML markup in parent templates.
const HTML5_ELEMENTS: &[&str] = &[
    "a", "abbr", "address", "area", "article", "aside", "audio",
    "b", "base", "bdi", "bdo", "blockquote", "body", "br", "button",
    "canvas", "caption", "cite", "code", "col", "colgroup",
    "data", "datalist", "dd", "del", "details", "dfn", "dialog",
    "div", "dl", "dt",
    "em", "embed",
    "fieldset", "figcaption", "figure", "footer", "form",
    "h1", "h2", "h3", "h4", "h5", "h6", "head", "header", "hgroup",
    "hr", "html",
    "i", "iframe", "img", "input", "ins",
    "kbd",
    "label", "legend", "li", "link",
    "main", "map", "mark", "math", "menu", "meta", "meter",
    "nav", "noscript",
    "object", "ol", "optgroup", "option", "output",
    "p", "picture", "pre", "progress",
    "q",
    "rp", "rt", "ruby",
    "s", "samp", "script", "search", "section", "select", "slot",
    "small", "source", "span", "strong", "style", "sub", "summary",
    "sup", "svg",
    "table", "tbody", "td", "template", "textarea", "tfoot", "th",
    "thead", "time", "title", "tr", "track",
    "u", "ul",
    "var", "video",
    "wbr",
];

#[derive(Default)]
struct ComponentArgs {
    name: Option<LitStr>,
    template: Option<LitStr>,
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
}

impl Parse for ComponentArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pairs: Punctuated<MetaNameValue, Token![,]> =
            Punctuated::parse_terminated(input)?;
        let mut args = ComponentArgs::default();
        for kv in pairs {
            let lit = match kv.value {
                Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => s,
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "expected a string literal",
                    ));
                }
            };
            if kv.path.is_ident("name") {
                args.name = Some(lit);
            } else if kv.path.is_ident("template") {
                args.template = Some(lit);
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
                    "unknown key — expected one of: name, template, style, role, \
                     display, transition, transition_in, transition_out, animate",
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


/// Kebab-case an ident: `TodoItem` → `todo-item`.
fn kebab_case(ident: &str) -> String {
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

    let template_path: LitStr = match &args.template {
        Some(s) => s.clone(),
        None => LitStr::new(&format!("{ident_str}.poco"), struct_ident.span()),
    };

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
        let Some(ident) = field.ident.clone() else { continue };
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
                let parsed: syn::Result<(Option<String>, Option<Vec<String>>, bool)> =
                    match &a.meta {
                        Meta::Path(_) => Ok((None, None, false)),
                        Meta::List(_) => a.parse_args_with(
                            |input: syn::parse::ParseStream| {
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
                                            let mut leaves = Vec::with_capacity(
                                                arr.elems.len(),
                                            );
                                            for e in arr.elems.iter() {
                                                match e {
                                                    Expr::Lit(ExprLit {
                                                        lit: Lit::Str(s),
                                                        ..
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
                            },
                        ),
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
            let name_on_root = rename
                .unwrap_or_else(|| ident.to_string().trim_start_matches("r#").to_string());
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
    let flatten_leaf_get_arms =
        flatten_fields.iter().flat_map(|(container, leaves)| {
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

    let flatten_leaf_set_arms =
        flatten_fields.iter().flat_map(|(container, leaves)| {
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

    let get_arms = field_idents.iter().zip(field_names.iter()).map(|(id, name)| {
        quote! {
            #name => ::pocopine::__private::serde_wasm_bindgen::to_value(&self.#id)
                .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
        }
    }).chain(flatten_leaf_get_arms);

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
                #field_name => ::pocopine::__private::serde_wasm_bindgen::to_value(
                    &self.#field_ident,
                )
                .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
            })
        });

    // Resolve `role = "..."` → canonical HTML tag name. An unknown
    // role is a compile-time error; an explicitly-omitted role
    // keeps the classic `inject_pp_data`-only path so non-primitive
    // components don't need a placeholder root.
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
            quote! { Some((#tag, #role_name)) }
        }
        None => quote! { None },
    };

    // RFC-038 transition / animate args → string literals the
    // generated `ComponentState` impl returns. Precedence:
    //   transition_in wins for enter, transition_out wins for leave,
    //   `transition` provides the fallback for whichever isn't
    //   explicitly set. Missing entirely → "". `animate = "flip"`
    //   sets the animate-kind literal; anything else falls back to
    //   the raw string for forwards compatibility.
    let transition_sym = args.transition.as_ref().map(|l| l.value()).unwrap_or_default();
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

    let register_template_stmt = quote! {
        const _: &str = include_str!(#template_path);
        ::pocopine::__private::register_template(
            #name_str,
            ::pocopine::__private::compile_template(
                include_str!(#template_path),
                #name_str,
                #role_arg,
            ),
        );
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

        #observe_impl

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
            fn setup(&mut self) {
                <Self as ::pocopine::__private::HandlerDispatch>::setup(self);
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
            fn unmount(&mut self) {
                <Self as ::pocopine::__private::HandlerDispatch>::unmount(self);
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
            /// Register this component (template, stylesheet, constructor)
            /// with the pocopine runtime. Idempotent. Call directly or via
            /// [`pocopine::App::register`].
            pub fn register() {
                ::pocopine::__private::register_component(
                    #name_str,
                    || {
                        let instance: ::std::rc::Rc<::std::cell::RefCell<#struct_ident>> =
                            ::std::rc::Rc::new(::std::cell::RefCell::new(
                                <#struct_ident as ::core::default::Default>::default()
                            ));
                        ::pocopine::__private::Scope::new(instance)
                    },
                );
                #register_template_stmt
                #register_style_stmt
                #register_display_stmt
            }
        }

        impl ::pocopine::__private::Component for #struct_ident {
            const NAME: &'static str = #name_str;
            fn register() {
                <#struct_ident>::register();
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
    let mut on_mount_extractor_count: usize = 0;
    let mut on_ready_extractor_count: usize = 0;
    // (method_ident, field_ident, value_type) for each `#[watch(f)]`
    // method. The macro auto-generates an `on_ready` that wires a
    // `watch_field` per entry.
    let mut watches: Vec<(syn::Ident, syn::Ident, syn::Type)> = Vec::new();

    // First pass: collect watch metadata while the `#[watch(...)]`
    // attribute is still on each method. Strip the attribute in the
    // same loop so the compiler doesn't see an unknown attr on the
    // rewritten output.
    let mut methods_to_skip_in_arms: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for impl_item in input.items.iter_mut() {
        let ImplItem::Fn(method) = impl_item else { continue };
        let mut watch_field: Option<syn::Ident> = None;
        method.attrs.retain(|attr| {
            if attr.path().is_ident("watch") {
                if let Ok(ident) = attr.parse_args::<syn::Ident>() {
                    watch_field = Some(ident);
                }
                false // strip
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
    }

    for item in &input.items {
        let ImplItem::Fn(method) = item else { continue };
        let Some(_receiver) = method.sig.receiver() else {
            continue;
        };
        let ident = method.sig.ident.clone();
        let name = ident.to_string();
        match name.as_str() {
            "on_setup" => has_on_setup = true,
            "on_mount" => {
                has_on_mount = true;
                // Count non-receiver params — each becomes a
                // `__ctx.into()` in the generated forwarder.
                on_mount_extractor_count = method
                    .sig
                    .inputs
                    .iter()
                    .filter(|a| matches!(a, FnArg::Typed(_)))
                    .count();
                continue; // lifecycle; don't emit an invoke arm
            }
            "on_ready" => {
                has_on_ready = true;
                on_ready_extractor_count = method
                    .sig
                    .inputs
                    .iter()
                    .filter(|a| matches!(a, FnArg::Typed(_)))
                    .count();
                continue; // lifecycle; don't emit an invoke arm
            }
            "on_unmount" => has_on_unmount = true,
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
    let user_on_setup_call = has_on_setup.then(|| {
        quote! { Self::on_setup(self); }
    });
    let setup_impl = Some(quote! {
        fn setup(&mut self) {
            <Self>::__pocopine_observe_seed(self);
            let __me = ::pocopine::this::<Self>();
            <Self>::__pocopine_observe_install(__me);
            #user_on_setup_call
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
    let unmount_impl = has_on_unmount.then(|| {
        quote! {
            fn unmount(&mut self) {
                Self::on_unmount(self);
            }
            fn has_on_unmount(&self) -> bool { true }
        }
    });

    let out = quote! {
        #input

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
        let pairs: Punctuated<MetaNameValue, Token![,]> =
            Punctuated::parse_terminated(input)?;
        let mut args = StoreArgs::default();
        for kv in pairs {
            let lit = match kv.value {
                Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => s,
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

    let get_arms = field_idents.iter().zip(field_names.iter()).map(|(id, name)| {
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
