//! Verilog function/task lowering to [`FuncIr`]: header scan (name, return
//! width, formals, locals, body), the slot-space body swap (slot 0 = return
//! value, then args, then locals), and the module hook. Split out of
//! `verilog_lower.rs` to keep it at its standards baseline.

use std::collections::HashMap;

use super::{is_stmt_node, real_children, type_kw_width, Lower};
use crate::ir::{ArgMode, FuncIr, SigId, SigKind, SignalIr, Stmt};
use crate::value::Value;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

use crate::ir::ModuleIr;

impl<'a> Lower<'a> {
    pub(super) fn lower_function(
        &mut self,
        module: &mut ModuleIr,
        f: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let func = self.lower_function_ir(f, env);
        module.functions.push(func);
    }

    pub(super) fn lower_function_ir(
        &mut self,
        f: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> FuncIr {
        let kids = real_children(f);
        let mut name = String::new();
        let mut ret_width = 1usize;
        let mut arg_nodes = Vec::new();
        let mut local_nodes = Vec::new();
        let mut body_nodes = Vec::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::Name if name.is_empty() => name = self.text(k),
                SyntaxKind::Range => ret_width = self.range_width(k, env),
                // Return type keyword (`function int f...`): pick up its width.
                SyntaxKind::Ident if name.is_empty() => {
                    if let Some(w) = type_kw_width(&self.text(k)) {
                        ret_width = w;
                    }
                }
                SyntaxKind::Port => arg_nodes.push(k.clone()),
                SyntaxKind::Decl => local_nodes.push(k.clone()),
                _ if is_stmt_node(k.kind()) => body_nodes.push(k.clone()),
                _ => {}
            }
        }
        // Slot 0 is the function name (the return value); args follow.
        let mut slots: Vec<(String, usize)> = vec![(name.clone(), ret_width.max(1))];
        let args = self.func_args(&arg_nodes, env, &mut slots);
        let locals = self.func_locals(&local_nodes, env, &mut slots);
        let body = self.func_body_slots(&slots, &body_nodes, env);
        FuncIr {
            name,
            ret_width,
            // Verilog task formals record no directions today — all `in`.
            modes: vec![ArgMode::In; args.len()],
            args,
            defaults: Vec::new(),
            locals,
            dyn_locals: Vec::new(),
            body,
            hidden: Vec::new(),
        }
    }

    /// Formal widths: `args` as `(name, width, mode)` triples (Verilog
    /// task formals record no directions — all `in`), pushed in parallel
    /// into the caller-seeded `slots` table.
    fn func_args(
        &mut self,
        arg_nodes: &[SyntaxNode],
        env: &HashMap<String, Value>,
        slots: &mut Vec<(String, usize)>,
    ) -> Vec<(String, usize, String)> {
        let mut args = Vec::new();
        for a in arg_nodes {
            let ak = real_children(a);
            let aname = ak
                .iter()
                .find(|k| k.kind() == SyntaxKind::Name)
                .map(|k| self.text(k))
                .unwrap_or_default();
            let aw = ak
                .iter()
                .find(|k| k.kind() == SyntaxKind::Range)
                .map(|r| self.range_width(r, env))
                .or_else(|| {
                    ak.iter()
                        .filter(|k| k.kind() == SyntaxKind::Ident)
                        .find_map(|k| type_kw_width(&self.text(k)))
                })
                .unwrap_or(1);
            args.push((aname.clone(), aw, String::new()));
            slots.push((aname, aw));
        }
        args
    }

    /// Locals: one `SignalIr` per declared name, registered in `slots` too.
    fn func_locals(
        &mut self,
        local_nodes: &[SyntaxNode],
        env: &HashMap<String, Value>,
        slots: &mut Vec<(String, usize)>,
    ) -> Vec<SignalIr> {
        let mut locals = Vec::new();
        for d in local_nodes {
            let dk = real_children(d);
            let width = dk
                .iter()
                .find(|k| k.kind() == SyntaxKind::Range)
                .map(|r| self.range_width(r, env))
                .or_else(|| {
                    dk.iter()
                        .filter(|k| k.kind() == SyntaxKind::Ident)
                        .find_map(|k| type_kw_width(&self.text(k)))
                })
                .unwrap_or(1);
            for k in &dk {
                if k.kind() == SyntaxKind::Name {
                    let n = self.text(k);
                    locals.push(SignalIr {
                        name: n.clone(),
                        width,
                        kind: SigKind::Reg,
                        init: None,
                        mem: None,
                        signed: false,
                    });
                    slots.push((n, width));
                }
            }
        }
        locals
    }

    /// Lower the body in slot space: names map to slot indexes while the
    /// body lowers, then the previous scope is restored.
    fn func_body_slots(
        &mut self,
        slots: &[(String, usize)],
        body_nodes: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Stmt {
        let saved_names = std::mem::take(&mut self.names);
        self.names = slots
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i as SigId))
            .collect();
        let saved_in_fn = self.in_function;
        self.in_function = true;
        let body = Stmt::Block(body_nodes.iter().map(|s| self.lower_stmt(s, env)).collect());
        self.in_function = saved_in_fn;
        self.names = saved_names;
        body
    }
}
