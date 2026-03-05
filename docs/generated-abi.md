# Generated ABI/API Reference

The `wars` compiler (with the `waffle` feature) compiles a WebAssembly binary into a Rust
module.  This document describes every piece of Rust code that the compiler emits so you
know exactly what to implement, what to call, and what to expect at the boundary between
the generated code and your host program.

---

## Overview

For a call like

```rust
wars::OptsCore { name: format_ident!("Greeter"), … }
    .inflate::<LegacyPortalWaffleBackend>()
    .to_waffle_mod()
```

the compiler emits roughly:

```
pub struct GreeterData<Target: Greeter + ?Sized> { … }

pub trait Greeter: wars_rt::func::CtxSpec<ExternRef = Self::_ExternRef> + … {
    type _ExternRef: Clone;
    fn data(&mut self) -> &mut GreeterData<Self>;
    // … one method per table, global, memory, and import …
}

pub trait GreeterImpl: Greeter {
    // … one method per wasm export …
    fn init(&mut self) -> anyhow::Result<()> where Self: 'static;
}

// blanket impl of GreeterImpl for every C: Greeter
const _: () = {
    use wars_rt::Memory;
    impl<C: Greeter> GreeterImpl for C { … }
    // free functions, one per wasm function body
    fn func0_name<'a, C: Greeter + 'static>(ctx: &'a mut C, …) -> … { … }
    …
};
```

The name `Greeter` is whatever you pass as `OptsCore::name`.

---

## The `*Data` struct

```rust
pub struct FooData<Target: Foo + ?Sized> {
    // one field per wasm table   – Vec<func::Value<Target>>
    // one field per wasm global  – the corresponding Rust primitive
    // one field per owned memory – Vec<u8>  (or Arc<Mutex<Vec<u8>>> if shared)
    // plus any extra fields you injected via OptsCore::data
}
```

`FooData` is `Default` and `Clone` (both bounds require `Target: Foo`).

It also implements `wars_rt::Traverse<Target>`, which lets a GC or reference scanner
walk every `ExternRef` stored inside the instance.

Your concrete host type must store a `FooData<Self>` and return it from `Foo::data`.

---

## The host trait (`Foo`)

