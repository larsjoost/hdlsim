//!
//! Seeded RNG and a small deterministic constraint solver — the engine behind
//! reproducible stimulus (`--seed N` / do-file `seed N`) and the `$urandom`,
//! `$urandom_range`, and `$rand_solve` system functions.
//!
//! Pipeline position: support module of `hdl-sim`; the kernel (`sim`) owns the
//! RNG state (persisted in checkpoints), this module provides the algorithms.
//!
//! Invariants:
//! - xorshift64* only: std-only, deterministic for a fixed seed.
//! - `Rng::next_u32` is bit-identical to the kernel's `$random` value, so both
//!   draw from one shared stream per seed.
//! - `below`/`range` use rejection sampling: uniform, no modulo bias.
//! - Parsing and solving never panic on spec text; failures are `Err(String)`.
//! - The solver explores at most `max_attempts` candidate assignments and is
//!   deterministic for a fixed seed and budget.

use std::collections::HashMap;

/// Seed used when `--seed 0` / no seed is given (matches `Simulator`'s default).
pub const DEFAULT_SEED: u64 = 0x243F6A8885A308D3;

/// Default candidate budget for [`solve_spec`] (`$rand_solve`).
pub const DEFAULT_SOLVE_ATTEMPTS: u32 = 10_000;

/// Range domains up to this size are enumerated (shuffled); larger ranges are
/// sampled at up to `SAMPLE_MAX` random candidates per variable.
const ENUM_DOMAIN_MAX: u64 = 256;
const SAMPLE_MAX: usize = 64;

/// xorshift64* generator.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// New generator from a seed (`0` selects [`DEFAULT_SEED`]).
    pub fn new(seed: u64) -> Rng {
        Rng::from_state(seed)
    }

    /// Wrap a raw xorshift64* state (`0` selects [`DEFAULT_SEED`]).
    pub fn from_state(state: u64) -> Rng {
        Rng {
            state: if state == 0 { DEFAULT_SEED } else { state },
        }
    }

    /// Current raw state (for checkpointing).
    pub fn state(&self) -> u64 {
        self.state
    }

    /// Next raw xorshift64* value, advancing the state.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x
    }

    /// Next 32-bit value — bit-identical to the kernel's `$random` output.
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64().wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u32
    }

    /// Uniform value in `0..n` (`n <= 1` yields 0); rejection sampling.
    pub fn below(&mut self, n: u64) -> u64 {
        if n <= 1 {
            return 0;
        }
        let threshold = n.wrapping_neg() % n;
        loop {
            let r = self.next_u64();
            if r >= threshold {
                return r % n;
            }
        }
    }

    /// Uniform value in the inclusive range `lo..=hi`; `None` if `hi < lo`.
    pub fn range(&mut self, lo: i64, hi: i64) -> Option<i64> {
        if hi < lo {
            return None;
        }
        let span = hi as i128 - lo as i128 + 1; // in 1..=2^64
        let off = if span >= 1i128 << 64 {
            self.next_u64() as i128
        } else {
            self.below(span as u64) as i128
        };
        Some((lo as i128 + off) as i64)
    }

    /// True with probability `num/den` (`den == 0` is always false).
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        if den == 0 {
            return false;
        }
        if num >= den {
            return true;
        }
        self.below(den) < num
    }

    /// In-place Fisher-Yates shuffle.
    pub fn shuffle<T>(&mut self, xs: &mut [T]) {
        for i in (1..xs.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            xs.swap(i, j);
        }
    }

    /// Uniform random element of `xs`.
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> Option<&'a T> {
        if xs.is_empty() {
            None
        } else {
            let i = self.below(xs.len() as u64) as usize;
            Some(&xs[i])
        }
    }
}

/// Binary operator in a constraint expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Unary operator in a constraint expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CUnOp {
    Neg,
    Not,
}

/// A constraint expression over named integer variables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CExpr {
    Int(i64),
    Var(String),
    Bin {
        op: CBinOp,
        l: Box<CExpr>,
        r: Box<CExpr>,
    },
    Un {
        op: CUnOp,
        e: Box<CExpr>,
    },
}

impl CExpr {
    /// Variables referenced by this expression.
    pub fn vars(&self, out: &mut Vec<String>) {
        match self {
            CExpr::Int(_) => {}
            CExpr::Var(v) => out.push(v.clone()),
            CExpr::Bin { l, r, .. } => {
                l.vars(out);
                r.vars(out);
            }
            CExpr::Un { e, .. } => e.vars(out),
        }
    }

