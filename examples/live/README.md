# Live Example

Small app showing the current live invalidation flow:

- server functions mutate a process-local `posts` collection,
- the host publishes `LiveInvalidation` events through a memory backend,
- the browser uses `LiveQuery::scoped(...)` with `QueryState<Vec<Post>>`,
- matching query-tag events refetch the list.

Run it with the CLI:

```bash
cargo run -p pocopine-cli -- dev --path examples/live
```

Open the app in two browser tabs. Publishing or resetting posts in one tab
refreshes the other through the SSE stream.

This example uses `MemoryEventBackend`, so it is intentionally
single-process. Switch the hub construction to the Redis backend when the
app needs multiple server processes.

The tutorial for wiring this pattern into an app lives in
[`docs/live.md`](../../docs/live.md). Keep that tutorial, this example,
and `tests/live_refresh.rs` updated together when the live API changes.
