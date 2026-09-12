use pocopine::prelude::*;

struct Editor {
    start_time: String,
    end_time: String,
}

#[handlers]
impl Editor {
    #[watch(start_time, end_time)]
    fn on_when_changed(&mut self) {}
}

fn main() {}
