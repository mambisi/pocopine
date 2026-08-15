//! RFC-116 — a standalone `poco!` may be a fragment, but a component
//! template may not: the single-root rule still applies to the inline form.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "poco-multi-root", template = poco! {
    <div>a</div>
    <div>b</div>
})]
struct PocoMultiRoot {
    count: i32,
}

#[handlers]
impl PocoMultiRoot {}

fn main() {}
