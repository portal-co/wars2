# `wars-rt` — Runtime Library Reference

`wars-rt` is the `#![no_std]` (+ optional `std`) runtime crate that every
program compiled by `wars` depends on.  It provides:

- the `Memory` trait and its built-in implementations
- the `func` module: typed, trampolined, sync function-reference values
- the `func::unsync` module: the async equivalent
- the `CtxSpec` / `Traverse` traits that glue the host context to the runtime
- wasm operator implementations (arithmetic, memory loads/stores, …)
- optional GC support (`dumpster` feature)
- optional ICP stable-memory adapter (`ic-stable-structures` feature)

---

## Cargo features

| Feature | Enables |
|---------|---------|
| `std` *(default off)* | `std::sync::Mutex` instead of `spin::Mutex`; `anyhow` std support |
| `dumpster` | `gc` module, GC-traced `Value` variants, requires `std` |
| `ic-stable-structures` | `ic::Stable<T>` wrapper so ICP stable memory implements `Memory` |

---

## Core traits

### `CtxSpec`

```rust
pub trait CtxSpec: Sized {
    type ExternRef: Clone;
}
```

Every host context type must implement `CtxSpec`.  `ExternRef` is the
host-side type for wasm `externref` values.  Use `core::convert::Infallible`
when your module never touches externrefs.

### `Traverse<C>`

```rust
pub trait Traverse<C: CtxSpec> {
    fn traverse<'a>(&'a self)
        -> Box<dyn Iterator<Item = &'a C::ExternRef> + 'a>;
    fn traverse_mut<'a>(&'a mut self)
        -> Box<dyn Iterator<Item = &'a mut C::ExternRef> + 'a>;
}
```

Implemented by `FooData` (generated), `Value`, `Vec<V>`, `u32`, and `u64`.
Used by host code that needs to enumerate or update every externref held
inside an instance (e.g. for a moving GC or reference-counting scheme).

### `Memory`

```rust
pub trait Memory {
    fn read<'a>(&'a self, offset: u64, len: u64)
        -> anyhow::Result<Box<dyn AsRef<[u8]> + 'a>>;
    fn write(&mut self, offset: u64, data: &[u8])
        -> anyhow::Result<()>;
    fn size(&self)  -> anyhow::Result<u64>;   // current byte count
    fn grow(&mut self, extra_bytes: u64) -> anyhow::Result<()>;
}
```

All offsets, lengths and sizes are in **bytes**.

Built-in implementations:

| Type | Notes |
|------|-------|
| `Vec<u8>` | Heap-allocated linear memory |
| `Box<dyn Memory>` | Forwards to the inner `Memory` |
| `Arc<std::sync::Mutex<T: Memory>>` | Shared memory (requires `std`) |
| `Arc<spin::Mutex<T: Memory>>` | Shared memory (no-std) |
| `ic::Stable<T: ic_stable_structures::Memory>` | ICP stable memory (`ic-stable-structures` feature) |

---

## `func` — sync function references

`wars_rt::func` is the sync (trampolined) half of the runtime.

### `Value<C>`

```rust
#[repr(transparent)]
pub struct Value<C: CtxSpec>(pub value::Value<C, BorrowForLt<C>>);
```

A type-erased wasm value that can hold any of:

| Variant | Rust type stored |
|---------|-----------------|
| `I32` | `u32` |
| `I64` | `u64` |
| `F32` | `f32` |
| `F64` | `f64` |
| `FunRef` | `Arc<dyn Fn(&mut C, Vec<Value<C>>) -> BorrowRec<…>>` |
| `Null` | — |
| `ExRef` | `C::ExternRef` |
| `Gc` | `gc::GcCore<Value<C>>` *(dumpster feature)* |

`Value<C>` is `Clone`.

### `value::Value<C, R>` — the generic inner enum

The inner enum is parameterised over a higher-kinded lifetime token `R:
for<'a> ForLt<'a>` that determines what the `FunRef` variant returns for
lifetime `'a`.  Direct use of this type is rarely needed; prefer the
`Value<C>` wrapper from `func` or `func::unsync`.

### `Coe<C>` — coercions between concrete types and `Value<C>`

```rust
pub trait Coe<C: CtxSpec>: Sized {
    fn coe(self) -> Value<C>;
    fn uncoe(x: Value<C>) -> anyhow::Result<Self>;
}
```

Implemented for: `Value<C>` (identity), `u32`, `u64`, `f32`, `f64`,
`Option<D: Coe<C>>`, and `Df<A,B,C>` (see below).

