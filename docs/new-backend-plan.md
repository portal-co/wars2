# New Backend Plan — `wasmparser`-based ABI v0 Compiler

## Motivation

The existing `LegacyPortalWaffleBackend` (behind the `waffle` Cargo feature) pulls
in a large dependency tree: `portal-pc-waffle`, `portal-pc-waffle-passes-shared`,
and `waffle-func-reloop`.  Those crates lift the entire binary into an SSA IR, run
passes (max-SSA, reducify, reloop), and only then emit tokens.  This is expensive
both at compile time (waffle is a heavy crate) and at proc-macro invocation time
(the IR construction and pass pipeline add latency on every build).

`wasmparser` is already a workspace dependency (`wasmparser = "0.240.0"`), is
`no_std`-compatible, streaming/event-driven, and already has a stub
`WasmparserBackend` type in `crates/wars/src/lib.rs`.  By driving code generation
directly from `wasmparser`'s structured operator stream we can:

- eliminate the three `waffle`-family dependencies entirely (they remain
  optional behind `feature = "waffle"` for users who need the old backend),
- reduce proc-macro wall-clock time for clean builds,
- produce Rust that targets the same ABI v0 output shape, so all downstream
  users migrate transparently.

---

## Scope

This document covers only the new backend.  `wars-rt` is unchanged.
ABI v0 (documented in `docs/generated-abi.md`) is the fixed target.

---

## High-level architecture

```
OptsLt<'_, &[u8], WasmparserBackend>
        │
        │  new_backend::go(&opts) -> TokenStream
        │
        ├─ 1. Parse pass  (wasmparser streaming)
        │      Collect all section data into a plain Module struct
        │
        ├─ 2. Scaffolding emission
        │      *Data struct, host trait, FooImpl trait, blanket impl shell
        │
        ├─ 3. Function body emission  (per function)
        │      structured-code walk → token stream
        │
        └─ 4. init() emission
               memory grow/write, global init, table fill
```

No IR.  No SSA.  No passes.  The wasmparser byte stream is walked once.

---

## Step 1 — Parser pass

### What to collect

Walk `wasmparser::Parser::parse_all(bytes)` and accumulate:

```rust
struct ParsedModule<'a> {
    // indexed by their u32 section-order index
    types:    Vec<FuncType>,          // TypeSection
    imports:  Vec<Import<'a>>,        // ImportSection  (kept as-is)
    // resolved counts (imports precede definitions in the index space)
    func_type_indices: Vec<u32>,      // FunctionSection + import slots
    tables:   Vec<TableType>,         // TableSection + import slots
    memories: Vec<MemoryType>,        // MemorySection + import slots
    globals:  Vec<GlobalType>,        // GlobalSection + import slots
    exports:  Vec<Export<'a>>,        // ExportSection
    start:    Option<u32>,            // StartSection
    elements: Vec<Element<'a>>,       // ElementSection
    data:     Vec<Data<'a>>,          // DataSection
    // function bodies stored as raw bytes for deferred parsing
    bodies:   Vec<FunctionBody<'a>>,  // CodeSectionEntry payloads
    // name section (optional, best-effort)
    func_names: HashMap<u32, &'a str>,
}
```

All index spaces match the wasm spec: imports come first, then definitions.
`func_type_indices[i]` gives the type-section index of function `i` (across
both imported and defined functions).

### Key wasmparser API used

| Goal | API |
|------|-----|
| Iterate sections | `Parser::parse_all(bytes)` → `Payload` variants |
| Read function types | `Payload::TypeSection` → iterate `TypeSectionReader` → `SubType` → `CompositeInnerType::Func(FuncType)` |
| Read imports | `Payload::ImportSection` → `ImportSectionReader` |
| Read function index→type mapping | `Payload::FunctionSection` → `FunctionSectionReader` (each entry is a `u32` type index) |
| Read tables/memories/globals | corresponding `*SectionReader` variants |
| Read exports | `Payload::ExportSection` → `ExportSectionReader` |
| Read element segments | `Payload::ElementSection` → `ElementSectionReader` |
| Read data segments | `Payload::DataSection` → `DataSectionReader` |
| Read function bodies | `Payload::CodeSectionEntry(FunctionBody)` |
| Read operators | `FunctionBody::get_operators_reader()` → `OperatorsReader::read()` → `Operator<'_>` |
| Read locals | `FunctionBody::get_locals_reader()` → `(count, ValType)` pairs |

