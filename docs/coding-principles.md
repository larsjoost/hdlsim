# Coding principles

> **Status:** recommendations, not hard rules. Follow them as closely as makes
> sense; deviate deliberately and mark the deviation at the site with a
> one-line comment, e.g. `// >5 lines deliberately: single pass over the sample
> buffer`. The harness rules in `ai-harness/rules/` remain the binding
> baseline.
>
> Origin: the *Five Lines of Code* (Christian Clausen) rule set, adapted for
> this Rust codebase, plus reviewability principles agreed 2026-10-20 and
> Rust ownership/layout rules agreed 2026-11-26.

## Core

- **Small, single-purpose functions** — ~5-line body target; a reviewer should
  understand a function in one read without scrolling. If the name needs a
  qualifier to be honest, the function does two things.
- **Limit iterator pipelines** — 2–3 simple steps; when a chain needs nested
  closures or complex `fold`/`filter_map` logic, use a `for` loop or a named
  helper.
- **Names state the contract** — the signature conveys intent; the body only
  confirms the name.
- **Make illegal states unrepresentable** — newtypes, enums, `Result`/`Option`
  over sentinel values; the compiler does part of the review.
- **Explicit over implicit** — no hidden side effects, no magic numbers (name
  the constant), no mutation hiding behind `get_*` names.
- **No lazy cloning** — do not add `.clone()`, `Rc`, or `Arc<Mutex<…>>` to
  satisfy the borrow checker; borrow (`&T`, `&[T]`) or split the struct so
  ownership is clear.
- **Fail fast, fail closed at boundaries** — validate peer-controlled input
  (HDL source text, `hdlsim.ini`, CLI arguments) once at the edge; inner code
  assumes validity. Release is `panic = "abort"`, so unvalidated input is a
  release blocker.
- **No new panics** — never introduce `unwrap()`/`expect()` outside tests,
  and never on peer-controlled input. Propagate with `?` and `let … else`
  into the existing error enums (`thiserror` in the library, `anyhow` in
  bins). `unwrap` is acceptable only where the invariant is statically
  guaranteed, with the reason stated at the site.
- **No bare arithmetic on peer-controlled values** — `checked_*`,
  `saturating_*`, `wrapping_*` spell out the overflow intent; debug builds
  panic on overflow, release wraps silently.
- **Data close to behavior** — behavior lives on its type; all of a type's
  behavior is reviewable in one place.
- **Thin impl surface, pure logic core** — keep structs small and cohesive, with
  invariant-enforcing behavior and trait impls directly on the type so its state
  guarantees are reviewable in one place. Multi-step workflows and data
  transformations live in pure standalone functions taking `&T`/`&[T]`; the
  `impl` block acts as a thin API surface that may delegate to them. Small
  cohesive operations on a type's own data may simply be methods.
- **Change by addition** — a feature adds a type or module; it does not edit
  fragile core logic (OCP).
- **One concept per diff** — one behavior change or one refactor per change;
  no drive-by cleanups. Low coupling is what keeps the effective diff small.
- **Dead code is a review tax** — delete, don't comment out.
- **Testable ⇒ reviewable** — pure functions, injected dependencies,
  deterministic streams (`derive_seed`).
- **Consistent file order for new files** — imports, public types,
  orchestration functions, private helpers, `#[cfg(test)]` tests. Apply when
  creating a file; do not reshuffle existing files to match.

## Apply as-is (fit Rust)

| Rule | Notes |
|------|-------|
| Guard clauses first | `if` only at the top; early `return` / `?`, no nested branches |
| Pure conditions | No I/O or mutation inside `if`/`while` expressions |
| Either call or pass | An orchestration function coordinates calls *or* prepares data, not both |
| No trait with one implementation | Exception: test seams (production impl + test mock) |
| Avoid premature generality | Pairs with "no unjustified new dependencies" |
| Love deleting code | No exceptions |
| Collaborate with the compiler | Small compile-green steps; before reporting done run the repo gates in `AGENTS.md` (`cargo smoke`, both clippy gates, `cargo test`) |

## Adapted (Java-flavored originals → Rust reading)

| Original | Rust reading |
|----------|--------------|
| Never use `else` | Ban *nested branching*, not the token — `if` is an expression; `let x = if c { a } else { b }` is idiomatic |
| Never use `switch` | Ban *grown-up procedural dispatch*, not `match` — exhaustive compiler-checked arms are fine; a 15-arm `match` mutating a struct is a refactor signal |
| Only inherit from interfaces | Traits at genuine swap/extension points, composition over coupling; never create a trait to satisfy the rule |
| No getters/setters | `pub(crate)` fields are the convention; "tell, don't ask" where behavior belongs on the type |

## Deliberate deviations

| Rule | Deviation | Why |
|------|-----------|-----|
| 5-line limit | Dense numeric/table code in `crates/hdl-ir/src/value.rs` (real-value folding, bit-state letter tables) may exceed it | A single pass over a value's bits is one computation; splitting it is harder to review. Test: is the length essential to one computation, or does the function do two things? |
| Zero inline comments | Doc comments at type/module level are allowed | Some knowledge structurally cannot be a name (see comment ladder) |

### Comment ladder

An inline comment is a refactor signal — migrate it up:

1. Explains a block → extract a function; the name carries the intent.
2. Explains a value → named constant.
3. Explains a type's role or physics → doc comment on the type.
4. Explains a rejected alternative or trade-off → doc comment at the nearest
   boundary, commit message, or design doc.

The only surviving comments are facts a name cannot carry — *why a scheme
exists* (physics) and *external contract facts* (e.g. wire-protocol sizes) —
and they live in type/module doc comments, never inline.

Extraction guard: don't extract a function just to kill a comment if the
resulting name is weaker than the comment. Test: would a reviewer get more
from the name than from the comment?

## Review questions

- Does each function fit one read, and does its name state the contract?
- Does this feature add a module/type, or edit fragile core logic?
- Is peer-controlled input validated at the boundary before use?
- Is new logic a pure free function, with the impl block as thin surface?
- Are there new `unwrap()`/`expect()` calls, or bare arithmetic on
  peer-controlled values?
- Is there an inline comment that should be a name, a constant, or a doc comment?
- Is every dead line deleted and every deviation marked?
