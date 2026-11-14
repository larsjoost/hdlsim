#!/bin/sh
# Run all RV32I example testbenches with hdlsim.
# Usage: ./run_tests.sh [path-to-hdlsim-binary]   (default: ../../target/debug/hdlsim)
set -u
cd "$(dirname "$0")"
HDLSIM="${1:-../../target/debug/hdlsim}"
fail=0
for tb in tb_alu tb_decode tb_regfile tb_cpu_fib tb_cpu_sum tb_cpu_jal; do
    out="$("$HDLSIM" run --top "$tb" --max-time 10us -L work=. ./*.vhd 2>&1)"
    if printf '%s' "$out" | grep -q PASS && ! printf '%s' "$out" | grep -q FAIL; then
        printf '%s\n' "$out" | grep PASS
    else
        printf '%s\n' "$out"
        fail=1
    fi
done
if [ "$fail" -eq 0 ]; then
    echo "riscv-vhdl: all testbenches PASS"
else
    echo "riscv-vhdl: FAILURES" >&2
fi
exit "$fail"
