//! Shared floating-overlay runtime — modal focus trap + scroll lock.
//!
//! Used by the AppShell drawer (`app_shell`) and the Workspace floating
//! Aside (`workspace`). Focus traps / saved-focus handles aren't
//! serde-friendly, so they live in a per-scope side-table keyed by the
//! floating element's scope id — the same shape `pine::overlay` uses for
//! Dialog/Popover. Idempotent open/close.

#[cfg(target_arch = "wasm32")]
mod imp {
    use pocopine::{focus, scroll_lock};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use web_sys::Element;

    struct Runtime {
        saved: focus::Saved,
        // Held purely for its `Drop` — dropping detaches the trap.
        _trap: focus::TrapHandle,
    }

    thread_local! {
        static ACTIVE: RefCell<HashMap<u64, Runtime>> = RefCell::new(HashMap::new());
    }

    /// Activate: lock scroll, save focus, trap focus inside `el`, and move
    /// focus to its first focusable.
    pub(crate) fn open(scope: u64, el: &Element) {
        let already = ACTIVE.with(|d| d.borrow().contains_key(&scope));
        if already {
            return;
        }
        scroll_lock::lock();
        let saved = focus::save();
        let trap = focus::trap(el);
        let _ = focus::auto_focus_first(el);
        ACTIVE.with(|d| {
            d.borrow_mut().insert(scope, Runtime { saved, _trap: trap });
        });
    }

    /// Deactivate: untrap, unlock scroll, restore focus.
    pub(crate) fn close(scope: u64) {
        let rt = ACTIVE.with(|d| d.borrow_mut().remove(&scope));
        if let Some(rt) = rt {
            // `_trap` drops here, detaching the focus trap.
            scroll_lock::unlock();
            focus::restore(rt.saved);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use web_sys::Element;
    pub(crate) fn open(_scope: u64, _el: &Element) {}
    pub(crate) fn close(_scope: u64) {}
}

pub(crate) use imp::{close, open};
