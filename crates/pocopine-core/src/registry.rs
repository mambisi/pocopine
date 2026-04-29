//! Component registry — RFC 056.
//!
//! `wasm32-unknown-unknown` has no portable "collect static entries at
//! startup" mechanism (no `linkme`/`ctor`/`inventory` story that survives
//! `wasm-bindgen(start)`), so registration is explicit. The `#[component]`
//! macro emits a `pub fn register()` on the struct; users call it from
//! their startup code before `pocopine::run()`.
//!
//! Per RFC 056 §6.1 the registry is fail-fast — a collision between two
//! distinct owners on the same canonical tag (or alias) records a
//! [`RegistryError`] instead of silently overwriting the previous entry.
//! `App::run()` / `pocopine::run()` query the accumulated errors before
//! the first walk and refuse to mount when any are present (see
//! [`verify_registry`] and [`render_boot_error`]).

use std::any::TypeId;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::reactive::ScopeId;
use crate::scope::Scope;
use crate::templates_plan::StaticTemplatePlan;

/// Constructor returned by the `#[component]` macro. Builds a fresh typed
/// `Rc<RefCell<Self>>`, wraps it in a [`Scope`] (which stashes both the
/// erased and typed forms), and returns the scope.
pub type ComponentCtor = fn() -> Scope;

/// RFC 062 — per-component compiled template mount entry.
/// Macro-emitted components point this at their generated mount
/// body. It is intentionally the component mount path, not a
/// static-plan fallback hook.
pub type ComponentMountFn = fn(&Element, ScopeId, &JsValue);

/// Kept as a public type so users with their own registration path have
/// something to hand back to the runtime.
pub struct ComponentEntry {
    pub name: &'static str,
    pub ctor: ComponentCtor,
    pub mount_template: Option<ComponentMountFn>,
}

/// Snapshot of a registered canonical entry. Aliases resolve to the
/// canonical entry by name, so introspection happens through here.
#[derive(Clone, Copy)]
pub struct RegisteredComponent {
    pub canonical: &'static str,
    pub owner: &'static str,
    pub ctor: ComponentCtor,
    pub mount_template: Option<ComponentMountFn>,
}

/// Why a registration call failed. The variants distinguish *what* the
/// incoming tag collided with so error surfaces can render specific
/// messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryErrorKind {
    /// Two distinct owners both tried to register the same canonical
    /// tag.
    DuplicateCanonicalTag,
    /// Two distinct owners both tried to register the same alias.
    DuplicateAlias,
    /// The incoming alias collides with a canonical tag already owned
    /// by a different owner.
    AliasConflictsWithCanonical,
    /// The incoming canonical tag collides with an alias already owned
    /// by a different owner.
    CanonicalConflictsWithAlias,
}

/// Recorded collision between two registration calls. Owners are kept
/// as `&'static str` so error surfaces can show provenance without
/// chasing extra lookups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistryError {
    pub kind: RegistryErrorKind,
    pub tag: &'static str,
    pub first_owner: &'static str,
    pub second_owner: &'static str,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            RegistryErrorKind::DuplicateCanonicalTag => "duplicate component tag",
            RegistryErrorKind::DuplicateAlias => "duplicate component alias",
            RegistryErrorKind::AliasConflictsWithCanonical => {
                "alias collides with an existing canonical tag"
            }
            RegistryErrorKind::CanonicalConflictsWithAlias => {
                "canonical tag collides with an existing alias"
            }
        };
        write!(
            f,
            "pocopine registry: {what} `{}` (owners: `{}` and `{}`)",
            self.tag, self.first_owner, self.second_owner,
        )
    }
}

