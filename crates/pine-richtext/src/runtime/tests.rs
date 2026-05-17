//! Tests for the per-instance [`super::EditorRuntime`] and named-runtime
//! registry.
//!
//! The most important assertion here is **schema byte-identity**: a
//! runtime built via [`super::RuntimeBuilder::new`] must produce
//! exactly the same rendered HTML for a seed doc as the legacy
//! `schema_basic::schema()` path. That's our migration-correctness
//! gate for commits 2–5.

use serde_json::json;

use crate::extension::{ExtensionNodeView, KeyBindings, NamedCommand, RichTextExtension};
use crate::extensions::CoreNodesExtension;
use crate::model::{Attrs, Fragment, Node};
use crate::render::render_doc_to_html;
use crate::{schema_basic, state::Plugin};

use super::registry::lock_tests;
use super::{registry, EditorRuntime, NodeViewSpec, RuntimeBuilder};

fn seed_doc(schema: &crate::model::Schema) -> Node {
    let title = schema
        .text("Title".to_string(), Vec::new())
        .expect("title text");
    let heading_attrs = {
        let mut a = Attrs::new();
        a.insert("level".to_string(), json!(2));
        a
    };
    let heading = schema
        .node("heading", heading_attrs, Fragment::from(title))
        .expect("heading");

    let hello = schema
        .text("Hello world".to_string(), Vec::new())
        .expect("hello text");
    let paragraph = schema
        .node("paragraph", Attrs::new(), Fragment::from(hello))
        .expect("paragraph");

    let item_text = schema
        .text("first item".to_string(), Vec::new())
        .expect("item text");
    let item_para = schema
        .node("paragraph", Attrs::new(), Fragment::from(item_text))
        .expect("item paragraph");
    let list_item = schema
        .node("list_item", Attrs::new(), Fragment::from(item_para))
        .expect("list_item");
    let bullet_list = schema
        .node("bullet_list", Attrs::new(), Fragment::from(list_item))
        .expect("bullet_list");

    schema
        .node(
            "doc",
            Attrs::new(),
            Fragment::from(vec![heading, paragraph, bullet_list]),
        )
        .expect("doc")
}

#[test]
fn runtime_builder_produces_schema_byte_identical_to_schema_basic() {
    let _g = lock_tests();

    // Building both schemas in the same process: schema_basic::schema()
    // folds default_extensions() under its OnceLock, RuntimeBuilder::new()
    // does the same fold but writes to its own bundle. The rendered
    // HTML of the same seed doc must be byte-identical.
    let basic = schema_basic::schema();
    let runtime = RuntimeBuilder::new().build();

    let basic_doc = seed_doc(&basic);
    let runtime_doc = seed_doc(runtime.schema());

    let basic_html = render_doc_to_html(&runtime, &basic_doc);
    let runtime_html = render_doc_to_html(&runtime, &runtime_doc);

    assert_eq!(
        basic_html, runtime_html,
        "runtime schema must render byte-identical HTML to schema_basic::schema()"
    );
}

#[test]
fn runtime_builder_without_defaults_yields_minimal_schema() {
    let _g = lock_tests();

    let runtime = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .build();

    // CoreNodesExtension contributes doc + paragraph + text (and a few
    // more) but no marks, no lists, no task items, no history plugin.
    assert!(runtime.schema().node_type("paragraph").is_ok());
    assert!(runtime.schema().node_type("bullet_list").is_err());
    assert!(runtime.schema().node_type("task_item").is_err());
    // Phase 5 C2: every runtime now auto-installs the input-rules
    // state-tracking plugin. That's the ONLY plugin a `without_defaults`
    // build carries — extension-contributed plugins (history, etc.)
    // are still absent.
    let plugin_keys: Vec<&str> = runtime.plugins().iter().map(|p| p.key()).collect();
    assert_eq!(plugin_keys, vec!["pine_richtext_input_rules"]);
    assert!(
        runtime.list_item_type_names().is_empty(),
        "minimal runtime carries no list-item types"
    );
}

