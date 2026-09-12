use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "test")]
struct State { a: u32, b: u32, c: u32 }
#[handlers]
impl State {
#[watch(a, updates(b))]
fn first(a: Change<u32>) -> Update<Self> { Update::new().b(a.current) }
#[watch(b, updates(c))]
fn second(b: Change<u32>) -> Update<Self> { Update::new().c(b.current) }
#[watch(c, updates(a))]
fn third(c: Change<u32>) -> Update<Self> { Update::new().a(c.current) }
}
fn main() {}
