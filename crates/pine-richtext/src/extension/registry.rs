//! Legacy process-global extension registry.
//!
//! This module is a thin compatibility shim over the current per-runtime
//! architecture. It does not own schema, command, keymap, plugin, or node-view
//! tables: adapter reads delegate to [`crate::runtime::registry::default`].
//!
//! What stays:
//!
//! * [`EXTENSIONS`] is the write target for [`register`]. The default
//!   runtime reads it once during its lazy init, so this Vec is the
//!   source of truth for user-registered extensions until the
//!   runtime fold caches them.
//! * [`SCHEMA_REALIZED`] flips when the default (or any named)
//!   runtime is first resolved; subsequent [`register`] calls panic.
//! * Typed component views are intentionally absent. The erased
//!   [`RichTextExtension`] stored here contains model contributions only;
//!   browser views are paired and validated by
//!   [`crate::runtime::RuntimeBuilder::with_view`] on an explicit runtime.
//!
//! Apps that want per-instance scoping should call
//! [`crate::runtime::registry::register`] with a
//! [`crate::runtime::RuntimeBuilder`] instead. See
//! `docs/extensions.md`.
//!
//! [`SCHEMA_REALIZED`]: SCHEMA_REALIZED

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use crate::state::Plugin;

use super::{KeyBindingFactory, NamedCommand, RichTextExtension};

static EXTENSIONS: OnceLock<RwLock<Vec<Arc<dyn RichTextExtension>>>> = OnceLock::new();
static SCHEMA_REALIZED: AtomicBool = AtomicBool::new(false);

fn extensions() -> &'static RwLock<Vec<Arc<dyn RichTextExtension>>> {
    EXTENSIONS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Read-guard for the user-extension list. Used by
/// [`crate::runtime::registry::default`] during its lazy fold — that's
/// the one consumer that can't go through the runtime adapter
/// (circular).
pub(crate) fn read_extensions()
-> std::sync::RwLockReadGuard<'static, Vec<Arc<dyn RichTextExtension>>> {
    extensions().read().expect("extension registry poisoned")
}

/// Register an extension into the default runtime's extension chain.
/// Must be called before any `<pine-rich-text-root>` mount resolves
/// (default or named); the first resolve seals this registry and
/// further `register` calls panic.
///
/// **Deprecated.** Apps targeting per-instance scoping should
/// build a [`crate::runtime::EditorRuntime`] explicitly via
/// [`crate::runtime::RuntimeBuilder`] and register it through
/// [`crate::runtime::registry::register`]. This function continues
/// to append model contributions to the one-time default-runtime fold. It
/// cannot contribute typed component views and never mutates an already-built
/// runtime.
///
/// Duplicate `name()` is dropped first-wins with a `tracing::warn!`.
#[deprecated(
    since = "0.1.0-phase-4b",
    note = "Use `runtime::RuntimeBuilder::new().with(ext).build()` + `runtime::registry::register(name, rt)` for per-instance scoping. This function only mutates the default runtime."
)]
pub fn register(ext: Box<dyn RichTextExtension>) {
    if SCHEMA_REALIZED.load(Ordering::Acquire) {
        panic!(
            "pine-richtext: register extension `{}` before any runtime is first resolved \
             (typically before App::run())",
            ext.name()
        );
    }

    let arc: Arc<dyn RichTextExtension> = Arc::from(ext);
    let mut exts = extensions().write().expect("extension registry poisoned");
    if exts.iter().any(|e| e.name() == arc.name()) {
        tracing::warn!(
            target: "pocopine.log",
            "pine-richtext: extension `{}` already registered, dropping duplicate",
            arc.name()
        );
        return;
    }
    exts.push(arc);
}

/// Snapshot of every registered extension in registration order.
/// Reads `EXTENSIONS` directly so [`crate::runtime::registry::default`]
/// can call this during its lazy fold without recursing.
pub fn registered() -> Vec<Arc<dyn RichTextExtension>> {
    read_extensions().clone()
}

/// Adapter: look up a named command via the default runtime.
/// Triggers default-runtime init on first call.
pub fn named_command(name: &str) -> Option<NamedCommand> {
    crate::runtime::registry::default().named_command(name)
}

/// Adapter: snapshot every key-binding factory contributed by the
/// default runtime's extensions, deduped first-wins. Preserves the
/// legacy contract that callers of this API get a merged view
/// (not the raw stored list, which can contain same-combo
/// duplicates).
pub fn merged_keymap_factories() -> Vec<(String, KeyBindingFactory)> {
    let runtime = crate::runtime::registry::default();
    let raw = runtime.merged_keymap_factories();
    let mut out: Vec<(String, KeyBindingFactory)> = Vec::with_capacity(raw.len());
    let mut seen: HashSet<String> = HashSet::with_capacity(raw.len());
    for (combo, factory) in raw {
        if seen.insert(combo.clone()) {
            out.push((combo.clone(), factory.clone()));
        }
    }
    out
}

/// Has the legacy registry been sealed? Used by
/// [`crate::runtime::registry::register`] to enforce the same panic-
/// on-late-registration contract for named runtimes that
/// [`register`] enforces for the legacy path.
pub fn is_schema_realized() -> bool {
    SCHEMA_REALIZED.load(Ordering::Acquire)
}

/// Flip the one-way "schema realized" switch when runtime resolution seals the
/// legacy default overlay; further [`register`] calls panic afterwards.
pub fn mark_schema_realized() {
    SCHEMA_REALIZED.store(true, Ordering::Release);
}

/// Adapter: every list-item-shaped node-type name contributed by the
/// default runtime's extensions.
pub fn list_item_type_names() -> HashSet<String> {
    crate::runtime::registry::default()
        .list_item_type_names()
        .clone()
}

/// Adapter: is `name` a list-item-shaped node type in the default
/// runtime? Used by [`crate::commands::is_list_item_type`] at command-
/// apply time on the list-conversion fast path.
pub fn is_list_item_type(name: &str) -> bool {
    crate::runtime::registry::default().is_list_item_type(name)
}

/// Adapter: every plugin contributed by the default runtime.
pub fn merged_plugins() -> Vec<Plugin> {
    crate::runtime::registry::default().plugins().to_vec()
}

/// Test-only: clear the user-extension list and unmark schema-
/// realized. The default runtime's `OnceLock` cache is untouched —
/// once it's been read in a test process, the cached Arc is the
/// authoritative folding for the rest of the run. Per-test isolation
/// for the runtime is achieved by building fresh
/// [`crate::runtime::RuntimeBuilder`]s instead.
#[cfg(test)]
pub(crate) fn __reset_for_tests() {
    if let Some(lock) = EXTENSIONS.get() {
        lock.write()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
    SCHEMA_REALIZED.store(false, Ordering::Release);
}