No validator or component-model features are needed; the baseline `wasmparser`
crate with no extra features is sufficient.

### Index space bookkeeping

The index space for each entity type (functions, tables, memories, globals)
starts at 0 and counts imports before definitions.  During the parse pass,
maintain a running counter per entity type as imports are encountered:

```rust
let mut n_func_imports:   u32 = 0;
let mut n_table_imports:  u32 = 0;
let mut n_memory_imports: u32 = 0;
let mut n_global_imports: u32 = 0;
```

This lets the function-body emission phase refer to entities by their final
index-space index, which is what the emitted Rust methods are named after
(`memory0`, `global0`, etc.).

---

## Step 2 — Scaffolding emission

This step is a near-direct port of the scaffolding logic in the existing
`go()` function in `crates/wars/src/impl.rs`.  The helpers `alloc()`, `fp()`,
`render_ty()`, `render_generics()`, `render_fn_sig()`, `render_export()`,
`render_self_sig_import()`, and `fname()` from `impl.rs` need only minimal
changes because they depend only on `OptsCore` plus a few pieces of type
information.

### Refactoring plan for shared helpers

Move the helpers that only depend on `OptsCore` (i.e. not on `waffle::Module`)
out of `impl.rs` into a new `crates/wars/src/shared.rs` module, parametrised
over a thin `TypeInfo` trait or struct instead of `waffle::SignatureData`.

```rust
// crates/wars/src/shared.rs

pub(crate) struct FuncSig<'a> {
    pub params:  &'a [ValType],   // wasmparser::ValType
    pub returns: &'a [ValType],
}

// render_ty, render_generics, render_fn_sig, render_export,
// render_self_sig_import all move here, taking FuncSig instead of
// SignatureData.  bindname and alloc/fp helpers move here too.
```

Both `impl.rs` (waffle backend) and `new_backend.rs` then import from
`shared.rs`, avoiding duplication.

### Scaffolding token stream structure

The emitted `TokenStream` has the same shape as ABI v0:

```rust
pub struct FooData<Target: Foo + ?Sized> { … }

impl<Target: Foo + ?Sized> Traverse<Target> for FooData<Target> { … }

pub trait Foo: fp()::CtxSpec<ExternRef = Self::_ExternRef> {
    type _ExternRef: Clone;
    fn data(&mut self) -> &mut FooData<Self>;
    // tables, globals, memories, imports
}

pub trait FooImpl: Foo {
    // exports + init
}

const _: () = {
    use wars_rt::Memory;
    impl<C: Foo> FooImpl for C {
        // export shims + init body
    }
    // free functions (one per defined wasm function)
};
```

---

## Step 3 — Function body emission

This is the hardest part, and the biggest architectural difference from the
waffle backend.

### The challenge: wasmparser is a stack machine, waffle was an SSA IR

`wasmparser` gives us a flat, structured stream of `Operator` values using
the wasm structured control flow (block/loop/if/else/end).  There is no
implicit register assignment, no relooper, and no max-SSA pass.  We must
implement value tracking ourselves.

### Approach: explicit operand stack + block stack

Maintain two stacks at token-generation time (during the single pass over
`Operator` values):

#### Operand stack

Each entry is either:
- A named local (`local_N`) — for `LocalGet`/`LocalSet`/`LocalTee`
- An SSA-style temporaries (`_t0`, `_t1`, …) — for all other pushes

When an operator produces a value, emit:

```rust
let _t42 = <result expr>;
```

and push `_t42` onto the operand stack.

When an operator consumes N values, pop N temporaries off the stack and
splice them into the emitted expression.

#### Block stack

Each wasm control structure (block/loop/if) corresponds to a Rust control
structure.  Maintain a stack of `Frame` entries:

