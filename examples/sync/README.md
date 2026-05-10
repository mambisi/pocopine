# Sync Example

Small app showing the first sync protocol slice:

- server functions mutate a process-local `MemorySyncShape<Post>`,
- `pocopine-sync` serves snapshot and incremental pulls,
- `pocopine-live` sends only wake-up events,
- the browser stores rows in `CollectionState<Post>` and pulls with its cursor.

Run it with the CLI:

```bash
cargo run -p pocopine-cli -- dev --path examples/sync
```

Open the app in two browser tabs. Creating or resetting posts in one tab
wakes the other tab over SSE; the data itself is fetched from
`/__pocopine/sync/v1/pull`.

This example uses memory backends, so it is intentionally single-process.
Future database adapters should keep this same browser shape while
replacing the server-side source.
