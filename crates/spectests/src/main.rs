//! CLI entry point for the spectests harness.
//!
//! Modes:
//! - default: run the suite (filtered by CLI args) and write a JSON report
//! - `--check <report.json>`: enforce expectations from the known-failures
//!   manifest (unexpected failures and stale entries fail)
//! - `--write-manifest`: regenerate the manifest from current failures

mod manifest;
mod plugin;
mod report;
mod runner;
mod worker_main_text;

use anyhow::{bail, Result};
use clap::Parser;
use manifest::Manifest;
use report::Report;
use runner::{Backend, Runner};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "spectests", about = "WebAssembly spec-test conformance harness for wars2")]
struct Args {
    /// Backends to exercise (comma-separated: wasmparser,waffle).
    #[arg(long, default_value = "wasmparser")]
    backends: String,

    /// Directory containing the spec test suite (…/test/core) or any directory
    /// of .wast files.
    #[arg(long, default_value = "crates/spectests/spec/test/core")]
    spec_dir: PathBuf,

    /// Only run files whose stem matches this substring.
    #[arg(long)]
    filter: Option<String>,

    /// JSON report output path.
    #[arg(long, default_value = "out/spectests-report.json")]
    report: PathBuf,

    /// Known-failures manifest path.
    #[arg(long, default_value = "crates/spectests/known-failures.toml")]
    manifest: PathBuf,

    /// Check the report against the manifest instead of running tests.
    #[arg(long)]
    check: bool,

    /// Regenerate the manifest from the failures in `--report`.
    #[arg(long)]
    write_manifest: bool,

    /// Print failure details to stdout.
    #[arg(long)]
    verbose: bool,

    /// Maximum files to run (smoke-testing).
    #[arg(long, default_value_t = usize::MAX)]
    limit: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.check {
        let report_path = &args.report;
        let text = std::fs::read_to_string(report_path)?;
        let report: Report = serde_json::from_str(&text)?;
        let m = Manifest::load(&args.manifest)?;
        m.check(&report)?;
        let t = report.totals();
        println!(
            "check OK: {} pass, {} fail (all known), {} skip",
            t.pass, t.fail, t.skip
        );
        return Ok(());
    }

    if args.write_manifest {
        let text = std::fs::read_to_string(&args.report)?;
        let report: Report = serde_json::from_str(&text)?;
        let m = Manifest::from_report_failures(&report);
        m.save(&args.manifest)?;
        println!("wrote {} entries to {}", m.entries.len(), args.manifest.display());
        return Ok(());
    }

    let mut backends = vec![];
    for b in args.backends.split(',') {
        backends.push(match b.trim() {
            "wasmparser" => Backend::Wasmparser,
            "waffle" => Backend::Waffle,
            other => bail!("unknown backend: {other}"),
        });
    }

    let core_dir = if args.spec_dir.join("core").exists() {
        args.spec_dir.join("core")
    } else {
        // Allow pointing directly at a directory of wast files.
        args.spec_dir.clone()
    };
    if !core_dir.exists() {
        bail!(
            "spec test suite not found at {} — add the submodule with `git submodule update --init`",
            core_dir.display()
        );
    }

    let runner = Runner {
        gen_root: PathBuf::from("target/spectests-gen"),
        backend: backends[0],
    };

    let mut wast_files: Vec<PathBuf> = std::fs::read_dir(&core_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "wast").unwrap_or(false))
        .collect();
    wast_files.sort();
    if let Some(f) = &args.filter {
        wast_files.retain(|p| p.file_stem().unwrap().to_string_lossy().contains(f));
    }
    wast_files.truncate(args.limit);

    let mut report = Report::default();
    for backend in backends {
        let runner = Runner { backend, ..runner.clone() };
        for path in &wast_files {
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            eprint!("{stem} [{}] ... ", backend.name());
            match runner::run_file(&runner, path, None) {
                Ok(fr) => {
                    let c = fr.counts();
                    eprintln!("pass={} fail={} skip={}", c.pass, c.fail, c.skip);
                    report.files.push(fr);
                }
                Err(e) => {
                    eprintln!("ERROR: {e:#}");
                }
            }
        }
    }

    report.write_json(&args.report)?;
    println!("{}", report.summary_markdown());

    if args.verbose {
        for (file, fails) in report.failures() {
            println!("== {file}");
            for (idx, line, kind, msg) in fails.iter().take(20) {
                println!("  [{kind}] cmd {idx} (line {line}): {msg}");
            }
        }
    }

    let t = report.totals();
    if t.fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}
