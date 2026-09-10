// The canonical `#[watch]` shape: `&self` + `(next: V, prev: Option<V>)`.
// Hosted on a `#[store]` so the generated observe machinery exists.
use pocopine::prelude::*;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "editor")]
struct EditorStore {
    start_time: String,
    count: i32,
}

#[handlers]
impl EditorStore {
    #[watch(start_time)]
    fn on_start_time(&self, _next: String, _prev: Option<String>) {
        let _ = self.count;
    }

    #[watch(count)]
    fn on_count(&self, next: i32, prev: Option<i32>) {
        let _ = (next, prev);
    }
}

fn main() {}
