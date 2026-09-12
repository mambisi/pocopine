#![deny(warnings)]
use pocopine::prelude::*;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Payload { value: u32 }

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "forms")]
struct State { payload: Payload, name: String, count: u32, output: u32 }

#[handlers]
impl State {
    #[watch(payload, name, count, updates(output))]
    fn mixed(payload: &Payload, name: String, count: Change<u32>) -> Update<Self> {
        Update::new().output(payload.value + name.len() as u32 + count.current)
    }
    #[watch(name)]
    fn borrowed_string(name: &str) { let _ = name; }
    #[watch(count)]
    fn owned_scalar(count: u32) { let _ = count; }
    // An inactive all-fields observer must not require Payload: Clone.
    #[cfg(any())]
    #[watch]
    fn inactive(changes: Changes<Self>) { let _ = changes; }
}
fn main() {}
