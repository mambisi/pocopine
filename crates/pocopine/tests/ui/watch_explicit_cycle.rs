use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "invalid")]
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch(value)]
fn first(value: u32) -> Update<Self, (Self::Output,)> { Update::new().output(value) }
#[watch(output, updates(value))]
fn second(output: u32) -> Update<Self> { Update::new().value(output) }
}
fn main() {}
