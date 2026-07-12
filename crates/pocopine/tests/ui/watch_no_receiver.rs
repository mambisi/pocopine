// A `#[watch]` handler with no `self` receiver can never be installed —
// the generated dispatch calls it as a method.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time)]
    fn on_start_time(_next: String, _prev: Option<String>) {}
}

fn main() {}
