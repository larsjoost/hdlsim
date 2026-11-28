//! Module-item lowering: the per-item dispatch (`lower_module_item`), SVA
//! `assert property` processes, late Decl/ParamDecl handling inside
//! generate blocks, continuous `assign`, generate-block item recursion,
//! declaration init application, and `parameter` declarations. Split out
//! of `verilog_lower.rs` to keep it at its standards baseline.

use std::collections::HashMap;

use super::{const_eval, is_expr_node, is_module_item, real_children, Lower, TypeDef, TypeKind};
use crate::ir::{
    AssignIr, Edge, Expr, ModuleIr, ParamIr, ProcKind, ProcessIr, Sens, SigId, SigKind, SignalIr,
    Stmt,
};
use crate::value::Value;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::Diagnostic;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    pub(super) fn lower_module_item(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        match item.kind() {
            SyntaxKind::AssertProperty => self.lower_assert_property(module, item, env),
            SyntaxKind::AssumeProperty | SyntaxKind::CoverProperty => {
                self.warn_unsimulated_property(item);
            }
            SyntaxKind::Port | SyntaxKind::Decl | SyntaxKind::ParamDecl => {
                self.lower_late_decl(module, item, env);
            }
            // Handled in the pre-pass.
            SyntaxKind::Typedef | SyntaxKind::Import => {}
            SyntaxKind::Class | SyntaxKind::Package => {}
            SyntaxKind::Assign => self.lower_cont_assign(module, item, env),
            SyntaxKind::Always | SyntaxKind::Initial => self.lower_process(module, item, env),
            SyntaxKind::Function => self.lower_function(module, item, env),
            SyntaxKind::Generate => self.lower_generate(module, item, env),
            SyntaxKind::GenFor => self.lower_gen_for(module, item, env),
            SyntaxKind::GenIf => self.lower_gen_if(module, item, env),
            SyntaxKind::Inst => self.lower_inst(module, item, env),
            _ => self.err(
                item.span(),
                format!("unsupported module item {:?}", item.kind()),
            ),
        }
    }

    /// SVA-lite: parsed, reported, not simulated.
    fn warn_unsimulated_property(&mut self, item: &SyntaxNode) {
        // SVA-lite: parsed, reported, not simulated.
        let what = if item.kind() == SyntaxKind::AssumeProperty {
            "assume"
        } else {
            "cover"
        };
        self.diags.push(Diagnostic::warning(
            "W0401",
            Some(item.span()),
            format!("{} property is parsed but not simulated", what),
        ));
    }

    /// Port/Decl/ParamDecl items reaching the item loop: handled in the
    /// pre-pass / param pass at module level; re-lowering a Decl for its
    /// init value. A ParamDecl inside a generate block was not covered by
    /// the pre-pass, so lower it now.
    fn lower_late_decl(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        // Handled in the pre-pass / param pass at module level;
        // re-lowering a Decl for its init value. A ParamDecl inside a
        // generate block was not covered by the pre-pass, so lower it now.
        if item.kind() == SyntaxKind::Decl {
            // Declarations inside generate blocks were not registered
            // by the module-level pre-pass: register them now.
            let dk = real_children(item);
            let width = self.decl_width(&dk, env);
            let tname = self.decl_type_name(&dk);
            for k in &dk {
                if k.kind() == SyntaxKind::Name {
                    let n = self.text(k);
                    if self.sig_id(&n).is_none() {
                        let id = module.signals.len() as SigId;
                        module.signals.push(SignalIr {
                            name: n.clone(),
                            width: width.max(1),
                            kind: SigKind::Wire,
                            init: None,
                            mem: None,
                            signed: false,
                        });
                        if let Some(t) = &tname {
                            self.sig_types.insert(id, t.clone());
                        }
                        self.names.insert(n, id);
                    }
                }
            }
            self.lower_decl_init(module, item, env);
        } else if item.kind() == SyntaxKind::ParamDecl {
            self.lower_param(module, item, env);
        }
    }

    /// Continuous `assign lhs = rhs;` at module level.
    fn lower_cont_assign(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kids = real_children(item);
        let eq = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=");
        if let Some(eq) = eq {
            let lhs = kids[..eq].iter().find(|k| is_expr_node(k.kind()));
            let rhs = kids[eq + 1..].iter().find(|k| is_expr_node(k.kind()));
            match (lhs, rhs) {
                (Some(l), Some(r)) => module.assigns.push(AssignIr {
                    lhs: self.lower_expr(l, env),
                    rhs: self.lower_expr(r, env),
                }),
                _ => self.err(item.span(), "malformed assign"),
            }
        } else {
            self.err(item.span(), "assign without `=`");
        }
    }

    /// A `generate` block: recurse into its for/if constructs and items.
    fn lower_generate(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        for k in real_children(item) {
            match k.kind() {
                SyntaxKind::GenFor => self.lower_gen_for(module, &k, env),
                SyntaxKind::GenIf => self.lower_gen_if(module, &k, env),
                SyntaxKind::GenBlock => {
                    for k2 in real_children(&k) {
                        if is_module_item(k2.kind()) {
                            self.lower_module_item(module, &k2, env);
                        }
                    }
                }
                _ if is_module_item(k.kind()) => self.lower_module_item(module, &k, env),
                _ => {}
            }
        }
    }

    /// `assert property (@(sens) ante |-> cons);` lowers to an always process:
    /// `|->` checks `cons` at the same edge, `|=>` waits one more edge; a plain
    /// property checks its expr at every edge. The check itself is the
    /// `$assert` system task (kernel turns a false condition into TestFailed).
    fn lower_assert_property(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kids = real_children(item);
        let sens_node = kids.iter().find(|k| k.kind() == SyntaxKind::SensList);
        let prop_node = kids.iter().find(|k| k.kind() == SyntaxKind::Property);
        let (Some(sens_node), Some(prop_node)) = (sens_node, prop_node) else {
            self.err(item.span(), "malformed assert property");
            return;
        };
        let sens = self.lower_sens_list(sens_node, env);
        let Some(edges) = self.assert_clock_edges(&sens, prop_node) else {
            return;
        };
        let Some(body) = self.assert_body(prop_node, env, edges) else {
            return;
        };
        module.processes.push(ProcessIr {
            kind: ProcKind::Always,
            sens,
            body,
            origin: format!("assert property {}", self.text(prop_node)),
            gen_scope: self.gen_path.join("."),
            initial: false,
        });
    }

    /// The `@(...) `clocking event as an edge list; `None` (with a
    /// diagnostic) for `@*`, which has no meaning for a property.
    fn assert_clock_edges(&mut self, sens: &Sens, prop_node: &SyntaxNode) -> Option<Vec<Edge>> {
        let edges = match sens {
            Sens::List(es) => es.clone(),
            Sens::All => {
                self.err(
                    prop_node.span(),
                    "assert property needs an explicit `@(...)` clocking event",
                );
                return None;
            }
        };
        Some(edges)
    }

    /// The property body: one expr checked at every edge, or `ante |-> cons`
    /// (same edge) / `ante |=> cons` (next edge) for two.
    fn assert_body(
        &mut self,
        prop_node: &SyntaxNode,
        env: &HashMap<String, Value>,
        edges: Vec<Edge>,
    ) -> Option<Stmt> {
        let pk = real_children(prop_node);
        let exprs: Vec<&SyntaxNode> = pk.iter().filter(|k| is_expr_node(k.kind())).collect();
        let op: String = pk
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Punct)
            .map(|k| self.text(k))
            .collect();
        // NB: syscall names are stored without the leading `$`. `sva_assert`
        // (not `assert`, which is VHDL's severity-gated form) always fails on
        // a false condition, per SVA.
        let check = |cons: Expr| Stmt::SysCall {
            name: "sva_assert".to_string(),
            args: vec![cons],
            shapes: Vec::new(),
        };
        let body = match exprs.len() {
            1 => check(self.lower_expr(exprs[0], env)),
            2 => {
                let ante = self.lower_expr(exprs[0], env);
                let cons = self.lower_expr(exprs[1], env);
                if op.contains('-') {
                    // `|->`: overlapping — consequent holds at the same edge.
                    Stmt::If {
                        cond: ante,
                        then: Box::new(check(cons)),
                        els: None,
                    }
                } else {
                    // `|=>`: consequent holds at the next edge.
                    Stmt::If {
                        cond: ante,
                        then: Box::new(Stmt::Block(vec![Stmt::Wait {
                            sens: edges,
                            body: Box::new(check(cons)),
                        }])),
                        els: None,
                    }
                }
            }
            _ => {
                self.err(prop_node.span(), "malformed property expression");
                return None;
            }
        };
        Some(body)
    }

    /// Apply init values from a declaration (names/widths were pre-registered).
    fn lower_decl_init(
        &mut self,
        module: &mut ModuleIr,
        d: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kids = real_children(d);
        let mut i = 1;
        while i < kids.len() {
            if kids[i].kind() == SyntaxKind::Name {
                let name = self.text(&kids[i]);
                i = self.decl_init_one(module, &kids, i, &name, env);
            } else {
                i += 1;
            }
        }
    }

    /// Handle one declared name at `i`; returns the next scan index.
    fn decl_init_one(
        &mut self,
        module: &mut ModuleIr,
        kids: &[SyntaxNode],
        i: usize,
        name: &str,
        env: &HashMap<String, Value>,
    ) -> usize {
        // Optional memory dimension after the name: `reg [7:0] mem [0:255]`.
        if kids
            .get(i + 1)
            .is_some_and(|k| k.kind() == SyntaxKind::Range)
        {
            self.decl_init_mem(module, name, &kids[i + 1], env);
            return i + 2;
        }
        // Optional `= expr` after the name.
        if kids.get(i + 1).is_some_and(|k| self.text(k) == "=") {
            if let Some(e) = kids.get(i + 2) {
                if let Some(step) = self.decl_init_assign(module, kids, name, e, env) {
                    return i + step;
                }
            }
        }
        i + 1
    }

    /// Record the memory dimension of the declared name.
    fn decl_init_mem(
        &mut self,
        module: &mut ModuleIr,
        name: &str,
        r: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let depth = self.range_depth(r, env);
        if let Some(id) = self.sig_id(name) {
            module.signals[id as usize].mem = Some(depth);
        }
    }

    /// The `= expr` value: `new(...)` constructor or a constant-folded init.
    /// Returns the scan step (3) when consumed, `None` to fall through.
    fn decl_init_assign(
        &mut self,
        module: &mut ModuleIr,
        dk: &[SyntaxNode],
        name: &str,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<usize> {
        if e.kind() == SyntaxKind::Allocator {
            self.decl_init_new(module, dk, name, e, env);
            return Some(3);
        }
        if is_expr_node(e.kind()) {
            self.decl_init_expr(module, name, e, env);
            return Some(3);
        }
        None
    }

    /// `C c = new(args);` — emit a one-shot initial process performing the
    /// `$new` call (see the extracted comment block below).
    fn decl_init_new(
        &mut self,
        module: &mut ModuleIr,
        dk: &[SyntaxNode],
        name: &str,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        // `C c = new(args);` — runtime constructor: emit a
        // one-shot initial process performing the $new call.
        let cn = self
            .decl_type_name(dk)
            .and_then(|t| self.types.get(&t).cloned())
            .and_then(|td| match td.kind {
                TypeKind::Class(cn) => Some(cn),
                _ => None,
            });
        if let (Some(cn), Some(id)) = (cn, self.sig_id(name)) {
            let mut args = vec![Expr::Sig(id)];
            args.extend(
                real_children(e)
                    .iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, env)),
            );
            module.processes.push(ProcessIr {
                kind: ProcKind::Initial,
                sens: Sens::List(Vec::new()),
                body: Stmt::SysCall {
                    name: format!("$new::{}", cn),
                    args,
                    shapes: Vec::new(),
                },
                origin: format!("{} = new(...)", name),
                gen_scope: self.gen_path.join("."),
                initial: true,
            });
        }
    }

    /// A plain expression init: constant-fold it into the signal slot.
    fn decl_init_expr(
        &mut self,
        module: &mut ModuleIr,
        name: &str,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let expr = self.lower_expr(e, env);
        if let Some(id) = self.sig_id(name) {
            let w = module.signals[id as usize].width.max(1);
            module.signals[id as usize].init = const_eval(&expr, env).map(|v| v.resize(w));
        }
    }

    pub(super) fn lower_param(
        &mut self,
        module: &mut ModuleIr,
        p: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kids = real_children(p);
        // `parameter type T = ...` declares a type, not a value constant.
        let is_type_param = kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "type");
        let mut i = 1;
        while i < kids.len() {
            if kids[i].kind() == SyntaxKind::Name
                && kids.get(i + 1).is_some_and(|k| self.text(k) == "=")
            {
                if is_type_param {
                    self.param_type_def(&kids, i, env);
                    i += 3;
                    continue;
                }
                if let Some(e) = kids.get(i + 2) {
                    if is_expr_node(e.kind()) {
                        self.param_value(module, &kids[i], e, env);
                        i += 3;
                        continue;
                    }
                }
            }
            i += 1;
        }
    }

    /// `parameter type T = struct/enum/width`: register the type so the
    /// name can be used as a value (type-param passing) and for struct
    /// field selects.
    fn param_type_def(&mut self, kids: &[SyntaxNode], i: usize, env: &HashMap<String, Value>) {
        // `parameter type rule_t = struct packed {...}` or
        // `parameter type t = logic [7:0]`: register the type so the
        // name can be used as a value (type-param passing) and for
        // struct field selects.
        let pname = self.text(&kids[i]);
        match kids.get(i + 2).map(|k| k.kind()) {
            Some(SyntaxKind::StructType) => {
                let td = self.struct_type_def(&kids[i + 2], env);
                self.types.insert(pname, td);
            }
            Some(SyntaxKind::EnumType) => {
                self.types.insert(
                    pname,
                    TypeDef {
                        width: 32,
                        signed: false,
                        real: false,
                        kind: TypeKind::Plain,
                    },
                );
            }
            _ => {
                let mut width = 1usize;
                if kids
                    .get(i + 3)
                    .is_some_and(|k| k.kind() == SyntaxKind::Range)
                {
                    width = self.range_width(&kids[i + 3], env);
                }
                self.types.insert(
                    pname,
                    TypeDef {
                        width,
                        signed: false,
                        real: false,
                        kind: TypeKind::Plain,
                    },
                );
            }
        }
    }

    /// One `name = expr` parameter value: push it (unless the module-level
    /// pass already did) and expose it as a constant to later items.
    fn param_value(
        &mut self,
        module: &mut ModuleIr,
        name_node: &SyntaxNode,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let expr = self.lower_expr(e, env);
        let val = self.ce(&expr, env).unwrap_or_else(|| Value::x(32));
        let pname = self.text(name_node);
        // The module-level param pass may have registered this
        // already (lower_module_item re-lowers ParamDecls inside
        // generate blocks) — don't push a duplicate.
        if !module.params.iter().any(|p| p.name == pname) {
            module.params.push(ParamIr {
                name: pname.clone(),
                default: val.clone(),
                ty: None,
            });
        }
        // Also visible as a constant to later items (e.g. decls
        // inside generate blocks that the pre-pass did not see).
        self.gconsts.insert(pname, val);
    }
}
