//! ABI v0 backend driven by `wasmparser`.
//!
//! This backend replaces the waffle pipeline for the common case where you just
//! want fast, dependency-light code generation.  It walks the binary once,
//! collects all section data, and emits ABI v0 Rust tokens in a single pass.

use super::*;
use crate::shared::{self, bindname, alloc, fp, FuncSig, FuncSigOwned};
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
    types: Vec<FuncSigOwned>,
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
    global_init_vals: Vec<Option<TokenStream>>,
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
        let mut types: Vec<FuncSigOwned> = vec![];
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
        let mut global_init_vals: Vec<Option<TokenStream>> = vec![];

        for payload in Parser::new(0).parse_all(bytes) {
            let payload = payload?;
            match payload {
                Payload::TypeSection(r) => {
                    for rec_group in r {
                        let rec_group = rec_group?;
                        for sub_ty in rec_group.types() {
                            // Only care about func types; skip everything else.
                            let sig = match &sub_ty.composite_type.inner {
                                CompositeInnerType::Func(f) => FuncSigOwned {
                                    params: f.params().to_vec(),
                                    returns: f.results().to_vec(),
                                },
                                _ => FuncSigOwned { params: vec![], returns: vec![] },
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

    /// Resolve a function index to its `FuncSigOwned`.
    fn func_sig(&self, func_idx: u32) -> &FuncSigOwned {
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
fn const_val_expr(reader: wasmparser::BinaryReader<'_>) -> Option<TokenStream> {
    let mut ops = wasmparser::OperatorsReader::new(reader);
    while !ops.eof() {
        let op = ops.read().ok()?;
        match op {
            Operator::I32Const { value } => return Some(quote! { (#value as u32) }),
            Operator::I64Const { value } => return Some(quote! { (#value as u64) }),
            Operator::F32Const { value } => {
                let bits = value.bits();
                return Some(quote! { f32::from_bits(#bits) });
            }
            Operator::F64Const { value } => {
                let bits = value.bits();
                return Some(quote! { f64::from_bits(#bits) });
            }
            Operator::End => break,
            _ => return None,
        }
    }
    None
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

pub(crate) fn go(opts: &Opts<'_>) -> anyhow::Result<TokenStream> {
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
    let defaults = field_names.iter().map(|n| quote! { #n: Default::default() });
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
                    quote! { dyn #root::Memory + 'a }
                } else {
                    quote! { impl #root::Memory + 'a }
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
                blanket_methods.push(shared::render_export(core, rust_name, free_fn, sig.as_ref()));
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
                    quote! { dyn #root::Memory + 'a }
                } else {
                    quote! { impl #root::Memory + 'a }
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
        if let Some(val_ts) = m.global_init_vals.get(g_def_idx) {
            let g_ty = shared::render_ty(core, &quote! { C }, m.global_types[g_abs_idx as usize].content_type);
            let val = val_ts.clone();
            init_stmts.push(quote! {
                *ctx.#gn() = (#val as #g_ty);
            });
        }
    }

    // Tables: element segments.
    for elem in m.elements.iter() {
        let t_n = format_ident!("table{}", elem.table_idx);
        let offset = elem.offset as usize;
        let pushes = elem.func_indices.iter().enumerate().map(|(slot, &fidx)| {
            let abs = offset + slot;
            let fun_ref = render_fun_ref(core, m, fidx);
            quote! {
                while ctx.#t_n().len() <= #abs {
                    ctx.#t_n().push(Default::default());
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
    let total_funcs = m.func_type_idx.len() as u32;
    for func_idx in 0..total_funcs {
        let ts = render_fn(core, m, func_idx)?;
        free_fns.push(ts);
    }

    // init() declaration in the FooImpl trait.
    impl_trait_methods.push(quote! {
        fn init(&mut self) -> #root::_rexport::anyhow::Result<()> where Self: 'static;
    });
    // init() implementation in the blanket impl.
    blanket_methods.push(quote! {
        fn init(&mut self) -> #root::_rexport::anyhow::Result<()> where Self: 'static {
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

    Ok(quote! {
        // ── *Data ──────────────────────────────────────────────────────────
        pub struct #data_ty<Target: #name + ?Sized> {
            #embed_field
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
                    ::core::iter::empty()
                    #(#traverse_chain)*
                )
            }
            fn traverse_mut<'a>(
                &'a mut self,
            ) -> #alloc_ts::boxed::Box<dyn Iterator<Item = &'a mut Target::ExternRef> + 'a> {
                #alloc_ts::boxed::Box::new(
                    ::core::iter::empty()
                    #(#traverse_mut_chain)*
                )
            }
        }

        // ── Host trait ────────────────────────────────────────────────────
        pub trait #name:
            #fp_ts::CtxSpec<ExternRef = Self::_ExternRef>
            #async_bounds
            #(#extra_bounds)*
        {
            type _ExternRef: Clone #(#extra_exref)*;
            #(#trait_methods)*
        }

        // ── FooImpl trait ─────────────────────────────────────────────────
        pub trait #impl_trait: #name {
            #(#impl_trait_methods)*
        }

        // ── Blanket impl + free functions ─────────────────────────────────
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

// ─── Function reference helper ────────────────────────────────────────────────

fn render_fun_ref(core: &OptsCore<'_>, m: &ParsedModule, func_idx: u32) -> TokenStream {
    let root = core.crate_path.clone();
    let fp_ts = fp(core);
    let sig = m.func_sig(func_idx);
    let ctx_ts = quote! { c };
    let generics = shared::render_generics(core, &ctx_ts, sig.as_ref());
    let fname = m.fname(func_idx);
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

fn render_fn(core: &OptsCore<'_>, m: &ParsedModule, func_idx: u32) -> anyhow::Result<TokenStream> {
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
    let body_ts = emit_body(core, m, func_idx, &local_types, op_bytes)?;

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
    /// Output buffer stack: `out_stack.last_mut()` is where we currently write.
    /// Pushed on Block/Loop/If entry, popped and merged on End/Else.
    out_stack: Vec<Vec<TokenStream>>,
}

struct Frame {
    kind: FrameKind,
    /// Rust lifetime label index (used for 'lN).
    label: usize,
    /// Result types of the block.
    result_tys: Vec<ValType>,
    /// Temp ident used to carry block results out (for block/if).
    result_tmp: Option<Ident>,
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
            out_stack: vec![vec![]],
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
) -> anyhow::Result<TokenStream> {
    let mut ctx = EmitCtx::new(core, m, func_idx, local_types);
    let sig = m.func_sig(func_idx);

    // Outer frame: the function body itself.
    let fn_label = ctx.fresh_label();
    ctx.frames.push(Frame {
        kind: FrameKind::Block,
        label: fn_label,
        result_tys: sig.returns.to_vec(),
        result_tmp: None, // functions return via `return`, not block-result
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
    let idx = ctx.frames.len().saturating_sub(depth + 1);
    if idx >= ctx.frames.len() {
        return quote! { return; };
    }
    let frame = &ctx.frames[idx];
    match frame.kind {
        FrameKind::Loop => {
            let lt = Lifetime::new(&format!("'l{}", frame.label), Span::call_site());
            quote! { continue #lt; }
        }
        _ => {
            let lt = Lifetime::new(&format!("'l{}", frame.label), Span::call_site());
            quote! { break #lt; }
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
                // Depth reaches 0: pop corresponding frame and its buffer.
                ctx.frames.pop();
                ctx.pop_buf(); // discard the unreachable body
                return Ok(());
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
            ctx.push_tmp(quote! { *ctx.#gn() });
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
                ((match #root::Memory::size(ctx.#mn()) {
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
                let _old = match #root::Memory::size(ctx.#mn()) {
                    Ok(a) => a,
                    Err(e) => return #fp_ts::ret(Err(e)),
                };
                match #root::Memory::grow(ctx.#mn(), (#delta as u64) * #page_size) {
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
                    let _mc_buf = match #root::Memory::read(ctx.#smn(), #src_ptr as u64, #len as u64) {
                        Ok(a) => a.as_ref().as_ref().to_owned(),
                        Err(e) => return #fp_ts::ret(Err(e)),
                    };
                    match #root::Memory::write(ctx.#dmn(), #dst_ptr as u64, &_mc_buf) {
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
                    match #root::Memory::write(ctx.#mn(), #dst as u64, &_mf_buf) {
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
            ctx.push_tmp(quote! { (#a.rotate_right((#b & 0xffffffff) as u32)) });
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

        // ── Numeric: f32 ─────────────────────────────────────────────────────
        Operator::F32Add => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a + #b) }); }
        Operator::F32Sub => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a - #b) }); }
        Operator::F32Mul => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a * #b) }); }
        Operator::F32Div => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a / #b) }); }
        Operator::F32Min => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a.min(#b)) }); }
        Operator::F32Max => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a.max(#b)) }); }
        Operator::F32Abs => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.abs()) }); }
        Operator::F32Neg => { let a = ctx.pop(); ctx.push_tmp(quote! { (-#a) }); }
        Operator::F32Ceil => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.ceil()) }); }
        Operator::F32Floor => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.floor()) }); }
        Operator::F32Trunc => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.trunc()) }); }
        Operator::F32Nearest => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.round()) }); }
        Operator::F32Sqrt => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.sqrt()) }); }
        Operator::F32Copysign => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a.copysign(#b)) }); }
        Operator::F32Eq => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a == #b { 1u32 } else { 0u32 }) }); }
        Operator::F32Ne => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a != #b { 1u32 } else { 0u32 }) }); }
        Operator::F32Lt => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a < #b { 1u32 } else { 0u32 }) }); }
        Operator::F32Gt => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a > #b { 1u32 } else { 0u32 }) }); }
        Operator::F32Le => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a <= #b { 1u32 } else { 0u32 }) }); }
        Operator::F32Ge => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a >= #b { 1u32 } else { 0u32 }) }); }

        // ── Numeric: f64 ─────────────────────────────────────────────────────
        Operator::F64Add => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a + #b) }); }
        Operator::F64Sub => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a - #b) }); }
        Operator::F64Mul => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a * #b) }); }
        Operator::F64Div => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a / #b) }); }
        Operator::F64Min => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a.min(#b)) }); }
        Operator::F64Max => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a.max(#b)) }); }
        Operator::F64Abs => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.abs()) }); }
        Operator::F64Neg => { let a = ctx.pop(); ctx.push_tmp(quote! { (-#a) }); }
        Operator::F64Ceil => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.ceil()) }); }
        Operator::F64Floor => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.floor()) }); }
        Operator::F64Trunc => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.trunc()) }); }
        Operator::F64Nearest => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.round()) }); }
        Operator::F64Sqrt => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.sqrt()) }); }
        Operator::F64Copysign => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (#a.copysign(#b)) }); }
        Operator::F64Eq => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a == #b { 1u32 } else { 0u32 }) }); }
        Operator::F64Ne => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a != #b { 1u32 } else { 0u32 }) }); }
        Operator::F64Lt => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a < #b { 1u32 } else { 0u32 }) }); }
        Operator::F64Gt => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a > #b { 1u32 } else { 0u32 }) }); }
        Operator::F64Le => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a <= #b { 1u32 } else { 0u32 }) }); }
        Operator::F64Ge => { let b = ctx.pop(); let a = ctx.pop(); ctx.push_tmp(quote! { (if #a >= #b { 1u32 } else { 0u32 }) }); }

        // ── Conversions ───────────────────────────────────────────────────────
        Operator::I32WrapI64 => { let a = ctx.pop(); ctx.push_tmp(quote! { ((#a & 0xffffffff_u64) as u32) }); }
        Operator::I64ExtendI32U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as u64) }); }
        Operator::I64ExtendI32S => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i32 as i64 as u64) }); }
        Operator::I32TruncF32S => { let a = ctx.pop(); ctx.push_tmp(quote! { (unsafe { #a.trunc().to_int_unchecked::<i32>() } as u32) }); }
        Operator::I32TruncF32U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as u32) }); }
        Operator::I32TruncF64S => { let a = ctx.pop(); ctx.push_tmp(quote! { (unsafe { #a.trunc().to_int_unchecked::<i32>() } as u32) }); }
        Operator::I32TruncF64U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as u32) }); }
        Operator::I64TruncF32S => { let a = ctx.pop(); ctx.push_tmp(quote! { (unsafe { #a.trunc().to_int_unchecked::<i64>() } as u64) }); }
        Operator::I64TruncF32U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as u64) }); }
        Operator::I64TruncF64S => { let a = ctx.pop(); ctx.push_tmp(quote! { (unsafe { #a.trunc().to_int_unchecked::<i64>() } as u64) }); }
        Operator::I64TruncF64U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as u64) }); }
        Operator::F32ConvertI32S => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i32 as f32) }); }
        Operator::F32ConvertI32U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as f32) }); }
        Operator::F32ConvertI64S => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i64 as f32) }); }
        Operator::F32ConvertI64U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as f32) }); }
        Operator::F64ConvertI32S => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i32 as f64) }); }
        Operator::F64ConvertI32U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as f64) }); }
        Operator::F64ConvertI64S => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i64 as f64) }); }
        Operator::F64ConvertI64U => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as f64) }); }
        Operator::F32DemoteF64 => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as f32) }); }
        Operator::F64PromoteF32 => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as f64) }); }
        Operator::I32ReinterpretF32 => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.to_bits()) }); }
        Operator::I64ReinterpretF64 => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a.to_bits()) }); }
        Operator::F32ReinterpretI32 => { let a = ctx.pop(); ctx.push_tmp(quote! { (f32::from_bits(#a)) }); }
        Operator::F64ReinterpretI64 => { let a = ctx.pop(); ctx.push_tmp(quote! { (f64::from_bits(#a)) }); }
        Operator::I32Extend8S  => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i8 as i32 as u32) }); }
        Operator::I32Extend16S => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i16 as i32 as u32) }); }
        Operator::I64Extend8S  => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i8 as i64 as u64) }); }
        Operator::I64Extend16S => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i16 as i64 as u64) }); }
        Operator::I64Extend32S => { let a = ctx.pop(); ctx.push_tmp(quote! { (#a as i32 as i64 as u64) }); }

        // ── Reference types ───────────────────────────────────────────────────
        Operator::RefNull { .. } => {
            ctx.push_tmp(quote! { Default::default() });
        }
        Operator::RefIsNull => {
            let a = ctx.pop();
            // Value<C> is null when it is the null variant; use Option pattern via cast.
            ctx.push_tmp(quote! {{
                let _v: Option<#fp_ts::Value<C>> = #fp_ts::cast::<_,_,C>(#a);
                if _v.is_none() { 1u32 } else { 0u32 }
            }});
        }
        Operator::RefFunc { function_index } => {
            let fun_ref = render_fun_ref(ctx.core, ctx.m, function_index);
            ctx.push_tmp(fun_ref);
        }

        // ── Tables ────────────────────────────────────────────────────────────
        Operator::TableGet { table } => {
            let tn = format_ident!("table{table}");
            let idx = ctx.pop();
            ctx.push_tmp(quote! { ctx.#tn()[#idx as usize].clone() });
        }
        Operator::TableSet { table } => {
            let val = ctx.pop();
            let idx = ctx.pop();
            let tn = format_ident!("table{table}");
            ctx.emit(quote! { ctx.#tn()[#idx as usize] = #fp_ts::cast::<_,_,C>(#val); });
        }
        Operator::TableSize { table } => {
            let tn = format_ident!("table{table}");
            ctx.push_tmp(quote! { (ctx.#tn().len() as u32) });
        }
        Operator::TableGrow { table } => {
            let tn = format_ident!("table{table}");
            let n   = ctx.pop();
            let val = ctx.pop();
            let old_len_tmp = ctx.fresh_tmp();
            ctx.emit(quote! {
                let #old_len_tmp = ctx.#tn().len() as u32;
                for _ in 0u32..#n {
                    ctx.#tn().push(#fp_ts::cast::<_,_,C>(#val.clone()));
                }
            });
            ctx.push(quote! { #old_len_tmp });
        }
        Operator::TableFill { table } => {
            let tn = format_ident!("table{table}");
            let n = ctx.pop();
            let val = ctx.pop();
            let off = ctx.pop();
            ctx.emit(quote! {
                for _tf_i in 0u32..#n {
                    ctx.#tn()[(#off + _tf_i) as usize] = #fp_ts::cast::<_,_,C>(#val.clone());
                }
            });
        }
        Operator::TableCopy { dst_table, src_table } => {
            let dtn = format_ident!("table{dst_table}");
            let stn = format_ident!("table{src_table}");
            let n = ctx.pop();
            let src = ctx.pop();
            let dst = ctx.pop();
            ctx.emit(quote! {
                for _tc_i in 0u32..#n {
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
                ctx.emit(quote! { let mut #t: #ty = Default::default(); });
                Some(t)
            };
            let sh = ctx.stack.len();
            ctx.push_buf();
            ctx.frames.push(Frame {
                kind: FrameKind::Block,
                label,
                result_tys,
                result_tmp,
                stack_height: sh,
                condition: None,
                if_stmts: None,
            });
        }
        Operator::Loop { blockty } => {
            let label = ctx.fresh_label();
            let result_tys = blocktype_results(ctx.m, blockty);
            let sh = ctx.stack.len();
            ctx.push_buf();
            ctx.frames.push(Frame {
                kind: FrameKind::Loop,
                label,
                result_tys,
                result_tmp: None,
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
                ctx.emit(quote! { let mut #t: #ty = Default::default(); });
                Some(t)
            };
            let sh = ctx.stack.len();
            ctx.push_buf();
            ctx.frames.push(Frame {
                kind: FrameKind::If,
                label,
                result_tys,
                result_tmp,
                stack_height: sh,
                condition: Some(cond),
                if_stmts: None,
            });
        }
        Operator::Else => {
            // Snapshot the if-branch buffer, start a fresh else buffer.
            let if_body = ctx.pop_buf();
            {
                let frame = ctx.frames.last_mut().expect("else without frame");
                frame.if_stmts = Some(if_body);
                frame.kind = FrameKind::Else;
            }
            ctx.push_buf();
        }
        Operator::End => {
            if let Some(frame) = ctx.frames.pop() {
                let body = ctx.pop_buf();
                let stmts = quote! { #(#body)* };
                match frame.kind {
                    FrameKind::Block => {
                        let lt = Lifetime::new(&format!("'l{}", frame.label), Span::call_site());
                        // Assign result if any.
                        let result_assign = frame.result_tmp.as_ref().and_then(|rt| {
                            ctx.stack.last().map(|val| {
                                let val = val.clone();
                                quote! { #rt = #fp_ts::cast::<_,_,C>(#val); }
                            })
                        }).unwrap_or_default();
                        ctx.emit(quote! {
                            #lt: {
                                #stmts
                                #result_assign
                            }
                        });
                        if let Some(rt) = frame.result_tmp {
                            ctx.push(quote! { #rt });
                        }
                    }
                    FrameKind::Loop => {
                        let lt = Lifetime::new(&format!("'l{}", frame.label), Span::call_site());
                        ctx.emit(quote! {
                            #lt: loop {
                                #stmts
                                break;
                            }
                        });
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
                        ctx.emit(quote! {
                            if #cond != 0u32 {
                                #stmts
                                #result_assign
                            }
                        });
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
                        ctx.emit(quote! {
                            if #cond != 0u32 {
                                #if_stmts_ts
                            } else {
                                #stmts
                                #result_assign
                            }
                        });
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
            let val = ctx.pop();
            let def = br_target(ctx, targets.default() as usize);
            let arms: Vec<TokenStream> = targets
                .targets()
                .enumerate()
                .map(|(i, t)| {
                    let t = t.unwrap();
                    let br = br_target(ctx, t as usize);
                    quote! { #i => { #br } }
                })
                .collect();
            ctx.emit(quote! {
                match #val as usize {
                    #(#arms,)*
                    _ => { #def }
                }
            });
            ctx.unreachable_depth = 1;
        }
        Operator::Return => {
            emit_return(ctx);
            ctx.unreachable_depth = 1;
        }

        // ── Calls ─────────────────────────────────────────────────────────────
        Operator::Call { function_index } => {
            let sig = ctx.m.func_sig(function_index).clone();
            let n_params = sig.params.len();
            let mut args: Vec<TokenStream> = (0..n_params).map(|_| ctx.pop()).collect();
            args.reverse();
            let call_ts = emit_call(ctx, function_index, &args)?;
            if sig.returns.is_empty() {
                ctx.emit(call_ts);
            } else {
                let results = unwrap_call_result(ctx, call_ts, &sig.returns);
                for r in results {
                    ctx.push(r);
                }
            }
        }
        Operator::CallIndirect { type_index, table_index } => {
            let sig = ctx.m.types[type_index as usize].clone();
            let n_params = sig.params.len();
            let idx = ctx.pop(); // table index is top of stack
            let mut args: Vec<TokenStream> = (0..n_params).map(|_| ctx.pop()).collect();
            args.reverse();
            let tn = format_ident!("table{table_index}");
            let generics = shared::render_generics(ctx.core, &quote! { c }, sig.as_ref());
            let call_ts = if ctx.core.flags.contains(Flags::ASYNC) {
                quote! {
                    #fp_ts::call_ref::<#generics, C>(
                        ctx,
                        #fp_ts::cast(ctx.#tn()[#idx as usize].clone()),
                        #root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args.clone())),*)
                    ).go().await?
                }
            } else {
                quote! {
                    match #root::_rexport::tramp::tramp(
                        #fp_ts::call_ref::<#generics, C>(
                            ctx,
                            #fp_ts::cast(ctx.#tn()[#idx as usize].clone()),
                            #root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args.clone())),*)
                        )
                    ) {
                        Ok(a) => a,
                        Err(e) => return #fp_ts::ret(Err(e)),
                    }
                }
            };
            if sig.returns.is_empty() {
                ctx.emit(call_ts);
            } else {
                let results = unwrap_call_result(ctx, call_ts, &sig.returns);
                for r in results {
                    ctx.push(r);
                }
            }
        }

        // ── Return-calls ──────────────────────────────────────────────────────
        Operator::ReturnCall { function_index } => {
            let sig = ctx.m.func_sig(function_index).clone();
            let mut args: Vec<TokenStream> = (0..sig.params.len()).map(|_| ctx.pop()).collect();
            args.reverse();
            let call_ts = emit_return_call(ctx, function_index, &args)?;
            ctx.emit(call_ts);
            ctx.unreachable_depth = 1;
        }
        Operator::ReturnCallIndirect { type_index, table_index } => {
            let sig = ctx.m.types[type_index as usize].clone();
            let idx = ctx.pop();
            let mut args: Vec<TokenStream> = (0..sig.params.len()).map(|_| ctx.pop()).collect();
            args.reverse();
            let tn = format_ident!("table{table_index}");
            let generics = shared::render_generics(ctx.core, &quote! { c }, sig.as_ref());
            let call_ts = if ctx.core.flags.contains(Flags::ASYNC) {
                quote! {
                    return #fp_ts::call_ref::<#generics, C>(
                        ctx,
                        #fp_ts::cast(ctx.#tn()[#idx as usize].clone()),
                        #root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args.clone())),*)
                    );
                }
            } else {
                quote! {
                    return #root::_rexport::tramp::BorrowRec::Call(
                        #root::_rexport::tramp::Thunk::new(move || {
                            #fp_ts::call_ref::<#generics, C>(
                                ctx,
                                #fp_ts::cast(ctx.#tn()[#idx as usize].clone()),
                                #root::_rexport::tuple_list::tuple_list!(#(#fp_ts::cast::<_,_,C>(#args.clone())),*)
                            )
                        })
                    );
                }
            };
            ctx.emit(call_ts);
            ctx.unreachable_depth = 1;
        }

        // ── SIMD (stub) ───────────────────────────────────────────────────────
        // V128 operators: emit todo!() with a clear message.
        _ => {
            let msg = format!("wasmparser backend: unsupported operator in func {}", ctx.func_idx);
            ctx.emit(quote! { todo!(#msg); });
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn blocktype_results(m: &ParsedModule, blockty: wasmparser::BlockType) -> Vec<ValType> {
    match blockty {
        wasmparser::BlockType::Empty => vec![],
        wasmparser::BlockType::Type(t) => vec![t],
        wasmparser::BlockType::FuncType(idx) => m.types[idx as usize].returns.clone(),
    }
}

fn emit_return(ctx: &mut EmitCtx<'_>) {
    let fp_ts = ctx.fp();
    let sig = ctx.m.func_sig(ctx.func_idx);
    let n_rets = sig.returns.len();
    let mut vals: Vec<TokenStream> = (0..n_rets).map(|_| ctx.pop()).collect();
    vals.reverse();
    let root = ctx.root().clone();
    ctx.emit(quote! {
        return #fp_ts::ret(Ok(#root::_rexport::tuple_list::tuple_list!(
            #(#fp_ts::cast::<_,_,C>(#vals)),*
        )));
    });
}

fn emit_call(
    ctx: &mut EmitCtx<'_>,
    func_idx: u32,
    args: &[TokenStream],
) -> anyhow::Result<TokenStream> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let sig = ctx.m.func_sig(func_idx).clone();

    if ctx.m.is_defined(func_idx) {
        let fname = ctx.m.fname(func_idx);
        if ctx.core.flags.contains(Flags::ASYNC) {
            Ok(quote! {
                match #fname(ctx, #root::_rexport::tuple_list::tuple_list!(
                    #(#fp_ts::cast::<_,_,C>(#args.clone())),*
                )).go().await {
                    Ok(a) => a,
                    Err(e) => return #fp_ts::ret(Err(e)),
                }
            })
        } else {
            Ok(quote! {
                match #root::_rexport::tramp::tramp(
                    #fname(ctx, #root::_rexport::tuple_list::tuple_list!(
                        #(#fp_ts::cast::<_,_,C>(#args.clone())),*
                    ))
                ) {
                    Ok(a) => a,
                    Err(e) => return #fp_ts::ret(Err(e)),
                }
            })
        }
    } else {
        let imp = ctx.m.import_for_func(func_idx).unwrap();
        let mname = format_ident!("{}_{}", bindname(&imp.module), bindname(&imp.name));
        // Check plugin.
        let plugin_result: Option<TokenStream> = ctx.core.plugins.iter()
            .find_map(|p| {
                p.import(ctx.core, &imp.module, &imp.name,
                    args.iter().cloned().collect())
                    .ok()
                    .flatten()
            });
        let call = if let Some(ts) = plugin_result {
            ts
        } else {
            quote! {
                ctx.#mname(#root::_rexport::tuple_list::tuple_list!(
                    #(#fp_ts::cast::<_,_,C>(#args.clone())),*
                ))
            }
        };
        if ctx.core.flags.contains(Flags::ASYNC) {
            Ok(quote! {
                match #root::_rexport::alloc::boxed::Box::pin(#call.go()).await {
                    Ok(a) => a,
                    Err(e) => return #fp_ts::ret(Err(e)),
                }
            })
        } else {
            Ok(quote! {
                match #root::_rexport::tramp::tramp(#call) {
                    Ok(a) => a,
                    Err(e) => return #fp_ts::ret(Err(e)),
                }
            })
        }
    }
}

fn emit_return_call(
    ctx: &mut EmitCtx<'_>,
    func_idx: u32,
    args: &[TokenStream],
) -> anyhow::Result<TokenStream> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();

    if ctx.m.is_defined(func_idx) {
        let fname = ctx.m.fname(func_idx);
        if ctx.core.flags.contains(Flags::ASYNC) {
            Ok(quote! {
                return #fname(ctx, #root::_rexport::tuple_list::tuple_list!(
                    #(#fp_ts::cast::<_,_,C>(#args.clone())),*
                ));
            })
        } else {
            Ok(quote! {
                return #root::_rexport::tramp::BorrowRec::Call(
                    #root::_rexport::tramp::Thunk::new(move || {
                        #fname(ctx, #root::_rexport::tuple_list::tuple_list!(
                            #(#fp_ts::cast::<_,_,C>(#args.clone())),*
                        ))
                    })
                );
            })
        }
    } else {
        let imp = ctx.m.import_for_func(func_idx).unwrap();
        let mname = format_ident!("{}_{}", bindname(&imp.module), bindname(&imp.name));
        let call = quote! {
            ctx.#mname(#root::_rexport::tuple_list::tuple_list!(
                #(#fp_ts::cast::<_,_,C>(#args.clone())),*
            ))
        };
        if ctx.core.flags.contains(Flags::ASYNC) {
            Ok(quote! { return #call; })
        } else {
            Ok(quote! {
                return #root::_rexport::tramp::BorrowRec::Call(
                    #root::_rexport::tramp::Thunk::new(move || { #call })
                );
            })
        }
    }
}