The free function `cast` uses `Coe` plus a `castaway` fast-path to convert
between any two `Coe`-able types without allocating when the types are already
identical:

```rust
pub fn cast<A: Coe<C> + 'static, B: Coe<C> + 'static, C: CtxSpec>(a: A) -> B
```

### `CoeVec<C>` — coercions for argument/return lists

```rust
pub trait CoeVec<C: CtxSpec>: Sized {
    const NUM: usize;
    fn coe(self) -> Vec<Value<C>>;
    fn uncoe(a: Vec<Value<C>>) -> anyhow::Result<Self>;
}
```

Implemented for `()` (empty list) and `(A: Coe<C>, B: CoeVec<C>)` (cons
cell), matching the `tuple_list` cons-list convention.  Note the list is
**reversed**: the head element is pushed last, so `pop()` retrieves the first
element.

### `Df<A, B, C>` — typed function reference

```rust
pub type Df<A, B, C> =
    Arc<dyn for<'a> Dx<'a, A, B, C>>;

// where Dx is shorthand for:
// Fn(&'a mut C, A) -> BorrowRec<'a, anyhow::Result<B>> + Send + Sync + 'static
```

`A` and `B` must implement `CoeVec<C>`.

Construct a `Df` with:

```rust
pub fn da<A, B, C, F>(f: F) -> Df<A, B, C>
where F: for<'a> Fn(&'a mut C, A) -> BorrowRec<'a, anyhow::Result<B>>
         + Send + Sync + 'static
```

`Df<A,B,C>` itself implements `Coe<C>` so it can be stored in a `Value<C>`,
allowing function-reference tables to hold heterogeneous function types.

### `call_ref`

```rust
pub fn call_ref<'a, A, B, C>(
    ctx: &'a mut C,
    go: Df<A, B, C>,
    a: A,
) -> BorrowRec<'a, anyhow::Result<B>>
```

Calls a typed function reference with the trampoline protocol.  Used by
generated code for `call_ref` and `call_indirect` wasm instructions.

### `ret`

```rust
pub fn ret<'a, T>(a: T) -> BorrowRec<'a, T>
```

Wraps a value in `BorrowRec::Ret`.  Shorthand used by generated function
bodies at every return site.

### `Call<A, B, C>` — eager helper

```rust
pub trait Call<A, B, C>: … {
    fn call(&self, c: &mut C, a: A) -> anyhow::Result<B>;
}
```

Blanket-implemented for any `Df`; calls `tramp::tramp` internally.
Useful in host code that needs to call a typed function reference without
needing to thread lifetimes:

```rust
let result = my_df.call(&mut ctx, tuple_list!(42u32))?;
```

---

## `func::unsync` — async function references

Mirrors `func` but uses `async`/`await` instead of trampolining.

### `Value<C>`

Same structure as `func::Value<C>` but the `FunRef` variant's closure returns
`AsyncRec<'a, …>` instead of `BorrowRec<'a, …>`.

### `AsyncRec<'a, T>`

```rust
pub enum AsyncRec<'a, T> {
    Ret(T),
    Async(Pin<Box<dyn UnwrappedAsyncRec<'a, T>>>),
}
```

The async analogue of `tramp::BorrowRec`.  It is itself `Future`-like but
must be driven to completion with `.go().await`:

```rust
impl<'a, T> AsyncRec<'a, T> {
    pub async fn go(mut self) -> T { … }
}
```

`Ret(value)` is the base case; `Async(future)` is the recursive case.

### `UnwrappedAsyncRec<'a, T>`

```rust
pub trait UnwrappedAsyncRec<'a, T>:
    Future<Output = AsyncRec<'a, T>> + Send + Sync + 'a
```

Any `Future` that yields an `AsyncRec` automatically implements this trait.
Generated function bodies return `impl UnwrappedAsyncRec<'a, …>`.

### `Wrap<'a, T>`

```rust
pub trait Wrap<'a, T>: Sized {
    fn wrap(self) -> AsyncRec<'a, T>;
}
```

Converts either an `AsyncRec` (identity) or any `UnwrappedAsyncRec`
(box-pins it) into an `AsyncRec`.  Used by the generated export shims:

```rust
return AsyncRec::wrap(inner_func(self, args));
```

### `ret` (async)

```rust
pub fn ret<'a, T>(a: T) -> AsyncRec<'a, T>
```

Wraps a value in `AsyncRec::Ret`.

### `Df<A, B, C>` (async)

