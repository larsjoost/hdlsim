//! Structural lowering of selected-name chains: root resolution
//! (signal, constant, materialized-constant, alias classification),
//! the Field/Index segment walk, and the multi-dimensional index
//! linearization shared by the chain's index segments.
//!
//! Extracted from objects.rs (pure move, no logic changes).

use super::*;

/// One step of a folded selected-name chain: a record field select (`.f`)
/// or an index group (`(i)` / `(i, j)`). The parser folds `a.b(i).c` into
/// nested Name/Call nodes; `flatten_selected_name` recovers the order.
#[derive(Debug)]
pub(super) enum ChainSeg {
    Field(String),
    Index(Vec<SyntaxNode>),
}

/// Effective index dims of the current array context: `(low, size)` per
/// dimension, as stored on array types and record-field decl constraints.
type ChainDims = Vec<(i64, usize)>;

/// A selected-name chain's resolved root: either a terminal literal — a
/// constant root folding from its VALUE (constant initializers like
/// `constant x : t := c.field;` cannot read the materialized hidden
/// signal, and the value is equally valid at runtime: field selects and
/// `$const_idx` evaluate over literals) — or a walk start.
enum ChainRoot {
    Lit(Value),
    Walk {
        cur: Expr,
        cur_ty: String,
        /// Element-record type of an array-of-records root, from the slot
        /// registration; consumed by the first Index segment.
        root_elem: Option<String>,
    },
}

impl ChainRoot {
    /// Walk start whose root is not an array-of-records slot.
    fn walk(cur: Expr, cur_ty: String) -> ChainRoot {
        ChainRoot::Walk {
            cur,
            cur_ty,
            root_elem: None,
        }
    }
}

/// Linearize a multi-dimensional index group against per-dimension
/// `(low, size)` bounds: `(i-lo0)*n1*n2 + (j-lo1)*n2 + (k-lo2)`. Same
/// formula as `lower_call`'s signal-array path and
/// `lower_const_array_index`.
pub(super) fn flatten_dims(
    args: &[SyntaxNode],
    dims: &[(i64, usize)],
    env: &HashMap<String, Value>,
    lx: &mut Lower,
) -> Expr {
    let mut flat: Option<Expr> = None;
    for (arg, (lo, n)) in args.iter().zip(dims.iter()) {
        let a = Expr::Bin {
            op: BinOp::Sub,
            l: Box::new(lx.lower_expr(arg, env)),
            r: Box::new(Expr::Lit(Value::from_u64(32, *lo as u64))),
        };
        flat = Some(match flat {
            None => a,
            Some(f) => Expr::Bin {
                op: BinOp::Add,
                l: Box::new(Expr::Bin {
                    op: BinOp::Mul,
                    l: Box::new(f),
                    r: Box::new(Expr::Lit(Value::from_u64(32, *n as u64))),
                }),
                r: Box::new(a),
            },
        });
    }
    flat.unwrap_or_else(|| Expr::Lit(Value::from_u64(32, 0)))
}

