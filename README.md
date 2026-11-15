# hdlsim

[![CI](https://github.com/larsjoost/hdlsim/actions/workflows/ci.yml/badge.svg)](https://github.com/larsjoost/hdlsim/actions/workflows/ci.yml)

A self-contained VHDL + Verilog simulator written in std-only Rust — no external
crates, and no wrapping of GHDL, Icarus Verilog, Verilator, or any other existing
simulator. Lexers, parsers, library manager, elaboration, simulation kernel, and
the graphical wave display are all implemented here.

## Highlights

- **Reusable syntax trees.** `hdl-syntax` exposes a lossless CST and typed AST for
  Verilog and VHDL with source spans and parse recovery, so other applications
  (linters, formatters, LSP, doc generators) can build on the same parser:
  `hdl_syntax::parse(Language::Verilog | Language::Vhdl, &source)`.
- **Automatic dependency search across libraries.** Named libraries mapped to
  directories (`hdlsim.ini` — created by `hdlsim init` — plus `-L name=dir` flags);
  undefined modules and `` `include `` files are found and compiled on demand in
  dependency order, with an in-session compile cache. `std` and `ieee` are built in
  as real VHDL source files shipped in the repo (`crates/hdl-lib/stdlib/`, not
  compiled into the binary) and parsed by the same front-end; they are found via
  `$HDLSIM_STDLIB`, an installed `share/hdlsim/stdlib` next to the executable, or
  the source tree, and are searched last so `-L ieee=...` overrides them.
- **Persistent parse cache.** The CLI caches each file's parse tree under
  `.hdlsim-cache/` (own binary format, keyed by source-text hash + format
  version), so repeated runs skip lexing/parsing unchanged files. Control with
  `HDLSIM_NO_CACHE=1` or `HDLSIM_CACHE_DIR=path`. Within one run, each file is
  compiled at most once regardless.
- **Graphical display.** `hdlsim run tb --live` opens a real-time wave window in the
  browser (built-in HTTP server + SSE, pause/step controls); `--wave out.html`
  writes a self-contained interactive viewer; `--vcd` for interchange; ASCII waves
  for logs.
- **rustc-style diagnostics.** Every parse/lower error is rendered like rustc —
  `error[E..]: message` with the source line, a caret span, and notes.
- **Project export for other tools.** `hdlsim project --format vivado-tcl
  --format json manifest` generates Vivado TCL project files (and a JSON manifest)
  from the same library/compile machinery, through the public `hdlsim::project`
  API so external tools can reuse it.
- **One kernel for both languages.** Both front-ends lower to a shared IR, so
  mixed VHDL/Verilog designs simulate natively in one wave window.
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

Early development. See `docs/plan.md` for the roadmap, `docs/worldclass.md` for
feature ideas under consideration, and `REVIEWING.md` for the code conventions
(reviewability + extensibility rules) every contribution follows.

## Install

```
cargo install --path crates/hdlsim    # from a checkout
cargo install hdlsim                  # from crates.io (once published)
```

All six workspace crates (`hdl-syntax`, `hdl-lib`, `hdl-ir`, `hdl-sim`,
`hdl-wave`, `hdlsim`) are std-only with zero dependencies and published-ready
(`[workspace.package]` metadata + `repository`/`license` fields), so external
tools can depend on e.g. `hdl-sim` or `hdl-syntax` directly to reuse the
kernel or the parsers.

## Build

```
cargo build --release   # std-only, no dependencies to fetch
cargo test              # unit + integration tests
```

## Usage

```
hdlsim init                                  # create a starter hdlsim.ini
hdlsim check file.v ...                      # parse + lower + resolve, report diagnostics
hdlsim ast file.v                            # dump the reusable CST (s-expression)
hdlsim run --top tb_counter counter.v tb.v   # simulate; --vcd out.vcd --max-time 1us
hdlsim run --top tb tb.v -L work=rtl         # resolve missing units from libraries
hdlsim run --top tb                           # no files: resolve everything from hdlsim.ini libraries
hdlsim run --top tb tb.v --wave waves.html   # offline interactive wave viewer
hdlsim run --top tb tb.v --live              # live wave window in the browser
hdlsim run --top tb tb.v --max-time 50ns --checkpoint ck.bin   # save state
hdlsim run --top tb tb.v --restore ck.bin    # resume from a checkpoint
hdlsim project --top cpu --format vivado-tcl # export Vivado TCL project files
hdlsim bundle --out repro.tar --top tb [files]  # pack the project into one .tar reproducer
hdlsim run --bundle repro.tar --top tb       # reproduce straight from the bundle
hdlsim debug --top tb --dump                 # dump the elaborated design (signals, processes)
hdlsim debug --top tb --watch sig --all      # watch signal changes live during the sim
hdlsim debug --top tb --mem imem.mem --watch '#12'   # dump memories / watch by id
hdlsim debug --top tb --driver q                     # which process/assign last drove a signal
hdlsim regress --junit results.xml --list tests.txt  # run many testbenches, JUnit XML report
hdlsim lsp                                   # LSP server on stdio (diagnostics, hover, definition, completion)
hdlsim fmt file.v                        # print canonically formatted source
hdlsim fmt --check src/...               # CI gate: exit 1 if any file would change
hdlsim fmt -w file.vhd                   # rewrite in place
hdlsim lint file.v ...                   # lint (exit 1 on any finding); --fix applies safe fixes
hdlsim do run.do --top tb file.v         # mini-Tcl do-file: run/restart/vcd dumpfile/examine
hdlsim deps --top tb -L work=rtl tb.v    # list all files the top depends on (transitive)
hdlsim assist "why does tb.v fail"       # local prompt-to-task router (prints $ command; --yes runs it)
hdlsim explain E0301                     # explain a diagnostic/lint code (no arg lists all)
hdlsim gen tb --top counter counter.v    # generate a testbench skeleton from the top's ports
hdlsim analyze counter.v                 # static heuristics: undriven (W0501) / unread (W0502) signals
```

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

## Reporting bugs — reproducer bundles

If hdlsim fails on your project (compile error, wrong simulation result), pack
it into a single file and attach it to an issue:

```
cd your-project
hdlsim bundle --out repro.tar --top tb_my_test my_tb.vhd ...   # add -L/--config as usual
# repro.tar now contains: your sources, every library dir from hdlsim.ini/-L,
# include dirs, a generated hdlsim.ini, and a REPRO.md with the exact commands
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

## License

MIT