```rust
pub type Df<A, B, C> =
    Arc<dyn for<'a> Fn(&'a mut C, A) -> AsyncRec<'a, anyhow::Result<B>>
        + Send + Sync + 'static>;
```

### `call_ref` (async)

```rust
pub fn call_ref<'a, A, B, C>(
    ctx: &'a mut C,
    go: Df<A, B, C>,
    a: A,
) -> AsyncRec<'a, anyhow::Result<B>>
```

### `Coe<C>` / `CoeVec<C>` / `cast` / `da`

All exist in `func::unsync` with identical semantics to the sync versions,
operating on `unsync::Value<C>` and `unsync::Df`.

---

## `gc` — GC support *(feature: `dumpster`)*

Requires the `dumpster` feature (which in turn requires `std`).

### `GcCore<R>`

```rust
#[non_exhaustive]
pub enum GcCore<R> {
    Fields(Vec<Field<R>>),
}
```

The internal representation of a wasm GC object.  Currently only the
struct-style `Fields` variant exists.

Methods:

```rust
// Read field at index; panics if out of bounds.
pub fn get_field(&self, index: usize) -> R where R: Clone

// Write field at index; silently ignores writes to Const fields.
pub fn set_field(&self, index: usize, value: R) where R: Clone
```

### `Field<R>`

```rust
#[non_exhaustive]
pub enum Field<R> {
    Const(R),
    Mut(Arc<Mutex<R>>),
}
```

A struct field.  `Const` fields are read-only; `Mut` fields are
`Arc<Mutex<R>>` so they can be shared across GC roots.

### Newtype wrappers

```rust
pub struct Struct<W>(pub W);   // a wasm struct value
pub struct Array<W>(pub W);    // a wasm array value (reserved)
pub struct Const<W>(pub W);    // marks a field as immutable
pub struct Mut<W>(pub W);      // marks a field as mutable
```

All four implement `dumpster::Trace` when `W: Trace`.

### `CoeField<C>` / `CoeFieldVec<C>`

Parallel to `Coe`/`CoeVec` but for `Field<Value<C>>` lists.  Implemented for
`Const<V: Coe<C>>`, `Mut<V: Coe<C>>`, `()`, and `(A: CoeField<C>, B:
CoeFieldVec<C>)`.

`Struct<V: CoeFieldVec<C>>` implements `Coe<C>`, so a fully typed struct value
can be round-tripped through `Value<C>`.

---

## Wasm operator implementations

`wars_rt` exposes every wasm arithmetic, bitwise, and memory instruction as a
plain free function.  Generated code calls these functions directly.

### Integer operations (i32 / i64)

All take `u32` or `u64` arguments (wasm integers are always
bit-pattern-compatible with their unsigned Rust equivalents) and return
`anyhow::Result<tuple_list_type!(T)>` for the result type.

| Function | Wasm mnemonic |
|----------|--------------|
| `i32add` / `i64add` | `i32.add` / `i64.add` (wrapping) |
| `i32sub` / `i64sub` | `i32.sub` / `i64.sub` (wrapping) |
| `i32mul` / `i64mul` | `i32.mul` / `i64.mul` (wrapping) |
| `i32divu` / `i64divu` | `i32.div_u` / `i64.div_u` |
| `i32divs` / `i64divs` | `i32.div_s` / `i64.div_s` |
| `i32remu` / `i64remu` | `i32.rem_u` / `i64.rem_u` |
| `i32rems` / `i64rems` | `i32.rem_s` / `i64.rem_s` |
| `i32and` / `i64and` | `i32.and` / `i64.and` |
| `i32or` / `i64or` | `i32.or` / `i64.or` |
| `i32xor` / `i64xor` | `i32.xor` / `i64.xor` |
| `i32shl` / `i64shl` | `i32.shl` / `i64.shl` |
| `i32shru` / `i64shru` | `i32.shr_u` / `i64.shr_u` |
| `i32shrs` / `i64shrs` | `i32.shr_s` / `i64.shr_s` |
| `i32rotl` / `i64rotl` | `i32.rotl` / `i64.rotl` |
| `i32clz` / `i64clz` | `i32.clz` / `i64.clz` |
| `i32ctz` / `i64ctz` | `i32.ctz` / `i64.ctz` |
| `i32eqz` / `i64eqz` | `i32.eqz` / `i64.eqz` → `u32` result |
| `i32eq` / `i64eq` | `i32.eq` / `i64.eq` → `u32` result |
| `i32ne` / `i64ne` | `i32.ne` / `i64.ne` → `u32` result |
| `i32ltu` / `i64ltu` | `i32.lt_u` / `i64.lt_u` → `u32` |
| `i32gtu` / `i64gtu` | `i32.gt_u` / `i64.gt_u` → `u32` |
| `i32leu` / `i64leu` | `i32.le_u` / `i64.le_u` → `u32` |
| `i32geu` / `i64geu` | `i32.ge_u` / `i64.ge_u` → `u32` |
| `i32lts` / `i64lts` | `i32.lt_s` / `i64.lt_s` → `u32` |
| `i32gts` / `i64gts` | `i32.gt_s` / `i64.gt_s` → `u32` |
| `i32les` / `i64les` | `i32.le_s` / `i64.le_s` → `u32` |
| `i32ges` / `i64ges` | `i32.ge_s` / `i64.ge_s` → `u32` |

