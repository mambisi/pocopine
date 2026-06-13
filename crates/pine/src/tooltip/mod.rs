//! `<pine-tooltip-*>` — hover / focus tooltip (compound).
//!
//! Reka-ui anatomy, minus Provider + Arrow for v0:
//!
//! - **Root** (`pine-tooltip-root`) — `open: bool`,
//!   `delay_duration` (default 700 ms). Provides Handle.
//! - **Trigger** (`pine-tooltip-trigger`) — wraps the author's
//!   target element. `on_ready` installs `mouseenter` /
//!   `mouseleave` / `focus` / `blur` listeners that drive
//!   Root.open with the Root-configured delay.
//! - **Portal** (`pine-tooltip-portal`) — teleport wrapper.
//! - **Content** (`pine-tooltip-content`) — `role="tooltip"`,
//!   auto-anchors to Trigger (same stamp scheme as DropdownMenu
//!   / Popover). No focus trap — tooltips never steal focus.
//!
//! ```html
//! <pine-tooltip-root>
//!   <pine-tooltip-trigger>
//!     <button>Save</button>
//!   </pine-tooltip-trigger>
//!   <pine-tooltip-portal>
//!     <pine-tooltip-content>Saves your work.</pine-tooltip-content>
//!   </pine-tooltip-portal>
//! </pine-tooltip-root>
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use crate::compound;
use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id, watch_scope_field_scoped, ScopeId};
use pocopine_core::scope::Scope;
use serde::{Deserialize, Serialize};

const SLUG: &str = "tooltip";

create_context!(ROOT: Handle<PineTooltipRoot>);
// Provider → descendants. Defaults + singleton policy live on the
// provider scope; tooltip Roots inside consult this handle in their
// `on_setup` and register with it when they open.
create_context!(PROVIDER: Handle<PineTooltipProvider>);

// ── Provider ──────────────────────────────────────────────────────

/// Groups a subtree of tooltips under one policy. Optional — a
/// `<pine-tooltip-root>` used outside a Provider keeps its own
/// local delay with no singleton coordination. Inside a Provider:
///
/// - `delay_duration` on the Provider becomes the default for
///   descendants (a Tooltip Root whose own `delay_duration` is
///   left at its default 700 picks up the Provider's value).
/// - At any moment only **one** tooltip inside the Provider is
///   open; when a second opens, the first closes.
///
/// ```html
/// <pine-tooltip-provider delay_duration="400">
///   <pine-tooltip-root>
///     <pine-tooltip-trigger><button>Save</button></pine-tooltip-trigger>
///     <pine-tooltip-portal><pine-tooltip-content>Saves</pine-tooltip-content></pine-tooltip-portal>
///   </pine-tooltip-root>
///   <pine-tooltip-root>
///     <pine-tooltip-trigger><button>Undo</button></pine-tooltip-trigger>
///     <pine-tooltip-portal><pine-tooltip-content>Undoes</pine-tooltip-content></pine-tooltip-portal>
///   </pine-tooltip-root>
/// </pine-tooltip-provider>
/// ```
#[derive(Serialize, Deserialize)]
#[component(template = "PineTooltipProvider.poco", role = "scope")]
// RFC 049 — Provider groups tooltip Roots under one policy.
// Only direct children that matter to Provider are Roots.
#[slot(default, only = [PineTooltipRoot])]
pub struct PineTooltipProvider {
    /// Default `delay_duration` inherited by descendants. Authors
    /// can still set `delay_duration` on an individual Root to
    /// override.
    #[prop]
    pub delay_duration: u32,
}

impl Default for PineTooltipProvider {
    fn default() -> Self {
        Self {
            delay_duration: 700,
        }
    }
}

#[handlers]
impl PineTooltipProvider {
    fn on_setup(&mut self) {
        PROVIDER.provide(this::<Self>());
    }

    fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            CURRENT_OPEN.with(|m| {
                m.borrow_mut().remove(&scope);
            });
        }
    }
}

// Tracks which Tooltip Root is currently open within each Provider.
// `None` entry = Provider present but no tooltip open right now.
thread_local! {
    static CURRENT_OPEN: RefCell<HashMap<ScopeId, Option<ScopeId>>> =
        RefCell::new(HashMap::new());
}

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineTooltipRoot.poco", role = "scope")]
// RFC 049 — Tooltip's Root has exactly Trigger + Portal.
// Provider sits OUTSIDE this Root (it's the top-level wrapper
// that shares delay/skip-duration state across tooltips) so
// isn't listed here.
#[slot(default, only = [PineTooltipTrigger, PineTooltipPortal])]
pub struct PineTooltipRoot {
    #[prop]
    pub open: bool,
    /// Delay (ms) before the tooltip appears on hover. Focus
    /// opens immediately (no delay — matches WAI-ARIA).
    #[prop]
    pub delay_duration: u32,
}

impl Default for PineTooltipRoot {
    fn default() -> Self {
        Self {
            open: false,
            delay_duration: 700,
        }
    }
}