impl<'a> Lower<'a> {
    /// Flatten a folded selected-name chain (`a.b(i).c`) into the root name
    /// (the first identifier) and the ordered segments after it. The parser
    /// folds the chain into nested Name/Call nodes: dotted segments stay
    /// flat in one Name, an index introduces a Call, and every `.field`
    /// after a Call wraps an outer Name.
    pub(super) fn flatten_selected_name(&self, e: &SyntaxNode, segs: &mut Vec<ChainSeg>) -> String {
        let mut root = String::new();
        for k in real_children(e) {
            match k.kind() {
                SyntaxKind::Ident => {
                    if root.is_empty() && segs.is_empty() {
                        root = self.kw(&k);
                    } else {
                        segs.push(ChainSeg::Field(self.kw(&k)));
                    }
                }
                SyntaxKind::Name => {
                    root = self.flatten_selected_name(&k, segs);
                }
                SyntaxKind::Call => {
                    let ck: Vec<SyntaxNode> = real_children(&k);
                    // The callee base is the first Name child; the index
                    // arguments are the remaining expression children.
                    let base_pos = ck.iter().position(|c| c.kind() == SyntaxKind::Name);
                    if let Some(bp) = base_pos {
                        root = self.flatten_selected_name(&ck[bp], segs);
                    }
                    let args: Vec<SyntaxNode> = ck
                        .iter()
                        .enumerate()
                        .filter(|(i, c)| Some(*i) != base_pos && is_expr_node(c.kind()))
                        .map(|(_, c)| c.clone())
                        .collect();
                    if !args.is_empty() {
                        segs.push(ChainSeg::Index(args));
                    }
                }
                _ => {}
            }
        }
        root
    }
    /// Walk start for a slot root: a record slot starts on the record
    /// type, an elem-record slot (array of records) on the element type
    /// with the elem-record carried for the first Index segment; a plain
    /// (non-record) slot yields `None` — the caller decides the fallback.
    fn sig_slot_root(&self, id: SigId) -> Option<ChainRoot> {
        let rec = self.sig_rec.get(&id).cloned();
        let elem_rec = self.sig_elem_rec.get(&id).cloned();
        Some(match (rec, elem_rec) {
            (Some(rn), _) => ChainRoot::walk(Expr::Sig(id), rn),
            (None, Some(en)) => ChainRoot::Walk {
                cur: Expr::Sig(id),
                root_elem: Some(en.clone()),
                cur_ty: en,
            },
            _ => return None,
        })
    }

