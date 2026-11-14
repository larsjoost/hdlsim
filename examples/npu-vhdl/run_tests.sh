#!/bin/sh
# Run the NPU example testbenches (unit + cpu-integration) with hdlsim.
# Usage: ./run_tests.sh [path-to-hdlsim-binary]   (default: ../../target/debug/hdlsim)
set -u
# Resolve the binary to an absolute path BEFORE cd (CI passes a repo-relative path).
HDLSIM="${1:-../../target/debug/hdlsim}"
case "$HDLSIM" in /*) ;; *) HDLSIM="$(pwd)/$HDLSIM" ;; esac
cd "$(dirname "$0")"
fail=0
for tb in tb_npu tb_cpu_npu; do
    # hdlsim exits non-zero when a test fails (assert/report severity error|failure)
    if out="$("$HDLSIM" run --top "$tb" --max-time 100us 2>&1)"; then
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
