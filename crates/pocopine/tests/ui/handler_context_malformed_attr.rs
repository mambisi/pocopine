use pocopine::prelude::*;

struct MalformedContext {}

#[handlers]
impl MalformedContext {
    fn broken(&mut self, #[context(from = "event")] _context: HandlerContext) {}
}

fn main() {}
