//! Parent-owned slot fragment ABI (RFC-058 §5.5).
//!
//! Today the runtime walker captures slot content from a child
//! component's `light DOM` children, stashes the resulting
//! `DocumentFragment`s in a thread-local keyed on the child's
//! `ScopeId`, and replays them when the walker reaches the
//! child's `<slot>` placeholder. That model puts the walker in
//! the middle of every parent/child slot exchange — which is
//! the exact ownership boundary RFC-058 §5.5 wants to remove.
//!
//! The replacement: parents emit slot **fragment functions**
//! at compile time and pass them to the child's mount call.
//! When the child reaches `<slot>` it invokes the parent's
//! fragment function directly, with no walker discovery in
//! between. The fragment function runs in the parent's scope
//! (so `@click="parent_handler"` inside slotted content works
//! without scope acrobatics) and stamps directly into the
//! child's slot host.
//!
//! This module ships the **type surface** the macro-generated
//! mount code (RFC-058 Phase 3+) will populate. Phase 1 only
//! defines the shapes — the existing slots / capture path
//! continues to drive walker-mounted parents unchanged. Phase 3
//! replaces the runtime capture path for compiled parents with
//! direct fragment-function passing.

use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::JsCast;
use web_sys::DocumentFragment;

use crate::reactive::ScopeId;

/// Function pointer the macro emits per parent-authored slot.
///
/// The function is stateless and `'static` — it captures the
/// expression ASTs and constants from the parent template at
/// macro time, then runs against the live `SlotMountCtx` at
/// invocation time. Stateless `fn` (rather than `Box<dyn FnMut>`)
/// keeps the SlotSet payload compact across the eventual
/// Component Model boundary (RFC-058 §5.10).
pub type SlotFragment = fn(ctx: SlotMountCtx<'_>);

/// Per-invocation context for a slot fragment. The fragment
/// appends DOM into `host` (a buffer the runtime then splices
/// before the live `<slot>` element it's materialising), and
/// receives both scope ids — the parent's (so directive
/// expressions evaluate in the right scope) and the child's
/// (so refs registered inside the slotted content participate
/// in the correct child component's `refs::register` table).
///
/// Using a `DocumentFragment` rather than the live slot host
/// keeps the fragment side oblivious to where in the DOM the
/// content lands — same buffer pattern the legacy walker's
/// capture/replay path uses (see [`crate::walker::materialize_slot`]).
pub struct SlotMountCtx<'a> {
    pub host: &'a DocumentFragment,
    pub parent_scope_id: ScopeId,
    pub child_scope_id: ScopeId,
}

/// Set of slot fragments a parent passes to a child mount call.
/// Built fluently by the macro:
///
/// ```ignore
/// SlotSet::new()
///     .default(parent_default_slot_fn)
///     .named("footer", parent_footer_slot_fn);
/// ```
///
/// Backed by a small `HashMap<&'static str, SlotFragment>` —
/// the slot-name keys are macro-emitted string literals so the
/// hash cost is negligible at typical slot counts (1-3 slots
/// per component is the canonical case).
#[derive(Default)]
pub struct SlotSet {
    fragments: HashMap<&'static str, SlotFragment>,
}

/// Reserved slot name for the default (unnamed) slot. Matches
/// the wire-name the runtime walker uses for `default` slot
/// keying so the migration from walker-captured slots to
/// fragment-function slots can interoperate during Phase 3.
pub const DEFAULT_SLOT_NAME: &str = "default";

impl SlotSet {
    /// Empty set — what the runtime walker passes today for
    /// walker-driven mounts (no parent-emitted fragments yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the parent's default slot fragment. Convenience
    /// for `named(DEFAULT_SLOT_NAME, frag)`.
    pub fn default_slot(mut self, frag: SlotFragment) -> Self {
        self.fragments.insert(DEFAULT_SLOT_NAME, frag);
        self
    }