    /// Evaluate with the given bindings; `None` on an unbound variable or
    /// division by zero. Arithmetic wraps (never panics).
    pub fn eval(&self, env: &HashMap<String, i64>) -> Option<i64> {
        match self {
            CExpr::Int(v) => Some(*v),
            CExpr::Var(v) => env.get(v).copied(),
            CExpr::Un { op, e } => {
                let v = e.eval(env)?;
                Some(match op {
                    CUnOp::Neg => v.wrapping_neg(),
                    CUnOp::Not => (v == 0) as i64,
                })
            }
            CExpr::Bin { op, l, r } => {
                let a = l.eval(env)?;
                let b = r.eval(env)?;
                Some(match op {
                    CBinOp::Add => a.wrapping_add(b),
                    CBinOp::Sub => a.wrapping_sub(b),
                    CBinOp::Mul => a.wrapping_mul(b),
                    CBinOp::Div => {
                        if b == 0 {
                            return None;
                        }
                        a.wrapping_div(b)
                    }
                    CBinOp::Rem => {
                        if b == 0 {
                            return None;
                        }
                        a.wrapping_rem(b)
                    }
                    CBinOp::Eq => (a == b) as i64,
                    CBinOp::Ne => (a != b) as i64,
                    CBinOp::Lt => (a < b) as i64,
                    CBinOp::Le => (a <= b) as i64,
                    CBinOp::Gt => (a > b) as i64,
                    CBinOp::Ge => (a >= b) as i64,
                    CBinOp::And => ((a != 0) && (b != 0)) as i64,
                    CBinOp::Or => ((a != 0) || (b != 0)) as i64,
                })
            }
        }
    }
}

/// A `$rand_solve` variable domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Domain {
    /// Inclusive range `lo..=hi`.
    Range(i64, i64),
    /// Explicit set of values.
    Set(Vec<i64>),
}

/// A parsed constraint spec: variable domains plus constraints that must all
/// evaluate to nonzero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    pub domains: Vec<(String, Domain)>,
    pub constraints: Vec<CExpr>,
}

/// Parse a `$rand_solve` spec: `;`-separated clauses, each either a domain
/// (`name: lo..hi` or `name: v1, v2, ...`) or a constraint expression over the
/// declared variables.
pub fn parse_spec(text: &str) -> Result<Spec, String> {
    let mut p = Parser {
        b: text.as_bytes(),
        pos: 0,
    };
    let mut spec = Spec::default();
    loop {
        p.skip_ws();
        if p.eof() {
            break;
        }
        let save = p.pos;
        if let Some(name) = p.try_ident() {
            p.skip_ws();
            if p.peek() == Some(b':') {
                p.bump();
                let dom = p.parse_domain()?;
                if spec.domains.iter().any(|(n, _)| *n == name) {
                    return Err(format!("duplicate variable '{}'", name));
                }
                spec.domains.push((name, dom));
            } else {
                p.pos = save;
                spec.constraints.push(p.parse_or()?);
            }
        } else {
            spec.constraints.push(p.parse_or()?);
        }
        p.skip_ws();
        if p.peek() == Some(b';') {
            p.bump();
        } else if !p.eof() {
            return Err(format!("expected ';' at byte {}", p.pos));
        }
    }
    Ok(spec)
}

struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn eof(&self) -> bool {
        self.pos >= self.b.len()
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.b.get(self.pos + 1).copied()
    }

    fn bump(&mut self) {
        self.pos += 1;
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.bump();
        }
    }

    fn try_ident(&mut self) -> Option<String> {
        let start = self.pos;
        if !matches!(self.peek(), Some(c) if c.is_ascii_alphabetic() || c == b'_') {
            return None;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
            self.bump();
        }
        Some(String::from_utf8_lossy(&self.b[start..self.pos]).into_owned())
    }

    fn parse_int(&mut self) -> Result<i64, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        if start == self.pos {
            return Err(format!("expected integer at byte {}", self.pos));
        }
        let s = std::str::from_utf8(&self.b[start..self.pos])
            .map_err(|_| format!("invalid integer at byte {}", start))?;
        s.parse::<i64>()
            .map_err(|_| format!("integer literal out of range at byte {}", start))
    }

    /// Domain body: `lo..hi` or `v1, v2, ...`.
    fn parse_domain(&mut self) -> Result<Domain, String> {
        self.skip_ws();
        let first = self.parse_int()?;
        self.skip_ws();
        if self.peek() == Some(b'.') && self.peek2() == Some(b'.') {
            self.bump();
            self.bump();
            self.skip_ws();
            let hi = self.parse_int()?;
            Ok(Domain::Range(first, hi))
        } else {
            let mut set = vec![first];
            loop {
                self.skip_ws();
                if self.peek() == Some(b',') {
                    self.bump();
                    self.skip_ws();
                    set.push(self.parse_int()?);
                } else {
                    break;
                }
            }
            Ok(Domain::Set(set))
        }
    }

    fn parse_or(&mut self) -> Result<CExpr, String> {
        let mut l = self.parse_and()?;
        loop {
            self.skip_ws();
            if self.peek() == Some(b'|') && self.peek2() == Some(b'|') {
                self.bump();
                self.bump();
                let r = self.parse_and()?;
                l = CExpr::Bin {
                    op: CBinOp::Or,
                    l: Box::new(l),
                    r: Box::new(r),
                };
            } else {
                return Ok(l);
            }
        }
    }

    fn parse_and(&mut self) -> Result<CExpr, String> {
        let mut l = self.parse_cmp()?;
        loop {
            self.skip_ws();
            if self.peek() == Some(b'&') && self.peek2() == Some(b'&') {
                self.bump();
                self.bump();
                let r = self.parse_cmp()?;
                l = CExpr::Bin {
                    op: CBinOp::And,
                    l: Box::new(l),
                    r: Box::new(r),
                };
            } else {
                return Ok(l);
            }
        }
    }

    fn parse_cmp(&mut self) -> Result<CExpr, String> {
        let l = self.parse_add()?;
        self.skip_ws();
        let op = match (self.peek(), self.peek2()) {
            (Some(b'='), Some(b'=')) => Some(CBinOp::Eq),
            (Some(b'!'), Some(b'=')) => Some(CBinOp::Ne),
            (Some(b'<'), Some(b'=')) => Some(CBinOp::Le),
            (Some(b'>'), Some(b'=')) => Some(CBinOp::Ge),
            (Some(b'<'), _) => Some(CBinOp::Lt),
            (Some(b'>'), _) => Some(CBinOp::Gt),
            _ => None,
        };
        match op {
            Some(op) => {
                self.bump();
                if matches!(op, CBinOp::Eq | CBinOp::Ne | CBinOp::Le | CBinOp::Ge) {
                    self.bump();
                }
                let r = self.parse_add()?;
                Ok(CExpr::Bin {
                    op,
                    l: Box::new(l),
                    r: Box::new(r),
                })
            }
            None => Ok(l),
        }
    }

    fn parse_add(&mut self) -> Result<CExpr, String> {
        let mut l = self.parse_mul()?;
        loop {
            self.skip_ws();
            let op = match self.peek() {
                Some(b'+') => CBinOp::Add,
                Some(b'-') => CBinOp::Sub,
                _ => return Ok(l),
            };
            self.bump();
            let r = self.parse_mul()?;
            l = CExpr::Bin {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
    }

    fn parse_mul(&mut self) -> Result<CExpr, String> {
        let mut l = self.parse_unary()?;
        loop {
            self.skip_ws();
            let op = match self.peek() {
                Some(b'*') => CBinOp::Mul,
                Some(b'/') => CBinOp::Div,
                Some(b'%') => CBinOp::Rem,
                _ => return Ok(l),
            };
            self.bump();
            let r = self.parse_unary()?;
            l = CExpr::Bin {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
    }

    fn parse_unary(&mut self) -> Result<CExpr, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'-') => {
                self.bump();
                let e = self.parse_unary()?;
                Ok(CExpr::Un {
                    op: CUnOp::Neg,
                    e: Box::new(e),
                })
            }
            Some(b'!') => {
                self.bump();
                let e = self.parse_unary()?;
                Ok(CExpr::Un {
                    op: CUnOp::Not,
                    e: Box::new(e),
                })
            }
            _ => self.parse_prim(),
        }
    }

    fn parse_prim(&mut self) -> Result<CExpr, String> {
        self.skip_ws();
        match self.peek() {
            Some(c) if c.is_ascii_digit() => Ok(CExpr::Int(self.parse_int()?)),
            Some(b'(') => {
                self.bump();
                let e = self.parse_or()?;
                self.skip_ws();
                if self.peek() == Some(b')') {
                    self.bump();
                    Ok(e)
                } else {
                    Err(format!("expected ')' at byte {}", self.pos))
                }
            }
            _ => match self.try_ident() {
                Some(name) => Ok(CExpr::Var(name)),
                None => Err(format!("expected expression at byte {}", self.pos)),
            },
        }
    }
}

