use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "test")]
struct State { a: u32, b: u32, c: u32 }
#[handlers]
impl State {
#[watch(a, writes(b))]
fn bad(a: Change<u32>) -> Update<Self> { Update::new().b("wrong") }
}
fn main() {}
