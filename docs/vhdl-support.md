# VHDL support matrix

What hdlsim's VHDL front-end (parse → lower → elaborate → simulate) supports
today, and the test that pins each row. Status:

- ✅ supported and tested
- ◐ partial (parses; some forms unsupported or not simulated)
- ❌ not supported

Every PR that adds or fixes a construct must add/promote a row here **and** a
test (parser unit test, `crates/hdlsim/tests/vhdl_*.rs` fixture, or a
`tests/corpus/` file promoted in `expected.json`).

## Standard libraries (embedded in the binary, see `crates/hdl-lib/stdlib/`)

| Package | Status | Notes | Test |
|---|---|---|---|
| `std.standard` | ✅ | boolean/bit/character/integer/… types; `true`/`false` | `tests/vhdl_std_ulogic.rs`, corpus |
| `std.env` | ✅ | `finish`/`stop` are simulator builtins | `tests/fixtures/tb_pass.vhd`, `tests/vhdl_std_arith.rs` |
| `ieee.std_logic_1164` | ✅ | 9-value logic as 1-bit scalar, vectors, logic ops, `rising_edge`/`falling_edge`, `to_x01` | `tests/vhdl_std_ulogic.rs`, examples |
| `ieee.numeric_std` | ✅ | `unsigned`/`signed` arithmetic, comparisons, `resize`, `to_integer`, `to_unsigned`/`to_signed` | `tests/vhdl_signed.rs`, `tests/vhdl_std_arith.rs` |
| `ieee.std_logic_arith` | ✅ | package resolves; `conv_*` conversions and shift/rotate builtins simulated | `tests/vhdl_std_arith.rs`, `tests/vhdl_shifts.rs` |
| `ieee.std_logic_signed` / `std_logic_unsigned` | ◐ | same: decls resolve, common ops builtin | `tests/vhdl_std_arith.rs` |
| `ieee.std_logic_textio` | ◐ | declarations only — no text I/O simulated | parse via `tests/corpus.rs` |
| `ieee.fixed_pkg`, `float_pkg`, `vital_*` | ❌ | | |

A `use lib.unit.all;` that resolves nowhere now warns (`W0501`) instead of
being silently ignored (`tests/vhdl_std_arith.rs`).

## Design units & context

| Construct | Status | Test |
|---|---|---|
| `entity` / `architecture` (same or separate files, any order) | ✅ | `tests/vhdl_arch_unknown_entity.rs` |
| `package` + cross-file `use lib.pkg.all;` visibility | ✅ | `tests/vhdl_use_clause.rs` |
| `package … is new …` (VHDL-2008 instantiation) | ◐ parsed | parser tests |
| `configuration` (top-level `for <arch>`) + `entity lib.unit(arch)` instantiation | ✅ per-arch variant modules `unit/arch`; default = configuration choice else last analyzed (IEEE rule) | `tests/vhdl_arch_select.rs` |
| nested block configurations (`for … use entity …`) | ◐ parsed, not wired | parser tests |
| `library` / `use` / `context` clauses | ✅ (unresolved `use` → W0501) | `tests/vhdl_std_arith.rs` |

## Declarations

| Construct | Status | Test |
|---|---|---|
| constant / signal / variable | ✅ | examples, `tests/vhdl_counter.rs` |
| enum types (incl. as array index) | ✅ | `tests/vhdl_array_enum.rs` |
| integer/natural/positive ranges, `time` | ✅ | examples |
| 1-D arrays (range- or enum-indexed), `(others => x)` aggregates | ✅ | `tests/vhdl_array_enum.rs` |
| multi-dimensional arrays `array (a to b, c to d) of …`, `m(i, j)` | ✅ (flat index, first dim = MSB) | `tests/vhdl_gaps.rs` |
| unconstrained box index `range <>` | ◐ (depth 1) | — |
| records (flat vector + `r.field` select + named/positional aggregates) | ✅ | `tests/vhdl_record.rs` |
| subtypes, type conversions (`signed(x)`, `std_logic_vector(x)`, …) | ✅ | `tests/vhdl_signed.rs` |
| functions/procedures with bodies, `return` | ◐ | `tests/vhdl2008.rs` |
| component declarations + `entity work.x(rtl)` / component instantiation with generic/port map | ✅ | `tests/vhdl_use_clause.rs`, user repro |
| alias / attribute decl / file objects | ◐ parsed | parser tests |
| `real` | ❌ (literals → X) | lowerer warning |

