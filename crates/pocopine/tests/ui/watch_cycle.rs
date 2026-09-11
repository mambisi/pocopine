use pocopine::prelude::*;
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "test")]
struct State { a: u32, b: u32, c: u32 }
#[handlers]
impl State {
#[watch(a, writes(b))]
fn first(a: FieldUpdate<u32>) -> Update<Self> { Update::new().b(a.current) }
#[watch(b, writes(c))]
fn second(b: FieldUpdate<u32>) -> Update<Self> { Update::new().c(b.current) }
#[watch(c, writes(a))]
fn third(c: FieldUpdate<u32>) -> Update<Self> { Update::new().a(c.current) }
}
fn main() {}
