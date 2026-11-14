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
- Phase 2: coverage (line/branch/toggle/FSM + functional), regression runner with
  JUnit XML, LSP (`hdlsim lsp`), formatter (`hdlsim fmt`), linter (`hdlsim lint
  --fix`), driver tracing in wave viewer, checkpoint/restore, mini-TCL `do` files,
  SVA/PSL-lite, FST, analog waves, `.hwf` streaming viewer.
- Phase 3: SystemVerilog proper, VHDL-2008, constrained-random, reverse debug,
  own CDCL + BMC (`hdlsim prove`), UVM-lite.

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
