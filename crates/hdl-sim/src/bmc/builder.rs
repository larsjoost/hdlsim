//! CNF builder with Tseitin gates for the BMC encoder. Pure move from
//! `bmc.rs`.

use crate::sat::{lit_of, neg, Lit, Solver};

/// CNF builder: a [`Solver`] plus gate helpers. Bit vectors are `Vec<Lit>`,
/// LSB first. A reserved variable encodes the constants.
pub(super) struct Builder {
    /// The underlying solver; `prove` reads it to run the final solve.
    pub(super) s: Solver,
    tlit: Lit,
}

impl Builder {
    pub(super) fn new() -> Builder {
        let mut s = Solver::new();
        let t = lit_of(s.new_var(), false);
        s.add_clause(&[t]);
        Builder { s, tlit: t }
    }
    pub(super) fn fresh(&mut self) -> Lit {
        lit_of(self.s.new_var(), false)
    }
    pub(super) fn true_lit(&self) -> Lit {
        self.tlit
    }
    pub(super) fn false_lit(&self) -> Lit {
        neg(self.tlit)
    }
    pub(super) fn const_lit(&self, b: bool) -> Lit {
        if b {
            self.tlit
        } else {
            neg(self.tlit)
        }
    }
    pub(super) fn clause(&mut self, lits: &[Lit]) {
        self.s.add_clause(lits);
    }
    pub(super) fn and_lit(&mut self, a: Lit, b: Lit) -> Lit {
        if a == self.false_lit() || b == self.false_lit() {
            return self.false_lit();
        }
        if a == self.true_lit() {
            return b;
        }
        if b == self.true_lit() {
            return a;
        }
        if a == b {
            return a;
        }
        let o = self.fresh();
        self.clause(&[neg(o), a]);
        self.clause(&[neg(o), b]);
        self.clause(&[o, neg(a), neg(b)]);
        o
    }
    pub(super) fn or_lit(&mut self, a: Lit, b: Lit) -> Lit {
        if a == self.true_lit() || b == self.true_lit() {
            return self.true_lit();
        }
        if a == self.false_lit() {
            return b;
        }
        if b == self.false_lit() {
            return a;
        }
        if a == b {
            return a;
        }
        let o = self.fresh();
        self.clause(&[o, neg(a)]);
        self.clause(&[o, neg(b)]);
        self.clause(&[neg(o), a, b]);
        o
    }
    pub(super) fn xor_lit(&mut self, a: Lit, b: Lit) -> Lit {
        if a == self.false_lit() {
            return b;
        }
        if b == self.false_lit() {
            return a;
        }
        if a == self.true_lit() {
            return neg(b);
        }
        if b == self.true_lit() {
            return neg(a);
        }
        if a == b {
            return self.false_lit();
        }
        if a == neg(b) {
            return self.true_lit();
        }
        let o = self.fresh();
        self.clause(&[neg(o), a, b]);
        self.clause(&[neg(o), neg(a), neg(b)]);
        self.clause(&[o, neg(a), b]);
        self.clause(&[o, a, neg(b)]);
        o
    }
    /// out = c ? a : b
    pub(super) fn mux_lit(&mut self, c: Lit, a: Lit, b: Lit) -> Lit {
        if c == self.true_lit() {
            return a;
        }
        if c == self.false_lit() {
            return b;
        }
        if a == b {
            return a;
        }
        let o = self.fresh();
        self.clause(&[neg(c), neg(o), a]);
        self.clause(&[neg(c), o, neg(a)]);
        self.clause(&[c, neg(o), b]);
        self.clause(&[c, o, neg(b)]);
        o
    }
    pub(super) fn eq_lit(&mut self, a: Lit, b: Lit) -> Lit {
        if a == b {
            return self.true_lit();
        }
        if a == neg(b) {
            return self.false_lit();
        }
        neg(self.xor_lit(a, b))
    }
}
