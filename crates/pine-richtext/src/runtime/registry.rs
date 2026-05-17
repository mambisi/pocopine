//! Process-level **named-runtime** registry.
//!
//! `App::register::<T>()` registers a *component class* once per
//! process; the pine-richtext analogue here registers a per-name
//! [`EditorRuntime`] configuration. A `<pine-rich-text-root
//! runtime="comment">` mount in the DOM resolves its runtime by
//! looking the string up in this table.
//!
//! ## The default runtime
//!
//! [`default`] returns the runtime used by every
//! `<pine-rich-text-root>` mount that doesn't carry a `runtime`
//! attribute. It's built lazily on first read using
//! [`RuntimeBuilder::new()`] — the kitchen-sink set of
//! `default_extensions()` plus any extensions the app registered via
//! the soft-deprecated `extension::register` path. Once realized,
//! further changes to the default runtime are disallowed in the same
//! way `schema_basic::schema()` seals the global registry today.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use super::{EditorRuntime, RuntimeBuilder};

static RUNTIMES: OnceLock<RwLock<HashMap<String, Arc<EditorRuntime>>>> = OnceLock::new();
static DEFAULT: OnceLock<Arc<EditorRuntime>> = OnceLock::new();

/// Flipped the first time any runtime is resolved (via [`resolve`]
/// or [`default`]). [`register`] panics afterward — registration of
/// named runtimes is sealed once mounts start consuming them.
///
/// Independent of `extension::registry::SCHEMA_REALIZED`, which
/// flips on any `schema_basic::*` helper use. An app that builds a
/// seed doc via `schema_basic::*` before declaring its named
/// runtimes is the documented happy path; that flow shouldn't be
/// blocked from registering runtimes.
static RUNTIMES_RESOLVED: AtomicBool = AtomicBool::new(false);

fn mark_runtimes_resolved() {
    RUNTIMES_RESOLVED.store(true, Ordering::Release);
}

fn runtimes_resolved() -> bool {
    RUNTIMES_RESOLVED.load(Ordering::Acquire)
}

/// Test-only: explicitly flip the resolved seal. Used by tests that
/// need to assert the panic-after-resolve contract without
/// depending on `runtime::default()`'s `OnceLock` state (which is
/// process-sticky and only flips on the genuine first-init call per
/// process).
#[cfg(test)]
pub(crate) fn __mark_runtimes_resolved_for_tests() {
    mark_runtimes_resolved();
}

fn runtimes() -> &'static RwLock<HashMap<String, Arc<EditorRuntime>>> {
    RUNTIMES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register `runtime` under `name`. Subsequent mounts with
/// `runtime="<name>"` resolve to this `Arc<EditorRuntime>`. On a
/// duplicate name the second registration is dropped with a
/// `tracing::warn!` (first-wins).
///
/// **Sealed after first resolve.** Once any surface mounts and
/// resolves a runtime (default or named), this function panics on
/// further calls — matching the lifecycle of
/// [`crate::extension::registry::register`]. Authors must register
/// all runtimes before `App::run()` (or at least before any
/// `<pine-rich-text-root>` enters the DOM).
pub fn register(name: impl Into<String>, runtime: Arc<EditorRuntime>) {
    let name = name.into();
    if runtimes_resolved() {
        panic!(
            "pine-richtext: register runtime `{}` before any runtime is first resolved \
             (typically before App::run())",
            name
        );
    }
    let mut table = runtimes().write().expect("runtime registry poisoned");
    if table.contains_key(&name) {
        tracing::warn!(
            target: "pocopine.log",
            "pine-richtext: runtime `{}` already registered, dropping duplicate",
            name
        );
        return;
    }
    table.insert(name, runtime);
}

