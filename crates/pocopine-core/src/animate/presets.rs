//! Named transition presets — `"fade"`, `"scale"`, `"fade-scale"`,
//! `"zoom"`, `"slide-up"`, `"slide-down"`, `"slide-left"`,
//! `"slide-right"`, `"collapse"`, `"none"`.
//!
//! Each preset (except `collapse` / `none`) maps to three CSS atom
//! classes declared in `atoms.css`. [`apply_preset`] stamps the six
//! `pp-transition:*` attributes that the existing
//! [`crate::directives::transition`] state machine already consumes —
//! no new directive needed.
//!
//! Asymmetric presets (Svelte-style `in:` / `out:` split) come from
//! passing different `in_name` / `out_name` to [`apply_preset`]; the
//! macro turns `transition_in = "scale"` + `transition_out = "fade"`
//! into exactly that call.

use std::cell::RefCell;
use std::collections::HashMap;

use web_sys::Element;

/// The six class-string atoms that back one enter or leave phase of
/// a preset.
#[derive(Clone, Debug)]
pub struct Phase {
    /// Held for the whole phase. Carries the `transition-*` rules —
    /// `transition-property`, `transition-duration`, easing.
    pub base: &'static str,
    /// Visual extreme the phase starts from (opacity 0, scale 0.95,
    /// etc.). Applied for one frame, then swapped with `to`.
    pub from: &'static str,
    /// Visual extreme the phase ends at. Stays applied until the
    /// directive cleans up post-transition.
    pub to: &'static str,
}

/// Preset spec — two phases (enter + leave) describing the atom
/// classes applied at each step. Custom presets can be registered
/// via [`register_preset`].
#[derive(Clone, Debug)]
pub struct Preset {
    pub enter: Phase,
    pub leave: Phase,
}

impl Preset {
    /// Convenience: symmetric preset where leave is the reverse of
    /// enter (same base, from/to swapped).
    pub const fn symmetric(base: &'static str, from: &'static str, to: &'static str) -> Self {
        Self {
            enter: Phase { base, from, to },
            // For leave the visual state starts from `to` (the
            // visible/settled state) and animates back to `from`
            // (the extreme), so the directive's `leave-start /
            // leave-end` pair is (to, from).
            leave: Phase { base, from: to, to: from },
        }
    }
}

thread_local! {
    /// Registry of preset name → Preset. Seeded with the built-ins
    /// on first access; authors extend with [`register_preset`].
    static REGISTRY: RefCell<Option<HashMap<&'static str, Preset>>> =
        const { RefCell::new(None) };
}

fn with_registry<R>(f: impl FnOnce(&mut HashMap<&'static str, Preset>) -> R) -> R {
    REGISTRY.with(|cell| {
        let mut slot = cell.borrow_mut();
        let map = slot.get_or_insert_with(built_ins);
        f(map)
    })
}

fn built_ins() -> HashMap<&'static str, Preset> {
    let mut m = HashMap::new();
    m.insert(
        "fade",
        Preset::symmetric("pp-tx-fade-base", "pp-tx-fade-from", "pp-tx-fade-to"),
    );
    m.insert(
        "scale",
        Preset::symmetric("pp-tx-scale-base", "pp-tx-scale-from", "pp-tx-scale-to"),
    );
    m.insert(
        "fade-scale",
        Preset::symmetric(
            "pp-tx-fade-scale-base",
            "pp-tx-fade-scale-from",
            "pp-tx-fade-scale-to",
        ),
    );
    m.insert(
        "zoom",
        Preset::symmetric("pp-tx-zoom-base", "pp-tx-zoom-from", "pp-tx-zoom-to"),
    );
    m.insert(
        "slide-up",
        Preset::symmetric(
            "pp-tx-slide-up-base",
            "pp-tx-slide-up-from",
            "pp-tx-slide-up-to",
        ),
    );
    m.insert(
        "slide-down",
        Preset::symmetric(
            "pp-tx-slide-down-base",
            "pp-tx-slide-down-from",
            "pp-tx-slide-down-to",
        ),
    );
    m.insert(
        "slide-left",
        Preset::symmetric(
            "pp-tx-slide-left-base",
            "pp-tx-slide-left-from",
            "pp-tx-slide-left-to",
        ),
    );
    m.insert(
        "slide-right",
        Preset::symmetric(
            "pp-tx-slide-right-base",
            "pp-tx-slide-right-from",
            "pp-tx-slide-right-to",
        ),
    );
    // `collapse` — CSS-only auto-height open/close via
    // `grid-template-rows: 0fr | 1fr` (atoms.css ships the
    // matching rules). Works on arbitrary content without
    // measuring scrollHeight; the `.pp-tx-collapse-base > *`
    // descendant rule clamps the inner element so it can shrink
    // cleanly.
    m.insert(
        "collapse",
        Preset::symmetric(
            "pp-tx-collapse-base",
            "pp-tx-collapse-from",
            "pp-tx-collapse-to",
        ),
    );
    // `none` is the explicit opt-out — no classes, `apply_preset`
    // clears whatever was there.
    m.insert("none", Preset::symmetric("", "", ""));
    m
}

