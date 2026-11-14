//! Verilog lowering: CST (from `hdl-syntax`) → module IR (`crate::ir`).
//!
//! Deterministic and total: every construct either lowers or produces a
//! diagnostic; nothing panics. Constant expressions (ranges, parameter
//! defaults, generate-for bounds, part-selects) are evaluated here with
//! [`const_eval`], with parameters/genvars supplied through an environment.
//!
//! Generate-for loops are unrolled at lowering time. Function bodies are
//! lowered in "slot space": signal id `i` refers to slot `i` of the function
//! (args first, then locals) — elaboration must not remap those ids.

use crate::ir::*;
use crate::value::{BinOp, UnOp, Value, V0, V1, VX, VZ};
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::{Diagnostic, Severity};
use hdl_syntax::kinds::SyntaxKind;
use hdl_syntax::source::{FileId, SourceDb, Span};
use std::collections::HashMap;

/// Lower one parsed file into modules plus diagnostics.
pub fn lower_file(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
) -> Vec<ModuleIr> {
    let _ = file; // spans on nodes already carry the file id
    let mut lx = Lower {
        db,
        diags,
        time_unit_fs: 1_000_000_000, // 1 ns default
        names: HashMap::new(),
    };
    let mut modules = Vec::new();
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Module {
            modules.push(lx.lower_module(&child));
        }
    }
    modules
}

pub(crate) fn real_children(n: &SyntaxNode) -> Vec<SyntaxNode> {
    n.children()
        .filter(|c| !matches!(c.kind(), SyntaxKind::Whitespace | SyntaxKind::Comment))
        .collect()
}

fn is_expr_node(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Literal
            | SyntaxKind::Name
            | SyntaxKind::BinExpr
            | SyntaxKind::UnaryExpr
            | SyntaxKind::CondExpr
            | SyntaxKind::Call
            | SyntaxKind::Concat
            | SyntaxKind::Replicate
            | SyntaxKind::PartSelect
            | SyntaxKind::Index
    )
}

fn is_module_item(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Port
            | SyntaxKind::Decl
            | SyntaxKind::ParamDecl
            | SyntaxKind::Assign
            | SyntaxKind::Always
            | SyntaxKind::Initial
            | SyntaxKind::Function
            | SyntaxKind::Generate
            | SyntaxKind::GenFor
            | SyntaxKind::GenIf
            | SyntaxKind::Inst
    )
}

fn is_stmt_node(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Block
            | SyntaxKind::If
            | SyntaxKind::Case
            | SyntaxKind::StmtAssign
            | SyntaxKind::Delay
            | SyntaxKind::SysCall
    )
}

struct Lower<'a> {
    db: &'a SourceDb,
    diags: &'a mut Vec<Diagnostic>,
    time_unit_fs: u64,
    /// Current scope's name → signal id map (module signals, or function slots).
    names: HashMap<String, SigId>,
}

