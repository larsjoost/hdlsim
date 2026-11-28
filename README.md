# hdlsim

[![CI](https://github.com/larsjoost/hdlsim/actions/workflows/ci.yml/badge.svg)](https://github.com/larsjoost/hdlsim/actions/workflows/ci.yml)

A self-contained VHDL + Verilog simulator written in Rust with only a small,
vetted set of common external crates (declared in `[workspace.dependencies]`),
and no wrapping of GHDL, Icarus Verilog, Verilator, or any other existing
simulator. Lexers, parsers, library manager, elaboration, simulation kernel, and
the graphical wave display are all implemented here.

**Primary aim: the debug and verification tool for AI-driven VHDL/Verilog
development.** hdlsim is built for coding agents that write and debug HDL. An
agent needs a simulator it can trust and drive programmatically: LRM-conformant
semantics pinned by regression fixtures, deterministic reproducible runs,
unambiguous pass/fail (exit code plus a zero-errors/zero-warnings bar), fast
failure instead of deadlock (`--max-time`, `--max-wall`), and machine-readable diagnosis
(`--trace` scheduler trace, rustc-style diagnostics, JUnit XML). Interrupting a
hung run (Ctrl-C/SIGTERM) dumps the active process, file:line, and function chain before
exiting 130. The
human-facing surface — wave windows, LSP, formatter — serves the same loop from
the other side; every feature is judged by how well it shortens the agent's
edit → simulate → diagnose cycle.

## Highlights

- **Reusable syntax trees.** `hdl-syntax` exposes a lossless CST and typed AST for
  Verilog and VHDL with source spans and parse recovery, so other applications
  (linters, formatters, LSP, doc generators) can build on the same parser:
  `hdl_syntax::parse(Language::Verilog | Language::Vhdl, &source)`.
- **Automatic dependency search across libraries.** Named libraries mapped to
  directories (`hdlsim.ini` — created by `hdlsim init` — plus `-L name=dir` flags);
  undefined modules, `` `include `` files, and `use`-clause packages
  (`use lib.pkg.all;`) are found and compiled on demand in dependency order,
  with an in-session compile cache. Works out of the box: with no `hdlsim.ini`
  and no `-L`, the current directory is indexed as `work`, and any unit still
  missing is found by searching the project tree (with a warning pointing at
  `hdlsim init` / `-L` to make it instant). `std` and `ieee` are built in
  as real VHDL source files (`crates/hdl-lib/stdlib/`), embedded into the binary
  with `include_str!` and parsed by the same front-end, so they always resolve —
  no on-disk stdlib needed. They are searched last so `-L ieee=...` overrides them;
  `$HDLSIM_STDLIB` points the built-ins at a custom source tree.
- **Persistent parse cache.** The CLI caches each file's parse tree under
  `.hdlsim-cache/` (own binary format, keyed by source-text hash + format
  version), so repeated runs skip lexing/parsing unchanged files. Control with
  `--no-cache` or `--cache-dir path`. Within one run, each file is
  compiled at most once regardless.
- **Phase timing and peak-RSS reporting.** Every `run`/`debug` prints one stderr
  line per phase — `hdlsim: compile 1.8s (524 modules) peak-rss 210 MB`, then
  `elaborate`, `sim-init`, `simulate` — so a slow or hungry run localizes itself
  without external sampling. `--max-mem-mb <MiB>` turns the peak reading
  into a guard: crossing it stops the run with an actionable diagnostic instead
  of the OS OOM-killer (exit 137, no output).
- **Interrupt diagnosis.** Ctrl-C or SIGTERM stops the kernel and prints where
  it was — current time, active process origin (file:line), function call
  chain — then exits 130; `--max-wall 30s` does the same on a wall-clock
  budget with exit 124. `--progress-sec <seconds>` additionally prints a
  periodic "still running" heartbeat with the same location for runs that
  grind without time advance.
- **Progress ticks.** `hdlsim run --progress` prints a stderr line while the
  sim runs: with `--max-time 5us`, one line per decile of the budget the
  simulated time crosses — `hdlsim: sim t=1.2us / 5us (24%)` — so a long run
  shows how close it is to the stop time; without `--max-time`, rate-limited
  `hdlsim: sim t=...` lines (>= 500 ms apart, only when time moved). Off by
  default; all other output is byte-identical.
- **Graphical display.** `hdlsim run tb --live` opens a real-time wave window in the
  browser (built-in HTTP server + SSE, pause/step controls); `--wave out.html`
  writes a self-contained interactive viewer; `--vcd` for interchange; `--fst`
  writes GTKWave's FST format (std-only writer/reader, no dependencies); ASCII
  waves for logs. Verilog `real` / VHDL `real` signals simulate as IEEE-754
  doubles and render as analog traces (VCD `r` values, HTML/live polylines,
  FST real vars in GTKWave).
- **rustc-style diagnostics.** Every parse/lower error is rendered like rustc —
  `error[E..]: message` with the source line, a caret span, and notes.
- **Project export for other tools.** `hdlsim project --format vivado-tcl
  --format json manifest` generates Vivado TCL project files (and a JSON manifest)
  from the same library/compile machinery, through the public `hdlsim::project`
  API so external tools can reuse it.
- **One kernel for both languages.** Both front-ends lower to a shared IR, so
  mixed VHDL/Verilog designs simulate natively in one wave window.
- **Reproducible constrained-random stimulus.** `$urandom`,
  `$urandom_range(min, max)`, and `$rand_solve("a: 0..255; len: 1..16; a + len <= 260", "a", a, "len", len)`
  — a small deterministic constraint solver over the seeded RNG. `--seed N`
  (or do-file `seed N`) makes the whole stimulus sequence reproducible.
- **Easy to extend.** Closed core data structures, open trait+registry seams at
  every pipeline edge — adding a language, system task, wave format, lint rule, or
  CLI command is a new module plus one registration line. See
  `docs/extensibility.md`.

## Workspace layout

```
crates/
├── hdl-syntax/   lexers, parsers, CST/AST for Verilog + VHDL (reusable library)
├── hdl-lib/      named libraries, search paths, on-demand compile, dep graph, cache
├── hdl-ir/       language-neutral elaborated design IR
├── hdl-sim/      simulation kernel: values, time, scheduler, observer bus
├── hdl-wave/     VCD/HTML/ASCII/SSE wave sinks and viewers
└── hdlsim/       the CLI binary (thin: args + wiring + I/O)
```

Pipeline: `source → tokens → CST → AST → IR → sim events → sinks`.

## Status

Early development. Verilog-2005, a SystemVerilog subset (packages with
`pkg::` scope resolution and const-evaluable package functions, typedefs,
structs/enums, `parameter type`, typed parameters, gen-block localparams,
multi-dim ranges, `inst.port` reads, arrays/queues, classes with
`randomize()` + constraints, generate, `unique case`), and VHDL incl. VHDL-2008
(`process(all)`, matching
case/operators, `to_xstring`, conditional/selected assigns) are supported and
locked in by self-checking examples in `examples/features/` (each prints
`PASS`; run via `cargo test -p hdlsim --test examples`). See `docs/plan.md`
for the roadmap, `docs/worldclass.md` for feature ideas under consideration,
`docs/vhdl-support.md` for the VHDL feature support matrix (with the tests
that pin each row), and `REVIEWING.md` for the code conventions (reviewability
+ extensibility rules) every contribution follows.

## Install

```
cargo install --path crates/hdlsim    # from a checkout
cargo install hdlsim                  # from crates.io (once published)
```

All six workspace crates (`hdl-syntax`, `hdl-lib`, `hdl-ir`, `hdl-sim`,
`hdl-wave`, `hdlsim`) depend only on the vetted common crates from
`[workspace.dependencies]` and are published-ready
(`[workspace.package]` metadata + `repository`/`license` fields), so external
tools can depend on e.g. `hdl-sim` or `hdl-syntax` directly to reuse the
kernel or the parsers.

## Build

```
cargo build --release   # fetches only the vetted workspace crates
cargo test              # unit + integration tests
```

## Usage

```
hdlsim init                                  # scan the project, write hdlsim.ini (indexed resolution)
hdlsim init --progress                       # same, with live scan progress on stderr
hdlsim help <command>                        # detailed help for any command (e.g. hdlsim help init)
hdlsim check file.v ...                      # parse + lower + resolve, report diagnostics
hdlsim ast file.v                            # dump the reusable CST (s-expression)
hdlsim run --top tb_counter counter.v tb.v   # simulate; --vcd out.vcd --max-time 1us --seed 42
hdlsim run --top tb tb.v -L work=rtl         # resolve missing units from libraries
hdlsim run --top tb tb.v -g WIDTH=32         # override a top-level generic (repeatable; widths/decisions see it)
hdlsim run --top tb                           # no files: resolve everything from hdlsim.ini libraries
hdlsim run --top tb tb.v --wave waves.html   # offline interactive wave viewer
hdlsim run --top tb tb.v --hwf waves.hwf     # compact binary wave format
hdlsim run --top tb tb.v --fst waves.fst     # GTKWave FST wave format
hdlsim wave waves.hwf -o waves.html          # render a .hwf offline (no re-run)
hdlsim wave waves.fst -o waves.html          # render a .fst (also GTKWave files)
hdlsim run --top tb tb.v --live              # live wave window in the browser
hdlsim run --top tb tb.v --max-time 50ns --checkpoint ck.bin   # save state
hdlsim run --top tb tb.v --restore ck.bin    # resume from a checkpoint
hdlsim project --top cpu --format vivado-tcl # export Vivado TCL project files
hdlsim bundle --out repro.tar --top tb [files]  # pack the project into one .tar reproducer
hdlsim run --bundle repro.tar --top tb       # reproduce straight from the bundle
hdlsim debug --top tb --dump                 # dump the elaborated design (signals, processes)
hdlsim debug --top tb --watch sig --all      # watch signal changes live during the sim
hdlsim run --top tb --list-signals=rdy ...   # print (filtered) signal names, no sim — find --watch names
hdlsim debug --top tb --mem imem.mem --watch '#12'   # dump memories / watch by id
hdlsim debug --top tb --driver q                     # which process/assign last drove a signal
hdlsim regress --junit results.xml --list tests.txt  # run many testbenches, JUnit XML report
hdlsim lsp                                   # LSP server on stdio (diagnostics, hover, definition, completion)
hdlsim fmt file.v                        # print canonically formatted source
hdlsim fmt --check src/...               # CI gate: exit 1 if any file would change
hdlsim fmt -w file.vhd                   # rewrite in place
hdlsim lint file.v ...                   # lint (exit 1 on any finding); --fix applies safe fixes
hdlsim do run.do --top tb file.v         # mini-Tcl do-file: run/restart/vcd dumpfile/examine/seed/checkpoint/restore/back
hdlsim deps --top tb -L work=rtl tb.v    # list all files the top depends on (transitive)
hdlsim assist "why does tb.v fail"       # local prompt-to-task router (prints $ command; --yes runs it)
hdlsim explain E0301                     # explain a diagnostic/lint code (no arg lists all)
hdlsim gen tb --top counter counter.v    # generate a testbench skeleton from the top's ports
hdlsim analyze counter.v                 # static heuristics: undriven (W0501) / unread (W0502) signals
hdlsim doc counter.v tb.v --out html-docs/  # cross-linked HTML docs (index + page per unit, highlighted source)
hdlsim doctor                              # environment check: stdlib, config, libraries, cache (exit 1 on failure)
hdlsim completions bash > ~/.local/share/bash-completion/completions/hdlsim  # shell completions (bash|zsh|fish)
hdlsim prove --top dut --bmc 20 dut.v      # bounded model checking: check `assert property` within N clock edges
```

`hdlsim prove` runs bounded model checking with the built-in CDCL SAT solver
(`hdl_sim::sat` + `hdl_sim::bmc`): it unrolls the design `N` clock edges
(`--bmc`, default 10), treats the DUT ports as free inputs, and checks every
`assert property` / VHDL `assert`. Exit **0** = no violation within the bound
(SAFE, not a proof), **1** = property violated (a counterexample trace is
printed), **2** = unsupported construct (memories, `#delay`, non-constant
indices, …).

SVA-lite (Verilog): concurrent `assert property (@(posedge clk) a |-> b)` /
`|=>` and immediate `assert (e) [else s];` are simulated — a failed assertion
exits 1. `assume`/`cover` parse with a warning (W0401) but are not simulated.

`hdlsim regress` runs each test as its own simulation (fresh design per test)
and exits 1 if any test fails. The `--list` file holds one test per line:
`name=top [file.v ...]` (`#` starts a comment); plain positional tops also work.
`--coverage` folds each test's line/branch/toggle coverage into the XML
`<system-out>`.

The `debug` command (and the `hdl_sim::debug` module: `Watcher`, `fmt_value`,
`fmt_time`) is also the debugging toolkit for your own tooling built on the
libraries — attach a `Watcher` observer to any `Simulator`.

### Exit codes

`hdlsim run` (and `debug`) exit **0 when all tests pass** and **1 when any test
fails** — a VHDL `assert`/`report` with severity `error`/`failure`, or a Verilog
`$error`/`$fatal` (the kernel emits `SimEvent::TestFailed`; `Simulator::failures()`
exposes the count to library users). Compile errors also exit 1, usage errors
exit 2. So CI and `run_tests.sh` scripts can rely on the exit code alone.

A VHDL `std.env.finish(status)` argument (VHDL-2008) also becomes the exit
code — truncated to the OS range 0..=255, and with test failures still taking
precedence (a failing run always exits 1). Verilog `$finish(n)` is unaffected:
its argument is a diagnostic level, not an exit status.

### Transcript output

`report`/`assert` lines carry the emitting statement's location, an explicit
severity tag, and the instance path of the scope that emitted them:
`file:line: [warning] message (at u_core.u_fifo)`. The tag keeps the level
legible without color; the path matches the names `--list-signals` prints, so
a check instantiated more than once is attributable to the exact instance —
including generate-unrolled copies, which name their lane (`(at lane_gen(0))`,
`(at lane_gen[1].u_lane)`; top-level statements have no path suffix). Colors
stay on by default — even
piped, so agent-consumed transcripts keep the signal; `NO_COLOR` or
`--no-color` disables, `--color auto` colors only on a terminal. A severity
written as a named `severity_level` constant is const-folded
(`severity C_SEV` behaves like the keyword it evaluates to).

## Reporting bugs — reproducer bundles

Whenever hdlsim exits non-zero, it prints a short `note:` hint on stderr with
the bundle command and the issue link, so the report path is always one
copy-paste away. To include in an issue: the full error output (diagnostic
codes like `E0301` can be explained with `hdlsim explain E0301`), the
`hdlsim doctor` output (first line has the version), and a reproducer bundle.

**Privacy first:** issues are public, and so is anything attached or linked
(gist) in them. If your design is confidential, do not attach your project and
do not run `--report` on it — instead reduce the failure to a **minimal
reproducer written as new code based on a bundled example** (`examples/counter`,
`examples/vhdl-counter`, `examples/features`, `examples/fp-vhdl`, ...): copy an
example, modify it until hdlsim fails on it, and bundle only that:

```
cp -r examples/vhdl-counter repro && cd repro   # then modify the copied files
hdlsim bundle --out hdlsim-repro.tar --top tb_counter counter.vhd tb_counter.vhd
```

Anyone can then reproduce from the bundle alone (`hdlsim run --bundle
hdlsim-repro.tar --top tb_counter`), so the issue is fixable without exposing
your code. The issue templates (`.github/ISSUE_TEMPLATE/`) spell out the same
checklist when you open an issue.

If hdlsim fails on your project (compile error, wrong simulation result), pack
it into a single file and attach it to an issue:

```
cd your-project
hdlsim bundle --out repro.tar --top tb_my_test my_tb.vhd ...   # add -L/--config as usual
# repro.tar now contains: your sources, every library dir from hdlsim.ini/-L,
# include dirs, a generated hdlsim.ini, and a REPRO.md with the exact commands
```

Or file the issue directly from the failing command (uses the `gh` CLI's auth —
`gh auth login` or `GITHUB_TOKEN`). Because the bundle with your sources is
uploaded as a gist and linked from a **public** issue, `--report` prints an
upfront warning before running anything; `--report-dry-run` previews exactly
what would be uploaded without uploading:

```
hdlsim check my_tb.vhd --report "wrong result on my design"   # files a GitHub issue on failure
```

Reproduce it (with any hdlsim build) either by extracting:

```
tar -xf repro.tar -C repro && cd repro && hdlsim run --top tb_my_test
```

or directly from the bundle, without extracting:

```
hdlsim run --bundle repro.tar --top tb_my_test
```

`--bundle` works on `run`, `check`, `debug`, and `project`. Issues:
<https://github.com/larsjoost/hdlsim/issues/new>

## Examples

- `examples/counter` — Verilog counter, `examples/vhdl-counter` — the same in VHDL.
- `examples/riscv-vhdl` — a single-cycle RV32I CPU written in VHDL (alu, decoder,
  register file, instruction/data memory) with six self-checking testbenches:
  unit tests for alu/decode/regfile plus whole-program tests (fibonacci, sum of
  1..10, jal/jalr subroutine call) that check results in data memory. Run them all:

  ```
  ./examples/riscv-vhdl/run_tests.sh                 # uses target/debug/hdlsim
  cargo test -p hdlsim --test riscv                  # same thing as a cargo test
  ```

  The cpu is split across multiple libraries (`rv32i` for the RTL, `work` for
  the testbenches) to exercise cross-library dependency resolution; the
  testbenches are run with no file arguments: `hdlsim run --top tb_alu`.
- `examples/npu-vhdl` — an 8x8 matrix-vector neural-network accelerator in
  VHDL, memory-mapped at `0x1000` so the RV32I cpu can drive it: a unit test
  plus a whole-program integration test where the cpu loads weights/inputs,
  starts a matvec, and reads back the results over MMIO. Its `hdlsim.ini`
  pulls the cpu in from `../riscv-vhdl/rtl`:

  ```
  ./examples/npu-vhdl/run_tests.sh                   # tb_npu + tb_cpu_npu
  cargo test -p hdlsim --test npu
  ```
- `examples/fp-vhdl` — hand-written IEEE-754 single-precision `fp_add` /
  `fp_mul` in pure VHDL (multi-cycle FSMs with start/done handshake; NaN/inf/
  zero handling), checked by `tb_fp` against expected bit patterns:

  ```
  ./examples/fp-vhdl/run_tests.sh
  cargo test -p hdlsim --test fp
  ```

`crates/hdlsim/tests/corpus/` holds real-world third-party VHDL/Verilog fetched
from the internet (see `corpus/README.md`), including Xilinx `unisim`-style
code — every file must lex/parse cleanly and compile through the lowerer
(`cargo test -p hdlsim --test corpus`).

`hdlsim.ini` (modelsim.ini style) maps logical libraries to directories:

```ini
[Library]
work = .
mylib = libs/mylib
```

Run `hdlsim init` in a project to generate it: `work = .` indexes the whole
tree, and each directory containing VHDL sources gets a one-line mapping named
after the directory (`processor = hw/hdl/sccore/processor`; if several dirs
share a name, the shallowest path wins so build-tree copies lose). Vendor
library roots are persisted too: `--lib-root DIR` (each subdir of DIR becomes
a library named after it) and Xilinx auto-detection via `HDLSIM_XILINX_LIBS`,
`XILINX_VIVADO`/`XILINX_HLS` (`data/vhdl/src` -> unisim, unimacro, secureip,
...). Without any config, hdlsim already indexes the current directory as
`work`, auto-detects those Xilinx roots, and falls back to searching the
project (plus any `-S DIR` search roots) for unknown units (warning
`W0502`/`W0501` tells you to run `hdlsim init` or add `-L name=dir`).
Resolution is by design-unit name across all libraries, so `library X;`
clauses in sources never need to match the ini names. Library-wide
`use lib.all;` compiles every unit of library `lib`. The config is found by
walking up from the cwd (like git), so running from a project subdirectory
still loads the root `hdlsim.ini`. See `hdlsim help init` and `hdlsim help check`.

## License

MIT
