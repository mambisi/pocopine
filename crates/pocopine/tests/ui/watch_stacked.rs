// Two `#[watch]` attributes on one method: the first used to be
// silently discarded (last-one-wins). One handler per watched field.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
    end_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time)]
    #[watch(end_time)]
    fn on_times(&mut self, _next: String, _prev: Option<String>) {}
}

fn main() {}
