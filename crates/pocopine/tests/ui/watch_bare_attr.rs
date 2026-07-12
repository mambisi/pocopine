// Bare `#[watch]` (no field name) used to strip the attribute and
// silently downgrade the method to an ordinary handler.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch]
    fn on_start_time(&mut self, _next: String, _prev: Option<String>) {}
}

fn main() {}
