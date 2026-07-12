// `#[watch("field")]` — a string literal instead of a field ident —
// used to fail the ident parse and silently drop the watch.
use pocopine::prelude::*;

struct Editor {
    start_time: String,
}

#[handlers]
impl Editor {
    #[watch("start_time")]
    fn on_start_time(&mut self, _next: String, _prev: Option<String>) {}
}

fn main() {}