/// Destructure a multi-value call result tuple into individual stack entries.
fn unwrap_call_result(
    ctx: &mut EmitCtx<'_>,
    call_ts: TokenStream,
    returns: &[ValType],
) -> Vec<TokenStream> {
    if returns.is_empty() {
        return vec![];
    }
    if returns.len() == 1 {
        let tmp = ctx.fresh_tmp();
        let fp_ts = ctx.fp();
        let root = ctx.root().clone();
        ctx.emit(quote! {
            let (#tmp, ()) = #call_ts;
        });
        return vec![quote! { #tmp }];
    }
    // Multi-value: destructure tuple list.
    let tmps: Vec<Ident> = (0..returns.len()).map(|_| ctx.fresh_tmp()).collect();
    let fp_ts = ctx.fp();
    let root = ctx.root().clone();
    // Build the nested tuple pattern.
    let pat = build_tuple_pat(&tmps);
    ctx.emit(quote! { let #pat = #call_ts; });
    tmps.iter().map(|t| quote! { #t }).collect()
}

fn build_tuple_pat(ids: &[Ident]) -> TokenStream {
    if ids.is_empty() {
        return quote! { () };
    }
    let first = &ids[0];
    let rest = build_tuple_pat(&ids[1..]);
    quote! { (#first, #rest) }
}

// ── Load/Store helpers ────────────────────────────────────────────────────────

fn emit_load(
    ctx: &mut EmitCtx<'_>,
    fn_name: &str,
    memarg: wasmparser::MemArg,
    _align: u32,
) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let mn = format_ident!("memory{}", memarg.memory);
    let fn_id = format_ident!("{fn_name}");
    let off = memarg.offset;
    let ptr = ctx.pop();
    let tmp = ctx.fresh_tmp();
    ctx.emit(quote! {
        let (#tmp, ()) = match #root::#fn_id(ctx.#mn(), (#ptr as u64).wrapping_add(#off)) {
            Ok(a) => a,
            Err(e) => return #fp_ts::ret(Err(e)),
        };
    });
    ctx.push(quote! { #tmp });
    Ok(())
}

fn emit_load_f(ctx: &mut EmitCtx<'_>, is_f64: bool, memarg: wasmparser::MemArg) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let mn = format_ident!("memory{}", memarg.memory);
    let off = memarg.offset;
    let ptr = ctx.pop();
    let tmp = ctx.fresh_tmp();
    if is_f64 {
        ctx.emit(quote! {
            let (#tmp, ()) = match #root::i64load(ctx.#mn(), (#ptr as u64).wrapping_add(#off)) {
                Ok(a) => a,
                Err(e) => return #fp_ts::ret(Err(e)),
            };
        });
        ctx.emit(quote! { let #tmp = f64::from_bits(#tmp); });
    } else {
        ctx.emit(quote! {
            let (#tmp, ()) = match #root::i32load(ctx.#mn(), (#ptr as u64).wrapping_add(#off)) {
                Ok(a) => a,
                Err(e) => return #fp_ts::ret(Err(e)),
            };
        });
        ctx.emit(quote! { let #tmp = f32::from_bits(#tmp); });
    }
    ctx.push(quote! { #tmp });
    Ok(())
}

fn emit_store(
    ctx: &mut EmitCtx<'_>,
    fn_name: &str,
    memarg: wasmparser::MemArg,
    _align: u32,
) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let mn = format_ident!("memory{}", memarg.memory);
    let fn_id = format_ident!("{fn_name}");
    let off = memarg.offset;
    let val = ctx.pop();
    let ptr = ctx.pop();
    ctx.emit(quote! {
        match #root::#fn_id(ctx.#mn(), (#ptr as u64).wrapping_add(#off), #fp_ts::cast::<_,_,C>(#val)) {
            Ok(()) => {}
            Err(e) => return #fp_ts::ret(Err(e)),
        }
    });
    Ok(())
}

fn emit_store_f(ctx: &mut EmitCtx<'_>, is_f64: bool, memarg: wasmparser::MemArg) -> anyhow::Result<()> {
    let root = ctx.root().clone();
    let fp_ts = ctx.fp();
    let mn = format_ident!("memory{}", memarg.memory);
    let off = memarg.offset;
    let val = ctx.pop();
    let ptr = ctx.pop();
    if is_f64 {
        ctx.emit(quote! {
            match #root::i64store(ctx.#mn(), (#ptr as u64).wrapping_add(#off), (#val).to_bits()) {
                Ok(()) => {}
                Err(e) => return #fp_ts::ret(Err(e)),
            }
        });
    } else {
        ctx.emit(quote! {
            match #root::i32store(ctx.#mn(), (#ptr as u64).wrapping_add(#off), (#val).to_bits()) {
                Ok(()) => {}
                Err(e) => return #fp_ts::ret(Err(e)),
            }
        });
    }
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
        let (#tmp, ()) = match #root::#fn_id(#fp_ts::cast::<_,_,C>(#a), #fp_ts::cast::<_,_,C>(#b)) {
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
        let (#tmp, ()) = match #root::#fn_id(#fp_ts::cast::<_,_,C>(#a)) {
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
