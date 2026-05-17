//! Process-global registry for [`super::RichTextExtension`].
//!
//! Extensions register before `App::run()`. [`mark_schema_realized`]
//! flips a one-way switch the first time `schema_basic::schema()` is
//! folded; subsequent [`register`] calls panic with a message pointing
//! at the misorder.
//!
//! Reads ([`registered`], [`named_command`], [`merged_keymap_factories`])
//! are cheap and idempotent so the surface's `default_keymap()` can
//! rebuild on every mount without paying for re-folding.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use crate::render::node_views;
use crate::state::Plugin;

use super::{KeyBindingFactory, NamedCommand, RichTextExtension};

static EXTENSIONS: OnceLock<RwLock<Vec<Arc<dyn RichTextExtension>>>> = OnceLock::new();
static COMMANDS: OnceLock<RwLock<HashMap<String, NamedCommand>>> = OnceLock::new();
static KEY_BINDINGS: OnceLock<RwLock<Vec<(String, KeyBindingFactory)>>> = OnceLock::new();
static SCHEMA_REALIZED: AtomicBool = AtomicBool::new(false);

/// Everything the built-in default extensions contribute, captured
/// once by [`install_base_extensions`] during the schema fold. Reads
/// (`merged_*`, `is_list_item_type`, `named_command`) blend this with
/// the user-extension tables above.
struct BaseExtensionTables {
    commands: HashMap<String, NamedCommand>,
    key_bindings: Vec<(String, KeyBindingFactory)>,
    plugins: Vec<Plugin>,
    list_item_types: HashSet<&'static str>,
}

static BASE: OnceLock<BaseExtensionTables> = OnceLock::new();

fn extensions() -> &'static RwLock<Vec<Arc<dyn RichTextExtension>>> {
    EXTENSIONS.get_or_init(|| RwLock::new(Vec::new()))
}

fn commands_storage() -> &'static RwLock<HashMap<String, NamedCommand>> {
    COMMANDS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn key_bindings_storage() -> &'static RwLock<Vec<(String, KeyBindingFactory)>> {
    KEY_BINDINGS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Read-guard for the user-extension list under the registry's read lock.
fn read_extensions() -> std::sync::RwLockReadGuard<'static, Vec<Arc<dyn RichTextExtension>>> {
    extensions().read().expect("extension registry poisoned")
}

/// Read-guard for the user-extension command table.
fn read_commands() -> std::sync::RwLockReadGuard<'static, HashMap<String, NamedCommand>> {
    commands_storage()
        .read()
        .expect("command registry poisoned")
}

/// Read-guard for the user-extension key-binding list.
fn read_key_bindings() -> std::sync::RwLockReadGuard<'static, Vec<(String, KeyBindingFactory)>> {
    key_bindings_storage()
        .read()
        .expect("key bindings registry poisoned")
}

/// Register an extension. Must be called before
/// `schema_basic::schema()` is first read (typically before
/// `pocopine::App::run()`). Registering after the schema has been
/// realized panics.
///
/// On a duplicate `name()`, the second registration is dropped with a
/// `tracing::warn!` so authors notice the collision. Command-name
/// collisions across extensions follow the same first-wins rule.
///
/// Node-view bindings are forwarded eagerly into
/// [`crate::render::node_views`] at registration time so the
/// reconciler can look them up before the schema fold ever runs.
pub fn register(ext: Box<dyn RichTextExtension>) {
    if SCHEMA_REALIZED.load(Ordering::Acquire) {
        panic!(
            "pine-richtext: register extension `{}` before the schema is first read \
             (typically before App::run())",
            ext.name()
        );
    }

    let arc: Arc<dyn RichTextExtension> = Arc::from(ext);

    {
        let mut exts = extensions().write().expect("extension registry poisoned");
        if exts.iter().any(|e| e.name() == arc.name()) {
            tracing::warn!(
                target: "pocopine.log",
                "pine-richtext: extension `{}` already registered, dropping duplicate",
                arc.name()
            );
            return;
        }
        exts.push(arc.clone());
    }

    {
        let mut cmds = commands_storage()
            .write()
            .expect("command registry poisoned");
        for (key, factory) in arc.commands() {
            if cmds.contains_key(&key) {
                tracing::warn!(
                    target: "pocopine.log",
                    "pine-richtext: command `{}` already registered, keeping earlier binding \
                     (extension `{}` lost the race)",
                    key,
                    arc.name()
                );
                continue;
            }
            cmds.insert(key, factory);
        }
    }

    {
        let mut bindings = key_bindings_storage()
            .write()
            .expect("key bindings registry poisoned");
        for binding in arc.key_bindings() {
            bindings.push(binding);
        }
    }

    for nv in arc.node_views() {
        match nv.content_selector {
            Some(sel) => node_views::register_with_content(nv.node_type, nv.tag, sel),
            None => node_views::register(nv.node_type, nv.tag),
        }
    }
}

/// Snapshot of every registered extension in registration order.
/// Cheap; intended for the schema fold and for diagnostic tooling.
pub fn registered() -> Vec<Arc<dyn RichTextExtension>> {
    read_extensions().clone()
}

/// Look up a named command. User-registered commands take priority over
/// built-in base commands so an extension can override a default by
/// registering the same name before `schema()` runs.
pub fn named_command(name: &str) -> Option<NamedCommand> {
    let user = read_commands().get(name).cloned();
    user.or_else(|| BASE.get().and_then(|b| b.commands.get(name).cloned()))
}

