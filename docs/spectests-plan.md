# Plan: WebAssembly Spec Tests (spectests) Conformance Test + CI — wars2

## Goal

Run the official [WebAssembly spec tests](https://github.com/webassembly/spec)
(`test/core/*.wast`) through the **wars2** pipeline — wasm → generated Rust →
native execution — per assertion, per backend, and wire it into CI.

Companion plan for the `wars` (v0.6) repo: see
`../wars-spectests/docs/spectests-plan.md`. The two share a design; this
document covers wars2-specific architecture and divergences.

## Repo facts (verified)

- Worktree: `/Users/g/Code-local/portal-hot/wars2-spectests`, branch
  `spectests` off `main` @ `12473ef`.
- `crates/wars` exposes `OptsCore` (bytes, name, `Flags`, `plugins: Vec<Arc<dyn Plugin>>`)
  and a `Backend` trait with two implementations:
  - `WasmparserBackend` (`new_backend.rs`) — single-pass, ABI v0, `wasmparser` 0.240.
  - `LegacyPortalWaffleBackend` (`impl.rs`) — waffle IR + relooper + unswitch.
  Both feature-gated; `tester` enables both.
- `Plugin` trait: `pre` / `import` / `mem_import` / `post` / `bounds` /
  `exref_bounds` — same shape as wars 0.6, so the spectest host module plugs
  in the same way.
- **Traps are `Result`s, not panics**: `wars-rt` intrinsics
  (`i32divs`, `i32divu`, loads/stores, conversions, …) return
  `anyhow::Result<tuple_list!(T)>`; generated code propagates
  `Err(e) -> return #fp::ret(Err(e))` through the trampoline
  (`portal-pc-tramp`). This is a major advantage over wars 0.6 for
  `AssertTrap` fidelity — but trap *classification* (which trap) must be
  verified; the error payload type is caller-chosen via `CtxSpec::Error`.
- Generated-code consumption pattern proven by `crates/tester`:
  `build.rs` generates code → `prettyplease` → `include!` from `OUT_DIR`;
  host context implements the generated module trait + `CtxSpec`; calls go
  through `tramp(host.fn(tuple_list!(…)))`.
- `wars-rt` is `no_std` + `alloc`; `std` feature exists. `wrl` module
  (wasm_runtime_layer bridge) is present but its dependency is commented out.
- **Baseline status: `cargo test --workspace` currently FAILS** — 4 of 8
  `tester` tests fail, all on the **waffle backend** (`test_waffle_add`
  returns `(0, ())` instead of `(30, ())`; `calladd` and others too). The
  wasmparser-backend tests pass. The spectest harness must therefore treat
  backends independently and report per-backend results.

## Deliverables

1. New crate `crates/spectests` (workspace member) — harness library + binary.
2. `SpectestPlugin` implementing `wars::Plugin` (host module `spectest`).
3. Wast-script driver (`wast` crate) with per-assertion execution.
4. **Per-backend matrix**: each suite runs against `WasmparserBackend` and
   `LegacyPortalWaffleBackend` (behind `--backend` filter).
5. Known-failures manifest `crates/spectests/known-failures.toml` (keyed by
   file + command index + backend).
6. GitHub Actions workflow `.github/workflows/spectests.yml`.

## Step 1 — Vendor spec tests

- Git submodule `https://github.com/webassembly/spec` →
  `crates/spectests/spec`, pinned to a release tag, shallow.
- CI: `git submodule update --init --depth 1 crates/spectests/spec`.
- Weekly scheduled CI job proposes a submodule bump PR for triage.

## Step 2 — Wast parsing

- `wast` + `wat` + `wasmparser` crates in the harness only. Commands:
  `Module`, `AssertReturn` (incl. `nan:canonical` / `nan:arithmetic`),
  `AssertTrap`, `AssertExhaustion`, `AssertInvalid`, `AssertMalformed`,
  `AssertUnlinkable`, `Register`, `Thread`.
- Malformed/invalid binaries are pre-filtered by `wasmparser::Validator`
  before ever reaching `wars` — both `wars` backends `panic!` on malformed
  input today (e.g. `unreachable!("wasm function … fell off end")` is fine,
  but decode paths are not hardened), so the validator gate is mandatory.

## Step 3 — Driver: in-process generation, cargo-compile, dlopen

wars2's codegen is a library API producing `TokenStream`, and the `tester`
crate already proves the consumption pattern. Two execution strategies;
start with (a):

### (a) Batch crate compilation (primary)

1. For each spec module: build `OptsCore` with the module bytes, a mangled
   name (`spec_<file>_<idx>`), and `plugins: vec![Arc::new(SpectestPlugin)]`.
2. Run **both** backends (`WasmparserBackend` and, feature-enabled,
   `LegacyPortalWaffleBackend`) → two token streams per module.
3. Emit each into a scratch cargo project
   (`target/spectests/gen/`): one generated crate per backend holding all
   modules of a `.wast` file as `#[path]` modules (amortizes rustc startup;
   prettyplease-format for debuggability).
4. The generated crate depends on `wars-rt` (`std`) and a thin
   `spectests-shim` crate exposing the C-ABI surface below.
5. Harness `libloading`s the cdylib and drives assertions.

### (b) build.rs / include! (secondary, for inner-loop dev)

Mirror `tester`: generate into `OUT_DIR` at harness build time for a pinned
subset of suites, run as normal `#[test]`s. Useful for debugging failures,
not for full-suite CI.

### C-ABI shim surface (per module instance)

- `spec_init(name)` — instantiate: run start section, data/elem application.
- `spec_invoke(name, export_hash, args…) -> ResultCode` — args/results as
  C-ABI scalars; reference values as opaque handles into a harness-side
  registry. Return encodes trap classification (`Ok` / trap-kind enum).
- `spec_global_get/set(name, hash, …)` for `invoke … (get …)` assertions.
- Trap mapping: `CtxSpec::Error` is set to a harness error enum carrying a
  `TrapKind`; `SpectestPlugin::post` / shared glue maps `Err` to the C-ABI
  tag. **Audit needed**: confirm each trapping intrinsic in `wars-rt`
  (div/rem, load/store OOB, `unreachable`, indirect call type mismatch,
  conversion overflow, grow failure) yields a distinguishable error.

### Host module & linking

`SpectestPlugin` provides the standard `spectest` imports:
`print*` family (7 sinks), globals `global_i32` (666) / `global_i64` /
`global_f32` / `global_f64`, `table` (funcref 10..20), `memory` 1..2 pages
(via `mem_import`). `Register` support: harness keeps `name → instance`
map; `SpectestPlugin.import` resolves against compiled instances first, then
builtin spectest. Cross-module calls are direct Rust calls within the same
generated crate when both modules live in one file — the common case for
`imports.wast` / `linking.wast`; the shim exports `spec_link(name_a,
export_hash, name_b, import_slot)` for cross-crate cases.

## Step 4 — Assertion semantics

| Assertion | Behavior |
|---|---|
| `AssertReturn` | invoke via shim; compare results incl. NaN bit-pattern classes (canonical/arithmetic, sign-agnostic) |
| `AssertTrap` | shim returns trap tag; compare message prefix where the wast specifies it |
| `AssertExhaustion` | call-stack exhaustion — **note**: trampolining means unbounded recursion may not blow the Rust stack; may instead hang or hit an OOM in the trampoline arena. Needs explicit depth-limit support (count frames in shim) or a `Flags` addition. Verify `fac.wast`-style deep recursion behavior first. |
| `AssertInvalid`/`AssertMalformed` | `wasmparser::Validator`/`wat` parse must reject **before** codegen; also assert `wars` codegen doesn't panic if somehow reached |
| `AssertUnlinkable` | import resolution in `SpectestPlugin` returns a link error → trap-kind `unlinkable` |
| `Register` | instance rebinding in harness registry |

## Step 5 — Reporting & manifest

- One JSON line per command from the worker; per-file and per-backend totals
  `{pass, fail, known_fail, skip}`; CI step-summary table.
- Manifest keyed by `(file, cmd-idx, backend)` so a waffle-only failure
  doesn't mask wasmparser progress:
  ```toml
  [["float_exprs.wast"]]
  idx = 212
  backend = "waffle"
  reason = "wrong result on fused-multiply-subtract"
  ```
- `--check` mode: fails on unexpected failures **and** stale (now-passing)
  manifest entries.

## Step 6 — Suite phasing

1. **Phase 0 (smoke, fix-first):** make `crates/tester`'s 8 tests pass — the
   waffle backend is currently broken on its own smoke test; spectests should
   not be built on a red baseline. File/land the `add`/`calladd` fix before or
   alongside PR 1.
2. **Phase 1 (wasmparser backend, MVP suites):** `i32`, `i64`, `f32`, `f64`,
   `f32_cmp`, `f64_cmp`, `int_exprs`, `float_exprs`, `float_literals`,
   `conversions`, `memory*`, `data`, `start`, `labels`, `block`, `loop`,
   `br*`, `call*`, `local_*`, `global`, `select`, `stack`, `switch`,
   `unwind`, `forward`, `fac`, `func`, `if`, `left-to-right`, `load`,
   `store`, `address`, `align`, `endianness`, `unreached-valid`,
   `unreachable`, `traps`, `binary`, `binary-leb128`, `custom`, `elem`,
   `exports`, `imports`, `linking`, `names`, `nop`, `return`, `type`,
   `token`.
3. **Phase 2:** reference types (`ref_*`, `table*`, `bulk`), `multiple_*`,
   SIMD (`simd_*.wast`) — wasmparser backend first.
4. **Phase 3:** waffle-backend parity runs + proposal suites (tail-call, GC —
   `dumpster` feature, exception-handling) behind `--proposal`.
   GC suites need `CtxSpec`/`func::Value` GC support validation.

## Step 7 — CI

`.github/workflows/spectests.yml`:

```yaml
name: spectests
on:
  push: { branches: [main] }
  pull_request:
  schedule:
    - cron: "0 3 * * 1"

jobs:
  spectests:
    runs-on: ubuntu-latest
    timeout-minutes: 90
    strategy:
      fail-fast: false
      matrix:
        backend: [wasmparser, waffle]
    steps:
      - uses: actions/checkout@v4
        with: { submodules: recursive }
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Run spec tests
        run: >
          cargo run -p spectests --release --
          --backend ${{ matrix.backend }}
          --report out/report-${{ matrix.backend }}.json
      - uses: actions/upload-artifact@v4
        if: always()
        with: { name: report-${{ matrix.backend }}, path: out/ }
      - name: Enforce expectations
        run: >
          cargo run -p spectests --
          --check out/report-${{ matrix.backend }}.json
          crates/spectests/known-failures.toml
```

- GitHub Linux runners are the target — no QEMU/emulation involved (the
  AGENTS.md QEMU policy only applies if tests were ever run inside a Linux
  guest from the macOS VM; CI is native Linux).
- Concurrency group with `cancel-in-progress`; weekly cron also drives the
  submodule-bump PR.

## Risks / open questions

1. **Red baseline (highest priority):** waffle backend fails `tester`'s own
   add/calladd tests at `12473ef`. Phase 0 unblocks everything.
2. **Trap classification coverage:** verify every trapping intrinsic returns
   a classifiable `Err`; `unreachable` and call-indirect mismatch paths in
   both backends need auditing (they may `panic!` today — grep shows
   `unreachable!("wasm function {} fell off end")` used as codegen fallback).
3. **AssertExhaustion vs trampolining:** tramp defeats native stack checks;
   explicit frame-limit knob may be required in the shim or `Flags`.
4. **rustc wall-time** for ~hundreds of generated modules per backend:
   batch-per-file crates, `--release` in CI only, parallel `.wast` workers.
5. **`no_std` runtime + `std` feature in generated crates**: harness always
   builds with `std`; consider one no_std smoke suite later for embedders.
6. **SIMD**: neither backend's SIMD coverage is known — expect a large
   known-fail list; land with per-instruction report granularity.

## PR sequence

1. PR 1: fix `tester` waffle-backend failures (Phase 0).
2. PR 2: `crates/spectests` skeleton — submodule, wast parse, JSON report,
   asserts skipped.
3. PR 3: `SpectestPlugin` + shim crate + in-process `AssertReturn` for
   numeric suites on the wasmparser backend.
4. PR 4: process isolation, trap tags, `AssertTrap`/`AssertInvalid`/`Register`
   /`AssertUnlinkable`; `AssertExhaustion` depth knob.
5. PR 5: waffle-backend matrix + known-failures manifest + `--check` +
   GitHub Actions workflow.
6. PR 6: Phase 2/3 expansion.
