//! Page scroll lock — RFC-021.
//!
//! Pine Dialog / Sheet / Drawer all need to freeze body scroll while
//! an overlay is open. Without it, iOS Safari rubber-bands the page
//! under the dialog, and desktop browsers jump content when the
//! scrollbar vanishes. Ref-counted so nested overlays compose
//! cleanly — the inner one's `unlock()` won't un-freeze while the
//! outer is still open.

use std::cell::{Cell, RefCell};

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static SAVED: RefCell<Option<Saved>> = const { RefCell::new(None) };
}

struct Saved {
    overflow: String,
    padding_right: String,
}

/// Increment the lock depth. On the `0 → 1` transition, pin the
/// current scroll, switch `<body>` to `overflow: hidden`, and pad
/// the right side by the scrollbar's measured width so visible
/// content doesn't jump.
pub fn lock() {
    let was_zero = DEPTH.with(|d| {
        let prev = d.get();
        d.set(prev + 1);
        prev == 0
    });
    if !was_zero {
        return;
    }

    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else { return };
    let Some(body) = doc.body() else { return };

    // Scrollbar width = viewport width − document-element client width.
    // Rounded-down on high-DPI displays; close enough for visual
    // stability.
    let doc_el = doc.document_element();
    let scrollbar_w = match (window.inner_width().ok().and_then(|v| v.as_f64()), doc_el) {
        (Some(vw), Some(d)) => (vw - d.client_width() as f64).max(0.0),
        _ => 0.0,
    };

    let style = body.style();
    let overflow = style.get_property_value("overflow").unwrap_or_default();
    let padding_right = style
        .get_property_value("padding-right")
        .unwrap_or_default();

    SAVED.with(|s| {
        *s.borrow_mut() = Some(Saved {
            overflow: overflow.clone(),
            padding_right: padding_right.clone(),
        });
    });

    let _ = style.set_property("overflow", "hidden");
    if scrollbar_w > 0.0 {
        // Accept whatever the author already had as base padding and
        // append the compensation. Simpler than parsing the existing
        // value — browsers coalesce repeated shorthand declarations.
        let base_px = padding_right
            .trim_end_matches("px")
            .parse::<f64>()
            .unwrap_or(0.0);
        let total = base_px + scrollbar_w;
        let _ = style.set_property("padding-right", &format!("{total}px"));
    }
}

/// Decrement the lock depth. On the `1 → 0` transition, restore the
/// body's prior inline `overflow` and `padding-right`. Saturating
/// at 0 so a stray unlock doesn't panic.
pub fn unlock() {
    let hit_zero = DEPTH.with(|d| {
        let prev = d.get();
        if prev == 0 {
            return false;
        }
        d.set(prev - 1);
        prev == 1
    });
    if !hit_zero {
        return;
    }

    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else { return };
    let Some(body) = doc.body() else { return };

    let saved = SAVED.with(|s| s.borrow_mut().take());
    let Some(saved) = saved else { return };

    let style = body.style();
    // Setting to empty removes the inline property — the DOM's
    // canonical "go back to the stylesheet's value".
    let _ = style.set_property("overflow", &saved.overflow);
    let _ = style.set_property("padding-right", &saved.padding_right);
}

/// Current lock depth. Useful for tests and debugging.
pub fn depth() -> u32 {
    DEPTH.with(|d| d.get())
}
