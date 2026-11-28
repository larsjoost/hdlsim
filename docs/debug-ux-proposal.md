# Debug-UX proposal — cutting time-to-root-cause for the AI agent loop

hdlsim's primary user is an AI agent running the edit → simulate → diagnose
loop. This doc ranks what would cut that loop's time-to-root-cause the most.
It is a proposal only; the next agent (or follow-up PRs) implements the
winners. Precedent: two past wrong-output bugs — a vendor library silently
shadowing a design unit (#197) and `boolean'image` printing `-1` for `true`
(#198) — each cost a real debugging session, which is why "output that lies"
items rank aggressively here.

## Method

Everything below was produced by *using* the shipped CLI (v0.2.0) rather than
reading code:

1. **Inventory** of the documented debug surface (`AGENTS.md` "The debug
   loop", `help <command>` for `run`/`debug`/`regress`), each affordance
   exercised at least once — see Appendix A.
2. **Three failure classes, end to end, with timing**: compile error, failed
   assertion, wrong-value debugging. Minimal fixtures were re-derived with
   invented names (`dxp_*`); every transcript excerpt in this doc is from
   those invented fixtures or is synthesized as a sketch. No design material
   is quoted.
3. **Real-scale cross-check** against the design's own testbench logs: a
   19-testbench sweep and a multi-round debugging session (~10 simulator
   invocations to root-cause one failure). Those logs show the same frictions
   at scale (elaborate 5.4–6.3 s cold per round-trip, 200 KB watch logs,
   10 MB IR dumps used for diffing) but are not excerpted here.

Size scale: **S** ≈ one small focused PR (≤ ~100 lines), **M** ≈ one
cohesive change (~100–400 lines), **L** = cross-cutting, multi-PR.

## What already serves the loop well (keep — do not regress)

- Exit codes are honest today (0/1/2, 124, 130) and `hdlsim: FAILED — N test
  failure(s) (first at file:line)` names the first failure.
- Assertion/report transcript lines carry `file:line:` prefixes.
- `--watch`/`--when`/`--examine`/`--trace-assign` output is genuinely
  agent-shaped: `watch t=5ns d2 dut.shift 0b0000->0b1000 driver=rtl.vhd:7`
  has time, delta cycle, transition, and driver location on one line.
- A typo'd `--when`/`--watch` name fails loudly (`no signal matched shfit`,
  exit 2) instead of arming zero rules silently.
- `analyze` catches never-driven signals with an actionable note (W0501).
- `check --error-format json` gives one JSON object per compile diagnostic
  with span offsets; `regress --junit` gives CI a verdict per testbench.
- `--checkpoint`/`--restore` works: a restore skips compilation entirely
  (observed `compile 0.00s (0 modules)`), so the post-mortem seam exists.
- `doctor` verifies the install in one command.

With that baseline working, the highest-cost problems are below.

---

## Ranked proposal

### 1. Verdict honesty in `run` mode — say INCOMPLETE when the testbench never finished `[QUICK WIN] [S]`

**Friction — wrong output (the most expensive kind).** `AGENTS.md` tells
agents to *always* bound runs with `--max-time`. A run truncated by that
bound — with the testbench's assertions still unexecuted — prints a clean
tail and exits 0:

```
$ hdlsim run --top tb_dxp_deser ... --max-time 10ns
hdlsim: simulate 0.00s (t=10ns) peak-rss 12 MB
hdlsim: simulation stopped at t=10ns (5 signals, 3 processes)
exit=0
```

The testbench asserts at t=76ns; that check never ran, yet the loop ends
"green". The only hint is the timestamp, if the agent knows the expected end
time. `debug` mode prints a stop reason (`stopped: no more events`) — `run`
mode does not. An agent that bounds too tightly (or hits a stimulus hang)
concludes the design is verified.

**Proposal.** The scheduler knows why it stopped and whether events remain.
Print the reason and a verdict, symmetric with the existing FAILED line:

```
hdlsim: simulation stopped at t=10ns (max-time reached; 2 process(es) pending)
hdlsim: INCOMPLETE — 0 failure(s), but the testbench did not finish
```

and `hdlsim: PASSED — 0 failure(s) (N assertion(s) evaluated)` on a real
completion, so success is greppable rather than inferred from absence.
Decision point: keep exit 0 for INCOMPLETE (compat) or introduce exit 3
(recommended, documented next to the exit-code table) — either way the line
itself closes the loop-trap.

**Risk.** Exit-code change affects scripts; keep it behind the documented
table and one release note. The stop reason and pending-process count are
already computed by the kernel (`debug` prints one of them today).

### 2. Print the "file an issue" note only for internal errors `[QUICK WIN] [S]`

**Friction — wrong output.** The note

```
note: hdlsim 0.2.0 — to report this, attach a reproducer and open an issue:
        hdlsim bundle --out hdlsim-repro.tar --top <top> [files...]
      https://github.com/larsjoost/hdlsim/issues/new
        or rerun with `--report "what went wrong"` to file the issue automatically
        (--report uploads the bundle with your sources publicly — see README)
```

fires on every non-zero exit path: a one-character user typo (`check`), a
plain testbench assertion failure (`run`), `analyze` findings, and
`regress` failures. Every occurrence tells the agent that the *tool* broke
and that it should bundle the design's sources and upload them **publicly**.
An agent following instructions can waste a round-trip building a reproducer
for a design bug — or leak private sources via `--report`.

**Proposal.** Emit the note only when hdlsim itself is at fault: internal
guard stops, panics, scheduler dumps. For ordinary diagnostics end with the
existing one-line `hdlsim check: N error(s)` / `hdlsim: FAILED — ...`
summary. The `--report` workflow stays available via its own flag and help
text.

**Risk.** None beyond the changelog. This is the cheapest trust repair in
the list.

### 3. One time unit everywhere; `--examine` must refuse times it never reached `[QUICK WIN] [S]`

**Friction — wrong output, unit trap.** The same transcript mixes formats:
assertion lines print raw femtoseconds while watch/examine print scaled
units.

```
tb_dxp_deser.vhd:41: [error] assertion failed at t=76000000: word mismatch: got 13
watch t=5ns d2 dut.shift 0b0000->0b1000 driver=rtl.vhd:7
```

The trap fires on the obvious next step — copy the failure time into
`--examine`:

```
$ hdlsim run ... --examine 'word@76000000'
examine t=76ms word=0b1101 (end-of-run value; t=76ms not reached)
```

`76000000` is silently parsed as **76 ms**, and because that time was never
reached, `--examine` falls back to the *end-of-run value* — a plausible but
potentially wrong answer, flagged only in a parenthetical. The correct form
(`--examine 'word@76ns'`) works, but the agent must first convert units by
hand.

**Proposal.**
- Scale times in assertion/report transcript lines exactly as `watch` does
  (`t=76ns`), or print both (`t=76ns (76000000fs)`).
- `--examine sig@T` where T > final time is an error (exit 2): `t=76ms not
  reached; run stopped at t=76ns` — never a silent value substitution.
- Optional `--time-format raw` for agents that want unscaled integers
  consistently (then everywhere, including assertion lines).

**Risk.** Transcript-format change; agents parsing raw integers need the
escape hatch above. Small, self-contained change in the diagnostic printer.

### 4. Sequential procedure calls are silently not executed (correctness) `[M]`

**Friction — wrong output, found mid-audit.** A VHDL procedure call statement
in a process does nothing. Three variants verified:

```
-- process-local procedure with an argument:
warning: unsupported system task $send_bit at t=0        (call no-ops)
-- architecture-level, no argument (no message at all):
report "after poke, seen = " & integer'image(seen);      → after poke, seen = 0
-- architecture-level, with argument:
report "after poke(2), seen = " & integer'image(seen);   → after poke(2), seen = 0
```

Stimulus driven through procedures — the most common structured-testbench
idiom — never reaches the DUT, and the run ends "green". The zero-argument
case is completely silent; the argument case misdiagnoses the call as a
Verilog system task, pointing the agent at the wrong language. This cost a
real detour during the audit itself (a tb whose inputs never toggled, "exit
0, 0 failures", while `word` stayed X).

**Proposal.**
- Kernel: execute sequential procedure call statements (with and without
  arguments, process-local and architecture-level). If out of scope for this
  simulator version, the *minimum* is to fail loudly at compile time —
  "procedure call statements are not executed by this simulator; inline the
  body" — never silence.
- Reword the current warning: it is a VHDL procedure call, not an
  "unsupported system task"; name the construct and the fix.
- `analyze`/`lint` interim rule (ships regardless): flag procedure call
  statements as not-executed, severity error.

**Risk.** Kernel semantics work with a full regression pass (`cargo test`,
corpus). The analyze rule is S and can land first as a guardrail.

### 5. Assertion with metavalue operands silently passes (correctness) `[M]`

**Friction — wrong output.** With `word = "XXXX"`:

```
assert word = "1011" report "word mismatch" severity error;
-- no failure, no message; run continues and exits 0
assert false report "control assert fires" severity error;
-- this one fails normally
```

X-propagating stimulus therefore produces *passing* tests: the loop ends
green with a broken DUT. Predefined array equality over `std_logic` treats
`X ≠ '1'` as false, so the assertion must fire (any stock simulator does).

**Proposal.** Make the comparison LRM-conformant (metavalue element ⇒ not
equal ⇒ assertion fires). Complementary UX: when an assertion fires and an
operand contains metavalues, append a one-line hint naming the first
metavalue signal — connecting this fix to the `--watch-x` workflow.

**Risk.** Testbenches that (accidentally) relied on lax X equality will flip
to failing — that is the point; changelog note + corpus run. Kernel change
isolated to the equality/compare lowering.

### 6. Root-cause-first error cascades for entity/architecture mismatch `[M]`

**Friction — missing diagnosis.** When an architecture binds to an entity
whose port list is stale or incomplete, hdlsim reports one `unknown
identifier` error *per use site* and never names the real cause. Minimal
reproduction (entity declares only `clk, dv`; architecture uses `din`,
`word`):

```
error[E0401]: unknown identifier 'din'
   --> dxp_deser2_rtl.vhd:11:38
error[E0401]: unknown identifier 'word'
   --> dxp_deser2_rtl.vhd:15:3
```

A real testbench hit by this shape produced **23 cascading errors** before
the first line pointed anywhere near the entity. The agent pays for every
use site instead of one diagnosis.

**Proposal.** When an unresolved identifier inside an architecture matches
no local declaration and the bound entity is known, emit one error with the
root cause and collapse the rest to notes:

```
error[E04xx]: architecture 'rtl' of 'dxp_deser2' references 'din' and 'word',
              which the bound entity 'dxp_deser2' does not declare
  --> dxp_deser2_rtl.vhd:11:38
  note: entity dxp_deser2 (dxp_deser_entity.vhd) declares: clk, dv
  note: 4 more use site(s): dxp_deser2_rtl.vhd:15:3, ...
  help: the entity file may be stale or the wrong one is on the command line
```

Missing-entity (orphan architecture) is already diagnosed well — keep that.

**Risk.** Dedup can hide genuinely distinct causes; mitigate by keeping the
total count in the summary and listing all use sites as notes. Analysis-pass
change only, no kernel work.

### 7. Machine-readable run transcript (`--transcript-format json`) `[M]`

**Friction — missing feature.** `--error-format json` covers *compile*
diagnostics only: a failing run emits **zero** JSON objects. Run-time
verdicts (assertion failures, the FAILED summary, the proposed PASSED/
INCOMPLETE lines from item 1) exist only as ANSI-colored prose — and color
is on by default even when piped (`--color auto|always|never`, default
`always`), so agents must strip escapes to parse. `regress --junit` covers
batch CI, not the single-run diagnose loop.

**Proposal.** `--transcript-format json` (global flag like
`--error-format`): one JSON object per transcript event, plus a final
verdict object.

```json
{"kind":"failure","file":"tb_dxp_deser.vhd","line":41,"time_fs":76000000,
 "message":"word mismatch: got 13","hierarchy":"tb_dxp_deser.stim"}
{"kind":"verdict","status":"failed","failures":1,"first":"tb_dxp_deser.vhd:41"}
```

Fold the `watch`/`when`/`examine`/`trace-assign` events into the same switch
so the whole diagnose output is machine-readable in one flag. While there:
default `--color` to `auto` (pipes get clean text; terminals keep color).

**Risk.** Two output modes to keep in sync — mitigate with a shared event
type rendered by two small writers (matches the existing observer/edge
seams in `docs/extensibility.md`). Color-default change is cosmetic but
worth a release note.

### 8. `--watch-x`: show what fed the X `[M]`

**Friction — missing feature.** `--watch-x` advertises "the fastest path to
where did the first X come from", and it does print the arrival with the
driver location:

```
watch t=10ns d2 y 0b1->"x" driver=dxp_xsrc.vhd:8
```

But for a driver that is an expression (mux, adder), the line doesn't say
*which input* was X. The agent needs a second round-trip watching the
driver's inputs. `--trace-assign` already solves exactly this with its
`reads{...}` annotation.

**Proposal.** Reuse it: on `--watch`/`--watch-x` lines whose driver is an
expression, append the read set (capped):

```
watch t=10ns d2 y 0b1->"x" driver=dxp_xsrc.vhd:8 reads{b=x}
```

**Risk.** Line width on wide buses; cap the number of printed reads and
gate behind `--watch-reads` if needed. The assignment-evaluation machinery
already computes the values.

### 9. Post-mortem workflow: `--stop-on-error` + checkpoint at the failure `[M]`

**Friction — round-trip economics.** Diagnosing one failure class took ~10
simulator invocations in the real session (list-signals → watch → trace-assign
→ debug → reruns), each re-paying compile + elaborate (5.4–6.3 s cold,
~1.9 s warm, 150 MB RSS on the real design), plus 10 MB IR dumps used for
manual diffing. `run` simulates *through* failures (a real log shows 85
failures from one root cause); `debug` stops at the first but cannot
checkpoint; `--checkpoint`/`--restore` saves only at the stop point of a
normal run and restore already skips recompilation.

**Proposal.** One flag composes the pieces that exist:

```
hdlsim run --top tb --stop-on-error --checkpoint fail.ckpt ...
# stops at the first failed assertion, exit 1, state frozen at t=failure
hdlsim run --top tb --restore fail.ckpt --watch sig --examine 'sig@T'
# no recompile, no re-simulation up to the failure; inspect from the frozen state
```

Optionally `--checkpoint-at T` for freezing at a chosen timestamp. The
2-round-trip post-mortem (fail+freeze, then inspect) replaces N full re-runs.

**Risk.** Resuming mid-stimulus exercises checkpoint correctness beyond
end-of-run saves — bound the risk by validating restore determinism in tests
(restore + short run must equal the uninterrupted run's transcript).

### 10. Deduplicate repeated warnings by (code, canonical path, span) `[QUICK WIN] [S]`

**Friction — noise.** A real sweep log printed the *same* warning five times
with four different path spellings of one file (absolute with `./`, absolute
without, `./relative`, relative):

```
warning[W0401]: constant initializer is not constant; treated as X
   --> <abs>/./pkg/unit.vhd:16:3      (×2 spellings)
   --> <abs>/pkg/unit.vhd:16:3
   --> ./pkg/unit.vhd:16:3
   --> pkg/unit.vhd:16:3
warning: 5 warnings generated
```

Same-file-twice on one command line already compiles once, so the repeats
come from repeated analysis through different library mappings — the agent
sees five problems where there is one.

**Proposal.** Key emitted diagnostics by (code, canonicalized path, span);
emit once and append `note: repeated 4× (same location via different path
mappings)`. Canonicalize for display too (one spelling per file).

**Risk.** None; pure printer-side dedup with a counter.

### 11. `--list-signals` pattern semantics: accept globs, say what it is `[QUICK WIN] [S]`

**Friction — discoverability.** `--list-signals=PATTERN` is documented as a
substring filter, but every agent's first instinct is a glob — which fails
invisibly:

```
$ hdlsim run ... --list-signals='*'
hdlsim: 0 signal(s) matching '*'
```

**Proposal.** Treat patterns containing `*`/`?` as globs (substring
otherwise). Zero-match message teaches the rule:
`no signal matches '*' (patterns with * or ? are globs; plain text is a substring)`.

**Risk.** None.

### 12. Raw OS errors leak with reused diagnostic codes `[S]`

**Friction — wrong output (minor).** Passing a stray word after a boolean
flag surfaces an OS error with no context, and the code collides with an
unrelated warning family:

```
$ hdlsim run ... --watch-x y        # y meant to be the watched signal
error[E0501]: cannot read y: No such file or directory (os error 2)
```

W0501 is "signal is never driven"; E0501 is a file read failure. An agent
keying on codes (as `--error-format json` encourages) gets a wrong model.

**Proposal.** For source-file reads, print `cannot read source file 'y'
(os error 2: no such file or directory); check the file argument list` with a
fresh code. Renumber or namespace the E/W codes so a number implies one
meaning across severities.

**Risk.** Code renumbering touches tests that pin codes; mechanical.

---

## Suggested order

| # | Item | Size | Quick win | Depends on |
|---|------|------|-----------|------------|
| 2 | Issue-note only on internal errors | S | yes | — |
| 3 | Time units + strict `--examine` | S | yes | — |
| 10 | Warning dedup | S | yes | — |
| 11 | `--list-signals` globs | S | yes | — |
| 1 | INCOMPLETE/PASSED verdict lines | S | yes | pairs with 7 |
| 12 | Diagnostic-code hygiene | S | — | — |
| 4 | Procedure calls: fix or fail loudly | M | — | interim lint rule first |
| 5 | Metavalue assert conformance | M | — | — |
| 6 | Root-cause-first cascades | M | — | — |
| 7 | JSON transcript + color=auto | M | — | pairs with 1 |
| 8 | `--watch-x` reads | M | — | — |
| 9 | `--stop-on-error` + checkpoint | M | — | — |

The six quick wins are one-PR-each and remove actively misleading output
(items 2, 3) or silent-green traps (item 1) — highest trust-per-line ratio.
Items 4–5 are correctness: they decide whether the loop's "green" means
anything. Items 6–9 then shorten the diagnose phase itself.

## Appendix A — inventory of the documented debug surface, as observed

| Affordance | Status | Observation |
|---|---|---|
| `syntax` (parse-only) | works | ms-scale; caught nothing it shouldn't |
| `check` + `--error-format json` | works | good spans, codes, `help` field (null in practice) |
| bounded `run --max-time/--max-wall` | friction | truncation is silent-green (item 1) |
| `file:line:` failure prefixes + FAILED summary | works | time units inconsistent (item 3) |
| issue-note on failure | friction | fires on every failure path (item 2) |
| `--list-signals[=PATTERN]` | friction | substring, not glob (item 11); hierarchies correct |
| `--watch` / `--watch-x` / `--watch-deltas` | works | format is good; X contributors missing (item 8) |
| `--when` / `--examine` | friction | typo check good; unit trap + silent fallback (item 3) |
| `--trace-assign` | works | `reads{...}` is the pattern item 8 reuses |
| `debug --dump / --driver / --mem` | works | duplicate of watch flags; no `--checkpoint` (item 9) |
| `--checkpoint` / `--restore` | works | restore skips recompile; stop-point only (item 9) |
| `--trace*` scheduler traces | not exercised | reserved for kernel-level bugs |
| waves (`--vcd/--hwf/--fst/--wave/--live`) | not exercised | out of scope for this audit |
| `--coverage` | not exercised | out of scope |
| `regress --junit` | works | tops-from-list clear; per-test stdout absent |
| `bundle` / `--report` / `assist` / `doctor` | works | but advertised from every failure (item 2) |
| `analyze` | works | W0501 undriven detection is excellent |

## Appendix B — measured round-trip economics

- Toy design (4 files): full `run` ≈ 0.02 s; each diagnose flag re-runs
  everything (no incremental state), which is why items 8/9 matter even here.
- Real design (from its own logs): compile ~0.05–0.13 s, **elaborate
  5.4–6.3 s cold / ~1.9 s warm cache**, simulate 0.5–39 s depending on tb.
- A real diagnose session visible in the logs: ~10 invocations (list-signals,
  watch over ~30 signals → ~1000 lines, trace-assign, two `debug` stops,
  several full re-runs) — minutes of pure re-elaboration, plus two 10 MB IR
  dumps produced for manual diffing.
- `--restore` after `--checkpoint` skips compilation (`compile 0.00s
  (0 modules)`) — the post-mortem seam exists (item 9); it is just not
  reachable at the failure point today.
