#![deny(warnings)]
use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "empty-allowed")]
struct State { value: u32, output: u32 }
#[handlers]
impl State {
    #[allow(deprecated)]
    #[watch(value)]
    fn empty(value: u32) -> Update<Self, ()> { let _ = value; Update::new() }
    #[allow(deprecated)]
    #[watch(value, updates())]
    fn empty_sugar(value: u32) -> Update<Self> { let _ = value; Update::new() }
    // No warning: a nonempty capability may return an empty patch at runtime.
    #[watch(value)]
    fn skip(value: u32) -> Update<Self, (Self::Output,)> { let _ = value; Update::new() }
    #[cfg(any())]
    #[watch(value)]
    fn inactive(value: u32) -> Update<Self, ()> { let _ = value; Update::new() }
}
fn main() {}
