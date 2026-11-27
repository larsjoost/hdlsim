//! The `--watch` signal-change watch. The `--when` /
//! `--examine` probes live in the sibling `probe.rs`.
//!
//! Child module of `sim` so it can read the kernel's private fields, the
//! same pattern as `sim/ckpt.rs`.

use super::Simulator;
use hdl_ir::elab::FlatSigId;
use hdl_ir::fmt_time;
use hdl_ir::value::Value;

impl Simulator {
    /// Arm the signal watch: `spec` is a comma-separated list of
    /// hierarchical signal names (case-insensitive; a name also matches as
    /// a dot-suffix, `dut.q` matches `tb.dut.q`). Resolved immediately so
    /// a typo fails before the run instead of silently watching nothing —
    /// zero matches for a name is an error, multiple matches are intended.
    pub fn set_watch(
        &mut self,
        spec: &str,
        x_only: bool,
        snapshots: bool,
        max_lines: u64,
    ) -> Result<(), String> {
        let mut unmatched: Vec<String> = Vec::new();
        for cand in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let hits = self.resolve_signal_pattern(cand);
            if hits.is_empty() {
                unmatched.push(cand.to_string());
            }
            self.watch.extend(hits);
        }
        if !unmatched.is_empty() {
            self.watch.clear();
            let mut msg = format!("--watch: no signal matched {}", unmatched.join(", "));
            for cand in &unmatched {
                let s = self.watch_suggestions(cand);
                if !s.is_empty() {
                    msg.push('\n');
                    msg.push_str(&s);
                }
            }
            return Err(msg);
        }
        self.watch_x_only = x_only;
        self.watch_snapshots = snapshots;
        self.watch_max_lines = max_lines.max(1);
        eprintln!(
            "watch: {} signal(s) [{}] — x-only={} snapshots={} cap={}",
            self.watch.len(),
            self.watch
                .iter()
                .map(|(_, n)| n.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            self.watch_x_only,
            self.watch_snapshots,
            self.watch_max_lines
        );
        Ok(())
    }

