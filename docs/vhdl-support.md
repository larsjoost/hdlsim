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
| `std.standard` | ✅ | boolean/bit/character/integer/… types; `true`/`false`; `severity_level` literals; char control literals `NUL`…`DEL` (`LF`, `CR`, `HT`); VHDL-2008 `file_open_kind`/`file_open_status` (`OPEN_OK`, `READ_MODE`…, VHDL-2019 `READ_WRITE_MODE`), `delay_length`; VHDL-2019 vector types `integer_vector`/`real_vector`/`time_vector`/`boolean_vector` and range-record direction literals (`ascending`/`descending`); `now` (sim time); `std.env` (`resolution_limit`, tool/version constants, assert API stubs) | `tests/vhdl_std_ulogic.rs`, `tests/vhdl_severity.rs`, `tests/vhdl_osvvmish.rs`, corpus OSVVM |
| `std.env` | ✅ | `finish`/`stop` are simulator builtins; `finish(status)` reports STATUS as the process exit code (`tests/exit_code.rs`); VHDL-2019 time API decls present (`time_record`, `epoch`, `LOCALTIME`/`GMTIME`, `to_string(time_record)`) — decl-only, not simulated | `tests/fixtures/tb_pass.vhd`, `tests/vhdl_std_arith.rs`, corpus `ct19_tb_011.vhd` |
| `std.textio` | ✅ | simulated: `line` buffer, `write`/`read` (string/integer/vector), `readline`/`writeline`, `endfile`; `file f : text open … is "path"` reads/writes real files, `input`=stdin, `output`=stdout; VHDL-2019 file API decls present (`file_open`/`file_close`, `file_state`, `file_mode`) — decl-only | `tests/vhdl_textio.rs`, corpus `ct19_tb_006a.vhd` |
| `ieee.std_logic_1164` | ✅ | 9-value logic as 1-bit scalar, vectors, logic ops, `rising_edge`/`falling_edge`, `to_x01` | `tests/vhdl_std_ulogic.rs`, examples |
| `ieee.numeric_std` | ✅ | `unsigned`/`signed` arithmetic, comparisons, `resize`, `to_integer`, `to_unsigned`/`to_signed` | `tests/vhdl_signed.rs`, `tests/vhdl_std_arith.rs` |
| `ieee.std_logic_arith` | ✅ | package resolves; `conv_*` conversions and shift/rotate builtins simulated | `tests/vhdl_std_arith.rs`, `tests/vhdl_shifts.rs` |
| `ieee.std_logic_signed` / `std_logic_unsigned` | ◐ | same: decls resolve, common ops builtin | `tests/vhdl_std_arith.rs` |
| `ieee.std_context` (VHDL-2008) | ✅ | embedded context declaration; `context ieee.ieee_std_context;` resolves and applies std.standard + ieee.std_logic_1164 + ieee.numeric_std | `tests/vhdl_std2019.rs` |
| `ieee.numeric_std_unsigned` (VHDL-2019) | ✅ | re-exports ieee.numeric_std transitively; `to_slv` builtin (bit-identical std_logic_vector view) | `tests/vhdl_std2019.rs` |
| `ieee.math_real` | ◐ | constants incl. the VHDL-2019 additions (`MATH_SQRT_PI`, `MATH_DEG_TO_RAD`, …) are const-evaluable; `sqrt`/`log`/`log2`/`log10`/`exp`/`sin`/`cos`/`tan`/`arcsin`/`arccos`/`arctan`/`floor`/`ceil` lower to the kernel's real-math builtins | `tests/vhdl_real.rs`, `tests/vhdl_r18.rs`, corpus `ghdls_synth_issue2176_*` |
| `ieee_proposed.standard_additions` | ✅ | decl-only `to_string`/`to_hstring`/`to_ostring`/`to_bstring` family; `to_string(real)` formats decimal | `tests/vhdl_std2019.rs` |
| `ieee.std_logic_textio` | ✅ | its `write`/`read` on `line` share the std.textio runtime (vector args print decimal/hex, see `std.textio`) | `tests/vhdl_textio.rs`, parse via `tests/corpus.rs` |
| `ieee.fixed_pkg` | ✅ | embedded subset: `ufixed`/`sfixed` simulated as f64 `real` (bounds not modeled); `to_ufixed`/`to_sfixed`/`to_real`/`resize` + arithmetic/comparison via real ops | `tests/vhdl_real.rs` |
| `ieee.float_pkg` | ✅ | embedded subset: `float` as f64 `real`; `to_float`/`to_real`/`to_integer`/`resize` | `tests/vhdl_real.rs` |
| `ieee.vital_*` | ❌ | | |