/// Solve `spec` with `rng`: returns the variable bindings in declaration
/// order, or `None` when no solution is found within `max_attempts`
/// candidate assignments. Deterministic for a fixed seed and budget.
pub fn solve(rng: &mut Rng, spec: &Spec, max_attempts: u32) -> Option<Vec<(String, i64)>> {
    let mut s = Solver {
        rng,
        spec,
        attempts: 0,
        max_attempts,
    };
    let mut env: Vec<(String, i64)> = Vec::with_capacity(spec.domains.len());
    if s.assign(0, &mut env) {
        Some(env)
    } else {
        None
    }
}

/// Parse `text` and solve it: `Ok(bindings)` in declaration order, or `Err`
/// with a human-readable reason (parse error / budget exhausted).
pub fn solve_spec(
    rng: &mut Rng,
    text: &str,
    max_attempts: u32,
) -> Result<Vec<(String, i64)>, String> {
    let spec = parse_spec(text)?;
    match solve(rng, &spec, max_attempts) {
        Some(b) => Ok(b),
        None => Err(format!(
            "no solution found within {} attempts",
            max_attempts
        )),
    }
}

struct Solver<'a> {
    rng: &'a mut Rng,
    spec: &'a Spec,
    attempts: u32,
    max_attempts: u32,
}

impl<'a> Solver<'a> {
    fn assign(&mut self, idx: usize, env: &mut Vec<(String, i64)>) -> bool {
        if idx == self.spec.domains.len() {
            return true;
        }
        let (name, domain) = &self.spec.domains[idx];
        for v in self.candidates(domain) {
            if self.attempts >= self.max_attempts {
                return false;
            }
            self.attempts += 1;
            env.push((name.clone(), v));
            if self.consistent(env) && self.assign(idx + 1, env) {
                return true;
            }
            env.pop();
        }
        false
    }

    /// Forward check: every constraint whose variables are all bound must
    /// evaluate to nonzero (division by zero with all vars bound fails too).
    fn consistent(&self, env: &[(String, i64)]) -> bool {
        let map: HashMap<String, i64> = env.iter().map(|(n, v)| (n.clone(), *v)).collect();
        self.spec.constraints.iter().all(|c| {
            let mut vars = Vec::new();
            c.vars(&mut vars);
            if vars.iter().all(|v| map.contains_key(v)) {
                matches!(c.eval(&map), Some(x) if x != 0)
            } else {
                true
            }
        })
    }

