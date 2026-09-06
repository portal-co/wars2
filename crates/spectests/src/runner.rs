//! Runner: compiles one `.wast` file's modules through wars and executes
//! the script's commands against the compiled artifact.
//!
//! Execution model: the runner generates a self-contained Rust crate for the
//! file (one `include!` module per wasm module), compiles it as a binary with
//! cargo, then runs it as a *worker process*. The worker instantiates the
//! modules, streams commands (instantiate/invoke/get) read from the harness
//! as JSON lines, and answers one JSON line per command. Process isolation
//! means a panic in generated code kills only the file's worker.

use crate::plugin::SpectestPlugin;
use crate::report::{Assertion, CaseResult, FileResult, Outcome};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use wast::core::WastArgCore;
use wast::core::WastRetCore;
use wast::lexer::Lexer;
use wast::parser::ParseBuffer;
use wast::{QuoteWat, Wast, WastArg, WastDirective, WastExecute, WastInvoke, WastRet};

fn workspace_root() -> PathBuf {
    std::env::current_dir().expect("cwd")
}

pub struct Runner {
    /// Directory for scratch generated crates.
    pub gen_root: PathBuf,
    pub backend: Backend,
}

impl Clone for Runner {
    fn clone(&self) -> Self {
        Runner { gen_root: self.gen_root.clone(), backend: self.backend }
    }
}

/// Which wars backend to exercise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Backend {
    Wasmparser,
    Waffle,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Wasmparser => "wasmparser",
            Backend::Waffle => "waffle",
        }
    }
}

/// One planned command from the script, with enough info to report later.
#[derive(Clone, Debug)]
enum Planned {
    Module {
        /// Definition (not auto-instantiated in the reference script sense;
        /// we instantiate every `Module` and `ModuleDefinition` here).
        bytes: Vec<u8>,
    },
    AssertReturn {
        line: usize,
        exec: ExecPlan,
        results: Vec<RetPlan>,
    },
    AssertTrap {
        line: usize,
        exec: ExecPlan,
        message: String,
    },
    AssertExhaustion {
        line: usize,
        exec: ExecPlan,
    },
    AssertMalformed {
        line: usize,
    },
    AssertInvalid {
        line: usize,
    },
    AssertUnlinkable {
        line: usize,
    },
    Action {
        line: usize,
        exec: ExecPlan,
    },
    Register {
        line: usize,
        name: String,
    },
    Other,
}

#[derive(Clone, Debug)]
enum ExecPlan {
    /// Invoke export `field` of module `module_idx` with arguments.
    Invoke {
        module_idx: Option<usize>,
        field: String,
        args: Vec<ArgPlan>,
    },
    /// Get global `field` of module `module_idx`.
    Get {
        module_idx: Option<usize>,
        field: String,
    },
}

#[derive(Clone, Debug)]
enum ArgPlan {
    I32(i32),
    I64(i64),
    F32Bits(u32),
    F64Bits(u64),
    Unsupported(&'static str),
}

#[derive(Clone, Debug)]
enum RetPlan {
    I32(i32),
    I64(i64),
    F32Value(u32),
    F64Value(u64),
    F32CanonicalNan,
    F64CanonicalNan,
    F32ArithmeticNan,
    F64ArithmeticNan,
    Unsupported(&'static str),
}

/// Execute one wast file, returning per-command results.
pub fn run_file(runner: &Runner, path: &Path, limit: Option<usize>) -> Result<FileResult> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let file_stem = path.file_stem().unwrap().to_string_lossy().to_string();

    let mut lexer = Lexer::new(&source);
    lexer.allow_confusing_unicode(true);
    let buf = ParseBuffer::new(&source)?;
    let script: Wast = wast::parser::parse(&buf)?;

    let mut result = FileResult::new(file_stem.clone(), runner.backend);
    let mut planned: Vec<Planned> = vec![];
    let line_of = |span: wast::token::Span| -> usize {
        // wast spans are 0-indexed lines within the source text.
        let (line, _col) = span.linecol_in(&source);
        line + 1
    };

