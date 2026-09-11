// Named input parameters replace the legacy next/previous argument pair.
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