```rust
pub trait Foo: wars_rt::func::CtxSpec<ExternRef = Self::_ExternRef>
    /* + Send + Sync  – only when compiled with Flags::ASYNC */
{
    /// The type of host-provided external references held inside the instance.
    type _ExternRef: Clone;

    /// Access the instance's state.
    fn data(&mut self) -> &mut FooData<Self>;

    // ── Tables ──────────────────────────────────────────────────────────────
    // One method per wasm table, named after the waffle entity index (table0,
    // table1, …).  Returns a mutable reference to the Vec that backs the table.
    fn table0(&mut self) -> &mut alloc::vec::Vec<wars_rt::func::Value<Self>>;

    // ── Globals ─────────────────────────────────────────────────────────────
    // One method per wasm global, named after the entity index (global0, …).
    // Returns &mut <rust-type-of-global>.
    fn global0<'a>(&'a mut self) -> &'a mut u32;

    // ── Owned memories ──────────────────────────────────────────────────────
    // One method per wasm memory that is NOT imported, named memory0, memory1 …
    // Returns &mut Vec<u8>  (or &mut Arc<Mutex<Vec<u8>>> when shared).
    fn memory0<'a>(&'a mut self) -> &'a mut Vec<u8>;

    // ── Imported memories ───────────────────────────────────────────────────
    // An imported memory produces TWO methods.
    //
    // The first is the canonical name built from the import's module+name
    // (after name-mangling), e.g. for import("env","mem") → env_mem.
    // This is the one you implement on your concrete type.
    fn env_mem<'a>(&'a mut self) -> &'a mut (impl wars_rt::Memory + 'a);
    //
    // The second, named after the entity index, delegates to the first
    // and is provided for free:
    // fn memory0<'a>(&'a mut self) -> &'a mut (impl wars_rt::Memory + 'a) {
    //     self.env_mem()
    // }
    //
    // When Flags::LEGACY is set the return type is `dyn Memory + 'a` instead
    // of `impl Memory + 'a`.

    // ── Wasm imports ────────────────────────────────────────────────────────
    // One required method per imported *function*, named
    //   <module>_<name>
    // after applying name-mangling (alphanumeric chars kept; every other char
    // replaced with _<codepoint>_).
    //
    // Sync mode:
    fn env_42_my_func<'a>(
        &'a mut self,
        imp: tuple_list::tuple_list_type!(u32, u64),
    ) -> tramp::BorrowRec<'a, anyhow::Result<tuple_list::tuple_list_type!(u32)>>
    where Self: 'static;
    //
    // Async mode (Flags::ASYNC):
    fn env_42_my_func<'a>(
        &'a mut self,
        imp: tuple_list::tuple_list_type!(u32, u64),
    ) -> wars_rt::func::unsync::AsyncRec<'a, anyhow::Result<tuple_list::tuple_list_type!(u32)>>
    where Self: 'static;
}
```

### Name mangling

Module and function names from the wasm import section are mapped to Rust identifiers
with `bindname`:

| character | replacement |
|-----------|-------------|
| alphanumeric | kept as-is |
| anything else | `_<decimal-codepoint>_` |

For example `wasi_snapshot_preview1` → `wasi_snapshot_preview1` (unchanged),
`env` / `abort` → `env_abort`, `wars/bind` → `wars_47_bind`.

---

## The impl trait (`FooImpl`)

```rust
pub trait FooImpl: Foo {
    // One method per wasm export of kind "function":
    fn exported_name<'a>(
        &'a mut self,
        /* params matching the export signature */
    ) -> /* return type */
    where Self: 'static;

    /// Initialise the instance: grow / populate memories, fill tables,
    /// and set globals to their initial values.
    fn init(&mut self) -> anyhow::Result<()> where Self: 'static;
}
```

This trait is implemented automatically for every `C: Foo` by the blanket
`impl<C: Foo> FooImpl for C { … }` inside the emitted `const _: ()` block.
You never implement it yourself; you just call `ctx.init()?` once after
constructing your host type, and then call the export methods directly.

### Export method signature (sync)

```rust
fn my_export<'a>(
    self: &'a mut Self,
    tuple_list!(p0, p1, …): tuple_list_type!(T0, T1, …),
) -> tramp::BorrowRec<'a, anyhow::Result<tuple_list_type!(R0, R1, …)>>
where Self: 'static
```

The return value is a trampolined recursive call.  Evaluate it to completion
with `tramp::tramp(ctx.my_export(args))`.

### Export method signature (async, Flags::ASYNC)

```rust
fn my_export<'a>(
    self: &'a mut Self,
    tuple_list!(p0, p1, …): tuple_list_type!(T0, T1, …),
) -> wars_rt::func::unsync::AsyncRec<'a, anyhow::Result<tuple_list_type!(R0, R1, …)>>
where Self: 'static
```

Evaluate with `.go().await`.

---

## Type mapping

| WebAssembly type | Rust type in signatures |
|-----------------|------------------------|
| `i32` | `u32` |
| `i64` | `u64` |
| `f32` | `f32` |
| `f64` | `f64` |
| `v128` | `u128` |
| non-nullable `funcref` to signature S | `wars_rt::func::Df<Params, Returns, C>` (sync) or the async variant |
| nullable `funcref` | `Option<wars_rt::func::Df<…>>` |
| any other ref type | `wars_rt::func::Value<C>` |

Multi-value returns and multi-param function types are represented with
`tuple_list` cons-lists: `(T0, (T1, (T2, ())))`.  The macro
`tuple_list!(a, b, c)` constructs a value; `tuple_list_type!(T0, T1, T2)`
names the type.

---

## Internal free functions

Inside the `const _: ()` block the compiler emits one free function per wasm
function body:

```rust
fn func42_my_internal_name<'a, C: Foo + 'static>(
    ctx: &'a mut C,
    tuple_list!(p0, p1): tuple_list_type!(u32, u64),
) -> tramp::BorrowRec<'a, anyhow::Result<tuple_list_type!(u32)>>
```

The function name is `<waffle-entity-index>_<wasm-name>`, where the wasm name
is passed through `bindname`.  These functions are not part of any public API;
they are used internally and by the blanket `FooImpl` impl.

---

## Memory access protocol

The generated code never accesses linear memory directly.  Instead it calls
the `wars_rt::Memory` trait methods through the context:

```
ctx.memory0().read(offset, len)?
ctx.memory0().write(offset, data)?
ctx.memory0().size()?        // returns byte count
ctx.memory0().grow(n_bytes)?
```

All offsets, lengths and sizes are `u64`.  The compiler emits the static
offset from the wasm instruction already added to the dynamic address before
calling `read`/`write`, so your `Memory` implementation receives the final
byte address.

`memory.grow` receives the number of *bytes* to append, not the number of
wasm pages.

---

## Table access protocol

Tables are accessed directly through the context as `Vec<wars_rt::func::Value<C>>`:

```rust
ctx.table0()[index as usize].clone()    // TableGet
ctx.table0()[index as usize] = value;   // TableSet
ctx.table0().len() as u32               // TableSize
ctx.table0().push(value);               // TableGrow (repeated n times)
```

Function-reference tables are pre-populated during `init()`.

---

## Call protocol

### Direct calls to wasm-defined functions

```rust
let result = tramp::tramp(func42_name(ctx, tuple_list!(arg0, arg1)))?;
```

In async mode:

```rust
let result = func42_name(ctx, tuple_list!(arg0, arg1)).go().await?;
```

### Indirect calls / call_ref

```rust
wars_rt::func::call_ref::<Params, Returns, C>(ctx, fun_ref_value, args)
```

Both sync and async variants of `call_ref` exist in `wars_rt::func` and
`wars_rt::func::unsync` respectively.

### Return-call / tail-call

In sync mode the compiler emits a `tramp::BorrowRec::Call` thunk so the
host trampoline (`tramp::tramp`) handles tail-call elimination in O(1) stack
space.

In async mode the future is returned directly (the Rust async executor
handles the stack).

---

## Initialisation sequence

After constructing your host type `ctx`, call:

```rust
ctx.init()?;
```

This will, in order:

1. Grow each owned linear memory to at least its `minimum` page count.
2. Write every wasm data segment into memory.
3. Set every global to its initialiser value.
4. Populate every table with its element-section function references.

It is safe (and necessary) to call `init` exactly once before invoking
any exports.

---

## Optional features that affect the generated code

| `Flags` bit | Effect on generated code |
|-------------|--------------------------|
| `Flags::ASYNC` | All function signatures use `unsync::AsyncRec` instead of `tramp::BorrowRec`; the context trait gains `Send + Sync` bounds |
| `Flags::LEGACY` | Imported-memory return types use `dyn Memory + 'a` instead of `impl Memory + 'a` |
| `Flags::NEW_ABI` | Not yet implemented; panics at compile time if set |

---

## Putting it all together: minimal example

```rust
use wars_rt::{Memory, func::{self, Value}};
use tramp::tramp;

// 1. Implement the host trait
struct MyHost {
    data: GreeterData<Self>,
    memory: Vec<u8>,
}
impl wars_rt::CtxSpec for MyHost {
    type ExternRef = std::convert::Infallible;
}
impl wars_rt::func::CtxSpec for MyHost {
    type ExternRef = std::convert::Infallible;
}
impl Greeter for MyHost {
    type _ExternRef = std::convert::Infallible;
    fn data(&mut self) -> &mut GreeterData<Self> { &mut self.data }
    fn memory0<'a>(&'a mut self) -> &'a mut Vec<u8> { &mut self.memory }
    // … implement imported functions …
}

// 2. Construct and initialise
let mut host = MyHost {
    data: Default::default(),
    memory: vec![0u8; 65536],
};
host.init().unwrap();

// 3. Call an export
let result = tramp(host.greet(tuple_list!(42u32))).unwrap();
```