#[derive(Default)]
struct Registry {
    /// Canonical entries keyed by their canonical tag.
    canonical: HashMap<&'static str, RegisteredComponent>,
    /// Aliases pointing to a canonical tag plus the owner that
    /// declared the alias (an alias can have a different owner from
    /// its canonical entry, e.g. `pine-icons` aliasing a generic icon
    /// component).
    aliases: HashMap<&'static str, (&'static str, &'static str)>,
    /// Conflicts collected during registration. Cleared by
    /// [`__reset_for_test`].
    errors: Vec<RegistryError>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
    static ACTIVE_PHF_REGISTRY: RefCell<Option<&'static phf::Map<&'static str, &'static ComponentVTable>>> =
        const { RefCell::new(None) };
    /// RFC 060 Tier 1 — visited set of component types whose
    /// `register()` body has already started executing on this thread.
    /// Used by [`mark_registered`] to short-circuit transitive
    /// `T::register()` calls emitted from `uses = [...]`.
    static REGISTERED: RefCell<HashSet<TypeId>> = RefCell::new(HashSet::new());
}

/// RFC 060 Tier 4 — static vtable per `#[component]` type. The
/// `app!{}` macro collects an explicit `components: [...]` list
/// and emits a `&'static phf::Map<&'static str, &'static
/// ComponentVTable>` that the runtime queries instead of (or in
/// addition to) the legacy thread-local `HashMap`. Each entry
/// lives in `.rodata` — no allocation, no init order.
///
/// RFC 061 Phase 4 adds the compiled template payloads so
/// runtime lookups can consult the `app!{}` phf map before the
/// thread-local registration maps. Bundles use `None` for
/// template-bearing fields because they are registration-only.
pub struct ComponentVTable {
    pub name: &'static str,
    pub register: fn(),
    pub template_html: Option<&'static str>,
    pub plan: Option<&'static StaticTemplatePlan>,
    pub mount_template: Option<ComponentMountFn>,
}

/// Install the active static registry for phf-first runtime
/// lookups. `App::run_with_registry` calls this before invoking
/// each vtable's `register()` function so template and plan
/// lookup can use the static table directly.
pub fn set_active_phf_registry(
    registry: &'static phf::Map<&'static str, &'static ComponentVTable>,
) {
    ACTIVE_PHF_REGISTRY.with(|active| {
        *active.borrow_mut() = Some(registry);
    });
}

/// Test-only reset for suites that need to assert fallback
/// behavior independent of a previously installed app registry.
#[doc(hidden)]
pub fn clear_active_phf_registry_for_test() {
    ACTIVE_PHF_REGISTRY.with(|active| {
        *active.borrow_mut() = None;
    });
    crate::templates::clear_template_element_cache_for_test();
}

pub fn active_component_vtable(name: &str) -> Option<&'static ComponentVTable> {
    ACTIVE_PHF_REGISTRY.with(|active| {
        active
            .borrow()
            .and_then(|registry| registry.get(name).copied())
    })
}

pub fn active_has_template(name: &str) -> bool {
    ACTIVE_PHF_REGISTRY.with(|active| {
        active
            .borrow()
            .and_then(|registry| registry.get(name))
            .and_then(|v| v.template_html)
            .is_some()
    })
}

pub fn active_component_names() -> Vec<&'static str> {
    ACTIVE_PHF_REGISTRY.with(|active| {
        active
            .borrow()
            .map(|registry| registry.keys().copied().collect())
            .unwrap_or_default()
    })
}

/// Cycle/dedupe guard for the macro-emitted `Component::register()`
/// body. Returns `true` the first time `T` is seen on this thread,
/// `false` on every subsequent call. The `#[component]` macro emits
/// `if !mark_registered::<Self>() { return; }` as the first statement
/// of `register()`, so a cyclic `uses` graph (`A.uses=[B]`,
/// `B.uses=[A]`) terminates and a converging graph short-circuits the
/// redundant work.
///
/// TypeId-keyed (rather than NAME-keyed) so two distinct types sharing
/// a NAME both reach [`register_component`] and surface
/// [`RegistryErrorKind::DuplicateCanonicalTag`].
pub fn mark_registered<T: 'static>() -> bool {
    REGISTERED.with(|r| r.borrow_mut().insert(TypeId::of::<T>()))
}

/// Register `canonical` against `ctor`, attributing the entry to
/// `owner`. Re-registration by the *same* owner is a no-op (test
/// harnesses, hot reload). Distinct owners colliding on the same tag
/// records a [`RegistryError`] instead of overwriting.
///
/// `owner` is the macro-generated provenance label — typically
/// `concat!(module_path!(), "::", stringify!(StructIdent))`.
pub fn register_component(canonical: &'static str, owner: &'static str, ctor: ComponentCtor) {
    register_component_with_mount(canonical, owner, ctor, None);
}

