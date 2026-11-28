//! Verilog module lowering: header scan and the ordered per-module passes —
//! params first (so ports and items can use them in ranges), imports and
//! typedefs, ANSI ports, classes, the use-before-declare signal pre-pass,
//! class memories and methods, then the item lowering itself. Split out of
//! `verilog_lower.rs` to keep it at its standards baseline.

use std::collections::HashMap;

use super::{is_module_item, real_children, Lower, TypeKind};
use crate::ir::{ArgMode, Dir, FuncIr, ModuleIr, PortIr, SigId, SigKind, SignalIr};
use crate::value::Value;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    /// Lower one module/package node: header scan, the ordered registration
    /// passes, item lowering, and the extra-signal drain.
    pub(super) fn lower_module(&mut self, m: &SyntaxNode) -> ModuleIr {
        let (name, ports_node, item_nodes) = self.module_header(m);
        let mut module = self.new_module_ir(name);
        // First pass: params (so ports and later items can use them in ranges).
        let mut env: HashMap<String, Value> = HashMap::new();
        self.module_param_pass(&mut module, &item_nodes, &mut env);
        // Imports and typedefs come before signal registration so user types
        // are known when declarations are scanned.
        self.module_typedef_pass(&mut module, m, &item_nodes, &env);
        self.module_ports_pass(&mut module, ports_node.as_ref(), &env);
        // Classes declared inside the module must be registered before the
        // signal pre-pass so class-typed declarations get their type.
        self.module_class_pass(&item_nodes);
        self.module_signal_prepass(&mut module, &item_nodes, &env);
        self.module_name_map(&module);
        self.module_class_mems(&item_nodes);
        let class_methods = self.class_method_funcs();
        module.functions.extend(class_methods);
        for item in &item_nodes {
            self.lower_module_item(&mut module, item, &env);
        }
        // Signals allocated while lowering statements (foreach index vars).
        for sig in self.extra_signals.drain(..) {
            let id = module.signals.len() as SigId;
            self.names.insert(sig.name.clone(), id);
            module.signals.push(sig);
        }
        module
    }

    /// Scan the module header: name, the `PortList`, and the module-item
    /// nodes (`module`/`endmodule` keywords and header punctuation skipped).
    fn module_header(&self, m: &SyntaxNode) -> (String, Option<SyntaxNode>, Vec<SyntaxNode>) {
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
        (name, ports_node, item_nodes)
    }

    /// Build the module IR and reset the per-module name/state maps.
    fn new_module_ir(&mut self, name: String) -> ModuleIr {
        let module = ModuleIr {
            name,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Vec::new(),
            assigns: Vec::new(),
            processes: Vec::new(),
            functions: Vec::new(),
            instances: Vec::new(),
            time_unit_fs: self.time_unit_fs,
            is_vhdl: false,
            real_ranges: HashMap::new(),
            sig_dims_debug: None,
        };
        // The name map is per-module: without this, a declaration in the second
        // module of a file would be skipped as "already known" from the first.
        self.names.clear();
        self.sig_types.clear();
        self.sig_is_mem.clear();
        self.class_mems.clear();
        self.inst_conns.clear();
        self.gconsts
            .insert("null".to_string(), Value::from_u64(64, u64::MAX));
        module
    }

    /// First pass: lower `parameter` declarations, then seed the item env
    /// and the qualified `Module::param` gconsts from them.
    fn module_param_pass(
        &mut self,
        module: &mut ModuleIr,
        item_nodes: &[SyntaxNode],
        env: &mut HashMap<String, Value>,
    ) {
        for item in item_nodes {
            if item.kind() == SyntaxKind::ParamDecl {
                self.lower_param(module, item, env);
                for p in &module.params {
                    env.insert(p.name.clone(), p.default.clone());
                    self.gconsts
                        .insert(format!("{}::{}", module.name, p.name), p.default.clone());
                }
            }
        }
    }

    /// Imports and typedefs come before signal registration so user types
    /// are known when declarations are scanned.
    fn module_typedef_pass(
        &mut self,
        module: &mut ModuleIr,
        m: &SyntaxNode,
        item_nodes: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) {
        let pkg_of = if m.kind() == SyntaxKind::Package {
            Some(module.name.clone())
        } else {
            None
        };
        for item in item_nodes {
            match item.kind() {
                SyntaxKind::Import => self.lower_import(module, item, env),
                SyntaxKind::Typedef => self.lower_typedef(item, env, pkg_of.as_deref()),
                // Bare enum declaration: `enum logic [2:0] { A, B } s;` —
                // register the labels like a typedef enum (module scope).
                SyntaxKind::Decl
                    if real_children(item)
                        .iter()
                        .any(|k| k.kind() == SyntaxKind::EnumType) =>
                {
                    self.lower_typedef(item, env, pkg_of.as_deref());
                }
                _ => {}
            }
        }
    }

    /// ANSI header ports: lower each `Port` against a snapshot of the
    /// parameter env.
    fn module_ports_pass(
        &mut self,
        module: &mut ModuleIr,
        ports_node: Option<&SyntaxNode>,
        env: &HashMap<String, Value>,
    ) {
        if let Some(pl) = ports_node {
            let env_snapshot = env.clone();
            for p in real_children(pl) {
                if p.kind() == SyntaxKind::Port {
                    self.lower_port(module, &p, &env_snapshot);
                }
            }
        }
    }

    /// Classes declared inside the module must be registered before the
    /// signal pre-pass so class-typed declarations get their type.
    fn module_class_pass(&mut self, item_nodes: &[SyntaxNode]) {
        for item in item_nodes {
            if item.kind() == SyntaxKind::Class {
                self.lower_class(item);
            }
        }
    }

    /// Register all declared signal names up front so items can reference
    /// them in any order (Verilog allows use-before-declare within a module).
    fn module_signal_prepass(
        &mut self,
        module: &mut ModuleIr,
        item_nodes: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) {
        for item in item_nodes {
            match item.kind() {
                SyntaxKind::Port => self.register_body_port(module, item, env),
                SyntaxKind::Decl => self.register_decl_signals(module, item, env),
                _ => {}
            }
        }
    }

    /// Non-ANSI port declaration in the body.
    fn register_body_port(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        // Non-ANSI port declaration in the body.
        let pk = real_children(item);
        let width = pk
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range)
            .map(|r| self.range_width(r, env))
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
                    mem: None,
                    signed: false,
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

    /// Register every declared name of a `Decl` item as a module signal.
    fn register_decl_signals(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let dk = real_children(item);
        let width = self.decl_width(&dk, env);
        let is_real = self.decl_is_real(&dk);
        let tname = self.decl_type_name(&dk);
        for (i, k) in dk.iter().enumerate() {
            if k.kind() == SyntaxKind::Name {
                self.register_decl_name(module, &dk, i, k, width, is_real, &tname, env);
            }
        }
    }

    /// The declaration is `real`, or names a user type marked real.
    fn decl_is_real(&self, dk: &[SyntaxNode]) -> bool {
        dk.iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "real")
            || self
                .decl_type_name(dk)
                .and_then(|t| self.types.get(&t))
                .is_some_and(|t| t.real)
    }

    /// Register one declared name as a module signal (width, real/signed,
    /// memory dimension, type bookkeeping) and bind it in the name map.
    #[allow(clippy::too_many_arguments)]
    fn register_decl_name(
        &mut self,
        module: &mut ModuleIr,
        dk: &[SyntaxNode],
        i: usize,
        k: &SyntaxNode,
        width: usize,
        is_real: bool,
        tname: &Option<String>,
        env: &HashMap<String, Value>,
    ) {
        let n = self.text(k);
        if self.sig_id(&n).is_none() {
            let id = module.signals.len() as SigId;
            // Memory dimension after the name: `mem [0:255]`,
            // dynamic `da []` / queue `q [$]`.
            let mem = dk.get(i + 1).and_then(|r| {
                if r.kind() == SyntaxKind::Range {
                    Some(self.range_depth(r, env))
                } else {
                    None
                }
            });
            module.signals.push(SignalIr {
                name: n.clone(),
                width,
                kind: if is_real {
                    SigKind::Real
                } else {
                    SigKind::Wire
                },
                init: None,
                mem,
                signed: tname
                    .as_ref()
                    .and_then(|t| self.types.get(t))
                    .is_some_and(|t| t.signed),
            });
            if let Some(t) = &tname {
                self.sig_types.insert(id, t.clone());
            }
            if mem.is_some() {
                self.sig_is_mem.insert(id);
            }
            self.names.insert(n, id);
        }
    }

    /// Rebuild the name map from the module (ports + signals), then lower items.
    fn module_name_map(&mut self, module: &ModuleIr) {
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        self.extra_base = module.signals.len();
    }

    /// Allocate an object memory for each class-typed declaration in this
    /// module, and expose class methods as `Class::method` functions.
    fn module_class_mems(&mut self, item_nodes: &[SyntaxNode]) {
        for item in item_nodes {
            if item.kind() == SyntaxKind::Decl {
                let dk = real_children(item);
                if let Some(t) = self.decl_type_name(&dk) {
                    let cn = self.types.get(&t).and_then(|td| match &td.kind {
                        TypeKind::Class(cn) => Some(cn.clone()),
                        _ => None,
                    });
                    if let Some(cn) = cn {
                        self.class_mem_sig(&cn);
                    }
                }
            }
        }
    }

    /// Expose each class's methods as `Class::method` module functions.
    fn class_method_funcs(&self) -> Vec<FuncIr> {
        let class_methods: Vec<FuncIr> = self
            .classes
            .values()
            .flat_map(|ci| {
                ci.methods.iter().map(move |m| FuncIr {
                    name: format!("{}::{}", ci.name, m.name),
                    ret_width: m.ret_width,
                    args: m.args.clone(),
                    defaults: m.defaults.clone(),
                    modes: vec![ArgMode::In; m.args.len()],
                    locals: m.locals.clone(),
                    dyn_locals: Vec::new(),
                    body: m.body.clone(),
                    hidden: Vec::new(),
                })
            })
            .collect();
        class_methods
    }
}
