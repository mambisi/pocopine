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

use std::collections::HashMap;

use web_sys::Element;

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

/// Per-invocation context for a slot fragment. Owns the host
/// element the fragment should append into, plus both scope
/// ids — the parent's (so directive expressions evaluate in
/// the right scope) and the child's (so refs registered inside
/// the slotted content participate in the correct child
/// component's `refs::register` table).
pub struct SlotMountCtx<'a> {
    pub host: &'a Element,
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
