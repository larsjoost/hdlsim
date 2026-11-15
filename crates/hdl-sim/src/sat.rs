//! Own CDCL SAT solver — MiniSat-style: two-watched-literal propagation,
//! first-UIP conflict analysis with clause learning, VSIDS variable
//! branching with phase saving, Luby restarts, and learned-clause
//! reduction. Backs `hdlsim prove` (bounded model checking).
//!
//! Invariants:
//! - A literal is `2*var` (positive) or `2*var+1` (negative); `var_of(l) = l >> 1`.
//! - `assigns[v]` is 0 (unassigned), 1 (true), or -1 (false); `trail` is the
//!   assignment stack, `trail_lim` marks decision levels.
//! - Every stored clause keeps its two watched literals in `lits[0]`/`lits[1]`.
//! - No external crates, no panics on any input; `solve` returns `None` on
//!   UNSAT and `Some(model)` (one bool per variable, unassigned = false) on SAT.

/// A propositional literal: `2*var` positive, `2*var+1` negative.
pub type Lit = i32;

#[inline]
pub fn var_of(l: Lit) -> usize {
    (l >> 1) as usize
}

#[inline]
pub fn neg(l: Lit) -> Lit {
    l ^ 1
}

#[inline]
pub fn is_neg(l: Lit) -> bool {
    l & 1 == 1
}

/// Literal of variable `v` with polarity `negative`.
#[inline]
pub fn lit_of(v: usize, negative: bool) -> Lit {
    ((v as i32) << 1) | (negative as i32)
}

#[inline]
fn lit_val(assigns: &[i8], l: Lit) -> i8 {
    let a = assigns[var_of(l)];
    if a == 0 {
        0
    } else if (a > 0) == !is_neg(l) {
        1
    } else {
        -1
    }
}

#[derive(Clone, Debug)]
struct Clause {
    lits: Vec<Lit>,
    learned: bool,
}

const NONE: u32 = u32::MAX;

/// CDCL solver over a CNF built with [`Solver::add_clause`].
pub struct Solver {
    n_vars: usize,
    clauses: Vec<Clause>,
    watches: Vec<Vec<usize>>,
    assigns: Vec<i8>,
    level: Vec<u32>,
    reason: Vec<u32>,
    trail: Vec<Lit>,
    trail_lim: Vec<usize>,
    phase: Vec<bool>,
    activity: Vec<f64>,
    var_inc: f64,
    heap: Vec<usize>,
    in_heap: Vec<bool>,
    ok: bool,
    luby_i: usize,
    qhead: usize,
    /// Conflict counter; also drives restarts (`100 * luby(2, luby_i)`).
    pub conflicts: u64,
    pub decisions: u64,
    pub propagations: u64,
}

impl Solver {
    pub fn new() -> Solver {
        Solver {
            n_vars: 0,
            clauses: Vec::new(),
            watches: Vec::new(),
            assigns: Vec::new(),
            level: Vec::new(),
            reason: Vec::new(),
            trail: Vec::new(),
            trail_lim: Vec::new(),
            phase: Vec::new(),
            activity: Vec::new(),
            var_inc: 1.0,
            heap: Vec::new(),
            in_heap: Vec::new(),
            ok: true,
            luby_i: 0,
            qhead: 0,
            conflicts: 0,
            decisions: 0,
            propagations: 0,
        }
    }

    pub fn n_vars(&self) -> usize {
        self.n_vars
    }

    /// Allocate a fresh variable, returns its index.
    pub fn new_var(&mut self) -> usize {
        let v = self.n_vars;
        self.n_vars += 1;
        self.watches.push(Vec::new());
        self.watches.push(Vec::new());
        self.assigns.push(0);
        self.level.push(0);
        self.reason.push(NONE);
        self.phase.push(true);
        self.activity.push(0.0);
        self.in_heap.push(true);
        self.heap.push(v);
        self.heap_sift_up(self.heap.len() - 1);
        v
    }

    #[inline]
    fn decision_level(&self) -> u32 {
        self.trail_lim.len() as u32
    }

