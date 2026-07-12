// The contract is literally `&mut self` — a shared receiver compiles
// as Rust but leaves the handler unable to store its result.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time)]
    fn on_start_time(&self, _next: String, _prev: Option<String>) {}
}

fn main() {}