#[test]
fn runtime_with_name_is_diagnostic_only() {
    let _g = lock_tests();
    let rt = RuntimeBuilder::new().name("comment").build();
    assert_eq!(rt.name(), Some("comment"));
}

#[test]
fn default_runtime_is_shared_arc() {
    let _g = lock_tests();
    let a = registry::default();
    let b = registry::default();
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "default() must return the same Arc on every call"
    );
}

#[test]
fn named_runtime_registers_and_resolves() {
    let _g = lock_tests();
    crate::extension::registry::__reset_for_tests();
    registry::__reset_named_for_tests();

    let comment = RuntimeBuilder::new().name("comment").build();
    registry::register("comment", comment.clone());

    let resolved = registry::resolve(Some("comment"));
    assert!(
        std::sync::Arc::ptr_eq(&resolved, &comment),
        "resolve must return the registered Arc"
    );
}

#[test]
fn unknown_runtime_name_falls_back_to_default() {
    let _g = lock_tests();
    registry::__reset_named_for_tests();

    let resolved = registry::resolve(Some("nope"));
    let default = registry::default();
    assert!(
        std::sync::Arc::ptr_eq(&resolved, &default),
        "unknown names must fall back to default (no panic)"
    );
}

#[test]
fn empty_runtime_name_falls_back_to_default() {
    let _g = lock_tests();
    let resolved = registry::resolve(Some(""));
    let default = registry::default();
    assert!(
        std::sync::Arc::ptr_eq(&resolved, &default),
        "empty string treated the same as None"
    );
}

#[test]
fn schema_basic_touch_does_not_block_runtime_register() {
    let _g = lock_tests();
    crate::extension::registry::__reset_for_tests();
    registry::__reset_named_for_tests();

    // Simulate the app boot path: build a seed doc via
    // `schema_basic::*` which flips `SCHEMA_REALIZED`. Explicitly
    // call `mark_schema_realized()` so the flag is DEFINITELY set
    // regardless of `schema_basic::schema()`'s `OnceLock` state
    // (which is process-sticky across the test suite).
    let _basic = schema_basic::schema();
    crate::extension::registry::mark_schema_realized();
    assert!(
        crate::extension::registry::is_schema_realized(),
        "SCHEMA_REALIZED must be set for this test to be meaningful"
    );

    // `runtime::register` must NOT gate on `SCHEMA_REALIZED`. The
    // legacy schema-realized flag is independent of the new
    // runtime-resolution seal that this commit introduced.
    let comment = RuntimeBuilder::new().name("comment").build();
    registry::register("comment", comment.clone());
    let resolved = registry::resolve(Some("comment"));
    assert!(
        std::sync::Arc::ptr_eq(&resolved, &comment),
        "schema-realized state must not seal the named-runtime registry"
    );
}

#[test]
#[should_panic(expected = "before any runtime is first resolved")]
fn named_runtime_register_after_resolve_panics() {
    let _g = lock_tests();
    crate::extension::registry::__reset_for_tests();
    registry::__reset_named_for_tests();

    // Mark the resolved seal directly (instead of going through
    // `default()` which is OnceLock-cached and might no-op on
    // re-entry). The contract under test is: once the seal flips,
    // `register` panics — regardless of how it got flipped.
    registry::__mark_runtimes_resolved_for_tests();
    let late = RuntimeBuilder::new().name("late").build();
    registry::register("late", late);
}

#[test]
#[should_panic(expected = "before any runtime is first resolved")]
fn named_runtime_resolve_seals_legacy_extension_register() {
    let _g = lock_tests();
    crate::extension::registry::__reset_for_tests();
    registry::__reset_named_for_tests();

    // Page that mounts only named-runtime editors. resolve(Some(...))
    // must seal the legacy `extension::register` path so a late
    // mount-time extension can't sneak into the global registry the
    // reconciler / commands still read in C2.
    let comment = RuntimeBuilder::new().name("comment").build();
    registry::register("comment", comment);
    let _ = registry::resolve(Some("comment"));

    struct LateExt;
    impl RichTextExtension for LateExt {
        fn name(&self) -> &str {
            "late"
        }
    }
    #[allow(deprecated)]
    crate::extension::registry::register(Box::new(LateExt));
}