    /// Add a clause at level 0. Returns `false` if the CNF became UNSAT.
    pub fn add_clause(&mut self, lits: &[Lit]) -> bool {
        if !self.ok {
            return false;
        }
        let mut ls: Vec<Lit> = lits.to_vec();
        ls.sort_unstable();
        ls.dedup();
        // Tautology (contains l and ¬l) is always satisfied: drop it.
        let mut taut = false;
        for i in 0..ls.len().saturating_sub(1) {
            if ls[i] == neg(ls[i + 1]) {
                taut = true;
                break;
            }
        }
        if taut {
            return true;
        }
        // Drop literals already false at level 0; a true one satisfies the clause.
        let mut kept: Vec<Lit> = Vec::with_capacity(ls.len());
        for &l in &ls {
            match lit_val(&self.assigns, l) {
                1 => return true,
                -1 => {}
                _ => kept.push(l),
            }
        }
        match kept.len() {
            0 => {
                self.ok = false;
                false
            }
            1 => self.enqueue(kept[0], NONE),
            _ => {
                let ci = self.clauses.len();
                self.clauses.push(Clause {
                    lits: kept.clone(),
                    learned: false,
                });
                self.watches[kept[0] as usize].push(ci);
                self.watches[kept[1] as usize].push(ci);
                true
            }
        }
    }

    /// Solve. `None` = UNSAT, `Some(model)` = SAT (model[v] = value of var v).
    pub fn solve(&mut self) -> Option<Vec<bool>> {
        if !self.ok {
            return None;
        }
        loop {
            match self.propagate() {
                Some(confl) => {
                    self.conflicts += 1;
                    if self.decision_level() == 0 {
                        self.ok = false;
                        return None;
                    }
                    let (learnt, bt) = self.analyze(confl);
                    self.backtrack(bt);
                    if learnt.len() == 1 {
                        // Root-level unit: record permanently (bt == 0 here).
                        if !self.add_clause(&learnt) {
                            self.ok = false;
                            return None;
                        }
                        continue;
                    }
                    let ci = self.add_learned(learnt);
                    self.var_inc *= 1.0 / 0.95;
                    for i in 0..self.clauses[ci].lits.len() {
                        let v = var_of(self.clauses[ci].lits[i]);
                        self.activity[v] += self.var_inc;
                        if self.in_heap[v] {
                            self.heap_bump(v);
                        }
                    }
                    if self.var_inc > 1e100 {
                        for a in self.activity.iter_mut() {
                            *a *= 1e-100;
                        }
                        self.var_inc *= 1e-100;
                    }
                    self.enqueue(self.clauses[ci].lits[0], ci as u32);
                    if self.conflicts >= 100 * luby(2.0, self.luby_i + 1) as u64 {
                        self.luby_i += 1;
                        self.backtrack(0);
                        self.reduce_db();
                    }
                }
                None => {
                    if self.trail.len() == self.n_vars {
                        return Some(self.model());
                    }
                    let l = self.pick_branch();
                    self.decisions += 1;
                    self.trail_lim.push(self.trail.len());
                    self.enqueue(l, NONE);
                }
            }
        }
    }

    fn model(&self) -> Vec<bool> {
        (0..self.n_vars).map(|v| self.assigns[v] > 0).collect()
    }

    fn enqueue(&mut self, l: Lit, reason: u32) -> bool {
        match lit_val(&self.assigns, l) {
            1 => return true,
            -1 => return false,
            _ => {}
        }
        let v = var_of(l);
        self.assigns[v] = if is_neg(l) { -1 } else { 1 };
        self.level[v] = self.decision_level();
        self.reason[v] = reason;
        self.trail.push(l);
        self.heap_remove(v);
        true
    }

