//! Helpers shared between backends.
//!
//! These functions depend only on `OptsCore` plus a thin `FuncSig` description
//! of a wasm function type expressed via the `WasmTy` trait.  No backend
//! crate (`wasmparser`, `waffle`, …) is imported at the module level, so this
//! file compiles under any feature combination.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Ident;

use crate::{Flags, OptsCore};

// ── Name mangling ─────────────────────────────────────────────────────────────

/// Map a wasm import/export name to a valid Rust identifier fragment.
///
/// Alphanumeric characters are kept; everything else becomes `_<codepoint>_`.
pub(crate) fn bindname(a: &str) -> String {
    let mut v = vec![];
    for k in a.chars() {
        if k.is_alphanumeric() {
            v.push(k)
        } else {
            v.extend(format!("_{}_", k as u32).chars());
        }
    }
    v.into_iter().collect()
}

// ── Core token helpers ────────────────────────────────────────────────────────

/// Path to `wars_rt::_rexport::alloc`.
pub(crate) fn alloc(core: &OptsCore<'_>) -> TokenStream {
    let p = core.crate_path.clone();
    quote! { #p::_rexport::alloc }
}

/// Path to `wars_rt::func` or `wars_rt::func::unsync` depending on
/// `Flags::ASYNC`.
pub(crate) fn fp(core: &OptsCore<'_>) -> TokenStream {
    let root = core.crate_path.clone();
    if core.flags.contains(Flags::ASYNC) {
        quote! { #root::func::unsync }
    } else {
        quote! { #root::func }
    }
}

// ── WasmTy trait ─────────────────────────────────────────────────────────────

/// Abstraction over a single WebAssembly value type that is sufficient for
/// ABI v0 code generation.
///
/// Implementing this trait is the only thing a backend-specific value-type
/// needs to do in order to use the generic rendering helpers below.
pub(crate) trait WasmTy: Copy {
    /// Is this an `i32` / `u32` in the generated Rust code?
    fn is_i32(self) -> bool;
    /// Is this an `i64` / `u64`?
    fn is_i64(self) -> bool;
    /// Is this `f32`?
    fn is_f32(self) -> bool;
    /// Is this `f64`?
    fn is_f64(self) -> bool;
    /// Is this `v128`?
    fn is_v128(self) -> bool;
    /// Is this a reference type (funcref, externref, …)?
    fn is_ref(self) -> bool;
}

/// Map a single value type (described via `WasmTy`) to the Rust token stream
/// used in ABI v0 signatures.
///
/// `ctx` is the token stream used as the context type parameter (e.g.
/// `quote!{C}`).
pub(crate) fn render_ty<T: WasmTy>(core: &OptsCore<'_>, ctx: &TokenStream, ty: T) -> TokenStream {
    let fp_ts = fp(core);
    if ty.is_i32() {
        quote! { u32 }
    } else if ty.is_i64() {
        quote! { u64 }
    } else if ty.is_f32() {
        quote! { f32 }
    } else if ty.is_f64() {
        quote! { f64 }
    } else if ty.is_v128() {
        quote! { u128 }
    } else {
        // All reference types fall back to Value<C>.
        quote! { #fp_ts::Value<#ctx> }
    }
}

// ── Type description ──────────────────────────────────────────────────────────

/// A wasm function signature over an abstract value type `T`.
#[derive(Clone)]
pub(crate) struct FuncSig<'a, T> {
    pub params: &'a [T],
    pub returns: &'a [T],
}

/// Owned variant of `FuncSig`.
#[derive(Clone)]
pub(crate) struct FuncSigOwned<T> {
    pub params: Vec<T>,
    pub returns: Vec<T>,
}

impl<T: Clone> FuncSigOwned<T> {
    pub fn as_ref(&self) -> FuncSig<'_, T> {
        FuncSig {
            params: &self.params,
            returns: &self.returns,
        }
    }
}

// ── Generic rendering helpers ─────────────────────────────────────────────────

/// Emit the `tuple_list_type!(P0, P1, …), tuple_list_type!(R0, R1, …)` generic
/// arguments for `Df` / `call_ref`.
pub(crate) fn render_generics<T: WasmTy>(
    core: &OptsCore<'_>,
    ctx: &TokenStream,
    sig: FuncSig<'_, T>,
) -> TokenStream {
    let root = core.crate_path.clone();
    let params = sig.params.iter().map(|t| render_ty(core, ctx, *t));
    let returns = sig.returns.iter().map(|t| render_ty(core, ctx, *t));
    quote! {
        #root::_rexport::tuple_list::tuple_list_type!(#(#params),*),
        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*)
    }
}

/// Emit the free-function signature:
///
/// ```rust
/// fn name<'a, C: Base + 'static>(ctx: &'a mut C, tuple_list!(p0, p1): tuple_list_type!(T0, T1))
///     -> BorrowRec<'a, anyhow::Result<tuple_list_type!(R0, R1)>>
/// ```
pub(crate) fn render_fn_sig<T: WasmTy>(
    core: &OptsCore<'_>,
    name: Ident,
    sig: FuncSig<'_, T>,
) -> TokenStream {
    let root = core.crate_path.clone();
    let base = core.name.clone();
    let ctx = quote! { C };
    let params2: Vec<_> = sig.params.iter().map(|t| render_ty(core, &ctx, *t)).collect();
    let param_ids: Vec<_> = sig
        .params
        .iter()
        .enumerate()
        .map(|(i, _)| format_ident!("p{i}"))
        .collect();
    let returns: Vec<_> = sig.returns.iter().map(|t| render_ty(core, &ctx, *t)).collect();
    let mut x = if core.flags.contains(Flags::ASYNC) {
        quote! {
            fn #name<'a, C: #base + 'static>(
                ctx: &'a mut C,
                #root::_rexport::tuple_list::tuple_list!(#(#param_ids),*):
                    #root::_rexport::tuple_list::tuple_list_type!(#(#params2),*)
            ) -> impl #root::func::unsync::UnwrappedAsyncRec<'a,
                    #root::_rexport::anyhow::Result<
                        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*)>>
        }
    } else {
        quote! {
            fn #name<'a, C: #base + 'static>(
                ctx: &'a mut C,
                #root::_rexport::tuple_list::tuple_list!(#(#param_ids),*):
                    #root::_rexport::tuple_list::tuple_list_type!(#(#params2),*)
            ) -> #root::_rexport::tramp::BorrowRec<'a,
                    #root::_rexport::anyhow::Result<
                        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*)>>
        }
    };
    if let Some(t) = core.roots.get("tracing") {
        x = quote! {
            #[#t::instrument]
            #x
        };
    }
    x
}

