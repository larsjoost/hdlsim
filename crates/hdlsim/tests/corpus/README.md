# Real-world source corpus

Third-party VHDL/Verilog sources fetched from the internet, used as parser and
front-end test fixtures (`../corpus.rs`). Each file is used unmodified; licenses
are in `licenses/` and each file header retains its upstream copyright notice.

Fetched 2026-11 from raw.githubusercontent.com:

| File | Project | License |
|---|---|---|
| `verilog/serv_alu.v`, `verilog/serv_decode.v` | [olofk/serv](https://github.com/olofk/serv) — bit-serial RISC-V CPU (Verilog) | ISC |
| `verilog/xilinx_unisim_{BUFG,IBUF,ODDR}.v` | [Xilinx/XilinxUnisimLibrary](https://github.com/Xilinx/XilinxUnisimLibrary) — official Xilinx UNISIM primitives (Verilog) | Apache-2.0 |
| `vhdl/neorv32_cpu_alu.vhd`, `vhdl/neorv32_imem.vhd`, `vhdl/neorv32_package.vhd`, `vhdl/neorv32_imem_image.vhd` | [stnolting/neorv32](https://github.com/stnolting/neorv32) — RISC-V SoC in VHDL (the two `neorv32_*` files are the entities' package dependencies, so cross-file `use neorv32.…all;` resolves) | BSD-3-Clause |
| `vhdl/neorv32_bus.vhd`, `vhdl/neorv32_cache.vhd`, `vhdl/neorv32_clint.vhd`, `vhdl/neorv32_cpu_control.vhd`, `vhdl/neorv32_cpu_pmp.vhd`, `vhdl/neorv32_cpu_regfile.vhd`, `vhdl/neorv32_dmem.vhd`, `vhdl/neorv32_gpio.vhd` | same project — more `rtl/core` units (arrays of records, record ports, generics without defaults, multi-entity files) | BSD-3-Clause |
| `vhdl/neorv32_bootrom.vhd`, `vhdl/neorv32_bootrom_image.vhd`, `vhdl/neorv32_cache_ram.vhd`, `vhdl/neorv32_cfs.vhd`, `vhdl/neorv32_core_complex.vhd`, `vhdl/neorv32_cpu_alu_bitmanip.vhd`, `vhdl/neorv32_cpu_alu_cfu.vhd`, `vhdl/neorv32_cpu_alu_cond.vhd`, `vhdl/neorv32_cpu_alu_crypto.vhd`, `vhdl/neorv32_cpu_alu_fpu.vhd`, `vhdl/neorv32_cpu_counters.vhd`, `vhdl/neorv32_cpu_hwtrig.vhd`, `vhdl/neorv32_cpu_trace.vhd`, `vhdl/neorv32_debug_auth.vhd`, `vhdl/neorv32_debug_dm.vhd`, `vhdl/neorv32_debug_dtm.vhd`, `vhdl/neorv32_dma.vhd`, `vhdl/neorv32_gptmr.vhd`, `vhdl/neorv32_neoled.vhd`, `vhdl/neorv32_prim.vhd`, `vhdl/neorv32_sdi.vhd`, `vhdl/neorv32_slink.vhd`, `vhdl/neorv32_smc.vhd`, `vhdl/neorv32_spi.vhd`, `vhdl/neorv32_sys.vhd`, `vhdl/neorv32_sysinfo.vhd`, `vhdl/neorv32_top.vhd`, `vhdl/neorv32_tracebuf.vhd`, `vhdl/neorv32_twd.vhd`, `vhdl/neorv32_twi.vhd`, `vhdl/neorv32_uart.vhd`, `vhdl/neorv32_xbus.vhd` | same project — all remaining `rtl/core` units (FPU uses `ieee.float_pkg`/`fixed_pkg` types; `neorv32_cpu_trace` clean (const array-of-record field access supported)) | BSD-3-Clause |
| `vhdl/unisim_{BUFG,IBUF,ODDR,FD}.vhd` | [deuskane/xilinx-primitive-unisim](https://github.com/deuskane/xilinx-primitive-unisim) — mirror of the Xilinx UNISIM VHDL component models (Copyright Xilinx, Inc.; used here as parse fixtures) | see upstream |
| `vhdl/IfElsePkg.vhd`, `vhdl/RandomPkg.vhd`, `vhdl/AlertLogPkg.vhd`, `vhdl/ScoreboardGenericPkg.vhd` | [OSVVM/OSVVM](https://github.com/OSVVM/OSVVM) — VHDL verification library (VHDL-2008: generic packages, protected types, textio-heavy) | MIT |

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
