#!/bin/sh
# Run the fp-vhdl example testbench (fp32 add/mul) with hdlsim.
# Usage: ./run_tests.sh [path-to-hdlsim-binary]   (default: ../../target/debug/hdlsim)
set -u
cd "$(dirname "$0")"
HDLSIM="${1:-../../target/debug/hdlsim}"
out="$("$HDLSIM" run --top tb_fp --max-time 100us 2>&1)"
if printf '%s' "$out" | grep -q PASS && ! printf '%s' "$out" | grep -q FAIL; then
    printf '%s\n' "$out" | grep -E "PASS: fp" | tail -1
    echo "fp-vhdl: all testbenches PASS"
else
    printf '%s\n' "$out"
    echo "fp-vhdl: FAILURES" >&2
    exit 1
fi