pub fn register_component_with_mount(
    canonical: &'static str,
    owner: &'static str,
    ctor: ComponentCtor,
    mount_template: Option<ComponentMountFn>,
) {
    REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        // alias-by-this-name owned by someone else first wins
        if let Some(&(_canon, alias_owner)) = reg.aliases.get(canonical) {
            if alias_owner != owner {
                reg.errors.push(RegistryError {
                    kind: RegistryErrorKind::CanonicalConflictsWithAlias,
                    tag: canonical,
                    first_owner: alias_owner,
                    second_owner: owner,
                });
                return;
            }
        }
        if let Some(existing_owner) = reg.canonical.get(canonical).map(|e| e.owner) {
            if existing_owner == owner {
                // idempotent re-registration — no-op
                return;
            }
            reg.errors.push(RegistryError {
                kind: RegistryErrorKind::DuplicateCanonicalTag,
                tag: canonical,
                first_owner: existing_owner,
                second_owner: owner,
            });
            return;
        }
        reg.canonical.insert(
            canonical,
            RegisteredComponent {
                canonical,
                owner,
                ctor,
                mount_template,
            },
        );
    });
}

/// Register `alias` as another name for `canonical`. The canonical
/// entry must already exist — the alias-side `ctor` is recorded under
/// the canonical tag if it isn't there yet so direct callers don't
/// have to issue two separate calls.
pub fn register_component_as(
    alias: &'static str,
    canonical: &'static str,
    owner: &'static str,
    ctor: ComponentCtor,
) {
    register_component(canonical, owner, ctor);
    REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        if let Some(existing_owner) = reg.canonical.get(alias).map(|e| e.owner) {
            if existing_owner == owner {
                // alias coincides with the canonical name we just
                // registered — silently ignored
                return;
            }
            reg.errors.push(RegistryError {
                kind: RegistryErrorKind::AliasConflictsWithCanonical,
                tag: alias,
                first_owner: existing_owner,
                second_owner: owner,
            });
            return;
        }
        if let Some(&(_canon, existing_owner)) = reg.aliases.get(alias) {
            if existing_owner == owner {
                return;
            }
            reg.errors.push(RegistryError {
                kind: RegistryErrorKind::DuplicateAlias,
                tag: alias,
                first_owner: existing_owner,
                second_owner: owner,
            });
            return;
        }
        reg.aliases.insert(alias, (canonical, owner));
    });
}

/// Convenience wrapper that builds the alias by joining `prefix` and
/// `short` with a hyphen. Mirrors the canonical pine-* / app-* tag
/// convention without forcing callers to format the string themselves.
pub fn register_component_prefixed(
    prefix: &'static str,
    short: &'static str,
    owner: &'static str,
    ctor: ComponentCtor,
) {
    let combined: &'static str = Box::leak(format!("{prefix}-{short}").into_boxed_str());
    register_component(combined, owner, ctor);
}

/// Exposed for symmetry with the `ComponentEntry` type.
pub static COMPONENT_ENTRIES: &[ComponentEntry] = &[];

/// Instantiate a component by name. Resolves through aliases. `None` if
/// the name wasn't registered.
pub fn instantiate(name: &str) -> Option<Scope> {
    REGISTRY.with(|r| {
        let reg = r.borrow();
        if let Some(entry) = reg.canonical.get(name) {
            return Some((entry.ctor)());
        }
        if let Some(&(canon, _)) = reg.aliases.get(name) {
            if let Some(entry) = reg.canonical.get(canon) {
                return Some((entry.ctor)());
            }
        }
        None
    })
}

pub fn mount_template_for(name: &str) -> Option<ComponentMountFn> {
    REGISTRY.with(|r| {
        let reg = r.borrow();
        if let Some(entry) = reg.canonical.get(name) {
            return entry.mount_template;
        }
        if let Some(&(canon, _)) = reg.aliases.get(name) {
            if let Some(entry) = reg.canonical.get(canon) {
                return entry.mount_template;
            }
        }
        None
    })
}

