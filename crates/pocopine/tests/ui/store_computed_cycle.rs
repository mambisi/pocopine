// Issue #260 — compile-fail: computed-to-computed dependencies form a
// static graph; a cycle is rejected at compile time. Store computed
// reuses this exact `#[handlers]` topological check, so the contract is
// pinned on a bare `#[handlers]` impl to keep the snapshot to our own
// `compile_error!` (see store_computed_self.rs for why).
use pocopine::prelude::*;

struct Derived {
    seed: usize,
}

#[handlers]
impl Derived {
    #[computed]
    fn a(b: usize) -> usize {
        b
    }

    #[computed]
    fn b(a: usize) -> usize {
        a
    }
}

fn main() {}
