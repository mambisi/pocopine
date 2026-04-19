# pine

Unstyled, accessible UI primitives for [pocopine](../pocopine).
Pine ships **behavior, keyboard, focus, and ARIA**. It ships
**zero CSS** — authors style components via their own stylesheets.

## Components (MVP)

| Tag | Role | Highlights |
|---|---|---|
| `<pine-button>` | Button | `pp-as` polymorphism, `variant` / `size` via `data-*` |
| `<pine-dialog>` | Modal dialog | Focus trap + scroll lock + teleport + `$id` wiring |
| `<pine-popover>` | Floating panel | `pp-anchor` positioning, click-outside to dismiss |
| `<pine-dropdown-menu>` | Menu overlay | Popover + `pp-roving` + menu ARIA |
| `<pine-tabs>` | Tablist | `pp-roving.horizontal`, emits `pp:update:model` |
| `<pine-tooltip>` | Tooltip | Hover / focus with delay, anchored |
| `<pine-switch>` | Toggle | `role="switch"`, `pp-model` compatible |
| `<pine-checkbox>` | Tri-state checkbox | `aria-checked="mixed"` on indeterminate |

## Usage

```rust
use pocopine::prelude::*;

fn main() {
    pine::register_all();
    App::new().register::<MyApp>().run();
}
```

See `examples/pine-demo` for each component in use.