## Statements

| Construct | Status | Test |
|---|---|---|
| concurrent: process (sensitivity list / `all`), assigns, conditional (`when/else`), selected (`with/select`) | ✅ (no `after` delays) | examples, `tests/loops.rs` |
| signal `<=` (NBA/delta) vs variable `:=` (blocking) | ✅ | `tests/vhdl_counter.rs` |
| sequential: if/case/for/while/loop, `exit`/`next`, `null`, `wait` | ✅ (`wait until`/`on`/`for`; `clk'event` idiom) | `tests/loops.rs`, `tests/vhdl2008.rs`, `tests/vhdl_event.rs` |
| `assert … report … severity …` (sequential and concurrent) | ✅ concurrent → always-process on the condition's signals; severity drives exit codes | `tests/exit_code.rs`, `tests/vhdl_gaps.rs` |
| generate (for/if/case, incl. `elsif`/`else`, declarative parts, nesting), blocks | ✅ unrolled/selected at lower time (loop var as constant) | parser tests, `tests/vhdl_generate.rs` |
| port modes `in`/`out`/`inout`/`buffer` (incl. out/inout ports connected to bit slices or slices, bridged through an implicit wire) | ✅ | `tests/vhdl_generate.rs` |
| instantiation `label : entity lib.name(arch)` | ✅ | user repro, corpus |

## Expressions

| Construct | Status | Test |
|---|---|---|
| arithmetic/relational operators, `mod`/`rem`, `abs`/`not` | ✅ | `tests/vhdl_counter.rs`, `vhdl_signed.rs` |
| logical `and/or/xor/…` on std_logic/vectors | ✅ (via 1164 operator functions) | examples |
| concatenation `&`, aggregates (positional, `others =>`, named `f => v`, choice lists `1 | 2 => v`), slices `v(a downto b)`, indexing `v(i)` | ✅ | `tests/vhdl_array_enum.rs`, `tests/vhdl_record.rs`, parser tests |
| attributes | ◐ `'event`, `'last_value`, `'stable` on scalar signals; `'length`/`'high`/`'low`/`'left`/`'right` static; `'range` in for-loops; `'image`/`to_string`; others → X with diagnostic | `tests/vhdl_event.rs`, `tests/vhdl_lastval.rs`, `tests/vhdl_gaps.rs`, `tests/loops.rs` |
| builtin function calls (`rising_edge`, `to_integer`, `resize`, `conv_*`, `to_unsigned/signed`, …) | ✅ | `tests/vhdl_std_arith.rs` |
| shifts/rotates `sll`/`srl`/`sla`/`sra`/`rol`/`ror` + function forms `shift_left`/`shift_right`/`rotate_left`/`rotate_right` | ✅ (signed `sra`/`to_integer` decode signed; rotates keep operand width; not in `prove`/BMC) | `tests/vhdl_shifts.rs`, value unit tests |

## Real-world corpus

`crates/hdlsim/tests/corpus/` holds licensed third-party sources (neorv32,
Xilinx UNISIM, serv). Every file must parse cleanly and `check` must never
panic; per-file progress is tracked in `crates/hdlsim/tests/corpus/expected.json`
(`clean` = zero error diagnostics, `diags` = still uses unsupported constructs).
Promote a file's entry in the same PR that fixes it. See `corpus/README.md`.
