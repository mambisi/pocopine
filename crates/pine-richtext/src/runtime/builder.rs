//! Builder for [`super::EditorRuntime`].
//!
//! Mirrors the schema-fold logic in `schema_basic::schema()` and the
//! base-extension table population in
//! `extension::registry::install_base_extensions`, but writes into a
//! per-runtime bundle instead of process-global `OnceLock`s. The user-
//! name-wins overlay semantics carry over: a custom extension whose
//! `name()` matches a default-set extension *replaces* the default in
//! fold position, preserving node-insertion rank for content-match
//! resolution.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::extension::{KeyBindings, NamedCommand, RichTextExtension};
use crate::extensions::default_extensions;
use crate::inputrules::{input_rules as input_rules_plugin, InputRule};
use crate::model::Schema;
use crate::state::Plugin;

use super::node_views::{NodeViewRegistry, NodeViewSpec};
use super::EditorRuntime;

type ExtChain = Vec<Arc<dyn RichTextExtension>>;

/// Fluent builder for [`EditorRuntime`]. Created via
/// [`EditorRuntime::builder`] or [`RuntimeBuilder::new`].
///
/// ```ignore
/// let comment = RuntimeBuilder::new()
///     .name("comment")
///     .without_defaults()
///     .with(CoreNodesExtension)
///     .with(CoreInlineExtension)
///     .with(CoreMarksExtension)
///     .build();
/// ```
pub struct RuntimeBuilder {
    name: Option<String>,
    include_defaults: bool,
    extensions: Vec<Arc<dyn RichTextExtension>>,
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self {
            name: None,
            include_defaults: true,
            extensions: Vec::new(),
        }
    }

    /// Attach a diagnostic label. Surfaces in [`EditorRuntime::name`]
    /// and in `tracing::warn!` collision messages.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Add an extension. Extensions added later in fold order shadow
    /// earlier ones only if names collide; otherwise both contribute.
    pub fn with<E: RichTextExtension>(mut self, ext: E) -> Self {
        self.extensions
            .push(Arc::new(ext) as Arc<dyn RichTextExtension>);
        self
    }

    /// Add a pre-boxed extension. Useful when a trait object comes
    /// from a generic context that already produced `Box<dyn ...>`.
    pub fn with_boxed(mut self, ext: Box<dyn RichTextExtension>) -> Self {
        self.extensions.push(Arc::from(ext));
        self
    }

    /// Add a pre-Arc'd extension. Used by `runtime::default()` to
    /// bridge user extensions registered via the legacy
    /// `extension::registry::register` path (which stores
    /// `Arc<dyn RichTextExtension>` already) without losing the
    /// shared reference or re-boxing.
    pub fn with_arc(mut self, ext: Arc<dyn RichTextExtension>) -> Self {
        self.extensions.push(ext);
        self
    }

    /// Opt out of the kitchen-sink default extension chain. Use this
    /// for editors that should NOT have headings/lists/task-items —
    /// e.g. a comment box with only `paragraph` + basic marks.
    pub fn without_defaults(mut self) -> Self {
        self.include_defaults = false;
        self
    }

    /// Test-only: build the runtime with an explicit base extension
    /// list instead of `default_extensions()`. Lets tests pin
    /// base-vs-user precedence contracts (e.g. "user rules fire
    /// before base rules in the merged input-rules list") by
    /// injecting a known stub into the base chain.
    #[cfg(test)]
    pub(crate) fn build_with_explicit_base_for_tests(
        self,
        explicit_base: Vec<Arc<dyn RichTextExtension>>,
    ) -> Arc<EditorRuntime> {
        self.build_inner(Some(explicit_base))
    }

    /// Fold the extension chain into an immutable [`EditorRuntime`].
    /// Wraps in `Arc` so multiple mounts can share one fold.
    pub fn build(self) -> Arc<EditorRuntime> {
        self.build_inner(None)
    }

    /// Internal fold. Production path calls `build_inner(None)` to
    /// use `default_extensions()` as the base chain; tests can pass
    /// an explicit override.
    fn build_inner(
        self,
        explicit_base: Option<Vec<Arc<dyn RichTextExtension>>>,
    ) -> Arc<EditorRuntime> {
        let RuntimeBuilder {
            name: runtime_name,
            include_defaults,
            extensions: raw_user,
        } = self;

        // Step 1: dedup the user list first-wins by name, matching
        // `extension::registry::register`'s semantics. A repeated
        // `.with(MyExt).with(MyExt)` keeps the first and warns about
        // the second, instead of folding both into the schema and
        // crashing `Schema::builder().finish()` on duplicate node-types.
        let mut seen_user_names: HashSet<String> = HashSet::new();
        let user: ExtChain = raw_user
            .into_iter()
            .filter_map(|arc| {
                if !seen_user_names.insert(arc.name().to_string()) {
                    tracing::warn!(
                        target: "pocopine.log",
                        "pine-richtext: runtime builder dropping duplicate extension `{}` (first-wins)",
                        arc.name()
                    );
                    return None;
                }
                Some(arc)
            })
            .collect();

        // Step 2: resolve the effective schema-fold chain with user-name-wins
        // overlay. For each default-set extension, swap in any user
        // extension that shares its name(); then append remaining user
        // extensions in registration order. This preserves node-insertion
        // rank for content-match resolution (the same way
        // `schema_basic::schema()` does today).
        let (base_arcs, effective): (ExtChain, ExtChain) = if include_defaults
            || explicit_base.is_some()
        {
            let base: ExtChain = explicit_base.unwrap_or_else(|| {
                default_extensions()
                    .into_iter()
                    .map(|boxed| -> Arc<dyn RichTextExtension> { Arc::from(boxed) })
                    .collect()
            });
            let base_names: HashSet<String> = base.iter().map(|e| e.name().to_string()).collect();

            let mut effective: ExtChain = Vec::with_capacity(base.len() + user.len());
            for base_ext in &base {
                let replacement = user.iter().find(|u| u.name() == base_ext.name());
                effective.push(replacement.cloned().unwrap_or_else(|| base_ext.clone()));
            }
            for user_ext in &user {
                if !base_names.contains(user_ext.name()) {
                    effective.push(user_ext.clone());
                }
            }
            (base, effective)
        } else {
            (Vec::new(), user.clone())
        };

        // Step 3: fold node + mark specs into a schema using the overlay
        // order. Node-insertion rank matches `schema_basic::schema()` for
        // the default-runtime case.
        let schema = {
            let mut builder = Schema::builder();
            for ext in &effective {
                for spec in ext.nodes() {
                    builder = builder.node(spec);
                }
                for spec in ext.marks() {
                    builder = builder.mark(spec);
                }
            }
            builder.finish().expect("composed runtime schema is valid")
        };

        // Step 4: fold runtime tables. Each table has its own precedence
        // rule, mirroring the legacy `extension::registry::*` contracts:
        //
        //   * commands / key bindings — **user first, then base**, first-
        //     wins (user can override base by reusing a command key).
        //     Matches `named_command` and `merged_keymap_factories`.
        //   * plugins — **base first, then user**, first-wins on
        //     `plugin.key()`. Built-in plugins (e.g. `history`) protect
        //     their state field from accidental user shadowing.
        //     Matches `merged_plugins`'s documented contract.
        //   * list-item types — union (`HashSet`).
        //   * node-views — **user only**, later-wins among user
        //     extensions (matches `render::node_views::register`'s
        //     replace-on-insert semantics, which the legacy eager-forward
        //     in `extension::registry::register` relies on). Base
        //     extensions don't contribute node-view bindings — the
        //     legacy `install_base_extensions` ignored `node_views()`
        //     and only the user-registered path mirrored bindings into
        //     `render::node_views`.
        let mut commands: HashMap<String, NamedCommand> = HashMap::new();
        let mut key_bindings: KeyBindings = Vec::new();
        let mut list_item_types: HashSet<String> = HashSet::new();
        let mut node_views = NodeViewRegistry::new();
        let mut input_rules: Vec<InputRule> = Vec::new();

        // Iteration order: user extensions FIRST, then base
        // extensions whose name isn't shadowed by a user extension.
        // First-wins for commands / keymaps / list-item-types means
        // user contributions override built-in ones. Input rules
        // are first-MATCH-wins (a typing-time regex against the
        // cursor-adjacent text), so this same ordering means
        // user-contributed rules take precedence over built-in
        // rules when their patterns overlap. Documented contract:
        // `.with(MyExt)` registers rules ahead of any base rules
        // the runtime ships, AND ahead of any non-shadowing user
        // extensions added later (registration-order wins among
        // user extensions).
        for ext in user.iter().chain(
            base_arcs
                .iter()
                .filter(|b| !user.iter().any(|u| u.name() == b.name())),
        ) {
            for (key, factory) in ext.commands() {
                commands.entry(key).or_insert(factory);
            }
            for binding in ext.key_bindings() {
                key_bindings.push(binding);
            }
            for &name in ext.list_item_types() {
                list_item_types.insert(name.to_string());
            }
            for rule in ext.input_rules() {
                input_rules.push(rule);
            }
        }

        // Node-views: user-only, later-wins via `BTreeMap::insert`.
        for ext in &user {
            for nv in ext.node_views() {
                node_views.insert(
                    nv.node_type,
                    NodeViewSpec {
                        tag: nv.tag,
                        content_selector: nv.content_selector,
                    },
                );
            }
        }

        // Plugins: base first, then user. Same dedupe pattern as
        // `extension::registry::merged_plugins`. The input-rules
        // plugin is always included up-front so the rule-fire meta
        // path + `undo_input_rule` work for every runtime,
        // regardless of which extensions are registered. Its key
        // (`pine_richtext_input_rules`) is reserved.
        let mut plugins: Vec<Plugin> = vec![input_rules_plugin()];
        let mut seen_plugin_keys: HashSet<String> =
            plugins.iter().map(|p| p.key().to_string()).collect();
        let plugin_order = base_arcs
            .iter()
            .filter(|b| !user.iter().any(|u| u.name() == b.name()))
            .chain(user.iter());
        for ext in plugin_order {
            for plugin in ext.plugins() {
                if !seen_plugin_keys.insert(plugin.key().to_string()) {
                    tracing::warn!(
                        target: "pocopine.log",
                        "pine-richtext: plugin key `{}` already registered, keeping earlier binding (extension `{}` lost the race)",
                        plugin.key(),
                        ext.name()
                    );
                    continue;
                }
                plugins.push(plugin);
            }
        }

        Arc::new(EditorRuntime {
            name: runtime_name,
            schema,
            extensions: effective,
            node_views,
            commands,
            key_bindings,
            plugins,
            list_item_types,
            input_rules,
        })
    }
}
