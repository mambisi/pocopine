#![deny(warnings)]
use pocopine::{Changes, prelude::*};

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Cache;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "all")]
pub struct State {
    value: String,
    r#type: bool,
    #[serde(skip)]
    cache: Cache,
}

#[handlers]
impl State {
    #[watch]
    pub fn observe(changes: Changes<Self>) {
        let _: String = changes.value.current;
        let _: Option<bool> = changes.r#type.previous;
    }
    #[computed]
    fn size(value: &str) -> usize { value.len() }
    fn use_cache(&self) { let _ = &self.cache; }
}

// Handler placement does not depend on the component declaration order.
#[handlers]
impl Empty {
    #[watch]
    fn observe(changes: Changes<Self>) { let _ = changes; }
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "empty")]
struct Empty {}
fn main() {}
