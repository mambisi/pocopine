use pocopine::prelude::*;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "watch-mutation")]
struct EditorStore {
    count: i32,
}

#[handlers]
impl EditorStore {
    #[watch(count)]
    fn on_count(&self, next: i32, _prev: Option<i32>) {
        self.count = next + 1;
    }
}

fn main() {}
