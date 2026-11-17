# Real-world source corpus

Third-party VHDL/Verilog sources fetched from the internet, used as parser and
front-end test fixtures (`../corpus.rs`). Each file is used unmodified; licenses
are in `licenses/` and each file header retains its upstream copyright notice.

Fetched 2026-11 from raw.githubusercontent.com:

| File | Project | License |
|---|---|---|
| `verilog/serv_alu.v`, `verilog/serv_decode.v` | [olofk/serv](https://github.com/olofk/serv) — bit-serial RISC-V CPU (Verilog) | ISC |
| `verilog/serv_*.v` (remaining `rtl/` units: aligner, bufreg, bufreg2, compdec, csr, ctrl, debug, immdec, mem_if, rf_if, rf_ram, rf_ram_if, rf_top, state, synth_wrapper, top) | same project — completes the serv `rtl/` set | ISC |
| `verilog/cv32e40p_*.sv` (27 core `rtl/` units + `rtl/include/cv32e40p_{pkg,apu_core_pkg,fpu_pkg}.sv`; `cv32e40p_fp_wrapper.sv` excluded — needs the fpnew vendor tree) | [openhwgroup/cv32e40p](https://github.com/openhwgroup/cv32e40p) — RISC-V core, SystemVerilog (module-header `import pkg::*;`, bare enum decls, multi-dim port ranges, `inside` sets, `>>>`, `+:` part-selects, ternaries in indices, `{n{concat}}` replication, intra-assignment `#delay`) | SHL-0.51 |
| `verilog/xilinx_unisim_{BUFG,IBUF,ODDR}.v` | [Xilinx/XilinxUnisimLibrary](https://github.com/Xilinx/XilinxUnisimLibrary) — official Xilinx UNISIM primitives (Verilog) | Apache-2.0 |
| `verilog/xilinx_unisim_{BUFGCTRL,CARRY4,DSP48E1,FDCE,FDRE,ISERDESE2,OSERDESE2,OBUF,RAM32M,RAM64M,LUT4,LUT6}.v` | same library — more primitives (sensitivity lists with part-selects, bare task calls, `#delay` in procedural assignments) | Apache-2.0 |
| `verilog/cc_{pkg,addr_decode,binary_to_gray,counter,delta_counter,edge_detect,onehot,onehot_to_bin,popcount,rr_arb_tree,shift_register,spill_register,stream_fifo,stream_mux,stream_xbar,sync_wedge,rstgen,cdc_2phase,fifo}.sv` | [pulp-platform/common_cells](https://github.com/pulp-platform/common_cells) — widely used SystemVerilog library; **all 20 files clean** (typed parameters, `parameter type` incl. struct defaults, `pkg::fn(...)` const-eval in param defaults, gen-block localparams, multi-dim ranges with `**`, struct patterns with field labels, `inst.port` reads, for-loop inline decls, gen-for compound steps, implicit generate, `(* attrs *)`, size casts, macro invocations) | Apache-2.0 (Solderpad 2.0) |
| `vhdl/neorv32_cpu_alu.vhd`, `vhdl/neorv32_imem.vhd`, `vhdl/neorv32_package.vhd`, `vhdl/neorv32_imem_image.vhd` | [stnolting/neorv32](https://github.com/stnolting/neorv32) — RISC-V SoC in VHDL (the two `neorv32_*` files are the entities' package dependencies, so cross-file `use neorv32.…all;` resolves) | BSD-3-Clause |
| `vhdl/neorv32_bus.vhd`, `vhdl/neorv32_cache.vhd`, `vhdl/neorv32_clint.vhd`, `vhdl/neorv32_cpu_control.vhd`, `vhdl/neorv32_cpu_pmp.vhd`, `vhdl/neorv32_cpu_regfile.vhd`, `vhdl/neorv32_dmem.vhd`, `vhdl/neorv32_gpio.vhd` | same project — more `rtl/core` units (arrays of records, record ports, generics without defaults, multi-entity files) | BSD-3-Clause |
| `vhdl/neorv32_bootrom.vhd`, `vhdl/neorv32_bootrom_image.vhd`, `vhdl/neorv32_cache_ram.vhd`, `vhdl/neorv32_cfs.vhd`, `vhdl/neorv32_core_complex.vhd`, `vhdl/neorv32_cpu_alu_bitmanip.vhd`, `vhdl/neorv32_cpu_alu_cfu.vhd`, `vhdl/neorv32_cpu_alu_cond.vhd`, `vhdl/neorv32_cpu_alu_crypto.vhd`, `vhdl/neorv32_cpu_alu_fpu.vhd`, `vhdl/neorv32_cpu_counters.vhd`, `vhdl/neorv32_cpu_hwtrig.vhd`, `vhdl/neorv32_cpu_trace.vhd`, `vhdl/neorv32_debug_auth.vhd`, `vhdl/neorv32_debug_dm.vhd`, `vhdl/neorv32_debug_dtm.vhd`, `vhdl/neorv32_dma.vhd`, `vhdl/neorv32_gptmr.vhd`, `vhdl/neorv32_neoled.vhd`, `vhdl/neorv32_prim.vhd`, `vhdl/neorv32_sdi.vhd`, `vhdl/neorv32_slink.vhd`, `vhdl/neorv32_smc.vhd`, `vhdl/neorv32_spi.vhd`, `vhdl/neorv32_sys.vhd`, `vhdl/neorv32_sysinfo.vhd`, `vhdl/neorv32_top.vhd`, `vhdl/neorv32_tracebuf.vhd`, `vhdl/neorv32_twd.vhd`, `vhdl/neorv32_twi.vhd`, `vhdl/neorv32_uart.vhd`, `vhdl/neorv32_xbus.vhd` | same project — all remaining `rtl/core` units (FPU uses `ieee.float_pkg`/`fixed_pkg` types; `neorv32_cpu_trace` clean (const array-of-record field access supported)) | BSD-3-Clause |
| `vhdl/unisim_{BUFGCTRL,CARRY4,DSP48E1,FDCE,FDRE,LUT4,LUT6,MMCME2_BASE,OBUF,RAM32M}.vhd` | [deuskane/xilinx-primitive-unisim](https://github.com/deuskane/xilinx-primitive-unisim) — more UNISIM VHDL component models | see upstream |
| `vhdl/ghdl_*.vhd` (9 files: `comp01/and3,and6,and6comp,and6conf`, `issue1139/ent,ent1`, `issue3171/rotate_rl_conf,top_level_conf`, `vests/.../ch_13_fg_13_01`) | [ghdl/ghdl](https://github.com/ghdl/ghdl) testsuite — configuration declarations, block configurations, configuration specifications, `configuration work.cfg` instantiations; **all 9 fully `clean`** | GPL-2.0 |
| `vhdl/unisim_{BUFG,IBUF,ODDR,FD}.vhd` | [deuskane/xilinx-primitive-unisim](https://github.com/deuskane/xilinx-primitive-unisim) — mirror of the Xilinx UNISIM VHDL component models (Copyright Xilinx, Inc.; used here as parse fixtures) | see upstream |
| `vhdl/IfElsePkg.vhd`, `vhdl/RandomPkg.vhd`, `vhdl/AlertLogPkg.vhd`, `vhdl/ScoreboardGenericPkg.vhd` | [OSVVM/OSVVM](https://github.com/OSVVM/OSVVM) — VHDL verification library (VHDL-2008: generic packages, protected types, textio-heavy) | Apache-2.0 |
| `vhdl/{ClockResetPkg,CoveragePkg,CoveragePtPkg,DelayCoveragePkg,DynamicVectorGenericPkg,DynamicVectorPkg_instances,FileLinePathPkg,IdFifoPtPkg,MemoryGenericPkg,MemoryPkg,MemoryPkgIs01,MemorySupportPkg,MessageListPkg,MessagePkg,OsvvmContext,OsvvmScriptSettingsPkg,OsvvmScriptSettingsPkg_default,OsvvmSettingsPkg,OsvvmSettingsPkg_default,RandomPkg2019,RandomProcedurePkg,ReportPkg,ResizePkg,ScoreboardPkg_IntV,ScoreboardPkg_int,ScoreboardPkg_signed,ScoreboardPkg_slv,ScoreboardPkg_unsigned,SetGetBoundedGenericPkg,SetGetBoundedPkg_Instances,TranscriptPkg}.vhd` | same project — the remaining root packages (VHDL-2008/2019: context declarations, `attribute foreign of f[sig]: ...`, VHDL-2019 `generic (type V is array (type is range <>) of type is private)`, `subtype E is V'element`, multi-element waveforms with `after`, prefix reduction `or x`; package instantiations `package ScoreboardPkg_slv is new work.ScoreboardGenericPkg generic map (...)`; `OsvvmSettingsPkg_template.vhd` excluded — it is a `${...}` substitution template, not valid VHDL; `CoverageVendorApiPkg_NVC.vhd` excluded — needs the NVC simulator's proprietary `nvc.cover_pkg` (type `t_scope_handle`)) | Apache-2.0 |
| `vhdl/{AlertLogPkg,AssertApiPkg,FileUtilPkg,LanguageSupport2019Pkg,NamePkg,NameStorePkg,OsvvmGlobalPkg,OsvvmScriptSettingsPkg,OsvvmSettingsPkg,OsvvmTypesPkg,RandomBasePkg,ResolutionPkg,SortListPkg_int,TbUtilPkg,TextUtilPkg,TranscriptBasePkg}.vhd` | same project — the `use work.…` dependency closure of the seeds; **all 16 fully `clean`** (VHDL-2008/2019: std.standard vector types `integer_vector` etc., `file_open_kind`/`status`, char literals `LF`/`CR`, `now`, generic package `type` params, incomplete + protected + access types, alias signatures `f[T1, T2]`, `.all` deref, case-insensitive identifiers, package-const export, package-level signals / shared variables / protected-type methods resolve at lower time — protected method *bodies* and shared-variable mutation are not simulated) | MIT |

The tests assert: every file lexes and parses without errors (the CST is
lossless), and a full `check` (parse + lower + resolve) runs to completion
without panicking, rendering any diagnostics rustc-style. Each language
directory is compiled as one library (the directory is registered under the
logical `library` names its files declare, e.g. `-L neorv32=<dir>`), so
cross-file design units resolve on demand — the same way a user runs
`hdlsim check -L neorv32=rtl rtl/*.vhd`. Files that use language features
outside hdlsim's current subset are expected to produce *diagnostics* (that
is what the check is for) — never a crash.

## expected.json — per-file progress tracking

`expected.json` records each file's current status: `clean` (compiles+lowers
with zero *error* diagnostics) or `diags` (parses clean, still hits unsupported
constructs). `corpus_files_meet_their_recorded_status` asserts every file
reaches at least its recorded status, so a fix only moves files up: promote the
entry in `expected.json` in the same PR that fixes the file (regression below
the recorded status fails CI). New corpus files must be added to `expected.json`
explicitly — regenerate with `HDLSIM_UPDATE_CORPUS_EXPECTED=1 cargo test -p hdlsim
--test corpus`.
