//! ABI v0 backend driven by `wasmparser`.
//!
//! This backend replaces the waffle pipeline for the common case where you just
//! want fast, dependency-light code generation.  It walks the binary once,
//! collects all section data, and emits ABI v0 Rust tokens in a single pass.

use super::*;
use crate::shared::{self, bindname, alloc, fp, FuncSig, FuncSigOwned, WasmTy};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, ToTokens};
use syn::{Ident, Lifetime};
use wasmparser::{
    CompositeInnerType, ElementItems, ElementKind, ExternalKind, GlobalType, MemoryType,
    Operator, Parser, Payload, RefType, TableType, TypeRef, ValType,
};

// ─── Parsed module ────────────────────────────────────────────────────────────

/// Flat index-space record of an import.
#[derive(Clone)]
struct ImportEntry {
    module: String,
    name: String,
    kind: ImportKind,
}

#[derive(Clone, PartialEq, Eq)]
enum ImportKind {
    Func(u32),   // function index
    Table(u32),  // table index
    Memory(u32), // memory index
    Global(u32), // global index
}

/// Everything we need from the wasm binary, collected in one streaming pass.
struct ParsedModule {
    /// All function types from the type section (by type-section index).
    types: Vec<FuncSigOwned<ValType>>,
    /// All imports, in order.
    imports: Vec<ImportEntry>,
    /// type-section index for every function (imports first, then defined).
    func_type_idx: Vec<u32>,
    /// table types (imports first, then defined).
    table_types: Vec<TableType>,
    /// memory types (imports first, then defined).
    memory_types: Vec<MemoryType>,
    /// global types (imports first, then defined).
    global_types: Vec<GlobalType>,
    /// Exports, in order.
    exports: Vec<(String, ExternalKind, u32)>,
    /// Optional start function index.
    #[allow(dead_code)]
    start: Option<u32>,
    /// Active element segments: (table_idx, offset_expr_bytes, func_indices).
    elements: Vec<ElementSeg>,
    /// Active data segments: (memory_idx, offset, bytes).
    data_segs: Vec<DataSeg>,
    /// Function bodies (raw bytes), one per *defined* function.
    /// `defined_bodies[i]` corresponds to function index `n_func_imports + i`.
    defined_bodies: Vec<(Vec<(u32, ValType)>, Vec<u8>)>, // (locals, op_bytes)
    /// Number of imported functions.
    n_func_imports: u32,
    /// Number of imported memories.
    n_mem_imports: u32,
    /// Number of imported tables.
    n_table_imports: u32,
    /// Number of imported globals.
    n_global_imports: u32,
    /// Best-effort function names from the name section.
    func_names: std::collections::HashMap<u32, String>,
    /// Constant-expression init values for *defined* globals (index 0 = first defined global).
    global_init_vals: Vec<Option<ConstInit>>,
}

struct ElementSeg {
    table_idx: u32,
    /// Evaluated constant offset (we only handle i32.const initialiser).
    offset: u32,
    /// Function indices referenced by the segment.
    func_indices: Vec<u32>,
}

struct DataSeg {
    memory_idx: u32,
    offset: u64,
    bytes: Vec<u8>,
}

impl ParsedModule {
    fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        let mut types: Vec<FuncSigOwned<ValType>> = vec![];
        let mut imports: Vec<ImportEntry> = vec![];
        let mut func_type_idx: Vec<u32> = vec![];
        let mut table_types: Vec<TableType> = vec![];
        let mut memory_types: Vec<MemoryType> = vec![];
        let mut global_types: Vec<GlobalType> = vec![];
        let mut exports: Vec<(String, ExternalKind, u32)> = vec![];
        let mut start: Option<u32> = None;
        let mut elements: Vec<ElementSeg> = vec![];
        let mut data_segs: Vec<DataSeg> = vec![];
        let mut defined_bodies: Vec<(Vec<(u32, ValType)>, Vec<u8>)> = vec![];
        let mut n_func_imports = 0u32;
        let mut n_table_imports = 0u32;
        let mut n_mem_imports = 0u32;
        let mut n_global_imports = 0u32;
        let mut func_names: std::collections::HashMap<u32, String> = Default::default();
        let mut global_init_vals: Vec<Option<ConstInit>> = vec![];

