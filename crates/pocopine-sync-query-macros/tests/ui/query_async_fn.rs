//! `#[query]` selectors must be synchronous — compute runs on the
//! reactive rerun path, which is not an async context.
use pocopine_sync_query::QueryClient;
use pocopine_sync_query_macros::query;

#[query]
async fn bad_async(_client: QueryClient, ws: String) -> u32 {
    let _ = ws;
    0
}

fn main() {}
