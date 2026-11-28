//! Selected concurrent assignment: `with sel select target <= w1 when c1, …;`
//! lowered to nested ternaries (first matching choice wins, `others` last).
//! Split out of `process.rs` to keep it at its standards baseline.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn lower_selected_assign(&mut self, s: &SyntaxNode) -> Option<AssignIr> {
        let kids = real_children(s);
        let sel_pos = kids.iter().position(|k| is_expr_node(k.kind()))?;
        let target_pos = kids[sel_pos + 1..]
            .iter()
            .position(|k| k.kind() == SyntaxKind::Name)?
            + sel_pos
            + 1;
        let sel = self.lower_expr(&kids[sel_pos], &self.consts.clone());
        let lhs = self.lower_expr(&kids[target_pos], &self.consts.clone());
        // Walk `value [after t] when (choice | ... | others) [, ...]` pairs.
        let mut pairs: Vec<(SyntaxNode, Vec<SyntaxNode>, bool)> = Vec::new();
        let mut cur_val: Option<SyntaxNode> = None;
        let mut cur_choices: Vec<SyntaxNode> = Vec::new();
        let mut cur_others = false;
        let mut in_choices = false;
        for k in &kids[target_pos + 1..] {
            match self.kw(k).as_str() {
                "when" => in_choices = true,
                "others" => cur_others = true,
                _ if k.kind() == SyntaxKind::Punct && self.text(k) == "," => {
                    if let Some(v) = cur_val.take() {
                        pairs.push((v, std::mem::take(&mut cur_choices), cur_others));
                        cur_others = false;
                    }
                    in_choices = false;
                }
                _ if is_expr_node(k.kind()) => {
                    if in_choices {
                        cur_choices.push(k.clone());
                    } else {
                        cur_val = Some(k.clone());
                    }
                }
                _ => {}
            }
        }
        if let Some(v) = cur_val {
            pairs.push((v, cur_choices, cur_others));
        }
        if pairs.is_empty() {
            return None;
        }
        let mut rhs: Option<Expr> = None;
        for (val, choices, others) in pairs.iter().rev() {
            let v = self.lower_expr(val, &self.consts.clone());
            if *others {
                rhs = Some(rhs.unwrap_or(v));
                continue;
            }
            let mut cond: Option<Expr> = None;
            for c in choices {
                let e = Expr::Bin {
                    op: BinOp::Eq,
                    l: Box::new(sel.clone()),
                    r: Box::new(self.lower_expr(c, &self.consts.clone())),
                };
                cond = Some(match cond {
                    None => e,
                    Some(prev) => Expr::Bin {
                        op: BinOp::LogOr,
                        l: Box::new(prev),
                        r: Box::new(e),
                    },
                });
            }
            let else_e = rhs.unwrap_or_else(|| Expr::Lit(Value::x(1)));
            rhs = Some(match cond {
                Some(c) => Expr::Cond {
                    c: Box::new(c),
                    t: Box::new(v),
                    e: Box::new(else_e),
                },
                None => else_e,
            });
        }
        Some(AssignIr { lhs, rhs: rhs? })
    }
}
