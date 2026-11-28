//! Expression lowering: CST expression nodes → IR `Expr`. Split out of
//! `verilog_lower.rs` to keep it at its standards baseline: one helper per
//! expression kind off the [`Lower::lower_expr`] dispatcher, the `inside`
//! set-membership OR-chain, and literals (string packing, SV time
//! literals).

use std::collections::HashMap;

use super::{is_expr_node, pack_str, parse_int_literal, real_children, Lower, TypeKind};
use crate::ir::Expr;
use crate::value::{BinOp, UnOp, Value};
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    pub(super) fn lower_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        match e.kind() {
            SyntaxKind::Literal => self.lower_literal(e),
            SyntaxKind::Name => self.lower_name_expr(e, env),
            SyntaxKind::ParenExpr => self.lower_paren_expr(e, env),
            SyntaxKind::Cast => self.lower_cast_expr(e, env),
            SyntaxKind::BinExpr => self.lower_bin_expr(e, env),
            SyntaxKind::UnaryExpr => self.lower_unary_expr(e, env),
            SyntaxKind::CondExpr => self.lower_cond_expr(e, env),
            SyntaxKind::Call => self.lower_call_expr(e, env),
            SyntaxKind::Concat => self.lower_concat_expr(e, env),
            SyntaxKind::StreamConcat => self.lower_stream_expr(e, env),
            SyntaxKind::Pattern => self.lower_pattern_expr(e, env),
            SyntaxKind::Allocator => self.lower_allocator_expr(e),
            SyntaxKind::Replicate => self.lower_replicate_expr(e, env),
            SyntaxKind::PartSelect => self.lower_part_select_expr(e, env),
            SyntaxKind::IndexedPartSelect => self.lower_indexed_part_select(e, env),
            SyntaxKind::Index => self.lower_index_expr(e, env),
            _ => {
                self.err(e.span(), format!("unsupported expression {:?}", e.kind()));
                Expr::Lit(Value::x(1))
            }
        }
    }

    /// Grouping only — lower the inner expression.
    fn lower_paren_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let inner = real_children(e)
            .into_iter()
            .find(|k| is_expr_node(k.kind()));
        match inner {
            Some(k) => self.lower_expr(&k, env),
            None => Expr::Lit(Value::x(1)),
        }
    }

    /// `signed'(e)` / `unsigned'(e)` / `N'(e)` / `Type'(e)`: lower the
    /// operand; a signed cast keeps the signedness hint, a type cast
    /// resizes to the typedef's width when known.
    fn lower_cast_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let target = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Ident || k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k));
        let is_signed = target.as_deref() == Some("signed");
        let inner = kids
            .iter()
            .rev()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, env))
            .unwrap_or_else(|| Expr::Lit(Value::x(1)));
        if is_signed {
            return Expr::Signed(Box::new(inner));
        }
        if let Some(t) = target {
            if let Some(td) = self.types.get(&t.to_ascii_lowercase()).cloned() {
                if td.width > 0 && !td.real {
                    return Expr::Resize {
                        e: Box::new(inner),
                        width: td.width,
                        signed: td.signed,
                    };
                }
            }
        }
        inner
    }

    fn lower_bin_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        // `lhs inside { a, b, [lo:hi], ... }` — lower to an OR chain of
        // equality / range comparisons.
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "inside")
        {
            return self.lower_inside(&kids, env);
        }
        let expr_nodes: Vec<&SyntaxNode> = kids.iter().filter(|k| is_expr_node(k.kind())).collect();
        let op_text: String = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Punct)
            .map(|k| self.text(k))
            .collect();
        let Some(op) = binop_from_str(&op_text) else {
            self.err(e.span(), format!("unsupported operator '{}'", op_text));
            return Expr::Lit(Value::x(1));
        };
        if expr_nodes.len() == 2 {
            Expr::Bin {
                op,
                l: Box::new(self.lower_expr(expr_nodes[0], env)),
                r: Box::new(self.lower_expr(expr_nodes[1], env)),
            }
        } else {
            Expr::Lit(Value::x(1))
        }
    }

    fn lower_unary_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let op_text: String = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Punct)
            .map(|k| self.text(k))
            .collect();
        let operand = kids.iter().find(|k| is_expr_node(k.kind()));
        // `x++` / `x--` (postfix) and `++x` / `--x` (prefix) become
        // `x+1` / `x-1` (the assignment side effect is modeled by the
        // enclosing statement).
        if op_text == "++" || op_text == "--" {
            return self.lower_incdec(&op_text, operand, env);
        }
        let Some(op) = unop_from_str(&op_text) else {
            self.err(
                e.span(),
                format!("unsupported unary operator '{}'", op_text),
            );
            return Expr::Lit(Value::x(1));
        };
        match operand {
            Some(o) => Expr::Un {
                op,
                e: Box::new(self.lower_expr(o, env)),
            },
            None => Expr::Lit(Value::x(1)),
        }
    }

    /// `x++` / `++x` / `x--` / `--x` → `x+1` / `x-1`.
    fn lower_incdec(
        &mut self,
        op_text: &str,
        operand: Option<&SyntaxNode>,
        env: &HashMap<String, Value>,
    ) -> Expr {
        let Some(o) = operand else {
            return Expr::Lit(Value::x(1));
        };
        let base = self.lower_expr(o, env);
        Expr::Bin {
            op: if op_text == "++" {
                BinOp::Add
            } else {
                BinOp::Sub
            },
            l: Box::new(base),
            r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
        }
    }

    fn lower_cond_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let exprs: Vec<&SyntaxNode> = kids.iter().filter(|k| is_expr_node(k.kind())).collect();
        if exprs.len() == 3 {
            Expr::Cond {
                c: Box::new(self.lower_expr(exprs[0], env)),
                t: Box::new(self.lower_expr(exprs[1], env)),
                e: Box::new(self.lower_expr(exprs[2], env)),
            }
        } else {
            Expr::Lit(Value::x(1))
        }
    }

    fn lower_call_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let name = kids
            .first()
            .map(|k| self.text(k).trim_start_matches('$').to_string())
            .unwrap_or_default();
        // Method call on a class object: `c.get()` → `$method::C::get(c, ...)`.
        if let Some(x) = self.class_method_call(&name, &kids, env) {
            return x;
        }
        // `q.size()` on an array → size query with the array as arg.
        if let Some(x) = self.array_size_call(&name) {
            return x;
        }
        let args = kids[1..]
            .iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, env))
            .collect();
        Expr::Call {
            name,
            args,
            shapes: Vec::new(),
        }
    }

    /// `obj.method(args)` where obj is a class-typed signal.
    fn class_method_call(
        &mut self,
        name: &str,
        kids: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let dot = name.rfind('.')?;
        let (base, method) = (&name[..dot], &name[dot + 1..]);
        let id = self.sig_id(base)?;
        let tn = self.sig_types.get(&id).cloned()?;
        let td = self.types.get(&tn).cloned()?;
        let TypeKind::Class(cn) = &td.kind else {
            return None;
        };
        let cn = cn.clone();
        let mut args = vec![Expr::Sig(id)];
        args.extend(
            kids[1..]
                .iter()
                .filter(|k| is_expr_node(k.kind()))
                .map(|k| self.lower_expr(k, env)),
        );
        if method == "randomize" {
            let spec = self.randomize_spec(&cn);
            args.push(Expr::Lit(pack_str(&spec)));
            return Some(Expr::Call {
                name: self.randomize_name(&cn),
                args,
                shapes: Vec::new(),
            });
        }
        Some(Expr::Call {
            name: format!("$method::{}::{}", cn, method),
            args,
            shapes: Vec::new(),
        })
    }

    /// `q.size()` / `q.num()` on a memory signal → `$array_size(q)`.
    fn array_size_call(&mut self, name: &str) -> Option<Expr> {
        let dot = name.rfind('.')?;
        let (base, method) = (&name[..dot], &name[dot + 1..]);
        if !matches!(method, "size" | "num") {
            return None;
        }
        let id = self.sig_id(base)?;
        if !self.sig_is_mem.contains(&id) {
            return None;
        }
        Some(Expr::Call {
            name: "$array_size".to_string(),
            args: vec![Expr::Sig(id)],
            shapes: Vec::new(),
        })
    }

    fn lower_concat_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        Expr::Concat(
            real_children(e)
                .into_iter()
                .filter(|k| is_expr_node(k.kind()))
                .map(|k| self.lower_expr(&k, env))
                .collect(),
        )
    }

    /// SV streaming operator `{>>{e}}` (identity) / `{<<W{e}}`
    /// (chunk-reverse, LRM 11.4.14). `>>` lowers to the operand
    /// unchanged; `<<` carries the slice width to the kernel, which
    /// reverses the value's `slice`-wide chunks at runtime (the
    /// lowerer has no general expression width to do it statically).
    /// Expr children: just the operand, or the slice then the operand.
    fn lower_stream_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let exprs: Vec<SyntaxNode> = real_children(e)
            .into_iter()
            .filter(|k| is_expr_node(k.kind()))
            .collect();
        let (slice_expr, op) = match exprs.as_slice() {
            [op] => (None, op),
            [slice, op] => (Some(slice), op),
            _ => {
                self.err(e.span(), "stream operator needs exactly one operand");
                return Expr::Lit(Value::x(32));
            }
        };
        let slice = match slice_expr {
            None => 1,
            Some(w) => {
                let wv = self.lower_expr(w, env);
                self.ce(&wv, env)
                    .and_then(|v| v.to_u64())
                    .map(|n| n.max(1) as u32)
                    .unwrap_or(1)
            }
        };
        let lowered = self.lower_expr(op, env);
        if self.text(e).starts_with("{>>") {
            lowered
        } else {
            Expr::Stream {
                e: Box::new(lowered),
                slice,
            }
        }
    }

    /// Assignment pattern `'{ a: x, b: y }`: lowered contextually in
    /// assignments (struct field order); here, positional concat.
    /// `Field:` label names are children too — skip them.
    fn lower_pattern_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let mut parts = Vec::new();
        for (i, k) in kids.iter().enumerate() {
            if k.kind() == SyntaxKind::Name
                && kids
                    .get(i + 1)
                    .is_some_and(|n| n.kind() == SyntaxKind::Punct && self.text(n) == ":")
            {
                continue;
            }
            if is_expr_node(k.kind()) {
                parts.push(self.lower_expr(k, env));
            }
        }
        Expr::Concat(parts)
    }

    /// `new[n]` for dynamic arrays: handled in assignment context; as a
    /// bare expression it is an error.
    fn lower_allocator_expr(&mut self, e: &SyntaxNode) -> Expr {
        self.err(
            e.span(),
            "`new` is only supported as a constructor call or array allocation in an assignment",
        );
        Expr::Lit(Value::x(32))
    }

    fn lower_replicate_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let exprs: Vec<&SyntaxNode> = kids.iter().filter(|k| is_expr_node(k.kind())).collect();
        if exprs.len() == 2 {
            Expr::Replicate {
                count: Box::new(self.lower_expr(exprs[0], env)),
                e: Box::new(self.lower_expr(exprs[1], env)),
            }
        } else {
            Expr::Lit(Value::x(1))
        }
    }

    fn lower_part_select_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let exprs: Vec<&SyntaxNode> = kids.iter().filter(|k| is_expr_node(k.kind())).collect();
        if exprs.len() == 3 {
            Expr::Sel {
                e: Box::new(self.lower_expr(exprs[0], env)),
                msb: Box::new(self.lower_expr(exprs[1], env)),
                lsb: Box::new(self.lower_expr(exprs[2], env)),
            }
        } else {
            Expr::Lit(Value::x(1))
        }
    }

    /// `base +: width` = `base[width-1 + base : base]`;
    /// `base -: width` = `base[base : base - (width-1)]`.
    fn lower_indexed_part_select(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let exprs: Vec<&SyntaxNode> = kids.iter().filter(|k| is_expr_node(k.kind())).collect();
        if exprs.len() != 2 {
            return Expr::Lit(Value::x(1));
        }
        let plus = e
            .children()
            .any(|c| c.kind() == SyntaxKind::Punct && self.db.span_text(c.span()) == "+");
        let one = Expr::Lit(parse_int_literal("1").unwrap_or(Value::x(32)));
        let base = self.lower_expr(exprs[0], env);
        let width = self.lower_expr(exprs[1], env);
        let span = Expr::Bin {
            op: BinOp::Sub,
            l: Box::new(width),
            r: Box::new(one),
        };
        if plus {
            Expr::Sel {
                e: Box::new(base.clone()),
                msb: Box::new(Expr::Bin {
                    op: BinOp::Add,
                    l: Box::new(base.clone()),
                    r: Box::new(span),
                }),
                lsb: Box::new(base),
            }
        } else {
            Expr::Sel {
                e: Box::new(base.clone()),
                msb: Box::new(base.clone()),
                lsb: Box::new(Expr::Bin {
                    op: BinOp::Sub,
                    l: Box::new(base),
                    r: Box::new(span),
                }),
            }
        }
    }

    fn lower_index_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let exprs: Vec<&SyntaxNode> = kids.iter().filter(|k| is_expr_node(k.kind())).collect();
        if exprs.len() == 2 {
            Expr::Idx {
                e: Box::new(self.lower_expr(exprs[0], env)),
                i: Box::new(self.lower_expr(exprs[1], env)),
            }
        } else {
            Expr::Lit(Value::x(1))
        }
    }

    /// `lhs inside { a, b, [lo:hi], ... }` — OR chain of `lhs == item` and
    /// `lo <= lhs <= hi` range comparisons.
    fn lower_inside(&mut self, kids: &[SyntaxNode], env: &HashMap<String, Value>) -> Expr {
        let inside_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "inside")
            .unwrap_or(0);
        let Some(lhs_node) = kids[..inside_pos]
            .iter()
            .rev()
            .find(|k| is_expr_node(k.kind()))
        else {
            return Expr::Lit(Value::x(1));
        };
        let x = self.lower_expr(lhs_node, env);
        let terms = self.inside_terms(&kids[inside_pos + 1..], &x, env);
        let mut acc = Expr::Lit(Value::from_u64(1, 0));
        for t in terms {
            acc = Expr::Bin {
                op: BinOp::LogOr,
                l: Box::new(acc),
                r: Box::new(t),
            };
        }
        acc
    }

    /// One comparison per `inside` item: equality for scalars, a
    /// `lo <= x && x <= hi` conjunction for `[lo:hi]` ranges.
    fn inside_terms(
        &mut self,
        rest: &[SyntaxNode],
        x: &Expr,
        env: &HashMap<String, Value>,
    ) -> Vec<Expr> {
        let mut terms: Vec<Expr> = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            if !is_expr_node(rest[i].kind()) {
                i += 1;
                continue;
            }
            let v = self.lower_expr(&rest[i], env);
            let is_range = rest
                .get(i + 1)
                .is_some_and(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
            if is_range {
                if let Some(hi_node) = rest[i + 2..].iter().find(|k| is_expr_node(k.kind())) {
                    let hi = self.lower_expr(hi_node, env);
                    terms.push(Expr::Bin {
                        op: BinOp::LogAnd,
                        l: Box::new(Expr::Bin {
                            op: BinOp::Ge,
                            l: Box::new(x.clone()),
                            r: Box::new(v),
                        }),
                        r: Box::new(Expr::Bin {
                            op: BinOp::Le,
                            l: Box::new(x.clone()),
                            r: Box::new(hi),
                        }),
                    });
                    // Skip past the range's hi expression.
                    if let Some(j) = rest[i + 2..].iter().position(|k| std::ptr::eq(k, hi_node)) {
                        i = i + 2 + j + 1;
                        continue;
                    }
                }
            } else {
                terms.push(Expr::Bin {
                    op: BinOp::Eq,
                    l: Box::new(x.clone()),
                    r: Box::new(v),
                });
            }
            i += 1;
        }
        terms
    }
}

