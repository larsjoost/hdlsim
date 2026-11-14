#!/bin/sh
# Run the fp-vhdl example testbench (fp32 add/mul) with hdlsim.
# Usage: ./run_tests.sh [path-to-hdlsim-binary]   (default: ../../target/debug/hdlsim)
set -u
# Resolve the binary to an absolute path BEFORE cd (CI passes a repo-relative path).
HDLSIM="${1:-../../target/debug/hdlsim}"
case "$HDLSIM" in /*) ;; *) HDLSIM="$(pwd)/$HDLSIM" ;; esac
cd "$(dirname "$0")"
# hdlsim exits non-zero when a test fails (assert/report severity error|failure)
if out="$("$HDLSIM" run --top tb_fp --max-time 100us 2>&1)"; then
    printf '%s\n' "$out" | grep -E "PASS: fp" | tail -1
    echo "fp-vhdl: all testbenches PASS"
else
    printf '%s\n' "$out"
    echo "fp-vhdl: FAILURES" >&2
    exit 1
fi
