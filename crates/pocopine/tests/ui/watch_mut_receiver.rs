// Mutable watchers must direct authors to computed values or actions.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time)]
    fn on_start_time(&mut self, _next: String, _prev: Option<String>) {}
}

fn main() {}