/// Snapshot of every extension-contributed key binding, deduped
/// first-wins, with user-registered bindings preceding built-in base
/// bindings (so extensions can shadow defaults). The base 4 keymap
/// entries (`Backspace`, `Delete`, `Enter`, `Mod-a`) in
/// [`crate::view::input::default_keymap`] are installed *before* this
/// list, so they're protected by `KeyMap::lookup`'s first-wins
/// semantics regardless of what extensions contribute.
pub fn merged_keymap_factories() -> Vec<(String, KeyBindingFactory)> {
    let mut out: Vec<(String, KeyBindingFactory)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // User extensions first — they get priority over built-in defaults.
    let raw = read_key_bindings();
    for (combo, factory) in raw.iter() {
        if !seen.insert(combo.clone()) {
            tracing::warn!(
                target: "pocopine.log",
                "pine-richtext: key binding `{}` registered twice, keeping earlier extension's binding",
                combo
            );
            continue;
        }
        out.push((combo.clone(), factory.clone()));
    }

    // Base / default extension bindings fill in remaining combos.
    if let Some(base) = BASE.get() {
        for (combo, factory) in &base.key_bindings {
            if seen.insert(combo.clone()) {
                out.push((combo.clone(), factory.clone()));
            }
        }
    }

    out
}

/// Flip the one-way "schema realized" switch. Called by
/// `schema_basic::schema()` the first time the schema is folded;
/// further `register` calls then panic.
pub fn mark_schema_realized() {
    SCHEMA_REALIZED.store(true, Ordering::Release);
}

/// Fold the built-in base extensions into [`BASE`]. Called exactly
/// once by `schema_basic::schema()` during its `OnceLock` init closure.
///
/// Base contributions live in their own table so user-registered
/// overrides win on name collisions during lookup
/// ([`named_command`] / [`merged_keymap_factories`] / [`merged_plugins`]).
pub fn install_base_extensions<I>(base: I)
where
    I: IntoIterator<Item = Arc<dyn RichTextExtension>>,
{
    let mut tables = BaseExtensionTables {
        commands: HashMap::new(),
        key_bindings: Vec::new(),
        plugins: Vec::new(),
        list_item_types: HashSet::new(),
    };
    for ext in base {
        for (key, factory) in ext.commands() {
            tables.commands.entry(key).or_insert(factory);
        }
        for binding in ext.key_bindings() {
            tables.key_bindings.push(binding);
        }
        for plugin in ext.plugins() {
            tables.plugins.push(plugin);
        }
        for &name in ext.list_item_types() {
            tables.list_item_types.insert(name);
        }
    }
    let _ = BASE.set(tables);
}

/// Snapshot of every list-item-shaped node-type name contributed by
/// base + user-registered extensions. Used by the list-conversion
/// fast path to detect whether the selection sits inside a list of
/// any item type.
pub fn list_item_type_names() -> HashSet<String> {
    let mut out: HashSet<String> = BASE
        .get()
        .map(|b| b.list_item_types.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    for ext in read_extensions().iter() {
        for &name in ext.list_item_types() {
            out.insert(name.to_string());
        }
    }
    out
}

/// Is `name` a list-item-shaped node type contributed by any
/// extension? Returns `false` when called before
/// `mark_schema_realized` — extensions haven't seeded the table yet.
pub fn is_list_item_type(name: &str) -> bool {
    BASE.get().is_some_and(|b| b.list_item_types.contains(name))
        || read_extensions()
            .iter()
            .any(|ext| ext.list_item_types().contains(&name))
}

/// Snapshot of every plugin contributed by built-in base + user
/// extensions. Base plugins claim their key first so user extensions
/// can't accidentally shadow built-in plugin state; a same-key
/// collision across user extensions is logged and skipped.
pub fn merged_plugins() -> Vec<Plugin> {
    let mut out: Vec<Plugin> = BASE.get().map(|b| b.plugins.clone()).unwrap_or_default();
    let mut seen_keys: HashSet<String> = out.iter().map(|p| p.key().to_string()).collect();
    for ext in read_extensions().iter() {
        for plugin in ext.plugins() {
            if !seen_keys.insert(plugin.key().to_string()) {
                tracing::warn!(
                    target: "pocopine.log",
                    "pine-richtext: plugin key `{}` already registered, keeping earlier binding (extension `{}` lost the race)",
                    plugin.key(),
                    ext.name()
                );
                continue;
            }
            out.push(plugin);
        }
    }
    out
}

/// Test-only: reset user-registry slots and unmark schema-realized.
///
/// Base extension tables are intentionally left alone — once the
/// process has folded the schema (which happens the first time any
/// test touches `schema_basic::schema()`), the base tables hold the
/// authoritative built-in commands/bindings for the rest of the
/// process, and rebuilding them would race with parallel tests
/// reading them through `named_command` / `merged_keymap_factories`.
#[cfg(test)]
pub(crate) fn __reset_for_tests() {
    if let Some(lock) = EXTENSIONS.get() {
        lock.write()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
    if let Some(lock) = COMMANDS.get() {
        lock.write()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
    if let Some(lock) = KEY_BINDINGS.get() {
        lock.write()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
    SCHEMA_REALIZED.store(false, Ordering::Release);
}
