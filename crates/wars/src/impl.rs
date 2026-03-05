use super::*;
use relooper::{reloop, BranchMode, ShapedBlock};
use waffle::{
    cfg::CFGInfo, entity::EntityRef, frontend::ModuleExt, passes, Block, BlockTarget, Export,
    ExportKind, Func, FunctionBody, HeapType, ImportKind, Memory, Module, Operator, Signature,
    SignatureData, Terminator, Type, Value, WithNullable,
};
use waffle_passes_shared::maxssa;
pub(crate) fn mangle_value(a: Value, b: usize) -> Ident {
    if b == 0 {
        format_ident!("{a}")
    } else {
        format_ident!("{a}p{}", b - 1)
    }
}
pub(crate) use crate::shared::bindname;

type Opts<'a> = OptsLt<'a, Module<'static>, LegacyPortalWaffleBackend>;

pub(crate) fn alloc(opts: &Opts<'_>) -> TokenStream {
    crate::shared::alloc(&opts.core)
}
pub(crate) fn fp(opts: &Opts<'_>) -> TokenStream {
    crate::shared::fp(&opts.core)
}
pub(crate) fn host_tpit(opts: &Opts<'_>) -> TokenStream {
    match opts.core.roots.get("tpit_rt") {
        None => quote! {
            ::core::convert::Infallible
        },
        Some(r) => quote! {
            #r::Tpit<()>
        },
    }
}
pub(crate) fn mem(opts: &Opts<'_>, m: Memory) -> anyhow::Result<TokenStream> {
    if let Some(i) = opts
        .module
        .imports
        .iter()
        .find(|x| x.kind == ImportKind::Memory(m))
    {
        // if i.module == "!!unsafe" && i.name == "host" && self.flags.contains(Flags::HOST_MEMORY)
        // {
        //     return quote! {
        //         unsafe{
        //             ::std::slice::from_raw_parts_mut(::std::ptr::null(),usize::MAX)
        //         }
        //     };
        // }
        for p in opts.core.plugins.iter() {
            if let Some(i) = p.mem_import(&opts.core, &i.module, &i.name)? {
                return Ok(quasiquote!(#{i.expr}));
            }
        }
        // if self.flags.contains(Flags::WASIX) {
        //     if i.module == "wasi_snapshot_preview1" || i.module == "wasix_32v1" {
        //         if i.name == "memory" {
        //             return quote! {
        //                 ctx.wasix_memory()
        //             };
        //         }
        //     }
        // }
    }
    let m2 = format_ident!("{m}");
    Ok(quote! {
        ctx.#m2()
    })
}
pub(crate) fn import(
    opts: &Opts<'_>,
    module: &str,
    name: &str,
    mut params: impl Iterator<Item = TokenStream>,
) -> anyhow::Result<TokenStream> {
    let params = params.collect::<Vec<_>>();
    for pl in opts.core.plugins.iter() {
        if let Some(a) = pl.import(&opts.core, module, name, params.clone())? {
            return Ok(a);
        }
    }
    let mut params = params.into_iter();
    let root = opts.core.crate_path.clone();
    // if self.flags.contains(Flags::UNSANDBOXED) {
    // if self.flags.contains(Flags::HOST_MEMORY) {
    //     if let Some(a) = module.strip_prefix("!!unsafe/"){
    //         if a == "linux"{
    //             if let Some(s) = name.strip_prefix("syscall"){
    //                 if let Some((s,_)) = s.split_once("/"){
    //                     if let Some(l) = self.roots.get("linux-syscall"){
    //                         return quasiquote! {
    //                             #{fp(opts)}::ret(match #l::syscall!(#l::#{format_ident!("SYS_{s}")}, #(#params),*).try_usize(){
    //                                 Ok(a) => Ok(a as u64),
    //                                 Err(e) => Err(e.into())
    //                             })
    //                         };
    //                     }
    //                 }
    //             }
    //         }
    //     }
    // }
    // if self.flags.contains(Flags::WASIX) {
    //     if module == "wasi_snapshot_preview1" || module == "wasix_32v1" {
    //         return quasiquote! {
    //             #root::wasix::#{format_ident!("{name}")}(#(#params),*)
    //         };
    //     }
    // }
    // if self.flags.contains(Flags::BIND) {
    //     if module == "wars/bind" {
    //         if name == "!!drop" {
    //             return quasiquote! {
    //                 {
    //                     let v = self.data().rust_table.remove(#(#params),*).unwrap();
    //                     Ok(())
    //                 }
    //             };
    //         }
    //         let params = params.collect::<Vec<_>>();
    //         return quasiquote! {
    //             {
    //                 let v = #{syn::parse_str::<TokenStream>(&name).unwrap()}(#(unsafe{#root::get_cell(self.data().rust_table.get(&#params)).unwrap()}),*)#{if self.flags.contains(Flags::ASYNC){
    //                     quote!{.await}
    //                 }else{
    //                     quote!{}
    //                 }};
    //                 v.map(|x|(alloc(&mut self.data().rust_table,#root::any_cell(x)),()))
    //             }
    //         };
    //     }
    // }
    //     if a == "fs" {
    //         match name {
    //             "open" => {
    //                 let p0 = params.next().unwrap();
    //                 let l = params.next().unwrap();
    //                 return quote! {
    //                     #root::_rexport::tramp::BorrowRec::Ret({
    //                         let f = #p0;
    //                         let l = #l;
    //                         let f = ::std::str::from_utf8(&ctx.memory()[(f as usize)..][..(l as usize)]);
    //                         let f = match f{
    //                             Ok(a) => a,
    //                             Err(e) => return #root::_rexport::tramp::BorrowRec::Ret(Er(e.into()));
    //                         };
    //                         let f = ::std::fs::open(f);
    //                         let f = match f{
    //                             Ok(a) =>  alloc(&mut ctx.data().files,::std::sync::Arc::new(a)) * 2,
    //                             Err(e) => alloc(&mut ctx.data().io_errors,::std::sync::Arc::new(e)) * 2 + 1;
    //                         };
    //                         f
    //                     })
    //                 };
    //             }
    //             "read" => {
    //                 let fd = params.next().unwrap();
    //                 let p0 = params.next().unwrap();
    //                 let l = params.next().unwrap();
    //                 return quote! {
    //                     {
    //                         let f = #p0;
    //                         let l = #l;
    //                         let fd = #fd;
    //                         let fd = ctx.data().files.get(&fd).unwrap().clone();
    //                         let f = &mut ctx.memory()[(f as usize)..][..(l as usize)];
    //                         let f = ::std::io::Read::read(&mut fd.as_ref(),f);
    //                         match f{
    //                             Ok(a) =>                               #root::_rexport::tuple_list::tuple_list!(a as u64 * 2),
    //                             Err(e) => #root::_rexport::tuple_list::tuple_list!(alloc(&mut ctx.data().io_errors,::std::sync::Arc::new(e)) as u64 * 2 + 1);
    //                         }
    //                     }
    //                 };
    //             }
    //             "write" => {
    //                 let fd = params.next().unwrap();
    //                 let p0 = params.next().unwrap();
    //                 let l = params.next().unwrap();
    //                 return quote! {
    //                     {
    //                         let f = #p0;
    //                         let l = #l;
    //                         let fd = #fd;
    //                         let fd = ctx.data().files.get(&fd).unwrap().clone();
    //                         let f = &ctx.memory()[(f as usize)..][..(l as usize)];
    //                         let f = ::std::io::Write::write(&mut fd.as_ref(),f);
    //                         match f{
    //                             Ok(a) =>                               #root::_rexport::tuple_list::tuple_list!(a as u64 * 2),
    //                             Err(e) => #root::_rexport::tuple_list::tuple_list!(alloc(&mut ctx.data().io_errors,::std::sync::Arc::new(e)) as u64 * 2 + 1);
    //                         }
    //                     }
    //                 };
    //             }
    //             _ => {}
    //         }
    //     }
    // }
    // if self.flags.contains(Flags::PIT) {
    // };
    let id = format_ident!("{}_{}", bindname(module), bindname(name));
    return Ok(quote! {
        ctx.#id(#root::_rexport::tuple_list::tuple_list!(#(#params),*))
    });
}
pub(crate) fn render_ty(opts: &Opts<'_>, ctx: &TokenStream, ty: Type) -> TokenStream {
    // Handle the typed-funcref case that needs module-level signature lookup —
    // the generic shared::render_ty can't do this because it has no module access.
    if let Type::Heap(WithNullable {
        nullable,
        value: HeapType::Sig { sig_index },
    }) = ty
    {
        if let SignatureData::Func { params, returns, .. } =
            &opts.module.signatures[sig_index]
        {
            let root = opts.core.crate_path.clone();
            let params = params.iter().map(|x| render_ty(opts, ctx, *x));
            let returns = returns.iter().map(|x| render_ty(opts, ctx, *x));
            let mut x = if opts.core.flags.contains(Flags::ASYNC) {
                quote! {
                    #root::func::unsync::Df<
                        #root::_rexport::tuple_list::tuple_list_type!(#(#params),*),
                        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*),
                        #ctx>
                }
            } else {
                quote! {
                    #root::func::Df<
                        #root::_rexport::tuple_list::tuple_list_type!(#(#params),*),
                        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*),
                        #ctx>
                }
            };
            if nullable {
                x = quote! { Option<#x> };
            }
            return x;
        }
    }
    // Everything else goes through the generic shared helper.
    crate::shared::render_ty(&opts.core, ctx, ty)
}

/// Convert a `&SignatureData` into a `FuncSig<'_, Type>` for use with
/// the generic `shared::` helpers.
fn sig_to_funcsig(data: &SignatureData) -> crate::shared::FuncSig<'_, Type> {
    match data {
        SignatureData::Func { params, returns, .. } => crate::shared::FuncSig {
            params,
            returns,
        },
        _ => crate::shared::FuncSig { params: &[], returns: &[] },
    }
}