    /// Resolve one name pattern against the flat signal list: exact match or
    /// dot-suffix match (`dut.q` matches `tb.dut.q`), case-insensitive —
    /// the same rule as `--watch`. Empty when nothing matches.
    pub(super) fn resolve_signal_pattern(&self, name: &str) -> Vec<(FlatSigId, String)> {
        let key = name.to_ascii_lowercase();
        let suffix = format!(".{key}");
        self.flat
            .signals
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let n = s.name.to_ascii_lowercase();
                n == key || n.ends_with(&suffix)
            })
            .map(|(i, s)| (i as FlatSigId, s.name.clone()))
            .collect()
    }

    /// ` driver=<origin>` for whoever is producing the current change: the
    /// running process, `assign` inside the continuous-assign sweep, empty
    /// outside evaluation (t=0 init).
    pub(super) fn driver_suffix(&self) -> String {
        match self.cur_proc {
            Some(p) if (p as usize) < self.flat.procs.len() => {
                format!(" driver={}", self.flat.procs[p as usize].origin)
            }
            Some(_) => " driver=assign".to_string(),
            None => String::new(),
        }
    }

    /// All signal names (hierarchical, exactly as `set_watch` matches
    /// them), sorted with duplicates removed (duplicate instance names
    /// produce duplicate signal names). Backs `run --list-signals`.
    pub fn signal_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.flat.signals.iter().map(|s| s.name.clone()).collect();
        names.sort();
        names.dedup();
        names
    }

    /// Ranked near-match candidates for an unmatched name: (tier, distance,
    /// full signal name) pairs sorted best-first, plus the count of signals
    /// sharing the trailing component. Tiers — 0: same trailing name
    /// component (the signal exists, the instance path is off), 1: trailing
    /// component within the edit budget (a typo'd component), 2: full name
    /// within the edit budget.
    fn watch_suggestion_candidates(&self, cand: &str) -> (Vec<(u8, usize, &str)>, usize) {
        let key = cand.to_ascii_lowercase();
        let last = key.rsplit('.').next().unwrap_or(&key).to_string();
        let max_d = (key.len() / 3).max(3);
        let mut cands: Vec<(u8, usize, &str)> = Vec::new();
        let mut suffix_count = 0usize;
        for s in &self.flat.signals {
            let n = s.name.to_ascii_lowercase();
            let nlast = n.rsplit('.').next().unwrap_or(&n);
            if nlast == last {
                suffix_count += 1;
                cands.push((0, 0, &s.name));
            } else {
                let dc = hdl_ir::levenshtein_bounded(&last, nlast, max_d);
                if dc <= max_d {
                    cands.push((1, dc, &s.name));
                } else {
                    let df = hdl_ir::levenshtein_bounded(&key, &n, max_d);
                    if df <= max_d {
                        cands.push((2, df, &s.name));
                    }
                }
            }
        }
        cands.sort();
        (cands, suffix_count)
    }

    /// rustc-style help for an unmatched `--watch` name: up to three
    /// similarly named signals ranked by [`Self::watch_suggestion_candidates`]
    /// (with the shared-trailing-component count as a `--list-signals`
    /// pointer). Empty-input note handled by the caller; this never returns
    /// an empty string.
    pub(super) fn watch_suggestions(&self, cand: &str) -> String {
        let (cands, suffix_count) = self.watch_suggestion_candidates(cand);
        let key = cand.to_ascii_lowercase();
        let last = key.rsplit('.').next().unwrap_or(&key).to_string();
        let shown: Vec<&str> = cands.iter().take(3).map(|(_, _, n)| *n).collect();
        let mut out = String::new();
        match shown.len() {
            0 => {}
            1 => out.push_str(&format!("did you mean '{}'?", shown[0])),
            _ => {
                let (last_one, rest) = shown
                    .split_last()
                    .expect("the 0- and 1-length arms are handled above");
                out.push_str(&format!(
                    "did you mean {} or '{}'?",
                    rest.iter()
                        .map(|s| format!("'{s}'"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    last_one
                ));
            }
        }
        if suffix_count > 0 {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!(
                "note: {suffix_count} signal(s) end in '.{last}' — run with --list-signals={last} to list them"
            ));
        }
        if out.is_empty() {
            out.push_str("note: run with --list-signals to print all signal names");
        }
        out
    }

    /// Line-budget gate shared by change lines and snapshots: false once
    /// the cap is hit (after printing the one-time notice on the line that
    /// exhausts the budget). Keeps a runaway design from flooding the log.
    fn watch_take_line(&mut self) -> bool {
        if self.watch_lines >= self.watch_max_lines {
            return false;
        }
        self.watch_lines += 1;
        if self.watch_lines == self.watch_max_lines {
            eprintln!(
                "watch: line cap ({}) reached — further lines suppressed (--watch-max)",
                self.watch_max_lines
            );
        }
        true
    }

    /// One `watch:` line for a changed watched signal. Called from
    /// `set_sig` only when the watch list is non-empty, before the
    /// observer emit, so the line keeps the causal order with other
    /// stderr diagnostics. In x-only mode only transitions into
    /// metavarues print — the fastest path to "where did the first X
    /// come from, and who wrote it".
    pub(super) fn watch_change(&mut self, sig: FlatSigId, old: &Value, new: &Value) {
        if !self.watch.iter().any(|(id, _)| *id == sig) {
            return;
        }
        if self.watch_x_only && !new.has_meta() {
            return;
        }
        if !self.watch_take_line() {
            return;
        }
        let name = self
            .watch
            .iter()
            .find(|(id, _)| *id == sig)
            .map(|(_, n)| n.as_str())
            .unwrap_or_default();
        let driver = self.driver_suffix();
        eprintln!(
            "watch t={} d{} {} {}->{}{}",
            fmt_time(self.time),
            self.delta_no,
            name,
            old.fmt_value(),
            new.fmt_value(),
            driver
        );
    }

    /// `--watch-deltas`: settled values of all watched signals at each
    /// timestamp — pump calls this at the settle point, once per timestamp,
    /// after the drain/NBA/fixpoint work has quiesced. Unchanged signals
    /// print too, so "did this ever become valid" is one grep.
    pub(super) fn watch_snapshot(&mut self) {
        if !self.watch_snapshots || self.watch.is_empty() || !self.watch_take_line() {
            return;
        }
        let vals: Vec<String> = self
            .watch
            .iter()
            .map(|(id, name)| format!("{}={}", name, self.vals[*id as usize].fmt_value()))
            .collect();
        eprintln!(
            "watch-snap t={} d{} {}",
            fmt_time(self.time),
            self.delta_no,
            vals.join(" ")
        );
    }
}
