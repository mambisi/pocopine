// A misspelled listener modifier used to install as a keyboard key
// filter — on a click event the filter can never match, so the
// handler silently never fired.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "plan-dead-listener-modifier",
    template = "plan_dead_listener_modifier.poco"
)]
struct PlanDeadListenerModifier {
    count: u32,
}

#[handlers]
impl PlanDeadListenerModifier {}

fn main() {}
