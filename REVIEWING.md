# REVIEWING.md — conventions every change follows

## Reviewability
- One concept per file; every file starts with a `//!` header: purpose, invariants,
  pipeline position.
- Plain-data AST/IR; behavior in free functions. No hidden global state: context
  structs (`ParseCtx`, `ResolveCtx`, `LowerCtx`) are passed explicitly.
- Errors are values (`Result<T, Diagnostic>`), never panics on user input. Panics
  only for violated internal invariants, with the invariant named. No
  `unwrap()`/`expect()` on anything derived from source files.
- No `unsafe`. No build.rs, no proc macros. External crates are limited to the vetted
  common set declared in `[workspace.dependencies]` (thiserror, anyhow, clap, serde,
  serde_json, log); a crate opts in with `x.workspace = true`. Prefer std when it is
  a reasonable fit; anything outside the vetted set needs a justification in the PR
  description.
- One grammar production / one idea per function, typically < 60 lines. Parsers
  mirror the IEEE 1364 / 1076 grammar so reviewers can diff against the standard.
- No magic numbers; enums instead of boolean flags at call sites.
- `///` on every public item; inline comments only for invariants and "why".
- Deterministic output (sorted iteration where order isn't semantic) so golden
  diffs are meaningful.
- Tests colocated in `#[cfg(test)] mod tests`, named for the behavior they pin;
  golden files in `tests/golden/`.
- `cargo fmt --check` and `cargo test` must pass before a milestone is done.
- One logical change per commit; milestones land as reviewable slices.

## Extensibility (closed core, open edges)
- Core data structures (AST, IR, `SimEvent`) are closed enums: the compiler forces
  exhaustive handling. Do not weaken this with catch-all arms that hide new variants.
- New functionality attaches via trait + registry at pipeline edges (see
  `docs/extensibility.md`): languages, unit providers, lowering, observers, system
  tasks, wave sinks, CLI commands, lint rules. Adding a feature must not require
  editing existing logic; if it does, flag it in review.
- Every seam has a conformance test with a toy implementation.

## Per-crate checklist
- hdl-syntax: lexer never panics on arbitrary bytes; parser always returns a tree
  (possibly with error nodes) + diagnostics; spans are correct (tested).
- hdl-lib: resolution order is work → configured libs → libdirs → libfiles;
  errors list everything searched; cache round-trips.
- hdl-ir: lowering is deterministic; name resolution uses only the ResolveCtx.
- hdl-sim: delta-cycle semantics per language; no allocation in the event loop;
  observers cannot corrupt kernel state (events are immutable snapshots).
- hdl-wave: sinks handle arbitrary value widths; HTML viewer is self-contained.
- hdlsim: CLI does I/O only; all logic delegated to library crates.
