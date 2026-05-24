use syn::parse_quote;
use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction, Module,
    TypeSection, ValType, GlobalSection, GlobalType, MemorySection, MemoryType, MemArg, ConstExpr,
};
use wars::{Flags, OptsCore, LegacyPortalWaffleBackend, WasmparserBackend};
use quote::quote;
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    // 1. Create a simple WASM binary
    let mut module = Module::new();
    
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32, ValType::I32], [ValType::I32]); // index 0: (i32, i32) -> i32
    types.ty().function([], [ValType::I32]); // index 1: () -> i32
    types.ty().function([ValType::I32], []); // index 2: (i32) -> ()
    types.ty().function([ValType::I32], [ValType::I32]); // index 3: (i32) -> i32
    types.ty().function([ValType::I32, ValType::I32], []); // index 4: (i32, i32) -> ()
    module.section(&types);

    let mut functions = FunctionSection::new();
    functions.function(0); // 0: add
    functions.function(0); // 1: sub
    functions.function(0); // 2: calladd
    functions.function(1); // 3: getglobal
    functions.function(2); // 4: setglobal
    functions.function(3); // 5: loadi32
    functions.function(4); // 6: storei32
    module.section(&functions);

    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&memories);

    let mut globals = GlobalSection::new();
    // i32.const 42
    globals.global(GlobalType {
        val_type: ValType::I32,
        mutable: true,
        shared: false,
    }, &ConstExpr::i32_const(42));
    module.section(&globals);

    let mut exports = ExportSection::new();
    exports.export("add", ExportKind::Func, 0);
    exports.export("sub", ExportKind::Func, 1);
    exports.export("calladd", ExportKind::Func, 2);
    exports.export("getglobal", ExportKind::Func, 3);
    exports.export("setglobal", ExportKind::Func, 4);
    exports.export("loadi32", ExportKind::Func, 5);
    exports.export("storei32", ExportKind::Func, 6);
    module.section(&exports);

    let mut code = CodeSection::new();
    // add
    let mut f = Function::new([]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::End);
    code.function(&f);
    // sub
    let mut f = Function::new([]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::End);
    code.function(&f);
    // calladd
    let mut f = Function::new([]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::Call(0));
    f.instruction(&Instruction::End);
    code.function(&f);
    // getglobal
    let mut f = Function::new([]);
    f.instruction(&Instruction::GlobalGet(0));
    f.instruction(&Instruction::End);
    code.function(&f);
    // setglobal
    let mut f = Function::new([]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::GlobalSet(0));
    f.instruction(&Instruction::End);
    code.function(&f);
    // loadi32
    let mut f = Function::new([]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Load(MemArg { offset: 0, align: 2, memory_index: 0 }));
    f.instruction(&Instruction::End);
    code.function(&f);
    // storei32
    let mut f = Function::new([]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }));
    f.instruction(&Instruction::End);
    code.function(&f);
    module.section(&code);

    let wasm_bytes = module.finish();

    // 2. Generate Rust code using both backends
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    let mut data = std::collections::BTreeMap::new();
    data.insert(
        quote::format_ident!("_marker"),
        quote! { ::core::marker::PhantomData<fn() -> Target> },
    );

    let core = OptsCore {
        crate_path: syn::parse_str("::wars_rt")?,
        bytes: &wasm_bytes,
        name: quote::format_ident!("TestModule"),
        flags: Flags::default(),
        embed: quote! {},
        data,
        roots: Default::default(),
        plugins: vec![],
        chunk_size: None,
    };

    // Waffle backend (no chunking)
    let opts_waffle = core.clone().inflate::<LegacyPortalWaffleBackend>();
    let generated_waffle = parse_quote! { #opts_waffle };
    fs::write(out_dir.join("generated_waffle.rs"), prettyplease::unparse(&generated_waffle))?;

    // Wasmparser backend (no chunking)
    let opts_wp = core.clone().inflate::<WasmparserBackend>();
    let generated_wp = parse_quote! { #opts_wp };
    fs::write(out_dir.join("generated_wp.rs"), prettyplease::unparse(&generated_wp))?;

    // Wasmparser backend (chunked, chunk_size=3 — forces cross-chunk calls)
    let chunked_core = OptsCore { chunk_size: Some(3), ..core };
    let opts_wp_chunked = chunked_core.inflate::<WasmparserBackend>();
    let generated_wp_chunked = parse_quote! { #opts_wp_chunked };
    fs::write(out_dir.join("generated_wp_chunked.rs"), prettyplease::unparse(&generated_wp_chunked))?;

    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