fn binop_from_str(s: &str) -> Option<BinOp> {
    Some(match s {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "%" => BinOp::Mod,
        "**" => BinOp::Pow,
        "<<" | "<<<" => BinOp::Shl,
        ">>" => BinOp::Shr,
        ">>>" => BinOp::Sar,
        "<" => BinOp::Lt,
        "<=" => BinOp::Le,
        ">" => BinOp::Gt,
        ">=" => BinOp::Ge,
        "==" => BinOp::Eq,
        "!=" => BinOp::Ne,
        "===" => BinOp::EqStrict,
        "!==" => BinOp::NeStrict,
        "&" => BinOp::BAnd,
        "|" => BinOp::BOr,
        "^" => BinOp::BXor,
        "~&" => BinOp::BNand,
        "~|" => BinOp::BNor,
        "~^" | "^~" => BinOp::BXnor,
        "&&" => BinOp::LogAnd,
        "||" => BinOp::LogOr,
        _ => return None,
    })
}

fn unop_from_str(s: &str) -> Option<UnOp> {
    Some(match s {
        "!" => UnOp::Not,
        "~" => UnOp::BNot,
        "-" => UnOp::Neg,
        "+" => UnOp::Pos,
        "&" => UnOp::RedAnd,
        "|" => UnOp::RedOr,
        "^" => UnOp::RedXor,
        "~&" => UnOp::RedNand,
        "~|" => UnOp::RedNor,
        "~^" | "^~" => UnOp::RedXnor,
        _ => return None,
    })
}
