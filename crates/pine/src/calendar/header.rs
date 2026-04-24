//! Calendar header + navigation primitives:
//!
//! - `<pine-calendar-header>` — layout wrapper for the heading
//!   plus prev/next buttons. Structural only.
//! - `<pine-calendar-heading>` — renders the visible title
//!   (`"June 2024"`) mirrored from the root's `heading` state.
//! - `<pine-calendar-prev>` / `<pine-calendar-next>` — buttons
//!   that dispatch `prev_page` / `next_page` on the root and
//!   mirror the root's computed `prev_disabled` / `next_disabled`
//!   flags onto `disabled` / `aria-disabled` / `data-disabled`.
//!
//! Both Prev and Next are `<button>` primitives — the canonical
//! interactive-root tag per Pine's default-tag rule.

use crate::calendar::root::{PineCalendarRoot, ROOT};
use pocopine::inject;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

// ── Header ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarHeader.poco",
    role = "panel",
    display = "contents"
)]
// RFC 049 — a Header holds exactly Prev + Heading + Next.
// The three parts cooperate on month navigation (Prev/Next
// shift the visible-month; Heading labels it); dropping a
// stray button or div here confuses screen readers about the
// navigation unit.
#[slot(default, only = [PineCalendarPrev, PineCalendarHeading, PineCalendarNext])]
pub struct PineCalendarHeader {}

#[handlers]
impl PineCalendarHeader {}

// ── Heading ──────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarHeading.poco",
    role = "heading",
    display = "contents"
)]
pub struct PineCalendarHeading {
    #[observe(ROOT)]
    pub heading: String,
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineCalendarHeading {}

// ── Prev ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarPrev.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineCalendarPrev {
    #[observe(ROOT)]
    pub prev_disabled: bool,
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineCalendarPrev {
    pub fn on_click(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineCalendarRoot| r.prev_page());
        }
    }
}

// ── Next ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarNext.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineCalendarNext {
    #[observe(ROOT)]
    pub next_disabled: bool,
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineCalendarNext {
    pub fn on_click(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineCalendarRoot| r.next_page());
        }
    }
}
