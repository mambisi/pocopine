//! Breakpoint engine + `<pine-breakpoint>` provider.
//!
//! The engine watches the viewport via `matchMedia` at the five
//! Stylekit thresholds and resolves the largest matching tier. It is
//! the shared core behind both `<pine-breakpoint>` and
//! `<pine-app-shell>`; any component can reuse [`install`] to react to
//! breakpoint changes.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

create_context!(BREAKPOINT: Handle<PineBreakpoint>);

/// A responsive tier. Thresholds match Pine Stylekit's `sm:`/`md:`/
/// `lg:`/`xl:`/`2xl:` utility prefixes exactly, so a reactive
/// `data-breakpoint` value never disagrees with a utility class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breakpoint {
    /// `< 640px`.
    Base,
    /// `≥ 640px`.
    Sm,
    /// `≥ 768px`.
    Md,
    /// `≥ 1024px`.
    Lg,
    /// `≥ 1280px`.
    Xl,
    /// `≥ 1536px`.
    Xxl,
}

impl Breakpoint {
    /// Ascending `(tier, min-width-px)` pairs above [`Breakpoint::Base`].
    pub const TIERS: [(Breakpoint, u32); 5] = [
        (Breakpoint::Sm, 640),
        (Breakpoint::Md, 768),
        (Breakpoint::Lg, 1024),
        (Breakpoint::Xl, 1280),
        (Breakpoint::Xxl, 1536),
    ];

    /// Token string — `base`/`sm`/`md`/`lg`/`xl`/`2xl`.
    pub fn as_str(self) -> &'static str {
        match self {
            Breakpoint::Base => "base",
            Breakpoint::Sm => "sm",
            Breakpoint::Md => "md",
            Breakpoint::Lg => "lg",
            Breakpoint::Xl => "xl",
            Breakpoint::Xxl => "2xl",
        }
    }

    /// Parse a token string back into a tier. `None` for anything that
    /// is not one of the six tokens.
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "base" => Breakpoint::Base,
            "sm" => Breakpoint::Sm,
            "md" => Breakpoint::Md,
            "lg" => Breakpoint::Lg,
            "xl" => Breakpoint::Xl,
            "2xl" => Breakpoint::Xxl,
            _ => return None,
        })
    }

    /// Ordinal rank (`Base = 0 … Xxl = 5`) for ordered comparisons —
    /// e.g. "is the viewport at least `md`?" is `bp.rank() >=
    /// Breakpoint::Md.rank()`.
    pub fn rank(self) -> u8 {
        match self {
            Breakpoint::Base => 0,
            Breakpoint::Sm => 1,
            Breakpoint::Md => 2,
            Breakpoint::Lg => 3,
            Breakpoint::Xl => 4,
            Breakpoint::Xxl => 5,
        }
    }

    /// Lower bound of this tier, in CSS pixels.
    pub fn min_width(self) -> u32 {
        match self {
            Breakpoint::Base => 0,
            Breakpoint::Sm => 640,
            Breakpoint::Md => 768,
            Breakpoint::Lg => 1024,
            Breakpoint::Xl => 1280,
            Breakpoint::Xxl => 1536,
        }
    }
}

/// Live `matchMedia` `change` listeners, retained for the scope's
/// lifetime and detached together on unmount.
#[cfg(target_arch = "wasm32")]
type ChangeListeners =
    std::rc::Rc<std::cell::RefCell<Vec<wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>>>>;

/// Install a viewport breakpoint watcher for the current scope.
///
/// Calls `on_change` once with the initial tier (deferred past the
/// `on_ready` borrow) and again on every threshold crossing. All
/// `matchMedia` listeners are detached automatically when the scope
/// unmounts. Must be called from a component lifecycle hook so the
/// scope context is live.
#[cfg(target_arch = "wasm32")]
pub fn install(on_change: std::rc::Rc<dyn Fn(Breakpoint)>) {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    let Some(window) = web_sys::window() else {
        return;
    };

    let mut list = Vec::with_capacity(Breakpoint::TIERS.len());
    for (_tier, min_px) in Breakpoint::TIERS {
        if let Ok(Some(mql)) = window.match_media(&format!("(min-width: {min_px}px)")) {
            list.push(mql);
        }
    }
    let mqls = Rc::new(list);

    // One `change` listener per MediaQueryList. Each recomputes the
    // resolved tier from the full set, so we never miss a crossing.
    let closures: ChangeListeners = Rc::new(RefCell::new(Vec::with_capacity(mqls.len())));
    for mql in mqls.iter() {
        let mqls_for_cb = mqls.clone();
        let on_change_for_cb = on_change.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_ev: web_sys::Event| {
            on_change_for_cb(resolve(&mqls_for_cb));
        });
        let _ = mql.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref());
        closures.borrow_mut().push(cb);
    }

    // Detach + drop on unmount. Registered synchronously here (not in
    // `tick::next`) so the scope binding is still live.
    let mqls_for_cleanup = mqls.clone();
    let closures_for_cleanup = closures.clone();
    pocopine::on_scope_unmount(move || {
        {
            let cbs = closures_for_cleanup.borrow();
            for (mql, cb) in mqls_for_cleanup.iter().zip(cbs.iter()) {
                let _ =
                    mql.remove_event_listener_with_callback("change", cb.as_ref().unchecked_ref());
            }
        }
        closures_for_cleanup.borrow_mut().clear();
    });

    // Seed the initial value past the `on_ready` immutable borrow.
    let initial = resolve(&mqls);
    pocopine::tick::next(move || on_change(initial));
}