/// Snapshot of every collision recorded so far.
pub fn registry_errors() -> Vec<RegistryError> {
    REGISTRY.with(|r| r.borrow().errors.clone())
}

/// Snapshot of every canonical + alias name currently registered.
/// Sorted for stable diagnostics output.
pub fn registered_component_names() -> Vec<String> {
    REGISTRY.with(|r| {
        let reg = r.borrow();
        let mut names: Vec<String> = reg.canonical.keys().map(|s| s.to_string()).collect();
        names.extend(reg.aliases.keys().map(|s| s.to_string()));
        names.sort();
        names
    })
}

/// `Ok` when there are no collisions; `Err` lists every error
/// recorded so far. Used by [`App::run`] before mounting and by tests
/// via [`assert_registry_clean`].
pub fn verify_registry() -> Result<(), Vec<RegistryError>> {
    let errors = registry_errors();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Test helper. Panics with a multi-line summary when the registry
/// has any collisions.
#[track_caller]
pub fn assert_registry_clean() {
    if let Err(errors) = verify_registry() {
        let mut msg = String::from("pocopine registry has unresolved collisions:\n");
        for err in &errors {
            msg.push_str("  - ");
            msg.push_str(&err.to_string());
            msg.push('\n');
        }
        panic!("{msg}");
    }
}

/// Reset the registry between tests. Not part of the stable surface;
/// the wasm runtime is single-app and never touches this.
#[doc(hidden)]
pub fn __reset_for_test() {
    REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        reg.canonical.clear();
        reg.aliases.clear();
        reg.errors.clear();
    });
    REGISTERED.with(|r| r.borrow_mut().clear());
}

/// Render the boot-time error surface. Replaces `<body>` with a
/// permanent banner listing every collision so the page loudly
/// fails instead of silently mounting half a registry.
///
/// Best-effort only — when there is no document (SSR / non-wasm test
/// context) this is a no-op.
pub fn render_boot_error(errors: &[RegistryError]) {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Some(body) = doc.body() else { return };
    body.set_inner_html("");
    let Ok(banner) = doc.create_element("div") else {
        return;
    };
    let _ = banner.set_attribute(
        "style",
        "position:fixed;inset:0;background:#1b1b1f;color:#f5f5f7;\
         font-family:ui-monospace,monospace;padding:24px;overflow:auto;\
         z-index:2147483647;",
    );
    let mut html = String::from(
        "<h2 style=\"margin:0 0 12px 0;color:#ff6b6b;\">pocopine: \
         component registry has unresolved collisions</h2>\
         <p style=\"margin:0 0 16px 0;\">The runtime refused to mount \
         because two owners registered the same component tag. Resolve \
         the conflicts and reload.</p><ul style=\"margin:0;padding-left:20px;\">",
    );
    for err in errors {
        html.push_str("<li style=\"margin-bottom:8px;\"><code>");
        html.push_str(&html_escape(err.tag));
        html.push_str("</code> — ");
        html.push_str(match err.kind {
            RegistryErrorKind::DuplicateCanonicalTag => "duplicate canonical tag",
            RegistryErrorKind::DuplicateAlias => "duplicate alias",
            RegistryErrorKind::AliasConflictsWithCanonical => {
                "alias collides with an existing canonical tag"
            }
            RegistryErrorKind::CanonicalConflictsWithAlias => {
                "canonical tag collides with an existing alias"
            }
        });
        html.push_str(" (owners: <code>");
        html.push_str(&html_escape(err.first_owner));
        html.push_str("</code> and <code>");
        html.push_str(&html_escape(err.second_owner));
        html.push_str("</code>)</li>");
    }
    html.push_str("</ul>");
    banner.set_inner_html(&html);
    let _ = body.append_child(&banner);
    web_sys::console::error_1(
        &format!(
            "pocopine: component registry has {} unresolved collision(s); refusing to mount",
            errors.len()
        )
        .into(),
    );
    for err in errors {
        web_sys::console::error_1(&err.to_string().into());
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
