// A misspelled directive head compiled green before the registry
// check — the attribute was preserved on the cleaned HTML, which the
// retired runtime dispatch never reads: dead markup.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-unknown-directive",
    template = "plan_unknown_directive.poco"
)]
struct PlanUnknownDirective {
    is_open: bool,
}

#[handlers]
impl PlanUnknownDirective {}

fn main() {}