#[test]
fn duplicate_runtime_name_drops_second_registration() {
    let _g = lock_tests();
    crate::extension::registry::__reset_for_tests();
    registry::__reset_named_for_tests();

    let a = RuntimeBuilder::new().name("dup").build();
    let b = RuntimeBuilder::new().name("dup").build();
    registry::register("dup", a.clone());
    registry::register("dup", b.clone());

    let resolved = registry::resolve(Some("dup"));
    assert!(
        std::sync::Arc::ptr_eq(&resolved, &a),
        "first-wins on duplicate registration"
    );
}

#[test]
fn two_runtimes_carry_independent_node_view_tags() {
    let _g = lock_tests();

    struct TaggedExt {
        name: &'static str,
        bindings: Vec<ExtensionNodeView>,
    }
    impl RichTextExtension for TaggedExt {
        fn name(&self) -> &str {
            self.name
        }
        fn node_views(&self) -> Vec<ExtensionNodeView> {
            self.bindings
                .iter()
                .map(|nv| ExtensionNodeView {
                    node_type: nv.node_type.clone(),
                    tag: nv.tag.clone(),
                    content_selector: nv.content_selector.clone(),
                })
                .collect()
        }
    }

    let a = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(TaggedExt {
            name: "rt-a",
            bindings: vec![ExtensionNodeView {
                node_type: "paragraph".into(),
                tag: "pine-paragraph-a".into(),
                content_selector: None,
            }],
        })
        .build();

    let b = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(TaggedExt {
            name: "rt-b",
            bindings: vec![ExtensionNodeView {
                node_type: "paragraph".into(),
                tag: "pine-paragraph-b".into(),
                content_selector: None,
            }],
        })
        .build();

    assert_eq!(
        a.lookup_node_view("paragraph").map(|s| s.tag.as_str()),
        Some("pine-paragraph-a")
    );
    assert_eq!(
        b.lookup_node_view("paragraph").map(|s| s.tag.as_str()),
        Some("pine-paragraph-b")
    );
}

#[test]
fn runtime_named_command_resolves_factory() {
    let _g = lock_tests();

    use crate::commands::BoxedCommand;
    use crate::state::{EditorState, Transaction};

    fn noop_named_command() -> NamedCommand {
        std::sync::Arc::new(|_args| {
            Some(Box::new(|_state: &EditorState| -> Option<Transaction> { None }) as BoxedCommand)
        })
    }

    struct CmdExt;
    impl RichTextExtension for CmdExt {
        fn name(&self) -> &str {
            "cmd-ext"
        }
        fn commands(&self) -> Vec<(String, NamedCommand)> {
            vec![("hello_command".into(), noop_named_command())]
        }
    }

    let rt = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(CmdExt)
        .build();

    assert!(rt.named_command("hello_command").is_some());
    assert!(rt.named_command("nope").is_none());
}

#[test]
fn runtime_keymap_factories_deduped_first_wins() {
    let _g = lock_tests();

    use crate::commands::BoxedCommand;
    use crate::extension::KeyBindingFactory;
    use crate::state::{EditorState, Transaction};

    fn noop_factory() -> KeyBindingFactory {
        std::sync::Arc::new(|| {
            Box::new(|_state: &EditorState| -> Option<Transaction> { None }) as BoxedCommand
        })
    }

    struct BindExt {
        name: &'static str,
        combo: &'static str,
    }
    impl RichTextExtension for BindExt {
        fn name(&self) -> &str {
            self.name
        }
        fn key_bindings(&self) -> KeyBindings {
            vec![(self.combo.into(), noop_factory())]
        }
    }

    let rt = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(BindExt {
            name: "a",
            combo: "Ctrl-Alt-Shift-x",
        })
        .with(BindExt {
            name: "b",
            combo: "Ctrl-Alt-Shift-x",
        })
        .build();

    // Builder appends every binding; first-wins is applied at
    // keymap construction time in view::input::default_keymap.
    let matching: Vec<_> = rt
        .merged_keymap_factories()
        .iter()
        .filter(|(c, _)| c == "Ctrl-Alt-Shift-x")
        .collect();
    assert_eq!(
        matching.len(),
        2,
        "runtime stores every binding; dedup is the keymap layer's job"
    );
}

