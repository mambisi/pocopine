//! Tests for the legacy `extension::registry` shim. Most adapter
//! behavior is covered exhaustively in `runtime/tests.rs` against
//! `RuntimeBuilder`; the tests here focus on the two contracts that
//! are specific to the legacy module:
//!
//! 1. `register` writes into the default runtime's extension list
//!    (until the runtime is sealed).
//! 2. `register` after seal panics, preserving the
//!    `mark_schema_realized` contract that the Phase 4 registry
//!    enforced via `SCHEMA_REALIZED`.
//!
//! Test isolation is best-effort: all tests share a process-global
//! `DEFAULT` cache in `runtime::registry`. Tests that exercise the
//! adapter reads (`named_command`, `merged_keymap_factories`, etc.)
//! belong in `runtime/tests.rs` where each test builds a fresh
//! `RuntimeBuilder` and doesn't depend on the shared cache.

#[allow(deprecated)]
use crate::extension::{ExtensionNodeView, KeyBindings, NamedCommand, RichTextExtension, registry};
use crate::runtime::registry::lock_tests;

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

    #[allow(deprecated)]
    registry::register(Box::new(StubExtension::named("foo")));
    let exts = registry::registered();
    assert!(
        exts.iter().any(|e| e.name() == "foo"),
        "registered extension should appear in registered() snapshot"
    );
}

#[test]
fn duplicate_name_drops_second_registration() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    #[allow(deprecated)]
    registry::register(Box::new(StubExtension::named("dup")));
    #[allow(deprecated)]
    registry::register(Box::new(StubExtension::named("dup")));
    let dup_count = registry::registered()
        .iter()
        .filter(|e| e.name() == "dup")
        .count();
    assert_eq!(dup_count, 1, "second registration with same name dropped");
}

#[test]
#[should_panic(expected = "before any runtime is first resolved")]
fn register_after_schema_realized_panics() {
    let _g = lock_tests();
    registry::__reset_for_tests();

    registry::mark_schema_realized();
    #[allow(deprecated)]
    registry::register(Box::new(StubExtension::named("late")));
}
