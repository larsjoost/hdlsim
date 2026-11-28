//! VHDL part-select and single-element bound remapping: `name(msb dir lsb)`
//! slices and `v(k)` element reads over memory-backed arrays, plain vectors
//! (ascending ranges, multi-bit elements, shape-bound formals), constants,
//! record-field arrays, and `$const_idx` element reads. Split out of
//! `expr.rs` to keep every function at its standards baseline.

use super::*;
use crate::vhdl_lower::width::{dyn_remapped_sel_bounds, remapped_sel_bounds};

impl<'a> Lower<'a> {
    pub(super) fn lower_part_select(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Expr {
        // name ( msb dir lsb ) — the base name is not a bound expr.
        // A selected-chain base (`c_arr(0).f(0)(0 TO 15)`) arrives as
        // a Call child instead of a bare Name: the first expression
        // child is then the base and the remaining ones the bounds.
        let kids = real_children(e);
        let (base, bound_nodes) = self.slice_base_bounds(&kids);
        if bound_nodes.len() < 2 {
            return Expr::Lit(Value::x(1));
        }
        let (dir, msb, lsb) = self.slice_dir_bounds(&kids, &bound_nodes);
        let base_e = match &base {
            Some(b) => self.lower_expr(b, env),
            None => Expr::Lit(Value::x(1)),
        };
        self.part_select_routes(&base, base_e, &msb, &lsb, &dir, env)
    }

    /// The slice base and bound expressions: a leading `Name` child is the
    /// base (the rest are bounds); otherwise the first of three or more
    /// expression children is the base of a selected-chain slice.
    fn slice_base_bounds(&self, kids: &[SyntaxNode]) -> (Option<SyntaxNode>, Vec<SyntaxNode>) {
        let name_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        match name_pos {
            Some(np) => (
                Some(kids[np].clone()),
                kids.iter()
                    .enumerate()
                    .filter(|(i, k)| Some(*i) != Some(np) && is_expr_node(k.kind()))
                    .map(|(_, k)| k.clone())
                    .collect(),
            ),
            None => {
                let exprs: Vec<SyntaxNode> = kids
                    .iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .cloned()
                    .collect();
                match exprs.len() {
                    n if n >= 3 => (Some(exprs[0].clone()), exprs[1..].to_vec()),
                    _ => (None, exprs),
                }
            }
        }
    }

    /// The range direction keyword and the (msb, lsb) bound pair,
    /// normalized so `downto` keeps the source order.
    fn slice_dir_bounds(
        &self,
        kids: &[SyntaxNode],
        bound_nodes: &[SyntaxNode],
    ) -> (String, SyntaxNode, SyntaxNode) {
        let dir = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .unwrap_or_else(|| "downto".to_string());
        let (msb, lsb) = if dir == "downto" {
            (bound_nodes[0].clone(), bound_nodes[1].clone())
        } else {
            (bound_nodes[1].clone(), bound_nodes[0].clone())
        };
        (dir, msb, lsb)
    }

    /// The slice routes, in priority order: memory-backed element slice,
    /// plain-vector element addressing, constant slice, record-field array
    /// slice, constant-element slice, then the raw bound lowering.
    fn part_select_routes(
        &mut self,
        base: &Option<SyntaxNode>,
        base_e: Expr,
        msb: &SyntaxNode,
        lsb: &SyntaxNode,
        dir: &str,
        env: &HashMap<String, Value>,
    ) -> Expr {
        if let Some(sel) = self.mem_elem_slice(&base_e, msb, lsb, env) {
            return sel;
        }
        // Element addressing on a plain (non-memory) vector: the
        // bounds are element positions, so they must remap for an
        // ascending range (element k = value bit W-1-(k-lo)·ew —
        // element 0 is the most significant, mirroring the
        // memory-backed mapping above) and scale by the element
        // width (`string` characters are 8 bits). A descending
        // 1-bit range from 0 addresses value bits directly and
        // keeps the raw lowering.
        if let Some(sel) = self.vector_elem_slice(&base_e, msb, lsb, env) {
            return sel;
        }
        // Slice of a constant (`ASC_DATA(0 to 3)`): remap the index
        // bounds to value-bit positions from the recorded decl
        // direction, like the signal path above.
        if let Some(sel) = self.const_slice_sel(base.as_ref(), &base_e, msb, lsb, env) {
            return sel;
        }
        if let Some(sel) = self.field_array_slice(base, &base_e, msb, lsb, env) {
            return sel;
        }
        if let Some(sel) = self.const_idx_slice(&base_e, msb, lsb, dir, env) {
            return sel;
        }
        Expr::Sel {
            e: Box::new(base_e),
            msb: Box::new(self.lower_expr(msb, env)),
            lsb: Box::new(self.lower_expr(lsb, env)),
        }
    }

    /// Element slice on a memory-backed array (`int_sig(0 to
    /// DELAY-2)`): the kernel reads element k as word k, packed
    /// MSB-first (element 0 = most significant bits), so the
    /// element bounds must map to that bit range — using them as
    /// flat bit indices selects from the wrong end and a shift
    /// register packs X into its first word forever. Element e
    /// occupies bits [total-1-e·w … total-(e+1)·w]; `sig_width`
    /// holds the element width, `mems` the depth.
    fn mem_elem_slice(
        &mut self,
        base_e: &Expr,
        msb: &SyntaxNode,
        lsb: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let Expr::Sig(sid) = base_e else {
            return None;
        };
        let (Some(&depth), Some(&ew)) = (self.mems.get(sid), self.sig_width.get(sid)) else {
            return None;
        };
        // `lsb`/`msb` hold the source range's low/high bounds
        // in both directions (normalized by the caller), and the word
        // formulas below want (low element, high element):
        // element e occupies bits [total-1-e·w … total-(e+1)·w],
        // so the slice spans element `lo`'s top bit down to
        // element `hi`'s bottom bit regardless of direction.
        let (e_lo, e_hi) = (lsb.clone(), msb.clone());
        let total = depth.saturating_mul(ew).max(1);
        let e_lo = self.lower_expr(&e_lo, env);
        let e_hi = self.lower_expr(&e_hi, env);
        let word = |base: u64, e: Expr| Expr::Bin {
            op: BinOp::Sub,
            l: Box::new(Expr::Lit(Value::from_u64(32, base))),
            r: Box::new(Expr::Bin {
                op: BinOp::Mul,
                l: Box::new(e),
                r: Box::new(Expr::Lit(Value::from_u64(32, ew as u64))),
            }),
        };
        Some(Expr::Sel {
            e: Box::new(base_e.clone()),
            msb: Box::new(word(total as u64 - 1, e_lo)),
            lsb: Box::new(word(total as u64 - ew as u64, e_hi)),
        })
    }

    /// Element slice on a plain (non-memory) vector: remap the element
    /// bounds for shape-bound formals (dynamic) or recorded ranges
    /// (ascending / offset / multi-bit elements). `None` keeps the raw
    /// lowering.
    fn vector_elem_slice(
        &mut self,
        base_e: &Expr,
        msb: &SyntaxNode,
        lsb: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let Expr::Sig(sid) = base_e else {
            return None;
        };
        if self.mems.contains_key(sid) {
            return None;
        }
        // Dynamic first: a shape-bound formal addresses
        // through its hidden shape slots — the actual's low
        // bound and direction, bound per call — so the slice
        // lands at the actual's layout, whatever it is.
        if let Some(&(lo_id, asc_id)) = self.formal_dyn.get(sid) {
            let (msb_e, lsb_e) = dyn_remapped_sel_bounds(
                self.slot_width_expr(*sid),
                self.sig_elem.get(sid).copied().unwrap_or(1).max(1) as u64,
                Expr::Sig(lo_id),
                Expr::Sig(asc_id),
                self.lower_expr(msb, env),
                self.lower_expr(lsb, env),
            );
            return Some(Expr::Sel {
                e: Box::new(base_e.clone()),
                msb: Box::new(msb_e),
                lsb: Box::new(lsb_e),
            });
        }
        let (lo, asc) = self.sig_asc.get(sid).copied().unwrap_or((0, false));
        let ew = self.sig_elem.get(sid).copied().unwrap_or(1).max(1) as u64;
        if !(asc || lo != 0 || ew > 1) {
            return None;
        }
        let w = self.slot_width_expr(*sid);
        let lo_u = lo.max(0) as u64;
        // `msb`/`lsb` hold the source range's high/low
        // bounds; for `to` the left (MSB-most) element is
        // `lsb`, for `downto` it is `msb`.
        let (msb_e, lsb_e) = remapped_sel_bounds(
            w,
            ew,
            lo_u,
            asc,
            self.lower_expr(msb, env),
            self.lower_expr(lsb, env),
        );
        Some(Expr::Sel {
            e: Box::new(base_e.clone()),
            msb: Box::new(msb_e),
            lsb: Box::new(lsb_e),
        })
    }

    /// Slice of a record field that is itself an array
    /// (`r.options(0 to n)`, `r.description(1 to len)`): the
    /// bounds are element positions inside the field's packed
    /// value. Field-array elements address MSB-first (element 0
    /// = most significant bits, matching `$const_idx` reads), so
    /// the bounds remap like an ascending signal slice over a
    /// carrier of the field's bit width.
    fn field_array_slice(
        &mut self,
        base: &Option<SyntaxNode>,
        base_e: &Expr,
        msb: &SyntaxNode,
        lsb: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        if !matches!(base_e, Expr::Sel { .. }) {
            return None;
        }
        let (flo, few, fw) = base
            .as_ref()
            .and_then(|b| self.field_array_shape(&self.text(b)))?;
        let w_e = Expr::Lit(Value::from_u64(32, fw));
        let lo_e = Expr::Lit(Value::from_u64(32, flo.max(0) as u64));
        let off_left = Expr::Bin {
            op: BinOp::Sub,
            l: Box::new(self.lower_expr(lsb, env)),
            r: Box::new(lo_e.clone()),
        };
        let off_right = Expr::Bin {
            op: BinOp::Sub,
            l: Box::new(self.lower_expr(msb, env)),
            r: Box::new(lo_e),
        };
        let scaled = |e: Expr, k: u64| Expr::Bin {
            op: BinOp::Mul,
            l: Box::new(e),
            r: Box::new(Expr::Lit(Value::from_u64(32, k))),
        };
        Some(Expr::Sel {
            e: Box::new(base_e.clone()),
            msb: Box::new(Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(Expr::Bin {
                    op: BinOp::Sub,
                    l: Box::new(w_e),
                    r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                }),
                r: Box::new(scaled(off_left, few)),
            }),
            lsb: Box::new(Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(Expr::Lit(Value::from_u64(32, fw))),
                r: Box::new(scaled(
                    Expr::Bin {
                        op: BinOp::Add,
                        l: Box::new(off_right),
                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                    },
                    few,
                )),
            }),
        })
    }

    /// Slice of a constant-element read (`C_PKT(0)(0 TO 15)` —
    /// the base lowers to `$const_idx` over the constant's flat
    /// value). Element k packs MSB-first (element 0 = most
    /// significant bits, the same rule `$const_idx` applies), so
    /// an ascending slice's bounds remap to value-bit positions
    /// over a carrier of the element width. Descending slices
    /// keep the raw bounds (the flat default mapping).
    fn const_idx_slice(
        &mut self,
        base_e: &Expr,
        msb: &SyntaxNode,
        lsb: &SyntaxNode,
        dir: &str,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let Expr::Call { name, args, .. } = base_e else {
            return None;
        };
        if name != "$const_idx" || dir != "to" {
            return None;
        }
        let Expr::Lit(ewv) = args.get(2)? else {
            return None;
        };
        let ew = ewv.to_u64().unwrap_or(1).max(1);
        let (msb_e, lsb_e) = remapped_sel_bounds(
            Expr::Lit(Value::from_u64(32, ew)),
            1,
            0,
            true,
            self.lower_expr(msb, env),
            self.lower_expr(lsb, env),
        );
        Some(Expr::Sel {
            e: Box::new(base_e.clone()),
            msb: Box::new(msb_e),
            lsb: Box::new(lsb_e),
        })
    }

    /// Single-element addressing `v(k)` on a plain (non-memory) vector with a
    /// recorded index direction or a multi-bit element width: ascending
    /// ranges address mirrored (element k = value bit W-1-(k-lo)·ew) and
    /// `string` characters are 8 bits, so the element must lower to a sized
    /// `Sel`, not a raw 1-bit `Idx`. `None` keeps `Idx` — a 1-bit descending
    /// range from 0, where raw bit addressing is already correct.
    pub(super) fn elem_sel(&self, sid: SigId, k: &Expr) -> Option<Expr> {
        let ew = self.sig_elem.get(&sid).copied().unwrap_or(1).max(1) as u64;
        // A shape-bound formal addresses through its hidden shape slots —
        // the actual's low bound and direction, bound per call — so element
        // k lands at the actual's layout, whatever it is.
        if let Some(&(lo_id, asc_id)) = self.formal_dyn.get(&sid) {
            let (msb, lsb) = dyn_remapped_sel_bounds(
                self.slot_width_expr(sid),
                ew,
                Expr::Sig(lo_id),
                Expr::Sig(asc_id),
                k.clone(),
                k.clone(),
            );
            return Some(Expr::Sel {
                e: Box::new(Expr::Sig(sid)),
                msb: Box::new(msb),
                lsb: Box::new(lsb),
            });
        }
        let (lo, asc) = self.sig_asc.get(&sid).copied().unwrap_or((0, false));
        if !asc && lo == 0 && ew == 1 {
            return None;
        }
        Some(self.sized_elem_sel(sid, k, lo.max(0) as u64, asc, ew))
    }

    /// Sized element select: element k covers value bits [W-1-(k-lo)·ew …
    /// W-(k-lo+1)·ew] ascending, [(k-lo)·ew … (k-lo+1)·ew-1] descending.
    fn sized_elem_sel(&self, sid: SigId, k: &Expr, lo: u64, asc: bool, ew: u64) -> Expr {
        let w = self.slot_width_expr(sid);
        let off = || Expr::Bin {
            op: BinOp::Sub,
            l: Box::new(k.clone()),
            r: Box::new(Expr::Lit(Value::from_u64(32, lo))),
        };
        let scaled = |e: Expr| Expr::Bin {
            op: BinOp::Mul,
            l: Box::new(e),
            r: Box::new(Expr::Lit(Value::from_u64(32, ew))),
        };
        let (msb, lsb) = if asc {
            (
                Expr::Bin {
                    op: BinOp::Sub,
                    l: Box::new(Expr::Bin {
                        op: BinOp::Sub,
                        l: Box::new(w.clone()),
                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                    }),
                    r: Box::new(scaled(off())),
                },
                Expr::Bin {
                    op: BinOp::Sub,
                    l: Box::new(w),
                    r: Box::new(scaled(Expr::Bin {
                        op: BinOp::Add,
                        l: Box::new(off()),
                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                    })),
                },
            )
        } else {
            (
                Expr::Bin {
                    op: BinOp::Add,
                    l: Box::new(scaled(off())),
                    r: Box::new(Expr::Lit(Value::from_u64(32, ew - 1))),
                },
                scaled(off()),
            )
        };
        Expr::Sel {
            e: Box::new(Expr::Sig(sid)),
            msb: Box::new(msb),
            lsb: Box::new(lsb),
        }
    }
}
