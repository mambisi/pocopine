// A lifecycle-named method without `self` used to be skipped before
// name matching — `on_ready` compiled green and silently never fired.
use pocopine::prelude::*;

struct Clock {
    ticks: u32,
}

#[handlers]
impl Clock {
    fn on_ready() {}
}

fn main() {}
