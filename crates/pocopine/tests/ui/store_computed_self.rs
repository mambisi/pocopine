// Issue #260 — compile-fail: `#[computed]` must declare its dependencies
// as parameters, never read through `self`. Store computed reuses this
// exact `#[handlers]` rejection, so the contract is pinned on a bare
// `#[handlers]` impl: that keeps the snapshot to our own `compile_error!`
// (toolchain-stable), whereas a `#[store]`/`#[component]` target would
// also cascade a rustc "HandlerDispatch not implemented" error whose
// framing drifts between the CI and workspace nightlies.
use pocopine::prelude::*;

struct Derived {
    items: Vec<String>,
}

#[handlers]
impl Derived {
    #[computed]
    fn bad(&self) -> usize {
        self.items.len()
    }
}

fn main() {}
