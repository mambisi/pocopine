// `#[computed(...)]` has no argument surface; extra tokens used to be
// recognized, never parsed, and silently stripped.
use pocopine::prelude::*;

struct Derived {
    items: Vec<String>,
}

#[handlers]
impl Derived {
    #[computed(cached)]
    fn total(items: &Vec<String>) -> usize {
        items.len()
    }
}

fn main() {}
