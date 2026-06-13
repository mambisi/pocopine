//! Shared overlay runtime used by Pine's Dialog and Popover
//! compounds (and available to DropdownMenu when we refactor it
//! to use the same plumbing).
//!
//! Owns a per-scope [`OverlayRuntime`] side-table: focus save
//! handle, focus trap, and whether scroll-lock is held. The
//! monolithic Pine overlays all had their own copy of this
//! exact struct + activate/deactivate pair; extracting it kills
//! ~40 lines per compound and matches reka-ui's
//! `DialogContentModal` / `PopoverContentModal` mixin shape.
//!
//! Each compound's Content calls [`activate`] when `open` flips
//! true and [`deactivate`] on close / unmount. Pass `modal=true`
//! to acquire scroll-lock + focus trap (Dialog); `modal=false`
//! for non-modal overlays (Popover).
//!
//! Popover and DropdownMenu also share the cancelable outside
//! interaction event contract here. Their template-level
//! `@*.outside` listeners call the dispatch helpers before
//! closing so authors can use `@pp:pointer-down-outside.prevent`
//! or `@pp:interact-outside.prevent` on the Content primitive.

use std::cell::RefCell;
use std::collections::HashMap;

use pocopine::{ScopeId, emit_cancelable, focus, scroll_lock};
use web_sys::Element;

/// Fired before an outside pointerdown dismisses an overlay.
///
/// The event is cancelable; `preventDefault()` keeps the overlay
/// open and suppresses that primitive's default outside-dismiss
/// action.
pub const POINTER_DOWN_OUTSIDE_EVENT: &str = "pp:pointer-down-outside";

/// Fired before a click/interact outside dismisses an overlay.
///
/// This covers the post-pointer activation step, matching Radix's
/// "interact outside" terminology while still travelling through
/// normal browser `CustomEvent.detail` plumbing.
pub const INTERACT_OUTSIDE_EVENT: &str = "pp:interact-outside";

thread_local! {
    static RUNTIMES: RefCell<HashMap<ScopeId, OverlayRuntime>> =
        RefCell::new(HashMap::new());
}

#[derive(Default)]
struct OverlayRuntime {
    trap: Option<focus::TrapHandle>,
    saved: Option<focus::Saved>,
    scroll_locked: bool,
}

/// Dispatch [`POINTER_DOWN_OUTSIDE_EVENT`] from the current
/// Content element. Returns `true` when an author prevented the
/// default dismiss action.
pub fn dispatch_pointer_down_outside() -> bool {
    emit_cancelable(POINTER_DOWN_OUTSIDE_EVENT, ())
}

/// Dispatch [`INTERACT_OUTSIDE_EVENT`] from the current Content
/// element. Returns `true` when an author prevented the default
/// dismiss action.
pub fn dispatch_interact_outside() -> bool {
    emit_cancelable(INTERACT_OUTSIDE_EVENT, ())
}

/// Install focus management + optional scroll-lock for an
/// overlay whose content element is `content`.
///
/// - Always saves the previously-focused element so it can be
///   restored on close.
/// - If `modal=true`, installs a focus trap on `content` and
///   locks body scroll.
/// - Auto-focuses the first focusable inside `content`.
///
/// Safe to call repeatedly on the same scope; if a runtime is
/// already registered, the existing one is released first.
pub fn activate(scope: ScopeId, content: &Element, modal: bool) {
    // Defensive — drop any stale runtime so we don't leak traps.
    deactivate(scope);

    let saved = focus::save();
    let trap = if modal {
        Some(focus::trap(content))
    } else {
        None
    };
    let scroll_locked = modal;
    if modal {
        scroll_lock::lock();
    }
    focus::auto_focus_first(content);

    RUNTIMES.with(|r| {
        r.borrow_mut().insert(
            scope,
            OverlayRuntime {
                trap,
                saved: Some(saved),
                scroll_locked,
            },
        );
    });
}

/// Teardown symmetric with [`activate`]: release the trap,
/// unlock scroll (if modal), blur + restore focus to the
/// previously-focused element.
pub fn deactivate(scope: ScopeId) {
    let Some(mut rt) = RUNTIMES.with(|r| r.borrow_mut().remove(&scope)) else {
        return;
    };
    if let Some(trap) = rt.trap.take() {
        trap.release();
    }
    if rt.scroll_locked {
        scroll_lock::unlock();
    }
    // Blur first so mobile keyboards dismiss even if the saved
    // element is gone; then restore.
    focus::blur();
    if let Some(saved) = rt.saved.take() {
        focus::restore(saved);
    }
}
