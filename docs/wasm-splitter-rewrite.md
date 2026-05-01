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

## Current foundation

`pocopine-wasm-split` now records the module's index-space sizes:

```text
functions, types, tables, memories, globals, tags, data segments,
element segments
```

`ModuleAnalysis::validate_indices()` is the first fail-closed gate. It
checks that every recorded instruction dependency, every exported item,
and every function signature type index points inside the input
module's index spaces before later passes try to split the graph.

For example, a module with one function and a body containing
`call 99` fails as `FunctionIndexOutOfBounds` instead of being copied
into a route chunk with a stale index.

After validation, `ModuleAnalysis::dependency_closure()` can walk from
typed roots such as `Function(42)` to every directly and transitively
required dependency. Function roots pull in their signature type and
their recorded instruction dependencies, so a route root can become a
concrete set like:

```text
Function(route_entry)
Function(render_helper)
Type(component_signature)
Global(component_state)
Table(function_table)
```

This is still analysis-only. The next passes must decide which
closures belong to shell, route, or shared chunks, then build remap
tables for each output module.

`ModuleAnalysis::plan_route_split()` performs that first deterministic
classification:

```text
explicit shell roots        -> shell
used by every route         -> shell
used by exactly one route   -> that route chunk
used by some routes         -> shared chunk for that route subset
```

It still returns dependency sets, not wasm files. That boundary is
intentional: planning should be testable before the emitter starts
rewriting functions, imports, exports, tables, memories, and data.

`SplitPlan::build_remaps()` turns each planned dependency set into
compact per-index-space remap tables:

```text
old Function(928495) -> new function 12
old Type(41)         -> new type 3
old Global(8)        -> new global 0
```

Those tables are the emitter contract. A future body rewriter must
look up every recorded index-bearing operator in the remap for the
output it is generating, or fail the build. The current remap layer
does not yet decide cross-chunk imports; it only proves that owned
dependencies can be assigned deterministic new indices.

`ModuleAnalysis::build_link_plan()` then separates each output's
owned dependencies from external references. External references
include original wasm imports and dependencies owned by another
planned output, such as a route body calling a shared helper. This is
not byte emission yet; it is the link contract the emitter must
satisfy when it decides whether an external reference becomes an
actual wasm import, copied metadata, or a host-ABI call.

`ModuleAnalysis::validate_link_plan()` checks the contract before
emission. For every owned function body, each signature type and each
recorded instruction dependency must resolve through the chunk's local
or external remap. If a `call`, `ref.func`, table, memory, global,
data, element, or tag index has no mapping, the split must fail before
writing bytes.

`ModuleAnalysis::emit_function_chunk()` is the first byte-writing
pass. It emits a valid core wasm module for the function/type/import
subset of a link plan, re-encoding function bodies through the remap
instead of copying bytes. This first emitter intentionally fails on
tables, memories, globals, tags, data, and elements until those
sections have the same explicit ownership and remap treatment.
Original wasm function imports preserve their module/name/type
metadata. Cross-chunk function references still use a temporary
`pocopine:split` import namespace until the shared runtime ABI is
defined. Function exports are only emitted for functions owned by the
chunk, never for functions that are merely external references.
The emitter also handles table dependencies for indirect calls and
table operators, preserving original table import names and emitting
owned table definitions. Function-index element segments are now part
of the graph too: active, passive, and declared segments remap their
function items, and active segments remap their table. Expression
element segments and non-`i32.const` active offsets still fail closed
until const-expression remapping is expanded.

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