```rust
enum FrameKind { Block, Loop, If, Else }

struct Frame {
    kind:        FrameKind,
    label:       usize,            // for break/continue targets
    block_ty:    BlockType,        // result type
    result_temp: Option<Ident>,    // _b0, _b1 … for block results
    // for If: the condition temp and the else-branch accumulated tokens
}
```

Map wasm control structures to Rust:

| Wasm | Rust |
|------|------|
| `block … end` | `let _b0; 'l0: { … _b0 = last_val; }` |
| `loop … end` | `'l0: loop { … }` |
| `if … end` | `if _cond != 0 { … }` |
| `if … else … end` | `if _cond != 0 { … } else { … }` |
| `br N` | `break 'lN` / `continue 'lN` |
| `br_if N` | `if _cond != 0 { break/continue 'lN }` |
| `br_table …` | `match _val { … }` |
| `return` | `return fp()::ret(Ok(tuple_list!(…)))` |

### Locals

At function entry, emit:

```rust
let mut local_0: u32 = p0;    // params become locals
let mut local_1: u64 = p1;
let mut local_2: u32 = 0;     // other locals default-initialised
…
```

`LocalGet { local_index }` → push `local_N` onto operand stack (no `let`
needed; locals are always in scope).  
`LocalSet { local_index }` → `local_N = <popped>;`  
`LocalTee { local_index }` → `local_N = <peeked>; /* keep on stack */`

### Operator mapping

Most operators map directly to a call into `wars_rt`:

```rust
// e.g. I32Add
let _t5 = {
    let (a, (b, ())) = (popped_1, (popped_0, ()));
    wars_rt::i32add(a, b)?   // returns tuple_list!(u32) via tuple_list!
};
let (local_res_0, ()) = _t5;
```

For memory ops, add the static offset before calling `read`/`write`:

```rust
// I32Load { memarg: MemArg { offset, .. } }
let _t7 = match wars_rt::i32load(ctx.memory0(), _t6 + #offset) {
    Ok(a) => a,
    Err(e) => return fp()::ret(Err(e)),
};
let (_t7_0, ()) = _t7;
```

For calls:
- `Call { function_index }` — if defined: `func_N_name(ctx, tuple_list!(…))`,
  if imported: `ctx.module_name(tuple_list!(…))`, both followed by the
  tramp/await unwrap pattern.
- `CallIndirect { type_index, table_index }` — `fp()::call_ref(ctx,
  fp()::cast(ctx.tableN()[idx as usize].clone()), tuple_list!(…))`.
- `ReturnCall` / `ReturnCallRef` — same as above but wrapped in
  `BorrowRec::Call(Thunk::new(…))` for sync or returned directly for async.

### Unreachable code

After `unreachable`, `return`, `br` (unconditional), and at the `else`/`end`
boundaries of unreachable regions, the operand stack may be in an arbitrary
state.  Track a `unreachable_depth: u32` counter (incremented on entering an
unreachable region, decremented on `end`); suppress all token emission while
`unreachable_depth > 0`.

### Multi-value and `Drop`

The `tuple_list` ABI makes multi-value natural: when a wasm call returns
multiple values, destructure:

```rust
let (_t8_0, (_t8_1, ())) = result_tuple;
```

and push each part separately.  `Drop` just pops one temp without emitting
anything.

---

## Step 4 — `init()` body emission

Walk `data`, `element`, and `global` sections to emit the init body.

### Memory initialisation

For each memory (in index order):
1. Emit the grow-to-minimum call (same as the waffle backend):

```rust
let l = #min_bytes.max(ctx.memoryN().size()?);
let s = ctx.memoryN().size()?;
ctx.memoryN().grow(l - s)?;
```

2. For each active data segment targeting this memory, chunk the bytes and
   emit `ctx.memoryN().write(#offset, &[#(#bytes),*])?;`.

### Global initialisation

For each defined global whose init expression is a constant:

```rust
*ctx.globalN() = (#value as #ty);
```

Only `i32.const`, `i64.const`, `f32.const`, `f64.const`, and `global.get`
constant expressions need to be handled; anything more complex can be deferred
to a future iteration.

### Table element segments

For each active element segment targeting a table that contains function
indices, emit:

