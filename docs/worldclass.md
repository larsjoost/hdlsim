# What makes a simulator world-class — feature ideas for hdlsim

Constraints: std-only Rust, no external tools/libs, reusable CST/AST, reviewable code.

## Tier 1 — candidate phase-2 features (high value, fits the architecture)
| Feature | Why it matters |
|---|---|
| Mixed-language co-simulation (VHDL ↔ Verilog) in one kernel + one wave window | Native to our unified IR; commercial tools bolt it on |
| Built-in code coverage: line/branch/toggle/FSM + functional, merged across runs, HTML report | Usually a separate commercial license |
| Regression runner: test list, parallel via std::thread, JUnit XML + HTML, shuffle/seed | CI-grade regression out of the box |
| LSP server (`hdlsim lsp`): go-to-def across libraries, hover, diagnostics, rename, completion | Cheap: spans + library index already exist |
| Formatter `hdlsim fmt` + linter `hdlsim lint --fix` | Possible because the CST is lossless; rare in HDL |
| Driver tracing / cross-probing in the wave viewer; live force/release | The #1 debugging ask; we have spans + a live server |
| Checkpoint/restore of simulation state | Long sims resume in seconds |
| Mini-TCL interpreter for `do` files | ModelSim/Questa run scripts port unchanged |
| Hand-rolled parser fuzz harness (no panics, invariants hold) | Parser robustness = trust |
| `hdlsim doc`: cross-linked HTML docs from sources | Nearly free once LSP exists |
| SVA-lite / PSL-lite assertions with severity control | Modern verification is assertion-based |
| Constrained-random with seeded RNG (`--seed`) | Reproducible stimulus |
| Wave viewer power features: analog, transactions, pattern search, measurements, cursors | Competes with Surfer/GTKWave |

## Tier 2 — UX polish
- rustc-style diagnostics: color, snippet + caret, error codes with `--help <code>`
- Single static binary; self-generated shell completions; man page; `hdlsim doctor`
- Standalone `hdlsim wave file.vcd` viewer
- Transcript with timestamps; `$display` carries source location
- Correct timescale handling across mixed-language files
- Streaming wave output for huge designs (no full in-memory model)

## Tier 3 — stretch (phase 3+)
- Deterministic replay + reverse debugging
- Own CDCL SAT solver + bounded model checking (`hdlsim prove top --bmc N`)
- UVM-lite / full SystemVerilog
- Explicitly out of scope: VHDL-AMS/analog-mixed-signal, VPI/PLI (break zero-dep rule)

## Process/quality
- Authored conformance suite per language, run in CI
- Perf dashboard: events/sec tracked per release against the 10M-cycle gate
- Semver + changelog; versioned cache format with migration
- Compile-time plugin API (Rust modules) for custom system tasks / sinks