pub(crate) fn render_generics(
    opts: &Opts<'_>,
    ctx: &TokenStream,
    data: &SignatureData,
) -> TokenStream {
    // render_generics uses render_ty internally; we must pass our local
    // render_ty (which resolves typed funcrefs) rather than shared's.
    // So we inline the expansion here rather than calling shared::render_generics.
    let sig = sig_to_funcsig(data);
    let root = opts.core.crate_path.clone();
    let params = sig.params.iter().map(|x| render_ty(opts, ctx, *x));
    let returns = sig.returns.iter().map(|x| render_ty(opts, ctx, *x));
    quote! {
        #root::_rexport::tuple_list::tuple_list_type!(#(#params),*),
        #root::_rexport::tuple_list::tuple_list_type!(#(#returns),*)
    }
}

pub(crate) fn render_fn_sig(opts: &Opts<'_>, name: Ident, data: &SignatureData) -> TokenStream {
    // render_fn_sig uses render_ty internally; inline for the same reason as render_generics.
    let sig = sig_to_funcsig(data);
    let root = opts.core.crate_path.clone();
    let base = opts.core.name.clone();
    let ctx = quote! { C };
    let params2: Vec<_> = sig.params.iter().map(|x| render_ty(opts, &ctx, *x)).collect();
    let param_ids: Vec<_> = sig.params.iter().enumerate().map(|(a, _)| format_ident!("p{a}")).collect();
    let returns: Vec<_> = sig.returns.iter().map(|x| render_ty(opts, &ctx, *x)).collect();
    let mut x = if opts.core.flags.contains(Flags::ASYNC) {
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
    if let Some(t) = opts.core.roots.get("tracing") {
        x = quote! { #[#t::instrument] #x };
    }
    x
}

pub(crate) fn fname(opts: &Opts<'_>, a: Func) -> Ident {
    format_ident!("{a}_{}", bindname(opts.module.funcs[a].name()))
}
pub(crate) fn render_fun_ref(opts: &Opts<'_>, ctx: &TokenStream, x: Func) -> TokenStream {
    let root = opts.core.crate_path.clone();
    if x.is_invalid() {
        return quasiquote! {
            #{fp(opts)}::da::<(),(),C,_>(|ctx,arg|panic!("invalid func"))
        };
    }
    let generics =
        render_generics(opts, ctx, &opts.module.signatures[opts.module.funcs[x].sig()]);
    let x = fname(opts, x);
    let r = if opts.core.flags.contains(Flags::ASYNC) {
        quasiquote!(#root::func::unsync::AsyncRec::wrap(res))
    } else {
        quasiquote!(res)
    };
    quasiquote! {
        #{fp(opts)}::da::<#generics,C,_>(|ctx,arg|match #x(ctx,arg){
            res => #r
        })
    }
}
pub(crate) fn render_self_sig(
    opts: &Opts<'_>,
    name: Ident,
    wrapped: Ident,
    data: &SignatureData,
) -> TokenStream {
    render_export(opts, name, wrapped, data)
}
pub(crate) fn render_export(
    opts: &Opts<'_>,
    name: Ident,
    wrapped: Ident,
    data: &SignatureData,
) -> TokenStream {
    let sig = sig_to_funcsig(data);
    let root = opts.core.crate_path.clone();
    let ctx = quote! { Self };
    let params2: Vec<_> = sig.params.iter().map(|x| render_ty(opts, &ctx, *x)).collect();
    let param_ids: Vec<_> = sig.params.iter().enumerate()
        .map(|(a, _)| format_ident!("p{a}"))
        .collect::<Vec<_>>();
    let returns: Vec<_> = sig.returns.iter().map(|x| render_ty(opts, &ctx, *x)).collect();
    if opts.core.flags.contains(Flags::ASYNC) {
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
pub(crate) fn render_self_sig_import(
    opts: &Opts<'_>,
    name: Ident,
    data: &SignatureData,
) -> TokenStream {
    let sig = sig_to_funcsig(data);
    let root = opts.core.crate_path.clone();
    let ctx = quote! { Self };
    let params2: Vec<_> = sig.params.iter().map(|x| render_ty(opts, &ctx, *x)).collect();
    let returns: Vec<_> = sig.returns.iter().map(|x| render_ty(opts, &ctx, *x)).collect();
    if opts.core.flags.contains(Flags::ASYNC) {
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
fn render_statements(
    opts: &Opts<'_>,
    f: &Func,
    b: &FunctionBody,
    stmts: Block,
) -> anyhow::Result<Vec<TokenStream>> {
    let root = opts.core.crate_path.clone();
    let fp_ts = fp(opts);
    let stmts = b.blocks[stmts].params.iter().map(|a|a.1).chain(b.blocks[stmts].insts.iter().filter_map(|a|a.pure_core())).map(|a|{
        let av = b.values[a].tys(&b.type_pool).iter().enumerate().map(|b|mangle_value(a,b.0));
        let b = match &b.values[a]{
            waffle::ValueDef::BlockParam(k, i, _) => {
                let a = format_ident!("{k}param{i}");
                quote! {
                    #root::_rexport::tuple_list::tuple_list!(#a.clone())
                }
            },
            waffle::ValueDef::Operator(o, vals, _) => {
                let vals = &b.arg_pool[*vals];
                match o{
                    Operator::I32Const { value } => quote! {
                        #root::_rexport::tuple_list::tuple_list!(#value)
                    },
                    Operator::I64Const { value } => quote!{
                        #root::_rexport::tuple_list::tuple_list!(#value)
                    },
                    Operator::Call { function_index } => {
                        match opts.module.funcs[*function_index].body(){
                            Some(_) => {
                                let func = fname(opts, *function_index);
                                let vals = vals.iter().map(|a|format_ident!("{a}"));
                                quasiquote! {
                                    {
                                        let x = #func(ctx,#root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#vals .clone())),*));
                                        match #{if opts.core.flags.contains(Flags::ASYNC){
                                            quote!{
                                                x.go().await
                                            }
                                        }else{
                                            quote!{
                                                #root::_rexport::tramp::tramp(x)
                                            }
                                        }}{
                                            Ok(a) => a,
                                            Err(e) => return #{fp(opts)}::ret(Err(e))
                                        }
                                    }
                                }
                            },
                            None => {
                                let i = opts
                                .module
                                .imports
                                .iter()
                                .find(|a| a.kind == ImportKind::Func(*function_index))
                                .unwrap();
                            let x = import(
                                opts,
                                i.module.as_str(),
                                i.name.as_str(),
                                vals.iter().map(|a|format_ident!("{a}")).map(|a| quote! {#a}),
                            )?;
                            quasiquote!{
                                match #{if opts.core.flags.contains(Flags::ASYNC){
                                    quasiquote!{
                                        #{alloc(opts)}::boxed::Box::pin(#x.go()).await
                                    }
                                }else{
                                    quote!{
                                        #root::_rexport::tramp::tramp(#x)
                                    }
                                }}{
                                    Ok(a) => a,
                                    Err(e) => return #{fp(opts)}::ret(Err(e))
                                }
                            }
                            }
                        }
                    },
                    Operator::CallRef { sig_index } => {
                        let mut vals = vals.to_owned();
                        let r = vals.pop().expect(" a ref to call");
                            // let func = format_ident!("{function_index}");
                            let vals = vals.iter().map(|a|format_ident!("{a}"));
                            let r = format_ident!("{r}");
                            let g = render_generics(opts, &quote! {c}, &opts.module.signatures[*sig_index]);
                            quasiquote! {
                                {
                                let x = #{fp(opts)}::call_ref::<#g,C>(ctx,#{fp(opts)}(#r.clone()),#root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#vals .clone())),*));
                                match #{if opts.core.flags.contains(Flags::ASYNC){
                                    quote!{
                                        x.go().await
                                    }
                                }else{
                                    quote!{
                                        #root::_rexport::tramp::tramp(x)
                                    }
                                }}{
                                    Ok(a) => a,
                                    Err(e) => return #{fp(opts)}::ret(Err(e))
                                }
                            }
                            }
                    },
                    Operator::CallIndirect { sig_index, table_index } => {
                        let t = format_ident!("{table_index}");
                        let mut vals = vals.to_owned();
                        let r = vals.pop().expect("a table index to call");
                            // let func = format_ident!("{function_index}");
                            let vals = vals.iter().map(|a|format_ident!("{a}"));
                            let r = format_ident!("{r}");
                            let r = quote! {
                                ctx.#t()[#r as usize]
                            };
                            let g = render_generics(opts, &quote! {c}, &opts.module.signatures[*sig_index]);
                            quasiquote! {
                                {
                                let r = #r.clone();
                                let x = #{fp(opts)}::call_ref::<#g,C>(ctx,#{fp(opts)}::cast(r),#root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#vals .clone())),*));
                                match #{if opts.core.flags.contains(Flags::ASYNC){
                                    quote!{
                                        x.go().await
                                    }
                                }else{
                                    quote!{
                                        #root::_rexport::tramp::tramp(x)
                                    }
                                }}{
                                    Ok(a) => a,
                                    Err(e) => return #{fp(opts)}::ret(Err(e))
                                }
                            }
                            }
                    },
                    Operator::RefFunc { func_index } => {
                        render_fun_ref(opts, &quote! {C},*func_index)
                    },
                    waffle::Operator::MemorySize { mem: mem_idx } => {
                        let rt = if opts.module.memories[*mem_idx].memory64{
                            quote! {u64}
                        }else{
                            quote! {u32}
                        };
                        let n = match &opts.module.memories[*mem_idx].page_size_log2{
                            None => 65536usize,
                            Some(a) => 2usize.pow(*a)
                        };
                        let m = Ident::new(&mem_idx.to_string(), Span::call_site());
                        quasiquote! {
                            #root::_rexport::tuple_list::tuple_list!(((match #root::Memory::size(ctx.#m()){
                                Ok(a) => a,
                                Err(e) => return #{fp(opts)}::ret(Err(e))
                            }) / #n) as #rt)
                        }
                    }
                    waffle::Operator::MemoryGrow { mem: mem_idx } => {
                        let m = Ident::new(&mem_idx.to_string(), Span::call_site());
                        let a = vals[0];
                        let a = format_ident!("{a}");
                        let rt = if opts.module.memories[*mem_idx].memory64{
                            quote! {u64}
                        }else{
                            quote! {u32}
                        };
                        let n = match &opts.module.memories[*mem_idx].page_size_log2{
                            None => 65536usize,
                            Some(a) => 2usize.pow(*a)
                        };
                        quasiquote! {
                            {
                            let vn = (match #root::Memory::size(ctx.#m()){
                                Ok(a) => a,
                                Err(e) => return #{fp(opts)}::ret(Err(e))
                            }) / #n;
                            match #root::Memory::grow(ctx.#m(),(#a .clone() as u64) * #n){
                                Ok(a) => a,
                                Err(e) => return #{fp(opts)}::ret(Err(e))
                            };
                            #root::_rexport::tuple_list::tuple_list!(vn as #rt)
                            }
                        }
                    },
                    waffle::Operator::MemoryCopy { dst_mem, src_mem } => {
                        let dst = mem(opts, *dst_mem)?;
                        let src = mem(opts, *src_mem)?;
                        let dst_ptr = format_ident!("{}",vals[0].to_string());
                        let src_ptr = format_ident!("{}",vals[1].to_string());
                        let len = format_ident!("{}",vals[2].to_string());
                        quasiquote!{
                            {
                                let m = match #src.read(#src_ptr as u64,#len as u64){
                                    Ok(a) => a,
                                    Err(e) => return #{fp(opts)}::ret(Err(e))
                                }.as_ref().as_ref().to_owned();
                                match #dst.write(#dst_ptr as u64,&m){
                                    Ok(a) => a,
                                    Err(e) => return #{fp(opts)}::ret(Err(e))
                                };
                            ()
                            }
                        }
                    },
                    waffle::Operator::MemoryFill { mem: mem_idx } => {
                        let dst = mem(opts, *mem_idx)?;
                        // let src = mem(opts, *src_mem);
                        let dst_ptr = format_ident!("{}",vals[0].to_string());
                        let val = format_ident!("{}",vals[1].to_string());
                        let len = format_ident!("{}",vals[2].to_string());
                        quasiquote!{
                            {
                                let m = #{alloc(opts)}::vec![(#val & 0xff) as u8; #len as usize];
                                match #dst.write(#dst_ptr as u64,&m){
                                    Ok(a) => a,
                                    Err(e) => return #{fp(opts)}::ret(Err(e))
                                };
                            ()
                            }
                        }
                    },
                    waffle::Operator::GlobalGet { global_index } => {
                        let g = Ident::new(&global_index.to_string(), Span::call_site());
                        quote!{
                            #root::_rexport::tuple_list::tuple_list!(*ctx.#g())
                        }
                    }
                    waffle::Operator::GlobalSet { global_index } => {
                        let g = Ident::new(&global_index.to_string(), Span::call_site());
                        let r = vals[0];
                        let r = format_ident!("{r}");
                        quote!{
                            {
                                *ctx.#g() = #r;
                                ()
                            }
                        }
                    }
                    Operator::TableGet { table_index } => {
                        let table = format_ident!("{table_index}");
                        let [i,..] = vals else{
                            unreachable!()
                        };
                        let i = format_ident!("{i}");
                        // let j = format_ident!("{j}");
                        quasiquote!{
                            {
                            (ctx.#table()[#i as usize].clone(),())
                            }
                        }
                    },
                    Operator::TableSet { table_index } => {
                        let table = format_ident!("{table_index}");
                        let [i,j,..] = vals else{
                            unreachable!()
                        };
                        let i = format_ident!("{i}");
                        let j = format_ident!("{j}");
                        quasiquote!{
                            {
                            ctx.#table()[#i as usize] = #{fp(opts)}::cast::<_,_,C>(#j.clone());
                            ()
                            }
                        }
                    },
                    Operator::TableSize { table_index } => {
                        let table = format_ident!("{table_index}");
                        quote!{
                            (ctx.#table().len() as u32,())
                        }
                    },
                    Operator::TableGrow { table_index } => {
                        let table = format_ident!("{table_index}");
                        let [i,j,..] = vals else{
                            unreachable!()
                        };
                        let i = format_ident!("{i}");
                        let j = format_ident!("{j}");
                        quasiquote!{
                            {
                                for _ in 0..#i{
                                    ctx.#table().push(#{fp(opts)}::cast::<_,_,C>(#j.clone()));
                                }
                                ()
                            }
                        }
                    },
                    Operator::StructNew { sig } => {
                        let vals = vals.iter().zip(match &opts.module.signatures[*sig]{
                            SignatureData::Struct { fields,.. } => fields.iter(),
                            _ => anyhow::bail!("not a struct")
                        }).map(|(v,f)|quasiquote!{
                            #root::gc::#{if f.mutable{
                                quote!{Mut}
                            }else{
                                quote!{Const}
                            }}(#{format_ident!("{v}")})
                        });
                        quasiquote!{
                            #{fp(opts)}::cast::<_,_,C>(#root::gc::Struct(#root::_rexport::tuple_list::tuple_list!(#(#vals),*)))
                        }
                    }
                    Operator::StructGet { sig, idx } => {
                        let [i,..] = vals else{
                            unreachable!()
                        };
                        let i = format_ident!("{i}");
                        quasiquote!{
                            {fp(opts)}::cast::<_,_,C>(match #i.clone().0{
                                #{fp(opts)}::value::Value::Gc(g) => g.get_field(#idx),
                                _ => todo!()
                            })
                        }
                    }
                    Operator::StructSet { sig, idx } => {
                        let [i,j,..] = vals else{
                            unreachable!()
                        };
                        let i = format_ident!("{i}");
                        let j = format_ident!("{j}");
                        quasiquote!{
                            match #i.clone().0{
                                #{fp(opts)}::value::Value::Gc(g) => g.set_field(#idx,#{fp(opts)}::cast::<_,_,C>(#j.clone())),
                                _ => todo!()
                            }
                        }
                    }
                    _ if waffle::op_traits::mem_count(o) == 1 => {
                        let mut mem_idx = Memory::invalid();
                        waffle::op_traits::rewrite_mem(&mut o.clone(), &mut [();4], |m,_|{
                            mem_idx = *m;
                            Ok::<(),Infallible>(())
                        }).expect("wut");
                        // let clean = o.to_string();
                        let clean = format_ident!("{}",o.to_string().split_once("<").expect("a memory op").0);
                        let m2 = mem_idx;
                        let mem_tok = mem(opts, m2)?;
                        let mut vals = vals.iter().map(|a|format_ident!("{a}"));
                        let rt = if opts.module.memories[m2].memory64{
                            quote! {u64}
                        }else{
                            quote! {u32}
                        };
                        let offset = waffle::op_traits::memory_arg(o).expect(&format!("a memory arg from {}",o)).offset;
                        let offset =  if opts.module.memories[m2].memory64{
                            quote! {#offset}
                        } else{
                            let offset = offset as u32;
                            quote! {#offset}
                        };
                        let val = vals.next().expect("the runtime memory offset");
                        let vals = once(quote! {(#val.clone() + #offset)}).chain(vals.map(|w|quote!{#w}));
                        quasiquote! {
                            match #root::#clean::<#rt,_>(#mem_tok,#(#fp_ts::cast::<_,_,C>(#vals .clone())),*){
                                Ok(a) => a,
                                Err(e) => return #{fp(opts)}::ret(Err(e))
                            }
                        }
                    },
                    Operator::Select | Operator::TypedSelect { .. } => {
                        let vals: Vec<_> = vals.iter().map(|a|format_ident!("{a}")).collect();
                        let cond = vals[0].clone();
                        let then = vals[1].clone();
                        let els = vals[2].clone();
                        quote!{
                            #root::_rexport::tuple_list::tuple_list!(if #cond != 0{
                                #then
                            }else{
                                #els.into()
                            })
                        }
                    },
                    _ => {
                        // let clean = o.to_string();
                        let clean = format_ident!("{o}");
                        let vals = vals.iter().map(|a|format_ident!("{a}"));
                        quasiquote! {
                            match #root::#clean(#(#fp_ts::cast::<_,_,C>(#vals .clone())),*){
                                Ok(a) => a,
                                Err(e) => return #{fp(opts)}::ret(Err(e))
                            }
                        }
                    }
                }
            },
            waffle::ValueDef::PickOutput(w, i, _) => {
                let w = mangle_value(*w, *i as usize);
                quote! {
                    #root::_rexport::tuple_list::tuple_list!(#w)
                }
            },
            waffle::ValueDef::Alias(w) => {
                let w = format_ident!("{w}");
                quote! {
                    #root::_rexport::tuple_list::tuple_list!(#w)
                }
            },
            waffle::ValueDef::Placeholder(_) => todo!(),
            // waffle::ValueDef::Trace(_, _) => todo!(),
            waffle::ValueDef::None => todo!(),
        };
        anyhow::Ok(quote! {
            let #root::_rexport::tuple_list::tuple_list!(#(#av),*) = #b
        })
    }).collect::<anyhow::Result<Vec<_>>>()?;
    return Ok(stmts);
}
fn render_term(
    opts: &Opts<'_>,
    f: Func,
    b: &FunctionBody,
    k: Block,
    render_target: &impl Fn(&BlockTarget) -> TokenStream,
) -> anyhow::Result<TokenStream> {
    let root = opts.core.crate_path.clone();
    Ok(match &b.blocks[k].terminator.terminator {
        waffle::Terminator::Br { target } => render_target(target),
        waffle::Terminator::CondBr {
            cond,
            if_true,
            if_false,
        } => {
            let if_true = render_target(if_true);
            let if_false = render_target(if_false);
            let cond = format_ident!("{cond}");
            quote! {
                if #cond != 0{
                    #if_true
                }else{
                    #if_false
                }
            }
        }
        waffle::Terminator::Select {
            value,
            targets,
            default,
        } => {
            let value = format_ident!("{value}");
            let default = render_target(default);
            let targets = targets
                .iter()
                .map(&render_target)
                .enumerate()
                .map(|(a, b)| {
                    quote! {
                        #a => {#b}
                    }
                });
            quote! {
                match #value as usize{
                    #(#targets),*,
                    _ => {#default},
                }
            }
        }
        waffle::Terminator::Return { values } => {
            // let values = values.iter().map(|v| format_ident!("{v}"));
            let values = b.rets.iter().enumerate().map(|(a, _)| match values.get(a) {
                Some(v) => {
                    let v = format_ident!("{v}");
                    quasiquote! {
                        #{fp(opts)}::cast::<_,_,C>(#v)
                    }
                }
                None => {
                    quote! {
                        ::core::default::Default::default()
                    }
                }
            });
            quasiquote! {
                return #{fp(opts)}::ret(Ok(#root::_rexport::tuple_list::tuple_list!(#(#values),*)))
            }
        }
        waffle::Terminator::ReturnCall { func, args } => {
            match opts.module.funcs[*func].body() {
                Some(_) => {
                    let values = args.iter().map(|v| format_ident!("{v}")).map(|a| {
                        quasiquote! {
                            #{fp(opts)}::cast::<_,_,C>(#a)
                        }
                    });
                    let func = fname(opts, *func);
                    if opts.core.flags.contains(Flags::ASYNC) {
                        quote! {
                            #func(ctx,#root::_rexport::tuple_list::tuple_list!(#(#values),*))
                        }
                    } else {
                        quote! {
                            return #root::_rexport::tramp::BorrowRec::Call(#root::_rexport::tramp::Thunk::new(move||{
                                #func(ctx,#root::_rexport::tuple_list::tuple_list!(#(#values),*))
                            }))
                        }
                    }
                }
                None => {
                    let i = opts
                        .module
                        .imports
                        .iter()
                        .find(|a| a.kind == ImportKind::Func(*func))
                        .unwrap();
                    let x = import(
                        opts,
                        i.module.as_str(),
                        i.name.as_str(),
                        args.iter()
                            .map(|a| format_ident!("{a}"))
                            .map(|a| quote! {#a}),
                    )?;
                    if opts.core.flags.contains(Flags::ASYNC) {
                        x
                    } else {
                        quote! {
                            return #root::_rexport::tramp::BorrowRec::Call(#root::_rexport::tramp::Thunk::new(move||{#x}))
                        }
                    }
                }
            }
        }
        waffle::Terminator::ReturnCallIndirect { sig, table, args } => {
            let t = format_ident!("{table}");
            let mut vals = args.to_owned();
            let r = vals.pop().expect("a table index to call");
            // let func = format_ident!("{function_index}");
            let vals = vals.iter().map(|a| format_ident!("{a}")).map(|a| {
                quasiquote! {
                    #{fp(opts)}::cast::<_,_,C>(#a)
                }
            });
            let r = format_ident!("{r}");
            let r = quote! {
                ctx.#t()[#r as usize]
            };
            let g = render_generics(opts, &quote! {c}, &opts.module.signatures[*sig]);
            if opts.core.flags.contains(Flags::ASYNC) {
                quasiquote! {
                    return #{fp(opts)}::call_ref::<#g,C>(ctx,#{fp(opts)}::cast(r),#root::_rexport::tuple_list::tuple_list!(#(#{fp(opts)}::cast::<_,_,C>(#vals .clone())),*))
                }
            } else {
                quasiquote! {
                        let r = #r.clone();
                        return #root::_rexport::tramp::BorrowRec::Call(#root::_rexport::tramp::Thunk::new(move||{
                        #{fp(opts)}::call_ref::<#g,C>(ctx,#{fp(opts)}::cast(r),#root::_rexport::tuple_list::tuple_list!(#(#{fp(opts)}::cast::<_,_,C>(#vals .clone())),*))
                    }))
                }
            }
        }
        waffle::Terminator::ReturnCallRef { sig, args } => {
            let mut vals = args.clone();
            let r = vals.pop().expect(" a ref to call");
            // let func = format_ident!("{function_index}");
            let vals = vals.iter().map(|a| format_ident!("{a}")).map(|a| {
                quasiquote! {
                    #{fp(opts)}::cast::<_,_,C>(#a)
                }
            });
            let r = format_ident!("{r}");
            let g = render_generics(opts, &quote! {c}, &opts.module.signatures[*sig]);
            if opts.core.flags.contains(Flags::ASYNC) {
                quasiquote! {
                    return #{fp(opts)}::call_ref::<#g,C>(ctx,#root::func::cast(#r.clone()),#root::_rexport::tuple_list::tuple_list!(#(#root::func::cast::<_,_,C>(#vals .clone())),*))
                }
            } else {
                quasiquote! {
                        return #root::_rexport::tramp::BorrowRec::Call(#root::_rexport::tramp::Thunk::new(move||{
                        #{fp(opts)}::call_ref::<#g,C>(ctx,#root::func::cast(#r.clone()),#root::_rexport::tuple_list::tuple_list!(#(#root::func::cast::<_,_,C>(#vals .clone())),*))
                    }))
                }
            }
        }
        waffle::Terminator::Unreachable => quote! {
            unreachable!()
        },
        waffle::Terminator::None => panic!("none block terminator"),
        _ => todo!(),
    })
}
pub(crate) fn render_relooped_block(
    opts: &Opts<'_>,
    f: Func,
    x: &ShapedBlock<Block>,
) -> anyhow::Result<TokenStream> {
    let root = opts.core.crate_path.clone();
    let b = opts.module.funcs[f].body().unwrap();
    Ok(match x {
        ShapedBlock::Simple(s) => {
            let stmts = s.label;
            fn term(b: &BranchMode) -> TokenStream {
                match b {
                    relooper::BranchMode::LoopBreak(l) => {
                        let l = Lifetime::new(&format!("'l{}", l), Span::call_site());
                        quote! {
                            break #l;
                        }
                    }
                    relooper::BranchMode::LoopBreakIntoMulti(l) => {
                        let l = Lifetime::new(&format!("'l{}", l), Span::call_site());
                        quote! {
                            break #l;
                        }
                    }
                    relooper::BranchMode::LoopContinue(l) => {
                        let l = Lifetime::new(&format!("'l{}", l), Span::call_site());
                        quote! {
                            continue #l;
                        }
                    }
                    relooper::BranchMode::LoopContinueIntoMulti(l) => {
                        let l = Lifetime::new(&format!("'l{}", l), Span::call_site());
                        quote! {
                            continue #l;
                        }
                    }
                    relooper::BranchMode::MergedBranch => {
                        quote! {}
                    }
                    relooper::BranchMode::MergedBranchIntoMulti => quote! {},
                    relooper::BranchMode::SetLabelAndBreak => quote! {
                        break 'cff;
                    },
                }
            }
            if stmts.is_invalid() {
                let immediate = s
                    .immediate
                    .as_ref()
                    .map(|a| render_relooped_block(opts, f, a.as_ref()))
                    .transpose()?
                    .unwrap_or_default();
                let next = s
                    .next
                    .as_ref()
                    .map(|a| render_relooped_block(opts, f, a.as_ref()))
                    .transpose()?
                    .unwrap_or_default();
                let term2 = term(
                    &s.branches
                        .get(&b.entry)
                        .cloned()
                        .unwrap_or(relooper::BranchMode::MergedBranch),
                );
                return Ok(quote! {
                    #term2;
                    #immediate;
                    #next;
                });
            }
            let fp_ts = fp(opts);
            let stmts = render_statements(opts, &f, b, stmts)?;
            let render_target = |k: &BlockTarget| {
                let vars = k.args.iter().enumerate().map(|(i, a)| {
                    let a = format_ident!("{a}");
                    let i = format_ident!("{}param{i}", k.block.to_string());
                    quasiquote! {
                        #i = #{fp(opts)}::cast::<_,_,C>(#a);
                    }
                });
                let br = term(
                    &s.branches
                        .get(&k.block)
                        .cloned()
                        .unwrap_or(relooper::BranchMode::MergedBranch),
                );
                let bi = k.block.index();
                quote! {
                    #(#vars);*;
                    cff = #bi;
                    #br
                }
            };
            let term = render_term(opts, f, b, s.label, &render_target)?;
            let immediate = s
                .immediate
                .as_ref()
                .map(|a| render_relooped_block(opts, f, a.as_ref()))
                .transpose()?
                .unwrap_or_default();
            let next = s
                .next
                .as_ref()
                .map(|a| render_relooped_block(opts, f, a.as_ref()))
                .transpose()?
                .unwrap_or_default();
            quote! {
                #(#stmts);*;
                #term;
                #immediate;
                #next;
            }
        }
        ShapedBlock::Loop(l) => {
            let r = render_relooped_block(opts, f, &l.inner.as_ref())?;
            let next = l
                .next
                .as_ref()
                .map(|a| render_relooped_block(opts, f, a.as_ref()))
                .transpose()?
                .unwrap_or_default();
            let l = Lifetime::new(&format!("'l{}", l.loop_id), Span::call_site());
            quote! {
                #l : loop{
                    #r
                };
                #next;
            }
        }
        ShapedBlock::Multiple(k) => {
            let initial = k.handled.iter().enumerate().flat_map(|(a, b)| {
                b.labels.iter().map(move |l| {
                    let l = l.index();
                    quote! {
                        #l => #a
                    }
                })
            });
            let cases = k
                .handled
                .iter()
                .enumerate()
                .map(|(a, i)| {
                    let ib = render_relooped_block(opts, f, &i.inner)?;
                    let ic = if i.break_after {
                        quote! {}
                    } else {
                        quote! {
                            cff2 += 1;
                            continue 'cff
                        }
                    };
                    Ok(quote! {
                        #a => {
                            #ib;
                            #ic;
                        }
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            quote! {
                let mut cff2 = match cff{
                    #(#initial),*,
                    _ => unreachable!()
                };
                'cff: loop{
                    match cff2{
                        #(#cases),*,
                        _ => unreachable!()
                    };
                    break 'cff;
                };
            }
        }
    })
}
pub(crate) fn render_fn(opts: &Opts<'_>, f: Func) -> anyhow::Result<TokenStream> {
    let name = fname(opts, f);
    let sig = render_fn_sig(
        opts,
        name.clone(),
        &opts.module.signatures[opts.module.funcs[f].sig()],
    );
    let root = opts.core.crate_path.clone();
    let Some(b) = opts.module.funcs[f].body() else {
        let fsig = opts.module.funcs[f].sig();
        let fsig = &opts.module.signatures[fsig];
        let SignatureData::Func {
            params, returns, ..
        } = fsig
        else {
            todo!()
        };
        let params = params
            .iter()
            .enumerate()
            .map(|(a, _)| format_ident!("p{a}"));
        let i = opts
            .module
            .imports
            .iter()
            .find(|a| a.kind == ImportKind::Func(f))
            .unwrap();
        let x = import(
            opts,
            i.module.as_str(),
            i.name.as_str(),
            params.map(|a| quote! {#a}),
        )?;
        return Ok(quote! {
            #sig {
                return #x;
            }
        });
    };
    let cfg = CFGInfo::new(b);
    let bpvalues = b.blocks.entries().flat_map(|(k, d)| {
        d.params.iter().enumerate().map(move |(i, (ty, _))| {
            let x = match ty {
                Type::Heap(WithNullable {
                    nullable,
                    value: HeapType::Sig { sig_index },
                }) if !nullable => render_fun_ref(opts, &quote! {C}, Func::invalid()),
                _ => quote! {
                    Default::default()
                },
            };
            let ty = render_ty(opts, &quote! {c}, ty.clone());
            let a = format_ident!("{k}param{i}");
            if k == b.entry {
                let p = format_ident!("p{i}");
                quote! {
                    #a: #ty = #p
                }
            } else {
                quote! {
                    #a: #ty = #x
                }
            }
        })
    });
    let reloop = waffle_func_reloop::go(b);
    let x = render_relooped_block(opts, f, reloop.as_ref())?;
    let mut b = quote! {
        let mut cff: usize = 0;
        #(let mut #bpvalues);*;
        #x;
        panic!("should have returned");
    };
    if opts.core.flags.contains(Flags::ASYNC) {
        b = quasiquote! {
            return #{alloc(opts)}::boxed::Box::pin(async move{
                #b
            })
        }
    }
    Ok(quote! {
        #sig {
            #b
        }
    })
}

