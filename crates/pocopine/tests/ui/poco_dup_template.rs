//! RFC-116 — `template` is one key with two forms; giving both a path and a
//! `poco!` body is a duplicate, not a silent last-one-wins.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "poco-dup-template",
    template = "Somewhere.poco",
    template = poco! { <div>x</div> }
)]
struct PocoDupTemplate {
    count: i32,
}

#[handlers]
impl PocoDupTemplate {}

fn main() {}
