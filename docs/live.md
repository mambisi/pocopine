# Live Invalidation

`pocopine-live` streams database-agnostic invalidation events to the
browser. It is not the future offline-sync database and it is not the
collab CRDT layer. The current contract is deliberately smaller:

- the server publishes typed collection/query invalidations,
- the browser subscribes with SSE,
- component code decides how to refetch its data.

Enable the umbrella re-export with `pocopine = { features = ["live"] }`.
Use `live-redis` instead of `live` when the application needs the Redis
backend.

A runnable example lives in [`examples/live`](../examples/live/).

## Browser Shape

Use `LiveRefresh::scoped()` from a component lifecycle method or handler.
The stream is closed automatically when the component unmounts.

```rust
use pocopine::prelude::*;

#[handlers]
impl PostList {
    pub fn on_mount(&mut self) {
        self.reload();

        let handle = this::<Self>();
        let refresh = {
            let handle = handle.clone();
            move |_event: pocopine::live::LiveRefreshEvent| {
                handle.update(|s| s.reload());
            }
        };

        if let Err(err) = pocopine::live::LiveRefresh::scoped()
            .collection("posts", refresh)
            .on_gap(move |_event| {
                // Optional: record a cursor-gap warning in component state.
            })
            .open()
        {
            self.error = err.to_string();
        }
    }

    fn reload(&mut self) {
        self.loading = true;
        dispatch!(list_posts().await, |s, result| {
            s.loading = false;
            match result {
                Ok(posts) => {
                    s.posts = posts;
                    s.error.clear();
                }
                Err(err) => s.error = err.to_string(),
            }
        },);
    }
}
```

Query tags are for refetching a logical query rather than every view of a
collection:

```rust
pocopine::live::LiveRefresh::scoped()
    .query_tag("posts:list", move |_event| {
        handle.update(|s| s.reload());
    })
    .open()?;
```

On a replay gap, pocopine invokes every registered collection/query refresh
callback so the component refetches from scratch instead of continuing from
a stale cursor.

## Server Shape

Mount a live hub and explicitly allow the public topics the browser may
request. The default policy is deny-all.

```rust
use pocopine_events::{EventBackend, MemoryEventBackend};
use pocopine_live::{collection_topic, routes, LiveHub, LiveInvalidation};

let backend = MemoryEventBackend::new();
let hub = LiveHub::new(backend.clone())
    .allow_topics([collection_topic("posts")?]);

let app = axum::Router::new().merge(routes(hub));

backend
    .publish(
        LiveInvalidation::upsert("posts")
            .keys(["post_1"])
            .query_tags(["posts:list"])
            .into_draft()?,
    )
    .await?;
```

For multi-process deployments, use the Redis backend. The browser protocol
and component code stay the same; only the event backend changes.

## Failure Model

- SSE is at-least-once. Refresh handlers must tolerate duplicate events.
- `gap` means retained replay history was not enough. Refetch registered
  data from scratch.
- `error` is delivered to `on_error` callbacks and does not automatically
  refetch data.
- Audience fields are descriptive metadata today. Authorization lives in
  the server topic policy.
