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
  directories (`hdlsim.toml`, global config, `-L name=dir` flags); undefined modules,
  VHDL entities/packages, and `` `include `` files are found and compiled on demand
  in dependency order, with an incremental cache. `std` and `ieee` are built in.
- **Graphical display.** `hdlsim run tb --live` opens a real-time wave window in the
  browser (built-in HTTP server + SSE, pause/step controls); `--wave out.html`
  writes a self-contained interactive viewer; `--vcd` for interchange; ASCII waves
  for logs.
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

## Build

```
cargo build --release   # std-only, no dependencies to fetch
cargo test              # unit + golden tests
```

## License

MIT