    /// Two-watched-literal propagation. Returns the conflicting clause index.
    fn propagate(&mut self) -> Option<usize> {
        // `qhead` is persistent across calls: reprocessing already-processed
        // trail entries could surface conflicts whose last-falsified literal
        // is below the current decision level, which analyze() cannot handle.
        while self.qhead < self.trail.len() {
            let p = self.trail[self.qhead];
            self.qhead += 1;
            self.propagations += 1;
            let fl = neg(p) as usize;
            let mut i = 0usize;
            while i < self.watches[fl].len() {
                let ci = self.watches[fl][i];
                {
                    let lits = &mut self.clauses[ci].lits;
                    if lits[0] == neg(p) {
                        lits.swap(0, 1);
                    }
                }
                let first = self.clauses[ci].lits[0];
                if lit_val(&self.assigns, first) == 1 {
                    i += 1;
                    continue;
                }
                // Look for a new literal to watch.
                let n = self.clauses[ci].lits.len();
                let mut found = None;
                for k in 2..n {
                    if lit_val(&self.assigns, self.clauses[ci].lits[k]) != -1 {
                        found = Some(k);
                        break;
                    }
                }
                if let Some(k) = found {
                    // Move the watch from `fl` to lits[k]: a clause watching
                    // lit w lives in watches[w] (processed when w turns false).
                    let lits = &mut self.clauses[ci].lits;
                    lits.swap(1, k);
                    let wl = lits[1] as usize;
                    self.watches[wl].push(ci);
                    self.watches[fl].swap_remove(i);
                    continue; // element at i was removed; re-examine index i
                }
                if lit_val(&self.assigns, first) == -1 {
                    return Some(ci);
                }
                self.enqueue(first, ci as u32);
                i += 1;
            }
        }
        None
    }

    /// First-UIP conflict analysis. Returns (learned clause, backtrack level).
    fn analyze(&mut self, confl: usize) -> (Vec<Lit>, u32) {
        let mut learnt: Vec<Lit> = vec![0];
        let mut seen = vec![false; self.n_vars];
        let mut path_c = 0u32;
        let mut p: Lit = -1;
        let mut idx = self.trail.len();
        let mut clause = self.clauses[confl].lits.clone();
        let mut bt_level = 0u32;
        loop {
            let start = if p == -1 { 0 } else { 1 };
            for j in start..clause.len() {
                let q = clause[j];
                let v = var_of(q);
                if !seen[v] && self.level[v] > 0 {
                    seen[v] = true;
                    if self.level[v] >= self.decision_level() {
                        path_c += 1;
                    } else {
                        learnt.push(q);
                        if self.level[v] > bt_level {
                            bt_level = self.level[v];
                        }
                    }
                }
            }
            // Expand the most recently assigned seen variable.
            loop {
                idx -= 1;
                let v = var_of(self.trail[idx]);
                if seen[v] {
                    p = self.trail[idx];
                    seen[v] = false;
                    break;
                }
            }
            path_c -= 1;
            if path_c == 0 {
                break;
            }
            let r = self.reason[var_of(p)];
            clause = self.clauses[r as usize].lits.clone();
        }
        learnt[0] = neg(p);
        for &l in &learnt {
            seen[var_of(l)] = false;
        }
        (learnt, bt_level)
    }

    fn backtrack(&mut self, lvl: u32) {
        if self.decision_level() <= lvl {
            return;
        }
        let lim = self.trail_lim[lvl as usize];
        for i in lim..self.trail.len() {
            let v = var_of(self.trail[i]);
            self.assigns[v] = 0;
            self.reason[v] = NONE;
            self.level[v] = 0;
            self.phase[v] = !is_neg(self.trail[i]); // phase saving
            self.heap_insert(v);
        }
        self.trail.truncate(lim);
        self.trail_lim.truncate(lvl as usize);
        self.qhead = self.trail.len();
    }

    fn add_learned(&mut self, lits: Vec<Lit>) -> usize {
        let ci = self.clauses.len();
        self.clauses.push(Clause {
            lits,
            learned: true,
        });
        self.watches[self.clauses[ci].lits[0] as usize].push(ci);
        self.watches[self.clauses[ci].lits[1] as usize].push(ci);
        ci
    }

    fn pick_branch(&mut self) -> Lit {
        while let Some(&v) = self.heap.last() {
            if self.assigns[v] != 0 {
                self.heap.pop();
                self.in_heap[v] = false;
                continue;
            }
            self.heap.pop();
            self.in_heap[v] = false;
            return lit_of(v, !self.phase[v]);
        }
        // Heap exhausted (should not happen when trail < n_vars): fall back.
        for v in 0..self.n_vars {
            if self.assigns[v] == 0 {
                return lit_of(v, !self.phase[v]);
            }
        }
        lit_of(0, false)
    }

