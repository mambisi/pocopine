// A multi-field watch has no single (next, prev) — value args on the
// handler are a compile error pointing at the payload-less contract.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
    end_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time, end_time)]
    fn on_times(&mut self, _next: String, _prev: Option<String>) {}
}

fn main() {}