#[test]
fn runtime_plugins_include_user_extension_plugins() {
    let _g = lock_tests();

    struct PluginExt;
    impl RichTextExtension for PluginExt {
        fn name(&self) -> &str {
            "plugin-ext"
        }
        fn plugins(&self) -> Vec<Plugin> {
            vec![Plugin::builder("runtime-test-plugin").finish()]
        }
    }

    let rt = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(PluginExt)
        .build();

    assert!(rt
        .plugins()
        .iter()
        .any(|p| p.key() == "runtime-test-plugin"));
}

#[test]
fn runtime_aggregates_list_item_types() {
    let _g = lock_tests();

    struct CalloutExt;
    impl RichTextExtension for CalloutExt {
        fn name(&self) -> &str {
            "callout-ext"
        }
        fn list_item_types(&self) -> &'static [&'static str] {
            &["callout_item"]
        }
    }

    let rt = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(CalloutExt)
        .build();
    assert!(rt.is_list_item_type("callout_item"));
    assert!(!rt.is_list_item_type("not_a_list_item"));
}

#[test]
fn runtime_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EditorRuntime>();
    assert_send_sync::<std::sync::Arc<EditorRuntime>>();
}

#[test]
fn user_named_command_shadows_base_via_different_name() {
    let _g = lock_tests();

    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::commands::BoxedCommand;
    use crate::state::{EditorState, Transaction};

    static USER_UNDO_INVOKED: AtomicUsize = AtomicUsize::new(0);

    struct UndoOverride;
    impl RichTextExtension for UndoOverride {
        fn name(&self) -> &str {
            "custom-undo-ext"
        }
        fn commands(&self) -> Vec<(String, NamedCommand)> {
            let factory: NamedCommand = std::sync::Arc::new(|_args| {
                USER_UNDO_INVOKED.fetch_add(1, Ordering::SeqCst);
                Some(
                    Box::new(|_state: &EditorState| -> Option<Transaction> { None })
                        as BoxedCommand,
                )
            });
            vec![("undo".into(), factory)]
        }
    }

    // The base `HistoryExtension` also contributes `undo`. With user-first
    // fold order, the user-registered extension wins by name even when its
    // own `name()` doesn't shadow the base extension by name. Asserting
    // via a side-effect counter (instead of `Arc::ptr_eq`) so the
    // observation is robust to the factory closure being re-constructed
    // on each `commands()` call.
    USER_UNDO_INVOKED.store(0, Ordering::SeqCst);
    let rt = RuntimeBuilder::new().with(UndoOverride).build();
    let factory = rt.named_command("undo").expect("undo bound");
    let _ = factory(serde_json::Value::Null);

    assert_eq!(
        USER_UNDO_INVOKED.load(Ordering::SeqCst),
        1,
        "user `undo` factory must be the one stored in the runtime, not base HistoryExtension's"
    );
}

#[test]
fn duplicate_user_extension_name_first_wins() {
    let _g = lock_tests();

    struct DupExt;
    impl RichTextExtension for DupExt {
        fn name(&self) -> &str {
            "dup-name"
        }
    }

    // Two `.with(DupExt)` calls with identical `name()` must not
    // crash the schema fold by duplicating node-specs. The second one
    // should be dropped first-wins.
    let rt = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(DupExt)
        .with(DupExt)
        .build();

    let dups: Vec<_> = rt
        .extensions()
        .iter()
        .filter(|e| e.name() == "dup-name")
        .collect();
    assert_eq!(
        dups.len(),
        1,
        "duplicate user extension name must be deduped first-wins"
    );
}

