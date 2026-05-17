//! Tests for the extension registry. All tests share a process-global
//! state, so they serialize on `TEST_GUARD` and reset the registry at
//! the top of every test.

use std::sync::Arc;
use std::sync::Mutex;

use serde_json::Value;

use crate::commands::BoxedCommand;
use crate::extension::{
    registry, ExtensionNodeView, KeyBindingFactory, KeyBindings, NamedCommand, RichTextExtension,
};
use crate::state::{EditorState, Transaction};

static TEST_GUARD: Mutex<()> = Mutex::new(());

fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
    TEST_GUARD
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn noop_command_factory() -> KeyBindingFactory {
    Arc::new(|| Box::new(|_state: &EditorState| -> Option<Transaction> { None }) as BoxedCommand)
}

fn noop_named_command() -> NamedCommand {
    Arc::new(|_args: Value| {
        Some(Box::new(|_state: &EditorState| -> Option<Transaction> { None }) as BoxedCommand)
    })
}

#[derive(Default)]
struct StubExtension {
    name: &'static str,
    commands: Vec<(String, NamedCommand)>,
    key_bindings: KeyBindings,
    node_views: Vec<ExtensionNodeView>,
}

impl StubExtension {
    fn named(name: &'static str) -> Self {
        Self {
            name,
            commands: Vec::new(),
            key_bindings: Vec::new(),
            node_views: Vec::new(),
        }
    }
    fn with_command(mut self, key: &str, factory: NamedCommand) -> Self {
        self.commands.push((key.into(), factory));
        self
    }
    fn with_key_binding(mut self, combo: &str) -> Self {
        self.key_bindings
            .push((combo.into(), noop_command_factory()));
        self
    }
    fn with_node_view(mut self, node_type: &str, tag: &str) -> Self {
        self.node_views.push(ExtensionNodeView {
            node_type: node_type.into(),
            tag: tag.into(),
            content_selector: None,
        });
        self
    }
}

impl RichTextExtension for StubExtension {
    fn name(&self) -> &str {
        self.name
    }
    fn commands(&self) -> Vec<(String, NamedCommand)> {
        self.commands.clone()
    }
    fn key_bindings(&self) -> KeyBindings {
        self.key_bindings.clone()
    }
    fn node_views(&self) -> Vec<ExtensionNodeView> {
        self.node_views
            .iter()
            .map(|nv| ExtensionNodeView {
                node_type: nv.node_type.clone(),
                tag: nv.tag.clone(),
                content_selector: nv.content_selector.clone(),
            })
            .collect()
    }
}

#[test]
fn trait_is_object_safe() {
    // Compile-only check that `dyn RichTextExtension` resolves —
    // catches accidentally adding a generic method to the trait.
    let _: Box<dyn RichTextExtension> = Box::new(StubExtension::named("ok"));
}

#[test]
fn register_then_read_returns_extension() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    registry::register(Box::new(StubExtension::named("foo")));
    let exts = registry::registered();
    assert_eq!(exts.len(), 1);
    assert_eq!(exts[0].name(), "foo");
}

#[test]
fn duplicate_name_drops_second_registration() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    registry::register(Box::new(StubExtension::named("dup")));
    registry::register(Box::new(StubExtension::named("dup")));
    assert_eq!(
        registry::registered().len(),
        1,
        "second registration with same name dropped"
    );
}

#[test]
#[should_panic(expected = "before the schema is first read")]
fn register_after_schema_realized_panics() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    registry::mark_schema_realized();
    registry::register(Box::new(StubExtension::named("late")));
}

#[test]
fn named_command_lookup_resolves_factory() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    registry::register(Box::new(
        StubExtension::named("ext-with-cmd").with_command("my_command", noop_named_command()),
    ));

    assert!(registry::named_command("my_command").is_some());
    assert!(registry::named_command("nope").is_none());
}

#[test]
fn keymap_merge_dedupes_collisions_first_wins() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    // Use a combo no built-in extension claims so the test stays
    // robust to base-extension reads from other tests in the same
    // process (HistoryExtension contributes `Mod-z` / `Mod-Shift-z`).
    let combo = "Ctrl-Alt-Shift-x";

    registry::register(Box::new(StubExtension::named("a").with_key_binding(combo)));
    registry::register(Box::new(StubExtension::named("b").with_key_binding(combo)));

    let merged = registry::merged_keymap_factories();
    let matching: Vec<&(String, _)> = merged.iter().filter(|(c, _)| c == combo).collect();
    assert_eq!(matching.len(), 1, "duplicate combo deduped");
}

#[test]
fn list_item_type_names_aggregate_from_extensions() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    struct CalloutListExtension;
    impl RichTextExtension for CalloutListExtension {
        fn name(&self) -> &str {
            "callout-list"
        }
        fn list_item_types(&self) -> &'static [&'static str] {
            &["callout_item"]
        }
    }
    registry::register(Box::new(CalloutListExtension));

    assert!(
        registry::is_list_item_type("callout_item"),
        "registered extension's list-item type should resolve via is_list_item_type"
    );
    assert!(
        !registry::is_list_item_type("not_a_list_item"),
        "is_list_item_type is not a catch-all"
    );

    let names = registry::list_item_type_names();
    assert!(names.contains("callout_item"));
}

#[test]
fn merged_plugins_includes_user_registered_extension_plugins() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    use crate::state::Plugin;
    struct PluginExtension;
    impl RichTextExtension for PluginExtension {
        fn name(&self) -> &str {
            "extension-with-plugin"
        }
        fn plugins(&self) -> Vec<Plugin> {
            vec![Plugin::builder("my-extension-plugin").finish()]
        }
    }
    registry::register(Box::new(PluginExtension));

    let plugins = registry::merged_plugins();
    assert!(
        plugins.iter().any(|p| p.key() == "my-extension-plugin"),
        "user-registered extension's plugin should reach merged_plugins"
    );
}

#[test]
fn node_view_eager_forwards_into_render_registry() {
    let _g = lock_tests();
    registry::__reset_for_tests();
    crate::render::node_views::unregister("__test_node");

    registry::register(Box::new(
        StubExtension::named("nv").with_node_view("__test_node", "my-element"),
    ));

    let spec = crate::render::node_views::lookup("__test_node");
    assert!(spec.is_some(), "node-view should be eagerly forwarded");
    assert_eq!(spec.unwrap().tag, "my-element");

    crate::render::node_views::unregister("__test_node");
}