A `use lib.unit.all;` that resolves nowhere now warns (`W0501`) instead of
being silently ignored (`tests/vhdl_std_arith.rs`).

## Design units & context

| Construct | Status | Test |
|---|---|---|
| `entity` / `architecture` (same or separate files, any order) | ✅ | `tests/vhdl_arch_unknown_entity.rs` |
| `package` + `use lib.pkg.all;` visibility (cross-file **and same-file**) | ✅ | `tests/vhdl_use_clause.rs`, `tests/vhdl_genpkg.rs` |
| generic packages: `generic (…)` clause + `package p is new g generic map (…)` (design-unit or declarative part) | ✅ re-lowers the generic package with the map values as overrides; constants/types/functions all see them | `tests/vhdl_genpkg.rs` + parser test `parses_generic_package_declaration` |
| VHDL-2019 type instantiation `type x is new g generic map (…)`; anonymous-type formals `A : type is private`; `U` bit-string base prefix (`6Ux"a"`) | ◐ parsed + lowered to placeholders (instantiated type registers like the generic package; `type` formals register a placeholder type) | parser test `parses_r19_subprog_generic_return_of_seq_block_bitstring`, `tests/vhdl_r19.rs`, corpus `ct08_*`/`ct19_*` |
| parameterless function call without parentheses (`c_hi`) | ✅ | `tests/vhdl_genpkg.rs` |
| `configuration` (top-level `for <arch>`) + `entity lib.unit(arch)` instantiation | ✅ per-arch variant modules `unit/arch`; default = configuration choice else last analyzed (IEEE rule) | `tests/vhdl_arch_select.rs` |
| `generic map` on an instantiation re-binds the child's generics at elaboration | ✅ elab-time variant re-lower (`<entity>#<k=v,...>` modules): port widths and for-generate ranges use the mapped values; constant actuals only | `tests/vhdl_generic_map.rs` |
| nested block configurations (`for … use entity …`) | ◐ parsed, not wired | parser tests |
| `library` / `use` / `context` clauses | ✅ (unresolved `use` → W0501); `use lib.pkg.all;` is transitive through packages (what `pkg` itself used is visible too), as in VHDL. An architecture inherits its entity's context clauses and declarative items (constants/types) even when they live in a separate file (LRM 1076 §13.3/§1.2) | `tests/vhdl_std_arith.rs`, `tests/vhdl_osvvmish.rs`, `tests/vhdl_view.rs`, corpus OSVVM files |
| VHDL-2019 `view` declaration (`view v of t is … end view;`), `view [(]v[)] of t` interface mode in port/generic/subprogram-parameter lists (incl. nested `view v` elements); view names register as type aliases of their subtype, `v'converse` aliases resolve | ✅ parsed + lowered (a view-mode port is a port of the subtype's type) | parser test `parses_vhdl2019_block_comments_views_and_external_names`, `tests/vhdl_view.rs`, corpus `vhdl-osvvm/` |
| VHDL-2019 external names `<< signal|constant|variable ^.path.all : t >>` and the absolute `.path` form (expression primary and alias target) | ◐ parsed + lowered to a placeholder (X), not simulated; an external-name alias registers the alias name so uses resolve | parser test above, `tests/vhdl_view.rs`, `tests/vhdl_r18.rs` |
| VHDL-2019 `/* … */` block comments (non-nesting) | ✅ lexer | parser test above, corpus `vhdl-osvvm/` |
| record alias `alias a : r.f'subtype is r.f;` — `a.field` selects slice the aliased expression (`alias_rec`); process-local aliases are lowered | ✅ | `tests/vhdl_view.rs`, corpus `vhdl-osvvm/` |

## Declarations

| Construct | Status | Test |
|---|---|---|
| constant / signal / variable | ✅ | examples, `tests/vhdl_counter.rs` |
| enum types (incl. as array index) | ✅ | `tests/vhdl_array_enum.rs` |
| integer/natural/positive ranges, `time` | ✅ | examples |
| 1-D arrays (range- or enum-indexed), `(others => x)` aggregates | ✅ | `tests/vhdl_array_enum.rs` |
| multi-dimensional arrays `array (a to b, c to d) of …`, `m(i, j)` | ✅ (flat index, first dim = MSB) | `tests/vhdl_gaps.rs` |
| unconstrained box index `range <>` (multi-dim index lists too) | ✅ dims come from the signal's own constraint; `'length`/`'high`/`'low` use the element count / first-dim left | `tests/vhdl_boxarray.rs` |
| records (flat vector + `r.field` select + named/positional aggregates), comma-separated fields (`a, b : t`), record-typed ports/variables, **arrays of records `arr(i).field`** (read+write), field slices `r.f(a downto b)`, dotted names in sensitivity lists; **constant arrays/records indexed at runtime** (`c(i)`, `c(i).field`, `c.field` — arch/process-level via a hidden signal, function-local via `$const_idx`) | ✅ | `tests/vhdl_record.rs`, `tests/vhdl_arr_rec.rs`, `tests/vhdl_const_recarr.rs`, corpus `neorv32_bus.vhd`, `neorv32_cpu_trace.vhd` |
| subtypes (constrained subtype width is kept at use sites without their own constraint, e.g. `signal w : word_t`), type conversions (`signed(x)`, `std_logic_vector(x)`, …) | ✅ | `tests/vhdl_signed.rs`, `tests/vhdl_const_recarr.rs` |
| functions/procedures with bodies, `return` | ✅ runtime calls incl. package functions via `use` (registered at elab; for-loops in bodies supported); ✅ const-eval in constant initializers; functions with parameter-dependent local widths are skipped with a warning; unconstrained vector/string return types (`return string`) take the width of the returned expressions; array/record signal initializers are applied at simulation start. VHDL-2019 subprogram generic clause (`function f generic (type T) parameter (…) return a'subtype` — `X'subtype` formal/return marks adopt the base formal's width) and `return rv_t of T`; `f generic map (…)(args)` call suffix | `tests/vhdl2008.rs`, `tests/vhdl_constfn.rs`, `tests/vhdl_const_recarr.rs`, `tests/vhdl_r19.rs`, parser test `parses_r19_subprog_generic_return_of_seq_block_bitstring` |
| component declarations + `entity work.x(rtl)` / component instantiation with generic/port map | ✅ | `tests/vhdl_use_clause.rs`, user repro |
| `alias name is target` (whole signal / bit / slice, read+write); attribute decl / file objects | ◐ alias ✅ lowered; attr decl parsed only; `file f : text open … is "path"` ✅ bound to its path for textio | `tests/vhdl_alias.rs`, `tests/vhdl_textio.rs`, parser tests |
| `real` (and fixed/float `ufixed`/`sfixed`/`float`) | ✅ | f64 simulation: literals incl. exponents (`2.5e1`, `1.5E-2`), real physical literals (`2.5 ns`), `+ - * / mod`, comparisons, unary `-`, `integer(r)` (round to nearest, ties away from zero), `to_real`, `real'image`, mixed real/integer arithmetic | `tests/vhdl_real.rs`, `tests/analog.rs` |

## Statements

| Construct | Status | Test |
|---|---|---|
| concurrent: process (sensitivity list / `all`), assigns, conditional (`when/else`), selected (`with/select`) | ✅ (`after` delays in waveforms schedule transactions relative to statement execution; a trailing `else unaffected` keeps the current value) | examples, `tests/loops.rs`, `tests/vhdl_r18.rs` |
| sequential signal assignment with delays `s <= v [after t] [, v2 [after t2] ...] [when c];` | ✅ each element lands at now + its own delay (not cumulative, delay may be a runtime expression) without suspending the process; `when c` false schedules no transaction; `unaffected` elements schedule no transaction | `tests/vhdl_after.rs`, `tests/vhdl_r18.rs` |
| conditional expressions `v := a when c else b;` and conditional signal assignments in processes; for-loop bounds `t'left to t'right`, attribute arithmetic `input'length-1 downto 0`; `open` in port/generic maps | ✅ | parser tests `parses_vhdl2008_constructs`, `parses_for_loop_range_expressions`, corpus `neorv32_cpu_frontend.vhd` |
| VHDL-2008 parsing: `pure`/`impure` functions, `type T` + subprogram generics in generic clauses (type generics — `type T;` and the box form `T : type` — register a placeholder type), protected types + bodies (objects are 64-bit handles allocated by `$new::t` and initialized by the `t::new` ctor; state variables are fields of the object word — bare refs and `obj.field` read via `$getf::t::f@lsb,w`, writes via `$setf`, method calls via `$method::t::m`; body declarative parts lower `type`/`subtype` decls before the state variables that use them; bodies simulate — including across files, since the layout travels with the package (`PackageIr::prot_tys`); unrecognized names inside a body lower to placeholders), shared variables and package-level signals (name→type recorded; record-field/method refs resolve), access types (`type p is access t`, placeholder; `.all` deref lowers), incomplete types (`type T;`), alias signatures `f[T1, T2]` (resolve to the target call), alias-to-type, `.all` access deref (`L.all(i)` -> `L(i)`), case-insensitive identifiers, function-local constants/aliases/file decls referencing parameters, record fields on parameters and locals, package-body scope inheritance + own-decl export, aggregate range choices `(1 to N => x)`, qualified aggregates `t'(a, b)` / `t'(f => v)`, parenthesized targets `(a, b) := f()`, `end record/function <name>`, `??` condition operator, `abs`, `wait on/until/for` in any order, resolution-function subtypes (`subtype s is (resolved_f) t`) | ✅ parsed + simulated, also across files via `use` (`tests/vhdl_protected.rs`, `tests/vhdl_r20.rs`); `??` lowers to identity, `abs` simulated | parser tests `parses_vhdl2008_constructs`, `parses_vhdl2008_wait_and_condition_operator`, `tests/vhdl_osvvmish.rs`, corpus OSVVM files |
| signal `<=` (NBA/delta) vs variable `:=` (blocking) | ✅ | `tests/vhdl_counter.rs` |
| sequential: if/case (incl. VHDL-2008 matching `case? … end case?`), for/while/loop, `exit`/`next`, `null`, `wait`, block statements (`lbl : block … begin … end block;`) | ✅ (`wait until`/`on`/`for` in any order, e.g. `wait on clk until rising_edge(clk)`; `clk'event` idiom; a sequential block lowers to a `Stmt::Block` executed inline) | `tests/loops.rs`, `tests/vhdl2008.rs`, `tests/vhdl_event.rs`, `tests/vhdl_r19.rs`, parser test `parses_vhdl2008_wait_and_condition_operator` |
| `assert … report … severity …` (sequential and concurrent) | ✅ concurrent → always-process on the condition's signals; severity drives exit codes | `tests/exit_code.rs`, `tests/vhdl_gaps.rs` |
| generate (for/if/case, incl. `elsif`/`else`, declarative parts, nesting), blocks | ✅ unrolled/selected at lower time (loop var as constant) | parser tests, `tests/vhdl_generate.rs` |
| port modes `in`/`out`/`inout`/`buffer` (incl. out/inout ports connected to bit slices or slices, bridged through an implicit wire) | ✅ | `tests/vhdl_generate.rs` |
| instantiation `label : entity lib.name(arch)` | ✅ | user repro, corpus |

## Expressions

| Construct | Status | Test |
|---|---|---|
| arithmetic/relational operators, `mod`/`rem`, `**` (saturating), `abs`/`not` | ✅ | `tests/vhdl_counter.rs`, `vhdl_signed.rs`, `tests/vhdl_pow.rs` |
| logical `and/or/xor/…` on std_logic/vectors | ✅ (via 1164 operator functions) | examples |
| concatenation `&`, aggregates (positional, `others =>`, named `f => v`, choice lists `1 | 2 => v`, range choices `1 to N => v`), slices `v(a downto b)`, indexing `v(i)` | ✅ | `tests/vhdl_array_enum.rs`, `tests/vhdl_record.rs`, parser tests |
| attributes | ◐ `'event`, `'last_value`, `'stable` on scalar signals; `'length`/`'high`/`'low`/`'left`/`'right` static — on unconstrained array ports (`port (a : in std_ulogic_vector)`) lowered to runtime `Expr::SigAttr`; `'range` in for-loops; `'image`/`to_string`; `'val`/`'pos` (const-eval + runtime, `'val` takes the prefix type's width); others → X with diagnostic | `tests/vhdl_event.rs`, `tests/vhdl_lastval.rs`, `tests/vhdl_gaps.rs`, `tests/loops.rs`, `tests/vhdl_unconport.rs`, `tests/vhdl_const_attr.rs` |
| builtin function calls (`rising_edge`, `to_integer`, `resize`, `conv_*`, `to_unsigned/signed`, …) | ✅ | `tests/vhdl_std_arith.rs` |
| shifts/rotates `sll`/`srl`/`sla`/`sra`/`rol`/`ror` + function forms `shift_left`/`shift_right`/`rotate_left`/`rotate_right` | ✅ (signed `sra`/`to_integer` decode signed; rotates keep operand width; not in `prove`/BMC) | `tests/vhdl_shifts.rs`, value unit tests |

## Real-world corpus

`crates/hdlsim/tests/corpus/` holds licensed third-party sources (neorv32,
Xilinx UNISIM, serv). Every file must parse cleanly and `check` must never
panic; per-file progress is tracked in `crates/hdlsim/tests/corpus/expected.json`
(`clean` = zero error diagnostics, `diags` = still uses unsupported constructs).
Promote a file's entry in the same PR that fixes it. See `corpus/README.md`.
