//! Tests for the per-instance [`super::EditorRuntime`] and named-runtime
//! registry.
//!
//! The most important assertion here is **schema byte-identity**: a
//! runtime built via [`super::RuntimeBuilder::new`] must produce
//! exactly the same rendered HTML for a seed doc as the legacy
//! `schema_basic::schema()` path. That's our migration-correctness
//! gate for commits 2–5.

use serde_json::json;

use crate::extension::{KeyBindings, NamedCommand, RichTextExtension};
use crate::extensions::CoreNodesExtension;
use crate::model::{Attrs, Fragment, Node};
use crate::render::render_doc_to_html;
use crate::{schema_basic, state::Plugin};

use super::registry::lock_tests;
use super::{EditorRuntime, RuntimeBuilder, registry};

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
    // mount-time extension can't mutate the pending default-runtime overlay
    // after runtime composition has started.
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

    assert!(
        rt.plugins()
            .iter()
            .any(|p| p.key() == "runtime-test-plugin")
    );
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
fn markdown_serializer_picks_up_extension_emitters() {
    // Phase 6 C2: `EditorRuntime::markdown_serializer()` folds
    // each extension's `markdown_node_emitters()` into a fresh
    // `MarkdownSerializer`. `TaskListExtension` ships emitters
    // for `task_list` and `task_item` that render GFM task lists.
    use crate::extensions::TaskListExtension;

    let rt = RuntimeBuilder::new().with(TaskListExtension::new()).build();
    let serializer = rt.markdown_serializer();
    assert!(
        serializer.nodes.contains_key("task_list"),
        "task_list emitter should be registered",
    );
    assert!(
        serializer.nodes.contains_key("task_item"),
        "task_item emitter should be registered",
    );
}

#[test]
fn user_markdown_emitter_shadows_default_first_wins() {
    // Codex C2 P2: a user extension whose `markdown_node_emitters()`
    // contributes an entry for an already-registered node type
    // must shadow the default. The builder folds emitters
    // user-first / first-wins to match the same semantics as
    // commands and keymaps.
    use crate::extensions::TaskListExtension;
    use crate::markdown::{EventSink, NodeEmitter};
    use pulldown_cmark::{Event as MdEvent, Tag as MdTag, TagEnd as MdTagEnd};
    use std::sync::Arc;

    struct CustomTaskExt;
    impl RichTextExtension for CustomTaskExt {
        fn name(&self) -> &str {
            "custom-task-emitter"
        }
        fn markdown_node_emitters(&self) -> Vec<(String, NodeEmitter)> {
            vec![(
                "task_item".into(),
                Arc::new(|node, _parent, _index, sink: &mut EventSink<'_>| {
                    // Distinct sentinel so the test can detect the
                    // user emitter ran instead of the default.
                    sink.push(MdEvent::Start(MdTag::Item));
                    sink.push(MdEvent::Text("CUSTOM-MARKER ".into()));
                    sink.render_content(node);
                    sink.push(MdEvent::End(MdTagEnd::Item));
                }),
            )]
        }
    }

    // Build with the default extension chain (which includes
    // `TaskListExtension` contributing `task_item`+`task_list`
    // emitters) and add `CustomTaskExt` as a separate user
    // extension. User-first folding must let CustomTaskExt's
    // `task_item` emitter shadow the default.
    let rt = RuntimeBuilder::new().with(CustomTaskExt).build();
    let schema = rt.schema();

    let para_text = schema.text("body".to_string(), Vec::new()).unwrap();
    let para = schema
        .node("paragraph", Attrs::new(), Fragment::from(para_text))
        .unwrap();
    let mut attrs = Attrs::new();
    attrs.insert("checked".into(), json!(false));
    let item = schema
        .node("task_item", attrs, Fragment::from(para))
        .unwrap();
    let list = schema
        .node("task_list", Attrs::new(), Fragment::from(item))
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(list))
        .unwrap();

    let out = rt.markdown_serializer().serialize(&doc).unwrap();
    assert!(
        out.contains("CUSTOM-MARKER body"),
        "user emitter must shadow the default, got: {out}",
    );
    assert!(
        !out.contains("[ ] body"),
        "default GFM emitter must not have run, got: {out}",
    );

    // Suppress unused-import warning: TaskListExtension is
    // pulled in only because it provides the default emitter
    // we're proving the user can override.
    let _ = TaskListExtension::new();
}

#[test]
fn task_list_extension_emits_gfm_task_list_markdown() {
    // Build a doc containing a task_list with one checked + one
    // unchecked item, then serialize via the runtime's
    // markdown serializer. The output must use GFM `- [ ] ` /
    // `- [x] ` markers.
    use crate::extensions::TaskListExtension;

    let rt = RuntimeBuilder::new().with(TaskListExtension::new()).build();
    let schema = rt.schema();

    let para_text = schema.text("a thing".to_string(), Vec::new()).unwrap();
    let para = schema
        .node("paragraph", Attrs::new(), Fragment::from(para_text))
        .unwrap();

    let mut unchecked_attrs = Attrs::new();
    unchecked_attrs.insert("checked".into(), json!(false));
    let unchecked = schema
        .node("task_item", unchecked_attrs, Fragment::from(para.clone()))
        .unwrap();

    let mut checked_attrs = Attrs::new();
    checked_attrs.insert("checked".into(), json!(true));
    let para2_text = schema.text("done".to_string(), Vec::new()).unwrap();
    let para2 = schema
        .node("paragraph", Attrs::new(), Fragment::from(para2_text))
        .unwrap();
    let checked = schema
        .node("task_item", checked_attrs, Fragment::from(para2))
        .unwrap();

    let list = schema
        .node(
            "task_list",
            Attrs::new(),
            Fragment::from(vec![unchecked, checked]),
        )
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(list))
        .unwrap();

    let serializer = rt.markdown_serializer();
    let out = serializer.serialize(&doc).unwrap();
    assert!(
        out.contains("[ ] a thing"),
        "expected unchecked GFM marker, got: {out}",
    );
    assert!(
        out.contains("[x] done"),
        "expected checked GFM marker, got: {out}",
    );
}

