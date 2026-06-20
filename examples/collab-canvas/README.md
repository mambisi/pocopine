# collab-canvas

A tldraw-style collaborative canvas demo for `pocopine-collab`: two browser
sessions drag the same rectangles and converge in real time.

## What it shows

The realtime transport (`pocopine-realtime`) and the CRDT sync handshake
(`pocopine-collab`) are **generic** — this example reuses them untouched and
swaps in a canvas document schema. The point is the schema:

A canvas is a **map of objects**. Each rect's position is its own key in a `yrs`
Map, so:

- two people dragging _different_ rects never conflict;
- dragging the _same_ rect is a clean last-writer-wins on its position — none of
  the delete-then-insert content loss a sequence CRDT would suffer.

Contrast `pine-richtext-collab`, whose document is a sequence (`XmlFragment`) —
the right model for prose, the wrong one for a canvas. **Same transport, same
sync protocol, different schema.** That's the whole architecture in one diff.

A side effect of the fine-grained Map schema: there is no self-echo handling to
write. The gateway echoes your own publish back, but re-applying a `yrs` update
is a no-op and a Map observer only fires on a real change — so an echo paints
nothing. (The rich-text binder's coarse whole-doc re-encode needs explicit
change-detection; this doesn't.)

The canvas itself is rendered imperatively via `web_sys` — a freeform surface is
a custom renderer, not reactive document DOM.

## Run

Use the pocopine CLI — it builds the wasm client and launches the server bin
(the gateway + `CollabSync` + static files) together:

```sh
pocopine run --path examples/collab-canvas      # or: cd examples/collab-canvas && pocopine run
# → http://127.0.0.1:3030
```

Use `pocopine dev` instead for watch-and-rebuild while editing.

Open `/` for a single canvas, or **`/dual.html`** for the two-session
side-by-side view — two iframes, two independent sessions sharing one room. Drag
a rect in either pane and watch it converge in the other. (Two browser windows
on `/` work too.)

## Layout

| File | Role |
|------|------|
| `src/lib.rs` | The wasm client: the yrs Map schema, the collab handshake wiring, and the imperative canvas (pointer drag + render). |
| `src/bin/server.rs` | The dev server bin: mounts the gateway with a `CollabSync` handler and serves the static files. Reads `PORT` (set by `pocopine run`). |
| `index.html` | A single canvas session — the page `pocopine build` hash-rewrites. |
| `dual.html` | The two-session side-by-side view (two iframes of `/`). |

## How the sync flows

```
 drag in session A                                    session B
 ────────────────                                    ──────────
 pointermove
   └─ write positions["rect"] = {x,y}   (yrs Map)
        ├─ Map observer fires → repaint A's div
        └─ encode delta → Update ──► gateway ──► CollabSync ──► fan out
                                                                   │
                                          B: on Data → apply_update┘
                                               └─ Map observer fires → repaint B's div
```

A late joiner runs the SyncStep1/SyncStep2 handshake on connect and catches up
to the current positions before the first live update arrives.
