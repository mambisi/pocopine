//! Outline-on-hover helper.
//!
//! Any panel row carrying `data-scope-id="N"` triggers an outline
//! on the live DOM element that owns scope N — via [`set_from_scope`].
//! The inspect picker calls [`apply`] directly to outline arbitrary
//! elements the user is hovering on the page (not just component
//! roots).

use std::cell::RefCell;

use web_sys::{Element, Node};

use crate::reactive::ScopeId;

pub(super) const HIGHLIGHT_CLASS: &str = "__pp_dev_highlight";

thread_local! {
    /// The element currently carrying `__pp_dev_highlight`. Tracked so
    /// we strip the class before adding it to a new element — idempotent
    /// across rapid pointer movement.
    static HIGHLIGHTED: RefCell<Option<Element>> = const { RefCell::new(None) };
}

/// Highlight the element whose scope id is `scope_id`. Passing
/// `None` clears the current highlight. Resolves the scope id to an
/// element via `walker::find_element_for_scope`.
pub(super) fn set_from_scope(scope_id: Option<ScopeId>) {
    let next = scope_id.and_then(crate::walker::find_element_for_scope);
    apply(next);
}

/// Highlight `next` directly, regardless of whether it owns a scope.
/// Used by the inspect picker (user hovers arbitrary elements on
/// the page, not just component roots).
pub(super) fn apply(next: Option<Element>) {
    let prev = HIGHLIGHTED.with(|h| h.borrow().clone());
    let same = match (&prev, &next) {
        (Some(a), Some(b)) => {
            let b_node: &Node = b.as_ref();
            a.is_same_node(Some(b_node))
        }
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }
    if let Some(el) = prev {
        remove_class(&el);
    }
    if let Some(ref el) = next {
        add_class(el);
    }
    HIGHLIGHTED.with(|h| *h.borrow_mut() = next);
}

fn add_class(el: &Element) {
    let cls = el.class_name();
    if !cls.split_whitespace().any(|c| c == HIGHLIGHT_CLASS) {
        el.set_class_name(format!("{cls} {HIGHLIGHT_CLASS}").trim());
    }
}

fn remove_class(el: &Element) {
    let kept: String = el
        .class_name()
        .split_whitespace()
        .filter(|c| *c != HIGHLIGHT_CLASS)
        .collect::<Vec<_>>()
        .join(" ");
    el.set_class_name(&kept);
}
