// `#[server(idempotent)]` on a streaming function was parsed, stored,
// and never consulted — the replay-safety intent was silently dead.
use pocopine::StreamServerResult;

#[pocopine::server(public, idempotent)]
async fn feed() -> StreamServerResult<u32> {
    todo!()
}

fn main() {}
