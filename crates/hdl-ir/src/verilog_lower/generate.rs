//! Generate-construct lowering: `for`-generate unrolling at lowering time
//! (constant init/cond/step, per-iteration scope prefix) and `if`-generate.
//! Split out of `verilog_lower.rs` to keep it at its standards baseline.

use std::collections::HashMap;

use super::const_eval::const_eval;
use super::{is_expr_node, is_stmt_node, real_children, Lower};
use crate::ir::{Expr, ModuleIr};
use crate::value::{BinOp, Value};
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    pub(super) fn lower_gen_for(
        &mut self,
        module: &mut ModuleIr,
        g: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kids = real_children(g);
        let assigns: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::StmtAssign)
            .collect();
        let cond_node = kids.iter().find(|k| is_expr_node(k.kind()));
        // The body is the last statement node — not one of the header
        // StmtAssigns (init/step).
        let body = kids.iter().rev().find(|k| {
            k.kind() == SyntaxKind::Block
                || k.kind() == SyntaxKind::GenBlock
                || is_stmt_node(k.kind())
        });
        if assigns.len() < 2 || cond_node.is_none() || body.is_none() {
            self.err(g.span(), "malformed generate for");
            return;
        }
        let init = &assigns[0];
        let ik = real_children(init);
        let var = ik
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .unwrap_or_default();
        let init_val = ik
            .iter()
            .rfind(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, env))
            .and_then(|e| const_eval(&e, env))
            .and_then(|v| v.to_u64());
        let Some(i) = init_val else {
            self.err(g.span(), "generate for: init value not constant");
            return;
        };
        let gen_label = self.gen_block_label(body, &var);
        let step = &assigns[1];
        let body_node = body.expect("body.is_none() returned early above").clone();
        self.gen_for_loop(
            module, g, step, cond_node, body_node, &var, &gen_label, env, i,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn gen_for_loop(
        &mut self,
        module: &mut ModuleIr,
        g: &SyntaxNode,
        step: &SyntaxNode,
        cond_node: Option<&SyntaxNode>,
        body_node: SyntaxNode,
        var: &str,
        gen_label: &str,
        env: &HashMap<String, Value>,
        mut i: u64,
    ) {
        let mut iter = 0u32;
        loop {
            if iter > 1_000_000 {
                self.err(g.span(), "generate for: too many iterations (>1M)");
                return;
            }
            iter += 1;
            let mut env2 = env.clone();
            env2.insert(var.to_string(), Value::from_u64(32, i));
            // Lower the step against the current iteration value so `i = i + 1`
            // const-evaluates.
            let step_expr = self.gen_step_expr(step, &env2);
            let go = cond_node
                .map(|c| self.lower_expr(c, &env2))
                .and_then(|e| const_eval(&e, &env2))
                .and_then(|v| v.to_u64())
                .is_some_and(|v| v != 0);
            if !go {
                break;
            }
            self.gen_path.push(format!("{gen_label}[{i}]"));
            match body_node.kind() {
                SyntaxKind::Block | SyntaxKind::GenBlock => {
                    self.lower_gen_block_items(module, &body_node, &env2);
                }
                _ => self.lower_module_item(module, &body_node, &env2),
            }
            self.gen_path.pop();
            match self.gen_step_apply(g, &step_expr, &env2) {
                Some(next) => i = next,
                None => return,
            }
        }
    }

    /// Block label `begin : lane_gen` when the body names one; the genvar
    /// names the scope otherwise. It prefixes instance labels and process
    /// scopes inside (`lane_gen[0].u_lane`), so parallel lanes are
    /// distinguishable in the transcript and in signal names.
    fn gen_block_label(&self, body: Option<&SyntaxNode>, var: &str) -> String {
        match body {
            Some(b) if matches!(b.kind(), SyntaxKind::Block | SyntaxKind::GenBlock) => {
                real_children(b)
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k).to_string())
                    .filter(|l| !l.is_empty())
            }
            _ => None,
        }
        .unwrap_or_else(|| {
            if var.is_empty() {
                "gen".to_string()
            } else {
                var.to_string()
            }
        })
    }

    /// The per-iteration step expression: compound `j += 2` / `j -= 1`
    /// desugars to `j = j +/- 2`; `++`/`--` to +/- 1; anything else lowers
    /// its last expression.
    fn gen_step_expr(&mut self, step: &SyntaxNode, env2: &HashMap<String, Value>) -> Option<Expr> {
        let sk = real_children(step);
        let op_text: String = sk
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Punct)
            .map(|k| self.text(k))
            .collect();
        let exprs: Vec<&SyntaxNode> = sk.iter().filter(|k| is_expr_node(k.kind())).collect();
        // Compound step `j += 2` / `j -= 1`: desugar to `j = j +/- 2`.
        if exprs.len() >= 2 && (op_text.contains("+=") || op_text.contains("-=")) {
            let op = if op_text.contains("-=") {
                BinOp::Sub
            } else {
                BinOp::Add
            };
            Some(Expr::Bin {
                op,
                l: Box::new(self.lower_expr(exprs[0], env2)),
                r: Box::new(self.lower_expr(exprs[exprs.len() - 1], env2)),
            })
        } else if !exprs.is_empty() && op_text.contains("++") {
            Some(Expr::Bin {
                op: BinOp::Add,
                l: Box::new(self.lower_expr(exprs[0], env2)),
                r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
            })
        } else if !exprs.is_empty() && op_text.contains("--") {
            Some(Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(self.lower_expr(exprs[0], env2)),
                r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
            })
        } else {
            exprs.last().map(|k| self.lower_expr(k, env2))
        }
    }

    /// Apply the step; `None` (with a diagnostic) when the step is
    /// not a constant +/-.
    fn gen_step_apply(
        &mut self,
        g: &SyntaxNode,
        step_expr: &Option<Expr>,
        env2: &HashMap<String, Value>,
    ) -> Option<u64> {
        match step_expr {
            Some(Expr::Bin { op, l, r }) => {
                let lv = const_eval(l, env2).and_then(|v| v.to_u64());
                let rv = const_eval(r, env2).and_then(|v| v.to_u64());
                match (op, lv, rv) {
                    (BinOp::Add, Some(a), Some(b)) => Some(a.wrapping_add(b)),
                    (BinOp::Sub, Some(a), Some(b)) => Some(a.wrapping_sub(b)),
                    _ => {
                        self.err(g.span(), "generate for: step not a constant +/-");
                        None
                    }
                }
            }
            _ => {
                self.err(g.span(), "generate for: unsupported step");
                None
            }
        }
    }

    /// Lower a generate block's items, skipping `begin`/`end` keywords,
    /// `:` and the block label.
    fn lower_gen_block_items(
        &mut self,
        module: &mut ModuleIr,
        b: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        for k in real_children(b) {
            // Skip `begin`/`end` keywords, `:` and the block label.
            if matches!(
                k.kind(),
                SyntaxKind::Ident | SyntaxKind::Punct | SyntaxKind::Name
            ) {
                continue;
            }
            self.lower_module_item(module, &k, env);
        }
    }

    pub(super) fn lower_gen_if(
        &mut self,
        module: &mut ModuleIr,
        g: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kids = real_children(g);
        let cond_node = kids.iter().find(|k| is_expr_node(k.kind()));
        let take = cond_node
            .map(|c| self.lower_expr(c, env))
            .and_then(|e| const_eval(&e, env))
            .and_then(|v| v.to_u64())
            .is_some_and(|v| v != 0);
        let bodies: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| {
                is_stmt_node(k.kind())
                    || k.kind() == SyntaxKind::Block
                    || k.kind() == SyntaxKind::GenBlock
            })
            .collect();
        let chosen = if take { bodies.first() } else { bodies.get(1) };
        if let Some(b) = chosen {
            if b.kind() == SyntaxKind::Block || b.kind() == SyntaxKind::GenBlock {
                self.lower_gen_block_items(module, b, env);
            } else {
                self.lower_module_item(module, b, env);
            }
        }
    }
}