    /// Drop the oldest half of non-locked learned clauses once there are many.
    fn reduce_db(&mut self) {
        let learned: Vec<usize> = (0..self.clauses.len())
            .filter(|&i| self.clauses[i].learned)
            .collect();
        if learned.len() < 20_000 {
            return;
        }
        let cut = learned.len() / 2;
        let mut drop: Vec<bool> = vec![false; self.clauses.len()];
        for &ci in &learned[..cut] {
            let locked = self.reason.iter().any(|&r| r == ci as u32);
            if !locked {
                drop[ci] = true;
            }
        }
        let mut remap = vec![usize::MAX; self.clauses.len()];
        let mut kept: Vec<Clause> = Vec::new();
        for (ci, c) in self.clauses.iter().enumerate() {
            if drop[ci] {
                continue;
            }
            remap[ci] = kept.len();
            kept.push(c.clone());
        }
        // Fix reason pointers of assigned variables.
        for r in self.reason.iter_mut() {
            if *r != NONE {
                *r = remap[*r as usize] as u32;
            }
        }
        self.clauses = kept;
        self.watches = vec![Vec::new(); 2 * self.n_vars];
        for (ci, c) in self.clauses.iter().enumerate() {
            self.watches[c.lits[0] as usize].push(ci);
            if c.lits.len() > 1 {
                self.watches[c.lits[1] as usize].push(ci);
            }
        }
    }

    // --- binary heap over variables, max activity at the top ---

    fn heap_less(&self, a: usize, b: usize) -> bool {
        self.activity[a] < self.activity[b]
    }

    fn heap_sift_up(&mut self, mut i: usize) {
        while i > 0 {
            let parent = (i - 1) / 2;
            if self.heap_less(self.heap[parent], self.heap[i]) {
                self.heap.swap(parent, i);
                i = parent;
            } else {
                break;
            }
        }
    }

    fn heap_sift_down(&mut self, mut i: usize) {
        loop {
            let l = 2 * i + 1;
            let r = l + 1;
            let mut best = i;
            if l < self.heap.len() && self.heap_less(self.heap[best], self.heap[l]) {
                best = l;
            }
            if r < self.heap.len() && self.heap_less(self.heap[best], self.heap[r]) {
                best = r;
            }
            if best == i {
                break;
            }
            self.heap.swap(i, best);
            i = best;
        }
    }

    fn heap_insert(&mut self, v: usize) {
        if self.in_heap[v] {
            return;
        }
        self.in_heap[v] = true;
        self.heap.push(v);
        self.heap_sift_up(self.heap.len() - 1);
    }

    fn heap_remove(&mut self, v: usize) {
        if !self.in_heap[v] {
            return;
        }
        let pos = self.heap.iter().position(|&x| x == v).unwrap();
        self.heap.swap_remove(pos);
        self.in_heap[v] = false;
        if pos < self.heap.len() {
            self.heap_sift_up(pos);
            self.heap_sift_down(pos);
        }
    }

    fn heap_bump(&mut self, v: usize) {
        if let Some(pos) = self.heap.iter().position(|&x| x == v) {
            self.heap_sift_up(pos);
        }
    }
}