/// Emit an export method implementation (inside the blanket `impl<C: Foo>
/// FooImpl for C`), delegating to the free function `wrapped`.
pub(crate) fn render_export<T: WasmTy>(
    core: &OptsCore<'_>,
    name: Ident,
    wrapped: Ident,
    sig: FuncSig<'_, T>,
) -> TokenStream {
    let root = core.crate_path.clone();
    let ctx = quote! { Self };
    let params2: Vec<_> = sig.params.iter().map(|t| render_ty(core, &ctx, *t)).collect();
    let param_ids: Vec<_> = sig
        .params
        .iter()
        .enumerate()
        .map(|(i, _)| format_ident!("p{i}"))
        .collect();
    let returns: Vec<_> = sig.returns.iter().map(|t| render_ty(core, &ctx, *t)).collect();
    if core.flags.contains(Flags::ASYNC) {
        quote! {
            fn #name<'a>(
                self: &'a mut Self,
                #root::_rexport::tuple_list::tuple_list!(#(#param_ids),*):
                    #root::_rexport::tuple_list::tuple_list_type!(#(#params2),*)
            ) -> #root::func::unsync::AsyncRec<'a,
                    #root::_rexport::anyhow::Result<
                        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*)>>
            where Self: 'static {
                return #root::func::unsync::AsyncRec::wrap(
                    #wrapped(self, #root::_rexport::tuple_list::tuple_list!(#(#param_ids),*))
                );
            }
        }
    } else {
        quote! {
            fn #name<'a>(
                self: &'a mut Self,
                #root::_rexport::tuple_list::tuple_list!(#(#param_ids),*):
                    #root::_rexport::tuple_list::tuple_list_type!(#(#params2),*)
            ) -> #root::_rexport::tramp::BorrowRec<'a,
                    #root::_rexport::anyhow::Result<
                        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*)>>
            where Self: 'static {
                return #wrapped(self, #root::_rexport::tuple_list::tuple_list!(#(#param_ids),*));
            }
        }
    }
}

/// Emit an export method *declaration* (inside the `FooImpl` trait).
pub(crate) fn render_self_sig_import<T: WasmTy>(
    core: &OptsCore<'_>,
    name: Ident,
    sig: FuncSig<'_, T>,
) -> TokenStream {
    let root = core.crate_path.clone();
    let ctx = quote! { Self };
    let params2: Vec<_> = sig.params.iter().map(|t| render_ty(core, &ctx, *t)).collect();
    let returns: Vec<_> = sig.returns.iter().map(|t| render_ty(core, &ctx, *t)).collect();
    if core.flags.contains(Flags::ASYNC) {
        quote! {
            fn #name<'a>(
                self: &'a mut Self,
                imp: #root::_rexport::tuple_list::tuple_list_type!(#(#params2),*)
            ) -> #root::func::unsync::AsyncRec<'a,
                    #root::_rexport::anyhow::Result<
                        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*)>>
            where Self: 'static;
        }
    } else {
        quote! {
            fn #name<'a>(
                self: &'a mut Self,
                imp: #root::_rexport::tuple_list::tuple_list_type!(#(#params2),*)
            ) -> #root::_rexport::tramp::BorrowRec<'a,
                    #root::_rexport::anyhow::Result<
                        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*)>>
            where Self: 'static;
        }
    }
}

// ── WasmTy impl for wasmparser::ValType ──────────────────────────────────────

#[cfg(feature = "wasmparser")]
impl WasmTy for wasmparser::ValType {
    #[inline] fn is_i32(self) -> bool { matches!(self, wasmparser::ValType::I32) }
    #[inline] fn is_i64(self) -> bool { matches!(self, wasmparser::ValType::I64) }
    #[inline] fn is_f32(self) -> bool { matches!(self, wasmparser::ValType::F32) }
    #[inline] fn is_f64(self) -> bool { matches!(self, wasmparser::ValType::F64) }
    #[inline] fn is_v128(self) -> bool { matches!(self, wasmparser::ValType::V128) }
    #[inline] fn is_ref(self) -> bool { matches!(self, wasmparser::ValType::Ref(_)) }
}

// ── WasmTy impl for waffle::Type ─────────────────────────────────────────────

#[cfg(feature = "waffle")]
impl WasmTy for waffle::Type {
    #[inline] fn is_i32(self) -> bool { matches!(self, waffle::Type::I32) }
    #[inline] fn is_i64(self) -> bool { matches!(self, waffle::Type::I64) }
    #[inline] fn is_f32(self) -> bool { matches!(self, waffle::Type::F32) }
    #[inline] fn is_f64(self) -> bool { matches!(self, waffle::Type::F64) }
    #[inline] fn is_v128(self) -> bool { matches!(self, waffle::Type::V128) }
    #[inline] fn is_ref(self) -> bool { matches!(self, waffle::Type::Heap(_)) }
}
