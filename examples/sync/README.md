# Sync Example

Small app showing the first sync protocol slice:

- browser create actions push optimistic mutations into a process-local
  `MemorySyncStream<Post>`,
- `pocopine-sync` opens the stream, then serves snapshot and incremental pulls,
- `pocopine-live` sends only wake-up events,
- the browser stores rows in `CollectionState<Post>` and pulls with its cursor.

Run it with the CLI:

```bash
cargo run -p pocopine-cli -- dev --path examples/sync
```

Open the app in two browser tabs. Creating or resetting posts in one tab
wakes the other tab over SSE; the data itself is fetched from
`/__pocopine/sync/v1/pull`. Create uses
`/__pocopine/sync/v1/push`; reset remains a small server-function helper
that mutates the same memory stream.

This example uses memory backends, so it is intentionally single-process.
It registers its stream with `public_stream(...)` to avoid bundling an
auth setup into the demo; production apps should use `guarded_stream(...)`
or `guarded_stream_with(...)`.
The create form uses short client-local ids for readability; production
apps should use server-assigned ids or client-side UUIDs so multiple tabs
and devices do not collide.
Future database adapters should keep this same browser stream while
replacing the server-side source.

Checks:

```bash
cargo test -p sync-example
wasm-pack test --firefox --headless crates/pocopine-sync --test client_browser
```
