//! Module instantiation lowering: header scan (module name, `#(...)`
//! parameter group, instance name), port connections, parameter overrides,
//! and the generate-scope name prefixing. Split out of `verilog_lower.rs`
//! to keep it at its standards baseline.

use std::collections::HashMap;

use super::{is_expr_node, real_children, Conn, Lower};
use crate::ir::{Expr, InstIr, ModuleIr};
use crate::value::Value;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    pub(super) fn lower_inst(
        &mut self,
        module: &mut ModuleIr,
        inst: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kids = real_children(inst);
        let (module_name, mut inst_name, i) = self.inst_header(&kids);
        let mut conns = Vec::new();
        for c in &kids[i..] {
            if c.kind() == SyntaxKind::PortConn {
                conns.push(self.lower_conn(c, env))
            }
        }
        let mut param_overrides = Vec::new();
        for c in &kids[1..] {
            if c.kind() == SyntaxKind::ParamAssign {
                let (n, e) = self.lower_param_assign(c, env);
                param_overrides.push((Some(n), e));
            }
        }
        if inst_name.is_empty() {
            self.err(inst.span(), "instance without a name");
            return;
        }
        self.inst_conns.insert(inst_name.clone(), conns.clone());
        if !self.gen_path.is_empty() {
            inst_name = format!("{}.{}", self.gen_path.join("."), inst_name);
        }
        module.instances.push(InstIr {
            module: module_name,
            name: inst_name,
            param_overrides,
            conns,
            config: None,
        });
    }

    /// Scan the instance header: module name, the `#(...)` parameter
    /// override group (skipped whole), instance name; returns them with the
    /// number of children consumed.
    fn inst_header(&self, kids: &[SyntaxNode]) -> (String, String, usize) {
        let mut module_name = String::new();
        let mut inst_name = String::new();
        let mut i = 0;
        if kids.first().is_some_and(|k| k.kind() == SyntaxKind::Name) {
            module_name = self.text(&kids[0]);
            i = 1;
        }
        // `#(...)` parameter overrides arrive as ParamAssign nodes inside the
        // parens — skip the whole `#( ... )` group when looking for the name.
        if i < kids.len() && self.text(&kids[i]) == "#" {
            i += 1; // `#`
            let mut depth = 0i32;
            while i < kids.len() {
                let t = self.text(&kids[i]);
                if t == "(" {
                    depth += 1;
                } else if t == ")" {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
        }
        if i < kids.len() && kids[i].kind() == SyntaxKind::Name {
            inst_name = self.text(&kids[i]);
            i += 1;
        }
        (module_name, inst_name, i)
    }

    /// `.name(expr)` or positional — from a ParamAssign node.
    fn lower_param_assign(
        &mut self,
        c: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> (String, Expr) {
        let kids = real_children(c);
        // The first Name child is the port/parameter name; the expression is
        // the first expr node *after* it (a Name can be both).
        let name_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        let name = name_pos.map(|i| self.text(&kids[i])).unwrap_or_default();
        let expr = kids
            .iter()
            .enumerate()
            .find(|(i, k)| Some(*i) != name_pos && is_expr_node(k.kind()))
            .map(|(_, k)| self.lower_expr(k, env))
            .unwrap_or(Expr::Lit(Value::x(1)));
        (name, expr)
    }

    fn lower_conn(&mut self, c: &SyntaxNode, env: &HashMap<String, Value>) -> Conn {
        let kids = real_children(c);
        // `.*` — the wildcard marker is the lone `*` punct child.
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "*")
        {
            return Conn::Wildcard;
        }
        // First Name child = port name; first expr node after it = connection.
        let name_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        let name = name_pos.map(|i| self.text(&kids[i]));
        let expr = kids
            .iter()
            .enumerate()
            .find(|(i, k)| Some(*i) != name_pos && is_expr_node(k.kind()))
            .map(|(_, k)| self.lower_expr(k, env));
        match (name, expr) {
            (Some(n), Some(e)) => Conn::Named(n, e),
            (None, Some(e)) => Conn::Positional(e),
            // `.name` shorthand: bind to the same-named signal in scope.
            (Some(n), None) => Conn::ByName(n),
            (None, None) => Conn::Positional(Expr::Lit(Value::x(1))),
        }
    }
}
