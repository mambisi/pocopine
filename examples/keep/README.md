# Keep Example

Google Keep-style app showing Pocopine sync + the durable
SQLite/OPFS local store in a real application shape.

It demonstrates:

- **durable browser local storage** through `pocopine-sync-sqlite`
  (OPFS-backed SQLite); cached rows and pending mutations survive
  page reloads,
- a `KeepStore` `#[store]` that owns every durable UI field, paired
  with small components (`KeepBoard`, `KeepComposer`, `KeepNoteForm`,
  `KeepNoteBody`, `KeepNoteCard`, `KeepEditor`) and `pine-icons` for
  every glyph,
- optimistic sync pushes with live wake-up refreshes,
- a mixed note/checklist payload on one sync stream plus a separate
  synced tag stream for queryable labels. The sidebar registry is
  rebuilt from the tag stream and any labels already attached to
  hydrated notes, then missing labels are backfilled into the tag
  stream so refreshes do not lose the navigation list,
- a small memory-backed server stream so the example runs locally
  without database setup.

Run it with the CLI:

```bash
cargo run -p pocopine-cli -- dev --path examples/keep
```

Open the app in two browser tabs. Creating, pinning, archiving, or
checking todo items in one tab wakes the other tab over SSE; the
data itself is fetched from `__pocopine/sync/v1/pull`. Browser rows
and pending mutations survive reloads through the SQLite OPFS local
store.

The server sets `Cross-Origin-Opener-Policy` and
`Cross-Origin-Embedder-Policy` headers because SQLite's OPFS VFS
needs a cross-origin-isolated browser context. If those headers are
removed, the app still opens and network sync still works, but
durable local SQLite hydration will fail with `no such vfs: opfs`.

The server side uses a small rusqlite-backed stream source that stores
rows, row versions, incremental changes, and accepted mutation ids in
SQLite files under
`${TMPDIR}/pocopine_keep_notes.sqlite3` and
`${TMPDIR}/pocopine_keep_tags.sqlite3`. Restarting the bin rehydrates
from those databases, so notes and user-created tags survive
`cargo run` cycles and clients can keep using incremental cursors
across server restarts. Set `POCOPINE_KEEP_NOTES_DB_PATH` and
`POCOPINE_KEEP_TAGS_DB_PATH` when you want to isolate a dev or test
run from those default databases.

The development guide in [`DEVELOPMENT.md`](./DEVELOPMENT.md) explains
the app-building process behind this example: state ownership,
component boundaries, shared form extraction, directive pitfalls, CSS
splitting, and browser verification.

Checks:

```bash
cargo test -p keep-example
cargo check -p keep-example --target wasm32-unknown-unknown
```
