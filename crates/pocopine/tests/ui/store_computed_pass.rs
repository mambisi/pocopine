// Issue #260 — compile-pass: a `#[store]` may declare `#[computed]`
// synthetic fields in its `#[handlers]` impl, including a computed that
// depends on another computed.
use pocopine::prelude::*;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "demo")]
struct DemoStore {
    items: Vec<String>,
    filter: String,
}

#[handlers]
impl DemoStore {
    // raw-field computed
    #[computed]
    fn filtered(items: &[String], filter: &str) -> Vec<String> {
        items
            .iter()
            .filter(|i| i.contains(filter))
            .cloned()
            .collect()
    }

    // computed-on-computed
    #[computed]
    fn filtered_len(filtered: Vec<String>) -> usize {
        filtered.len()
    }
}

fn main() {}
