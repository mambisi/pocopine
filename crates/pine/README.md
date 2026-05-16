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

## Overlay Event Contract

Popover and DropdownMenu triggers isolate `pointerdown` and `click`
events by default, so embedding them in clickable cards, rows, or
tiles does not trigger the parent surface. Their content surfaces also
stop internal pointer/click bubbling; normal page-level bubbling still
occurs for true outside clicks.

Outside interactions are preventable before the primitive closes:

```html
<pine-popover-content @pp:pointer-down-outside.prevent="keep_open">
  ...
</pine-popover-content>
```

- `pp:pointer-down-outside` fires before outside pointerdown dismissal.
- `pp:interact-outside` fires before outside click/interact dismissal.
- Calling `preventDefault()` on either event keeps the overlay open.
- Clicking the trigger while open is exempt from outside-dismiss, so it
  toggles closed exactly once.

Application code may still need capture-phase or gesture-specific
guards for custom drag/select surfaces, but ordinary overlay
trigger/content click-through is owned by the Pine primitive.
