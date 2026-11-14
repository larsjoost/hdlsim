# Extensibility architecture — "closed core, open edges"

Principle: adding new functionality must NOT require editing existing logic.
New code = new module + one registration line. If a feature needs changes inside
the kernel/parser core, that is a design smell flagged in review.

Core data structures (AST, IR, events) are closed enums/structs: the compiler
forces every consumer to handle new variants explicitly (reviewable exhaustiveness).
Everything else extends through traits + registries at the pipeline edges.

## The pipeline as explicit data stages
```
source text → tokens → CST → AST → IR (elaborated) → simulation events → sinks
```
Each stage is a pure function over plain data. Extensions attach at stage boundaries.

## Extension seams (trait + registry each)
1. `hdl-syntax`: `trait LanguageFrontend { lex(); parse(); }` registered in a
   `LANGUAGES` table. Adding a language/dialect (e.g. SystemVerilog) = new module
   implementing the trait + one line in the table. Dialect differences are a
   `DialectFlags` config struct, not forked parsers.
2. `hdl-lib`: `trait UnitProvider` (filesystem library, built-in std/ieee, cache,
   later: zip/git providers) — libraries are just providers in a list; resolution
   code never changes when a new provider appears.
3. `hdl-ir`: `trait Lowering` per language: AST + ResolveCtx → IR. IR itself closed.
4. `hdl-sim` kernel: closed `SimEvent` enum broadcast to `Vec<Box<dyn Observer>>`.
   Observers = wave sinks, coverage collectors, debugger hooks, live server.
   New analysis feature = new Observer, zero kernel edits.
   System tasks: name → fn registry; `$mytask` works by registering a name.
   Kernel exposes ops (force/release, checkpoint) used by observers.
5. `hdl-wave`: `trait WaveSink` registry: vcd, html, ascii, live-sse, hwf.
   New wave format = new module + register.
6. `hdlsim` CLI: `trait Command { name(); run(); }` registry; `hdlsim <cmd>` dispatch.
7. Lint: `trait LintRule { code(); check(ast, db) -> Vec<Diagnostic> }` registry,
   enabled/disabled via `hdlsim.toml` `[lint]`; `--fix` support flag on rules.
8. Diagnostics: codes registered in one central table; new checks add codes without
   touching existing ones.

## Cookbook
`docs/extending/` will hold one page per seam: minimal example + checklist + test:
add-a-language, add-a-system-task, add-a-wave-format, add-a-lint-rule,
add-a-coverage-metric, add-a-cli-command, add-a-library-provider.

Each seam also has a conformance test using a toy implementation (a `ToySink` that
counts events, a `ToyTask`), proving the seam works without touching core code.

## Deliberately excluded
- No dynamic loading / .so plugins (would need unsafe or external crates).
  Extensions are compile-time: add a module + registration, rebuild.
- No VPI/PLI C ABI (breaks the zero-dependency rule).
- No macro-heavy codegen; extensions are ordinary readable code.
