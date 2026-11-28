//! VHDL operator lowering: infix `BinExpr` (concatenation `&` and the
//! binary operators, with signed-compare marking) and prefix `UnaryExpr`
//! (with the `-literal` signed fold). Split out of `expr.rs` to keep every
//! function at its standards baseline.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn lower_bin_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let exprs: Vec<&SyntaxNode> = kids.iter().filter(|k| is_expr_node(k.kind())).collect();
        let op_text: String = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Punct || k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .collect::<Vec<_>>()
            .join("");
        if op_text == "&" && exprs.len() == 2 {
            // Concatenation.
            return Expr::Concat(vec![
                self.lower_expr(exprs[0], env),
                self.lower_expr(exprs[1], env),
            ]);
        }
        let Some(op) = vhdl_binop(&op_text) else {
            self.err(e.span(), format!("unsupported operator '{}'", op_text));
            return Expr::Lit(Value::x(1));
        };
        if exprs.len() == 2 {
            let le = self.lower_expr(exprs[0], env);
            let re = self.lower_expr(exprs[1], env);
            self.mark_signed_compare(op, le, re)
        } else {
            Expr::Lit(Value::x(1))
        }
    }

    pub(super) fn lower_unary_expr(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Expr {
        let kids = real_children(e);
        let op_text = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Punct || k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .unwrap_or_default();
        let operand = kids.iter().find(|k| is_expr_node(k.kind()));
        let Some(op) = vhdl_unop(&op_text) else {
            self.err(
                e.span(),
                format!("unsupported unary operator '{}'", op_text),
            );
            return Expr::Lit(Value::x(1));
        };
        match operand {
            Some(o) => {
                let oe = self.lower_expr(o, env);
                // `-literal` is a signed universal integer: fold to a
                // signed-marked literal so ordered comparisons are
                // signed (`-1 >= 0` is false, not 4294967295 >= 0).
                if op == UnOp::Neg {
                    if let Expr::Lit(v) = &oe {
                        if let Some(n) = v.to_u64() {
                            let w = v.width().max(1);
                            let mask = if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                            let neg = (!(n & mask)).wrapping_add(1) & mask;
                            return Expr::Signed(Box::new(Expr::Lit(Value::from_u64(w, neg))));
                        }
                    }
                }
                Expr::Un {
                    op,
                    e: Box::new(oe),
                }
            }
            None => Expr::Lit(Value::x(1)),
        }
    }
}
