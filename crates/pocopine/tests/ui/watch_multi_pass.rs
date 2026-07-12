// RFC-115 — the multi-field form: two-plus fields require the
// payload-less `&mut self` shape; the handler is invoked once per
// flush when any listed field changes.
use pocopine::prelude::*;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "editor-multi")]
struct EditorStore {
    start_time: String,
    end_time: String,
    count: i32,
}

#[handlers]
impl EditorStore {
    #[watch(start_time, end_time)]
    fn on_when_changed(&mut self) {
        self.count += 1;
    }

    // The typed single-field contract coexists unchanged.
    #[watch(count)]
    fn on_count(&mut self, next: i32, prev: Option<i32>) {
        let _ = (next, prev);
    }
}

fn main() {}