```rust
ctx.tableN()[#offset as usize] = fp()::cast(render_fun_ref(func_idx));
```

where `render_fun_ref` produces the `fp()::da(…)` closure wrapping the
corresponding `func_N_name` free function (exactly as in the waffle backend).

---

## Cargo changes

### `crates/wars/Cargo.toml`

```toml
[dependencies]
# wasmparser is already a workspace dep; activate it for the new backend
wasmparser = { workspace = true, optional = true }

[features]
waffle      = ["dep:portal-pc-waffle", …]   # unchanged
wasmparser  = ["dep:wasmparser"]             # new
```

The `WasmparserBackend` type is already declared unconditionally in `lib.rs`;
gate only the implementation:

```rust
// lib.rs
#[cfg(feature = "wasmparser")]
pub(crate) mod new_backend;   // change from pub(crate) mod new_backend (stub)
```

---

## File layout

```
crates/wars/src/
├── lib.rs               – add wasmparser feature gate on new_backend mod
├── impl.rs              – waffle backend (unchanged)
├── shared.rs            – NEW: helpers shared by both backends
│                            bindname, alloc(), fp(), render_ty(),
│                            render_generics(), render_fn_sig(),
│                            render_export(), render_self_sig_import(),
│                            fname() (refactored to take FuncSig)
├── new_backend.rs       – NEW: WasmparserBackend implementation
│   ├── mod.rs (inline)  – go(), ToTokens impl for WasmparserBackend
│   ├── parse.rs         – ParsedModule, step 1
│   ├── scaffold.rs      – step 2 (uses shared.rs)
│   └── func.rs          – step 3: function body emission
└── unswitch.rs          – unchanged
```

---

## Known limitations to accept in v1

These are real constraints worth accepting in the first iteration to keep
scope manageable:

1. **No multi-memory beyond simple indexing.**  The backend will support
   multiple memories (they each get their own method) but won't implement
   `memory.copy` between two different memories in v1.

2. **Constant-expression globals only.**  Non-constant global initialisers
   (which are theoretically valid wasm but vanishingly rare in practice) will
   emit a compile-time panic with a clear error message.

3. **`select` with typed operands only (`TypedSelect`).**  The untyped
   `select` instruction requires type inference from the stack; in v1 emit a
   monomorphic `if` that relies on `Coe::cast` like the waffle backend does.

4. **No GC / reference types beyond funcref/externref.**  The `gc` feature
   requires `struct.new`, `struct.get`, etc.  These can be added behind a
   separate gate in a follow-on.

5. **No tail-call proposal (`return_call`, `return_call_ref`) in async mode
   in v1.**  Sync tail calls map cleanly to `BorrowRec::Call`; the async
   equivalents need more thought and can be handled later.

6. **No SIMD (`v128`).**  `v128` passes through as `u128` for signatures, but
   SIMD operators will emit `todo!()` with a clear message in v1.

---

## Work items, in order

1. **Extract `shared.rs`.**  Move `bindname`, `alloc`, `fp`, `render_ty`,
   `render_generics`, `render_fn_sig`, `render_export`, `render_self_sig_import`
   out of `impl.rs` into `shared.rs`, replacing the `waffle::SignatureData`
   parameter with the new `FuncSig` struct.  Update `impl.rs` to use
   `shared::FuncSig` as a thin wrapper around `waffle::SignatureData`.

2. **Implement `parse.rs`** (`ParsedModule` + the streaming parse pass).

3. **Implement `scaffold.rs`** (emit `*Data`, the host trait, `FooImpl` trait
   and blanket impl shell, using `shared.rs`).

4. **Implement `func.rs`** (operand stack + block stack + operator dispatch).
   Start with the core MVP operator set; add extended operators incrementally.

5. **Wire up `new_backend.rs`**: implement `ToTokens` for
   `OptsLt<'_, &[u8], WasmparserBackend>` calling `go()` (mirroring the
   waffle backend's `to_tokens`).

6. **Add `wasmparser` feature to `Cargo.toml`** and gate the new module.

7. **Write integration tests** using the same `.wasm` fixtures as the waffle
   backend to verify output parity on the ABI v0 surface.
