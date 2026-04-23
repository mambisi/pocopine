use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "StressDemo.poco", style = "../tags_input/tags_input.css", role = "panel")]
pub struct StressDemo {
    pub open: bool,
    pub items: Vec<String>,
    /// End-to-end shuffle duration in ms (handler start → after the
    /// reactive flush finishes the DOM reconcile). Formatted into
    /// `shuffle_display` for the readout; kept separate so it's easy
    /// to grep / bind numerically from tests.
    pub last_shuffle_ms: f64,
    pub shuffle_display: String,
}

#[handlers]
impl StressDemo {
    pub fn mount_fixture(&mut self) {
        if !self.open {
            self.items = (0..500).map(|i| format!("item-{i:03}")).collect();
            self.open = true;
        } else {
            self.items.clear();
            self.open = false;
        }
    }

    pub fn shuffle(&mut self) {
        if self.items.len() < 2 {
            return;
        }

        // End-to-end timing: start before we mutate `items`, stop
        // after `pp-for`'s reconcile (and every item's transition
        // scheduler) has run. The reconcile fires synchronously on
        // handler return via `trigger_scope`, but the tick-driven
        // tail work (RFC-039 FLIP play + scheduled enter/leave
        // triggers) lands on the next microtask — so we stop the
        // clock in `after_flush`.
        let start = now_ms();
        let head = self.items.remove(0);
        self.items.push(head);

        let handle = this::<Self>();
        pocopine::tick::after_flush(move || {
            let dur = now_ms() - start;
            handle.update(|s: &mut Self| {
                s.last_shuffle_ms = dur;
                s.shuffle_display = format!("{dur:.1} ms");
            });
        });
    }
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}