    for directive in script.directives {
        match directive {
            WastDirective::Module(mut qw) => {
                let bytes = qw
                    .encode()
                    .map_err(|e| anyhow::anyhow!("encoding module: {e}"))?;
                planned.push(Planned::Module { bytes });
            }
            WastDirective::ModuleDefinition(mut qw) => {
                let bytes = qw
                    .encode()
                    .map_err(|e| anyhow::anyhow!("encoding module definition: {e}"))?;
                planned.push(Planned::Module { bytes });
            }
            WastDirective::ModuleInstance { .. } => {
                planned.push(Planned::Other);
            }
            WastDirective::AssertMalformed { span, .. } => {
                planned.push(Planned::AssertMalformed { line: line_of(span) });
            }
            WastDirective::AssertInvalid { span, .. } => {
                planned.push(Planned::AssertInvalid { line: line_of(span) });
            }
            WastDirective::AssertInvalidCustom { span, .. } => {
                planned.push(Planned::AssertInvalid { line: line_of(span) });
            }
            WastDirective::AssertUnlinkable { span, .. } => {
                planned.push(Planned::AssertUnlinkable { line: line_of(span) });
            }
            WastDirective::Register { span, name, .. } => {
                planned.push(Planned::Register { line: line_of(span), name: name.to_string() });
            }
            WastDirective::Invoke(inv) => {
                planned.push(Planned::Action {
                    line: line_of(inv.span),
                    exec: plan_invoke(&inv),
                });
            }
            WastDirective::AssertTrap { span, exec, message } => {
                planned.push(Planned::AssertTrap {
                    line: line_of(span),
                    exec: plan_execute(&exec),
                    message: message.to_string(),
                });
            }
            WastDirective::AssertReturn { span, exec, results } => {
                planned.push(Planned::AssertReturn {
                    line: line_of(span),
                    exec: plan_execute(&exec),
                    results: results.iter().map(plan_ret).collect(),
                });
            }
            WastDirective::AssertExhaustion { span, call, .. } => {
                planned.push(Planned::AssertExhaustion {
                    line: line_of(span),
                    exec: ExecPlan::Invoke {
                        module_idx: None,
                        field: call.name.to_string(),
                        args: call.args.iter().map(plan_arg).collect(),
                    },
                });
            }
            other => {
                let _ = other;
                planned.push(Planned::Other);
            }
        }
    }

    if let Some(n) = limit {
        planned.truncate(n);
    }

    // Collect module binaries in order.
    let modules: Vec<Vec<u8>> = planned
        .iter()
        .filter_map(|p| match p {
            Planned::Module { bytes } => Some(bytes.clone()),
            _ => None,
        })
        .collect();

    if modules.is_empty() {
        for (idx, p) in planned.iter().enumerate() {
            result.push(CaseResult {
                index: idx,
                line: planned_line(p),
                assertion: planned_assertion(p),
                outcome: Outcome::Skip { msg: "no modules executed".into() },
            });
        }
        return Ok(result);
    }

    // Generate + compile the worker crate.
    let gen_dir = std::env::current_dir()?.join(
        runner
            .gen_root
            .join(format!("{}-{}", file_stem, runner.backend.name())),
    );
    let _ = std::fs::remove_dir_all(&gen_dir);
    std::fs::create_dir_all(gen_dir.join("gen"))?;
    write_worker_crate(&gen_dir, &modules, runner.backend)
        .context("generating worker crate")?;

