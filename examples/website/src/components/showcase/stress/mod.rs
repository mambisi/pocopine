use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

/// How many items get added per simulated-load batch, and how much
/// real time to wait between batches. Tuned so a 500-item fixture
/// loads in roughly one second with enough space between frames to
/// exercise the incremental-insert path of keyed pp-for.
const SIM_BATCH_SIZE: usize = 10;
const SIM_BATCH_DELAY_MS: i32 = 20;
const SIM_TOTAL_ITEMS: usize = 500;

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
    /// True while a `simulate_add` loop is streaming items in.
    /// The loop checks this between batches so toggling Unmount
    /// cancels the stream immediately instead of racing to the
    /// target count.
    pub simulating: bool,
}

#[handlers]
impl StressDemo {
    /// One-shot mount — dumps all 500 items at once.
    pub fn mount_fixture(&mut self) {
        if !self.open {
            self.items = (0..SIM_TOTAL_ITEMS)
                .map(|i| format!("item-{i:03}"))
                .collect();
            self.open = true;
        } else {
            self.items.clear();
            self.open = false;
            self.simulating = false;
        }
    }

    /// Streamed mount — add `SIM_BATCH_SIZE` items per
    /// `SIM_BATCH_DELAY_MS` until we hit the target count. Models a
    /// realistic "items arriving over time" scenario (paginated
    /// fetch, websocket feed, etc.) and stresses the incremental
    /// keyed-insert path — each batch adds new keys while existing
    /// clones stay put.
    pub fn simulate_add(&mut self) {
        if self.open {
            self.items.clear();
            self.open = false;
            self.simulating = false;
            return;
        }
        self.items.clear();
        self.open = true;
        self.simulating = true;
        // Grab the handle here (while we're inside a handler and
        // `CURRENT_SCOPE_ID` is bound); `schedule_sim_batch` then
        // keeps that handle alive through its own recursive
        // re-schedule so the setTimeout callback doesn't need to
        // re-enter scope context to find us.
        Self::schedule_sim_batch(this::<Self>());
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

    /// Advance one sim batch + reschedule the next one via
    /// `setTimeout` until we hit the target count or `simulating`
    /// flips false. The caller passes its own `Handle<Self>` so the
    /// recursive re-schedule doesn't have to re-enter handler
    /// context (setTimeout callbacks run with no `CURRENT_SCOPE_ID`
    /// bound — calling `this::<Self>()` from inside one panics,
    /// which silently killed the prior sim after the first batch).
    /// Self-terminating: no timer handle to clear on unmount, the
    /// next scheduled callback just bails on its flags.
    fn schedule_sim_batch(handle: Handle<Self>) {
        let Some(window) = web_sys::window() else { return };
        let handle_for_cb = handle.clone();
        let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
            let keep_going = handle_for_cb.update(|s: &mut Self| {
                if !s.simulating || !s.open {
                    s.simulating = false;
                    return false;
                }
                let current = s.items.len();
                let target = (current + SIM_BATCH_SIZE).min(SIM_TOTAL_ITEMS);
                for i in current..target {
                    s.items.push(format!("item-{i:03}"));
                }
                if target >= SIM_TOTAL_ITEMS {
                    s.simulating = false;
                    false
                } else {
                    true
                }
            });
            if keep_going {
                Self::schedule_sim_batch(handle_for_cb);
            }
        });
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.unchecked_ref(),
            SIM_BATCH_DELAY_MS,
        );
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