#[handlers]
impl PineTooltipRoot {
    fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
        // Inherit Provider's default delay if Root is still at its
        // own default (700). An explicit `delay_duration` on the
        // Root tag always wins because static props apply before
        // `on_setup`, so `self.delay_duration` here already holds
        // whatever the author wrote.
        if self.delay_duration == 700
            && let Some(prov) = PROVIDER.inject() {
                self.delay_duration = prov.with(|p| p.delay_duration);
            }
    }

    fn on_ready(&self, scope: ScopeId) {
        // Singleton policy — watch our own `open` field. When it
        // flips true, evict whichever tooltip is currently open
        // in this Provider and take the slot. When it flips
        // false, release the slot. Runs only when a Provider
        // ancestor exists; standalone tooltips are unaffected.
        let root_id = scope;
        let Some(prov) = PROVIDER.inject() else {
            return;
        };
        let prov_id = prov.scope_id();
        watch_scope_field_scoped::<bool, _>(root_id, "open", move |&is_open, _| {
            if is_open {
                singleton_take_over(root_id, prov_id);
            } else {
                singleton_release(root_id, prov_id);
            }
        });
    }
}

/// Evict whichever tooltip is currently open inside the given
/// Provider (if any, and if it isn't `root_id` itself) and take
/// the slot.
fn singleton_take_over(root_id: ScopeId, prov_id: ScopeId) {
    let prev = CURRENT_OPEN.with(|m| {
        let mut map = m.borrow_mut();
        let slot = map.entry(prov_id).or_insert(None);
        let prev = slot.take();
        *slot = Some(root_id);
        prev
    });
    if let Some(prev_id) = prev
        && prev_id != root_id
            && let Some(scope) = Scope::find(prev_id)
                && let Some(rc) = scope.typed::<PineTooltipRoot>() {
                    Handle::new(rc, prev_id).update(|r: &mut PineTooltipRoot| r.open = false);
                }
}

/// Release the slot when this tooltip closes. Guarded on identity
/// so a rapid "open A → open B" sequence doesn't let A's delayed
/// close clobber B's take-over.
fn singleton_release(root_id: ScopeId, prov_id: ScopeId) {
    CURRENT_OPEN.with(|m| {
        let mut map = m.borrow_mut();
        if let Some(slot) = map.get_mut(&prov_id)
            && *slot == Some(root_id) {
                *slot = None;
            }
    });
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTooltipTrigger.poco", role = "panel")]
pub struct PineTooltipTrigger {}

#[handlers]
impl PineTooltipTrigger {
    fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = ROOT.inject() else { return };
        // Stamp the trigger's slot root so Content's auto-anchor
        // resolves to it. `pp-ref="trigger"` points at the
        // wrapper the template renders around the author's slot.
        if let Some(el) = refs.get("trigger") {
            compound::stamp_trigger(&el, root.scope_id(), SLUG);
            install_trigger_listeners(el, root);
        }
    }
}

fn install_trigger_listeners(trigger_el: web_sys::Element, root: Handle<PineTooltipRoot>) {
    use pocopine::events::{self, ev};
    use pocopine::timers;

    // Pending show-delay timer slot — auto-cancels at unmount.
    let pending = timers::Debounced::new_scoped();

    let enter_root = root.clone();
    let enter_pending = pending.clone();
    events::on_scoped(&trigger_el, ev::mouseenter, move |_e| {
        let delay = enter_root.with(|r| r.delay_duration);
        let root_for_timer = enter_root.clone();
        enter_pending.schedule(delay, move || {
            root_for_timer.update(|s| s.open = true);
        });
    });

    let leave_root = root.clone();
    let leave_pending = pending.clone();
    events::on_scoped(&trigger_el, ev::mouseleave, move |_e| {
        leave_pending.cancel();
        leave_root.update(|s| s.open = false);
    });

    // `focusin` + `focusout` bubble, so they fire for focus on a
    // descendant (e.g. the user's <button> slotted into Trigger's
    // `<span>` wrapper). Plain `focus` / `blur` don't bubble.
    let focus_root = root.clone();
    events::on_scoped(&trigger_el, ev::focusin, move |_e| {
        focus_root.update(|s| s.open = true);
    });

    events::on_scoped(&trigger_el, ev::focusout, move |_e| {
        pending.cancel();
        root.update(|s| s.open = false);
    });
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTooltipPortal.poco", role = "scope")]
// RFC 049 — Portal teleports to body; wraps just Content.
#[slot(default, only = [PineTooltipContent])]
pub struct PineTooltipPortal {
    #[observe(ROOT)]
    pub open: bool,
}

#[handlers]
impl PineTooltipPortal {}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineTooltipContent.poco",
    role = "panel",
    transition_in = "scale",
    transition_out = "fade"
)]
pub struct PineTooltipContent {
    pub anchor: String,
    #[prop]
    pub side: String,
    #[prop]
    pub align: String,
    #[prop]
    pub side_offset: f64,
}

impl Default for PineTooltipContent {
    fn default() -> Self {
        Self {
            anchor: String::new(),
            side: "top".into(),
            align: "center".into(),
            side_offset: 6.0,
        }
    }
}

#[handlers]
impl PineTooltipContent {
    fn on_setup(&mut self) {
        if let Some(root) = ROOT.inject() {
            self.anchor = compound::trigger_selector(root.scope_id(), SLUG);
        }
    }

    fn on_ready(&self, refs: pocopine::Refs) {
        let Some(content) = refs.get("content") else {
            return;
        };
        if let Ok(floater) = content.dyn_into::<web_sys::HtmlElement>()
            && let Some(root) = ROOT.inject() {
                compound::install_anchor_to_trigger(
                    &floater,
                    root.scope_id(),
                    SLUG,
                    &self.side,
                    &self.align,
                    self.side_offset,
                    true,
                );
            }
    }
}
