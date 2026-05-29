# Pine Stylekit

Pine Stylekit is Pocopine's native utility-CSS compiler. You write
Tailwind-*shaped* utility classes in `.poco` templates; Stylekit reads
your `@theme` tokens, extracts the classes through the real Pocopine
parser, and emits a deterministic static stylesheet at build time.
There is no browser-side style runtime.

It is **Tailwind-shaped, not Tailwind-compatible** (RFC 092 D3): the
class grammar is familiar, but the supported set is the [catalog
below](#utility-catalog) — that catalog *is* the contract. An unknown
class is a build error with a suggestion, not a silent miss.

> Status: experimental (RFC 092, Milestone 2). Ported examples:
> `examples/file-browser` and `examples/tailwind`. The built-in Tailwind
> colour palette and Preflight ship; Tailwind remains available as a
> fallback.

## Why not just Tailwind?

Tailwind scans `.poco` as opaque text and runs as an external watcher.
Stylekit instead:

- **Parses, doesn't scan.** A `class="…"` inside a comment or string
  never leaks; component bindings are understood, not guessed.
- **Fails loud.** A typo (`bg-surafce`), a type error (`w-[red]`), or an
  undefined token (`text-brand`) is a build error with a source span —
  never stale CSS silently served.
- **Is deterministic.** The same inputs always produce byte-identical
  output (classes are deduped and sorted before emission).
- **Owns its tokens.** Your `@theme` block is the single source of
  truth, emitted to `:root` so the stylesheet is self-contained.

## Enabling it

Add a Stylekit block to your project's `Cargo.toml`:

```toml
[package.metadata.pocopine.stylekit]
input = "app.css"          # CSS holding your @theme tokens
output = "pkg/stylekit.css" # generated stylesheet
src = "src"                 # directory scanned for .poco files
preflight = true            # prepend the base reset (set false to opt out)
```

The output is self-contained: a base reset (Preflight), your `@theme`
tokens emitted to `:root`, then the utilities. Set `preflight = false`
if your page brings its own reset.

Then link the output in `index.html`:

```html
<link rel="stylesheet" href="/pkg/stylekit.css" />
```

`pocopine build` / `run` / `dev` now compile CSS in-process — no
external watcher. Or opt in ad-hoc without a config block:

```sh
pocopine build --stylekit
pocopine dev --stylekit
```

`pocopine dev` recompiles on every source change and, on a compile
error, keeps the last good stylesheet while printing the diagnostic
(RFC 092 D6) — it never overwrites good CSS with broken output.

## Theme tokens

Tokens live in your input CSS as a CSS-first `@theme` block — the same
shape Tailwind v4 uses, so porting is mostly *deleting* the
`@import "tailwindcss"` and `@source` lines:

```css
@theme {
  --color-surface: #ffffff;
  --color-ink-100: #18171a;
  --color-accent: oklch(0.54 0.13 252);
  --spacing: 0.25rem;
  --shadow-card: 0 1px 2px rgba(20, 18, 28, 0.05);
}
```

Stylekit reads every `@theme { … }` block (later blocks override
earlier ones), makes the tokens available to utility validation, and
emits them to `:root` in the output. Colour utilities like `bg-surface`
resolve to `var(--color-surface)`; an undefined token is an error that
lists the defined ones.

## Recipes

**Button.**
```html
<button class="inline-flex items-center gap-2 px-3.5 py-2 rounded-md
               bg-accent text-surface font-medium
               hover:bg-accent-strong focus-visible:ring-2 focus-visible:ring-accent
               disabled:opacity-60 disabled:cursor-not-allowed transition-colors">
```

**Card.**
```html
<div class="rounded-lg border border-line bg-surface p-4 shadow-card">
```

**Data-driven state** (variants compile to attribute selectors):
```html
<span class="text-ink-60 data-[status=error]:text-danger
             data-[status=error]:bg-danger-soft">
```

**Arbitrary values** (typed — a length where a length is expected):
```html
<div class="grid grid-cols-[minmax(0,1fr)_80px] gap-2.5 text-[13px]">
<hr class="h-[3px] w-[calc(100%-2rem)]" />
```

**Colour with alpha** (`/NN` → `color-mix`):
```html
<header class="bg-surface/95 backdrop-blur-md">
```

**Static class map in a binding** — discoverable classes are extracted;
opaque construction is a diagnostic with a migration hint:
```html
<li :class="{ 'bg-accent-soft': active, 'opacity-50': muted }">
```

## Diagnostics

Errors render rustc-style with the source span:

```text
error: unknown utility `bg-surafce`
  --> src/sidebar.poco:14:22
   |
14 |     <aside class="bg-surafce p-4">
   |                   ^^^^^^^^^^ did you mean `bg-surface`?
```

| Case | Severity |
|------|----------|
| Unknown utility | error (with suggestion) |
| Wrong arbitrary value type (`w-[red]`) | error |
| Undefined token (`text-brand`) | error |
| Opaque dynamic class binding | error (migration hint) |
| Conflicting classes (`p-2 p-4`) | warning |

During a migration, `--stylekit-compat=warn` downgrades unknown-utility
errors to warnings so a half-ported page still builds.

## Editor support

`pocopine stylekit --metadata` prints machine-readable JSON (utility
families, value kinds, variants) for autocomplete; pair it with
`ThemeTokens::to_manifest_json` for token-aware colour completion. The
human catalog below is regenerated with `pocopine stylekit --docs`.

## Migrating from Tailwind

1. Keep your `@theme` tokens; remove `@import "tailwindcss"` and
   `@source`.
2. Add the `[package.metadata.pocopine.stylekit]` block.
3. Run `pocopine build --stylekit` and fix the diagnostics — anything
   outside the catalog (unsupported families) is flagged. Tailwind's
   default palette (`slate-*`, `rose-*`, …) is built in, so those keep
   working; a `@theme` token of the same name overrides it.
4. Point `index.html` at `pkg/stylekit.css`. Keep the Tailwind block to
   A/B compare during the experiment.

## Utility catalog

> Generated from the registry by `pocopine stylekit --docs`. This is the
> supported set (RFC 092 D3). Each family lists its value kind, whether
> it accepts arbitrary `[…]` values, and a representative example.

<!-- BEGIN GENERATED CATALOG -->
### Display & position

| Utility | Value | Arbitrary | Example | Notes |
|---------|-------|-----------|---------|-------|
| `flex` | None | — | `flex` | also block, inline, inline-block, inline-flex, grid, hidden |
| `relative` | None | — | `relative` | also absolute, fixed, sticky, static |

### Flexbox & alignment

| Utility | Value | Arbitrary | Example | Notes |
|---------|-------|-----------|---------|-------|
| `flex-col` | None | — | `flex-col` | direction: flex-row/col; wrap: flex-wrap/nowrap; flex-1/none/auto |
| `items-center` | None | — | `items-center` | items-/justify- center\|start\|end\|between\|around\|evenly |
| `shrink` | None | — | `shrink-0` | shrink, shrink-0, grow, grow-0 |

### Spacing

| Utility | Value | Arbitrary | Example | Notes |
|---------|-------|-----------|---------|-------|
| `p` | Spacing | — | `p-4` | p/px/py/pt/pb/pl/pr |
| `m` | Spacing | — | `mx-auto` | m/mx/my/mt/mb/ml/mr (auto allowed) |
| `gap` | Spacing | — | `gap-2.5` | flex/grid gap |
| `space-y` | Spacing | — | `space-y-2` | space-y/space-x — margin between children |

### Sizing

| Utility | Value | Arbitrary | Example | Notes |
|---------|-------|-----------|---------|-------|
| `w` | Size | yes | `w-full` | w, h, size; px/full/screen/named |
| `min-w` | Size | yes | `min-w-0` | min-w/min-h/max-w/max-h; max-w-{xs..3xl} |

### Typography

| Utility | Value | Arbitrary | Example | Notes |
|---------|-------|-----------|---------|-------|
| `text` | Scale | yes | `text-[13px]` | named size (text-sm) OR color OR text-center/right |
| `font` | Scale | — | `font-medium` | weight (medium/semibold/…) + family (sans/serif/mono) |
| `leading` | Scale | yes | `leading-relaxed` | tight/snug/normal/relaxed/loose |
| `tracking` | Scale | yes | `tracking-tight` | tighter…widest |
| `underline` | None | — | `underline` | + underline-offset-{n}, uppercase, truncate, tabular-nums |

### Color (token-backed)

| Utility | Value | Arbitrary | Example | Notes |
|---------|-------|-----------|---------|-------|
| `bg` | Color | yes | `bg-surface` | any --color-* token, Tailwind palette (bg-slate-700), transparent/white/black/current |
| `text` | Color | — | `text-ink-50` | color when not a size/align keyword |
| `border` | Color | — | `border-line` | color when not a width/style keyword |
| `ring` | Color | — | `ring-accent` | ring-{n} width, ring-{color} colour |
| `decoration` | Color | — | `decoration-1` | thickness (number) or colour |

### Borders, radius, shadow

| Utility | Value | Arbitrary | Example | Notes |
|---------|-------|-----------|---------|-------|
| `border` | None | — | `border-b` | border, border-0/2/4, sides t/r/b/l, styles solid/dashed/… |
| `rounded` | Scale | yes | `rounded-md` | none/sm/md/lg/xl/2xl/3xl/full |
| `shadow` | Scale | yes | `shadow-card` | sm/md/lg/xl/2xl + any --shadow-* token |

### Positioning & effects

| Utility | Value | Arbitrary | Example | Notes |
|---------|-------|-----------|---------|-------|
| `top` | Spacing | yes | `top-0` | top/right/bottom/left, inset, inset-x/y |
| `z` | Number | — | `z-10` | z-index |
| `opacity` | Number | — | `opacity-40` | 0–100 → 0–1 |
| `duration` | Number | — | `duration-200` | ms |
| `scale` | Number | — | `scale-95` | transform scale |
| `backdrop-blur` | Scale | — | `backdrop-blur-md` | none/sm/md/lg/xl/2xl/3xl |
| `transition` | None | yes | `transition-colors` | transition, transition-colors, transition-[props] |

### Variants

| Prefix | Compiles to |
|--------|-------------|
| `hover:` | `:hover` |
| `focus:` | `:focus` |
| `focus-visible:` | `:focus-visible` |
| `focus-within:` | `:focus-within` |
| `active:` | `:active` |
| `disabled:` | `:disabled` |
| `checked:` | `:checked` |
| `first / last:` | `:first-child / :last-child` |
| `placeholder:` | `::placeholder` |
| `sm / md / lg / xl / 2xl:` | `@media (min-width: …)` |
| `data-[k=v]:` | `[data-k="v"]` |
| `aria-[k=v]:` | `[aria-k="v"]` |
<!-- END GENERATED CATALOG -->
