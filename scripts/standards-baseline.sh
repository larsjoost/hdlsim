#!/usr/bin/env bash
# Standards ratchet (AGENTS.md): shape debt must never grow.
#
# Regenerates docs/standards-baseline.json from the current tree, or with
# --check verifies the committed baseline. Debt is counted per file:
#   - functions over the 50-line split line (clippy::too_many_lines, all
#     targets),
#   - functions over cognitive complexity 30 (clippy::cognitive_complexity,
#     library code only — tests are exempt, like the crash rules),
#   - module sizes over ~600 lines.
# A count above the baseline — or debt in a file that has no baseline
# entry at all — fails. Improvements lower the numbers: regenerate and
# commit the lowered baseline with the change that earned it. Raising a
# number needs review agreement in the PR that edits the baseline.
#
# Usage: scripts/standards-baseline.sh [--check]
set -euo pipefail
cd "$(dirname "$0")/.."

case "${1:-}" in
  --check) check=1 ;;
  "") check=0 ;;
  *)
    echo "usage: $0 [--check]" >&2
    exit 2
    ;;
esac

baseline="docs/standards-baseline.json"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export CARGO_TERM_COLOR=never

# Plain output (no ANSI) so the `--> file:line` anchors parse; the runs
# are allowed to fail — warnings are the data, a broken compile must not
# hide behind them here (the blocking clippy job catches those).
cargo clippy --all-targets -- -W clippy::too_many_lines \
  >"$tmp/tml.txt" 2>&1 || true
cargo clippy --workspace --lib -- -W clippy::cognitive_complexity \
  >"$tmp/cc.txt" 2>&1 || true
find crates -name '*.rs' -path '*/src/*' -print0 \
  | xargs -0 wc -l >"$tmp/wc.txt" || true

python3 - "$tmp" "$baseline" "$check" <<'PYEOF'
import json
import re
import sys
from collections import Counter

tmp, baseline_path, check = sys.argv[1], sys.argv[2], sys.argv[3] == "1"


def lint_counts(name, marker):
    text = open(f"{tmp}/{name}", encoding="utf-8", errors="replace").read()
    counts = Counter()
    for block in re.split(r"(?=^warning: )", text, flags=re.M):
        if marker not in block:
            continue
        m = re.search(r"^\s+-->\s+(\S+?):\d+:\d+", block, re.M)
        if m and m.group(1).startswith("crates/"):
            counts[m.group(1)] += 1
    return dict(sorted(counts.items()))


module_lines = {}
for line in open(f"{tmp}/wc.txt", encoding="utf-8", errors="replace"):
    parts = line.split(None, 1)
    if len(parts) == 2 and parts[1].rstrip().startswith("crates/"):
        n, path = int(parts[0]), parts[1].rstrip()
        if n > 600:
            module_lines[path] = n

current = {
    "too_many_lines": lint_counts("tml.txt", "too many lines"),
    "cognitive_complexity": lint_counts("cc.txt", "cognitive complexity"),
    "module_lines": dict(sorted(module_lines.items())),
}

if not check:
    with open(baseline_path, "w", encoding="utf-8") as f:
        json.dump(current, f, indent=2, sort_keys=True)
        f.write("\n")
    for rule, files in current.items():
        print(f"{rule}: {sum(files.values())} in {len(files)} file(s)")
    print(f"baseline written to {baseline_path}")
    sys.exit(0)

try:
    baseline = json.load(open(baseline_path, encoding="utf-8"))
except (OSError, json.JSONDecodeError) as e:
    sys.exit(f"cannot read baseline {baseline_path}: {e}")

violations = []
for rule, files in current.items():
    base = baseline.get(rule)
    if base is None:
        violations.append(f"{rule}: rule table missing from the baseline")
        continue
    for path, n in sorted(files.items()):
        b = base.get(path)
        if b is None:
            violations.append(f"{rule}: {path} has {n} but has no baseline entry (new debt)")
        elif n > b:
            violations.append(f"{rule}: {path} has {n} > baseline {b}")

if violations:
    print("standards ratchet violated — shape debt grew:")
    for v in violations:
        print(f"  {v}")
    sys.exit(1)

for rule, files in current.items():
    base = baseline.get(rule, {})
    grown, total = sum(files.values()), sum(base.values())
    print(f"{rule}: {grown} (baseline {total})")
    if grown < total:
        print(f"  improved — consider lowering the baseline: scripts/standards-baseline.sh")
print("standards ratchet ok")
PYEOF
