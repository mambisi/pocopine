//! Ergonomics helpers shared by Pine's compound primitives.
//!
//! Every compound (Select, Popover, Dropdown, Combobox, Command,
//! HoverCard, Tooltip, …) implements the same few mechanical
//! patterns: stamp the Trigger with a per-root scope id, resolve
//! it back to an `Element` from Content, install `pp-anchor`
//! programmatically so author-side placement props flow through,
//! and — for combobox-style surfaces — wire a deferred,
//! self-GCing outside-dismiss listener. This module lifts those
//! patterns out of the per-primitive files so the authoring
//! surface of a new compound is *"Root provides its handle,
//! Trigger stamps, Content anchors."*
//!
//! Each helper takes an explicit `slug: &str` so the existing
//! (varied) attribute names — `"select"`, `"dm"`, `"dm-sub"`,
//! `"popover"`, `"hover-card"`, `"tooltip"`, `"combobox-input"` —
//! keep working without a data-attribute-rename migration.

use pocopine::ScopeId;
use web_sys::{Element, HtmlElement};

use pocopine_core::directives::anchor;

/// Stamp `data-pine-{slug}-trigger="{scope_id}"` on `el` so a
/// sibling Content can anchor to it without the author
/// coordinating selectors. Idempotent — re-stamping with the
/// same scope is a no-op.
pub fn stamp_trigger(el: &Element, root_scope: ScopeId, slug: &str) {
    let attr = format!("data-pine-{slug}-trigger");
    let _ = el.set_attribute(&attr, &format!("{}", root_scope.0));
}

/// The CSS selector matching a trigger stamped with
/// [`stamp_trigger`]: `[data-pine-{slug}-trigger="{scope_id}"]`.
pub fn trigger_selector(root_scope: ScopeId, slug: &str) -> String {
    format!("[data-pine-{slug}-trigger=\"{}\"]", root_scope.0)
}

/// Run `document.querySelector(trigger_selector(root_scope,
/// slug))`, returning the matching element if any.
pub fn resolve_trigger(root_scope: ScopeId, slug: &str) -> Option<Element> {
    let selector = trigger_selector(root_scope, slug);
    web_sys::window()?
        .document()?
        .query_selector(&selector)
        .ok()
        .flatten()
}

/// Install [`anchor::install`] on `floater` against the trigger
/// stamped with `slug`. Collapses the "resolve selector →
/// `dyn_into::<HtmlElement>` → parse `side`/`align` → install"
/// block that lived in every compound's Content `on_ready`.
///
/// No-op when the trigger isn't in the document (e.g. the Root
/// tag was authored without a Trigger).
pub fn install_anchor_to_trigger(
    floater: &HtmlElement,
    root_scope: ScopeId,
    slug: &str,
    side: &str,
    align: &str,
    side_offset: f64,
    flip: bool,
) {
    let Some(trigger) = resolve_trigger(root_scope, slug) else {
        return;
    };
    let placement = anchor::Placement {
        side: anchor::Side::parse(side),
        align: anchor::Align::parse(align),
    };
    anchor::install(floater, &trigger, placement, side_offset, flip);
}

/// Deferred outside-dismiss listener for a teleported dismissible
/// surface. Installs on the next macrotask (after the click that
/// opened the surface has finished dispatching) and self-removes
/// from the document once `content` detaches from the DOM —
/// so Portal's `pp-if` teardown is enough to GC the listener.
///
/// `inside_els` are all treated as "inside" the surface: a
/// `mousedown` whose target is contained by any of them is
/// ignored. Combobox uses this to keep its Input (teleported
/// apart from Content) inside the dismiss boundary.
pub fn install_outside_dismiss(
    content: Element,
    inside_els: Vec<Element>,
    on_dismiss: impl Fn() + 'static,
) {
    use pocopine::events::{self, ev, ListenerHandle};
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::JsCast;

    pocopine::tick::after_flush(move || {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        // Self-removing listener: the handle is stashed in an `Rc`
        // the callback closes over, so when `content` detaches the
        // callback drops the handle (which detaches the listener)
        // and then bails. The Rc itself is intentionally leaked —
        // ownership of the listener's lifetime moves to the
        // callback's content-detached path.
        let handle_slot: Rc<RefCell<Option<ListenerHandle>>> = Rc::new(RefCell::new(None));
        let handle_slot_for_cb = handle_slot.clone();
        let content_for_cb = content.clone();
        let inside_for_cb = inside_els.clone();
        let listener = events::on(&doc, ev::mousedown, move |ev| {
            let content_node: &web_sys::Node = content_for_cb.as_ref();
            if !content_node.is_connected() {
                handle_slot_for_cb.borrow_mut().take();
                return;
            }
            let Some(target) = ev.target().and_then(|t| t.dyn_into::<web_sys::Node>().ok()) else {
                return;
            };
            if content_node.contains(Some(&target)) {
                return;
            }
            for inside in &inside_for_cb {
                let n: &web_sys::Node = inside.as_ref();
                if n.contains(Some(&target)) || n.is_same_node(Some(&target)) {
                    return;
                }
            }
            on_dismiss();
        });
        *handle_slot.borrow_mut() = Some(listener);
        std::mem::forget(handle_slot);
    });
}
