//! Panel trait + registry.
//!
//! Replaces the hard-coded "Tree + Detail" rendering in the pre-split
//! devtools. Each panel owns its own state and DOM subtree; the shell
//! owns only the outer frame (header, meta line, body host container)
//! and the render loop, which asks the active panel to produce a
//! fingerprint and, when it differs from last tick, re-render.
//!
//! Panels are registered once at `install` time and never removed.
//! The active panel id selects which one paints into the body host;
//! inactive panels can still own state but don't render. PR A ships a
//! single panel (`scope`), so the tab strip is latent — the body
//! shows the active panel directly.
//!
//! Events dispatch here from `event.rs`: the delegated click handler
//! walks `data-action` ancestors and, for any action not handled by
//! the shell, calls [`dispatch_action_to_active`] which forwards to
//! the active panel's `handle_action`.

use std::cell::{Cell, RefCell};

use web_sys::Element;

/// What a panel promises to do. Everything string-based so panels
/// stay monomorphic and the shell can call through `dyn Panel` without
/// surprises.
pub(super) trait Panel {
    /// Stable id used as the DOM element id (`__pp_dev_panel_<id>`)
    /// and the tab-strip button target. Must be a valid attribute
    /// value and unique within the registry.
    fn id(&self) -> &'static str;

    /// Short human label shown in the (future) tab strip.
    fn label(&self) -> &'static str;

    /// One-time setup: populate `host` with the panel's static
    /// skeleton (tree container, detail container, etc.). Called
    /// once per host element. The render loop manages `host` —
    /// the panel just owns its inner HTML.
    fn mount(&self, host: &Element);

    /// Cheap string summarising "has anything visible changed?"
    /// Compared to the previous tick's value by pointer-eq on
    /// `String`. Empty string means "always re-render" (not
    /// recommended).
    fn fingerprint(&self) -> String;

    /// Repaint the panel body inside `host`. Called only when the
    /// previous fingerprint differs from [`Panel::fingerprint`] — so
    /// the heavy `set_inner_html` only fires when something the user
    /// can see actually changed.
    fn render(&self, host: &Element);

    /// Handle a `data-action="..."` click delegated from the shell.
    /// Return `true` if the panel consumed the action and the shell
    /// should stop dispatching. Returning `false` lets the shell fall
    /// through to its own handlers (and eventually to the copy-text
    /// path).
    fn handle_action(&self, action: &str, el: &Element) -> bool;
}

thread_local! {
    /// Registered panels in declaration order. PR A registers one
    /// panel (`scope`); later PRs append.
    static PANELS: RefCell<Vec<Box<dyn Panel>>> = RefCell::new(Vec::new());

    /// Per-panel last-fingerprint, indexed parallel to PANELS. Empty
    /// vec means "no render has happened yet"; growing PANELS always
    /// grows this too.
    static LAST_FPS: RefCell<Vec<String>> = RefCell::new(Vec::new());

    /// One-shot guard so `mount` fires exactly once per panel. Same
    /// indexing as PANELS / LAST_FPS.
    static MOUNTED: RefCell<Vec<bool>> = RefCell::new(Vec::new());

    /// Index of the currently-visible panel. `0` = the default
    /// (scope inspector); future PRs add a tab-strip setter.
    static ACTIVE: Cell<usize> = const { Cell::new(0) };
}

/// Push `panel` onto the registry. Called from `install`.
pub(super) fn register(panel: Box<dyn Panel>) {
    PANELS.with(|p| p.borrow_mut().push(panel));
    LAST_FPS.with(|f| f.borrow_mut().push(String::new()));
    MOUNTED.with(|m| m.borrow_mut().push(false));
}

/// Count of registered panels. The shell uses this to decide
/// whether to render a tab strip (≥2) or skip it (1).
pub(super) fn count() -> usize {
    PANELS.with(|p| p.borrow().len())
}

/// Index of the currently-visible panel.
pub(super) fn active_index() -> usize {
    ACTIVE.with(|c| c.get())
}

/// Switch the visible panel. No-op when `idx` is out of range. The
/// caller is responsible for triggering a repaint.
pub(super) fn set_active(idx: usize) {
    let n = count();
    if idx < n {
        ACTIVE.with(|c| c.set(idx));
    }
}

/// Copy out the `(id, label, is_active)` triple for each panel.
/// Used by the tab-strip renderer; exposed separately so the tab
/// strip doesn't need to peek into the registry.
pub(super) fn summary() -> Vec<(&'static str, &'static str, bool)> {
    let active = active_index();
    PANELS.with(|p| {
        p.borrow()
            .iter()
            .enumerate()
            .map(|(i, panel)| (panel.id(), panel.label(), i == active))
            .collect()
    })
}

/// Mount all unmounted panels into their respective host elements.
/// Safe to call every render — the per-panel `MOUNTED` flag
/// short-circuits repeat calls. `host_for(idx)` resolves the host
/// element the panel should paint into; the shell builds one host
/// per panel in its `build_shell_once` path.
pub(super) fn ensure_mounted(host_for: impl Fn(usize, &str) -> Option<Element>) {
    let n = count();
    for i in 0..n {
        let already = MOUNTED.with(|m| m.borrow()[i]);
        if already {
            continue;
        }
        let id = PANELS.with(|p| p.borrow()[i].id());
        let Some(host) = host_for(i, id) else { continue };
        PANELS.with(|p| p.borrow()[i].mount(&host));
        MOUNTED.with(|m| m.borrow_mut()[i] = true);
    }
}

/// Paint the active panel into `host` iff its fingerprint has
/// changed since the last render. The shell calls this once per
/// tick, passing the body-host element. Inactive panels aren't
/// asked to fingerprint — their state doesn't matter when they're
/// hidden.
pub(super) fn render_active(host: &Element) {
    let idx = active_index();
    if idx >= count() {
        return;
    }
    let fp = PANELS.with(|p| p.borrow()[idx].fingerprint());
    let changed = LAST_FPS.with(|fps| {
        let mut f = fps.borrow_mut();
        if f[idx] != fp {
            f[idx] = fp;
            true
        } else {
            false
        }
    });
    if !changed {
        return;
    }
    PANELS.with(|p| p.borrow()[idx].render(host));
}

/// Invalidate the active panel's fingerprint cache so the next
/// `render_active` call is forced to repaint. Used after event
/// handlers that mutate panel state — the handler's callers can
/// follow up with a synchronous `render()` at the shell level.
pub(super) fn invalidate_active() {
    let idx = active_index();
    if idx < count() {
        LAST_FPS.with(|fps| fps.borrow_mut()[idx] = String::new());
    }
}

/// Try the active panel's `handle_action`. Returns `true` if the
/// panel consumed the action.
pub(super) fn dispatch_action_to_active(action: &str, el: &Element) -> bool {
    let idx = active_index();
    if idx >= count() {
        return false;
    }
    PANELS.with(|p| p.borrow()[idx].handle_action(action, el))
}
