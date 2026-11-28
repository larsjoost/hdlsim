//! Struct pattern assignments: `'{a: x, b: y}` against the LHS struct
//! type, lowered to a field-order concat. Split out of `verilog_lower.rs`
//! to keep it at its standards baseline.

use std::collections::HashMap;

use super::{is_expr_node, real_children, Lower, TypeKind};
use crate::ir::Expr;
use crate::value::Value;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    pub(super) fn lower_pattern(
        &mut self,
        pat: &SyntaxNode,
        lhs: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        if lhs.kind() != SyntaxKind::Name {
            return None;
        }
        let id = self.sig_id(&self.text(lhs))?;
        let tn = self.sig_types.get(&id)?.clone();
        let td = self.types.get(&tn)?.clone();
        let fields = match &td.kind {
            TypeKind::Struct(f) => f.clone(),
            _ => return None,
        };
        let (named, positional, default) = self.scan_pattern(pat, env);
        Some(pattern_concat(fields, named, positional, default))
    }

    /// Split the `'{...}` items into named `field: expr` pairs (a
    /// `default:` key is pulled out separately), positional expressions,
    /// and the `default` expression.
    fn scan_pattern(
        &mut self,
        pat: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> (Vec<(String, Expr)>, Vec<Expr>, Option<Expr>) {
        let kids = real_children(pat);
        let mut named: Vec<(String, Expr)> = Vec::new();
        let mut positional: Vec<Expr> = Vec::new();
        let mut default: Option<Expr> = None;
        let mut i = 0;
        while i < kids.len() {
            if kids[i].kind() == SyntaxKind::Name {
                let n = self.text(&kids[i]);
                i += 1;
                if i < kids.len() && is_expr_node(kids[i].kind()) {
                    let e = self.lower_expr(&kids[i], env);
                    if n == "default" {
                        default = Some(e);
                    } else {
                        named.push((n, e));
                    }
                    i += 1;
                }
            } else if is_expr_node(kids[i].kind()) {
                positional.push(self.lower_expr(&kids[i], env));
                i += 1;
            } else {
                i += 1;
            }
        }
        (named, positional, default)
    }
}

/// Order the struct fields descending lsb → msb and build the concat
/// parts: a named match first, else the next positional expression, else
/// `default`, else all-x at the field width.
fn pattern_concat(
    fields: Vec<(String, usize, usize)>,
    named: Vec<(String, Expr)>,
    positional: Vec<Expr>,
    default: Option<Expr>,
) -> Expr {
    let mut ordered = fields.clone();
    ordered.sort_by_key(|f| std::cmp::Reverse(f.2)); // descending lsb → msb first
    let mut pos = 0;
    let mut parts: Vec<Expr> = Vec::new();
    for (n, w, _) in &ordered {
        let e = named
            .iter()
            .find(|(fn_, _)| fn_ == n)
            .map(|(_, e)| e.clone())
            .or_else(|| {
                if pos < positional.len() {
                    let e = positional[pos].clone();
                    pos += 1;
                    Some(e)
                } else {
                    None
                }
            })
            .or_else(|| default.clone())
            .unwrap_or_else(|| Expr::Lit(Value::x(*w)));
        parts.push(e);
    }
    Expr::Concat(parts)
}