/// Largest tier whose `min-width` query currently matches. `TIERS` is
/// ascending, so the last match wins.
#[cfg(target_arch = "wasm32")]
fn resolve(mqls: &[web_sys::MediaQueryList]) -> Breakpoint {
    let mut current = Breakpoint::Base;
    for (i, (tier, _px)) in Breakpoint::TIERS.iter().enumerate() {
        if mqls.get(i).map(|m| m.matches()).unwrap_or(false) {
            current = *tier;
        }
    }
    current
}

/// Non-wasm / SSR builds have no viewport: the breakpoint stays at its
/// configured `initial` value.
#[cfg(not(target_arch = "wasm32"))]
pub fn install(_on_change: std::rc::Rc<dyn Fn(Breakpoint)>) {}

/// `<pine-breakpoint>` — a standalone responsive provider.
///
/// Resolves the current viewport tier and exposes it three ways:
/// `data-breakpoint` on its element (for CSS), the `BREAKPOINT`
/// context (descendant components can `#[observe(BREAKPOINT)] value`),
/// and the `pp:update:value` model event (`pp-model:value="bp"` in app
/// logic). Ships no layout of its own — set `.pine-breakpoint {
/// display: contents }` if you need it to be transparent.
///
/// ```html
/// <pine-breakpoint pp-model:value="bp">
///   <p>Viewport is at the {{bp}} tier.</p>
/// </pine-breakpoint>
/// ```
#[derive(Serialize, Deserialize)]
#[component(template = "PineBreakpoint.poco", role = "panel")]
pub struct PineBreakpoint {
    /// Current tier — `base`/`sm`/`md`/`lg`/`xl`/`2xl`.
    #[model]
    pub value: String,
    /// Tier assumed before `matchMedia` resolves, and the permanent
    /// value on non-wasm / SSR targets. Defaults to `base`.
    #[prop]
    pub initial: String,
}

impl Default for PineBreakpoint {
    fn default() -> Self {
        Self {
            value: String::new(),
            initial: "base".into(),
        }
    }
}

#[handlers]
impl PineBreakpoint {
    fn on_setup(&mut self) {
        if self.value.is_empty() {
            self.value = self.initial.clone();
        }
        BREAKPOINT.provide(this::<Self>());
    }

    fn on_ready(&self, handle: Handle<Self>) {
        let sink = handle;
        install(std::rc::Rc::new(move |bp: Breakpoint| {
            sink.update(|s| s.value = bp.as_str().to_string());
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::Breakpoint::*;
    use super::*;

    const ALL: [Breakpoint; 6] = [Base, Sm, Md, Lg, Xl, Xxl];

    #[test]
    fn token_roundtrips() {
        for bp in ALL {
            assert_eq!(Breakpoint::from_token(bp.as_str()), Some(bp));
        }
        assert_eq!(Base.as_str(), "base");
        assert_eq!(Xxl.as_str(), "2xl");
        assert_eq!(Breakpoint::from_token("nope"), None);
        assert_eq!(Breakpoint::from_token(""), None);
    }

    #[test]
    fn rank_is_strictly_ascending() {
        for pair in ALL.windows(2) {
            assert!(pair[0].rank() < pair[1].rank(), "{pair:?} not ascending");
        }
    }

    #[test]
    fn min_widths_match_stylekit() {
        assert_eq!(Base.min_width(), 0);
        assert_eq!(Sm.min_width(), 640);
        assert_eq!(Md.min_width(), 768);
        assert_eq!(Lg.min_width(), 1024);
        assert_eq!(Xl.min_width(), 1280);
        assert_eq!(Xxl.min_width(), 1536);
    }

    #[test]
    fn tiers_are_ascending_and_match_min_width() {
        let widths: Vec<u32> = Breakpoint::TIERS.iter().map(|(_, w)| *w).collect();
        let mut sorted = widths.clone();
        sorted.sort_unstable();
        assert_eq!(widths, sorted, "TIERS must be ascending");
        for (tier, w) in Breakpoint::TIERS {
            assert_eq!(tier.min_width(), w);
        }
    }
}
