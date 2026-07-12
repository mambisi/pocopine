// The reported bug shape: a `#[watch]` handler that takes only `&mut self`
// used to compile green and silently never install — the watch never fired.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time)]
    fn on_start_time(&mut self) {}
}

fn main() {}
