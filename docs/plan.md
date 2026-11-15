# hdlsim — plan

Self-contained VHDL + Verilog simulator. Std-only Rust, zero external crates, no
wrapping of GHDL/Icarus/Verilator (including as test oracles).

## Naming
- `hdlsim` = the CLI binary. Library crates: `hdl-syntax`, `hdl-lib`, `hdl-ir`,
  `hdl-sim`, `hdl-wave`.

## Architecture
```
source → tokens → CST → AST → IR (elaborated) → sim events → sinks (waves/coverage/log)
```
- `hdl-syntax` — reusable lexers/parsers/CST/AST for Verilog + VHDL. Lossless CST
  (hand-rolled rowan-style green/red nodes, trivia kept), typed AST (`AstNode`),
  spans on every node, multi-file `SourceDb`, diagnostics with parse recovery.
  One entry API: `parse(Language::Verilog|Vhdl, &SourceFile) -> Parse<T>`.
  CLI escape hatch: `hdlsim ast file.sv --json` (own JSON serializer).
- `hdl-lib` — library manager. Logical libraries → dirs from `hdlsim.toml`,
  `~/.hdlsim/config.toml`, CLI flags (`-L name=dir`, `-f cmdfile`, `-I`, `-y`,
  `+libext`). Built-in precompiled `std`/`ieee`. Verilog: undefined module found in
  work → search_order libs → libdirs (`<Mod>.v/.sv`) → libfiles, compiled on demand
  recursively; `` `include `` via incdirs. VHDL: `library`/`use`/`entity lib.unit`
  via unit index (library, name, kind); latest-analyzed architecture rule.
  Dependency graph → automatic compile order; cycles reported with path.
  Incremental cache `.hdlsim-cache/` (own binary format, source hash + dep edges).
- `hdl-ir` — language-neutral elaborated IR (modules, signals, processes,
  instances, params), dumpable for other tools (synthesis/formal/coverage).
  Lowering resolves names through hdl-lib (`ResolveCtx`).
- `hdl-sim` — event-driven kernel: delta cycles, time wheel; Verilog 4-state +
  NBA region + `#delay`; VHDL 9-state std_logic + resolution + end-of-delta signal
  updates. Emits closed `SimEvent`s to `Vec<Box<dyn Observer>>`. System tasks via a
  name→fn registry. Embeddable as a library.
- `hdl-wave` — `WaveSink` registry: VCD, self-contained HTML/SVG viewer
  (`--wave out.html`), live HTTP+SSE wave window (`--live [--port N]`, pause/step),
  ASCII (`--waves-ascii`), `.hwf` binary. `hdlsim hierarchy --dot top` emits DOT.
- `hdlsim` (bin) — thin CLI: `init`, `compile`, `check`, `ast --json`, `run`,
  `test`, `wave`, `hierarchy`, `clean`, `regress` (phase 2). I/O only.

## Language scope
- Phase 1: practical Verilog-2005 (modules, ports, wire/reg/logic, params, assign,
  always/initial, if/case/casez/casex, operators, concat/replication/selects,
  memories, generate, functions, basic tasks, timescale; $display/$write/$strobe/
  $monitor/$finish/$stop/$time/$realtime/$random/$clog2, $dumpvars family,
  $readmemh/$readmemb) and practical VHDL (entity/architecture, std_logic family,
  numeric_std subset, processes + sensitivity, concurrent assignments,
  component+port map+generic map, generics, loops, slices, attributes, wait
  until/for in testbenches; assert/report, std.env.stop/finish).
- Phase 2 (done): coverage (line/branch/toggle/FSM + functional), regression runner
  with JUnit XML, LSP (`hdlsim lsp`: diagnostics/hover/definition/completion),
  formatter (`hdlsim fmt`), linter (`hdlsim lint --fix`), driver tracing,
  checkpoint/restore, mini-TCL `do` files, SVA/PSL-lite, `.hwf` viewer, parser
  fuzz harness, `hdlsim doc` HTML docs, `--seed` reproducible $random,
  `hdlsim doctor` / `hdlsim completions`, FST wave format (`--fst`, std-only
  writer + reader, readable by GTKWave and `hdlsim wave`), analog waves v1:
  Verilog `real` / VHDL `real` signals as IEEE-754 doubles in the kernel
  (arithmetic, comparisons, int<->real assignment conversion, `%f`/`%g`),
  rendered as analog traces in the HTML/live viewers and as VCD `r` values
  (`.hwf` v2 carries a real flag; FST real vars deferred).
- Phase 2 (remaining): FST `real` variables (proper GTKWave analog display);
  full analog kernel support (continuous `wreal`/VHDL-AMS is out of scope).
- Phase 3 prove notes: `hdl-sim::sat` is a MiniSat-style CDCL solver (two-watched
  literals, first-UIP learning, VSIDS, Luby restarts); `hdl-sim::bmc` encodes the
  elaborated IR 2-valued (X/Z→0, `rising_edge`≡true per step, div-by-0→0) with
  Tseitin gates and checks `assert property`/`assert` as safety properties.
  Memories, `#delay`, non-constant indices and dynamic loop bounds are clean
  errors. UNSAT proofs scale roughly linearly in BMC depth but can hit CDCL
  search cliffs (8-bit accumulator, depth 30 ≈ 20 s release).