#[test]
fn user_node_view_binding_shadows_base_via_different_name() {
    let _g = lock_tests();

    struct ShadowTaskView;
    impl RichTextExtension for ShadowTaskView {
        fn name(&self) -> &str {
            "user-task-view-overlay"
        }
        fn node_views(&self) -> Vec<ExtensionNodeView> {
            vec![ExtensionNodeView {
                node_type: "task_item".into(),
                tag: "user-task-item".into(),
                content_selector: None,
            }]
        }
    }

    // User-first fold means a user node-view binding for `task_item`
    // wins over any base binding for the same node_type. (Base
    // `TaskListExtension::new()` ships with no node-view binding today,
    // but the contract must hold even if a future base extension adds
    // one.)
    let rt = RuntimeBuilder::new().with(ShadowTaskView).build();
    assert_eq!(
        rt.lookup_node_view("task_item").map(|s| s.tag.as_str()),
        Some("user-task-item"),
        "user node-view binding must take precedence over any base binding"
    );
}

#[test]
fn later_user_node_view_replaces_earlier_for_same_node_type() {
    let _g = lock_tests();

    struct PBindA;
    impl RichTextExtension for PBindA {
        fn name(&self) -> &str {
            "p-bind-a"
        }
        fn node_views(&self) -> Vec<ExtensionNodeView> {
            vec![ExtensionNodeView {
                node_type: "paragraph".into(),
                tag: "pine-paragraph-a".into(),
                content_selector: None,
            }]
        }
    }
    struct PBindB;
    impl RichTextExtension for PBindB {
        fn name(&self) -> &str {
            "p-bind-b"
        }
        fn node_views(&self) -> Vec<ExtensionNodeView> {
            vec![ExtensionNodeView {
                node_type: "paragraph".into(),
                tag: "pine-paragraph-b".into(),
                content_selector: None,
            }]
        }
    }

    // `.with(PBindA).with(PBindB)` — later registration must override
    // earlier for the same node_type, matching
    // `render::node_views::register`'s replace-on-insert semantics.
    let rt = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(PBindA)
        .with(PBindB)
        .build();
    assert_eq!(
        rt.lookup_node_view("paragraph").map(|s| s.tag.as_str()),
        Some("pine-paragraph-b"),
        "later node-view registration must replace earlier for same node_type"
    );
}

#[test]
fn base_plugin_protected_from_user_same_key_shadowing() {
    let _g = lock_tests();

    struct ShadowHistoryPluginExt;
    impl RichTextExtension for ShadowHistoryPluginExt {
        fn name(&self) -> &str {
            "shadow-history-plugin"
        }
        fn plugins(&self) -> Vec<Plugin> {
            // Same key as base HistoryExtension's plugin ("history").
            // Base must claim the slot first; this user plugin is dropped
            // with a warning.
            vec![Plugin::builder("history").finish()]
        }
    }

    let rt = RuntimeBuilder::new().with(ShadowHistoryPluginExt).build();

    // Exactly one plugin with key "history" — the base one.
    let history_count = rt.plugins().iter().filter(|p| p.key() == "history").count();
    assert_eq!(
        history_count, 1,
        "base plugin protected from user-key-collision shadowing"
    );

    // The base history_plugin contributes a state field; the user shadow
    // wouldn't. Smoke-test by confirming undo command resolves (proves
    // HistoryExtension was the one that landed).
    assert!(
        rt.named_command("undo").is_some(),
        "base HistoryExtension command should still be resolvable"
    );
}

#[test]
fn node_view_spec_through_runtime_carries_content_selector() {
    let _g = lock_tests();

    struct WithContent;
    impl RichTextExtension for WithContent {
        fn name(&self) -> &str {
            "with-content"
        }
        fn node_views(&self) -> Vec<ExtensionNodeView> {
            vec![ExtensionNodeView {
                node_type: "paragraph".into(),
                tag: "pine-p".into(),
                content_selector: Some("[data-content]".into()),
            }]
        }
    }

    let rt = RuntimeBuilder::new()
        .without_defaults()
        .with(CoreNodesExtension)
        .with(WithContent)
        .build();

    let spec: NodeViewSpec = rt
        .lookup_node_view("paragraph")
        .cloned()
        .expect("node view bound");
    assert_eq!(spec.tag, "pine-p");
    assert_eq!(spec.content_selector.as_deref(), Some("[data-content]"));
}