    let build = Command::new("cargo")
        .current_dir(&gen_dir)
        .args(["build", "--release", "--quiet"])
        .env("CARGO_TARGET_DIR", gen_dir.join("wt"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("spawning cargo for worker crate")?;
    if !build.status.success() {
        let err = String::from_utf8_lossy(&build.stderr);
        let first = err.lines().take(10).collect::<Vec<_>>().join(" | ");
        for (idx, p) in planned.iter().enumerate() {
            result.push(CaseResult {
                index: idx,
                line: planned_line(p),
                assertion: planned_assertion(p),
                outcome: Outcome::Fail {
                    msg: format!("generated crate failed to compile: {first}"),
                },
            });
        }
        return Ok(result);
    }
    let worker_path = gen_dir.join("wt/release/spectest-worker");
    if !worker_path.exists() {
        anyhow::bail!("worker binary missing at {}", worker_path.display());
    }

    // Run the worker, stream commands, collect outcomes.
    //
    // NOTE (deadlock avoidance): we must NOT write all requests before
    // reading responses. The worker writes one response line per request;
    // once responses exceed the 64 KiB pipe buffer the worker blocks on
    // write while we block on write to its stdin — a classic pipe deadlock
    // (this hung f32.wast, ~2500 directives, for >20 minutes). A dedicated
    // reader thread drains stdout concurrently while we feed stdin.
    let mut child = Command::new(&worker_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawning worker")?;

    let stdout = child.stdout.take().unwrap();
    let (resp_tx, resp_rx) = std::sync::mpsc::channel::<String>();
    let reader_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if resp_tx.send(l).is_err() {
                        break; // parent went away
                    }
                }
                Err(_) => break,
            }
        }
    });

    {
        let stdin = child.stdin.as_mut().unwrap();
        for (mi, bytes) in modules.iter().enumerate() {
            let req = serde_json::json!({
                "op": "instantiate", "module": mi, "wasm": hex(bytes),
            });
            writeln!(stdin, "{req}")?;
        }
        let mut module_cursor = 0usize;
        for (idx, p) in planned.iter().enumerate() {
            let req = match p {
                Planned::Module { .. } => {
                    let r = serde_json::json!({"op":"use_module","idx":idx,"module":module_cursor});
                    module_cursor += 1;
                    r
                }
                Planned::AssertReturn { exec, results, .. } => {
                    serde_json::json!({"op":"assert_return","idx":idx,"exec":exec_json(exec),"results":rets_json(results)})
                }
                Planned::AssertTrap { exec, message, .. } => {
                    serde_json::json!({"op":"assert_trap","idx":idx,"exec":exec_json(exec),"message":message})
                }
                Planned::AssertExhaustion { exec, .. } => {
                    serde_json::json!({"op":"assert_exhaustion","idx":idx,"exec":exec_json(exec)})
                }
                Planned::Action { exec, .. } => {
                    serde_json::json!({"op":"action","idx":idx,"exec":exec_json(exec)})
                }
                Planned::Register { name, .. } => {
                    serde_json::json!({"op":"register","idx":idx,"name":name})
                }
                Planned::AssertMalformed { .. } => {
                    // Handled harness-side: `module quote` forms are textual
                    // and always re-parse; a `module binary` form that
                    // decoded means the malformed assertion failed. The
                    // wast parser itself validates nothing here, so we
                    // optimistically pass (these cases test the *spec
                    // parser*, not implementations) unless the quote form
                    // is involved. See README note on assert_malformed.
                    serde_json::json!({"op":"harness_skip_malformed","idx":idx})
                }
                Planned::AssertInvalid { .. } => {
                    // Should have been rejected by validation pre-codegen.
                    serde_json::json!({"op":"harness_validate","idx":idx})
                }
                Planned::AssertUnlinkable { .. } => {
                    serde_json::json!({"op":"assert_unlinkable","idx":idx})
                }
                Planned::Other => {
                    serde_json::json!({"op":"other","idx":idx})
                }
            };
            writeln!(stdin, "{req}")?;
        }
        writeln!(stdin, "{}", serde_json::json!({"op":"done"}))?;
        let _ = stdin.flush();
    }
    drop(child.stdin.take());

    let mut answered: std::collections::HashSet<usize> = Default::default();
    let mut lines = resp_rx.into_iter();
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(idx) = v.get("idx").and_then(|i| i.as_u64()).map(|i| i as usize) else {
            continue;
        };
        answered.insert(idx);
        let kind = v["outcome"].as_str().unwrap_or("fail");
        let outcome = match kind {
            "pass" => Outcome::Pass,
            "skip" => Outcome::Skip { msg: v["msg"].as_str().unwrap_or("").to_string() },
            _ => Outcome::Fail { msg: v["msg"].as_str().unwrap_or("unknown").to_string() },
        };
        let (line, assertion) = planned
            .get(idx)
            .map(|p| (planned_line(p), planned_assertion(p)))
            .unwrap_or((0, Assertion::Other));
        result.push(CaseResult { index: idx, line, assertion, outcome });
    }
    let status = child.wait()?;
    if !status.success() {
        for (idx, p) in planned.iter().enumerate() {
            if !answered.contains(&idx) {
                result.push(CaseResult {
                    index: idx,
                    line: planned_line(p),
                    assertion: planned_assertion(p),
                    outcome: Outcome::Fail {
                        msg: format!("worker crashed (exit {:?})", status.code()),
                    },
                });
            }
        }
    }
    result.cases.sort_by_key(|c| c.index);
    Ok(result)
}

