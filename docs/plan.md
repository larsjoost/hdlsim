| (direct) | VHDL corpus round 7: VUnit (`vunit/vhdl` + `examples/` + `tests/`) as a new `vhdl-vunit/` corpus group (own library) — **71/71 kept files fully `clean`** (corpus 4191/4191 clean, 0 diags; vhdl 4110). Front-end gaps fixed on the way: type aliases (`alias t is u;` register as types), selected type marks (`ieee.numeric_std.unsigned`), subprogram aliases with selected targets (`alias R is STD.STANDARD.R [..];`), package bodies now see their declaration's context clauses, package instantiations record type actuals (`generic map (type T => u)`), and the embedded stdlib gained `ieee.math_complex` + `ieee.numeric_bit`(+`_unsigned`) from ghdl's ieee2008 sources. Remaining VUnit bulk needs full generic-type-actual propagation through instantiated package bodies | 
| (direct) | VHDL corpus round 6: `gna/` bug-regression repros as `ghdl7_*.vhd` — **512/512 kept files fully `clean`** (corpus 4120/4120 clean, 0 diags; vhdl 4039). Excluded: unit collisions (corpus + `examples/`), one generic-package instantiation shadowing an OSVVM unit, negative/crash repros, non-UTF-8. Robustness fix driven by the crash repros: VHDL + Verilog lexers now consume whole UTF-8 chars on unexpected bytes (token spans stayed mid-char and `span_text` panicked slicing them; e.g. curly quotes in `gna/bug063/dff.vhdl`); regression test added; also removed a duplicated `#[test]` (test-code warning) | 
| (direct) | VHDL corpus round 5: `synth/` from the GHDL testsuite as `ghdl6_*.vhd` — **872/872 kept files fully `clean`** (corpus 3608/3608 clean, 0 diags; vhdl 3527). Excluded: unit collisions (corpus + `examples/`), negative tests (`synth/err01`, err/fail/crash-named), non-UTF-8 (`synth/issue2273`) | 
| (direct) | VHDL corpus round 4: remaining `vests/vhdl-93/` (billowitch/clifton-labs/ashenden leftovers) as `ghdl5_*.vhd` — **49/49 kept files fully `clean`** (corpus 2736/2736 clean, 0 diags; vhdl 2655). Robustness fixes driven by the excluded analyzer-failure negatives: for-generate over a non-discrete or oversized range no longer panics (checked iteration count, warn + skip), based literals with base outside 2..=36 (`0#121#`) no longer panic in `from_str_radix`; regression test added | 
| (direct) | VHDL-2008 features round: unlocks `sanity/004all08/all08.vhdl` (kitchen sink), now in the corpus as `ghdl4_sanity_004all08_all08.vhdl` — **26/26 `ghdl4_*` files fully `clean`** (corpus 2687/2687 clean, 0 diags). Parser/lowerer additions: conditional expressions `v when c else e` (`CondExpr` in expression position; waveforms keep the flat conditional form via an `in_waveform` flag), case-generate alternative labels `when lbl : c =>` with bare `begin ... end lbl;` blocks, concurrent configuration instantiation, alias signatures `f[T, boolean return boolean]`, file types (`type f is file of t`), mid-chain `.all` dereference (`res.all.chain`), block-header generics registered as constants so block port constraints resolve, `'driving`/`'driving_value` as silent X, sized signed bit-strings `32SB".."` in the lowerer, physical length/frequency units (`um`..`km`, `hz`..`ghz`), and E0401 suppression while lowering configuration binding actuals (they name enclosing-architecture signals) | 
| (direct) | VHDL corpus round 3: `billowitch/disputed` + `ashenden/non_compliant` + `sanity/` as `ghdl4_*.vhd` — **25/25 kept files fully `clean`** (corpus 2686/2686 clean, 0 diags; vhdl 2605). Parser fixes: VHDL-2008 `parameter` subprogram keyword, sized bit-strings `32D".."`/`32SB".."`. Excluded: unit collisions, negative tests, VHDL-AMS `ams08`, `all08` (conditional expressions unsupported) | # hdlsim — plan

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
- Phase 2 (done): coverage (line/branch/toggle/FSM + functional), regression runner
  with JUnit XML, LSP (`hdlsim lsp`: diagnostics/hover/definition/completion),
  formatter (`hdlsim fmt`), linter (`hdlsim lint --fix`), driver tracing,
  checkpoint/restore, mini-TCL `do` files, SVA/PSL-lite, `.hwf` viewer, parser
  fuzz harness, `hdlsim doc` HTML docs, `--seed` reproducible $random,
  `hdlsim doctor` / `hdlsim completions`, FST wave format (`--fst`, std-only
  writer + reader, readable by GTKWave and `hdlsim wave`), analog waves v1:
  Verilog `real` / VHDL `real` signals as IEEE-754 doubles in the kernel
  (arithmetic, comparisons, int<->real assignment conversion, `%f`/`%g`),
  rendered as analog traces in the HTML/live viewers and as VCD `r` values
  (`.hwf` v2 carries a real flag; FST carries reals as `FST_VT_VCD_REAL`).