    /// Resolve a selected-name chain's root: flatten the chain, reject
    /// function roots (a call's own lowering handles them) and empty
    /// chains, and classify the root — real signal (record, elem-record,
    /// or plain), constant folding from its value, a materialized hidden
    /// `__const_` signal, an alias, or a bare constant. Real signals win
    /// over constants (inner declarations shadow outer consts); only the
    /// materialized-constant fallback yields to the value.
    fn resolve_chain_root(
        &self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<(ChainRoot, Vec<ChainSeg>)> {
        let mut segs: Vec<ChainSeg> = Vec::new();
        let root = self.flatten_selected_name(e, &mut segs);
        if root.is_empty() || segs.is_empty() {
            return None;
        }
        let root_lc = root.to_ascii_lowercase();
        if self.funcs.contains_key(&root_lc) {
            return None;
        }
        let const_root = env
            .get(&root_lc)
            .cloned()
            .or_else(|| self.consts.get(&root_lc).cloned())
            .zip(self.const_types.get(&root_lc).cloned());
        let root = if let Some(id) = self.sig_id(&root) {
            match self.sig_slot_root(id) {
                Some(r) => r,
                None => return const_root.map(|(v, _ty)| (ChainRoot::Lit(v), segs)),
            }
        } else if let Some((v, ty)) = const_root {
            ChainRoot::walk(Expr::Lit(v), ty)
        } else if let Some(id) = self.names.get(&format!("__const_{}", root_lc)).copied() {
            return self.sig_slot_root(id).map(|r| (r, segs));
        } else if let Some(a) = self.aliases.get(&root_lc).cloned() {
            let ty = self.alias_rec.get(&root_lc).cloned()?;
            ChainRoot::walk(a, ty)
        } else {
            let v = self.consts.get(&root_lc).cloned()?;
            let ty = self.const_types.get(&root_lc).cloned()?;
            ChainRoot::walk(Expr::Lit(v), ty)
        };
        Some((root, segs))
    }

    /// Structural lowering of a selected-name chain rooted at a signal,
    /// constant, or alias: walk the segments, applying record field selects
    /// (slices of the flat record vector) and array element reads. Arrays
    /// inside records (and constant arrays) read as `$const_idx` over the
    /// flat value; whole signal arrays keep the kernel's memory `Idx`.
    /// Returns `None` when the chain does not resolve this way — the caller
    /// falls back to the legacy per-shape handling.
    pub(super) fn lower_selected_chain(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let (root, segs) = self.resolve_chain_root(e, env)?;
        match root {
            ChainRoot::Lit(v) => Some(Expr::Lit(v)),
            ChainRoot::Walk {
                cur,
                cur_ty,
                root_elem,
            } => self.walk_chain_segments(cur, cur_ty, root_elem, segs, env),
        }
    }

    /// Apply a record-field select segment: narrow the current expression to
    /// the field's bit range (fields packed first-declared = MSB) and step
    /// the type to the field's. The effective index dims come from the
    /// field's decl constraint, else the field type's own Array info.
    /// Returns the updated (expression, type, dims).
    fn chain_field_seg(
        &self,
        cur: Expr,
        cur_ty: String,
        f: &String,
    ) -> Option<(Expr, String, Option<ChainDims>)> {
        let Some(TypeInfo::Record { fields }) = self.types.get(&cur_ty) else {
            return None;
        };
        let total: usize = fields.iter().map(|fd| fd.1).sum();
        let mut run = 0usize;
        let mut hit: Option<(usize, usize)> = None;
        for (fn_, w, _) in fields {
            let lsb = total - run - w;
            if *fn_ == *f {
                hit = Some((lsb, *w));
                break;
            }
            run += w;
        }
        let (lsb, w) = hit?;
        let outer_ty = cur_ty.clone();
        let field_ty = self.rec_field_ty.get(&(cur_ty.clone(), f.clone()))?.clone();
        let cur_dims = self
            .rec_field_dims
            .get(&(outer_ty, f.clone()))
            .cloned()
            .or_else(|| match self.types.get(&field_ty) {
                Some(TypeInfo::Array { dims, .. }) if !dims.is_empty() => Some(dims.clone()),
                _ => None,
            });
        let cur = Expr::Sel {
            msb: Box::new(Expr::Lit(Value::from_u64(32, (lsb + w - 1) as u64))),
            lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
            e: Box::new(cur),
        };
        Some((cur, field_ty, cur_dims))
    }

    /// Apply an array-index segment. Memory-backed signal arrays index by
    /// element; flat value slots (function formals) read elements out of
    /// the flat vector — record elements via `$const_idx` (scalar elements
    /// keep the kernel's established bit read); non-signal bases index the
    /// flat constant value. Consumes the array-of-records element type,
    /// clears the dims context (a following Field seg re-derives it), and
    /// steps the type to the element's. Returns the updated (expr, type).
    fn chain_index_seg(
        &mut self,
        cur: Expr,
        cur_ty: String,
        root_elem: &mut Option<String>,
        cur_dims: &mut Option<ChainDims>,
        args: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<(Expr, String)> {
        // Array-of-records root: the element type is known from the slot
        // registration; other bases resolve it from the current type's
        // Array info.
        let mut root_ty = root_elem.take();
        let (cur, stepped) = if let Expr::Sig(sid) = cur {
            self.sig_index_expr(sid, &mut root_ty, args, env)?
        } else {
            (
                self.const_index_expr(cur, &cur_ty, cur_dims, args, env)?,
                None,
            )
        };
        let cur_ty = match stepped {
            Some(en) => en,
            None => match root_ty.take() {
                Some(en) => en,
                None => match self.types.get(&cur_ty).cloned() {
                    Some(TypeInfo::Array {
                        elem_name: Some(en),
                        ..
                    }) => en,
                    _ => return None,
                },
            },
        };
        // The element itself is not the constrained field array anymore;
        // a following Field seg re-derives the dims.
        *cur_dims = None;
        Some((cur, cur_ty))
    }

    /// Index a signal root: memory-backed arrays read by element; flat
    /// value slots (function formals) read elements out of the flat
    /// vector — record elements via `$const_idx` (scalar elements keep
    /// the kernel's established bit read). Returns the indexed expression
    /// and, when the record-element read already stepped the type to the
    /// element record, that element type.
    fn sig_index_expr(
        &mut self,
        sid: SigId,
        root_ty: &mut Option<String>,
        args: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<(Expr, Option<String>)> {
        let is_mem = self.mems.get(&sid).copied().is_some();
        if !is_mem && args.len() == 1 {
            let elem_rec_w = root_ty.as_ref().and_then(|en| match self.types.get(en) {
                Some(TypeInfo::Record { fields }) => {
                    Some(fields.iter().map(|f| f.1).sum::<usize>().max(1))
                }
                _ => None,
            });
            if let Some(ew) = elem_rec_w {
                let idx = self.lower_expr(&args[0], env);
                return Some((
                    Expr::Call {
                        name: "$const_idx".into(),
                        args: vec![
                            Expr::Sig(sid),
                            idx,
                            Expr::Lit(Value::from_u64(32, ew as u64)),
                        ],
                        shapes: Vec::new(),
                    },
                    root_ty.take(),
                ));
            }
        }
        if args.len() == 1 {
            let idx = self.lower_expr(&args[0], env);
            Some((
                Expr::Idx {
                    e: Box::new(Expr::Sig(sid)),
                    i: Box::new(idx),
                },
                None,
            ))
        } else {
            Some((self.multi_dim_idx(sid, args, env)?, None))
        }
    }

    /// Flatten a multi-dimensional index group against the signal's
    /// registered dims; an arity mismatch fails the chain.
    fn multi_dim_idx(
        &mut self,
        sid: SigId,
        args: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let sd = self.sig_dims.get(&sid).cloned()?;
        if sd.len() != args.len() {
            return None;
        }
        Some(Expr::Idx {
            e: Box::new(Expr::Sig(sid)),
            i: Box::new(flatten_dims(args, &sd, env, self)),
        })
    }

    /// Index a non-signal base (a flat constant value): the record
    /// field's decl constraint overrides the (empty) dims of an
    /// unconstrained array type; a single index with no dims at all keeps
    /// the raw value (0-based assumption beats dropping it).
    fn const_index_expr(
        &mut self,
        cur: Expr,
        cur_ty: &str,
        cur_dims: &mut Option<ChainDims>,
        args: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let Some(TypeInfo::Array { elem, dims, .. }) = self.types.get(cur_ty).cloned() else {
            return None;
        };
        let dims = cur_dims.take().unwrap_or(dims);
        let flat = if args.len() == 1 && dims.is_empty() {
            self.lower_expr(&args[0], env)
        } else {
            flatten_dims(args, &dims, env, self)
        };
        Some(Expr::Call {
            name: "$const_idx".into(),
            args: vec![
                cur,
                flat,
                Expr::Lit(Value::from_u64(32, elem.max(1) as u64)),
            ],
            shapes: Vec::new(),
        })
    }

    /// Walk a selected-name chain's segments from its resolved root,
    /// threading the current expression, type, array dims, and the
    /// array-of-records element type through each segment.
    fn walk_chain_segments(
        &mut self,
        cur: Expr,
        cur_ty: String,
        root_elem: Option<String>,
        segs: Vec<ChainSeg>,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        // Effective index dims of the current array context: the
        // type's own dims, overridden by a record field's decl
        // constraint (an unconstrained array type with `(0 to 3)`
        // at the field).
        let mut cur_dims: Option<ChainDims> = match self.types.get(&cur_ty) {
            Some(TypeInfo::Array { dims, .. }) if !dims.is_empty() => Some(dims.clone()),
            _ => None,
        };
        let (mut cur, mut cur_ty, mut root_elem) = (cur, cur_ty, root_elem);
        for seg in segs {
            match seg {
                ChainSeg::Field(f) => {
                    (cur, cur_ty, cur_dims) = self.chain_field_seg(cur, cur_ty, &f)?;
                }
                ChainSeg::Index(args) => {
                    (cur, cur_ty) = self.chain_index_seg(
                        cur,
                        cur_ty,
                        &mut root_elem,
                        &mut cur_dims,
                        &args,
                        env,
                    )?;
                }
            }
        }
        Some(cur)
    }
}
