//! Shared-layout animations via element IDs.
//!
//! The "wow" feature popularised by Framer Motion's `layoutId` and
//! react-flip-toolkit's `flipId`: two elements in different tree
//! positions can share a layout identity, and when the DOM swaps
//! from one to the other, the new element animates from the old
//! one's position. Hero card → detail view, tab indicators sliding
//! under the active tab, dropdown menus that expand from their
//! trigger — all the same primitive.
//!
//! ## How it works
//!
//! 1. Before the DOM change, call [`snapshot_layout`] on a root
//!    element. It walks descendants tagged with
//!    `data-pp-layout-id="<id>"` and records their bounding rects.
//! 2. Mutate the DOM (add / remove / move nodes).
//! 3. After the mutation, call [`play_layout`] with the snapshot.
//!    Elements with matching IDs in the new tree animate from their
//!    old rect (via a `translate(dx, dy) scale(sx, sy)` transform
//!    that inverts to identity over the animation).
//! 4. Child elements with `data-pp-layout-child` counter-scale so
//!    their intrinsic size (text, borders) doesn't distort during
//!    the parent's scale animation — Motion's child-scale
//!    correction, in its one-deep form.
//!
//! ## What's *not* here (vs Motion)
//!
//! - Multi-level ancestor-scale cascade. We only correct one
//!   generation of children. Nested shared-layout doesn't compose
//!   perfectly yet; that would need the full projection tree.
//! - Border-radius and box-shadow counter-scaling.
//! - Automatic re-projection on every reactive update — authors
//!   snapshot/play explicitly.
//!
//! The omissions cover ~95% of real use cases with ~15% of the
//! code. Promote to the full projection tree when real content
//! demands it.

use std::collections::HashMap;

use web_sys::{DomRect, Element};

use crate::animate::animate;
use crate::easing::Easing;
use crate::spring::Spring;

const LAYOUT_ID_ATTR: &str = "data-pp-layout-id";
const LAYOUT_CHILD_ATTR: &str = "data-pp-layout-child";

/// Rectangle snapshot for one layout-tagged element.
#[derive(Clone, Copy, Debug)]
pub struct LayoutRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl LayoutRect {
    fn from_dom(r: &DomRect) -> Self {
        Self {
            left: r.left(),
            top: r.top(),
            width: r.width(),
            height: r.height(),
        }
    }
}

/// A before-state snapshot of an element tree's layout-tagged
/// descendants. Produced by [`snapshot_layout`]; consumed by
/// [`play_layout`].
#[derive(Debug, Default)]
pub struct LayoutSnapshot {
    rects: HashMap<String, LayoutRect>,
}

impl LayoutSnapshot {
    pub fn get(&self, id: &str) -> Option<LayoutRect> {
        self.rects.get(id).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }
}

/// Walk `root` and its descendants, recording client rects for every
/// element tagged `data-pp-layout-id="<id>"`. Duplicate IDs silently
/// overwrite (the last one wins — keep IDs unique within the
/// snapshot scope).
pub fn snapshot_layout(root: &Element) -> LayoutSnapshot {
    let mut out = LayoutSnapshot::default();
    collect_tagged(root, LAYOUT_ID_ATTR, |el, id| {
        out.rects.insert(id, LayoutRect::from_dom(&el.get_bounding_client_rect()));
    });
    out
}

/// Replay the snapshot against the current DOM under `root`. For
/// each element matching a snapshotted ID, compute the delta between
/// its old and new rects and animate a `translate + scale` from the
/// old rect's appearance to identity.
///
/// Scale correction: descendants of a matched element tagged
/// `data-pp-layout-child` receive the inverse scale so their
/// intrinsic size doesn't distort during the parent's scale tween.
pub fn play_layout(root: &Element, snapshot: LayoutSnapshot, spring: Spring) {
    if snapshot.is_empty() {
        return;
    }
    collect_tagged(root, LAYOUT_ID_ATTR, |el, id| {
        let Some(old) = snapshot.get(&id) else { return };
        let new_rect = el.get_bounding_client_rect();
        let dx = old.left - new_rect.left();
        let dy = old.top - new_rect.top();
        let sx = if new_rect.width() > 0.0 {
            old.width / new_rect.width()
        } else {
            1.0
        };
        let sy = if new_rect.height() > 0.0 {
            old.height / new_rect.height()
        } else {
            1.0
        };
        // Rest threshold — if the element didn't actually move or
        // resize, skip the animation.
        if dx.abs() < 0.5 && dy.abs() < 0.5 && (sx - 1.0).abs() < 1e-3 && (sy - 1.0).abs() < 1e-3 {
            return;
        }
        let from = format!(
            "translate({dx}px, {dy}px) scale({sx}, {sy})"
        );
        let to = "translate(0px, 0px) scale(1, 1)".to_string();

        // Transform-origin: top-left so the translate + scale
        // combination maps corners exactly (otherwise scale pulls
        // toward center and the motion fights the translate).
        if let Ok(html) = el.clone().dyn_into::<web_sys::HtmlElement>() {
            let _ = html.style().set_property("transform-origin", "top left");
        }

        animate(el, &[("transform", &from, &to)], Easing::Spring(spring));

        // Counter-scale one level of children so text / borders
        // don't stretch. Motion does this recursively up the
        // ancestor cascade; v0 just handles the direct-children
        // layer which covers most Hero-card-style content.
        if (sx - 1.0).abs() > 1e-3 || (sy - 1.0).abs() > 1e-3 {
            let inv_sx = 1.0 / sx.max(1e-6);
            let inv_sy = 1.0 / sy.max(1e-6);
            for_each_direct_child_with_attr(el, LAYOUT_CHILD_ATTR, |child| {
                let from = format!("scale({inv_sx}, {inv_sy})");
                let to = "scale(1, 1)".to_string();
                animate(child, &[("transform", &from, &to)], Easing::Spring(spring));
            });
        }
    });
}

/// Convenience — snapshot + mutate + play in one call. Mostly for
/// docs / tests; prefer explicit snapshot/play at real call sites
/// where the mutation is three reactive updates away.
pub fn project_with<F: FnOnce()>(root: &Element, mutate: F, spring: Spring) {
    let snap = snapshot_layout(root);
    mutate();
    play_layout(root, snap, spring);
}

// ─── internal walkers ────────────────────────────────────────────

fn collect_tagged(root: &Element, attr: &str, mut on_match: impl FnMut(&Element, String)) {
    collect_tagged_inner(root, attr, &mut on_match);
}

fn collect_tagged_inner(
    root: &Element,
    attr: &str,
    on_match: &mut dyn FnMut(&Element, String),
) {
    if let Some(id) = root.get_attribute(attr) {
        on_match(root, id);
    }
    let children = root.children();
    for i in 0..children.length() {
        if let Some(child) = children.item(i) {
            collect_tagged_inner(&child, attr, on_match);
        }
    }
}

fn for_each_direct_child_with_attr(
    parent: &Element,
    attr: &str,
    mut on_match: impl FnMut(&Element),
) {
    let children = parent.children();
    for i in 0..children.length() {
        if let Some(child) = children.item(i) {
            if child.has_attribute(attr) {
                on_match(&child);
            }
        }
    }
}

use wasm_bindgen::JsCast;
