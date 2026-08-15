//! RFC-116 — the happy path: bare HTML tokens, poco sugar, quoted prose,
//! and interpolation all compile through the inline form.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

// Standalone, in const position, as a fragment.
const ROWS: PocoTemplate = poco! {
    <li>a</li>
    <li>b</li>
};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "poco-pass", template = poco! {
    <div class="card" :title="label">
        <span pp-text="label" pp-show="ready"></span>
        <button pp-on:click.debounce.300="bump">{{ count }}</button>
        <p>"Don't stop — © 2026 · ⌘K"</p>
        <input type="text" pp-model.number="count" />
        <template pp-if="ready">
            <em>ready</em>
        </template>
    </div>
})]
struct PocoPass {
    #[prop]
    label: String,
    #[prop]
    count: i32,
    #[prop]
    ready: bool,
}

#[handlers]
impl PocoPass {
    fn bump(&mut self) {
        self.count += 1;
    }
}

fn main() {
    assert!(ROWS.as_str().contains("<li>a</li>"));
}