- Phase 2 (remaining): full analog kernel support (continuous `wreal`/
  VHDL-AMS is out of scope).
- Phase 4 (done): SystemVerilog subset and VHDL-2008 on top of phase 1:
  SV packages + `import` + `pkg::` refs, typedefs (plain/enum/packed
  struct), `'{...}` assignment patterns, dynamic arrays/queues/assoc
  arrays (`new[]`, `push_back`/`pop_back`/`size()`), `foreach`, classes
  (`new`/ctor, fields, methods, tasks, `randomize()` with `rand` fields +
  `constraint` blocks solved by the built-in solver), `unique`/`priority`
  case, block-level declarations, 32-bit unsized literals. VHDL-2008:
  `process(all)`, matching `case?`/`?=`/`?<=` operators with `'-'`
  don't-care, `to_hstring`/`to_ostring`/`to_bstring`/`to_string`, delayed +
  conditional (`when/else`) + selected (`with/select`) concurrent
  assignments, architecture-level functions, implicit for-loop variables,
  `exit when`. Every feature is locked in by a self-checking PASS design in
  `examples/features/` (run by `crates/hdlsim/tests/examples.rs`).
- Phase 3 prove notes: `hdl-sim::sat` is a MiniSat-style CDCL solver (two-watched
  literals, first-UIP learning, VSIDS, Luby restarts); `hdl-sim::bmc` encodes the
  elaborated IR 2-valued (X/Z→0, `rising_edge`≡true per step, div-by-0→0) with
  Tseitin gates and checks `assert property`/`assert` as safety properties.
  Memories, `#delay`, non-constant indices and dynamic loop bounds are clean
  errors. UNSAT proofs scale roughly linearly in BMC depth but can hit CDCL
  search cliffs (8-bit accumulator, depth 30 ≈ 20 s release).
- Phase 3 (done): reverse debug (`back [N]` in do-files), constrained-random
  stimulus (`$urandom`/`$urandom_range`/`$rand_solve`), own CDCL + BMC
  (`hdlsim prove --top dut --bmc N`).
- Phase 3 (remaining): SystemVerilog proper, VHDL-2008, UVM-lite.

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

## Progress log (for continuing in a new session)

All work ships as draft PRs from `vibe/<slug>-d3e0f0` branches, merged manually on
GitHub. Gate before every push: `cargo fmt --check` + `cargo test --workspace` green.
`cargo publish` is deferred by the owner until the project is complete (all 6 crate
names - hdl-syntax, hdl-ir, hdl-lib, hdl-sim, hdl-wave, hdlsim - verified unique on
crates.io; publish from the owner's machine with `cargo login` first, in dependency
order: hdl-syntax, hdl-ir, hdl-lib, hdl-sim, hdl-wave, hdlsim).

Release prep 0.2.0 (main @ d80e1e2): 0.1.0 is the only version on crates.io, so the
workspace bumped 0.1.0 -> 0.2.0; version strings in `--help`, bundle manifest JSON,
VCD `$version` and FST writer headers now read `env!("CARGO_PKG_VERSION")`; the golden
VCD test normalizes the version header so bumps don't invalidate the fixture. Gates
green: `cargo fmt --check`, `cargo test --workspace`, `cargo publish --dry-run` (all 6
crates package + verify at 0.2.0, no index conflicts). Actual upload remains
owner-side: `cargo login`, then `cargo publish` from the workspace root.

Merged PRs (main @ c7feea8, 2026-11-16):

