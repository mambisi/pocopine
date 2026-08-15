//! RFC-116 — an empty `poco!` body is a hard error rather than an empty
//! template that would fail mysteriously at mount time.
use pocopine::poco;

fn main() {
    let _ = poco! {};
}