### Conversion operations

| Function | Wasm mnemonic |
|----------|--------------|
| `i32wrapi64(a: u64) -> u32` | `i32.wrap_i64` |
| `i64extendi32u(a: u32) -> u64` | `i64.extend_i32_u` |
| `i64extendi32s(a: u32) -> u64` | `i64.extend_i32_s` |
| `i64truncf64s(a: f64) -> u64` | `i64.trunc_f64_s` |

### Memory load/store

All memory functions are generic over the address type `T: TryInto<u64>` and
the memory type `M: Memory + ?Sized`.

**Loads** return `anyhow::Result<tuple_list_type!($int)>`.  
**Stores** return `anyhow::Result<()>`.

| Function | Width | Sign | Wasm mnemonic |
|----------|-------|------|--------------|
| `i32load` / `i64load` | native | — | `i32.load` / `i64.load` |
| `i32load8u` / `i64load8u` | 8-bit | zero-extend | `i32.load8_u` / `i64.load8_u` |
| `i32load8s` / `i64load8s` | 8-bit | sign-extend | `i32.load8_s` / `i64.load8_s` |
| `i32load16u` / `i64load16u` | 16-bit | zero-extend | `i32.load16_u` / `i64.load16_u` |
| `i32load16s` / `i64load16s` | 16-bit | sign-extend | `i32.load16_s` / `i64.load16_s` |
| `i64load32u` | 32-bit | zero-extend | `i64.load32_u` |
| `i64load32s` | 32-bit | sign-extend | `i64.load32_s` |
| `i32store` / `i64store` | native | — | `i32.store` / `i64.store` |
| `i32store8` / `i64store8` | low 8 bits | — | `i32.store8` / `i64.store8` |
| `i32store16` / `i64store16` | low 16 bits | — | `i32.store16` / `i64.store16` |
| `i64store32` | low 32 bits | — | `i64.store32` |

All values are stored in **native endian** byte order (matching the wasm
specification when the host is little-endian, which covers all practical wasm
targets).

### `select`

```rust
pub fn select<T>(cond: u32, if_true: T, if_false: T)
    -> anyhow::Result<tuple_list_type!(T)>
```

---

## `_rexport` — re-exported dependencies

`wars_rt::_rexport` re-exports crates that generated code depends on so that
the generated code does not need to name them in its own `Cargo.toml`:

| Path | Crate |
|------|-------|
| `wars_rt::_rexport::anyhow` | `anyhow` |
| `wars_rt::_rexport::tramp` | `portal-pc-tramp` |
| `wars_rt::_rexport::tuple_list` | `tuple_list` |
| `wars_rt::_rexport::alloc` | `alloc` (the standard `alloc` crate) |

---

## `Pit<X, H>` — portal interface type

```rust
#[derive(Clone)]
pub enum Pit<X, H> {
    Guest { id: [u8; 32], x: X, s: [u8; 32] },
    Host  { host: H },
}
```

A discriminated union used by the PIT (Portal Interface Type) bridge.
`X` is the wasm-side representation; `H` is the host-side representation.
Not needed for ordinary wasm compilation.

---

## `Err` helper trait

```rust
pub trait Err: Into<anyhow::Error> {}
impl<T: Into<anyhow::Error>> Err for T {}
```

Used as a bound on address-type errors in the memory load/store functions.
No user code needs to implement or name this trait directly.

---

## no_std compatibility

`wars-rt` is `#![no_std]` by default.  It depends on `alloc` for heap
allocation.  Without the `std` feature it uses `spin::Mutex` for shared
memories and disables the `anyhow` standard-library integration.

When targeting a platform without an allocator (e.g. bare-metal) you must
provide a global allocator before using any `wars_rt` types.