/// Resolve a runtime by name. `None` and unknown names both fall back
/// to [`default`] — the absence of a name is the documented "use the
/// kitchen-sink default" case, and unknown names are treated the same
/// way so a typo'd attribute degrades to "the default editor" instead
/// of panicking.
///
/// **Side effect:** every `resolve` call (named or default) seals the
/// legacy `extension::registry` so further `extension::register(...)`
/// calls panic. Without this seal, a page that mounts only
/// named-runtime editors would leave the global registry mutable, and
/// a late `extension::register` could change behavior C2's reconciler
/// / commands still read from globals.
pub fn resolve(name: Option<&str>) -> Arc<EditorRuntime> {
    if let Some(name) = name.filter(|s| !s.is_empty()) {
        if let Some(rt) = runtimes()
            .read()
            .expect("runtime registry poisoned")
            .get(name)
            .cloned()
        {
            mark_runtimes_resolved();
            seal_legacy_registry();
            return rt;
        }
        tracing::warn!(
            target: "pocopine.log",
            "pine-richtext: runtime `{}` not registered, falling back to default",
            name
        );
    }
    default()
}

/// Idempotent seal of the legacy `extension::registry`. Flips
/// `SCHEMA_REALIZED` so further `extension::register(...)` calls
/// panic. Called from every `resolve` / `default` path that returns
/// a runtime so the panic contract holds regardless of which
/// surface mounts first.
///
/// **Phase 4b C5:** the old `install_base_extensions` call is gone.
/// The legacy `BASE` table was deleted alongside it; consumers like
/// `commands::is_list_item_type` now read through the legacy
/// registry's adapters which delegate to the default runtime
/// directly. The seal is now just an atomic store.
fn seal_legacy_registry() {
    crate::extension::registry::mark_schema_realized();
}

/// The default runtime — the configuration every `<pine-rich-text-root>`
/// mount receives unless it specifies a `runtime` attribute or a
/// parent component injects one.
///
/// Built lazily on first read: the kitchen-sink default extensions
/// (folded by `RuntimeBuilder::new`) **overlaid with any user
/// extensions registered via the legacy `extension::registry::register`
/// path**. This bridge lets the demo's
/// `extension::register(TaskListExtension::with_node_view::<C>())`
/// flow through the runtime without code changes — the user's
/// extension wins by name over the base `TaskListExtension::new()` via
/// the builder's overlay semantics.
///
/// As a side-effect of folding the user extensions, this call also
/// flips `extension::registry::SCHEMA_REALIZED` so further
/// `extension::register` calls panic — same one-way contract
/// `schema_basic::schema()` enforces today.
pub fn default() -> Arc<EditorRuntime> {
    DEFAULT
        .get_or_init(|| {
            // Seal the legacy registry FIRST so a concurrent
            // `extension::register(...)` racing against our read of
            // `registered()` below panics instead of slipping into the
            // fold partway through.
            mark_runtimes_resolved();
            seal_legacy_registry();

            let mut builder = RuntimeBuilder::new();
            for ext in crate::extension::registry::registered() {
                builder = builder.with_arc(ext);
            }
            builder.build()
        })
        .clone()
}

/// Has the default runtime been materialized yet? Used in commit 5 to
/// gate the soft-deprecated `extension::register` panic message.
pub fn default_realized() -> bool {
    DEFAULT.get().is_some()
}

/// Test-only: drop every named runtime registration and reset the
/// "any runtime resolved" seal. The default runtime is intentionally
/// left alone — once the process has materialized it, the schema
/// fold is authoritative for the rest of the test run and rebuilding
/// would race with parallel readers.
#[cfg(test)]
pub(crate) fn __reset_named_for_tests() {
    if let Some(lock) = RUNTIMES.get() {
        lock.write()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
    RUNTIMES_RESOLVED.store(false, Ordering::Release);
}

/// Process-wide test guard. Shared across `extension::tests` and
/// `runtime::tests` so they don't race on `SCHEMA_REALIZED` /
/// `EXTENSIONS` / `RUNTIMES`. Tests acquire it via [`lock_tests`].
#[cfg(test)]
pub(crate) static TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
    TEST_GUARD
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}