impl<'a> Lower<'a> {
    fn err(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic {
            code: "E0401",
            severity: Severity::Error,
            span: Some(span),
            message: msg.into(),
            help: None,
        });
    }

    /// Node text, trimmed: `bump` attaches leading trivia to the node being
    /// started, so node spans may include surrounding whitespace. The CST stays
    /// lossless; readers trim.
    fn text(&self, n: &SyntaxNode) -> String {
        self.db.span_text(n.span()).trim().to_string()
    }

    fn sig_id(&self, name: &str) -> Option<SigId> {
        self.names.get(name).copied()
    }

    // ---- modules ------------------------------------------------------------

    fn lower_module(&mut self, m: &SyntaxNode) -> ModuleIr {
        let kids = real_children(m);
        let mut name = String::new();
        let mut ports_node = None;
        let mut item_nodes = Vec::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::Name if name.is_empty() => name = self.text(k),
                SyntaxKind::PortList => ports_node = Some(k.clone()),
                SyntaxKind::Ident => {} // `module` / `endmodule`
                _ if is_module_item(k.kind()) => item_nodes.push(k.clone()),
                _ => {} // header punctuation (`(`, `)`, `;`, `#`)
            }
        }
        let mut module = ModuleIr {
            name,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Vec::new(),
            assigns: Vec::new(),
            processes: Vec::new(),
            functions: Vec::new(),
            instances: Vec::new(),
            time_unit_fs: self.time_unit_fs,
        };
        // First pass: params (so ports and later items can use them in ranges).
        let mut env: HashMap<String, Value> = HashMap::new();
        for item in &item_nodes {
            if item.kind() == SyntaxKind::ParamDecl {
                self.lower_param(&mut module, item, &env);
                for p in &module.params {
                    env.insert(p.name.clone(), p.default.clone());
                }
            }
        }
        if let Some(pl) = &ports_node {
            let env_snapshot = env.clone();
            for p in real_children(pl) {
                if p.kind() == SyntaxKind::Port {
                    self.lower_port(&mut module, &p, &env_snapshot);
                }
            }
        }
        // Register all declared signal names up front so items can reference
        // them in any order (Verilog allows use-before-declare within a module).
        for item in &item_nodes {
            match item.kind() {
                SyntaxKind::Port => {
                    // Non-ANSI port declaration in the body.
                    let pk = real_children(item);
                    let width = pk
                        .iter()
                        .find(|k| k.kind() == SyntaxKind::Range)
                        .map(|r| self.range_width(r, &env))
                        .unwrap_or(1);
                    let dir = pk
                        .iter()
                        .find(|k| k.kind() == SyntaxKind::Ident)
                        .map(|k| self.text(k))
                        .unwrap_or_else(|| "input".to_string());
                    for k in &pk {
                        if k.kind() == SyntaxKind::Name {
                            let n = self.text(k);
                            let kind = match dir.as_str() {
                                "input" => SigKind::Input,
                                "output" => SigKind::Output,
                                _ => SigKind::Inout,
                            };
                            let id = module.signals.len() as SigId;
                            module.signals.push(SignalIr {
                                name: n.clone(),
                                width,
                                kind,
                                init: None,
                            });
                            module.ports.push(PortIr {
                                name: n,
                                dir: match dir.as_str() {
                                    "input" => Dir::In,
                                    "output" => Dir::Out,
                                    _ => Dir::Inout,
                                },
                                sig: id,
                            });
                        }
                    }
                }
                SyntaxKind::Decl => {
                    let dk = real_children(item);
                    let width = dk
                        .iter()
                        .find(|k| k.kind() == SyntaxKind::Range)
                        .map(|r| self.range_width(r, &env))
                        .unwrap_or(1);
                    for k in &dk {
                        if k.kind() == SyntaxKind::Name {
                            let n = self.text(k);
                            if self.sig_id(&n).is_none() {
                                let id = module.signals.len() as SigId;
                                module.signals.push(SignalIr {
                                    name: n,
                                    width,
                                    kind: SigKind::Wire,
                                    init: None,
                                });
                                self.names
                                    .insert(module.signals[id as usize].name.clone(), id);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Rebuild the name map from the module (ports + signals), then lower items.
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        for item in &item_nodes {
            self.lower_module_item(&mut module, item, &env);
        }
        module
    }

    fn lower_port(&mut self, module: &mut ModuleIr, p: &SyntaxNode, env: &HashMap<String, Value>) {
        let kids = real_children(p);
        let mut dir = Dir::In;
        let mut kind = SigKind::Wire;
        let mut width = 1usize;
        let mut name = String::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident => {
                    let t = self.text(k);
                    match t.as_str() {
                        "input" => {
                            dir = Dir::In;
                            kind = SigKind::Input;
                        }
                        "output" => {
                            dir = Dir::Out;
                            kind = SigKind::Output;
                        }
                        "inout" => {
                            dir = Dir::Inout;
                            kind = SigKind::Inout;
                        }
                        "reg" => kind = SigKind::Reg,
                        "wire" | "logic" => kind = SigKind::Wire,
                        _ => name = t,
                    }
                }
                SyntaxKind::Range => {
                    width = self.range_width(k, env);
                }
                SyntaxKind::Name => name = self.text(k),
                _ => {}
            }
        }
        if name.is_empty() {
            self.err(p.span(), "port without a name");
            return;
        }
        let id = module.signals.len() as SigId;
        module.signals.push(SignalIr {
            name: name.clone(),
            width,
            kind,
            init: None,
        });
        module.ports.push(PortIr { name, dir, sig: id });
    }

    fn range_width(&mut self, r: &SyntaxNode, env: &HashMap<String, Value>) -> usize {
        let exprs: Vec<Expr> = real_children(r)
            .into_iter()
            .filter(|c| is_expr_node(c.kind()))
            .map(|c| self.lower_expr(&c, env))
            .collect();
        if exprs.len() == 2 {
            if let (Some(a), Some(b)) = (const_eval(&exprs[0], env), const_eval(&exprs[1], env)) {
                if let (Some(a), Some(b)) = (a.to_u64(), b.to_u64()) {
                    return (a as i64 - b as i64).unsigned_abs() as usize + 1;
                }
            }
        } else if exprs.len() == 1 {
            if let Some(v) = const_eval(&exprs[0], env) {
                if let Some(w) = v.to_u64() {
                    return w as usize;
                }
            }
        }
        self.err(r.span(), "cannot evaluate range bounds to constants");
        1
    }

    fn lower_module_item(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        match item.kind() {
            SyntaxKind::Port | SyntaxKind::Decl | SyntaxKind::ParamDecl => {
                // Already handled in the pre-pass / param pass; re-lowering a Decl
                // for its init value:
                if item.kind() == SyntaxKind::Decl {
                    self.lower_decl_init(module, item, env);
                }
            }
            SyntaxKind::Assign => {
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
            SyntaxKind::Always | SyntaxKind::Initial => self.lower_process(module, item, env),
            SyntaxKind::Function => self.lower_function(module, item, env),
            SyntaxKind::Generate => {
                for k in real_children(item) {
                    match k.kind() {
                        SyntaxKind::GenFor => self.lower_gen_for(module, &k, env),
                        SyntaxKind::GenIf => self.lower_gen_if(module, &k, env),
                        _ => self.lower_module_item(module, &k, env),
                    }
                }
            }
            SyntaxKind::GenFor => self.lower_gen_for(module, item, env),
            SyntaxKind::GenIf => self.lower_gen_if(module, item, env),
            SyntaxKind::Inst => self.lower_inst(module, item, env),
            _ => self.err(
                item.span(),
                format!("unsupported module item {:?}", item.kind()),
            ),
        }
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
                // Optional `= expr` after the name.
                if kids.get(i + 1).map_or(false, |k| self.text(k) == "=") {
                    if let Some(e) = kids.get(i + 2) {
                        if is_expr_node(e.kind()) {
                            let expr = self.lower_expr(e, env);
                            if let Some(id) = self.sig_id(&name) {
                                module.signals[id as usize].init = const_eval(&expr, env);
                            }
                            i += 3;
                            continue;
                        }
                    }
                }
            }
            i += 1;
        }
    }

    fn lower_param(&mut self, module: &mut ModuleIr, p: &SyntaxNode, env: &HashMap<String, Value>) {
        let kids = real_children(p);
        let mut i = 1;
        while i < kids.len() {
            if kids[i].kind() == SyntaxKind::Name
                && kids.get(i + 1).map_or(false, |k| self.text(k) == "=")
            {
                if let Some(e) = kids.get(i + 2) {
                    if is_expr_node(e.kind()) {
                        let expr = self.lower_expr(e, env);
                        let val = const_eval(&expr, env).unwrap_or_else(|| Value::x(32));
                        module.params.push(ParamIr {
                            name: self.text(&kids[i]),
                            default: val,
                        });
                        i += 3;
                        continue;
                    }
                }
            }
            i += 1;
        }
    }

    // ---- processes ----------------------------------------------------------

    fn lower_process(
        &mut self,
        module: &mut ModuleIr,
        p: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kind = if p.kind() == SyntaxKind::Initial {
            ProcKind::Initial
        } else {
            ProcKind::Always
        };
        let kids = real_children(p);
        let mut sens = Sens::All;
        let mut body = None;
        let mut origin = String::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::SensList => {
                    sens = self.lower_sens_list(k, env);
                    origin = self.text(k);
                }
                SyntaxKind::Ident | SyntaxKind::Punct => {}
                _ => body = Some(k.clone()),
            }
        }
        let body = match body {
            Some(b) => self.lower_stmt(&b, env),
            None => Stmt::Null,
        };
        module.processes.push(ProcessIr {
            kind,
            sens,
            body,
            origin,
        });
    }

    fn lower_sens_list(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Sens {
        let mut edges = Vec::new();
        let mut pending = EdgeKind::Any;
        for c in real_children(s) {
            match c.kind() {
                SyntaxKind::Ident => {
                    let t = self.text(&c);
                    if t == "posedge" {
                        pending = EdgeKind::Pos;
                    } else if t == "negedge" {
                        pending = EdgeKind::Neg;
                    }
                    // `or` separators carry no meaning beyond joining the list.
                }
                _ if is_expr_node(c.kind()) => {
                    let e = self.lower_expr(&c, env);
                    match e {
                        Expr::Sig(id) => edges.push(Edge {
                            sig: id,
                            kind: pending,
                        }),
                        _ => self.err(c.span(), "sensitivity list entry must be a signal"),
                    }
                    pending = EdgeKind::Any;
                }
                _ => {}
            }
        }
        if edges.is_empty() {
            Sens::All
        } else {
            Sens::List(edges)
        }
    }

    // ---- statements -----------------------------------------------------------

    fn lower_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        match s.kind() {
            SyntaxKind::Block => Stmt::Block(
                real_children(s)
                    .into_iter()
                    .filter(|k| !matches!(k.kind(), SyntaxKind::Ident | SyntaxKind::Punct))
                    .map(|k| self.lower_stmt(&k, env))
                    .collect(),
            ),
            SyntaxKind::If => {
                let kids = real_children(s);
                let cond_node = kids.iter().find(|k| is_expr_node(k.kind()));
                let stmt_nodes: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_stmt_node(k.kind())).collect();
                let cond = cond_node
                    .map(|c| self.lower_expr(c, env))
                    .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0)));
                let then = stmt_nodes
                    .first()
                    .map(|b| Box::new(self.lower_stmt(b, env)))
                    .unwrap_or_else(|| Box::new(Stmt::Null));
                let els = stmt_nodes.get(1).map(|b| Box::new(self.lower_stmt(b, env)));
                Stmt::If { cond, then, els }
            }
            SyntaxKind::Case => self.lower_case(s, env),
            SyntaxKind::StmtAssign => {
                let kids = real_children(s);
                let body: Vec<&SyntaxNode> = kids
                    .iter()
                    .filter(|k| !(k.kind() == SyntaxKind::Punct && self.text(k) == ";"))
                    .collect();
                let op_pos = body.iter().position(|k| {
                    k.kind() == SyntaxKind::Punct && matches!(self.text(k).as_str(), "=" | "<=")
                });
                match op_pos {
                    Some(op) => {
                        let nba = self.text(body[op]) == "<=";
                        let lhs = body[..op].iter().find(|k| is_expr_node(k.kind()));
                        let rhs = body[op + 1..].iter().find(|k| is_expr_node(k.kind()));
                        match (lhs, rhs) {
                            (Some(l), Some(r)) => Stmt::Assign {
                                lhs: self.lower_expr(l, env),
                                rhs: self.lower_expr(r, env),
                                nba,
                            },
                            _ => Stmt::Null,
                        }
                    }
                    None => match body.iter().find(|k| is_expr_node(k.kind())) {
                        Some(e) => Stmt::ExprStmt(self.lower_expr(e, env)),
                        None => Stmt::Null,
                    },
                }
            }
            SyntaxKind::Delay => {
                let kids = real_children(s);
                let value = kids.iter().find(|k| is_expr_node(k.kind()));
                let stmt = kids.iter().find(|k| is_stmt_node(k.kind()));
                let time_fs = value
                    .map(|v| self.lower_expr(v, env))
                    .and_then(|e| const_eval(&e, env))
                    .and_then(|v| v.to_u64())
                    .map(|t| t * self.time_unit_fs)
                    .unwrap_or(0);
                let stmt = stmt
                    .map(|b| Box::new(self.lower_stmt(b, env)))
                    .unwrap_or_else(|| Box::new(Stmt::Null));
                Stmt::Delay { time_fs, stmt }
            }
            SyntaxKind::SysCall => {
                let kids = real_children(s);
                let name = kids
                    .first()
                    .map(|k| self.text(k).trim_start_matches('$').to_string())
                    .unwrap_or_default();
                let args = kids[1..]
                    .iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, env))
                    .collect();
                Stmt::SysCall { name, args }
            }
            _ => Stmt::Null,
        }
    }

    fn lower_case(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let expr = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, env))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0)));
        let mut items = Vec::new();
        let mut default = None;
        for item in kids.iter().filter(|k| k.kind() == SyntaxKind::CaseItem) {
            let ik = real_children(item);
            let is_default = ik
                .iter()
                .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "default");
            let conds = ik
                .iter()
                .filter(|k| is_expr_node(k.kind()))
                .map(|k| self.lower_expr(k, env))
                .collect::<Vec<_>>();
            let body = ik
                .iter()
                .find(|k| is_stmt_node(k.kind()))
                .map(|k| self.lower_stmt(k, env))
                .unwrap_or(Stmt::Null);
            if is_default {
                default = Some(Box::new(body));
            } else {
                items.push((conds, body));
            }
        }
        Stmt::Case {
            expr,
            items,
            default,
        }
    }

    // ---- generate -------------------------------------------------------------

    fn lower_gen_for(
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
        let body = kids
            .iter()
            .find(|k| is_stmt_node(k.kind()) || k.kind() == SyntaxKind::Block);
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
            .filter(|k| is_expr_node(k.kind()))
            .last()
            .map(|k| self.lower_expr(k, env))
            .and_then(|e| const_eval(&e, env))
            .and_then(|v| v.to_u64());
        let Some(mut i) = init_val else {
            self.err(g.span(), "generate for: init value not constant");
            return;
        };
        let step = &assigns[1];
        let step_expr = {
            let sk = real_children(step);
            sk.iter()
                .filter(|k| is_expr_node(k.kind()))
                .last()
                .map(|k| self.lower_expr(k, env))
        };
        let body_node = body.unwrap().clone();
        let mut iter = 0u32;
        loop {
            if iter > 1_000_000 {
                self.err(g.span(), "generate for: too many iterations (>1M)");
                return;
            }
            iter += 1;
            let mut env2 = env.clone();
            env2.insert(var.clone(), Value::from_u64(32, i));
            let go = cond_node
                .map(|c| self.lower_expr(c, &env2))
                .and_then(|e| const_eval(&e, &env2))
                .and_then(|v| v.to_u64())
                .map_or(false, |v| v != 0);
            if !go {
                break;
            }
            match body_node.kind() {
                SyntaxKind::Block => {
                    for k in real_children(&body_node) {
                        if matches!(k.kind(), SyntaxKind::Ident | SyntaxKind::Punct) {
                            continue;
                        }
                        self.lower_module_item(module, &k, &env2);
                    }
                }
                _ => self.lower_module_item(module, &body_node, &env2),
            }
            match &step_expr {
                Some(Expr::Bin { op, l, r }) => {
                    let lv = const_eval(l, &env2).and_then(|v| v.to_u64());
                    let rv = const_eval(r, &env2).and_then(|v| v.to_u64());
                    match (op, lv, rv) {
                        (BinOp::Add, Some(a), Some(b)) => i = a.wrapping_add(b),
                        (BinOp::Sub, Some(a), Some(b)) => i = a.wrapping_sub(b),
                        _ => {
                            self.err(g.span(), "generate for: step not a constant +/-");
                            return;
                        }
                    }
                }
                _ => {
                    self.err(g.span(), "generate for: unsupported step");
                    return;
                }
            }
        }
    }

    fn lower_gen_if(
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
            .map_or(false, |v| v != 0);
        let bodies: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| is_stmt_node(k.kind()) || k.kind() == SyntaxKind::Block)
            .collect();
        let chosen = if take { bodies.first() } else { bodies.get(1) };
        if let Some(b) = chosen {
            self.lower_module_item(module, b, env);
        }
    }

    // ---- functions / instances -------------------------------------------------

    fn lower_function(
        &mut self,
        module: &mut ModuleIr,
        f: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
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
                SyntaxKind::Port => arg_nodes.push(k.clone()),
                SyntaxKind::Decl => local_nodes.push(k.clone()),
                _ if is_stmt_node(k.kind()) => body_nodes.push(k.clone()),
                _ => {}
            }
        }
        let mut args = Vec::new();
        let mut slots: Vec<(String, usize)> = Vec::new();
        for a in &arg_nodes {
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
                .unwrap_or(1);
            args.push((aname.clone(), aw));
            slots.push((aname, aw));
        }
        let mut locals = Vec::new();
        for d in &local_nodes {
            let dk = real_children(d);
            let width = dk
                .iter()
                .find(|k| k.kind() == SyntaxKind::Range)
                .map(|r| self.range_width(r, env))
                .unwrap_or(1);
            for k in &dk {
                if k.kind() == SyntaxKind::Name {
                    let n = self.text(k);
                    locals.push(SignalIr {
                        name: n.clone(),
                        width,
                        kind: SigKind::Reg,
                        init: None,
                    });
                    slots.push((n, width));
                }
            }
        }
        // Lower the body in slot space.
        let saved_names = std::mem::take(&mut self.names);
        self.names = slots
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i as SigId))
            .collect();
        let body = Stmt::Block(body_nodes.iter().map(|s| self.lower_stmt(s, env)).collect());
        self.names = saved_names;
        module.functions.push(FuncIr {
            name,
            ret_width,
            args,
            locals,
            body,
        });
    }

    fn lower_inst(
        &mut self,
        module: &mut ModuleIr,
        inst: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kids = real_children(inst);
        let mut module_name = String::new();
        let mut inst_name = String::new();
        let mut conns = Vec::new();
        let mut param_overrides = Vec::new();
        let mut i = 0;
        if kids.first().map_or(false, |k| k.kind() == SyntaxKind::Name) {
            module_name = self.text(&kids[0]);
            i = 1;
        }
        // `#(...)` parameter overrides arrive as ParamAssign nodes.
        if i < kids.len() && self.text(&kids[i]) == "#" {
            i += 1; // `#`
        }
        if i < kids.len() && kids[i].kind() == SyntaxKind::Name {
            inst_name = self.text(&kids[i]);
            i += 1;
        }
        for c in &kids[i..] {
            match c.kind() {
                SyntaxKind::ParamAssign => param_overrides.push(self.lower_param_assign(c, env)),
                SyntaxKind::PortConn => conns.push(self.lower_conn(c, env)),
                _ => {}
            }
        }
        if inst_name.is_empty() {
            self.err(inst.span(), "instance without a name");
            return;
        }
        module.instances.push(InstIr {
            module: module_name,
            name: inst_name,
            param_overrides,
            conns,
        });
    }

    /// `.name(expr)` or positional — from a ParamAssign node.
    fn lower_param_assign(
        &mut self,
        c: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> (String, Expr) {
        let kids = real_children(c);
        let name = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .unwrap_or_default();
        let expr = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, env))
            .unwrap_or(Expr::Lit(Value::x(1)));
        (name, expr)
    }

    fn lower_conn(&mut self, c: &SyntaxNode, env: &HashMap<String, Value>) -> Conn {
        let kids = real_children(c);
        let mut name = None;
        let mut expr = None;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Name => name = Some(self.text(k)),
                _ if is_expr_node(k.kind()) => expr = Some(self.lower_expr(k, env)),
                _ => {}
            }
        }
        match (name, expr) {
            (Some(n), Some(e)) => Conn::Named(n, e),
            (None, Some(e)) => Conn::Positional(e),
            (Some(n), None) => Conn::Named(n, Expr::Lit(Value::x(1))),
            (None, None) => Conn::Positional(Expr::Lit(Value::x(1))),
        }
    }

    // ---- expressions ------------------------------------------------------------

    fn lower_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        match e.kind() {
            SyntaxKind::Literal => self.lower_literal(e),
            SyntaxKind::Name => {
                let name = self.text(e);
                let base = name.rsplit('.').next().unwrap_or(name.as_str());
                if let Some(v) = env.get(base) {
                    return Expr::Lit(v.clone());
                }
                match self.sig_id(base) {
                    Some(id) => Expr::Sig(id),
                    None => {
                        self.err(e.span(), format!("unknown identifier '{}'", base));
                        Expr::Lit(Value::x(1))
                    }
                }
            }
            SyntaxKind::BinExpr => {
                let kids = real_children(e);
                let expr_nodes: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
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
            SyntaxKind::UnaryExpr => {
                let kids = real_children(e);
                let op_text: String = kids
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Punct)
                    .map(|k| self.text(k))
                    .collect();
                let operand = kids.iter().find(|k| is_expr_node(k.kind()));
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
            SyntaxKind::CondExpr => {
                let kids = real_children(e);
                let exprs: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
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
            SyntaxKind::Call => {
                let kids = real_children(e);
                let name = kids
                    .first()
                    .map(|k| self.text(k).trim_start_matches('$').to_string())
                    .unwrap_or_default();
                let args = kids[1..]
                    .iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, env))
                    .collect();
                Expr::Call { name, args }
            }
            SyntaxKind::Concat => Expr::Concat(
                real_children(e)
                    .into_iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(&k, env))
                    .collect(),
            ),
            SyntaxKind::Replicate => {
                let kids = real_children(e);
                let exprs: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
                if exprs.len() == 2 {
                    Expr::Replicate {
                        count: Box::new(self.lower_expr(exprs[0], env)),
                        e: Box::new(self.lower_expr(exprs[1], env)),
                    }
                } else {
                    Expr::Lit(Value::x(1))
                }
            }
            SyntaxKind::PartSelect => {
                let kids = real_children(e);
                let exprs: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
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
            SyntaxKind::Index => {
                let kids = real_children(e);
                let exprs: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
                if exprs.len() == 2 {
                    Expr::Idx {
                        e: Box::new(self.lower_expr(exprs[0], env)),
                        i: Box::new(self.lower_expr(exprs[1], env)),
                    }
                } else {
                    Expr::Lit(Value::x(1))
                }
            }
            _ => {
                self.err(e.span(), format!("unsupported expression {:?}", e.kind()));
                Expr::Lit(Value::x(1))
            }
        }
    }

    fn lower_literal(&mut self, l: &SyntaxNode) -> Expr {
        let t = self.text(l);
        if t.starts_with('"') {
            // String literal: bytes as a value, first char in the low byte.
            let inner = t.trim_matches('"');
            let bytes = inner.as_bytes();
            let mut bits = vec![V0; bytes.len().max(1) * 8];
            for (bi, &b) in bytes.iter().enumerate() {
                for j in 0..8 {
                    bits[bi * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
                }
            }
            return Expr::Lit(Value { bits });
        }
        match parse_int_literal(&t) {
            Some(v) => Expr::Lit(v),
            None => {
                self.err(l.span(), format!("unsupported literal '{}'", t));
                Expr::Lit(Value::x(32))
            }
        }
    }
}