        for payload in Parser::new(0).parse_all(bytes) {
            let payload = payload?;
            match payload {
                Payload::TypeSection(r) => {
                    for rec_group in r {
                        let rec_group = rec_group?;
                        for sub_ty in rec_group.types() {
                            // Only care about func types; skip everything else.
                            let sig = match &sub_ty.composite_type.inner {
                                CompositeInnerType::Func(f) => FuncSigOwned::<ValType> {
                                    params: f.params().to_vec(),
                                    returns: f.results().to_vec(),
                                },
                                _ => FuncSigOwned::<ValType> { params: vec![], returns: vec![] },
                            };
                            types.push(sig);
                        }
                    }
                }
                Payload::ImportSection(r) => {
                    for imp in r {
                        let imp = imp?;
                        let kind = match imp.ty {
                            TypeRef::Func(t) => {
                                func_type_idx.push(t);
                                let k = ImportKind::Func(n_func_imports);
                                n_func_imports += 1;
                                k
                            }
                            TypeRef::Table(t) => {
                                table_types.push(t);
                                let k = ImportKind::Table(n_table_imports);
                                n_table_imports += 1;
                                k
                            }
                            TypeRef::Memory(m) => {
                                memory_types.push(m);
                                let k = ImportKind::Memory(n_mem_imports);
                                n_mem_imports += 1;
                                k
                            }
                            TypeRef::Global(g) => {
                                global_types.push(g);
                                let k = ImportKind::Global(n_global_imports);
                                n_global_imports += 1;
                                k
                            }
                            TypeRef::Tag(_) => continue,
                        };
                        imports.push(ImportEntry {
                            module: imp.module.to_string(),
                            name: imp.name.to_string(),
                            kind,
                        });
                    }
                }
                Payload::FunctionSection(r) => {
                    for type_idx in r {
                        func_type_idx.push(type_idx?);
                    }
                }
                Payload::TableSection(r) => {
                    for t in r {
                        let t = t?;
                        table_types.push(t.ty);
                    }
                }
                Payload::MemorySection(r) => {
                    for m in r {
                        memory_types.push(m?);
                    }
                }
                Payload::GlobalSection(r) => {
                    for g in r {
                        let g = g?;
                        global_types.push(g.ty);
                        // Try to extract a constant init expression.
                        let init_val = const_val_expr(g.init_expr.get_binary_reader());
                        global_init_vals.push(init_val);
                    }
                }
                Payload::ExportSection(r) => {
                    for e in r {
                        let e = e?;
                        exports.push((e.name.to_string(), e.kind, e.index));
                    }
                }
                Payload::StartSection { func, .. } => {
                    start = Some(func);
                }
                Payload::ElementSection(r) => {
                    for elem in r {
                        let elem = elem?;
                        // Only handle active segments with function indices.
                        let (table_idx, offset) = match elem.kind {
                            ElementKind::Active { table_index, offset_expr } => {
                                let tidx = table_index.unwrap_or(0);
                                // Parse constant offset — only i32.const supported.
                                let offset = const_i32_expr(offset_expr.get_binary_reader())?;
                                (tidx, offset)
                            }
                            _ => continue,
                        };
                        let func_indices = match elem.items {
                            ElementItems::Functions(r) => {
                                r.into_iter().collect::<Result<Vec<_>, _>>()?
                            }
                            ElementItems::Expressions(_, r) => {
                                let mut idxs = vec![];
                                for item in r {
                                    let item = item?;
                                    idxs.push(ref_expr_func_idx(item.get_binary_reader())?);
                                }
                                idxs
                            }
                        };
                        elements.push(ElementSeg { table_idx, offset, func_indices });
                    }
                }
                Payload::DataSection(r) => {
                    for seg in r {
                        let seg = seg?;
                        let (memory_idx, offset) = match seg.kind {
                            wasmparser::DataKind::Active { memory_index, offset_expr } => {
                                (memory_index, const_i32_expr(offset_expr.get_binary_reader())? as u64)
                            }
                            wasmparser::DataKind::Passive => continue,
                        };
                        data_segs.push(DataSeg {
                            memory_idx,
                            offset,
                            bytes: seg.data.to_vec(),
                        });
                    }
                }
                Payload::CodeSectionEntry(body) => {
                    // Collect locals.
                    let mut locals: Vec<(u32, ValType)> = vec![];
                    let lr = body.get_locals_reader()?;
                    for l in lr {
                        locals.push(l?);
                    }
                    // Store the full body bytes (includes locals prefix).
                    let op_bytes = body.as_bytes().to_vec();
                    defined_bodies.push((locals, op_bytes));
                }
                Payload::CustomSection(s) if s.name() == "name" => {
                    // Best-effort name section parsing.
                    let data = s.data();
                    let reader = wasmparser::BinaryReader::new(data, s.data_offset());
                    let nr = wasmparser::NameSectionReader::new(reader);
                    for item in nr {
                        if let Ok(wasmparser::Name::Function(fmap)) = item {
                            for entry in fmap {
                                if let Ok(n) = entry {
                                    func_names.insert(n.index, n.name.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(ParsedModule {
            types,
            imports,
            func_type_idx,
            table_types,
            memory_types,
            global_types,
            exports,
            start,
            elements,
            data_segs,
            defined_bodies,
            n_func_imports,
            n_mem_imports,
            n_table_imports,
            n_global_imports,
            func_names,
            global_init_vals,
        })
    }

    /// Resolve a function index to its `FuncSigOwned<ValType>`.
    fn func_sig(&self, func_idx: u32) -> &FuncSigOwned<ValType> {
        let ty_idx = self.func_type_idx[func_idx as usize];
        &self.types[ty_idx as usize]
    }

    /// Get an import entry for a function index (if the function is imported).
    fn import_for_func(&self, func_idx: u32) -> Option<&ImportEntry> {
        self.imports.iter().find(|i| i.kind == ImportKind::Func(func_idx))
    }

    /// Is this function index a defined (non-imported) function?
    fn is_defined(&self, func_idx: u32) -> bool {
        func_idx >= self.n_func_imports
    }

    /// Name to use for the internal free function for function `func_idx`.
    fn fname(&self, func_idx: u32) -> Ident {
        let raw = self.func_names.get(&func_idx).cloned()
            .unwrap_or_else(|| String::new());
        format_ident!("func{}_{}", func_idx, bindname(&raw))
    }
}

// ── Small helpers for constant-expression parsing ─────────────────────────────

fn const_i32_expr(reader: wasmparser::BinaryReader<'_>) -> anyhow::Result<u32> {
    let mut ops = wasmparser::OperatorsReader::new(reader);
    let mut val = 0u32;
    while !ops.eof() {
        let op = ops.read()?;
        match op {
            Operator::I32Const { value } => val = value as u32,
            Operator::I64Const { value } => val = value as u32,
            Operator::End => break,
            _ => {}
        }
    }
    Ok(val)
}

/// Extract a constant token stream from a wasm constant expression.
/// Returns `None` for non-trivial expressions.
/// Is this value type a reference type (externref/funcref/etc.)?
fn is_ref_ty(ty: wasmparser::ValType) -> bool {
    matches!(
        ty,
        wasmparser::ValType::EXTERNREF
            | wasmparser::ValType::FUNCREF
    )
}

/// Result of evaluating a global init expression.
pub enum ConstInit {
    /// ref.null — stored as the null runtime value.
    Null,
    /// A typed Rust expression; the caller casts it to the global's type.
    Val(TokenStream),
}

/// Evaluate a constant initializer expression (consts, `global.get`, and
/// the extended-const arithmetic add/sub/mul) to a Rust token stream.
/// Returns None when the expression uses unsupported operators.
fn const_val_expr(reader: wasmparser::BinaryReader<'_>) -> Option<ConstInit> {
    let mut ops = wasmparser::OperatorsReader::new(reader);
    let mut stack: Vec<ConstInit> = vec![];
    while !ops.eof() {
        match ops.read().ok()? {
            Operator::I32Const { value } => stack.push(ConstInit::Val(quote! { (#value as u32) })),
            Operator::I64Const { value } => stack.push(ConstInit::Val(quote! { (#value as u64) })),
            Operator::F32Const { value } => {
                let bits = value.bits();
                stack.push(ConstInit::Val(quote! { f32::from_bits(#bits) }));
            }
            Operator::F64Const { value } => {
                let bits = value.bits();
                stack.push(ConstInit::Val(quote! { f64::from_bits(#bits) }));
            }
            // `global.get` init (refers to an imported global): copy it.
            Operator::GlobalGet { global_index } => {
                let gn = format_ident!("global{global_index}");
                stack.push(ConstInit::Val(quote! { (*ctx.#gn()) }));
            }
            Operator::RefNull { .. } => stack.push(ConstInit::Null),
            Operator::I32Add => {
                let ConstInit::Val(b) = stack.pop()? else { return None };
                let ConstInit::Val(a) = stack.pop()? else { return None };
                stack.push(ConstInit::Val(quote! {
                    ((#a as u32).wrapping_add(#b as u32))
                }));
            }
            Operator::I32Sub => {
                let ConstInit::Val(b) = stack.pop()? else { return None };
                let ConstInit::Val(a) = stack.pop()? else { return None };
                stack.push(ConstInit::Val(quote! {
                    ((#a as u32).wrapping_sub(#b as u32))
                }));
            }
            Operator::I32Mul => {
                let ConstInit::Val(b) = stack.pop()? else { return None };
                let ConstInit::Val(a) = stack.pop()? else { return None };
                stack.push(ConstInit::Val(quote! {
                    ((#a as u32).wrapping_mul(#b as u32))
                }));
            }
            Operator::I64Add => {
                let ConstInit::Val(b) = stack.pop()? else { return None };
                let ConstInit::Val(a) = stack.pop()? else { return None };
                stack.push(ConstInit::Val(quote! {
                    ((#a as u64).wrapping_add(#b as u64))
                }));
            }
            Operator::I64Sub => {
                let ConstInit::Val(b) = stack.pop()? else { return None };
                let ConstInit::Val(a) = stack.pop()? else { return None };
                stack.push(ConstInit::Val(quote! {
                    ((#a as u64).wrapping_sub(#b as u64))
                }));
            }
            Operator::I64Mul => {
                let ConstInit::Val(b) = stack.pop()? else { return None };
                let ConstInit::Val(a) = stack.pop()? else { return None };
                stack.push(ConstInit::Val(quote! {
                    ((#a as u64).wrapping_mul(#b as u64))
                }));
            }
            Operator::End => break,
            _ => return None,
        }
    }
    stack.pop()
}

fn ref_expr_func_idx(reader: wasmparser::BinaryReader<'_>) -> anyhow::Result<u32> {
    let mut ops = wasmparser::OperatorsReader::new(reader);
    let mut idx = 0u32;
    while !ops.eof() {
        let op = ops.read()?;
        match op {
            Operator::RefFunc { function_index } => idx = function_index,
            Operator::End => break,
            _ => {}
        }
    }
    Ok(idx)
}

// ─── Code generation ──────────────────────────────────────────────────────────

type Opts<'a> = OptsLt<'a, &'a [u8], WasmparserBackend>;

// ─── Chunk context ────────────────────────────────────────────────────────────

struct ChunkCtx {
    /// func_chunk[i] = chunk index for func i; usize::MAX for imports.
    func_chunk: Vec<usize>,
    /// Total number of chunks.
    n_chunks: usize,
    /// Number of imported functions.
    n_func_imports: usize,
}

impl ChunkCtx {
    fn build(n_imports: usize, total: usize, chunk_size: usize) -> Self {
        let mut func_chunk = vec![usize::MAX; total];
        let n_defined = total.saturating_sub(n_imports);
        let n_chunks = if n_defined == 0 {
            0
        } else {
            (n_defined + chunk_size - 1) / chunk_size
        };
        for def_idx in 0..n_defined {
            func_chunk[n_imports + def_idx] = def_idx / chunk_size;
        }
        ChunkCtx { func_chunk, n_chunks, n_func_imports: n_imports }
    }

    fn chunk_of(&self, func_idx: u32) -> usize {
        self.func_chunk[func_idx as usize]
    }

    fn is_import(&self, func_idx: u32) -> bool {
        self.func_chunk[func_idx as usize] == usize::MAX
    }
}

pub fn go(opts: &Opts<'_>) -> anyhow::Result<TokenStream> {
    let m = ParsedModule::parse(opts.module)?;
    emit(&opts.core, &m)
}

fn emit(core: &OptsCore<'_>, m: &ParsedModule) -> anyhow::Result<TokenStream> {
    let root = core.crate_path.clone();
    let fp_ts = fp(core);
    let alloc_ts = alloc(core);
    let name = core.name.clone();
    let data_ty = format_ident!("{}Data", name);
    let impl_trait = format_ident!("{}Impl", name);

    // Build chunk context early — needed by export delegate generation and table element refs.
    let total_funcs = m.func_type_idx.len() as u32;
    let chunk_ctx_owned: Option<ChunkCtx> = core.chunk_size.map(|cs| {
        ChunkCtx::build(m.n_func_imports as usize, total_funcs as usize, cs)
    });

    // ── *Data struct fields ──────────────────────────────────────────────────
    let mut data_fields: Vec<TokenStream> = vec![];   // struct field declarations
    let mut field_names: Vec<Ident> = vec![];         // all field idents (for Default/Clone)
    let mut traverse_fields: Vec<Ident> = vec![];     // fields that need Traverse chain

    // Extra user-supplied fields.
    for (k, v) in core.data.iter() {
        data_fields.push(quote! { pub #k: #v });
        field_names.push(k.clone());
    }

    // Tables.
    for t_idx in 0..m.table_types.len() {
        let t_idx = t_idx as u32;
        let n = format_ident!("table{t_idx}");
        data_fields.push(quote! {
            pub #n: #alloc_ts::vec::Vec<#fp_ts::Value<Target>>
        });
        field_names.push(n.clone());
        traverse_fields.push(n);
    }

    // Globals.
    for g_idx in 0..m.global_types.len() {
        let g = &m.global_types[g_idx];
        let n = format_ident!("global{g_idx}");
        let t = shared::render_ty(core, &quote! { Target }, g.content_type);
        data_fields.push(quote! { pub #n: #t });
        field_names.push(n.clone());
    }

    // Owned memories.
    for me_idx in 0..m.memory_types.len() {
        let me_idx = me_idx as u32;
        // Skip imported memories — they are not stored in *Data.
        if me_idx < m.n_mem_imports {
            continue;
        }
        let d = &m.memory_types[me_idx as usize];
        let n = format_ident!("memory{me_idx}");
        let mut t = quote! { Vec<u8> };
        if d.shared {
            t = quote! { #alloc_ts::sync::Arc<#root::Mutex<#t>> };
        }
        data_fields.push(quote! { pub #n: #t });
        field_names.push(n.clone());
    }

    let embed_field = &core.embed;
    // Reference-typed globals hold wars_rt Value wrappers which have no
    // Default impl — initialize them to Value(Null) explicitly.
    let mut ref_global_fields: std::collections::HashSet<String> = Default::default();
    for (g_idx, g) in m.global_types.iter().enumerate() {
        if is_ref_ty(g.content_type) {
            ref_global_fields.insert(format!("global{g_idx}"));
        }
    }
    let defaults = field_names.iter().map(|n| {
        if ref_global_fields.contains(&n.to_string()) {
            quote! { #n: #fp_ts::Value(::wars_rt::func::value::Value::Null) }
        } else {
            quote! { #n: Default::default() }
        }
    });
    let clones = field_names.iter().map(|n| quote! { #n: self.#n.clone() });
    let traverse_chain = traverse_fields.iter().map(|n| {
        quote! { .chain(#root::Traverse::<Target>::traverse(&self.#n)) }
    });
    let traverse_mut_chain = traverse_fields.iter().map(|n| {
        quote! { .chain(#root::Traverse::<Target>::traverse_mut(&mut self.#n)) }
    });

    // ── Host trait methods ───────────────────────────────────────────────────
    let mut trait_methods: Vec<TokenStream> = vec![];
    // data() method
    trait_methods.push(quote! {
        fn data(&mut self) -> &mut #data_ty<Self>;
    });

    // One method per table.
    for t_idx in 0..m.table_types.len() {
        let t_idx = t_idx as u32;
        let n = format_ident!("table{t_idx}");
        trait_methods.push(quote! {
            fn #n(&mut self) -> &mut #alloc_ts::vec::Vec<#fp_ts::Value<Self>> {
                &mut self.data().#n
            }
        });
    }

    // One method per global.
    for g_idx in 0..m.global_types.len() {
        let g_ty = shared::render_ty(core, &quote! { Self }, m.global_types[g_idx].content_type);
        let n = format_ident!("global{g_idx}");
        trait_methods.push(quote! {
            fn #n<'a>(&'a mut self) -> &'a mut #g_ty {
                &mut self.data().#n
            }
        });
    }

    // One method per memory.
    for me_idx in 0..m.memory_types.len() {
        let me_idx_u = me_idx as u32;
        let n = format_ident!("memory{me_idx}");
        let d = &m.memory_types[me_idx];

        // Check if this memory is imported.
        let import_entry = m.imports.iter().find(|i| i.kind == ImportKind::Memory(me_idx_u));
        match import_entry {
            None => {
                // Owned memory — method returns &mut the field.
                let mut ret_ty = quote! { Vec<u8> };
                if d.shared {
                    ret_ty = quote! { #alloc_ts::sync::Arc<#root::Mutex<#ret_ty>> };
                }
                trait_methods.push(quote! {
                    fn #n<'a>(&'a mut self) -> &'a mut #ret_ty {
                        &mut self.data().#n
                    }
                });
            }
            Some(imp) => {
                // Imported memory: require user to implement a named method,
                // plus provide the entity-index alias.
                let imp_name = format_ident!("{}_{}", bindname(&imp.module), bindname(&imp.name));
                let mut p_ty = if core.flags.contains(Flags::LEGACY) {
                    quote! { dyn #root::Memory<Self::Error> + 'a }
                } else {
                    quote! { impl #root::Memory<Self::Error> + 'a }
                };
                if d.shared {
                    p_ty = quote! { #alloc_ts::sync::Arc<#root::Mutex<#p_ty>> };
                }
                // User must impl this.
                trait_methods.push(quote! {
                    fn #imp_name<'a>(&'a mut self) -> &'a mut (#p_ty);
                });
                // Alias by entity index.
                trait_methods.push(quote! {
                    fn #n<'a>(&'a mut self) -> &'a mut (#p_ty) {
                        self.#imp_name()
                    }
                });
            }
        }
    }

    // One method per imported function.
    for imp in m.imports.iter() {
        if let ImportKind::Func(func_idx) = imp.kind {
            // Check if any plugin handles this import.
            let plugin_handles = core.plugins.iter().any(|p| {
                p.import(&core, &imp.module, &imp.name, vec![])
                    .ok()
                    .and_then(|x| x)
                    .is_some()
            });
            if plugin_handles {
                continue;
            }
            let mname = format_ident!("{}_{}", bindname(&imp.module), bindname(&imp.name));
            let sig = m.func_sig(func_idx);
            trait_methods.push(shared::render_self_sig_import(core, mname, sig.as_ref()));
        }
    }

    // ── FooImpl trait: export declarations ───────────────────────────────────
    let mut impl_trait_methods: Vec<TokenStream> = vec![];
    let mut blanket_methods: Vec<TokenStream> = vec![];

    for (exp_name, exp_kind, exp_idx) in &m.exports {
        match exp_kind {
            ExternalKind::Func => {
                let func_idx = *exp_idx;
                let sig = m.func_sig(func_idx);
                let rust_name = format_ident!("{}", bindname(exp_name));
                let free_fn = m.fname(func_idx);
                impl_trait_methods.push(shared::render_self_sig_import(core, rust_name.clone(), sig.as_ref()));
                // When chunking, the free fn lives in _chunkN; use a path expression.
                let blanket = if let Some(ref cctx) = chunk_ctx_owned {
                    let chunk = cctx.chunk_of(func_idx);
                    let mod_name = format_ident!("_chunk{}", chunk);
                    let path = quote! { #mod_name::#free_fn };
                    shared::render_export_path(core, rust_name, path, sig.as_ref())
                } else {
                    shared::render_export(core, rust_name, free_fn, sig.as_ref())
                };
                blanket_methods.push(blanket);
            }
            ExternalKind::Table => {
                let t_idx = *exp_idx;
                let n = format_ident!("table{t_idx}");
                let mn = format_ident!("{}", bindname(exp_name));
                let t_ty = shared::render_ty(core, &quote! { Self }, ValType::Ref(m.table_types[t_idx as usize].element_type));
                trait_methods.push(quote! {
                    fn #mn(&mut self) -> &mut #alloc_ts::vec::Vec<#t_ty> {
                        self.#n()
                    }
                });
            }
            ExternalKind::Global => {
                let g_idx = *exp_idx;
                let n = format_ident!("global{g_idx}");
                let mn = format_ident!("{}", bindname(exp_name));
                let g_ty = shared::render_ty(core, &quote! { Self }, m.global_types[g_idx as usize].content_type);
                trait_methods.push(quote! {
                    fn #mn(&mut self) -> &mut #g_ty {
                        self.#n()
                    }
                });
            }
            ExternalKind::Memory => {
                let me_idx = *exp_idx;
                let n = format_ident!("memory{me_idx}");
                let mn = format_ident!("{}", bindname(exp_name));
                let d = &m.memory_types[me_idx as usize];
                let mut p_ty = if core.flags.contains(Flags::LEGACY) {
                    quote! { dyn #root::Memory<Self::Error> + 'a }
                } else {
                    quote! { impl #root::Memory<Self::Error> + 'a }
                };
                if d.shared {
                    p_ty = quote! { #alloc_ts::sync::Arc<#root::Mutex<#p_ty>> };
                }
                trait_methods.push(quote! {
                    fn #mn<'a>(&'a mut self) -> &'a mut (#p_ty) {
                        self.#n()
                    }
                });
            }
            _ => {}
        }
    }

    // ── init() body ──────────────────────────────────────────────────────────
    let mut init_stmts: Vec<TokenStream> = vec![];

    // Memory: grow + data segments.
    for me_idx in 0..m.memory_types.len() {
        let me_idx_u = me_idx as u32;
        let d = &m.memory_types[me_idx];
        let n = format_ident!("memory{me_idx}");
        let min_bytes = d.initial * 65536;
        let min_bytes = min_bytes as u64;
        init_stmts.push(quote! {
            let l = #min_bytes.max(ctx.#n().size()?);
            let s = ctx.#n().size()?;
            ctx.#n().grow(l - s)?;
        });
        for ds in m.data_segs.iter().filter(|ds| ds.memory_idx == me_idx_u) {
            for (i, chunk) in ds.bytes.chunks(65536).enumerate() {
                let off = ds.offset + (i * 65536) as u64;
                init_stmts.push(quote! {
                    ctx.#n().write(#off, &[#(#chunk),*])?;
                });
            }
        }
    }

    // Globals: set to initialiser value (constants only).
    for (g_def_idx, g_abs_idx) in (m.n_global_imports..m.global_types.len() as u32).enumerate() {
        let gn = format_ident!("global{g_abs_idx}");
        // NOTE: `.get()` returns Option<&Option<_>>; both layers matter:
        // outer = slot exists, inner = the slot holds a supported init.
        if let Some(Some(init)) = m.global_init_vals.get(g_def_idx) {
            let g_ty = shared::render_ty(core, &quote! { C }, m.global_types[g_abs_idx as usize].content_type);
            match init {
                ConstInit::Val(val) => init_stmts.push(quote! {
                    *ctx.#gn() = ((#val) as #g_ty);
                }),
                ConstInit::Null => init_stmts.push(quote! {
                    *ctx.#gn() = #fp_ts::Value(::wars_rt::func::value::Value::Null);
                }),
            }
        }
    }

    // Tables: element segments.
    for elem in m.elements.iter() {
        let t_n = format_ident!("table{}", elem.table_idx);
        let offset = elem.offset as usize;
        let pushes = elem.func_indices.iter().enumerate().map(|(slot, &fidx)| {
            let abs = offset + slot;
            let fun_ref = render_fun_ref(core, m, fidx, chunk_ctx_owned.as_ref());
            quote! {
                while ctx.#t_n().len() <= #abs {
                    ctx.#t_n().push(#fp_ts::Value(::wars_rt::func::value::Value::Null));
                }
                ctx.#t_n()[#abs] = #fp_ts::cast::<_,_,C>(#fun_ref);
            }
        });
        for p in pushes {
            init_stmts.push(p);
        }
    }

    // ── Free functions ───────────────────────────────────────────────────────
    let mut free_fns: Vec<TokenStream> = vec![];
    if chunk_ctx_owned.is_none() {
        // No chunking: emit all functions into the single const block (original behavior).
        for func_idx in 0..total_funcs {
            let ts = render_fn(core, m, func_idx, None)?;
            free_fns.push(ts);
        }
    }

    // init() declaration in the FooImpl trait.
    impl_trait_methods.push(quote! {
        fn init(&mut self) -> Result<(), Self::Error> where Self: 'static;
    });
    // init() implementation in the blanket impl.
    blanket_methods.push(quote! {
        fn init(&mut self) -> Result<(), Self::Error> where Self: 'static {
            let ctx = self;
            #(#init_stmts)*
            Ok(())
        }
    });

    // ── Plugin post ──────────────────────────────────────────────────────────
    let plugin_post = core.plugins.iter()
        .map(|p| p.post(core))
        .collect::<anyhow::Result<Vec<_>>>()?;

    // ── Plugin bounds ────────────────────────────────────────────────────────
    let plugin_bounds = core.plugins.iter()
        .map(|p| p.bounds(core))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let extra_bounds: Vec<TokenStream> = plugin_bounds.into_iter().flatten()
        .map(|b| quote! { + #b })
        .collect();

    let exref_bounds = core.plugins.iter()
        .map(|p| p.exref_bounds(core))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let extra_exref: Vec<TokenStream> = exref_bounds.into_iter().flatten()
        .map(|b| quote! { + #b })
        .collect();

    let async_bounds = if core.flags.contains(Flags::ASYNC) {
        quote! { + Send + Sync }
    } else {
        quote! {}
    };

    // ── Common preamble (Data + host trait) ─────────────────────────────────
    let preamble = quote! {
        pub struct #data_ty<Target: #name + ?Sized> {
            #(#data_fields),*
        }
        impl<Target: #name + ?Sized> Default for #data_ty<Target> {
            fn default() -> Self {
                Self { #(#defaults),* }
            }
        }
        impl<Target: #name + ?Sized> Clone for #data_ty<Target> {
            fn clone(&self) -> Self {
                Self { #(#clones),* }
            }
        }
        impl<Target: #name + ?Sized> #root::Traverse<Target> for #data_ty<Target> {
            fn traverse<'a>(
                &'a self,
            ) -> #alloc_ts::boxed::Box<dyn Iterator<Item = &'a Target::ExternRef> + 'a> {
                #alloc_ts::boxed::Box::new(
                    #root::_rexport::core::iter::empty()
                    #(#traverse_chain)*
                )
            }
            fn traverse_mut<'a>(
                &'a mut self,
            ) -> #alloc_ts::boxed::Box<dyn Iterator<Item = &'a mut Target::ExternRef> + 'a> {
                #alloc_ts::boxed::Box::new(
                    #root::_rexport::core::iter::empty()
                    #(#traverse_mut_chain)*
                )
            }
        }

        pub trait #name:
            #fp_ts::CtxSpec<ExternRef = Self::_ExternRef, Error = Self::_Error>
            #async_bounds
            #(#extra_bounds)*
        {
            type _ExternRef: Clone #(#extra_exref)*;
            type _Error: #root::_rexport::Error + 'static;
            #(#trait_methods)*
        }
    };

    if let Some(cctx) = chunk_ctx_owned {
        // ── Chunked output ────────────────────────────────────────────────────
        let mut chunk_mods: Vec<TokenStream> = vec![];
        let mut chunk_trait_names: Vec<Ident> = vec![];

        for chunk_idx in 0..cctx.n_chunks {
            let chunk_trait_name = format_ident!("{}Chunk{}", name, chunk_idx);
            let mod_name = format_ident!("_chunk{}", chunk_idx);
            chunk_trait_names.push(chunk_trait_name.clone());

            let cctx_ptr: *const ChunkCtx = &cctx;
            let chunk_ctx_pair = Some((cctx_ptr, chunk_idx));

            // Functions in this chunk.
            let mut trait_sigs: Vec<TokenStream> = vec![];
            let mut blanket_delegates: Vec<TokenStream> = vec![];
            let mut chunk_free_fns: Vec<TokenStream> = vec![];

            for func_idx in (m.n_func_imports..total_funcs)
                .filter(|&fi| cctx.chunk_of(fi) == chunk_idx)
            {
                let sig = m.func_sig(func_idx);
                let fname = m.fname(func_idx);
                trait_sigs.push(shared::render_self_sig_import(core, fname.clone(), sig.as_ref()));
                // Blanket delegate: call the local free fn (same module).
                blanket_delegates.push(shared::render_export(core, fname.clone(), fname.clone(), sig.as_ref()));
                // Free function with pub(super) visibility.
                let fn_ts = render_fn(core, m, func_idx, chunk_ctx_pair)?;
                chunk_free_fns.push(quote! { pub(super) #fn_ts });
            }

            chunk_mods.push(quote! {
                pub mod #mod_name {
                    use super::*;
                    pub trait #chunk_trait_name: super::#name {
                        #(#trait_sigs)*
                    }
                    const _: () = {
                        use #root::Memory;
                        impl<C: super::#name> #chunk_trait_name for C {
                            #(#blanket_delegates)*
                        }
                    };
                    #(#chunk_free_fns)*
                }
                pub use #mod_name::#chunk_trait_name;
            });
        }

        // FooImpl supertrait bounds: Foo + FooChunk0 + FooChunk1 + ...
        let chunk_super_bounds: Vec<TokenStream> = chunk_trait_names.iter()
            .map(|n| quote! { + #n })
            .collect();

        Ok(quote! {
            #preamble
            #(#chunk_mods)*
            pub trait #impl_trait: #name #(#chunk_super_bounds)* {
                #(#impl_trait_methods)*
            }
            const _: () = {
                use #root::Memory;
                impl<C: #name #(#chunk_super_bounds)*> #impl_trait for C {
                    #(#blanket_methods)*
                }
            };
            #(#plugin_post)*
        })
    } else {
        // ── Non-chunked output (original behavior) ────────────────────────────
        Ok(quote! {
            #preamble
            pub trait #impl_trait: #name {
                #(#impl_trait_methods)*
            }
            const _: () = {
                use #root::Memory;
                impl<C: #name> #impl_trait for C {
                    #(#blanket_methods)*
                }
                #(#free_fns)*
            };
            #(#plugin_post)*
        })
    }
}

// ─── Function reference helper ────────────────────────────────────────────────

fn render_fun_ref(core: &OptsCore<'_>, m: &ParsedModule, func_idx: u32, chunk_ctx: Option<&ChunkCtx>) -> TokenStream {
    let root = core.crate_path.clone();
    let fp_ts = fp(core);
    let sig = m.func_sig(func_idx);
    let ctx_ts = quote! { c };
    let generics = shared::render_generics(core, &ctx_ts, sig.as_ref());
    // When chunking, functions live in _chunkN sub-modules; emit a qualified path.
    let fname: TokenStream = if let Some(cctx) = chunk_ctx {
        if m.is_defined(func_idx) {
            let chunk = cctx.chunk_of(func_idx);
            let mod_name = format_ident!("_chunk{}", chunk);
            let raw = m.fname(func_idx);
            quote! { #mod_name::#raw }
        } else {
            let raw = m.fname(func_idx);
            quote! { #raw }
        }
    } else {
        let raw = m.fname(func_idx);
        quote! { #raw }
    };
    if core.flags.contains(Flags::ASYNC) {
        quote! {
            #fp_ts::da::<#generics, C, _>(|ctx, arg| {
                #root::func::unsync::AsyncRec::wrap(#fname(ctx, arg))
            })
        }
    } else {
        quote! {
            #fp_ts::da::<#generics, C, _>(|ctx, arg| match #fname(ctx, arg) {
                res => res
            })
        }
    }
}

// ─── Function body emission ───────────────────────────────────────────────────

fn render_fn(core: &OptsCore<'_>, m: &ParsedModule, func_idx: u32, chunk_ctx: Option<(*const ChunkCtx, usize)>) -> anyhow::Result<TokenStream> {
    let sig = m.func_sig(func_idx).clone();
    let fname = m.fname(func_idx);
    let sig_ts = shared::render_fn_sig(core, fname.clone(), sig.as_ref());
    let root = core.crate_path.clone();
    let fp_ts = fp(core);
    let alloc_ts = alloc(core);

    // Imported function: delegate to ctx method.
    if !m.is_defined(func_idx) {
        let imp = m.import_for_func(func_idx).unwrap();
        let mname = format_ident!("{}_{}", bindname(&imp.module), bindname(&imp.name));
        // Check if any plugin handles this import.
        let params: Vec<Ident> = (0..sig.params.len()).map(|i| format_ident!("p{i}")).collect();
        // Check plugins.
        let plugin_result: Option<TokenStream> = core.plugins.iter()
            .find_map(|p| {
                p.import(core, &imp.module, &imp.name,
                    params.iter().map(|id| quote! { #id }).collect())
                    .ok()
                    .flatten()
            });
        let body = if let Some(ts) = plugin_result {
            if core.flags.contains(Flags::ASYNC) {
                quote! {
                    return #alloc_ts::boxed::Box::pin(async move { #ts })
                }
            } else {
                quote! { return #ts; }
            }
        } else {
            let call = quote! {
                ctx.#mname(#root::_rexport::tuple_list::tuple_list!(#(#params),*))
            };
            if core.flags.contains(Flags::ASYNC) {
                quote! {
                    return #alloc_ts::boxed::Box::pin(async move {
                        #call.go().await
                    })
                }
            } else {
                quote! { return #call; }
            }
        };
        return Ok(quote! { #sig_ts { #body } });
    }

    let alloc_ts = alloc(core);

    // Defined function: emit body.
    let def_idx = (func_idx - m.n_func_imports) as usize;
    let (locals_decl, op_bytes) = &m.defined_bodies[def_idx];

    // Build the flat local variable list.
    // Params come first (local_0 … local_{nparams-1}), then declared locals.
    let param_count = sig.params.len();
    let mut local_types: Vec<ValType> = sig.params.to_vec();
    for (count, ty) in locals_decl {
        for _ in 0..*count {
            local_types.push(*ty);
        }
    }

    // Emit `let mut local_N: T = default;` for every local beyond params.
    let mut local_decls: Vec<TokenStream> = vec![];
    for (i, ty) in local_types.iter().enumerate() {
        let ln = format_ident!("local_{i}");
        let t = shared::render_ty(core, &quote! { C }, *ty);
        if i < param_count {
            let pi = format_ident!("p{i}");
            local_decls.push(quote! { let mut #ln: #t = #pi; });
        } else {
            local_decls.push(quote! { let mut #ln: #t = Default::default(); });
        }
    }

    // Now emit the operator stream as structured Rust.
    let body_ts = emit_body(core, m, func_idx, &local_types, op_bytes, chunk_ctx)?;

    let inner = quote! {
        #(#local_decls)*
        #body_ts
        unreachable!("wasm function {} fell off end", #func_idx);
    };

    let full_body = if core.flags.contains(Flags::ASYNC) {
        quote! {
            return #alloc_ts::boxed::Box::pin(async move {
                #inner
            });
        }
    } else {
        inner
    };

    Ok(quote! {
        #sig_ts {
            #full_body
        }
    })
}

// ─── Operator → TokenStream ───────────────────────────────────────────────────

/// State for one function body emission.
struct EmitCtx<'a> {
    core: &'a OptsCore<'a>,
    m: &'a ParsedModule,
    func_idx: u32,
    local_types: &'a [ValType],
    /// Operand stack — each entry is a token expression (either a local ident
    /// or an SSA temp ident).
    stack: Vec<TokenStream>,
    /// Block stack for control flow.
    frames: Vec<Frame>,
    /// Counter for fresh temp names.
    tmp_counter: usize,
    /// Counter for block label names.
    label_counter: usize,
    /// If > 0 we are in an unreachable region; suppress output.
    unreachable_depth: usize,
    /// Set when the current End closes a frame exited via unreachable code
    /// (function-End must not re-emit a trailing result capture).
    end_after_unreachable: bool,
    /// Output buffer stack: `out_stack.last_mut()` is where we currently write.
    /// Pushed on Block/Loop/If entry, popped and merged on End/Else.
    out_stack: Vec<Vec<TokenStream>>,
    /// Chunk context for cross-chunk call path generation. None = no chunking.
    /// Tuple: (chunk assignment table, index of the chunk currently being emitted).
    chunk_ctx: Option<(*const ChunkCtx, usize)>,
}

struct Frame {
    kind: FrameKind,
    /// Rust lifetime label index (used for 'lN).
    label: usize,
    /// Result types of the block.
    result_tys: Vec<ValType>,
    /// Temp ident used to carry block results out (for block/if).
    result_tmp: Option<Ident>,
    /// Function-scoped temps for multi-result blocks.
    result_tmps: Vec<Ident>,
    /// Stack height at block entry (for restoring stack on else/end).
    stack_height: usize,
    /// For If frames: the condition token stream.
    condition: Option<TokenStream>,
    /// For If/Else: tokens accumulated in the *if* branch before Else was seen.
    if_stmts: Option<Vec<TokenStream>>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum FrameKind {
    Block,
    Loop,
    If,
    Else,
}

impl<'a> EmitCtx<'a> {
    fn new(
        core: &'a OptsCore<'a>,
        m: &'a ParsedModule,
        func_idx: u32,
        local_types: &'a [ValType],
    ) -> Self {
        Self {
            core,
            m,
            func_idx,
            local_types,
            stack: vec![],
            frames: vec![],
            tmp_counter: 0,
            label_counter: 0,
            unreachable_depth: 0,
            end_after_unreachable: false,
            out_stack: vec![vec![]],
            chunk_ctx: None,
        }
    }

    fn fresh_tmp(&mut self) -> Ident {
        let n = self.tmp_counter;
        self.tmp_counter += 1;
        format_ident!("_t{n}")
    }

    fn fresh_label(&mut self) -> usize {
        let n = self.label_counter;
        self.label_counter += 1;
        n
    }

    fn push(&mut self, ts: TokenStream) {
        self.stack.push(ts);
    }

    fn pop(&mut self) -> TokenStream {
        self.stack.pop().unwrap_or_else(|| quote! { Default::default() })
    }

    fn peek(&self) -> TokenStream {
        self.stack.last().cloned().unwrap_or_else(|| quote! { Default::default() })
    }

    /// Append a statement to the current output buffer.
    fn emit(&mut self, ts: TokenStream) {
        if self.unreachable_depth == 0 {
            if let Some(buf) = self.out_stack.last_mut() {
                buf.push(ts);
            }
        }
    }

    /// Push a value and emit `let _tN = <expr>;`.
    fn push_tmp(&mut self, expr: TokenStream) -> Ident {
        let tmp = self.fresh_tmp();
        self.emit(quote! { let #tmp = #expr; });
        self.push(quote! { #tmp });
        tmp
    }

    /// Drain the current output buffer and return it.
    fn drain_buf(&mut self) -> Vec<TokenStream> {
        if let Some(buf) = self.out_stack.last_mut() {
            std::mem::take(buf)
        } else {
            vec![]
        }
    }

    /// Push a new (empty) output buffer onto the stack.
    fn push_buf(&mut self) {
        self.out_stack.push(vec![]);
    }

    /// Pop the topmost output buffer and return it.
    fn pop_buf(&mut self) -> Vec<TokenStream> {
        self.out_stack.pop().unwrap_or_default()
    }

    /// Lifetime label for frame `depth` (0 = innermost).
    fn lifetime_for_depth(&self, depth: usize) -> Lifetime {
        let idx = self.frames.len().saturating_sub(depth + 1);
        let label = if idx < self.frames.len() {
            self.frames[idx].label
        } else {
            0
        };
        Lifetime::new(&format!("'l{label}"), Span::call_site())
    }

    fn fp(&self) -> TokenStream { fp(self.core) }
    fn root(&self) -> &syn::Path { &self.core.crate_path }
    fn alloc(&self) -> TokenStream { alloc(self.core) }

    /// Collect the final output as a single TokenStream.
    fn finish(mut self) -> TokenStream {
        let stmts = self.out_stack.pop().unwrap_or_default();
        quote! { #(#stmts)* }
    }
}

fn emit_body(
    core: &OptsCore<'_>,
    m: &ParsedModule,
    func_idx: u32,
    local_types: &[ValType],
    op_bytes: &[u8],
    chunk_ctx: Option<(*const ChunkCtx, usize)>,
) -> anyhow::Result<TokenStream> {
    let mut ctx = EmitCtx::new(core, m, func_idx, local_types);
    ctx.chunk_ctx = chunk_ctx;
    let sig = m.func_sig(func_idx);

    // Outer frame: the function body itself.
    let fn_label = ctx.fresh_label();
    ctx.push_buf();
    ctx.frames.push(Frame {
        kind: FrameKind::Block,
        label: fn_label,
        result_tys: sig.returns.to_vec(),
        result_tmp: None, // functions return via `return`, not block-result
        result_tmps: vec![],
        stack_height: 0,
        condition: None,
        if_stmts: None,
    });

    // Re-parse function body from stored bytes.
    // `op_bytes` is the full body bytes (includes locals varint prefix).
    let body = wasmparser::FunctionBody::new(wasmparser::BinaryReader::new(op_bytes, 0));
    let mut ops_reader = body.get_operators_reader()?;
    while !ops_reader.eof() {
        let op = ops_reader.read()?;
        process_op(&mut ctx, op)?;
    }

    Ok(ctx.finish())
}

fn br_target(ctx: &EmitCtx<'_>, depth: usize) -> TokenStream {
    let fp_ts = fp(ctx.core);
    let idx = ctx.frames.len().saturating_sub(depth + 1);
    if idx >= ctx.frames.len() {
        return quote! { return; };
    }
    let frame = &ctx.frames[idx];
    let lt = Lifetime::new(&format!("'l{}", frame.label), Span::call_site());
    match frame.kind {
        FrameKind::Loop => {
            // A br to a loop jumps back to the loop start: no result.
            quote! { continue #lt; }
        }
        _ => {
            // A br to a block/if carries the block's results. If the frame
            // has a result temp, the value must be on the operand stack:
            // assign it before breaking (the normal block-end assignment is
            // skipped by the break).
            let assign = frame.result_tmp.as_ref().and_then(|rt| {
                // The result value is the top of the operand stack *at br
                // time* relative to this frame's entry height.
                let n_vals = ctx.stack.len().checked_sub(frame.stack_height).unwrap_or(0);
                let n_results = frame.result_tys.len();
                if n_results == 0 || n_vals < n_results {
                    return None;
                }
                let val = ctx.stack[ctx.stack.len() - n_results].clone();
                Some(quote! { #rt = #fp_ts::cast::<_,_,C>(#val); })
            });
            quote! { #assign break #lt; }
        }
    }
}

fn process_op(ctx: &mut EmitCtx<'_>, op: Operator<'_>) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let alloc_ts = ctx.alloc();

    // Handle unreachable tracking for control structures regardless.
    match &op {
        Operator::Block { .. } | Operator::Loop { .. } | Operator::If { .. } => {
            if ctx.unreachable_depth > 0 {
                ctx.unreachable_depth += 1;
                ctx.push_buf(); // match every End pop
                ctx.frames.push(Frame { // placeholder to balance End
                    kind: FrameKind::Block,
                    label: 0,
                    result_tys: vec![],
                    result_tmp: None,
                    result_tmps: vec![],
                    stack_height: 0,
                    condition: None,
                    if_stmts: None,
                });
                return Ok(());
            }
        }
        Operator::Else => {
            if ctx.unreachable_depth > 1 {
                // still inside nested unreachable
                return Ok(());
            }
            if ctx.unreachable_depth == 1 {
                // The if-branch was unreachable, but else might be reachable.
                ctx.unreachable_depth = 0;
                // Snapshot if-branch (empty/unreachable) and start else buffer.
                let if_body = ctx.pop_buf();
                {
                    let frame = ctx.frames.last_mut().expect("else without frame");
                    frame.if_stmts = Some(if_body);
                    frame.kind = FrameKind::Else;
                }
                ctx.push_buf();
                return Ok(()); // fall through to normal code from here
            }
        }
        Operator::End => {
            if ctx.unreachable_depth > 0 {
                ctx.unreachable_depth -= 1;
                if ctx.unreachable_depth > 0 {
                    return Ok(());
                }
                // Depth reaches 0: this End closes the block the `br`/
                // `return` jumped out of. The block's buffer still holds
                // live declarations and the branch's result assignment, so
                // we must NOT discard it; fall through to the normal End
                // handling below, remembering we came from unreachable code
                // (the function-End capture must be skipped).
                ctx.end_after_unreachable = true;
            }
        }
        _ if ctx.unreachable_depth > 0 => return Ok(()),
        _ => {}
    }

    match op {
        // ── Constants ────────────────────────────────────────────────────────
        Operator::I32Const { value } => {
            ctx.push_tmp(quote! { (#value as u32) });
        }
        Operator::I64Const { value } => {
            ctx.push_tmp(quote! { (#value as u64) });
        }
        Operator::F32Const { value } => {
            let bits = value.bits();
            ctx.push_tmp(quote! { f32::from_bits(#bits) });
        }
        Operator::F64Const { value } => {
            let bits = value.bits();
            ctx.push_tmp(quote! { f64::from_bits(#bits) });
        }

        // ── Locals ───────────────────────────────────────────────────────────
        Operator::LocalGet { local_index } => {
            let ln = format_ident!("local_{local_index}");
            ctx.push(quote! { #ln });
        }
        Operator::LocalSet { local_index } => {
            let val = ctx.pop();
            let ln = format_ident!("local_{local_index}");
            ctx.emit(quote! { #ln = #fp_ts::cast::<_,_,C>(#val); });
        }
        Operator::LocalTee { local_index } => {
            let val = ctx.peek();
            let ln = format_ident!("local_{local_index}");
            ctx.emit(quote! { #ln = #fp_ts::cast::<_,_,C>(#val); });
        }

        // ── Globals ──────────────────────────────────────────────────────────
        Operator::GlobalGet { global_index } => {
            let gn = format_ident!("global{global_index}");
            // Clone: ref-typed globals hold non-Copy Value wrappers.
            ctx.push_tmp(quote! { (*ctx.#gn()).clone() });
        }
        Operator::GlobalSet { global_index } => {
            let val = ctx.pop();
            let gn = format_ident!("global{global_index}");
            ctx.emit(quote! { *ctx.#gn() = #fp_ts::cast::<_,_,C>(#val); });
        }

        // ── Drop / Select ─────────────────────────────────────────────────────
        Operator::Drop => { ctx.pop(); }
        Operator::Select | Operator::TypedSelect { .. } => {
            let cond = ctx.pop();
            let b   = ctx.pop();
            let a   = ctx.pop();
            ctx.push_tmp(quote! { if #cond != 0u32 { #a } else { #fp_ts::cast::<_,_,C>(#b) } });
        }

        // ── Unreachable / Nop ─────────────────────────────────────────────────
        Operator::Unreachable => {
            ctx.emit(quote! { unreachable!(); });
            ctx.unreachable_depth = 1;
        }
        Operator::Nop => {}

        // ── Memory ───────────────────────────────────────────────────────────
        Operator::MemorySize { mem } => {
            let mn = format_ident!("memory{mem}");
            let mem_ty = &ctx.m.memory_types[mem as usize];
            let page_size = 65536u64;
            let rt = if mem_ty.memory64 { quote! { u64 } } else { quote! { u32 } };
            ctx.push_tmp(quote! {
                ((match #root::Memory::<C::Error>::size(ctx.#mn()) {
                    Ok(a) => a,
                    Err(e) => return #fp_ts::ret(Err(e)),
                }) / #page_size) as #rt
            });
        }
        Operator::MemoryGrow { mem } => {
            let mn = format_ident!("memory{mem}");
            let mem_ty = &ctx.m.memory_types[mem as usize];
            let page_size = 65536u64;
            let rt = if mem_ty.memory64 { quote! { u64 } } else { quote! { u32 } };
            let delta = ctx.pop();
            ctx.push_tmp(quote! {{
                let _old = match #root::Memory::<C::Error>::size(ctx.#mn()) {
                    Ok(a) => a,
                    Err(e) => return #fp_ts::ret(Err(e)),
                };
                match #root::Memory::<C::Error>::grow(ctx.#mn(), (#delta as u64) * #page_size) {
                    Ok(_) => {}
                    Err(e) => return #fp_ts::ret(Err(e)),
                };
                (_old / #page_size) as #rt
            }});
        }
        Operator::MemoryCopy { dst_mem, src_mem } => {
            let dmn = format_ident!("memory{dst_mem}");
            let smn = format_ident!("memory{src_mem}");
            let len = ctx.pop();
            let src_ptr = ctx.pop();
            let dst_ptr = ctx.pop();
            ctx.emit(quote! {
                {
                    let _mc_buf = match #root::Memory::<Self::_Error>::read(ctx.#smn(), #src_ptr as u64, #len as u64) {
                        Ok(a) => a.as_ref().as_ref().to_owned(),
                        Err(e) => return #fp_ts::ret(Err(e)),
                    };
                    match #root::Memory::<Self::_Error>::write(ctx.#dmn(), #dst_ptr as u64, &_mc_buf) {
                        Ok(()) => {}
                        Err(e) => return #fp_ts::ret(Err(e)),
                    }
                }
            });
        }
        Operator::MemoryFill { mem } => {
            let mn = format_ident!("memory{mem}");
            let len = ctx.pop();
            let val = ctx.pop();
            let dst = ctx.pop();
            ctx.emit(quote! {
                {
                    let _mf_buf = #alloc_ts::vec![(#val & 0xffu32) as u8; #len as usize];
                    match #root::Memory::<Self::_Error>::write(ctx.#mn(), #dst as u64, &_mf_buf) {
                        Ok(()) => {}
                        Err(e) => return #fp_ts::ret(Err(e)),
                    }
                }
            });
        }

        // ── Load / Store (handled uniformly below by matching op name) ────────
        // i32 loads
        Operator::I32Load { memarg } => emit_load(ctx, "i32load", memarg, 0)?,
        Operator::I32Load8S { memarg } => emit_load(ctx, "i32load8s", memarg, 0)?,
        Operator::I32Load8U { memarg } => emit_load(ctx, "i32load8u", memarg, 0)?,
        Operator::I32Load16S { memarg } => emit_load(ctx, "i32load16s", memarg, 0)?,
        Operator::I32Load16U { memarg } => emit_load(ctx, "i32load16u", memarg, 0)?,
        // i64 loads
        Operator::I64Load { memarg } => emit_load(ctx, "i64load", memarg, 0)?,
        Operator::I64Load8S { memarg } => emit_load(ctx, "i64load8s", memarg, 0)?,
        Operator::I64Load8U { memarg } => emit_load(ctx, "i64load8u", memarg, 0)?,
        Operator::I64Load16S { memarg } => emit_load(ctx, "i64load16s", memarg, 0)?,
        Operator::I64Load16U { memarg } => emit_load(ctx, "i64load16u", memarg, 0)?,
        Operator::I64Load32S { memarg } => emit_load(ctx, "i64load32s", memarg, 0)?,
        Operator::I64Load32U { memarg } => emit_load(ctx, "i64load32u", memarg, 0)?,
        // f32/f64 loads (use i32/i64 load then bitcast)
        Operator::F32Load { memarg } => emit_load_f(ctx, false, memarg)?,
        Operator::F64Load { memarg } => emit_load_f(ctx, true, memarg)?,
        // i32 stores
        Operator::I32Store { memarg } => emit_store(ctx, "i32store", memarg, 0)?,
        Operator::I32Store8 { memarg } => emit_store(ctx, "i32store8", memarg, 0)?,
        Operator::I32Store16 { memarg } => emit_store(ctx, "i32store16", memarg, 0)?,
        // i64 stores
        Operator::I64Store { memarg } => emit_store(ctx, "i64store", memarg, 0)?,
        Operator::I64Store8 { memarg } => emit_store(ctx, "i64store8", memarg, 0)?,
        Operator::I64Store16 { memarg } => emit_store(ctx, "i64store16", memarg, 0)?,
        Operator::I64Store32 { memarg } => emit_store(ctx, "i64store32", memarg, 0)?,
        // f32/f64 stores (bitcast then integer store)
        Operator::F32Store { memarg } => emit_store_f(ctx, false, memarg)?,
        Operator::F64Store { memarg } => emit_store_f(ctx, true, memarg)?,

        // ── Numeric: i32 ─────────────────────────────────────────────────────
        Operator::I32Add => bin_op(ctx, "i32add"),
        Operator::I32Sub => bin_op(ctx, "i32sub"),
        Operator::I32Mul => bin_op(ctx, "i32mul"),
        Operator::I32DivS => bin_op(ctx, "i32divs"),
        Operator::I32DivU => bin_op(ctx, "i32divu"),
        Operator::I32RemS => bin_op(ctx, "i32rems"),
        Operator::I32RemU => bin_op(ctx, "i32remu"),
        Operator::I32And => bin_op(ctx, "i32and"),
        Operator::I32Or  => bin_op(ctx, "i32or"),
        Operator::I32Xor => bin_op(ctx, "i32xor"),
        Operator::I32Shl => bin_op(ctx, "i32shl"),
        Operator::I32ShrS => bin_op(ctx, "i32shrs"),
        Operator::I32ShrU => bin_op(ctx, "i32shru"),
        Operator::I32Rotl => bin_op(ctx, "i32rotl"),
        Operator::I32Rotr => {
            let b = ctx.pop(); let a = ctx.pop();
            ctx.push_tmp(quote! { (#a.rotate_right((#b & 0xffffffff) as u32)) });
        }
        Operator::I32Clz => un_op(ctx, "i32clz"),
        Operator::I32Ctz => un_op(ctx, "i32ctz"),
        Operator::I32Popcnt => {
            let a = ctx.pop();
            ctx.push_tmp(quote! { (#a.count_ones() as u32) });
        }
        Operator::I32Eqz => un_op(ctx, "i32eqz"),
        Operator::I32Eq => bin_op(ctx, "i32eq"),
        Operator::I32Ne => bin_op(ctx, "i32ne"),
        Operator::I32LtS => bin_op(ctx, "i32lts"),
        Operator::I32LtU => bin_op(ctx, "i32ltu"),
        Operator::I32GtS => bin_op(ctx, "i32gts"),
        Operator::I32GtU => bin_op(ctx, "i32gtu"),
        Operator::I32LeS => bin_op(ctx, "i32les"),
        Operator::I32LeU => bin_op(ctx, "i32leu"),
        Operator::I32GeS => bin_op(ctx, "i32ges"),
        Operator::I32GeU => bin_op(ctx, "i32geu"),

        // ── Numeric: i64 ─────────────────────────────────────────────────────
        Operator::I64Add => bin_op(ctx, "i64add"),
        Operator::I64Sub => bin_op(ctx, "i64sub"),
        Operator::I64Mul => bin_op(ctx, "i64mul"),
        Operator::I64DivS => bin_op(ctx, "i64divs"),
        Operator::I64DivU => bin_op(ctx, "i64divu"),
        Operator::I64RemS => bin_op(ctx, "i64rems"),
        Operator::I64RemU => bin_op(ctx, "i64remu"),
        Operator::I64And => bin_op(ctx, "i64and"),
        Operator::I64Or  => bin_op(ctx, "i64or"),
        Operator::I64Xor => bin_op(ctx, "i64xor"),
        Operator::I64Shl => bin_op(ctx, "i64shl"),
        Operator::I64ShrS => bin_op(ctx, "i64shrs"),
        Operator::I64ShrU => bin_op(ctx, "i64shru"),
        Operator::I64Rotl => bin_op(ctx, "i64rotl"),
        Operator::I64Rotr => {
            let b = ctx.pop(); let a = ctx.pop();
            ctx.push_tmp(quote! { (#a.rotate_right((#b & 0xffffffffffffffff) as u32)) });
        }
        Operator::I64Clz => un_op(ctx, "i64clz"),
        Operator::I64Ctz => un_op(ctx, "i64ctz"),
        Operator::I64Popcnt => {
            let a = ctx.pop();
            ctx.push_tmp(quote! { (#a.count_ones() as u64) });
        }
        Operator::I64Eqz => un_op(ctx, "i64eqz"),
        Operator::I64Eq => bin_op(ctx, "i64eq"),
        Operator::I64Ne => bin_op(ctx, "i64ne"),
        Operator::I64LtS => bin_op(ctx, "i64lts"),
        Operator::I64LtU => bin_op(ctx, "i64ltu"),
        Operator::I64GtS => bin_op(ctx, "i64gts"),
        Operator::I64GtU => bin_op(ctx, "i64gtu"),
        Operator::I64LeS => bin_op(ctx, "i64les"),
        Operator::I64LeU => bin_op(ctx, "i64leu"),
        Operator::I64GeS => bin_op(ctx, "i64ges"),
        Operator::I64GeU => bin_op(ctx, "i64geu"),

        // ── Conversions ──────────────────────────────────────────────────────
        Operator::I32WrapI64 => un_op(ctx, "i32wrapi64"),
        Operator::I64ExtendI32S => un_op(ctx, "i64extendi32s"),
        Operator::I64ExtendI32U => un_op(ctx, "i64extendi32u"),
        Operator::I64TruncF64S => un_op(ctx, "i64truncf64s"),

        // ── Calls ────────────────────────────────────────────────────────────
        Operator::Call { function_index } => {
            let sig = ctx.m.func_sig(function_index);
            let mut args = vec![];
            for _ in 0..sig.params.len() {
                args.push(ctx.pop());
            }
            args.reverse();

            // Resolve call target, taking chunk boundaries into account.
            // SAFETY: chunk_ctx raw pointer comes from a ChunkCtx that lives
            // in emit(), which outlives emit_body() and thus this EmitCtx.
            let call = if let Some((cctx_ptr, cur_chunk)) = ctx.chunk_ctx {
                let cctx = unsafe { &*cctx_ptr };
                if cctx.is_import(function_index) {
                    // Imported function: call ctx method directly (no wrapper fn).
                    let imp = ctx.m.import_for_func(function_index).unwrap();
                    let mname = format_ident!("{}_{}", bindname(&imp.module), bindname(&imp.name));
                    let call_expr = quote! { ctx.#mname(#root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args)),*)) };
                    if ctx.core.flags.contains(Flags::ASYNC) {
                        quote! { #call_expr.go().await }
                    } else {
                        quote! { #root::_rexport::tramp::tramp(#call_expr) }
                    }
                } else {
                    let target_chunk = cctx.chunk_of(function_index);
                    let fname: TokenStream = if target_chunk == cur_chunk {
                        let f = ctx.m.fname(function_index);
                        quote! { #f }
                    } else {
                        let mod_name = format_ident!("_chunk{}", target_chunk);
                        let f = ctx.m.fname(function_index);
                        quote! { super::#mod_name::#f }
                    };
                    if ctx.core.flags.contains(Flags::ASYNC) {
                        quote! { #fname(ctx, #root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args)),*)).await }
                    } else {
                        quote! { #root::_rexport::tramp::tramp(#fname(ctx, #root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args)),*))) }
                    }
                }
            } else {
                let fname = ctx.m.fname(function_index);
                if ctx.core.flags.contains(Flags::ASYNC) {
                    quote! { #fname(ctx, #root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args)),*)).await }
                } else {
                    quote! { #root::_rexport::tramp::tramp(#fname(ctx, #root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args)),*))) }
                }
            };
            if sig.returns.is_empty() {
                ctx.emit(quote! {
                    match #call {
                        Ok(()) => {}
                        Err(e) => return #fp_ts::ret(Err(e)),
                    }
                });
            } else {
                let tmp = ctx.fresh_tmp();
                ctx.emit(quote! {
                    let #root::_rexport::tuple_list::tuple_list!(#tmp) = match #call {
                        Ok(a) => a,
                        Err(e) => return #fp_ts::ret(Err(e)),
                    };
                });
                ctx.push(quote! { #tmp });
            }
        }

        // ── Table ────────────────────────────────────────────────────────────
        Operator::TableGet { table } => {
            let tn = format_ident!("table{table}");
            let idx = ctx.pop();
            ctx.push_tmp(quote! { ctx.#tn()[#idx as usize].clone() });
        }
        Operator::TableSet { table } => {
            let tn = format_ident!("table{table}");
            let val = ctx.pop();
            let idx = ctx.pop();
            ctx.emit(quote! { ctx.#tn()[#idx as usize] = #fp_ts::cast::<_,_,C>(#val); });
        }
        Operator::TableSize { table } => {
            let tn = format_ident!("table{table}");
            ctx.push_tmp(quote! { (ctx.#tn().len() as u32) });
        }
        Operator::TableGrow { table } => {
            let tn = format_ident!("table{table}");
            let delta = ctx.pop();
            let val = ctx.pop();
            ctx.push_tmp(quote! {{
                let _old = ctx.#tn().len() as u32;
                for _ in 0..#delta {
                    ctx.#tn().push(#fp_ts::cast::<_,_,C>(#val.clone()));
                }
                _old
            }});
        }
        Operator::TableCopy { dst_table, src_table } => {
            let dtn = format_ident!("table{dst_table}");
            let stn = format_ident!("table{src_table}");
            let len = ctx.pop();
            let src = ctx.pop();
            let dst = ctx.pop();
            ctx.emit(quote! {
                for _tc_i in 0..#len {
                    let _tc_v = ctx.#stn()[(#src + _tc_i) as usize].clone();
                    ctx.#dtn()[(#dst + _tc_i) as usize] = _tc_v;
                }
            });
        }

        // ── Control flow ──────────────────────────────────────────────────────
        Operator::Block { blockty } => {
            let label = ctx.fresh_label();
            let result_tys = blocktype_results(ctx.m, blockty);
            let result_tmp = if result_tys.is_empty() {
                None
            } else {
                let t = format_ident!("_b{label}");
                let ty = shared::render_ty(ctx.core, &quote! { C }, result_tys[0]);
                // Declare at function level so the ident outlives nested
                // labeled scopes (mirrors push_tmp scope widening).
                if ctx.out_stack.len() > 1 {
                    if let Some(root_buf) = ctx.out_stack.first_mut() {
                        root_buf.push(quote! { let mut #t: #ty = Default::default(); });
                    }
                } else {
                    ctx.emit(quote! { let mut #t: #ty = Default::default(); });
                }
                Some(t)
            };
            let result_tmps: Vec<Ident> = if result_tys.len() > 1 {
                result_tys.iter().enumerate().map(|(i, ty)| {
                    let t = format_ident!("_b{label}_{i}");
                    let ty = shared::render_ty(ctx.core, &quote! { C }, *ty);
                    if ctx.out_stack.len() > 1 {
                        if let Some(root_buf) = ctx.out_stack.first_mut() {
                            root_buf.push(quote! { let mut #t: #ty = Default::default(); });
                        }
                    } else {
                        ctx.emit(quote! { let mut #t: #ty = Default::default(); });
                    }
                    t
                }).collect()
            } else {
                vec![]
            };
            let sh = ctx.stack.len();
            ctx.push_buf();
            ctx.frames.push(Frame {
                kind: FrameKind::Block,
                label,
                result_tys: result_tys.clone(),
                result_tmp,
                result_tmps,
                stack_height: sh,
                condition: None,
                if_stmts: None,
            });
        }
        Operator::Loop { blockty } => {
            let label = ctx.fresh_label();
            let result_tys = blocktype_results(ctx.m, blockty);
            let result_tmp = if result_tys.is_empty() {
                None
            } else {
                let t = format_ident!("_b{label}");
                let ty = shared::render_ty(ctx.core, &quote! { C }, result_tys[0]);
                if ctx.out_stack.len() > 1 {
                    if let Some(root_buf) = ctx.out_stack.first_mut() {
                        root_buf.push(quote! { let mut #t: #ty = Default::default(); });
                    }
                } else {
                    ctx.emit(quote! { let mut #t: #ty = Default::default(); });
                }
                Some(t)
            };
            let sh = ctx.stack.len();
            ctx.push_buf();
            ctx.frames.push(Frame {
                kind: FrameKind::Loop,
                label,
                result_tys,
                result_tmp,
                result_tmps: vec![],
                stack_height: sh,
                condition: None,
                if_stmts: None,
            });
        }
        Operator::If { blockty } => {
            let cond = ctx.pop();
            let label = ctx.fresh_label();
            let result_tys = blocktype_results(ctx.m, blockty);
            let result_tmp = if result_tys.is_empty() {
                None
            } else {
                let t = format_ident!("_b{label}");
                let ty = shared::render_ty(ctx.core, &quote! { C }, result_tys[0]);
                if ctx.out_stack.len() > 1 {
                    if let Some(root_buf) = ctx.out_stack.first_mut() {
                        root_buf.push(quote! { let mut #t: #ty = Default::default(); });
                    }
                } else {
                    ctx.emit(quote! { let mut #t: #ty = Default::default(); });
                }
                Some(t)
            };
            let sh = ctx.stack.len();
            ctx.push_buf();
            ctx.frames.push(Frame {
                kind: FrameKind::If,
                label,
                result_tys,
                result_tmp,
                result_tmps: vec![],
                stack_height: sh,
                condition: Some(cond),
                if_stmts: None,
            });
        }
        Operator::Else => {
            // Snapshot the if-branch buffer, start a fresh else buffer.
            let if_body = ctx.pop_buf();
            let frame = ctx.frames.last_mut().expect("else without frame");
            frame.if_stmts = Some(if_body);
            frame.kind = FrameKind::Else;
            // The else branch starts with the stack as it was at `if`.
            ctx.stack.truncate(frame.stack_height);
            ctx.push_buf();
        }
        Operator::End => {
            if let Some(frame) = ctx.frames.pop() {
                let body = ctx.pop_buf();
                let stmts = quote! { #(#body)* };


                if ctx.frames.is_empty() {
                    // Function end. If the body already terminated with an
                    // explicit `return`/`unreachable`, the trailing capture
                    // has nothing live to return — emit the body only.
                    if ctx.end_after_unreachable {
                        ctx.emit(quote! { #stmts });
                        return Ok(());
                    }
                    let sig = ctx.m.func_sig(ctx.func_idx);
                    let mut vals = vec![];
                    for _ in 0..sig.returns.len() {
                        vals.push(ctx.pop());
                    }
                    vals.reverse();
                    let fp_ts = ctx.fp();
                    let root = ctx.root().clone();
                    let ret = quote! {
                        return #fp_ts::ret(Ok::<_, C::Error>(#root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#vals)),*)));
                    };
                    ctx.emit(quote! {
                        #stmts
                        #ret
                    });
                    return Ok(());
                }

                match frame.kind {
                    FrameKind::Block => {
                        let lt = Lifetime::new(&format!("'l{}", frame.label), Span::call_site());
                        // Capture the block results (top n of the stack),
                        // truncate back to the block-entry height, then
                        // re-push the captured values — mirrors wasm
                        // `br`-to-function-end stack discipline.
                        let n = frame.result_tys.len();
                        let captured: Vec<TokenStream> = if n > 0 {
                            let start = ctx.stack.len().saturating_sub(n);
                            ctx.stack[start..].to_vec()
                        } else {
                            vec![]
                        };
                        let result_assign = frame.result_tmp.as_ref().and_then(|rt| {
                            captured.last().map(|val| {
                                let val = val.clone();
                                quote! { #rt = #fp_ts::cast::<_,_,C>(#val); }
                            })
                        }).unwrap_or_default();
                        let sh = frame.stack_height;
                        let inner_assigns = frame.result_tmps.iter().zip(captured.iter()).map(|(t, v)| {
                            quote! { #t = #fp_ts::cast::<_,_,C>(#v); }
                        });
                        ctx.emit(quote! {
                            #lt: {
                                #stmts
                                #result_assign
                                #(#inner_assigns)*
                            }
                        });
                        if n == 1 {
                            ctx.stack.truncate(sh);
                            if let Some(rt) = frame.result_tmp {
                                ctx.push(quote! { #rt });
                            }
                        } else if n > 1 {
                            // Multi-result blocks: assign each captured value
                            // to a function-scoped temp (declared at block
                            // entry) so idents stay in scope after the block.
                            let keep = captured.clone();
                            ctx.stack.truncate(ctx.stack.len() - keep.len());
                            for t in frame.result_tmps.clone() {
                                ctx.push(quote! { #t });
                            }
                        }
                    }
                    FrameKind::Loop => {
                        let lt = Lifetime::new(&format!("'l{}", frame.label), Span::call_site());
                        // Assign the loop's result (if any) from the stack top
                        // before breaking out of the loop body.
                        let result_assign = frame.result_tmp.as_ref().and_then(|rt| {
                            ctx.stack.last().map(|val| {
                                let val = val.clone();
                                quote! { #rt = #fp_ts::cast::<_,_,C>(#val); }
                            })
                        }).unwrap_or_default();
                        let sh = frame.stack_height;
                        ctx.emit(quote! {
                            #lt: loop {
                                #stmts
                                #result_assign
                                break;
                            }
                        });
                        ctx.stack.truncate(sh);
                        if let Some(rt) = frame.result_tmp {
                            ctx.push(quote! { #rt });
                        }
                    }
                    FrameKind::If => {
                        // if without else
                        let cond = frame.condition.clone().unwrap_or(quote! { 0u32 });
                        let result_assign = frame.result_tmp.as_ref().and_then(|rt| {
                            ctx.stack.last().map(|val| {
                                let val = val.clone();
                                quote! { #rt = #fp_ts::cast::<_,_,C>(#val); }
                            })
                        }).unwrap_or_default();
                        let sh = frame.stack_height;
                        ctx.emit(quote! {
                            if #cond != 0u32 {
                                #stmts
                                #result_assign
                            }
                        });
                        ctx.stack.truncate(sh);
                        if let Some(rt) = frame.result_tmp {
                            ctx.push(quote! { #rt });
                        }
                    }
                    FrameKind::Else => {
                        // if + else
                        let cond = frame.condition.clone().unwrap_or(quote! { 0u32 });
                        let if_body_stmts = frame.if_stmts.unwrap_or_default();
                        let if_stmts_ts = quote! { #(#if_body_stmts)* };
                        let result_assign = frame.result_tmp.as_ref().and_then(|rt| {
                            ctx.stack.last().map(|val| {
                                let val = val.clone();
                                quote! { #rt = #fp_ts::cast::<_,_,C>(#val); }
                            })
                        }).unwrap_or_default();
                        let sh = frame.stack_height;
                        ctx.emit(quote! {
                            if #cond != 0u32 {
                                #if_stmts_ts
                            } else {
                                #stmts
                                #result_assign
                            }
                        });
                        ctx.stack.truncate(sh);
                        if let Some(rt) = frame.result_tmp {
                            ctx.push(quote! { #rt });
                        }
                    }
                }
            }
        }

        // ── Branches ─────────────────────────────────────────────────────────
        Operator::Br { relative_depth } => {
            let br = br_target(ctx, relative_depth as usize);
            ctx.emit(quote! { #br });
            ctx.unreachable_depth = 1;
        }
        Operator::BrIf { relative_depth } => {
            let cond = ctx.pop();
            let br = br_target(ctx, relative_depth as usize);
            ctx.emit(quote! { if #cond != 0u32 { #br } });
        }
        Operator::BrTable { targets } => {
            let idx = ctx.pop();
            // The selector is a u32 stack value; match against u32 arms.
            let mut cases = vec![];
            for (i, t) in targets.targets().enumerate() {
                let t = t?;
                let br = br_target(ctx, t as usize);
                // Patterns must be literals: emit a u32 literal (the
                // selector value is a u32 stack value).
                let lit = proc_macro2::Literal::u32_suffixed(i as u32);
                cases.push(quote! { #lit => { #br } });
            }
            let default_br = br_target(ctx, targets.default() as usize);
            ctx.emit(quote! {
                match #idx {
                    #(#cases)*
                    _ => { #default_br }
                }
            });
            ctx.unreachable_depth = 1;
        }
        Operator::Return => {
            let sig = ctx.m.func_sig(ctx.func_idx);
            let mut vals = vec![];
            for _ in 0..sig.returns.len() {
                vals.push(ctx.pop());
            }
            vals.reverse();
            ctx.emit(quote! {
                return #fp_ts::ret(Ok::<_, C::Error>(#root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#vals)),*)));
            });
            // The returned values are consumed by the return; clear the
            // stack so the implicit function-End capture has nothing to
            // re-emit.
            ctx.stack.clear();
            ctx.unreachable_depth = 1;
        }

        _ => {
            // Log unimplemented op if needed.
        }
    }
    Ok(())
}

fn blocktype_results(m: &ParsedModule, bt: wasmparser::BlockType) -> Vec<ValType> {
    match bt {
        wasmparser::BlockType::Empty => vec![],
        wasmparser::BlockType::Type(t) => vec![t],
        wasmparser::BlockType::FuncType(idx) => m.types[idx as usize].returns.clone(),
    }
}

// ─── Load / Store helpers ────────────────────────────────────────────────────

fn emit_load(
    ctx: &mut EmitCtx<'_>,
    fn_name: &str,
    memarg: wasmparser::MemArg,
    _bit_width: u32,
) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let fn_id = format_ident!("{fn_name}");
    let ptr = ctx.pop();
    let mn = format_ident!("memory{}", memarg.memory);
    let off = memarg.offset;
    ctx.push_tmp(quote! {
        match #root::#fn_id(ctx.#mn(), (#ptr as u64).wrapping_add(#off)) {
            Ok(a) => a,
            Err(e) => return #fp_ts::ret(Err(e)),
        }.0
    });
    Ok(())
}

fn emit_load_f(
    ctx: &mut EmitCtx<'_>,
    is_f64: bool,
    memarg: wasmparser::MemArg,
) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let (fn_name, ty) = if is_f64 { ("i64load", quote! { f64 }) } else { ("i32load", quote! { f32 }) };
    let fn_id = format_ident!("{fn_name}");
    let ptr = ctx.pop();
    let mn = format_ident!("memory{}", memarg.memory);
    let off = memarg.offset;
    ctx.push_tmp(quote! {
        #ty::from_bits(match #root::#fn_id(ctx.#mn(), (#ptr as u64).wrapping_add(#off)) {
            Ok(a) => a,
            Err(e) => return #fp_ts::ret(Err(e)),
        }.0)
    });
    Ok(())
}

fn emit_store(
    ctx: &mut EmitCtx<'_>,
    fn_name: &str,
    memarg: wasmparser::MemArg,
    _bit_width: u32,
) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let fn_id = format_ident!("{fn_name}");
    let val = ctx.pop();
    let ptr = ctx.pop();
    let mn = format_ident!("memory{}", memarg.memory);
    let off = memarg.offset;
    ctx.emit(quote! {
        match #root::#fn_id(ctx.#mn(), (#ptr as u64).wrapping_add(#off), #fp_ts::cast::<_,_,C>(#val)) {
            Ok(()) => {}
            Err(e) => return #fp_ts::ret(Err(e)),
        }
    });
    Ok(())
}

fn emit_store_f(
    ctx: &mut EmitCtx<'_>,
    is_f64: bool,
    memarg: wasmparser::MemArg,
) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let fn_name = if is_f64 { "i64store" } else { "i32store" };
    let ptr = ctx.pop();
    let val = ctx.pop();
    let mn = format_ident!("memory{}", memarg.memory);
    let off = memarg.offset;
    ctx.emit(quote! {
        match #root::#fn_name(ctx.#mn(), (#ptr as u64).wrapping_add(#off), (#val).to_bits()) {
            Ok(()) => {}
            Err(e) => return #fp_ts::ret(Err(e)),
        }
    });
    Ok(())
}

fn bin_op(ctx: &mut EmitCtx<'_>, fn_name: &str) {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let fn_id = format_ident!("{fn_name}");
    let b = ctx.pop();
    let a = ctx.pop();
    let tmp = ctx.fresh_tmp();
    ctx.emit(quote! {
        let (#tmp, ()) = match #root::#fn_id::<C::Error>(#fp_ts::cast::<_,_,C>(#a), #fp_ts::cast::<_,_,C>(#b)) {
            Ok(a) => a,
            Err(e) => return #fp_ts::ret(Err(e)),
        };
    });
    ctx.push(quote! { #tmp });
}

fn un_op(ctx: &mut EmitCtx<'_>, fn_name: &str) {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let fn_id = format_ident!("{fn_name}");
    let a = ctx.pop();
    let tmp = ctx.fresh_tmp();
    ctx.emit(quote! {
        let (#tmp, ()) = match #root::#fn_id::<C::Error>(#fp_ts::cast::<_,_,C>(#a)) {
            Ok(a) => a,
            Err(e) => return #fp_ts::ret(Err(e)),
        };
    });
    ctx.push(quote! { #tmp });
}

// ─── ToTokens impl ────────────────────────────────────────────────────────────

impl<'a> ToTokens for OptsLt<'a, &'a [u8], WasmparserBackend> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match go(self) {
            Ok(ts) => ts.to_tokens(tokens),
            Err(e) => syn::Error::new(Span::call_site(), format!("{e:#}"))
                .to_compile_error()
                .to_tokens(tokens),
        }
    }
}
