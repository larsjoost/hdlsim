# Real-world source corpus

Third-party VHDL/Verilog sources fetched from the internet, used as parser and
front-end test fixtures (`../corpus.rs`). Each file is used unmodified; licenses
are in `licenses/` and each file header retains its upstream copyright notice.

Fetched 2026-11 from raw.githubusercontent.com:

| File | Project | License |
|---|---|---|
| `verilog/serv_alu.v`, `verilog/serv_decode.v` | [olofk/serv](https://github.com/olofk/serv) — bit-serial RISC-V CPU (Verilog) | ISC |
| `verilog/xilinx_unisim_{BUFG,IBUF,ODDR}.v` | [Xilinx/XilinxUnisimLibrary](https://github.com/Xilinx/XilinxUnisimLibrary) — official Xilinx UNISIM primitives (Verilog) | Apache-2.0 |
| `vhdl/neorv32_cpu_alu.vhd`, `vhdl/neorv32_imem.vhd` | [stnolting/neorv32](https://github.com/stnolting/neorv32) — RISC-V SoC in VHDL | BSD-3-Clause |
| `vhdl/unisim_{BUFG,IBUF,ODDR,FD}.vhd` | [deuskane/xilinx-primitive-unisim](https://github.com/deuskane/xilinx-primitive-unisim) — mirror of the Xilinx UNISIM VHDL component models (Copyright Xilinx, Inc.; used here as parse fixtures) | see upstream |

The tests assert: every file lexes and parses without errors (the CST is
lossless), and a full `check` (parse + lower + resolve) runs to completion on
each file without panicking, rendering any diagnostics rustc-style. Files that
use language features outside hdlsim's current subset are expected to produce
*diagnostics* (that is what the check is for) — never a crash.