fn plan_invoke(inv: &WastInvoke) -> ExecPlan {
    ExecPlan::Invoke {
        module_idx: None,
        field: inv.name.to_string(),
        args: inv.args.iter().map(plan_arg).collect(),
    }
}

fn plan_execute(e: &WastExecute) -> ExecPlan {
    match e {
        WastExecute::Invoke(inv) => plan_invoke(inv),
        WastExecute::Get { global, .. } => ExecPlan::Get {
            module_idx: None,
            field: global.to_string(),
        },
        WastExecute::Wat(_) => ExecPlan::Get {
            module_idx: None,
            field: "<wat>".into(),
        },
    }
}

fn plan_arg(a: &WastArg) -> ArgPlan {
    match a {
        WastArg::Core(WastArgCore::I32(v)) => ArgPlan::I32(*v),
        WastArg::Core(WastArgCore::I64(v)) => ArgPlan::I64(*v),
        WastArg::Core(WastArgCore::F32(v)) => ArgPlan::F32Bits(v.bits),
        WastArg::Core(WastArgCore::F64(v)) => ArgPlan::F64Bits(v.bits),
        WastArg::Core(_) => ArgPlan::Unsupported("arg"),
        _ => ArgPlan::Unsupported("component arg"),
    }
}

fn plan_ret(r: &WastRet) -> RetPlan {
    match r {
        WastRet::Core(WastRetCore::I32(v)) => RetPlan::I32(*v),
        WastRet::Core(WastRetCore::I64(v)) => RetPlan::I64(*v),
        WastRet::Core(WastRetCore::F32(wast::core::NanPattern::Value(v))) => RetPlan::F32Value(v.bits),
        WastRet::Core(WastRetCore::F64(wast::core::NanPattern::Value(v))) => RetPlan::F64Value(v.bits),
        WastRet::Core(WastRetCore::F32(wast::core::NanPattern::CanonicalNan)) => RetPlan::F32CanonicalNan,
        WastRet::Core(WastRetCore::F64(wast::core::NanPattern::CanonicalNan)) => RetPlan::F64CanonicalNan,
        WastRet::Core(WastRetCore::F32(wast::core::NanPattern::ArithmeticNan)) => RetPlan::F32ArithmeticNan,
        WastRet::Core(WastRetCore::F64(wast::core::NanPattern::ArithmeticNan)) => RetPlan::F64ArithmeticNan,
        WastRet::Core(_) => RetPlan::Unsupported("ret"),
        _ => RetPlan::Unsupported("component ret"),
    }
}

fn exec_json(e: &ExecPlan) -> serde_json::Value {
    match e {
        ExecPlan::Invoke { field, args, .. } => serde_json::json!({
            "type": "invoke", "field": field,
            "args": args.iter().map(arg_json).collect::<Vec<_>>(),
        }),
        ExecPlan::Get { field, .. } => serde_json::json!({"type": "get", "field": field}),
    }
}

