// A duplicated field in one #[watch] list is a typo, not a request
// to subscribe twice.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time, start_time)]
    fn on_times(&mut self) {}
}

fn main() {}
