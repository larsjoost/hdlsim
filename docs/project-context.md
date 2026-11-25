# Project Context

**Project name:** hdlsim

## Project purpose

A self-contained VHDL + Verilog simulator written in Rust — lexer, parser,
library manager, elaboration, simulation kernel, and graphical wave display,
all implemented here (no wrapping of GHDL, Icarus, or Verilator). Used to
simulate and debug HDL designs from the command line.

**Primary aim:** the debug and verification tool for AI-driven VHDL/Verilog
development. Features are judged by how well they serve an agent's
edit → simulate → diagnose loop: LRM-conformant semantics pinned by regression
fixtures, unambiguous pass/fail (exit code + zero errors/warnings), bounded
runs (`--max-time`), and machine-readable diagnosis (`--trace`
scheduler trace, rustc-style diagnostics, JUnit XML).

## Architecture overview

Both language front-ends lower to a shared IR, so mixed VHDL/Verilog designs
simulate in one kernel. Named libraries map to directories via `hdlsim.ini`
(`hdlsim init`) or `-L name=dir`; undefined units are found and compiled in
dependency order. `std`/`ieee` are built in as real VHDL sources embedded with
`include_str!`.

## Main modules

| Module / area | Responsibility | Key entry points |
|---------------|----------------|------------------|
| `crates/hdl-syntax` | Lossless CST + typed AST, spans, parse recovery for Verilog and VHDL | `hdl_syntax::parse` |
| `crates/hdl-ir` | Shared intermediate representation for both languages | |
| `crates/hdl-lib` | Built-in `std`/`ieee` VHDL sources (`stdlib/`) | |
| `crates/hdl-sim` | Simulation kernel | |
| `crates/hdl-wave` | Wave display / formats (HTML, live SSE, VCD, FST, ASCII) | |
| `crates/hdlsim` | CLI binary, library manager, project export | `hdlsim run`, `hdlsim init`, `hdlsim project` |

## Tech stack

| Layer     | Technology | Version / notes |
|-----------|------------|-----------------|
| Language  | Rust       | Workspace under `crates/`, vetted deps in `[workspace.dependencies]` |
| CI        | GitHub Actions | `.github/workflows/ci.yml` |

## Build commands

```bash
cargo build
```

## Test commands

```bash
cargo test        # corpus tests are #[ignore]d; the release CI corpus job runs -- --ignored
cargo smoke       # fast regression subset (see .cargo/config.toml)
```

## Standards ratchet

Shape debt — functions over the 50-line split line, functions over cognitive
complexity 30, modules over ~600 lines — is frozen per file in
`docs/standards-baseline.json`. CI fails when any count grows or a file
gains debt it does not have an entry for; improvements are committed by
regenerating the file with `scripts/standards-baseline.sh` alongside the
change that earned them.

## Workflow

- **Never commit directly to `main`.** Create a branch, push it, and open a
  GitHub PR (`gh pr create`). Merge happens through the PR, not by push.

## Security constraints

- **`project/` holds private HDL designs** — one subfolder per design, each
  with its own `hdlsim.ini` (`hdlsim init` runs **inside** the design's
  folder; sims run from there too). It is readable for debugging, and tests in
  this repo may be **inspired** by it — but **nothing in the repository may be
  traceable to it**: never copy or lightly rename source snippets, names,
  identifiers, comments, file structure, constants, or wave data into tests,
  fixtures, examples, docs, commit messages, PRs, or issues. A derived
  regression test must be re-derived and re-expressed in hdlsim's own fixture
  style with invented names — renaming alone is not enough.
- All simulation artifacts (waves, VCD/FST, logs, parse cache) stay under
  `project/` (gitignored) — never write them into tracked paths.
- `project/` is `edit: deny` in `docs/restricted-paths.jsonc`; if the design
  itself must change, the user edits it.

## Restricted paths

> **Do not list folders here.** Edit `docs/restricted-paths.jsonc` and set
> `"confirmed": true`, then run the command below from the top folder of this
> project. The command updates `opencode.jsonc`. It does not change this file
> or the restricted-path file.

```powershell
<your-ai-harness-folder>\scripts\init-project.ps1 -Apply
```

| What you write | What it means |
|----------------|---------------|
| `read: deny`   | The AI must not open those files or search them |
| `edit: deny`   | The AI must not change those files |
| `read: allow`  | You may write it. It does not grant access |

## Known risks

- `crates/hdlsim/tests/corpus/` contains third-party sources (e.g. VUnit);
  keep upstream licensing intact when touching tests.

## Glossary

| Term | Definition |
|------|------------|
| Testbench (`tb_*`) | Top-level HDL unit that drives stimulus and checks responses |
| Elaboration | Resolving the design hierarchy into a simulable netlist/IR |