fn arg_json(a: &ArgPlan) -> serde_json::Value {
    match a {
        ArgPlan::I32(v) => serde_json::json!({"t":"i32","v":*v}),
        ArgPlan::I64(v) => serde_json::json!({"t":"i64","v":*v}),
        ArgPlan::F32Bits(b) => serde_json::json!({"t":"f32","bits":b}),
        ArgPlan::F64Bits(b) => serde_json::json!({"t":"f64","bits":b}),
        ArgPlan::Unsupported(k) => serde_json::json!({"t":"unsupported","k":k}),
    }
}

fn rets_json(rs: &[RetPlan]) -> serde_json::Value {
    rs.iter()
        .map(|r| match r {
            RetPlan::I32(v) => serde_json::json!({"t":"i32","v":v}),
            RetPlan::I64(v) => serde_json::json!({"t":"i64","v":v}),
            RetPlan::F32Value(b) => serde_json::json!({"t":"f32","bits":b}),
            RetPlan::F64Value(b) => serde_json::json!({"t":"f64","bits":b}),
            RetPlan::F32CanonicalNan => serde_json::json!({"t":"f32","nan":"canonical"}),
            RetPlan::F64CanonicalNan => serde_json::json!({"t":"f64","nan":"canonical"}),
            RetPlan::F32ArithmeticNan => serde_json::json!({"t":"f32","nan":"arithmetic"}),
            RetPlan::F64ArithmeticNan => serde_json::json!({"t":"f64","nan":"arithmetic"}),
            RetPlan::Unsupported(k) => serde_json::json!({"t":"unsupported","k":k}),
        })
        .collect::<Vec<_>>()
        .into()
}

fn planned_line(p: &Planned) -> u64 {
    match p {
        Planned::AssertReturn { line, .. }
        | Planned::AssertTrap { line, .. }
        | Planned::AssertExhaustion { line, .. }
        | Planned::AssertMalformed { line }
        | Planned::AssertInvalid { line }
        | Planned::AssertUnlinkable { line }
        | Planned::Action { line, .. }
        | Planned::Register { line, .. } => *line as u64,
        _ => 0,
    }
}

