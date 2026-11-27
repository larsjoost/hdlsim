//! Loop constructs and wait statements: for/while/bare loops (with the
//! flat-slot range bounds used by function-formal loops), the For IR
//! assembly helper, and wait until/on/for lowering with its edge and
//! timeout analysis.
//!
//! Extracted from seq.rs (pure move, no logic changes).

use super::*;

/// The clauses of a `wait` statement: `until cond`, `on sens`, and
/// `for time` (a physical literal in fs, or a lowered expression).
#[derive(Default)]
struct WaitClauses {
    cond: Option<Expr>,
    sens: Option<SyntaxNode>,
    time_fs: Option<u64>,
    time_expr: Option<Expr>,
}

impl<'a> Lower<'a> {
    /// `name'range` bounds for a flat value slot (function formal, unsized
    /// signal): `0 to width-1`, resolved from the bound value at evaluation
    /// time via `SigAttr`. Only applies when the slot is not memory-backed
    /// and has no static dims — real array signals keep their static bounds.
    pub(super) fn flat_slot_range(&self, id: SigId) -> Option<(Expr, Expr)> {
        if self.mems.contains_key(&id) || self.sig_dims.contains_key(&id) {
            return None;
        }
        // The loop variable walks ELEMENTS, so the high bound is the bound
        // value's bit width divided by the element width, minus one — a
        // 64-bit-real formal's 'range yields count-1, not bit-1 (a bit-index
        // bound overran multi-bit-element formals by the element factor).
        // 1-bit elements keep the plain width-1 bound.
        let ew = self.sig_elem.get(&id).copied().unwrap_or(1).max(1) as u64;
        let count = if ew == 1 {
            Expr::SigAttr {
                sig: id,
                attr: "length",
            }
        } else {
            Expr::Bin {
                op: BinOp::Div,
                l: Box::new(Expr::SigAttr {
                    sig: id,
                    attr: "length",
                }),
                r: Box::new(Expr::Lit(Value::from_u64(32, ew))),
            }
        };
        Some((
            Expr::SigAttr {
                sig: id,
                attr: "low",
            },
            Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(count),
                r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
            },
        ))
    }

    /// A 32-bit zero literal — the degenerate `for` range bound.
    fn zero_lit(&self) -> Expr {
        Expr::Lit(Value::from_u64(32, 0))
    }

    /// VHDL for-loop variables are implicitly declared by the loop: reuse
    /// the slot if the name is already bound, else allocate a fresh 32-bit
    /// wire named `__for_<var>`.
    fn declare_for_var(&mut self, var: &str) -> SigId {
        match self.sig_id(var) {
            Some(id) => id,
            None => {
                let id = (self.extra_base + self.extra_signals.len()) as SigId;
                self.extra_signals.push(SignalIr {
                    name: format!("__for_{}", var),
                    width: 32,
                    kind: SigKind::Wire,
                    init: None,
                    mem: None,
                    // VHDL loop variables are integers: a signed slot keeps
                    // null ranges null (`for i in 0 to -1` / `-1 downto 0`
                    // must run zero iterations — unsigned, the -1 bound
                    // reads 4294967295 and the loop guard stays true for
                    // ~2^31 iterations).
                    signed: true,
                });
                self.names.insert(var.to_string(), id);
                id
            }
        }
    }

    pub(super) fn lower_for(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // for <var> in <range> loop <body> end loop
        let var = kids
            .iter()
            .find(|k| {
                k.kind() == SyntaxKind::Ident
                    && self.kw(k) != "for"
                    && self.kw(k) != "in"
                    && self.kw(k) != "loop"
                    && self.kw(k) != "end"
            })
            .map(|k| self.text(k));
        let range = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range || k.kind() == SyntaxKind::Attr);
        let Some(var) = var else {
            self.err(s.span(), "malformed for loop");
            return Stmt::Null;
        };
        let var_id = self.declare_for_var(&var);
        let (lo, hi, downto) = self.for_range_bounds(range);
        let body = self.lower_seq_block(s);
        self.make_for(var_id, lo, hi, downto, body)
    }

    /// Bounds of a `for` loop's range: a discrete range (`a to b` /
    /// `a downto b`), `s'range` / `s'reverse_range` (bounds from the named
    /// object's dims, its type's dims, or its memory depth), or a bare
    /// `name'range` attribute. Unresolvable ranges yield (0, 0) — the
    /// loop body never executes.
    fn for_range_bounds(&mut self, range: Option<&SyntaxNode>) -> (Expr, Expr, bool) {
        match range.map(|r| r.kind()) {
            Some(SyntaxKind::Range) => {
                let rk = real_children(range.expect("matched on range's kind, so range is Some"));
                // `for i in s'range loop` — the range is a Range node
                // holding an Attr child; resolve the bounds from the
                // signal's dims.
                if let Some(at) = rk.iter().find(|k| k.kind() == SyntaxKind::Attr) {
                    let ak = real_children(at);
                    let attr = ak
                        .iter()
                        .rfind(|k| k.kind() == SyntaxKind::Ident)
                        .map(|k| self.kw(k))
                        .unwrap_or_default();
                    if attr == "range" || attr == "reverse_range" {
                        if let Some(b) = ak.iter().find(|k| k.kind() == SyntaxKind::Name) {
                            if let Some(bounds) =
                                self.for_range_attr_bounds(&self.text(b), attr == "reverse_range")
                            {
                                return bounds;
                            }
                        }
                    }
                }
                self.for_discrete_range(&rk)
            }
            Some(SyntaxKind::Attr) => {
                // `name'range` on an array signal: 0 to depth-1.
                let ak = real_children(range.expect("matched on range's kind, so range is Some"));
                let base = ak.iter().find(|k| k.kind() == SyntaxKind::Name);
                base.and_then(|b| self.for_bare_attr_bounds(&self.text(b)))
                    .unwrap_or((self.zero_lit(), self.zero_lit(), false))
            }
            _ => (self.zero_lit(), self.zero_lit(), false),
        }
    }

    /// Bounds of a plain discrete range `a to b` / `a downto b`: the first
    /// two expression children with the direction ident; a bare direction
    /// defaults to `to`, missing bounds yield (0, 0).
    fn for_discrete_range(&mut self, rk: &[SyntaxNode]) -> (Expr, Expr, bool) {
        let exprs: Vec<&SyntaxNode> = rk.iter().filter(|k| is_expr_node(k.kind())).collect();
        let dir = rk
            .iter()
            .find(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .unwrap_or_else(|| "to".to_string());
        let a = exprs
            .first()
            .map(|k| self.lower_expr(k, &self.consts.clone()));
        let b = exprs
            .get(1)
            .map(|k| self.lower_expr(k, &self.consts.clone()));
        match (a, b) {
            (Some(a), Some(b)) => {
                // (start, end, downto) — `make_for`'s convention: the loop
                // starts at the left bound and, for `downto`, counts down
                // while `i >= end` (`for_range_attr_bounds` returns the
                // same shape for `reverse_range`).
                if dir == "downto" {
                    (a, b, true)
                } else {
                    (a, b, false)
                }
            }
            _ => (self.zero_lit(), self.zero_lit(), false),
        }
    }

    /// Bounds of `name'range` / `name'reverse_range` inside a discrete
    /// range: flat-slot runtime bounds, else the object's/type's dims,
    /// else the memory depth of a materialized array constant. `None`
    /// falls through to the discrete-range expression form.
    pub(super) fn for_range_attr_bounds(
        &self,
        bn: &str,
        reverse: bool,
    ) -> Option<(Expr, Expr, bool)> {
        // Flat slot (function formal, unsized signal): runtime bounds
        // from the bound value's width.
        if let Some((lo, hi)) = self.object_id(bn).and_then(|id| self.flat_slot_range(id)) {
            return Some((lo, hi, false));
        }
        let dims = self
            .object_id(bn)
            .and_then(|id| self.sig_dims.get(&id).cloned())
            .or_else(|| {
                self.types.get(&bn.to_ascii_lowercase()).and_then(|t| {
                    if let TypeInfo::Array { dims, .. } = t {
                        Some(dims.clone())
                    } else {
                        None
                    }
                })
            });
        if let Some(&(lo, n)) = dims.as_ref().and_then(|d| d.first()) {
            let hi = lo + n as i64 - 1;
            let (a, b, dt) = if reverse {
                (hi, lo, true)
            } else {
                (lo, hi, false)
            };
            return Some((
                Expr::Lit(Value::from_u64(32, a as u64)),
                Expr::Lit(Value::from_u64(32, b as u64)),
                dt,
            ));
        }
        // Memory-backed object without dims (materialized array constant
        // of an unconstrained type): the element count is the memory
        // depth; hidden consts pack element 0 at the top, so the range
        // is 0 to depth-1.
        if let Some(depth) = self
            .object_id(bn)
            .and_then(|id| self.mems.get(&id).copied())
            .filter(|&d| d > 0)
        {
            return Some((
                self.zero_lit(),
                Expr::Lit(Value::from_u64(32, depth as u64 - 1)),
                false,
            ));
        }
        None
    }

    /// Bounds of a bare `name'range` attribute on an array signal: the
    /// memory depth of a materialized array constant, else flat-slot
    /// runtime bounds. `None` degenerates to a never-executing loop.
    fn for_bare_attr_bounds(&self, bn: &str) -> Option<(Expr, Expr, bool)> {
        let id = self.object_id(bn)?;
        if let Some(depth) = self.mems.get(&id).copied() {
            return Some((
                self.zero_lit(),
                Expr::Lit(Value::from_u64(32, depth as u64 - 1)),
                false,
            ));
        }
        // Flat slot (function formal, unsized signal): the element count
        // comes from the bound value at runtime / const-eval (`SigAttr`).
        self.flat_slot_range(id).map(|(lo, hi)| (lo, hi, false))
    }

    pub(super) fn make_for(
        &mut self,
        var_id: SigId,
        lo: Expr,
        hi: Expr,
        downto: bool,
        body: Stmt,
    ) -> Stmt {
        let v = || Expr::Sig(var_id);
        let one = Expr::Lit(Value::from_u64(32, 1));
        let init = Stmt::Assign {
            lhs: v(),
            rhs: lo,
            nba: false,
        };
        let cond = Expr::Bin {
            op: if downto { BinOp::Ge } else { BinOp::Le },
            // The loop variable is an integer (LRM 1076 §8.9): mark the
            // comparison operands so the guard sign-extends — an unsigned
            // slot read would turn a null range (`0 to -1`, `-1 downto 0`)
            // into ~2^31 iterations against the raw 0xFFFFFFFF bound. The
            // init/step targets stay raw `Sig` writes.
            l: Box::new(Expr::Signed(Box::new(v()))),
            r: Box::new(Expr::Signed(Box::new(hi))),
        };
        let step = Stmt::Assign {
            lhs: v(),
            rhs: Expr::Bin {
                op: if downto { BinOp::Sub } else { BinOp::Add },
                l: Box::new(v()),
                r: Box::new(one),
            },
            nba: false,
        };
        Stmt::For {
            init: Box::new(init),
            cond,
            step: Box::new(step),
            body: Box::new(body),
        }
    }

    pub(super) fn lower_loop(&mut self, s: &SyntaxNode) -> Stmt {
        // `while cond loop body end loop` / `loop body end loop`
        let kids = real_children(s);
        let cond = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &self.consts.clone()))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
        let body = self.lower_seq_block(s);
        Stmt::For {
            init: Box::new(Stmt::Null),
            cond,
            step: Box::new(Stmt::Null),
            body: Box::new(body),
        }
    }

    /// Scan a `wait` statement's children for its clauses; later clauses
    /// of the same kind overwrite earlier ones, as before.
    fn wait_clauses(&mut self, kids: &[SyntaxNode]) -> WaitClauses {
        let mut cl = WaitClauses::default();
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if k.kind() == SyntaxKind::Ident {
                match self.kw(k).as_str() {
                    "until" => {
                        if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                            cl.cond = Some(self.lower_expr(e, &self.consts.clone()));
                            i += 1;
                        }
                    }
                    "on" => {
                        cl.sens = kids.get(i + 1).cloned();
                        i += 1;
                    }
                    "for" => {
                        let (fs, te, skip) = self.wait_for_clause(kids, i);
                        if fs.is_some() {
                            cl.time_fs = fs;
                        }
                        if te.is_some() {
                            cl.time_expr = te;
                        }
                        i += skip;
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        cl
    }

    /// A `for T` clause: a physical literal (`10 ns`, merged into one
    /// Literal token or with the unit as a separate Ident sibling) yields
    /// fs; any other expression lowers to a time expression — evaluated
    /// when the wait executes. Returns the parsed fs, the lowered
    /// expression, and the consumed-sibling count.
    fn wait_for_clause(
        &mut self,
        kids: &[SyntaxNode],
        i: usize,
    ) -> (Option<u64>, Option<Expr>, usize) {
        let num = kids.get(i + 1);
        if let Some(n) = num.filter(|k| k.kind() == SyntaxKind::Literal) {
            let t = self.text(n);
            let (v, mult) = match t.split_once(char::is_whitespace) {
                Some((a, b)) => (self.wait_int_fs(a.trim()), time_unit_fs(b.trim())),
                None => {
                    let unit = kids.get(i + 2).filter(|k| k.kind() == SyntaxKind::Ident);
                    (
                        self.wait_int_fs(&t),
                        unit.map(|k| time_unit_fs(&self.kw(k))).unwrap_or(1_000_000),
                    )
                }
            };
            (Some(v.saturating_mul(mult)), None, 1)
        } else if let Some(e) = num.filter(|k| is_expr_node(k.kind())) {
            (None, Some(self.lower_expr(e, &self.consts.clone())), 1)
        } else {
            (None, None, 0)
        }
    }

    /// An integer wait-literal; unparseable text yields 0.
    fn wait_int_fs(&mut self, t: &str) -> u64 {
        self.parse_int_literal(t)
            .and_then(|v| v.to_u64())
            .unwrap_or(0)
    }

    /// Wake sources of a `wait until`/`wait on`: an explicit `on` list
    /// wins over the edges derived from the condition (LRM 8.1). A
    /// non-list `on` child or a condition with no analyzable edges
    /// yields `None`.
    fn wait_edges(&mut self, sens: Option<&SyntaxNode>, cond: Option<&Expr>) -> Option<Vec<Edge>> {
        let edges = sens.and_then(|sl| {
            if sl.kind() == SyntaxKind::SensList {
                Some(match self.lower_sens_list(sl) {
                    Sens::List(es) => es,
                    Sens::All => Vec::new(),
                })
            } else {
                None
            }
        });
        edges.or_else(|| cond.and_then(|c| edges_from_cond(c, self)))
    }

    /// A pure timed wait (`wait for T;` with no until/on clauses): a
    /// physical literal becomes a Delay, an expression a timeout-only
    /// WaitUntil; a bare `wait;` parks forever.
    fn wait_timed_only(&self, time_fs: Option<u64>, timeout: Option<Expr>) -> Stmt {
        if let Some(t) = time_fs {
            return Stmt::Delay {
                time_fs: t,
                stmt: Box::new(Stmt::Null),
            };
        }
        if let Some(te) = timeout {
            return Stmt::WaitUntil {
                cond: None,
                sens: Vec::new(),
                timeout: Some(te),
            };
        }
        // Bare `wait;` — park forever.
        Stmt::Wait {
            sens: Vec::new(),
            body: Box::new(Stmt::Null),
        }
    }

    pub(super) fn lower_wait(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // wait [until cond] [on sens] [for time];
        let cl = self.wait_clauses(&kids);
        // Timeout for `wait until`/`wait on`: literal fs, or the lowered
        // expression (LRM 8.1: the timeout is evaluated when the wait runs).
        let timeout = cl
            .time_expr
            .or_else(|| cl.time_fs.map(|fs| Expr::Lit(Value::from_u64(64, fs))));

        // `wait for T;` — pure timed wait: literal → Delay, expression →
        // timeout-only WaitUntil. No until/on clauses.
        if cl.cond.is_none() && cl.sens.is_none() {
            return self.wait_timed_only(cl.time_fs, timeout);
        }

        // Explicit `on` list wins over cond-derived edges (LRM 8.1).
        let edges = self.wait_edges(cl.sens.as_ref(), cl.cond.as_ref());

        // A pure `rising_edge(s)`/`falling_edge(s)` condition is fully encoded
        // by its edge kind — no re-check. Any other condition wakes on any
        // event of its signals and re-checks the full expression (slightly
        // over-sensitive vs the LRM, but never misses the true wake).
        let recheck = match &cl.cond {
            Some(Expr::Rising(inner)) | Some(Expr::Falling(inner)) => {
                !matches!(inner.as_ref(), Expr::Sig(_))
            }
            _ => true,
        };
        if edges.is_none() && timeout.is_none() {
            // No wake source at all: the process can never resume.
            self.warn(
                s.span(),
                "wait statement has no sensitivity, condition signals, or timeout — it can never resume",
            );
        }
        Stmt::WaitUntil {
            cond: if recheck { cl.cond } else { None },
            sens: edges.unwrap_or_default(),
            timeout,
        }
    }
}
