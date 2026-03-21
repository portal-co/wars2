# wars2

A Rust library and code-generation tool that compiles WebAssembly modules into native Rust code. The output is a set of Rust traits and free functions that implement the module's semantics, with no interpreter loop at runtime.

Repository: https://github.com/portal-co/wars2

## What it does

Given a `.wasm` binary, `wars` (the compiler crate) reads the module and emits Rust token streams (via `proc_macro2` / `quote`) that:

- Define a trait named after the module containing one method per exported function.
- Define a `*Data` struct that holds the module's state: linear memories, globals, and tables.
- Implement all exported functions as free Rust functions that operate on a user-supplied context type.
- Wire up imports by delegating to methods the caller must implement on the context.

The generated code uses trampolining (`portal-pc-tramp`) to avoid stack overflows on recursive wasm programs, and represents all values either as primitive Rust types (`u32`, `u64`, `f32`, `f64`) or as `wars_rt::func::Value<C>` for reference types.

## Workspace crates

### `wars` (0.9.0-alpha.1)

The compiler. Takes `OptsCore` (wasm bytes, a name, flags, optional plugins) and produces a `TokenStream`.

Two backends are available via Cargo features:

- **`wasmparser`** (`WasmparserBackend`) — a single-pass backend that walks the binary with `wasmparser` directly. Intended to replace the waffle pipeline for common cases. Described in code as "fast, dependency-light."
- **`waffle`** (`LegacyPortalWaffleBackend`) — an older backend built on the `portal-pc-waffle` IR. It applies a relooper pass and an "unswitch" transform (converting `select`-terminator blocks into chains of conditional branches before handing the CFG to `relooper`) before emitting code.

Both backends share code in `src/shared.rs` for name mangling, type rendering, and function-signature helpers.

Generation flags (bitflags):
- `ASYNC` — emit async-compatible signatures using `wars_rt::func::unsync`.
- `LEGACY` — legacy ABI mode.
- `NEW_ABI` — opt into an updated calling convention (0x100).

A `Plugin` trait lets callers intercept import resolution and inject custom code at the pre/post generation hooks.

### `wars-rt` (0.9.0-alpha.1)

The runtime support library used by generated code. It is `no_std` + `alloc`-compatible.

Key pieces:
- `Memory` trait — abstraction over linear memory (read/write/size/grow). Implementations provided for `Vec<u8>`, `Box<dyn Memory>`, and `Arc<Mutex<T>>`. An optional `ic-stable-structures` feature adds an implementation backed by Internet Computer stable memory.
- `func::Value<C>` — the runtime value type for reference values, covering i32/i64/f32/f64, function references (`FunRef` as an `Arc<dyn Fn>`), external references, null, and (with the `dumpster` feature) GC-managed structs.
- `func::Df<A,B,C>` — type alias for a reference-counted, type-erased function reference.
- `CtxSpec` trait — marker trait that context types implement to declare their `ExternRef` and `Error` associated types.
- Integer/float intrinsics — wrapping arithmetic, shifts, comparisons, and load/store helpers generated via macros, exported as top-level free functions for use by generated code.
- `gc` module (feature `dumpster`) — GC support using the `dumpster` cycle-collecting GC, with `GcCore<R>`, `Field<R>` (const/mutable), and newtypes `Struct`, `Array`, `Const`, `Mut`.
- `wrl` module — adapter layer to `wasm_runtime_layer`, translating between `wars_rt`'s value types and `wasm_runtime_layer::Value`.
- `wasix` module — currently empty (stub).

### `waffle-func-reloop` (0.9.0-alpha.1)

A tiny helper crate used by the waffle backend. Given a `waffle::FunctionBody`, it builds the CFG, filters to dominance-reachable blocks, and calls `relooper::reloop` to produce a structured `ShapedBlock<Block>` tree. Panics with a diagnostic if `relooper` fails.

### `tester` (0.1.0)

Integration test crate. The build script (`build.rs`) constructs a simple wasm module in-memory using `wasm-encoder` (with functions: `add`, `sub`, `calladd`, `getglobal`, `setglobal`, `loadi32`, `storei32`), then runs both backends to generate Rust code, writes the output to `OUT_DIR`, and formats it with `prettyplease`. The generated modules are included via `include!` and exercised in `tests/integration.rs`.

## Current state

- Version 0.9.0-alpha.1 across all workspace crates. The project has had several previous release tags (v0.7.x, v0.8.x).
- The wasmparser backend is described in comments as a replacement for the waffle pipeline, suggesting the waffle backend is in a transitional / legacy state.
- Several items are commented out or stubbed: `wasix` module is nearly empty, `HOST_MEMORY` and `WASIX` flags exist but are commented out, some `TODO` comments remain (e.g., `Field::Mut` traversal in `gc.rs`).
- The `wrl` module (wasm_runtime_layer bridge) is present but its dependency is commented out in `Cargo.toml` (`# wasm_runtime_layer = "0.4.0"`), suggesting it is not currently wired up.
- License: CC0-1.0.

## Dependencies (notable)

- `portal-pc-waffle` / `portal-pc-waffle-ir` — a fork of the `waffle` wasm IR, sourced from `https://github.com/portal-co/waffle-.git`.
- `relooper` 0.1.0 — converts arbitrary CFGs into structured control flow.
- `portal-pc-tramp` 0.3.0 — trampolining to avoid stack overflows in generated recursive functions.
- `dumpster` 1.0.0 (optional) — cycle-collecting GC used for wasm GC support.
- `wasmparser` 0.240.0 — used by the new backend and the tester.
- `wasm-encoder` 0.240.0 — used by the tester to construct test wasm binaries.

## Building

```sh
cargo build
cargo test -p tester
```

The `tester` crate requires both the `waffle` and `wasmparser` features on the `wars` dependency (declared in its `Cargo.toml`).