    /// Candidate values for a domain: shuffled enumeration for small domains,
    /// random samples for large ones.
    fn candidates(&mut self, domain: &Domain) -> Vec<i64> {
        match domain {
            Domain::Set(vs) => {
                let mut v = vs.clone();
                self.rng.shuffle(&mut v);
                v
            }
            Domain::Range(lo, hi) => {
                let span = (*hi as i128 - *lo as i128 + 1).max(0);
                if span == 0 {
                    return Vec::new();
                }
                if span <= ENUM_DOMAIN_MAX as i128 {
                    let mut v: Vec<i64> = (*lo..=*hi).collect();
                    self.rng.shuffle(&mut v);
                    v
                } else {
                    let mut v = Vec::new();
                    while v.len() < SAMPLE_MAX {
                        let x = self.rng.range(*lo, *hi).unwrap_or(*lo);
                        if !v.contains(&x) {
                            v.push(x);
                        }
                    }
                    v
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(bindings: &[(String, i64)]) -> HashMap<String, i64> {
        bindings.iter().cloned().collect()
    }

    #[test]
    fn rng_is_deterministic_per_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
        let mut c = Rng::new(7);
        assert_ne!(a.next_u32(), c.next_u32());
    }

    #[test]
    fn next_u32_matches_kernel_random() {
        // The kernel's `$random` is (xorshift64*(x) * 0x2545F4914F6CDD1D) >> 32.
        let mut r = Rng::from_state(DEFAULT_SEED);
        let mut x = DEFAULT_SEED;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let expect = (x.wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u32;
        assert_eq!(r.next_u32(), expect);
        assert_eq!(r.state(), x);
    }

    #[test]
    fn below_is_bounded_and_roughly_uniform() {
        let mut r = Rng::new(1);
        let mut counts = [0u32; 10];
        for _ in 0..10_000 {
            let v = r.below(10);
            assert!(v < 10);
            counts[v as usize] += 1;
        }
        for c in counts {
            assert!(c > 500, "bucket count {} too small", c);
        }
    }

    #[test]
    fn range_is_inclusive_and_bounded() {
        let mut r = Rng::new(9);
        for _ in 0..1_000 {
            let v = r.range(5, 9).unwrap();
            assert!((5..=9).contains(&v));
        }
        assert!(r.range(9, 5).is_none());
        assert!(r.range(i64::MIN, i64::MAX).is_some());
    }

    #[test]
    fn chance_edges() {
        let mut r = Rng::new(3);
        assert!(!r.chance(0, 10));
        assert!(r.chance(10, 10));
        assert!(!r.chance(1, 0));
    }

    #[test]
    fn shuffle_is_a_deterministic_permutation() {
        let mut r = Rng::new(5);
        let mut v: Vec<i64> = (0..20).collect();
        r.shuffle(&mut v);
        let mut sorted = v.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
        let mut r2 = Rng::new(5);
        let mut v2: Vec<i64> = (0..20).collect();
        r2.shuffle(&mut v2);
        assert_eq!(v, v2);
    }

    #[test]
    fn parses_domains_and_constraints() {
        let s = parse_spec("a: 0..10; b: 1, 2, 4; a + b == 10 && a != b;").unwrap();
        assert_eq!(s.domains.len(), 2);
        assert_eq!(s.domains[0], ("a".to_string(), Domain::Range(0, 10)));
        assert_eq!(s.domains[1], ("b".to_string(), Domain::Set(vec![1, 2, 4])));
        assert_eq!(s.constraints.len(), 1);
        assert!(parse_spec("a: 1..2; a ==").is_err());
        assert!(parse_spec("a: 1..2; 1 ==").is_err());
        assert!(parse_spec("a: 1..1; a: 2..2").is_err());
        assert!(parse_spec("a: x..2").is_err());
    }

    #[test]
    fn expr_eval_precedence_and_errors() {
        let env = HashMap::from([("a".to_string(), 3i64), ("b".to_string(), 4i64)]);
        let e = parse_spec("a + b * 2 == 11").unwrap().constraints[0].clone();
        assert_eq!(e.eval(&env), Some(1));
        let e2 = parse_spec("!(a == 3) || b / 0 != 0").unwrap().constraints[0].clone();
        assert_eq!(e2.eval(&env), None); // division by zero -> undecidable
        let e3 = parse_spec("(a - b) < 0").unwrap().constraints[0].clone();
        assert_eq!(e3.eval(&env), Some(1));
        let unbound = HashMap::new();
        assert_eq!(e3.eval(&unbound), None);
    }

    #[test]
    fn solve_satisfies_constraints_deterministically() {
        let spec = parse_spec("a: 0..10; b: 0..10; a + b == 10; a != b").unwrap();
        let s1 = solve(&mut Rng::new(42), &spec, DEFAULT_SOLVE_ATTEMPTS).unwrap();
        let s2 = solve(&mut Rng::new(42), &spec, DEFAULT_SOLVE_ATTEMPTS).unwrap();
        assert_eq!(s1, s2);
        let env = env_of(&s1);
        for c in &spec.constraints {
            assert_eq!(c.eval(&env), Some(1));
        }
    }

    #[test]
    fn solve_set_domain_and_unsat() {
        let spec = parse_spec("op: 1, 2, 4, 8; op != 2").unwrap();
        let s = solve(&mut Rng::new(1), &spec, DEFAULT_SOLVE_ATTEMPTS).unwrap();
        assert!(matches!(s[0].1, 1 | 4 | 8));
        let bad = parse_spec("a: 1..1; a == 2").unwrap();
        assert!(solve(&mut Rng::new(1), &bad, DEFAULT_SOLVE_ATTEMPTS).is_none());
    }

    #[test]
    fn solve_respects_budget() {
        let spec = parse_spec("a: 0..1000; b: 0..1000; a + b == 1999").unwrap();
        assert!(solve(&mut Rng::new(1), &spec, 2).is_none());
    }

    #[test]
    fn solve_spec_reports_errors() {
        assert!(solve_spec(&mut Rng::new(1), "a: 1..1; a == 2", 100).is_err());
        assert!(solve_spec(&mut Rng::new(1), "a: x..2", 100).is_err());
        let ok = solve_spec(&mut Rng::new(1), "a: 0..5", 100).unwrap();
        assert_eq!(ok.len(), 1);
        assert!((0..=5).contains(&ok[0].1));
    }
}