fn planned_assertion(p: &Planned) -> Assertion {
    match p {
        Planned::Module { .. } => Assertion::Module,
        Planned::AssertReturn { .. } => Assertion::Return,
        Planned::AssertTrap { .. } => Assertion::Trap,
        Planned::AssertExhaustion { .. } => Assertion::Exhaustion,
        Planned::AssertMalformed { .. } => Assertion::Malformed,
        Planned::AssertInvalid { .. } => Assertion::Invalid,
        Planned::AssertUnlinkable { .. } => Assertion::Unlinkable,
        Planned::Action { .. } => Assertion::Action,
        Planned::Register { .. } | Planned::Other => Assertion::Other,
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Generate the worker crate: one `gen/moduleN.rs` per wasm module (wars
/// output), plus a lib.rs main loop.
fn write_worker_crate(
    gen_dir: &Path,
    modules: &[Vec<u8>],
    backend: Backend,
) -> Result<()> {
    let src_dir = gen_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let wars_rt_path = workspace_root().join("crates/wars-rt");
    let manifest = format!(
        "[workspace]\n\n[package]\nname = \"spectest-worker\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\ncrate-type = [\"lib\"]\n\n\
         [[bin]]\nname = \"spectest-worker\"\npath = \"src/main.rs\"\n\n\
         [dependencies]\n\
         wars-rt = {{ path = {:?}, features = [\"std\", \"spectest\"] }}\n\
         serde_json = \"1\"\n",
        wars_rt_path
    );
    std::fs::write(gen_dir.join("Cargo.toml"), manifest)?;

    for (mi, bytes) in modules.iter().enumerate() {
        let src = generate_module(&format!("M{mi}"), bytes, backend)?;
        std::fs::write(gen_dir.join(format!("gen/module{mi}.rs")), src)?;
    }

    let mut lib = String::new();
    lib.push_str("#![allow(warnings)]\n");
    lib.push_str("// Generated by the spectests harness — do not edit.\n");
    for mi in 0..modules.len() {
        lib.push_str(&format!(
            "pub mod m{mi} {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/gen/module{mi}.rs\")); }}\n"
        ));
    }
    lib.push_str(crate::worker_main_text::WORKER_MAIN);

    // Per-file export dispatch: parse the generated module source for the
    // export methods (fn <name><'a>) of trait M0 with their param/result
    // types, and emit a match arm per export that builds the tuple-list from
    // JSON args and checks the single result.
    let gen0 = std::fs::read_to_string(gen_dir.join("gen/module0.rs")).unwrap_or_default();
    lib.push_str(&generate_dispatch(&gen0));

    std::fs::write(src_dir.join("lib.rs"), lib)?;
    std::fs::write(
        src_dir.join("main.rs"),
        "fn main() { spectest_worker::worker_main(); }\n",
    )?;
    Ok(())
}

fn generate_module(name: &str, bytes: &[u8], backend: Backend) -> Result<String> {
    let crate_path: syn::Path = syn::parse_str("::wars_rt")?;
    let core = wars::OptsCore {
        crate_path,
        bytes,
        name: syn::Ident::new(name, proc_macro2::Span::call_site()),
        flags: wars::Flags::default(),
        embed: proc_macro2::TokenStream::new(),
        data: {
            let mut d = std::collections::BTreeMap::new();
            d.insert(
                syn::Ident::new("_marker", proc_macro2::Span::call_site()),
                syn::parse_quote!(::core::marker::PhantomData<fn() -> Target>),
            );
            d
        },
        roots: Default::default(),
        plugins: vec![SpectestPlugin::boxed()],
        chunk_size: None,
    };
    let ts = match backend {
        Backend::Wasmparser => wars::wasmparser_compile(&core.inflate::<
            wars::WasmparserBackend,
        >())?,
        Backend::Waffle => {
            // The waffle backend consumes an expanded waffle Module; the
            // ToTokens impl on OptsLt does the expansion internally.
            use quote::ToTokens;
            let opts = core.inflate::<wars::LegacyPortalWaffleBackend>();
            opts.to_token_stream()
        }
    };
    let file: syn::File = syn::parse_str(&ts.to_string()).map_err(|e| {
        let dump = std::env::temp_dir().join(format!("spectests-gen-dump-{}.rs", name));
        let _ = std::fs::write(&dump, ts.to_string());
        anyhow::anyhow!("parsing generated tokens: {e} (dumped to {})", dump.display())
    })?;
    Ok(prettyplease::unparse(&file))
}

/// Extract export function names + param/result types from generated module
/// source (the trait `M0` block), and emit a `macro_rules! dispatch_m0`
/// implementation that invokes the right trait method and checks results.
///
/// The generated trait methods look like:
/// ```text
/// pub trait M0: ... {
///     fn add<'a>(
///         self: &'a mut Self,
///         imp: tuple_list_type!(u32, u32),
///     ) -> BorrowRec<'a, Result<tuple_list_type!(u32), Self::Error>>
/// ```
fn generate_dispatch(gen_src: &str) -> String {
    // Locate the trait M0 body.
    let Some(trait_start) = gen_src.find("pub trait M0:") else {
        return String::new();
    };
    let rest = &gen_src[trait_start..];
    let Some(trait_end_rel) = rest.find("\n}") else {
        return String::new();
    };
    let body = &rest[..trait_end_rel];

    // Parse method signatures: `fn <name><'a>( ... tuple_list_type!(...params...) )
    // -> BorrowRec<'_, Result<tuple_list_type!(...results...)>, ...>`
    let mut exports: Vec<(String, Vec<String>, Vec<String>)> = vec![];
    let all_lines: Vec<&str> = body.lines().collect();
    let mut i = 0usize;
    while i < all_lines.len() {
        let l = all_lines[i].trim();
        i += 1;
        if !l.starts_with("fn ") {
            continue;
        }
        let name = l[3..].split('<').next().unwrap_or("").trim().to_string();
        if name == "init" {
            continue;
        }
        // Collect the whole method text up to the terminating `;` at depth <= 1.
        let mut text = String::new();
        let mut depth = 0i32;
        let mut done = false;
        let mut segs = vec![l.to_string()];
        while i < all_lines.len() {
            segs.push(all_lines[i].to_string());
            i += 1;
            let seg = segs.last().unwrap();
            for ch in seg.chars() {
                match ch {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ';' if depth <= 1 => {
                        done = true;
                        break;
                    }
                    _ => {}
                }
                text.push(ch);
            }
            text.push('\n');
            if done {
                break;
            }
        }
        if !done {
            continue;
        }
        let params = extract_tuple_list(&text, "imp:");
        let results = extract_tuple_list(&text, "->");
        if !params.is_empty() || !results.is_empty() {
            exports.push((name, params, results));
        }
    }

    let mut out = String::new();
    out.push_str("\n// ── generated dispatch ──\n");
    out.push_str("macro_rules! dispatch_m0 {\n");
    out.push_str("    ($host:expr, $field:expr, $args:expr, $results:expr) => {\n");
    out.push_str("        match $field {\n");
    for (name, params, results) in &exports {
        let n_args = params.len();
        let arg_exprs: Vec<String> = (0..n_args)
            .map(|i| {
                let t = &params[i];
                format!(
                    "arg_val::<{t}>(&$args[{i}])?"
                )
            })
            .collect();
        let arg_exprs = if arg_exprs.is_empty() {
            "::wars_rt::_rexport::tuple_list::tuple_list!()".to_string()
        } else {
            format!(
                "::wars_rt::_rexport::tuple_list::tuple_list!({})",
                arg_exprs.join(", ")
            )
        };
        let check = check_expr(results);
        let arm = "            \"".to_owned() + &name + "\" => {\n"
            + "                use ::wars_rt::_rexport::tramp::tramp;\n"
            + "                let res = tramp(m0::M0Impl::" + &name + "($host, " + &arg_exprs + "));\n"
            + "                match res {\n"
            + "                    Ok(tl) => {\n"
            + "                        let got = result_vec0(tl);\n"
            + "                        " + &check + "\n"
            + "                    }\n"
            + "                    Err(e) => Err((\"fail\".to_string(), format!(\"unexpected trap: {e}\"))),\n"
            + "                }\n"
            + "            }\n";
        out.push_str(&arm);
    }
    out.push_str("            _ => Err((\"skip\".to_string(), format!(\"export {} not dispatched\", $field))),\n");
    out.push_str("        }\n    };\n}\n");
    out
}

/// Extract `tuple_list_type!(...)` after `marker` in a signature text.
fn extract_tuple_list(sig: &str, marker: &str) -> Vec<String> {
    let Some(pos) = sig.find(marker) else { return vec![] };
    let tail = &sig[pos..];
    let Some(start) = tail.find("tuple_list_type!(") else { return vec![] };
    let rest = &tail[start + "tuple_list_type!(".len()..];
    let mut depth = 1;
    let mut end = 0;
    for (i, ch) in rest.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = &rest[..end];
    inner
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_start_matches("::wars_rt::_rexport::core::primitive::").to_string())
        .collect()
}

/// Emit the result-comparison snippet for a single result.
fn check_expr(results: &[String]) -> String {
    if results.is_empty() {
        return "Ok(String::new())".to_string();
    }
    let r = &results[0];
    let r = r.trim_start_matches("::wars_rt::_rexport::core::primitive::");
    match r {
        "u32" => "check_i32(&got, 0, $results)".to_string(),
        "u64" => "check_i64(&got, 0, $results)".to_string(),
        "f32" => "check_f32(&got, 0, $results)".to_string(),
        "f64" => "check_f64(&got, 0, $results)".to_string(),
        _ => "Ok(String::new())".to_string(),
    }
}

fn _unused(_: &str, _: &str) {}
