// Bare observers require a Changes<Self> argument, never a receiver.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch]
    fn on_start_time(&self, _next: String, _prev: Option<String>) {}
}

fn main() {}