| PR | What |
|---|---|
| #1 | workspace publish-ready (unique crate names, metadata) |
| #2 | full Verilog + VHDL syntax in hdl-syntax, unit-tested |
| #3 | on-disk stdlib `.vhd` files + persistent `.hdlsim-cache/` parse cache |
| #4 | line/branch/toggle coverage (`run --coverage`) |
| #5 | `hdlsim regress` with JUnit XML |
| #6-#9, #17 | LSP slices 1-3: diagnostics, hover, definition, completion |
| #7, #8 | `hdlsim fmt` (+`--check`, `-w`), `hdlsim lint --fix` |
| #10 | `hdlsim do` mini-Tcl do-files |
| #11 | milestone 9: perf gate + golden VCD + loop/break fixes |
| #12 | `hdlsim deps` (transitive file dependencies of a top) |
| #13 | `hdlsim assist` (local prompt-to-task router) + `hdlsim explain` |
| #14 | `hdlsim gen tb` testbench skeleton generator |
| #15 | `hdlsim analyze` (W0501 undriven / W0502 unread) |
| #16 | driver tracing (`debug --driver`) + verilog multi-module name-leak fix |
| #18 | checkpoint/restore (`HDLCKPT1`, `run --checkpoint/--restore`) |
| #19 | SVA-lite (`assert property`, `|->`/`|=>`, immediate `assert`) |
| #20 | `.hwf` binary waves + `hdlsim wave` offline viewer |
| #21 | reverse debug (`back [N]` in do-files) |
| #22 | deterministic parser fuzz tests + verilog lexer span-overflow fix |
| #23 | `hdlsim doc` cross-linked HTML docs |
| #24 | `--seed N` reproducible `$random`; mixed-language co-sim tests |
| #25 | `hdlsim doctor` + `hdlsim completions` (bash/zsh/fish) |
| #27 | constrained-random stimulus: `$urandom`/`$urandom_range` + `$rand_solve` |
| #28 | `hdlsim prove` — own CDCL SAT solver + BMC over the elaborated IR |
| #29 | FST wave format: `run --fst out.fst` (std-only stored-deflate writer) + `hdlsim wave out.fst`; full inflate reader for foreign FST files |
| #30 | analog waves v1: `real` signals (Verilog/VHDL) as IEEE-754 doubles in the kernel + analog traces in HTML/live viewers, VCD `r` values, `.hwf` v2 real flag |
| #31 | FST real vars: `FST_VT_VCD_REAL` (geom width 0, 8-byte doubles) in the writer, decoded by `read_fst` — GTKWave analog display |
| (direct) | VHDL-2008 slice: `process(all)`, matching case/operators, `to_xstring`, delayed+conditional+selected assigns, t=0 process start |
| (direct) | SV: packages, typedefs (enum/struct), `'{...}'` patterns, dynamic/queue/assoc arrays, `foreach`; postfix-parse + function-slot fixes |
| (direct) | SV classes: `new`/ctor, fields, methods, tasks, `randomize()` + constraints; class-body/endclass parser fixes |
| (direct) | `examples/features/`: 22 self-checking PASS designs (14 SV, 8 VHDL) wired into `tests/examples.rs`; fixes they exposed (32-bit unsized literals, concat/replicate parse, logical-not/reduction ops, `Stmt::Return`, `#(...)` instancing, generate blocks, block decls, VHDL functions/returns/exit-when/for-var, solver boundary values) |
| (direct) | VHDL corpus growth from the GHDL testsuite: all 755 `gna/` bug-regression files + all 462 `vests/vhdl-93/ashenden/compliant/` files sampled as `vhdl/ghdl2_*.vhd`; **1084/1084 kept files fully `clean`** (corpus total 1284/1284 clean, 0 `diags`). Parser/lowerer fixes driven by it: named-assoc formals that are indexed/selected/operator-symbols (`BitsA(0) => x`, `"<"(a => b)`), sized bit-string literals `32X"0"` in any expression, block headers (`block is port (...); port map (...); begin` — ports become signals via `lower_port_clause` + `names`), `disconnect … : t after x;` specs, file decls `file f : t is "path";`, `end postponed process`, aggregate targets in selected assigns `(a, b) <= … when …`, `guarded reject t inertial` delay-mechanism order, range-constrained array elements `array (…) of integer range 3 downto 0`, selected-name instantiations `lbl: lib.pkg.comp port map (…)`, reduction unary ops (`and x`, `or x`, …), port-map formals normalized to their base name (`i_data(0 to 1) => aa`), `assoc_is_named` lookahead shared by port/generic maps and call args. Excluded: unit-name collisions with existing corpus units (OSVVM/Bitvis package copies, duplicate `ent`/`controller`/… entities), files needing unavailable external libs (`bv_utilities`, `ieee_proposed`, VITAL), and negative tests | 
| (direct) | VHDL corpus growth round 2: `vests/vhdl-93/billowitch/compliant` + `clifton-labs` + `synth` from the GHDL testsuite as `vhdl/ghdl3_*.vhd`; **1338/1338 kept files fully `clean`** (corpus total 2622/2622 clean, 0 `diags`). Excluded: unit-name collisions, unavailable external libs, negative tests, non-UTF-8 `synth/issue2273` files, VHDL-AMS physical-unit tests | 
| (direct) | multi-file compile perf pass: `Design.packages`/`configurations` and `Lower.pkgs`/`cfgs`/`consts` now hold `Rc<_>` (were full clones per file / per expression — O(files × packages) and O(exprs × consts)); `did_you_mean` prunes candidates by length before running bounded Levenshtein; `hdlsim check` no longer copies all package function bodies into every per-top elaboration (`elaborate_with_opts(.., false)`). Full 2541-file VHDL corpus `check`: debug 393s → 19s, release 410s → 15s | 
| (direct) | ieee_proposed / fixed-float stdlib subset: embedded `ieee.fixed_float_types` (vendored Apache-2.0 from ghdl's libraries), decl-only `ieee.fixed_generic_pkg` + `ieee.float_generic_pkg`, an `ieee_proposed` built-in library alias, `side`/`width` in std.textio, `unresolved_ufixed`/`u_sfixed` in fixed_pkg, `float32/64`, `to_stdulogicvector`; package-as-generic-actual resolves (inert placeholder) and package actuals in generic maps are pulled into context resolution; package-level shared-variable reads lower to placeholders; saturating physical-literal multiply (debug overflow crash fix). Unlocked 39 more GHDL-testsuite corpus files (gna issue1924/2097/…, synth fixed-point examples, ticket89 additions) — corpus 2661/2661 clean, 0 diags | 
| (direct) | VHDL attribute sweep: `x'range`/`x'reverse_range` in for-specs now resolve the loop bounds statically (signal dims via `sig_id`/`sig_dims`, type marks via `types`); `'length/'high/'low/'left/'right` fall back to `type_attr` for integer/natural/boolean/enum/array type marks; `'quiet`/`'stable` = NOT(rising OR falling), `'active`/`'transaction`/`'event` = rising OR falling, `'ascending` = 1, `'last_value` = `Expr::Prev`; `'path_name`/`'instance_name`/`'last_event`/`'image`/`'subtype`/`'simple_name`/\u2026 lower to silent X; user-declared attributes (`attribute a : T;`, incl. via `use`d packages and in entity/architecture/subprogram declarative parts) lower to X without warning. Corpus warnings: 936 â 544 total, W0401 971 â 441, attribute-not-supported 198 â 0; 0 errors, corpus 2661/2661 clean | 
| (direct) | `hdlsim init` rework: `--progress` prints scan progress on stderr; the walk is paths-only (new `walk_source_files[_with]` in hdl-lib, no per-file parsing) and file contents are no longer read at all — each VHDL source directory maps to a logical library named after its last path component (`processor = hw/hdl/sccore/processor`), one line per name (shallowest dir wins, so build-tree copies under `vivado/...` lose to the real source dir), since `library X;` clauses are often stale; root stays `work = .`. Corpus dir (2580 files) init: ~0.9s | 
| (direct) | library search options + in-app docs: new common flags `--lib-root DIR` (each subdir of DIR becomes a library named after it) and `-S/--search-dir DIR` (on-demand project search roots); Xilinx auto-detect via HDLSIM_XILINX_LIBS / XILINX_VIVADO / XILINX_HLS `data/vhdl/src` (unisim, unimacro, secureip, ...) applied by build_manager and persisted by `hdlsim init` as absolute ini lines; new `hdlsim help <command>` prints detailed long-form docs for every command (all 22 covered, incl. init/check library-resolution rules); README updated | 
| (direct) | library-wide `use lib.all;` support (compiles every unit of library `lib`; `work` skipped to keep compile order stable) via new `context_libraries` in hdl-ir + `has_library`/`library_unit_names` in hdl-lib; default hdlsim.ini lookup now walks up parent dirs (like git), so running from a project subdir still loads the root config | Remaining, in suggested order: UVM-lite / broader SV coverage (interfaces, clocking blocks, covergroups), synthesis-oriented subsets. (VHDL configurations done: configuration declarations incl. block configs / `for all` / `for others` / generic+port maps in bindings, configuration specifications `for label : comp use ...;`, `configuration work.cfg` instantiations, `--top <configuration>`. Corpus clean: 209/209 real-world files parse+lower with zero error diagnostics — fixes driven by it include `glbl.GSR` implicit globals (Verilog + VHDL `GSR`), SV type casts `T'(expr)`, cross-file package typedefs via `VerilogScope.types` (struct field selects like `mstatus_q.mprv`), const-eval `$clog2` + div-by-zero→0 in parameter defaults, VHDL access-type implicit `.all` on field selects, nested record field selects (`rv.param.distribution`), `arr(i).field` on function params/locals, nested function declarations, 3-part selected names `lib.pkg.item`, package instantiations in the unit index + their generic targets in context resolution, bare physical unit literals (`ns`), and std.env VHDL-2019 `FILE_NAME`/`FILE_PATH`/`FILE_LINE`/`GET_CALL_PATH` + `call_path_vector(_ptr)` plus an `ieee.math_real` stdlib subset.)
| (direct) | lazy library indexing + `--progress` on source commands: unit lookup is staged (file-stem index without parsing -> in-memory built-ins -> full parse scan on miss -> project search); index keys lowercased; duplicate-name candidates all compiled (last package wins, as before); `--progress` (common flag) narrates indexing/resolution on stderr. Single-file check with a 20k-file unisim lib: 8.5s -> 2.2s; corpus W0502s 543 -> 507 (stem index resolves them directly), 0 errors | 