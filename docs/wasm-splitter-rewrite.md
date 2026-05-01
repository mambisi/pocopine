# Wasm splitter rewrite

Pocopine's route splitter must be a small wasm slicer, not a
relocation-only byte patcher.

## Why the prototype was removed

The prototype splitter used relocation records to discover
dependencies and to patch copied function bodies. That works only
if every index-bearing instruction in executable code has a matching
relocation record.

That assumption is false. `wasm-ld` can synthesize wrapper functions
after object files are compiled. Those generated functions can
contain instructions such as `call` without a `reloc.CODE` entry.
If a splitter raw-copies the body, the old input-module function
index survives in a smaller output module and can become invalid.

## The new invariant

The rewrite starts with this rule:

```text
Every emitted index is either remapped from structured wasm
instructions or the build fails.
```

Relocations are still useful for data-symbol layout and diagnostics,
but they are not the source of truth for executable code.

## Pipeline

The intended pipeline is:

```text
input wasm
  -> parse module sections
  -> parse every function body into index uses
  -> build a typed dependency graph
  -> assign graph nodes to main, route, and shared chunks
  -> build per-output remap tables
  -> re-encode function bodies from parsed operators
  -> validate every emitted wasm module
  -> emit JS loader and route manifest
```

The first crate is `pocopine-wasm-split`. Its initial surface only
does analysis. It records dependencies from instructions like:

```text
call f                 -> Function(f)
ref.func f             -> Function(f)
call_indirect t, table -> Type(t), Table(table)
global.get g           -> Global(g)
table.init e, table    -> Element(e), Table(table)
memory.init d, mem     -> Data(d), Memory(mem)
throw tag              -> Tag(tag)
```

This is deliberately lower-level than route manifests. The route
layer will later provide split roots; the wasm layer must prove the
binary can be sliced safely.

## Contributor mental model

A wasm module has index spaces:

```text
functions
types
tables
memories
globals
tags
data segments
element segments
```

Splitting creates new modules, so every index space changes. A
function that was `func[928495]` in the input may become `func[12]`
in a route chunk, or it may become an import from a shared chunk.

The splitter's job is to preserve meaning while changing those
index spaces. That means every instruction that carries an index
must pass through a remap table.

## Failure policy

Strict release splitting should fail closed:

```text
error: split build cannot prove wasm index remapping is safe
```

The framework should not emit a route chunk if validation is
incomplete. A larger monolithic wasm is acceptable; a subtly invalid
split wasm is not.