fn binop_from_str(s: &str) -> Option<BinOp> {
    Some(match s {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "%" => BinOp::Mod,
        "<<" => BinOp::Shl,
        ">>" => BinOp::Shr,
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

/// Parse a Verilog integer literal (`42`, `8'hFF`, `'b1010`, `4'd3`) into a
/// Value with per-bit X/Z for x/z/? digits. Returns None for non-integers.
pub fn parse_int_literal(t: &str) -> Option<Value> {
    let t = t.trim();
    let (size, rest) = match t.find('\'') {
        Some(q) => {
            let (sz, r) = t.split_at(q);
            let size = if sz.is_empty() {
                None
            } else {
                Some(sz.parse::<u64>().ok()?)
            };
            (size, &r[1..])
        }
        None => (None, t),
    };
    let rest = rest.trim_start_matches(['s', 'S']);
    let (base, digits) = match rest.chars().next()? {
        'b' | 'B' => (2u32, &rest[1..]),
        'o' | 'O' => (8, &rest[1..]),
        'd' | 'D' => (10, &rest[1..]),
        'h' | 'H' => (16, &rest[1..]),
        c if c.is_ascii_digit() => (10, rest),
        _ => return None,
    };
    let digits = digits.replace('_', "");
    if digits.is_empty() {
        return None;
    }
    let width =
        size.map(|s| s as usize)
            .unwrap_or_else(|| match u128::from_str_radix(&digits, base) {
                Ok(v) => (128 - v.leading_zeros() as usize).max(1),
                Err(_) => digits.len() * 4, // has x/z digits; approximate
            });
    if !digits
        .chars()
        .any(|c| matches!(c.to_ascii_lowercase(), 'x' | 'z' | '?'))
    {
        let v = u128::from_str_radix(&digits, base).ok()?;
        let mut bits = vec![V0; width.max(1)];
        for i in 0..width.min(128) {
            bits[i] = if (v >> i) & 1 == 1 { V1 } else { V0 };
        }
        return Some(Value { bits });
    }
    let digit_bits = match base {
        2 => 1,
        8 => 3,
        16 => 4,
        _ => 1,
    };
    let mut bits = vec![VX; width.max(1)];
    for (i, c) in digits.chars().enumerate() {
        let from_lsb = (digits.len() - 1 - i) * digit_bits;
        match c.to_ascii_lowercase() {
            'x' | '?' => {
                for j in 0..digit_bits {
                    let idx = from_lsb + j;
                    if idx < bits.len() {
                        bits[idx] = VX;
                    }
                }
            }
            'z' => {
                for j in 0..digit_bits {
                    let idx = from_lsb + j;
                    if idx < bits.len() {
                        bits[idx] = VZ;
                    }
                }
            }
            _ => {
                if let Some(d) = c.to_digit(base) {
                    for j in 0..digit_bits {
                        let idx = from_lsb + j;
                        if idx < bits.len() {
                            bits[idx] = if (d >> j) & 1 == 1 { V1 } else { V0 };
                        }
                    }
                }
            }
        }
    }
    Some(Value { bits })
}

/// Constant-evaluate an expression with an environment of named constants.
pub fn const_eval(e: &Expr, env: &HashMap<String, Value>) -> Option<Value> {
    match e {
        Expr::Lit(v) => Some(v.clone()),
        Expr::Sig(_) => None,
        Expr::Bin { op, l, r } => Some(const_eval(l, env)?.binop(*op, &const_eval(r, env)?)),
        Expr::Un { op, e } => Some(const_eval(e, env)?.unop(*op)),
        Expr::Cond { c, t, e } => {
            if const_eval(c, env)?.to_u64()? != 0 {
                const_eval(t, env)
            } else {
                const_eval(e, env)
            }
        }
        Expr::Concat(parts) => {
            let vals: Option<Vec<Value>> = parts.iter().map(|p| const_eval(p, env)).collect();
            Some(Value::concat(&vals?))
        }
        Expr::Replicate { count, e } => {
            let n = const_eval(count, env)?.to_u64()? as usize;
            let v = const_eval(e, env)?;
            Some(Value::concat(&vec![v; n]))
        }
        Expr::Sel { e, msb, lsb } => {
            let v = const_eval(e, env)?;
            let msb = const_eval(msb, env)?.to_u64()? as usize;
            let lsb = const_eval(lsb, env)?.to_u64()? as usize;
            Some(v.slice(msb, lsb))
        }
        Expr::Idx { e, i } => {
            let v = const_eval(e, env)?;
            let i = const_eval(i, env)?.to_u64()? as usize;
            Some(Value::from_bit(v.bit(i)))
        }
        Expr::Call { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_syntax::source::SourceDb;
    use hdl_syntax::verilog::parser::parse_file;

    fn lower_src(src: &str) -> (Vec<ModuleIr>, SourceDb, Vec<Diagnostic>) {
        let mut db = SourceDb::new();
        let id = db.add_file("t.v", src).unwrap();
        let parsed = parse_file(id, db.file(id));
        let mut diags = parsed.diags;
        let modules = lower_file(id, &parsed.root, &db, &mut diags);
        (modules, db, diags)
    }

    #[test]
    fn lowers_counter_module() {
        let src = r#"module counter(input clk, input rst, output reg [7:0] q);
always @(posedge clk) begin
  if (rst) q <= 8'h00;
  else q <= q + 1;
end
endmodule
"#;
        let (modules, _db, diags) = lower_src(src);
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errs.is_empty(),
            "{:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let m = &modules[0];
        assert_eq!(m.name, "counter");
        assert_eq!(m.ports.len(), 3);
        assert_eq!(m.processes.len(), 1);
        let p = &m.processes[0];
        assert!(matches!(p.sens, Sens::List(ref e) if e.len() == 1 && e[0].kind == EdgeKind::Pos));
        // q is 8 bits
        let q = m.signals.iter().find(|s| s.name == "q").unwrap();
        assert_eq!(q.width, 8);
    }

    #[test]
    fn lowers_assign_and_params() {
        let src = "module m #(parameter W = 8) (input [W-1:0] a, output [W-1:0] y);\nassign y = a;\nendmodule\n";
        let (modules, _db, diags) = lower_src(src);
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let m = &modules[0];
        assert_eq!(m.assigns.len(), 1);
        assert_eq!(m.params.len(), 1);
        let a = m.signals.iter().find(|s| s.name == "a").unwrap();
        assert_eq!(a.width, 8);
    }

    #[test]
    fn parse_int_literals() {
        assert_eq!(parse_int_literal("42").unwrap().to_u64(), Some(42));
        assert_eq!(parse_int_literal("8'hFF").unwrap().to_u64(), Some(255));
        assert_eq!(parse_int_literal("'b1010").unwrap().to_u64(), Some(10));
        assert_eq!(parse_int_literal("4'd3").unwrap().to_u64(), Some(3));
        let x = parse_int_literal("4'b10x1").unwrap();
        assert_eq!(x.to_u64(), None);
        assert_eq!(x.bit(1), VX);
    }
}