/// Luby sequence: u = 1,1,2,1,1,2,4,... built as U_{k+1} = U_k ++ U_k ++ [2^k];
/// `luby(y, x) = y^u[x-1]` (u entries are powers of two, stored as the value).
fn luby(y: f64, x: usize) -> f64 {
    let mut u: Vec<u64> = vec![1];
    let mut k = 1u32;
    while u.len() < x {
        let mut next = u.clone();
        next.extend_from_slice(&u);
        next.push(1u64 << k);
        u = next;
        k += 1;
    }
    y.powi(u[x - 1].trailing_zeros() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &mut Solver, i: usize) -> Lit {
        while s.n_vars() <= i {
            s.new_var();
        }
        lit_of(i, false)
    }

    #[test]
    fn empty_cnf_is_sat() {
        let mut s = Solver::new();
        assert!(s.solve().is_some());
    }

    #[test]
    fn unit_conflict_is_unsat() {
        let mut s = Solver::new();
        let a = v(&mut s, 0);
        assert!(s.add_clause(&[a]));
        assert!(!s.add_clause(&[neg(a)]));
        assert!(s.solve().is_none());
    }

    #[test]
    fn simple_sat_model_is_valid() {
        let mut s = Solver::new();
        let a = v(&mut s, 0);
        let b = v(&mut s, 1);
        assert!(s.add_clause(&[a, b]));
        assert!(s.add_clause(&[neg(a), b]));
        let m = s.solve().expect("sat");
        assert!(m[1], "b must be true in every model");
    }

    #[test]
    fn xor_chain() {
        // a xor b, b xor c, a xor c — parity of 3 vars can't be all-different → UNSAT.
        let mut s = Solver::new();
        let a = v(&mut s, 0);
        let b = v(&mut s, 1);
        let c = v(&mut s, 2);
        for (x, y) in [(a, b), (b, c), (a, c)] {
            s.add_clause(&[x, y]);
            s.add_clause(&[neg(x), neg(y)]);
        }
        assert!(s.solve().is_none());
    }

    /// Pigeonhole: `pigeons` pigeons into `holes` holes, one hole per pigeon.
    fn php(pigeons: usize, holes: usize) -> Option<Vec<bool>> {
        let mut s = Solver::new();
        let mut p = vec![vec![0 as Lit; holes]; pigeons];
        for i in 0..pigeons {
            for j in 0..holes {
                p[i][j] = v(&mut s, i * holes + j);
            }
            // Each pigeon sits in at least one hole.
            let row: Vec<Lit> = p[i].clone();
            s.add_clause(&row);
        }
        // No hole holds two pigeons.
        for j in 0..holes {
            for i1 in 0..pigeons {
                for i2 in i1 + 1..pigeons {
                    s.add_clause(&[neg(p[i1][j]), neg(p[i2][j])]);
                }
            }
        }
        s.solve()
    }

    #[test]
    fn pigeonhole_unsat_and_sat() {
        assert!(php(5, 4).is_none(), "5 pigeons in 4 holes is UNSAT");
        assert!(php(4, 4).is_some(), "4 pigeons in 4 holes is SAT");
    }

    /// Brute-force cross-check on small random 3-SAT instances.
    #[test]
    fn matches_brute_force_on_random_3sat() {
        // Deterministic xorshift so the test is reproducible.
        let mut rng: u64 = 0x1234_5678_9abc_def0;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for case in 0..30 {
            let n = 8 + (case % 5); // 8..12 variables
            let m = 3 * n + (case % 7);
            let mut s = Solver::new();
            let lits: Vec<Lit> = (0..n).map(|i| v(&mut s, i)).collect();
            let mut clauses: Vec<Vec<Lit>> = Vec::new();
            for _ in 0..m {
                let mut cl = Vec::new();
                for _ in 0..3 {
                    let l = lits[(next() as usize) % n];
                    cl.push(if next() & 1 == 1 { neg(l) } else { l });
                }
                s.add_clause(&cl);
                clauses.push(cl);
            }
            let got = s.solve().is_some();
            // Brute force over all 2^n assignments.
            let mut want = false;
            for mask in 0..(1u64 << n) {
                let sat = clauses.iter().all(|cl| {
                    cl.iter().any(|&l| {
                        let val = (mask >> var_of(l)) & 1 == 1;
                        val != is_neg(l)
                    })
                });
                if sat {
                    want = true;
                    break;
                }
            }
            assert_eq!(got, want, "case {} n={} m={}", case, n, m);
        }
    }

    #[test]
    fn luby_sequence() {
        let seq: Vec<f64> = (1..=12).map(|i| luby(2.0, i)).collect();
        assert_eq!(
            seq,
            vec![1.0, 1.0, 2.0, 1.0, 1.0, 2.0, 4.0, 1.0, 1.0, 2.0, 1.0, 1.0]
        );
    }

    #[test]
    fn model_satisfies_all_clauses() {
        let mut s = Solver::new();
        let a = v(&mut s, 0);
        let b = v(&mut s, 1);
        let c = v(&mut s, 2);
        let cls: Vec<Vec<Lit>> = vec![
            vec![a, b, c],
            vec![neg(a), b],
            vec![neg(b), neg(c)],
            vec![a, neg(c)],
        ];
        for cl in &cls {
            s.add_clause(cl);
        }
        let m = s.solve().expect("sat");
        for cl in &cls {
            assert!(
                cl.iter().any(|&l| m[var_of(l)] != is_neg(l)),
                "model violates {:?}",
                cl
            );
        }
    }
}
