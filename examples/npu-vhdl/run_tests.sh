#!/bin/sh
# Run the NPU example testbenches (unit + cpu-integration) with hdlsim.
# Usage: ./run_tests.sh [path-to-hdlsim-binary]   (default: ../../target/debug/hdlsim)
set -u
cd "$(dirname "$0")"
HDLSIM="${1:-../../target/debug/hdlsim}"
fail=0
for tb in tb_npu tb_cpu_npu; do
    out="$("$HDLSIM" run --top "$tb" --max-time 100us 2>&1)"
    if printf '%s' "$out" | grep -q PASS && ! printf '%s' "$out" | grep -q FAIL; then
        printf '%s\n' "$out" | grep -E "PASS: (npu|cpu)" | tail -1
    else
        printf '%s\n' "$out"
        fail=1
    fi
done
if [ "$fail" -eq 0 ]; then
    echo "npu-vhdl: all testbenches PASS"
else
    echo "npu-vhdl: FAILURES" >&2
fi
exit "$fail"
