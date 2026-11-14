#!/bin/sh
# Run all RV32I example testbenches with hdlsim.
# The example is split into two libraries (see hdlsim.ini: rv32i=rtl, work=tb);
# no file arguments are needed — hdlsim resolves the top testbench and every
# dependency (cpu -> alu/decode/regfile/imem/dmem) across the libraries.
# Usage: ./run_tests.sh [path-to-hdlsim-binary]   (default: ../../target/debug/hdlsim)
set -u
# Resolve the binary to an absolute path BEFORE cd (CI passes a repo-relative path).
HDLSIM="${1:-../../target/debug/hdlsim}"
case "$HDLSIM" in /*) ;; *) HDLSIM="$(pwd)/$HDLSIM" ;; esac
cd "$(dirname "$0")"
fail=0
for tb in tb_alu tb_decode tb_regfile tb_cpu_fib tb_cpu_sum tb_cpu_jal; do
    # hdlsim exits non-zero when a test fails (assert/report severity error|failure)
    if out="$("$HDLSIM" run --top "$tb" --max-time 10us 2>&1)"; then
        printf '%s\n' "$out" | grep PASS
    else
        printf '%s\n' "$out"
        fail=1
    fi
done
if [ "$fail" -eq 0 ]; then
    echo "riscv-vhdl: all testbenches PASS (multi-library resolution via hdlsim.ini)"
else
    echo "riscv-vhdl: FAILURES" >&2
fi
exit "$fail"
