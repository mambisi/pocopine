// A `#[watch]` handler missing `prev` used to slip past collection and
// fail later inside generated code with a confusing arity error.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time)]
    fn on_start_time(&mut self, _next: String) {}
}

fn main() {}
