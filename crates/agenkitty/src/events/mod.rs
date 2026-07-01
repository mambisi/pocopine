mod human;
mod jsonl;

pub use agenkitty_core::{FrameworkEvent, FrameworkEventKind, RunStatus};
pub use human::render_human;
pub use jsonl::render_jsonl;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventFormat {
    Human,
    Jsonl,
}
