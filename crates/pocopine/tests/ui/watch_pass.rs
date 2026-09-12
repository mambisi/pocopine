use pocopine::prelude::*;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "editor")]
struct EditorStore {
    start_time: String,
    count: i32,
    error: Option<String>,
    valid: bool,
}

#[handlers]
impl EditorStore {
    #[watch(start_time, updates(error, valid))]
    fn on_start_time(start_time: Change<String>) -> Update<Self> {
        let valid = !start_time.current.is_empty();
        Update::new().valid(valid).error(None)
    }

    #[watch(count)]
    fn on_count(count: Change<i32>) {
        let _ = (count.current, count.previous);
    }
}

fn main() {}
