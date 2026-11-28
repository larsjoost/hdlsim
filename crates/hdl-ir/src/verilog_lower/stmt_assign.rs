//! Assignment statement lowering: blocking/non-blocking procedural
//! assignments (`lhs = rhs` / `lhs <= rhs`), class-field setters, `new`
//! allocations, statement-position method and task calls, and procedural
//! continuous `assign`/`deassign`. Split out of `verilog_lower.rs`
//! (statements) to keep the modules at the module-size standard.

use std::collections::HashMap;

use super::{is_expr_node, pack_str, real_children, Lower, TypeKind};
use crate::ir::{Expr, Stmt};
use crate::value::Value;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    /// A `StmtAssign`: a blocking/non-blocking assignment, or (without an
    /// `=`) a statement-position expression.
    pub(super) fn lower_stmt_assign(
        &mut self,
        s: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Stmt {
        let kids = real_children(s);
        let body: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| !(k.kind() == SyntaxKind::Punct && self.text(k) == ";"))
            .collect();
        // `<=` arrives as two punct tokens `<` `=`; detect the pair
        // before treating a lone `=` as a blocking assignment.
        let mut op_pos = None;
        let mut nba = false;
        for i in 0..body.len() {
            if body[i].kind() != SyntaxKind::Punct {
                continue;
            }
            let t = self.text(body[i]);
            if t == "=" {
                if i > 0 && body[i - 1].kind() == SyntaxKind::Punct && self.text(body[i - 1]) == "<"
                {
                    op_pos = Some(i - 1);
                    nba = true;
                } else {
                    op_pos = Some(i);
                }
                break;
            }
        }
        match op_pos {
            Some(op) => {
                let lhs = body[..op].iter().find(|k| is_expr_node(k.kind()));
                let rhs = body[op + 1..].iter().find(|k| is_expr_node(k.kind()));
                match (lhs, rhs) {
                    (Some(l), Some(r)) => self.lower_assign_value(l, r, nba, env),
                    _ => Stmt::Null,
                }
            }
            None => match body.iter().find(|k| is_expr_node(k.kind())) {
                Some(e) => self.lower_expr_stmt(e, env),
                None => Stmt::Null,
            },
        }
    }

    /// The `lhs = rhs` core: class-field setter, `new` allocations, struct
    /// assignment pattern, then the plain assignment.
    fn lower_assign_value(
        &mut self,
        l: &SyntaxNode,
        r: &SyntaxNode,
        nba: bool,
        env: &HashMap<String, Value>,
    ) -> Stmt {
        if let Some(st) = self.lower_class_setf(l, r, env) {
            return st;
        }
        if let Some(st) = self.lower_new_object(l, r, env) {
            return st;
        }
        if let Some(st) = self.lower_array_new(l, r, env) {
            return st;
        }
        // `p = '{a: x, b: y}` — struct assignment pattern.
        if r.kind() == SyntaxKind::Pattern {
            if let Some(e) = self.lower_pattern(r, l, env) {
                return Stmt::Assign {
                    lhs: self.lower_expr(l, env),
                    rhs: e,
                    nba,
                };
            }
        }
        Stmt::Assign {
            lhs: self.lower_expr(l, env),
            rhs: self.lower_expr(r, env),
            nba,
        }
    }

    /// `obj.field = v` / `this.x = v` → $setf call (expression form so it
    /// also works inside functions, where only &self evaluation runs).
    fn lower_class_setf(
        &mut self,
        l: &SyntaxNode,
        r: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Stmt> {
        let (cn, field, obj) = self.class_field_lhs(l)?;
        Some(Stmt::ExprStmt(Expr::Call {
            name: self
                .setf_name(&cn, &field)
                .unwrap_or_else(|| format!("$setf::{}::{}", cn, field)),
            args: vec![obj, self.lower_expr(r, env)],
            shapes: Vec::new(),
        }))
    }

    /// `c = new C(args)` → `$new::C` syscall (alloc + ctor). `None` when
    /// the allocator is not a class construction (falls through).
    fn lower_new_object(
        &mut self,
        l: &SyntaxNode,
        r: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Stmt> {
        if r.kind() != SyntaxKind::Allocator {
            return None;
        }
        let ak = real_children(r);
        let has_paren = ak
            .iter()
            .any(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "(");
        if !has_paren {
            return None;
        }
        let cn = self.allocator_class_name(&ak, l)?;
        let mut args = vec![self.lower_expr(l, env)];
        args.extend(
            ak.iter()
                .filter(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name)
                .map(|k| self.lower_expr(k, env)),
        );
        Some(Stmt::SysCall {
            name: format!("$new::{}", cn),
            args,
            shapes: Vec::new(),
        })
    }

    /// The class name for `new ...`: explicit `new C(...)`, else the
    /// enclosing class, else — `c = new(...)`: from the LHS type.
    fn allocator_class_name(&self, ak: &[SyntaxNode], l: &SyntaxNode) -> Option<String> {
        ak.iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .or_else(|| self.cur_class.clone())
            .or_else(|| {
                if l.kind() == SyntaxKind::Name {
                    self.sig_id(&self.text(l)).and_then(|id| {
                        self.sig_types.get(&id).and_then(|t| {
                            self.types.get(t).and_then(|td| match &td.kind {
                                TypeKind::Class(cn) => Some(cn.clone()),
                                _ => None,
                            })
                        })
                    })
                } else {
                    None
                }
            })
    }

    /// `arr = new[n]` — dynamic array allocation.
    fn lower_array_new(
        &mut self,
        l: &SyntaxNode,
        r: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Stmt> {
        if r.kind() != SyntaxKind::Allocator
            || !real_children(r)
                .iter()
                .any(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "[")
        {
            return None;
        }
        let size = real_children(r)
            .into_iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(&k, env))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(32, 0)));
        Some(Stmt::SysCall {
            name: "$array_new".to_string(),
            args: vec![self.lower_expr(l, env), size],
            shapes: Vec::new(),
        })
    }

    /// A statement-position expression: class/array method calls get their
    /// statement forms, a bare unknown name is a task call, everything else
    /// lowers as an expression.
    fn lower_expr_stmt(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        if e.kind() == SyntaxKind::Call {
            if let Some(st) = self.lower_class_method_stmt(e, env) {
                return st;
            }
            // Array method call as a statement: `q.push_back(x);`
            if let Some(st) = self.lower_array_method_stmt(e, env) {
                return st;
            }
        }
        // Bare task call without parens: `my_task;` — a name that is neither
        // a signal nor a constant.
        if let Some(st) = self.lower_bare_task_stmt(e) {
            return st;
        }
        Stmt::ExprStmt(self.lower_expr(e, env))
    }

    /// Class method call as a statement: `c.randomize();`, `c.do();` →
    /// `$callmethod::C::m(c, ...)` (randomize keeps the expression form).
    fn lower_class_method_stmt(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Stmt> {
        let ck = real_children(e);
        let cname = ck
            .first()
            .map(|k| self.text(k).trim_start_matches('$').to_string())
            .unwrap_or_default();
        let dot = cname.rfind('.')?;
        let base = &cname[..dot];
        let method = &cname[dot + 1..];
        let id = self.sig_id(base)?;
        let tn = self.sig_types.get(&id).cloned()?;
        let td = self.types.get(&tn).cloned()?;
        let TypeKind::Class(cn) = &td.kind else {
            return None;
        };
        let mut args = vec![Expr::Sig(id)];
        args.extend(
            ck[1..]
                .iter()
                .filter(|k| is_expr_node(k.kind()))
                .map(|k| self.lower_expr(k, env)),
        );
        if method == "randomize" {
            let spec = self.randomize_spec(cn);
            args.push(Expr::Lit(pack_str(&spec)));
            return Some(Stmt::ExprStmt(Expr::Call {
                name: self.randomize_name(cn),
                args,
                shapes: Vec::new(),
            }));
        }
        Some(Stmt::SysCall {
            name: format!("$callmethod::{}::{}", cn, method),
            args,
            shapes: Vec::new(),
        })
    }

    /// Array method call as a statement: `q.push_back(x);` →
    /// `$array_<method>(q, ...)`.
    fn lower_array_method_stmt(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Stmt> {
        let ck = real_children(e);
        let cname = ck
            .first()
            .map(|k| self.text(k).trim_start_matches('$').to_string())
            .unwrap_or_default();
        let dot = cname.find('.')?;
        let base = &cname[..dot];
        let method = &cname[dot + 1..];
        let id = self.sig_id(base)?;
        if !self.sig_is_mem.contains(&id) {
            return None;
        }
        let mut args = vec![Expr::Sig(id)];
        args.extend(
            ck[1..]
                .iter()
                .filter(|k| is_expr_node(k.kind()))
                .map(|k| self.lower_expr(k, env)),
        );
        Some(Stmt::SysCall {
            name: format!("$array_{}", method),
            args,
            shapes: Vec::new(),
        })
    }

    /// Bare task call without parens: `my_task;` — a name that is neither a
    /// signal nor a constant.
    fn lower_bare_task_stmt(&mut self, e: &SyntaxNode) -> Option<Stmt> {
        if e.kind() != SyntaxKind::Name {
            return None;
        }
        let n = self.text(e);
        if self.sig_id(&n).is_none()
            && !self.gconsts.contains_key(&n)
            && !self.gconsts.contains_key(&n.to_ascii_lowercase())
        {
            return Some(Stmt::ExprStmt(Expr::Call {
                name: n,
                args: Vec::new(),
                shapes: Vec::new(),
            }));
        }
        None
    }

    /// Procedural continuous assignment `assign lhs = rhs;` (LRM 9.2.2).
    /// Module-level continuous assigns reach the module-item path, not
    /// this statement lowering.
    pub(super) fn lower_proc_cont_assign(
        &mut self,
        s: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Stmt {
        let kids = real_children(s);
        let eq = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=");
        match eq {
            Some(eq) => {
                let lhs = kids[..eq].iter().find(|k| is_expr_node(k.kind()));
                let rhs = kids[eq + 1..].iter().find(|k| is_expr_node(k.kind()));
                match (lhs, rhs) {
                    (Some(l), Some(r)) => Stmt::ProcContAssign {
                        lhs: self.lower_expr(l, env),
                        rhs: self.lower_expr(r, env),
                    },
                    _ => {
                        self.err(s.span(), "malformed procedural continuous assign");
                        Stmt::Null
                    }
                }
            }
            None => {
                self.err(s.span(), "assign without `=`");
                Stmt::Null
            }
        }
    }

    pub(super) fn lower_deassign(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let lhs = real_children(s)
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|l| self.lower_expr(l, env))
            .unwrap_or_else(|| Expr::Lit(Value::x(1)));
        Stmt::Deassign { lhs }
    }
}