#[test]
fn extension_can_contribute_both_mark_emitter_and_parse_rule() {
    // Phase 6 C4: prove the pluggable contract works end-to-end
    // for a custom mark. A fake `StrikethroughExtension` declares
    // a `strike` mark type, contributes a `markdown_mark_emitters`
    // entry that wraps text in `Tag::Strikethrough`, AND
    // contributes a `markdown_parse_rules` entry that builds the
    // mark from `Tag::Strikethrough` events on import. Symmetric:
    // export the model → serialized markdown → reparse → same
    // mark.
    use crate::markdown::{
        MarkEmitter, MarkRender, MarkdownParseRule, ParseMapping, ParseMatch, TagKind,
    };
    use crate::model::{Fragment, Mark, MarkSpec};
    use pulldown_cmark::{Tag as MdTag, TagEnd as MdTagEnd};
    use std::sync::Arc;

    struct StrikethroughExt;
    impl RichTextExtension for StrikethroughExt {
        fn name(&self) -> &str {
            "strikethrough"
        }
        fn marks(&self) -> Vec<MarkSpec> {
            vec![MarkSpec::new("strike")]
        }
        fn markdown_mark_emitters(&self) -> Vec<(String, MarkEmitter)> {
            vec![(
                "strike".into(),
                Arc::new(|_mark| MarkRender::Wrap(MdTag::Strikethrough)),
            )]
        }
        fn markdown_parse_rules(&self) -> Vec<MarkdownParseRule> {
            vec![MarkdownParseRule {
                matches: ParseMatch::Tag(TagKind::Strikethrough),
                maps_to: ParseMapping::Mark {
                    mark_type: "strike".into(),
                    get_attrs: None,
                },
            }]
        }
    }

    let rt = RuntimeBuilder::new().with(StrikethroughExt).build();
    let schema = rt.schema();

    // Build a doc with the custom mark applied.
    let strike = Mark::new("strike", Attrs::new());
    let text = schema.text("gone".to_string(), vec![strike]).unwrap();
    let para = schema
        .node("paragraph", Attrs::new(), Fragment::from(text))
        .unwrap();
    let doc = schema
        .node("doc", Attrs::new(), Fragment::from(para))
        .unwrap();

    // Export: serializer uses the registered mark emitter →
    // wraps text in Tag::Strikethrough → pulldown-cmark-to-cmark
    // renders as `~~gone~~`.
    let out = rt.markdown_serializer().serialize(&doc).unwrap();
    assert!(out.contains("~~gone~~"), "got: {out}");

    // Import: parser sees Tag::Strikethrough events and the
    // contributed rule builds the `strike` mark.
    let reparsed = rt.markdown_parser().parse(&out, schema).unwrap();
    // Find the text node and check its marks.
    fn find_marked_text<'a>(node: &'a Node, mark_name: &str) -> Option<&'a Node> {
        if node.type_name() == "text" && node.marks().iter().any(|m| m.type_name() == mark_name) {
            return Some(node);
        }
        for child in node.content().iter() {
            if let Some(found) = find_marked_text(child, mark_name) {
                return Some(found);
            }
        }
        None
    }
    let text_with_mark = find_marked_text(&reparsed, "strike").expect("strike mark on round-trip");
    assert_eq!(text_with_mark.text(), Some("gone"));

    // Silence the unused-import warning: we suppress this by
    // explicitly referencing TagEnd, since the parse rule
    // engine handles open/close itself.
    let _ = MdTagEnd::Strikethrough;
}

#[test]
fn pipe_table_markdown_without_table_rule_preserves_text() {
    // Codex C4 P2: when no `TagKind::Table` parse rule is
    // registered, the runtime parser must NOT enable
    // `pulldown_cmark::Options::ENABLE_TABLES`. Otherwise
    // pipe-table source like `| A | B |\n|---|---|\n| 1 | 2 |`
    // tokenizes into Tag::Table / Tag::TableRow / Tag::TableCell
    // events that the walker drops, leaving only the cell text
    // as loose paragraphs.
    //
    // With tables disabled, pulldown-cmark treats the same
    // source as plain text — the pipes survive as paragraph
    // content. This preserves user data even when the schema
    // doesn't support tables.
    let rt = RuntimeBuilder::new().build();
    let md = "| Header A | Header B |\n|----------|----------|\n| cell 1   | cell 2   |\n";
    let doc = rt.markdown_parser().parse(md, rt.schema()).unwrap();

    let mut all_text = String::new();
    fn gather(node: &Node, out: &mut String) {
        if let Some(t) = node.text() {
            out.push_str(t);
            return;
        }
        for c in node.content().iter() {
            gather(c, out);
        }
    }
    gather(&doc, &mut all_text);

    // Both header AND cell content must survive — preferably
    // verbatim with pipes, but at minimum the four data points.
    for needle in ["Header A", "Header B", "cell 1", "cell 2"] {
        assert!(
            all_text.contains(needle),
            "table content `{needle}` must survive import even without a table rule, got: {all_text:?}",
        );
    }
}
