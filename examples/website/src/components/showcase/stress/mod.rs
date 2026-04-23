use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "StressDemo.poco", style = "../tags_input/tags_input.css", role = "panel")]
pub struct StressDemo {
    pub open: bool,
    pub items: Vec<String>,
    /// End-to-end duration in ms for the most recent reorder
    /// (handler start → after the reactive flush finishes the DOM
    /// reconcile). Formatted into `shuffle_display` for the
    /// readout; kept numerically for tests / devtools.
    pub last_shuffle_ms: f64,
    pub shuffle_display: String,
    /// Label of the last operation — `"rotate"` or `"shuffle"`.
    /// Drives the readout text so the user can tell which timing
    /// they're looking at.
    pub last_op: String,
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

    /// Rotate: pop the first item and push it onto the tail. One
    /// item actually moves; a keyed pp-for + FLIP stack should
    /// animate only that one.
    pub fn rotate(&mut self) {
        if self.items.len() < 2 {
            return;
        }
        let start = now_ms();
        let head = self.items.remove(0);
        self.items.push(head);
        self.time_reconcile(start, "rotate");
    }

    /// Full Fisher-Yates shuffle via `Math.random()` — every item
    /// moves. Stresses the keyed pp-for reconcile + the FLIP
    /// snapshot / play loop across the whole list at once.
    pub fn shuffle(&mut self) {
        if self.items.len() < 2 {
            return;
        }
        let start = now_ms();
        fisher_yates(&mut self.items);
        self.time_reconcile(start, "shuffle");
    }
}

impl StressDemo {
    /// Stop the timer inside a `tick::after_flush` callback so the
    /// reading covers the full reactive reconcile (trigger →
    /// pp-for walk → FLIP play), not just the Rust-side Vec
    /// mutation.
    fn time_reconcile(&mut self, start: f64, op: &'static str) {
        let handle = this::<Self>();
        pocopine::tick::after_flush(move || {
            let dur = now_ms() - start;
            handle.update(|s: &mut Self| {
                s.last_shuffle_ms = dur;
                s.shuffle_display = format!("{dur:.1} ms");
                s.last_op = op.into();
            });
        });
    }
}

/// In-place Fisher-Yates shuffle. Uses `Math.random()` for the
/// swap index — seedless, which is fine for a visual stress demo.
fn fisher_yates<T>(xs: &mut [T]) {
    let len = xs.len();
    if len < 2 {
        return;
    }
    for i in (1..len).rev() {
        let j = (js_sys::Math::random() * (i as f64 + 1.0)) as usize;
        let j = j.min(i);
        xs.swap(i, j);
    }
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}
