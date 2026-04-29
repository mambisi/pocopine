# RFC 064 Phase 3 — template expression audit

Captured 2026-04-30 on `wip/rfc-064-phase-1ab`, immediately
after the RFC 064 Phase 1 cleanup and Phase 2 string-interning
start.

This audit is the required Phase 3 precondition before adding a
compiled expression ABI. It describes the expression shapes found
in checked-in `.poco` templates and inline `template_inline = r#"..."`
fixtures so the first compiled-expression envelope stays small and
measurable.

## Method

The scan covered:

- `crates/**/*.poco`
- `examples/**/*.poco`
- `jsbench/**/*.poco`
- Rust files under `crates/`, `examples/`, and `jsbench/` that
  contain raw `template_inline = r#"... "#` blocks.

The classifier extracted directive and interpolation expression
sites for:

- `pp-text`, `pp-html`, `pp-show`, `pp-if`, `pp-bind:*`, `:*`
- `pp-model`
- `@*`, `pp-on:*`
- `{{...}}` interpolation

The classifier is intentionally conservative. Listener expressions
are counted separately as out-of-envelope for Phase 3 because the
runtime already routes them through event handler dispatch, and
because call expressions need a separate ABI decision. Multiline
or unusual attribute formatting may need a parser-backed follow-up
before expanding the envelope.

## Shape Counts

Total expression sites found: 925.

| Shape | Count | Phase 3 envelope |
|---|---:|---|
| Bare identifier | 535 | In |
| Listener/function call | 163 | Out, listener path |
| Ternary | 99 | Out |
| Boolean combinations | 48 | In |
| Single-field access | 39 | In |
| Comparison with literal RHS | 19 | In |
| Unary `!` | 8 | In |
| Plus/string-concat expression | 5 | Out |
| Handler/method/function call | 4 | Out |
| Nested chain access | 4 | Out |
| Literal | 1 | In |

## First Compiled Envelope

Phase 3 should compile only the high-confidence forms:

- identifiers, including `$event` only where the host ABI explicitly
  provides an event value;
- single-field access such as `item.done`;
- literals;
- unary `!`;
- simple comparisons against literals;
- `&&` and `||` boolean combinations of the supported forms.

Everything else should continue through the runtime evaluator while
the compatibility feature remains enabled.

## Out-Of-Envelope Samples

Method/function calls:

```text
crates/pine/src/time_range_field/PineTimeRangeField.poco:11 :max-value=effective_start_max()
crates/pine/src/time_range_field/PineTimeRangeField.poco:21 :min-value=effective_end_min()
crates/pine/src/date_range_field/PineDateRangeField.poco:11 :max-value=effective_start_max()
crates/pine/src/date_range_field/PineDateRangeField.poco:20 :min-value=effective_end_min()
```

Nested chain access:

```text
examples/todo/src/TodoList.poco:6 pp-model=$store.preferences.theme
examples/todo/src/TodoList.poco:8 pp-text=$store.preferences.theme
examples/hn/src/components/comment/HnComment.poco:9 pp-if=comment.children.length
examples/spa/src/BlogPost.poco:5 pp-text=$route.params.id
```

String concatenation:

```text
crates/pine/src/context_menu/PineContextMenuContent.poco:6 :style='position:fixed;top:' + pointer_y + 'px;left:' + pointer_x + 'px;'
crates/pine/src/aspect_ratio/PineAspectRatio.poco:2 :style='aspect-ratio:' + ratio + ';'
crates/pine/src/splitter/PineSplitterPanel.poco:3 :style='flex: 0 0 ' + size + '%; overflow: hidden;'
examples/website/src/components/showcase/text/TextDemo.poco:45 pp-text=width + 'px'
examples/website/src/components/showcase/text/TextDemo.poco:47 :style='max-width:' + width + 'px'
```

Ternaries are common in Pine accessibility and style bindings:

```text
crates/pine/src/toggle_group/PineToggleGroupItem.poco:4 :aria-pressed=pressed ? 'true' : 'false'
crates/pine/src/checkbox/PineCheckbox.poco:3 :aria-checked=state == 'indeterminate' ? 'mixed' : (state == 'checked' ? 'true' : 'false')
crates/pine/src/slider/PineSliderRange.poco:4 :style=orientation == 'vertical' ? ('height:' + percent + '%;') : ('width:' + percent + '%;')
```

## ABI Implications

- The first ABI should be an optional compiled expression descriptor
  alongside existing `expr_src` fields, not a replacement for source
  strings.
- Generated code should emit compiled descriptors only when the
  parsed AST is in-envelope.
- The runtime evaluator remains the fallback for ternary, call,
  assignment, sequence, string-concat, nested-chain, and other
  out-of-envelope forms during RFC 064.
- Tests must include one fixture per in-envelope form and one
  explicit out-of-envelope fallback fixture.
