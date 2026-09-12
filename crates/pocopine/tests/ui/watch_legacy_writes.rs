use pocopine::prelude::*;
struct State { value: u32, output: u32 }
#[handlers]
impl State {
#[watch(value, writes(output))]
fn legacy(value: u32) -> Update<Self> { Update::new().output(value) }
}
fn main() {}
