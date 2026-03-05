//! Helpers shared between the waffle backend and the wasmparser backend.
//!
//! These functions depend only on `OptsCore` plus a thin `FuncSig` description
//! of a wasm function type; they do not touch any backend-specific IR.

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

/// Path to `wars_rt::_rexport::alloc` (so generated code doesn't hard-code
/// the crate name).
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

// ── Type description ──────────────────────────────────────────────────────────

/// A wasm function signature expressed in wasmparser `ValType` terms.
#[derive(Clone)]
pub(crate) struct FuncSig<'a> {
    pub params: &'a [wasmparser::ValType],
    pub returns: &'a [wasmparser::ValType],
}

/// A wasm function signature expressed with wasmparser `ValType` (owned).
#[derive(Clone)]
pub(crate) struct FuncSigOwned {
    pub params: Vec<wasmparser::ValType>,
    pub returns: Vec<wasmparser::ValType>,
}

impl FuncSigOwned {
    pub fn as_ref(&self) -> FuncSig<'_> {
        FuncSig {
            params: &self.params,
            returns: &self.returns,
        }
    }
}

/// Map a single `wasmparser::ValType` to the Rust type used in ABI v0 signatures.
///
/// `ctx` is the token stream used as the context type parameter (e.g. `quote!{C}`).
pub(crate) fn render_ty(core: &OptsCore<'_>, ctx: &TokenStream, ty: wasmparser::ValType) -> TokenStream {
    use wasmparser::{HeapType, RefType, ValType};
    let root = core.crate_path.clone();
    let fp_ts = fp(core);
    match ty {
        ValType::I32 => quote! { u32 },
        ValType::I64 => quote! { u64 },
        ValType::F32 => quote! { f32 },
        ValType::F64 => quote! { f64 },
        ValType::V128 => quote! { u128 },
        ValType::Ref(r) => {
            // funcref with a concrete signature → typed Df<P,R,C>
            // all other ref types → Value<C>
            match r.heap_type() {
                HeapType::Concrete(_) | HeapType::Abstract { .. } => {
                    // We can only lower typed funcrefs when we have a full
                    // ParsedModule; fall back to Value<C> here (the caller
                    // can specialise further if needed).
                    quote! { #fp_ts::Value<#ctx> }
                }
                _ => quote! { #fp_ts::Value<#ctx> },
            }
        }
    }
}

/// Emit the `tuple_list_type!(P0, P1, …), tuple_list_type!(R0, R1, …)` generic
/// arguments for `Df` / `call_ref`.
pub(crate) fn render_generics(core: &OptsCore<'_>, ctx: &TokenStream, sig: FuncSig<'_>) -> TokenStream {
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
pub(crate) fn render_fn_sig(core: &OptsCore<'_>, name: Ident, sig: FuncSig<'_>) -> TokenStream {
    let root = core.crate_path.clone();
    let base = core.name.clone();
    let ctx = quote! { C };
    let params2: Vec<_> = sig.params.iter().map(|t| render_ty(core, &ctx, *t)).collect();
    let param_ids: Vec<_> = sig.params.iter().enumerate().map(|(i, _)| format_ident!("p{i}")).collect();
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
pub(crate) fn render_export(
    core: &OptsCore<'_>,
    name: Ident,
    wrapped: Ident,
    sig: FuncSig<'_>,
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
pub(crate) fn render_self_sig_import(
    core: &OptsCore<'_>,
    name: Ident,
    sig: FuncSig<'_>,
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