/// Register a custom preset. Returns `Err` if the name is already
/// taken. Call from `App::new()` or another boot-path hook; preset
/// lookup is global thread-local state.
pub fn register_preset(name: &'static str, preset: Preset) -> Result<(), &'static str> {
    with_registry(|m| {
        if m.contains_key(name) {
            Err("preset name already registered")
        } else {
            m.insert(name, preset);
            Ok(())
        }
    })
}

/// Look up a preset by name. Returns `None` for unknown names — the
/// caller decides whether that's a fatal or should fall back to
/// `none` (no-op).
pub fn lookup(name: &str) -> Option<Preset> {
    // `name` is a runtime `&str`, but the registry is keyed on
    // `&'static str`. We compare by string equality.
    with_registry(|m| {
        m.iter()
            .find(|(k, _)| **k == name)
            .map(|(_, p)| p.clone())
    })
}

/// Stamp the six `pp-transition:*` attributes on `el` so the
/// existing `pp-transition` state machine picks them up on next
/// `enter` / `leave`.
///
/// `in_name` drives enter, `out_name` drives leave. Symmetric usage
/// (`transition = "fade"`) passes the same name to both; asymmetric
/// (`transition_in = "scale", transition_out = "fade"`) passes two.
/// Unknown names fall back to `none` — safe no-op, no panic on typo.
pub fn apply_preset(el: &Element, in_name: &str, out_name: &str) {
    let in_preset = lookup(in_name).unwrap_or_else(|| lookup("none").unwrap());
    let out_preset = lookup(out_name).unwrap_or_else(|| lookup("none").unwrap());

    // Helper to write (or clear) a single attr.
    let set_or_clear = |attr: &str, value: &str| {
        if value.is_empty() {
            let _ = el.remove_attribute(attr);
        } else {
            let _ = el.set_attribute(attr, value);
        }
    };

    set_or_clear("pp-transition:enter", in_preset.enter.base);
    set_or_clear("pp-transition:enter-start", in_preset.enter.from);
    set_or_clear("pp-transition:enter-end", in_preset.enter.to);
    set_or_clear("pp-transition:leave", out_preset.leave.base);
    set_or_clear("pp-transition:leave-start", out_preset.leave.from);
    set_or_clear("pp-transition:leave-end", out_preset.leave.to);
}

/// Idempotently inject the preset atom stylesheet into
/// `document.head`. Called by `crate::animate::install()`, which the
/// runtime runs at `App::new()` time. Authors don't call this
/// directly.
pub(crate) fn inject_atoms_stylesheet() {
    crate::styles::inject_style("pocopine-animate-atoms", include_str!("atoms.css"));
}