- Phase 3 (done): reverse debug (`back [N]` in do-files), constrained-random
  stimulus (`$urandom`/`$urandom_range`/`$rand_solve`), own CDCL + BMC
  (`hdlsim prove --top dut --bmc N`).
- Phase 3 (remaining): SystemVerilog proper, VHDL-2008, UVM-lite.

## Performance
Arena-allocated IR, indices not pointers, zero allocation in the event loop; heap
time queue; deterministic single-thread first. Gate: 10M-cycle counter testbench in
a few seconds (release).

## Testing
Unit tests colocated per file; golden CST dumps; operator truth tables; multi-library
resolution tests; self-checking testbenches in both languages with golden stdout +
golden VCDs; wave/live-server tests via std TcpStream; hand-rolled parser fuzz
(no panics on arbitrary input). No external simulators anywhere.

## Milestones (each a reviewable commit slice; `cargo fmt --check` + `cargo test` green)
1. Workspace scaffold; hdl-syntax core (SourceDb, diagnostics, CST).
2. hdl-lib: config, unit index, dep graph, on-demand compile, cache.
3. Kernel (values/time/scheduler).
4. hdl-wave: VCD sink + ASCII + HTML viewer.
5. Verilog front-end end-to-end: counter tb passes, multi-lib, waves out.
6. VHDL front-end end-to-end + ieee built-ins.
7. Live wave window (HTTP+SSE).
8. CLI polish + JSON AST dump + cmdfiles.
9. Examples + golden tests + perf gate + docs.

See also: `docs/extensibility.md` (extension seams), `docs/worldclass.md` (feature
ideas), `REVIEWING.md` (code conventions).

## Progress log (for continuing in a new session)

All work ships as draft PRs from `vibe/<slug>-d3e0f0` branches, merged manually on
GitHub. Gate before every push: `cargo fmt --check` + `cargo test --workspace` green.
`cargo publish` is deferred by the owner until the project is complete (all 6 crate
names - hdl-syntax, hdl-ir, hdl-lib, hdl-sim, hdl-wave, hdlsim - verified unique on
crates.io; publish from the owner's machine with `cargo login` first, in dependency
order: hdl-syntax, hdl-ir, hdl-lib, hdl-sim, hdl-wave, hdlsim).

Merged PRs (main @ 19b5f60, 2026-11-15):

| PR | What |
|---|---|
| #1 | workspace publish-ready (unique crate names, metadata) |
| #2 | full Verilog + VHDL syntax in hdl-syntax, unit-tested |
| #3 | on-disk stdlib `.vhd` files + persistent `.hdlsim-cache/` parse cache |
| #4 | line/branch/toggle coverage (`run --coverage`) |
| #5 | `hdlsim regress` with JUnit XML |
| #6-#9, #17 | LSP slices 1-3: diagnostics, hover, definition, completion |
| #7, #8 | `hdlsim fmt` (+`--check`, `-w`), `hdlsim lint --fix` |
| #10 | `hdlsim do` mini-Tcl do-files |
| #11 | milestone 9: perf gate + golden VCD + loop/break fixes |
| #12 | `hdlsim deps` (transitive file dependencies of a top) |
| #13 | `hdlsim assist` (local prompt-to-task router) + `hdlsim explain` |
| #14 | `hdlsim gen tb` testbench skeleton generator |
| #15 | `hdlsim analyze` (W0501 undriven / W0502 unread) |
| #16 | driver tracing (`debug --driver`) + verilog multi-module name-leak fix |
| #18 | checkpoint/restore (`HDLCKPT1`, `run --checkpoint/--restore`) |
| #19 | SVA-lite (`assert property`, `|->`/`|=>`, immediate `assert`) |
| #20 | `.hwf` binary waves + `hdlsim wave` offline viewer |
| #21 | reverse debug (`back [N]` in do-files) |
| #22 | deterministic parser fuzz tests + verilog lexer span-overflow fix |
| #23 | `hdlsim doc` cross-linked HTML docs |
| #24 | `--seed N` reproducible `$random`; mixed-language co-sim tests |
| #25 | `hdlsim doctor` + `hdlsim completions` (bash/zsh/fish) |
| #27 | constrained-random stimulus: `$urandom`/`$urandom_range` + `$rand_solve` |
| #28 | `hdlsim prove` — own CDCL SAT solver + BMC over the elaborated IR |
| #29 | FST wave format: `run --fst out.fst` (std-only stored-deflate writer) + `hdlsim wave out.fst`; full inflate reader for foreign FST files |
| #30 | analog waves v1: `real` signals (Verilog/VHDL) as IEEE-754 doubles in the kernel + analog traces in HTML/live viewers, VCD `r` values, `.hwf` v2 real flag |

Remaining, in suggested order: FST real vars, then SystemVerilog proper /
VHDL-2008 / UVM-lite.