impl<'a, X: AsRef<[u8]>> OptsLt<'a, X, LegacyPortalWaffleBackend> {
    pub(crate) fn to_waffle_mod(&self) -> OptsLt<'a, Module<'static>, LegacyPortalWaffleBackend> {
        let opts = self;
        let mut module =
            waffle::Module::from_wasm_bytes(opts.module.as_ref(), &Default::default()).unwrap();
        module.expand_all_funcs().unwrap();
        let mut module = module.without_orig_bytes();
        // module.per_func_body(|b|unswitch::go(b)); //TODO: reloop better and make it not needed
        // eprintln!("{}",module.display());
        module.per_func_body(|f| maxssa::run(f, None, &CFGInfo::new(f)));
        let opts = OptsLt {
            // crate_path: opts.crate_path.clone(),
            module,
            backend: self.backend.clone(),
            core: self.core.clone(), // tpit: opts.tpit.clone(),
                                     // cfg: opts.cfg.clone(),
        };
        return opts;
    }
}

pub(crate) fn go(
    opts: &OptsLt<'_, Module<'static>, LegacyPortalWaffleBackend>,
) -> anyhow::Result<proc_macro2::TokenStream> {
    let mut opts = opts.clone();
    let mut ps = vec![];
    while let Some(p) = opts.core.plugins.pop() {
        p.pre(&mut opts.core)?;
        ps.push(p);
    }
    opts.core.plugins = ps;
    for f in opts.module.funcs.values_mut() {
        if let Some(b) = f.body_mut() {
            if let Cow::Owned(c) = waffle::backend::reducify::Reducifier::new(b).run() {
                *b = c;
            }
        }
    }
    let internal_path = format_ident!("_{}_internal", opts.core.name);
    let data = format_ident!("{}Data", opts.core.name);
    let name = opts.core.name.clone();
    let root = opts.core.crate_path.clone();
    let funcs = opts
        .module
        .funcs
        .iter()
        .map(|a| render_fn(&opts, a))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut z = vec![];
    let mut fields = vec![];
    let mut sfields = vec![];
    let mut fs = vec![];
    fs.push(opts.core.embed.clone());
    for (k, v) in opts.core.data.iter() {
        fields.push(k.clone());
        z.push(quote! {
            #k : #v
        });
    }
    let mut init = vec![];
    for (t, d) in opts.module.tables.entries() {
        let n = Ident::new(&t.to_string(), Span::call_site());
        z.push(quasiquote! {
            #n: #{alloc(&opts)}::vec::Vec<#{fp(&opts)}::Value<Target>>
        });
        fields.push(n.clone());
        sfields.push(n.clone());
        if let Some(e) = d.func_elements.as_ref() {
            let e = e.iter().map(|x| render_fun_ref(&opts, &quote! {C}, *x));
            init.push(if opts.core.flags.contains(Flags::ASYNC) {
                quote! {
                    #(ctx.data().#n.push(#root::func::unsync::Coe::coe(#e)));*;
                }
            } else {
                quote! {
                    #(ctx.data().#n.push(#root::func::Coe::coe(#e)));*;
                }
            })
        }
        fs.push(quasiquote! {
            fn #n(&mut self) -> &mut #{alloc(&opts)}::vec::Vec<#{fp(&opts)}::Value<Self>>{
                &mut self.data().#n
            }
        })
    }
    // eprintln!("before globals");
    for (g, d) in opts.module.globals.entries() {
        let n = Ident::new(&g.to_string(), Span::call_site());
        let t = render_ty(&opts, &quote! {Target}, d.ty.clone());
        z.push(quote! {
            #n : #t
        });
        fields.push(n.clone());
        sfields.push(n.clone());
        fs.push(quote! {
            fn #n<'a>(&'a mut self) -> &'a mut #t{
                return &mut self.data().#n;
            }
        });
        if let Some(v) = d.value.clone() {
            init.push(quote! {
                *ctx.#n() = (#v as #t);
            })
        }
    }
    for (me, d) in opts.module.memories.entries() {
        let mut import_mod = None;
        for imp in opts.module.imports.iter() {
            if imp.kind == ImportKind::Memory(me) {
                import_mod = Some((imp.module.clone(), imp.name.clone()));
            }
        }
        let n = Ident::new(&me.to_string(), Span::call_site());
        match import_mod {
            None => {
                let mut t = quote! {
                    Vec<u8>
                };
                if d.shared {
                    t = quasiquote! {
                        #{alloc(&opts)}::sync::Arc<#root::Mutex<#t>>
                    };
                };
                z.push(quote! {
                    #n : #t
                });
                fields.push(n.clone());
                fs.push(quote! {
                    fn #n<'a>(&'a mut self) -> &'a mut #t{
                        return &mut self.data().#n;
                    }
                });
            }
            Some((a, b)) => {
                if opts.core.plugins.iter().any(|p| {
                    p.mem_import(&opts.core, &a, &b)
                        .ok()
                        .and_then(|a| a)
                        .is_some()
                }) {
                } else {
                    let m = Ident::new(&format!("{a}_{b}"), Span::call_site());
                    let mut p = if opts.core.flags.contains(Flags::LEGACY) {
                        quote! {dyn #root::Memory + 'a}
                    } else {
                        quote! {
                            impl #root::Memory + 'a
                        }
                    };
                    if d.shared {
                        p = quasiquote! {
                            #{alloc(&opts)}::sync::Arc<#root::Mutex<#p>>
                        };
                    };
                    fs.push(quote! {
                        fn #m<'a>(&'a mut self) -> &'a mut (#p);
                        fn #n<'a>(&'a mut self) -> &'a mut (#p){
                            return self.#m();
                        }
                    });
                }
            }
        }
        let pk = d.initial_pages * 65536;
        let pk = pk as u64;
        init.push(quote! {
            let l = #pk.max(ctx.#n().size()?);
            let s = ctx.#n().size()?;
            ctx.#n().grow(l - s)?;
        });
        for s in d.segments.clone() {
            for (i, d) in s.data.chunks(65536).enumerate() {
                let o = s.offset + i * 65536;
                let pk = o + d.len();
                let pk = pk + 1;
                let o = o as u64;
                init.push(quote! {
                    ctx.#n().write(#o,&[#(#d),*])?
                });
            }
        }
    }
    let mut fs2 = vec![];
    let mut fs3 = vec![];
    for xp in opts.module.exports.iter() {
        let xp = Export {
            name: bindname(&xp.name),
            kind: xp.kind.clone(),
        };
        match &xp.kind {
            ExportKind::Func(f) => {
                let f = *f;
                let d = render_export(
                    &opts,
                    format_ident!("{}", xp.name),
                    fname(&opts, f),
                    &opts.module.signatures[opts.module.funcs[f].sig()],
                );
                let e = render_self_sig_import(
                    &opts,
                    format_ident!("{}", xp.name),
                    &opts.module.signatures[opts.module.funcs[f].sig()],
                );
                fs2.push(quote! {
                    #d
                });
                fs3.push(quote! {
                    #e
                })
            }
            ExportKind::Table(t) => {
                let d = &opts.module.tables[*t];
                let tt = render_ty(&opts, &quote! {Self}, d.ty);
                let x = Ident::new(&t.to_string(), Span::call_site());
                let mn = Ident::new(&xp.name, Span::call_site());
                let i = quote! {
                    fn #mn(&mut self) -> &mut #{alloc(&opts)}::vec::Vec<#tt>{
                        return &mut self.z().#x;
                    }
                };
                fs.push(i);
            }
            ExportKind::Global(g) => {
                let d = &opts.module.globals[*g];
                let t = render_ty(&opts, &quote! {Self}, d.ty);
                let x = Ident::new(&g.to_string(), Span::call_site());
                let mn = Ident::new(&xp.name, Span::call_site());
                let i = quote! {
                    fn #mn(&mut self) -> &mut #t{
                        return self.#x()
                    }
                };
                fs.push(i);
            }
            ExportKind::Memory(m) => {
                let x = Ident::new(&m.to_string(), Span::call_site());
                let mn = Ident::new(&xp.name, Span::call_site());
                let i = quasiquote! {
                    fn #mn<'a>(&'a mut self) -> &'a mut (#{
                        let mut p = if opts.core.flags.contains(Flags::LEGACY) {
                            quote! {dyn #root::Memory + 'a}
                        } else {
                            quote! {
                                impl #root::Memory + 'a
                            }
                        };
                        if opts.module.memories[*m].shared{
                            p = quasiquote!{
                                #{alloc(&opts)}::sync::Arc<#root::Mutex<#p>>
                            };
                        };
                        p
                    }){
                        return self.#x()
                    }
                };
                fs.push(i);
            }
            _ => todo!(),
        }
    }
    for i in opts.module.imports.iter() {
        if let ImportKind::Func(f) = &i.kind {
            for plugin in opts.core.plugins.iter() {
                if plugin
                    .import(
                        &opts.core,
                        &i.module,
                        &i.name,
                        match &opts.module.signatures[opts.module.funcs[*f].sig()] {
                            SignatureData::Func {
                                params, returns, ..
                            } => params.iter().map(|_| quote! {}).collect(),
                            _ => todo!(),
                        },
                    )?
                    .is_some()
                {
                    continue;
                }
            }
            let name = format_ident!("{}_{}", bindname(&i.module), bindname(&i.name));
            fs.push(render_self_sig_import(
                &opts,
                name,
                &opts.module.signatures[opts.module.funcs[*f].sig()],
            ));
        }
    }
    let defaults = fields.iter().map(|a| {
        quote! {
            #a: Default::default()
        }
    });
    let clones = fields.iter().map(|a| {
        quote! {
            #a: self.#a.clone()
        }
    });
    Ok(quasiquote! {
        // mod #internal_path{
            // extern crate alloc;
            // pub(crate) fn old_alloc<T>(m: &mut  #{alloc(&opts)}::collections::BTreeMap<u32,T>, x: T) -> u32{
            //     let mut u = 0;
            //     while m.contains_key(&u){
            //         u += 1;
            //     };
            //     m.insert(u,x);
            //     return u;
            // }
            pub struct #data<Target: #name + ?Sized>{
                #(#z),*
            }
            impl<Target: #name + ?Sized> #root::Traverse<Target> for #data<Target>{
                fn traverse<'a>(&'a self) ->#{alloc(&opts)}::boxed::Box<dyn Iterator<Item = &'a Target::ExternRef> + 'a>{
                    return #{
                        let x = sfields.iter().map(|a|quote!{#root::Traverse::<Target>::traverse(&self.#a)});
                        quasiquote!{
                            #{alloc(&opts)}::boxed::Box::new(::core::iter::empty()#(.chain(#x))*)
                        }
                    }
                }
                fn traverse_mut<'a>(&'a mut self) -> #{alloc(&opts)}::boxed::Box<dyn Iterator<Item = &'a mut Target::ExternRef> + 'a>{
                    return #{
                        let x = sfields.iter().map(|a|quote!{#root::Traverse::<Target>::traverse_mut(&mut self.#a)});
                        quasiquote!{
                            #{alloc(&opts)}::boxed::Box::new(::core::iter::empty()#(.chain(#x))*)
                        }
                    }
                }
            }
            pub trait #name: #{fp(&opts)}::CtxSpec<ExternRef = Self::_ExternRef> #{if opts.core.flags.contains(Flags::ASYNC){
                quote! {+ Send + Sync}
            }else{
                quote! {}
            }}  #{
                let a = opts.core.plugins.iter().map(|p|{
                    let b = p.bounds(&opts.core)?;
                    anyhow::Ok(match b{
                        None => quote!{},
                        Some(a) => quote!{+ #a}
                    })
                }).collect::<anyhow::Result<Vec<_>>>()?;
                quote!{
                    #(#a)*
                }
            }{
                type _ExternRef: Clone  #{
                    let a = opts.core.plugins.iter().map(|p|{
                        let b = p.exref_bounds(&opts.core)?;
                        Ok(match b{
                            None => quote!{},
                            Some(a) => quote!{+ #a}
                        })
                    }).collect::<anyhow::Result<Vec<_>>>()?;
                    quote!{
                        #(#a)*
                    }
                };
                fn data(&mut self) -> &mut #data<Self>;
                #(#fs)*
            }
            pub trait #{format_ident!("{name}Impl")}: #name{
                #(#fs3)*
                fn init(&mut self) -> #root::_rexport::anyhow::Result<()> where Self: 'static;
            }
            const _: () = {
                use #root::Memory;
                impl<C: #name> #{format_ident!("{name}Impl")} for C{
                    #(#fs2)*
                    fn init(&mut self) -> #root::_rexport::anyhow::Result<()> where Self: 'static{
                        let ctx = self;
                        #(#init);*;
                        return Ok(())
                    }
                }
                #(#funcs)*
            };
            impl<Target: #name + ?Sized> Default for #data<Target>{
                fn default() -> Self{
                    Self{
                        #(#defaults),*
                    }
                }
            }
            impl<Target: #name + ?Sized> Clone for #data<Target>{
                fn clone(&self) -> Self{
                    Self{
                        #(#clones),*
                    }
                }
            }
            #{
                let a = opts.core.plugins.iter().map(|a|a.post(&opts.core)).collect::<anyhow::Result<Vec<_>>>()?;
                quote!(#(#a)*)
            }
        // }
        // use #internal_path::{#name,#data};
    })
}
impl<'a> OptsLt<'a, Module<'static>, LegacyPortalWaffleBackend> {
    pub(crate) fn to_tokens(&self, tokens: &mut TokenStream) {
        match go(self) {
            Ok(a) => a.to_tokens(tokens),
            Err(e) => syn::Error::new(Span::call_site(), e)
                .to_compile_error()
                .to_tokens(tokens),
        }
    }
}
impl<'a, X: AsRef<[u8]>> ToTokens for OptsLt<'a, X, LegacyPortalWaffleBackend> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        self.to_waffle_mod().to_tokens(tokens);
    }
}
