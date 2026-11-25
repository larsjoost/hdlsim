# AGENTS.md — coding agents working in this repo

hdlsim is a self-contained VHDL + Verilog simulator in Rust (workspace under
`crates/`; no wrapping of GHDL/Icarus/Verilator). It is built to be driven by
coding agents: deterministic runs, machine-readable diagnosis, unambiguous
pass/fail. This file is the map; follow it instead of rediscovering it.

## Build, test, lint (run from the repo root)

- `cargo build` — build the workspace (debug profile).
- `cargo test` — the test suite. The `corpus` tests are `#[ignore]`d (they
  compile thousands of third-party files, ~minutes) and run only in the
  release CI corpus job, which passes `-- --ignored` to them explicitly.
- `cargo smoke` — the fast regression subset (attribute/port/generic
  lowering + core CLI semantics, see `.cargo/config.toml`); run it after
  every change, full `cargo test` before opening a PR.
- `cargo fmt --check` must pass — run `cargo fmt` before committing.
- `cargo clippy --all-targets -- -D warnings` must pass (CI gate, zero
  findings) — run `cargo clippy --fix` and fix the rest before opening a PR.
  This gate also enforces the workspace-wide baseline in `[workspace.lints]`
  (root `Cargo.toml`, opted in per crate with `lints.workspace = true`):
  no `unsafe`, no `dbg!`/`todo!`/`unimplemented` in any crate or target.
- `cargo clippy --workspace --lib -- -D clippy::unwrap_used -D clippy::panic`
  must pass (CI gate): `.unwrap()` never states an invariant, so library
  code uses `.expect("why this cannot fail")` or recovers from poisoning
  with `into_inner()`, and explicit `panic!` belongs in tests, not the
  kernel. Tests and CLI entry points are outside this rule.
- CI runs a blocking standards **ratchet** (`scripts/standards-baseline.sh
  --check`): per-file counts of functions over the 50-line split and over
  cognitive complexity 30 (thresholds in `clippy.toml`), plus module size
  (well under ~600 lines), are frozen in `docs/standards-baseline.json` and
  must never grow; files absent from the baseline must be clean. Improve
  opportunistically: regenerate with `scripts/standards-baseline.sh` and
  commit the lowered numbers with the change that earned them; raising a
  number needs review agreement. The per-commit change-size check
  (≤ ~400 lines) stays advisory — `numstat` double-counts moved lines, so
  verify pure moves with `git diff --color-moved=dimmed-zebra`. Function
  ≤ 30, nesting ≤ 2, and dedup stay review-level (nesting's lint is
  nightly-only).

## Workflow (required)

- **Never commit or push to `main`.** Work on a branch
  (`git checkout -b vibe/<slug>-<id>`), push it, open a PR with
  `gh pr create --draft --head <branch> ...`. Merging happens through the PR.
- One logical change per commit. Follow `REVIEWING.md`: `//!` file headers,
  `///` on public items, no `unwrap()` on source-derived data, errors as
  `Result<T, Diagnostic>`, deterministic output, tests colocated in
  `#[cfg(test)]` plus end-to-end tests in `crates/hdlsim/tests/`.
- Docs map: `README.md` (features), `docs/project-context.md` (architecture,
  module table, commands), `docs/extensibility.md` (extension seams),
  `docs/plan.md` (change history), `REVIEWING.md` (review conventions),
  `docs/worldclass.md`.

## The debug loop (edit → simulate → diagnose)

When a simulation or compile fails, run these in order — they are the fastest
path from "something is wrong" to "here is the line":

1. `hdlsim syntax file.vhd` (or `check --syntax-only file.v`) — parse-only
   check of the edited file in milliseconds.
2. `hdlsim check [files]` — full compile + elaboration, rustc-style
   diagnostics with `file:line:col` carets. Add `--error-format json` (global
   flag, works anywhere on the command line) to get one JSON object per
   diagnostic on stderr instead — parse `code`/`severity`/`message`/
   `file`/`line`/`column`/`byte_start`/`byte_end`/`help` programmatically.
3. `hdlsim run --top tb [files] --max-time 10us --max-wall 30s` — always bound
   a run (`--max-time`/`--max-wall`) so a hang fails fast with a location dump
   (exit 124/130) instead of blocking. `--seed N` makes random stimulus
   reproducible.
4. On a test failure, the transcript line now carries a `file:line:` prefix
   (e.g. `tb.vhd:42: assertion failed at t=...`) and the summary prints
   `hdlsim: FAILED — N test failure(s) (first at tb.vhd:42)` — jump there.
5. To see why a signal is wrong: `hdlsim run --top tb --list-signals=PATTERN`
   to get exact hierarchical names, then `--watch sig --watch-x` (first
   x/z source) or `hdlsim debug --top tb --driver sig --dump`. `--trace`,
   `--trace-dispatch`, `--trace-picked`, `--trace-calls` print the scheduler
   trace on stderr.
6. Waves: `--vcd w.vcd` / `--hwf` / `--fst` / `--wave out.html` on `run`;
   `--coverage` for a line/branch report; `hdlsim regress --junit out.xml` to
   run many testbenches for CI.

Exit codes: `0` = clean pass; `1` = compile error or failed test
(assert/report severity error|failure, `$error`/`$fatal`) or an internal guard
stop; `2` = usage error (bad flags); `124` = `--max-wall` expired; `130` =
Ctrl-C/SIGTERM (dumps the active process and call chain first).

## Hard rules

- `project/` holds private HDL designs: **read allowed, edit denied** (see
  `docs/restricted-paths.jsonc` and `docs/project-context.md`). Nothing in the
  repo may be traceable to it — no copied or lightly renamed snippets, names,
  or wave data in tests, fixtures, examples, docs, commits, or PRs. Sim
  artifacts (waves, `.hdlsim-cache/`, logs) stay under `project/` (gitignored);
  never write them into tracked paths.
- External crates are limited to the vetted set in `[workspace.dependencies]`
  (thiserror, anyhow, clap, serde, serde_json, log, ctrlc); prefer std. A crate
  opts in with `x.workspace = true`; anything outside the set needs a
  justification in the PR description.
- `crates/hdlsim/tests/corpus/` contains third-party sources — keep upstream
  licensing intact when touching it.
- New CLI subcommands: one module under `crates/hdlsim/src/commands/` plus one
  entry in `COMMANDS` (a unit test asserts every command has a help section).
  New debug knobs are global flags stripped in `apply_global_args`, never
  environment variables (no spell check on env vars).