    /// Register a named slot fragment.
    pub fn named(mut self, name: &'static str, frag: SlotFragment) -> Self {
        self.fragments.insert(name, frag);
        self
    }

    /// Look up the fragment for `name`, or `None` if the parent
    /// didn't supply one for that slot. The child should fall
    /// back to its compiled default slot content in that case.
    pub fn get(&self, name: &str) -> Option<SlotFragment> {
        self.fragments.get(name).copied()
    }

    /// `true` when the parent supplied no fragments at all —
    /// i.e. an opaque component invocation with no children.
    /// The child's compiled default slot fragments handle
    /// every slot site.
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

// ─── runtime registry ────────────────────────────────────────────

thread_local! {
    /// Fragments the parent passed for each child component
    /// instance, keyed by the child's `ScopeId`. Populated by
    /// [`crate::walker::mount_child_component_with_slots`] right
    /// after [`crate::walker::mount_component`] instantiates the
    /// child, consumed by [`crate::walker::materialize_slot`]
    /// when the child template's `<slot>` element is reached.
    /// Cleared from [`crate::scope::Scope::remove`] so the map
    /// doesn't outlive the child component.
    static FRAGMENTS: RefCell<HashMap<ScopeId, SlotSet>> = RefCell::new(HashMap::new());
}

/// Register the parent-supplied [`SlotSet`] for a child component
/// instance. Idempotent — a repeat call replaces the prior set
/// (the runtime walker remounts the same scope id only after
/// teardown, so this only fires for fresh mounts).
///
/// No-op when the set is empty so an opaque mount call (every
/// `<my-tag></my-tag>` site) doesn't leak a registry entry.
pub fn install(scope_id: ScopeId, set: SlotSet) {
    if set.is_empty() {
        return;
    }
    FRAGMENTS.with(|m| {
        m.borrow_mut().insert(scope_id, set);
    });
}

/// Look up the parent fragment for `(child_scope_id, name)`.
/// Returns `None` when the parent didn't register a [`SlotSet`]
/// for this child, or registered one without an entry for
/// `name`. Caller (the walker's slot materialiser) falls back to
/// the legacy capture path when this returns `None`.
pub fn lookup(child_scope_id: ScopeId, name: &str) -> Option<SlotFragment> {
    FRAGMENTS.with(|m| m.borrow().get(&child_scope_id)?.get(name))
}

/// Drop the fragment registration for `child_scope_id`. Hooked
/// from [`crate::scope::Scope::remove`].
pub fn clear(child_scope_id: ScopeId) {
    FRAGMENTS.with(|m| {
        m.borrow_mut().remove(&child_scope_id);
    });
}

/// Stamp a static HTML string into the slot fragment buffer.
///
/// RFC-058 Phase 3.5b — the macro emits one [`SlotFragment`]
/// per child-mount site whose slot content is statically
/// eligible (no `pp-*` / `@` / `:` directives, no
/// non-HTML5-native descendants); the body of that fragment
/// is just `stamp_static_html(ctx.host, "<inline html>")`. By
/// going through a `<template>` element rather than
/// `set_inner_html` directly on the buffer, the parse picks
/// up the correct content-model rules (e.g. `<tr>` inside
/// `<table>`, `<li>` outside `<ul>`) the same way the
/// browser would for any author-written markup.
pub fn stamp_static_html(host: &DocumentFragment, html: &str) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(template) = doc.create_element("template") else {
        return;
    };
    template.set_inner_html(html);
    let Ok(template) = template.dyn_into::<web_sys::HtmlTemplateElement>() else {
        return;
    };
    let content = template.content();
    let kids = content.child_nodes();
    let mut snapshot: Vec<web_sys::Node> = Vec::with_capacity(kids.length() as usize);
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i) {
            snapshot.push(n);
        }
    }
    for n in snapshot {
        let _ = host.append_child(&n);
    }
}
