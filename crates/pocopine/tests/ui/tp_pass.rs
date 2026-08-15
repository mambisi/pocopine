//! Template-path validation — compile-pass: fields, `#[computed]`
//! fields, handler names (bare + call + assignment), `$`-magics,
//! `pp-for` loop locals, and `pp-let` slot idents all resolve.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "tp-pass",
    template = poco! {<div>
  <span pp-text="count"></span>
  <span pp-text="shout"></span>
  <button pp-on:click="reset">r</button>
  <button @click="count = count + 1">+</button>
  <button @click="pick(count)">pick</button>
  <span pp-text="$store.prefs.theme"></span>
  <template pp-for="item in items"><span><b pp-text="item"></b><i pp-text="$index"></i></span></template>
</div>}
)]
struct TpPass {
    count: i32,
    label: String,
    items: Vec<i32>,
}

#[handlers]
impl TpPass {
    #[computed]
    fn shout(label: &str) -> String {
        label.to_uppercase()
    }

    pub fn reset(&mut self) {
        self.count = 0;
    }

    pub fn pick(&mut self, n: i32) {
        self.count = n;
    }
}

fn main() {}
