//! VHDL lowering: CST → the shared IR (`ModuleIr`/`ProcessIr`/`AssignIr`...).
//!
//! One entity + its architectures become one `ModuleIr` (entity = interface:
//! generics/ports; architecture = body: signals, processes, concurrent
//! assignments, instances). Processes lower to `ProcKind::Always` (with a
//! sensitivity list) or `Initial` (wait-driven). VHDL signal assignment `<=`
//! inside a process is non-blocking (delta-cycle) like Verilog NBA; variable
//! assignment `:=` is blocking. The well-known `ieee.std_logic_1164` /
//! `numeric_std` functions (`rising_edge`, `to_integer`, `resize`, …) are
//! built in: recognized by name here and lowered to dedicated `Expr` nodes.

use crate::ir::*;
use crate::value::{BinOp, UnOp, Value, V0, V1, VX, VZ};
use crate::verilog_lower::const_eval;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::{Diagnostic, Severity};
use hdl_syntax::kinds::SyntaxKind;
use hdl_syntax::source::{FileId, SourceDb, Span};
use std::collections::HashMap;

/// Lower one parsed VHDL file into modules (entities with their architectures
/// attached). `time_unit_fs` is the fallback for physical literals; VHDL
/// physical literals carry their own unit (`10 ns`), so this is only used for
/// unit-less numbers.
pub fn lower_file(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    _time_unit_fs: u64,
) -> Vec<ModuleIr> {
    let _ = file;
    let mut lx = Lower {
        db,
        diags,
        consts: HashMap::new(),
        types: HashMap::new(),
        names: HashMap::new(),
        mems: HashMap::new(),
        sig_signed: HashMap::new(),
    };
    lx.register_builtin_types();
    let mut modules: Vec<ModuleIr> = Vec::new();
    let mut entities: HashMap<String, usize> = HashMap::new();
    // Pass 1: entities (interfaces).
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Entity {
            let m = lx.lower_entity(&child);
            entities.insert(m.name.clone(), modules.len());
            modules.push(m);
        }
    }
    // Pass 2: packages (constants/types into scope) and architectures (bodies).
    for child in real_children(root) {
        match child.kind() {
            SyntaxKind::Package => lx.lower_package(&child),
            SyntaxKind::Architecture => lx.lower_architecture(&child, &mut modules, &entities),
            _ => {}
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
            | SyntaxKind::ParenExpr
            | SyntaxKind::BinExpr
            | SyntaxKind::UnaryExpr
            | SyntaxKind::Call
            | SyntaxKind::Concat
            | SyntaxKind::Aggregate
            | SyntaxKind::PartSelect
            | SyntaxKind::Index
            | SyntaxKind::Attr
            | SyntaxKind::Others
    )
}

/// Static type info for a VHDL type mark.
#[derive(Clone, Debug)]
enum TypeInfo {
    /// Scalar 1-bit (std_logic, bit, boolean).
    Scalar,
    /// Vector (std_logic_vector/unsigned/signed/bit_vector); the width comes
    /// from the constraint at the declaration site.
    Vector { signed: bool },
    /// 32-bit integer (natural/positive unsigned).
    Integer { signed: bool },
    /// 64-bit IEEE-754 real.
    Real,
    /// 8-bit char.
    Char,
    /// Enumeration: literals in declaration order.
    Enum(Vec<String>),
    /// Array type: element width + depth (memories).
    Array { elem: usize, depth: usize },
}

struct Lower<'a> {
    db: &'a SourceDb,
    diags: &'a mut Vec<Diagnostic>,
    /// Compile-time constants in scope (enum literals, constants, generics).
    consts: HashMap<String, Value>,
    /// Type marks in scope (built-ins + user type/subtype decls).
    types: HashMap<String, TypeInfo>,
    /// Signal/variable name → signal id in the current module.
    names: HashMap<String, SigId>,
    /// Signal id → memory depth (for `name'range` loops over arrays).
    mems: HashMap<SigId, usize>,
    /// Signal id → signed flag (for `resize`/`to_signed` context).
    sig_signed: HashMap<SigId, bool>,
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

    fn warn(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic {
            code: "W0401",
            severity: Severity::Warning,
            span: Some(span),
            message: msg.into(),
            help: None,
        });
    }

    fn text(&self, n: &SyntaxNode) -> String {
        self.db.span_text(n.span()).trim().to_string()
    }

    fn kw(&self, n: &SyntaxNode) -> String {
        self.text(n).to_ascii_lowercase()
    }

    fn register_builtin_types(&mut self) {
        self.consts.insert("true".into(), Value { bits: vec![V1] });
        self.consts.insert("false".into(), Value { bits: vec![V0] });
        self.types.insert("std_logic".into(), TypeInfo::Scalar);
        self.types.insert("bit".into(), TypeInfo::Scalar);
        self.types.insert("boolean".into(), TypeInfo::Scalar);
        self.types.insert(
            "std_logic_vector".into(),
            TypeInfo::Vector { signed: false },
        );
        self.types.insert(
            "std_ulogic_vector".into(),
            TypeInfo::Vector { signed: false },
        );
        self.types
            .insert("bit_vector".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("unsigned".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("signed".into(), TypeInfo::Vector { signed: true });
        self.types
            .insert("integer".into(), TypeInfo::Integer { signed: true });
        self.types.insert("real".into(), TypeInfo::Real);
        self.types
            .insert("natural".into(), TypeInfo::Integer { signed: false });
        self.types
            .insert("positive".into(), TypeInfo::Integer { signed: false });
        self.types.insert("char".into(), TypeInfo::Char);
        self.types
            .insert("string".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("time".into(), TypeInfo::Integer { signed: false });
    }

    /// Resolve a type mark + optional constraint to (width, signed, mem_depth).
    fn resolve_type(
        &mut self,
        mark: &SyntaxNode,
        constraint: Option<&SyntaxNode>,
        span: Span,
    ) -> (usize, bool, Option<usize>, bool) {
        let name = self.kw(mark);
        let base = self.types.get(&name).cloned();
        let Some(base) = base else {
            self.err(span, format!("unknown type '{}'", name));
            return (1, false, None, false);
        };
        match base {
            TypeInfo::Vector { signed } => {
                let w = constraint
                    .map(|c| self.constraint_width(c))
                    .unwrap_or(1)
                    .max(1);
                (w, signed, None, false)
            }
            TypeInfo::Integer { signed } => (32, signed, None, false),
            TypeInfo::Real => (64, false, None, true),
            TypeInfo::Scalar => (1, false, None, false),
            TypeInfo::Char => (8, false, None, false),
            TypeInfo::Enum(ref lits) => {
                let w = bits_for(lits.len());
                (w, false, None, false)
            }
            TypeInfo::Array { elem, depth } => (elem, false, Some(depth), false),
        }
    }

    /// Width from a `(msb downto lsb)` / `(msb to lsb)` / `(n)` constraint.
    fn constraint_width(&mut self, c: &SyntaxNode) -> usize {
        // Box constraint `type range <>` — genuinely unconstrained; do not try
        // to evaluate the index type name (it would report "unknown identifier").
        if self.text(c).contains("<>") {
            return 1;
        }
        let exprs: Vec<Expr> = real_children(c)
            .into_iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(&k, &HashMap::new()))
            .collect();
        if exprs.len() >= 2 {
            if let (Some(a), Some(b)) = (
                const_eval(&exprs[0], &HashMap::new()),
                const_eval(&exprs[1], &HashMap::new()),
            ) {
                if let (Some(a), Some(b)) = (a.to_u64(), b.to_u64()) {
                    return (a as i64 - b as i64).unsigned_abs() as usize + 1;
                }
            }
            1
        } else if exprs.len() == 1 {
            const_eval(&exprs[0], &HashMap::new())
                .and_then(|v| v.to_u64())
                .map(|n| n as usize)
                .unwrap_or(1)
        } else {
            1
        }
    }

    fn sig_id(&self, name: &str) -> Option<SigId> {
        self.names.get(name).copied()
    }

    // ---- entities / architectures ---------------------------------------------

    fn lower_entity(&mut self, e: &SyntaxNode) -> ModuleIr {
        let kids = real_children(e);
        let name = kids
            .iter()
            .find(|k| {
                k.kind() == SyntaxKind::Ident
                    && !matches!(self.kw(k).as_str(), "entity" | "is" | "end" | "begin")
            })
            .map(|k| self.text(k))
            .unwrap_or_else(|| "<anon>".to_string());
        let mut module = ModuleIr {
            name,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Vec::new(),
            assigns: Vec::new(),
            processes: Vec::new(),
            functions: Vec::new(),
            instances: Vec::new(),
            time_unit_fs: 1_000_000,
        };
        self.names.clear();
        for k in &kids {
            match k.kind() {
                SyntaxKind::GenericClause => self.lower_generic_clause(&mut module, k),
                SyntaxKind::PortClause => self.lower_port_clause(&mut module, k),
                _ => {}
            }
        }
        // Rebuild names from signals (ports registered above).
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        module
    }

    fn lower_generic_clause(&mut self, module: &mut ModuleIr, g: &SyntaxNode) {
        for d in real_children(g)
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::GenericDecl)
        {
            let dk = real_children(&d);
            let mut names = Vec::new();
            let mut type_mark = None;
            let mut constraint = None;
            let mut default = None;
            let mut i = 0;
            while i < dk.len() {
                match dk[i].kind() {
                    SyntaxKind::Name => names.push(self.text(&dk[i])),
                    SyntaxKind::Range => constraint = Some(dk[i].clone()),
                    _ if is_expr_node(dk[i].kind()) => default = Some(dk[i].clone()),
                    _ => {}
                }
                // type mark is the Name that is not a declared name... find by elimination below
                i += 1;
            }
            // The type mark is the Name child that is not one of the declared names.
            for k in &dk {
                if k.kind() == SyntaxKind::Name && !names.contains(&self.text(k)) {
                    type_mark = Some(k.clone());
                }
            }
            let span = d.span();
            let (width, signed, _, _) = match &type_mark {
                Some(tm) => self.resolve_type(tm, constraint.as_ref(), span),
                None => (32, false, None, false),
            };
            let def = default
                .as_ref()
                .map(|e| {
                    let ex = self.lower_expr(e, &HashMap::new());
                    const_eval(&ex, &HashMap::new())
                })
                .flatten()
                .unwrap_or(Value::x(width.max(1)));
            for n in &names {
                self.consts.insert(n.to_ascii_lowercase(), def.clone());
                module.params.push(ParamIr {
                    name: n.clone(),
                    default: def.clone(),
                });
                let _ = (width, signed);
            }
        }
    }

    fn lower_port_clause(&mut self, module: &mut ModuleIr, p: &SyntaxNode) {
        for d in real_children(p)
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::PortDecl)
        {
            let dk = real_children(&d);
            let mut names = Vec::new();
            let mut mode = "in".to_string();
            let mut type_mark = None;
            let mut constraint = None;
            for k in &dk {
                match k.kind() {
                    SyntaxKind::Name => {
                        let t = self.text(k);
                        match t.to_ascii_lowercase().as_str() {
                            "in" | "out" | "inout" | "buffer" => mode = t.to_ascii_lowercase(),
                            _ => {
                                // First name(s) before ':' are the port names; the
                                // type mark follows the ':'. Distinguish by position:
                                // names come before the ':' punct.
                                names.push(t);
                            }
                        }
                    }
                    SyntaxKind::Range => constraint = Some(k.clone()),
                    _ => {}
                }
            }
            // The type mark is the last Name child (after the ':' punct).
            if let Some(pos) = dk
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
            {
                for k in &dk[pos + 1..] {
                    if k.kind() == SyntaxKind::Name && type_mark.is_none() {
                        type_mark = Some(k.clone());
                    }
                }
                // Port names are Name children before the ':'.
                names = dk[..pos]
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k))
                    .collect();
                // ...but the mode keyword was also before ':' and is an Ident, not a Name.
            }
            let span = d.span();
            let (width, signed, mem, _real) = match &type_mark {
                Some(tm) => self.resolve_type(tm, constraint.as_ref(), span),
                None => (1, false, None, false),
            };
            let dir = match mode.as_str() {
                "out" => Dir::Out,
                "inout" => Dir::Inout,
                _ => Dir::In,
            };
            let kind = match dir {
                Dir::In => SigKind::Input,
                Dir::Out => SigKind::Output,
                Dir::Inout => SigKind::Inout,
            };
            for n in &names {
                let id = module.signals.len() as SigId;
                module.signals.push(SignalIr {
                    name: n.clone(),
                    width,
                    kind,
                    init: None,
                    mem,
                    signed,
                });
                if let Some(d) = mem {
                    self.mems.insert(id, d);
                }
                self.sig_signed.insert(id, signed);
                module.ports.push(PortIr {
                    name: n.clone(),
                    dir,
                    sig: id,
                });
            }
        }
    }

    fn lower_package(&mut self, p: &SyntaxNode) {
        // Package declarations: constants and types become visible in this file.
        for d in real_children(p) {
            match d.kind() {
                SyntaxKind::ConstantDecl => self.lower_constant_decl(&d),
                SyntaxKind::TypeDecl => self.lower_type_decl(&d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(&d),
                _ => {}
            }
        }
    }

    fn lower_architecture(
        &mut self,
        a: &SyntaxNode,
        modules: &mut Vec<ModuleIr>,
        entities: &HashMap<String, usize>,
    ) {
        let kids = real_children(a);
        // `architecture NAME of ENTITY is ...`
        let idents: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .collect();
        // idents: [architecture, name, of, entity, is, begin/end...]
        let entity_name = idents.get(3).map(|k| self.text(k)).unwrap_or_default();
        let Some(&midx) = entities.get(&entity_name) else {
            self.err(
                a.span(),
                format!("architecture for unknown entity '{}'", entity_name),
            );
            return;
        };
        let module = &mut modules[midx];
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        // Declarative part: everything before `begin`.
        let begin_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "begin");
        let decls: Vec<&SyntaxNode> = match begin_pos {
            Some(b) => kids[..b].iter().collect(),
            None => kids.iter().collect(),
        };
        for d in &decls {
            match d.kind() {
                SyntaxKind::SignalDecl => self.lower_signal_decl(module, d),
                SyntaxKind::ConstantDecl => self.lower_constant_decl(d),
                SyntaxKind::VariableDecl => self.lower_signal_decl(module, d), // rare at arch level
                SyntaxKind::TypeDecl => self.lower_type_decl(d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(d),
                SyntaxKind::ComponentDecl => {} // components resolve to entities by name
                _ => {}
            }
        }
        // Rebuild names (signals added).
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        // Concurrent statements: after `begin`.
        let stmts: Vec<&SyntaxNode> = match begin_pos {
            Some(b) => kids[b + 1..].iter().collect(),
            None => Vec::new(),
        };
        let mut proc_idx = 0;
        for s in &stmts {
            match s.kind() {
                SyntaxKind::Process => {
                    let p = self.lower_process(module, s, proc_idx);
                    proc_idx += 1;
                    module.processes.push(p);
                }
                SyntaxKind::ConcAssign => {
                    if let Some(a) = self.lower_conc_assign(s) {
                        module.assigns.push(a);
                    }
                }
                SyntaxKind::Inst => {
                    if let Some(i) = self.lower_inst(s) {
                        module.instances.push(i);
                    }
                }
                SyntaxKind::AssertStmt => {
                    self.warn(s.span(), "concurrent assert not lowered yet");
                }
                _ => {}
            }
        }
    }

    // ---- declarations ------------------------------------------------------------

    fn lower_signal_decl(&mut self, module: &mut ModuleIr, d: &SyntaxNode) {
        let kids = real_children(d);
        let colon = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
        let Some(colon) = colon else {
            self.err(d.span(), "malformed signal declaration");
            return;
        };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        let type_mark = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name);
        let constraint = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range);
        let init = kids[colon + 1..]
            .iter()
            .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name);
        let span = d.span();
        let (width, signed, mem, real) = match type_mark {
            Some(tm) => self.resolve_type(tm, constraint, span),
            None => (1, false, None, false),
        };
        let kind = if real { SigKind::Real } else { SigKind::Wire };
        let init_v = init.and_then(|e| {
            let ex = self.lower_expr(e, &self.consts.clone());
            const_eval(&ex, &self.consts.clone())
        });
        for n in &names {
            if self.sig_id(n).is_some() {
                continue;
            }
            let id = module.signals.len() as SigId;
            module.signals.push(SignalIr {
                name: n.clone(),
                width,
                kind,
                init: init_v.clone(),
                mem,
                signed,
            });
            if let Some(d) = mem {
                self.mems.insert(id, d);
            }
            self.sig_signed.insert(id, signed);
            self.names.insert(n.clone(), id);
        }
    }

    fn lower_constant_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        let colon = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
        let Some(colon) = colon else { return };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        let init = kids[colon + 1..]
            .iter()
            .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name);
        let Some(init) = init else { return };
        let env = self.consts.clone();
        let ex = self.lower_expr(init, &env);
        if let Some(v) = const_eval(&ex, &env) {
            for n in &names {
                self.consts.insert(n.to_ascii_lowercase(), v.clone());
            }
        } else {
            self.warn(d.span(), "constant initializer is not constant; ignored");
        }
    }

    fn lower_type_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        // kids[0] is the `type` keyword; the name is the next ident.
        let Some(name_node) = kids.iter().filter(|k| k.kind() == SyntaxKind::Ident).nth(1) else {
            return;
        };
        let name = self.kw(name_node);
        // Enum: `type t is (a, b, c);`
        let enum_lits: Vec<String> = kids
            .iter()
            .skip_while(|k| !(k.kind() == SyntaxKind::Punct && self.text(k) == "("))
            .filter(|k| k.kind() == SyntaxKind::Ident || k.kind() == SyntaxKind::CharLiteral)
            .map(|k| self.text(k).trim_matches('\'').to_ascii_lowercase())
            .collect();
        if !enum_lits.is_empty() {
            for (i, l) in enum_lits.iter().enumerate() {
                self.consts.insert(l.clone(), Value::from_u64(32, i as u64));
            }
            self.types.insert(name, TypeInfo::Enum(enum_lits));
            return;
        }
        // Array: `type t is array (range) of elem;`
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "array")
        {
            let range = kids.iter().find(|k| k.kind() == SyntaxKind::Range);
            let elem_pos = kids.iter().rposition(|k| k.kind() == SyntaxKind::Name);
            let elem = elem_pos.map(|i| kids[i].clone());
            // Element constraint, if any: `... of std_logic_vector(31 downto 0)`.
            let elem_constraint = elem_pos.and_then(|i| {
                kids.iter()
                    .skip(i + 1)
                    .find(|k| k.kind() == SyntaxKind::Range)
            });
            let depth = range.map(|r| self.constraint_width(r)).unwrap_or(1).max(1);
            let (ew, _, _, _) = match elem {
                Some(e) => self.resolve_type(&e, elem_constraint, d.span()),
                None => (8, false, None, false),
            };
            self.types.insert(name, TypeInfo::Array { elem: ew, depth });
            return;
        }
        // Range type: `type t is range 0 to 10;` → integer
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "range")
        {
            self.types.insert(name, TypeInfo::Integer { signed: false });
        }
    }

    fn lower_subtype_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        // kids[0] is the `subtype` keyword; the name is the next ident.
        let name = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .nth(1)
            .map(|k| self.kw(k));
        let Some(name) = name else { return };
        // `subtype s is base [(constraint)] [range ...]`
        let is_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "is");
        let Some(is_pos) = is_pos else { return };
        let base = kids[is_pos + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .cloned();
        let constraint = kids[is_pos + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range)
            .cloned();
        if let Some(base) = base {
            let span = d.span();
            let (w, signed, mem, _r) = self.resolve_type(&base, constraint.as_ref(), span);
            let _ = mem;
            // Register as a vector type with the constrained width.
            let _ = w;
            self.types.insert(name, TypeInfo::Vector { signed });
        }
    }

    // ---- processes and statements ---------------------------------------------------

    fn lower_process(&mut self, module: &mut ModuleIr, p: &SyntaxNode, idx: usize) -> ProcessIr {
        let kids = real_children(p);
        let mut sens = Sens::List(Vec::new());
        let mut stmt_nodes: Vec<SyntaxNode> = Vec::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::SensList => {
                    sens = self.lower_sens_list(k);
                }
                SyntaxKind::SignalDecl
                | SyntaxKind::VariableDecl
                | SyntaxKind::ConstantDecl
                | SyntaxKind::TypeDecl
                | SyntaxKind::SubtypeDecl => {
                    // Process-local declarations.
                    match k.kind() {
                        SyntaxKind::VariableDecl => {
                            self.lower_process_var(module, k, idx);
                        }
                        SyntaxKind::ConstantDecl => self.lower_constant_decl(k),
                        SyntaxKind::TypeDecl => self.lower_type_decl(k),
                        SyntaxKind::SubtypeDecl => self.lower_subtype_decl(k),
                        _ => {}
                    }
                }
                _ if is_stmt_node(k.kind()) => stmt_nodes.push(k.clone()),
                _ => {}
            }
        }
        let body = if stmt_nodes.len() == 1 {
            self.lower_seq_stmt(&stmt_nodes[0])
        } else {
            Stmt::Block(stmt_nodes.iter().map(|k| self.lower_seq_stmt(k)).collect())
        };
        // VHDL processes always loop: with a sensitivity list they re-run on
        // each event (`always @(sens)`); without one they re-run from the top
        // whenever they reach the end, suspending only at wait statements
        // (empty sens list = bare `always`, re-queued immediately).
        let kind = ProcKind::Always;
        let origin = format!("process #{}", idx);
        ProcessIr {
            kind,
            sens,
            body,
            origin,
        }
    }

    /// A process-local variable becomes a (hidden) signal — VHDL variables
    /// keep their value between activations, exactly like a signal.
    fn lower_process_var(&mut self, module: &mut ModuleIr, d: &SyntaxNode, proc_idx: usize) {
        let kids = real_children(d);
        let colon = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
        let Some(colon) = colon else { return };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        let type_mark = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name);
        let constraint = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range);
        let init = kids[colon + 1..]
            .iter()
            .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name);
        let span = d.span();
        let (width, signed, _, real) = match type_mark {
            Some(tm) => self.resolve_type(tm, constraint, span),
            None => (32, false, None, false),
        };
        let init_v = init.and_then(|e| {
            let ex = self.lower_expr(e, &self.consts.clone());
            const_eval(&ex, &self.consts.clone())
        });
        for n in &names {
            let id = module.signals.len() as SigId;
            module.signals.push(SignalIr {
                name: format!("__p{}_var_{}", proc_idx, n),
                width,
                kind: if real { SigKind::Real } else { SigKind::Wire },
                init: init_v.clone(),
                mem: None,
                signed,
            });
            self.sig_signed.insert(id, signed);
            self.names.insert(n.clone(), id);
        }
    }

    fn lower_sens_list(&mut self, s: &SyntaxNode) -> Sens {
        let mut edges = Vec::new();
        for c in real_children(s) {
            if c.kind() == SyntaxKind::Name {
                let name = self.text(&c);
                if let Some(id) = self.sig_id(&name) {
                    edges.push(Edge {
                        sig: id,
                        kind: EdgeKind::Any,
                    });
                } else {
                    self.err(
                        c.span(),
                        format!("unknown signal '{}' in sensitivity list", name),
                    );
                }
            }
        }
        Sens::List(edges)
    }

    fn lower_conc_assign(&mut self, s: &SyntaxNode) -> Option<AssignIr> {
        let kids = real_children(s);
        let lhs = kids.iter().find(|k| {
            k.kind() == SyntaxKind::Name
                || k.kind() == SyntaxKind::Call
                || k.kind() == SyntaxKind::PartSelect
        })?;
        let rhs = kids.iter().rev().find(|k| is_expr_node(k.kind()))?;
        Some(AssignIr {
            lhs: self.lower_expr(lhs, &self.consts.clone()),
            rhs: self.lower_expr(rhs, &self.consts.clone()),
        })
    }

    fn lower_inst(&mut self, s: &SyntaxNode) -> Option<InstIr> {
        let kids = real_children(s);
        let name = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))?;
        // `entity work.counter` — drop the library prefix; the unit name is
        // the last selected component.
        let name = name.rsplit('.').next().unwrap_or(name.as_str()).to_string();
        let mut param_overrides = Vec::new();
        let mut conns = Vec::new();
        let mut in_generic = false;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident => {
                    let w = self.kw(k);
                    if w == "generic" {
                        in_generic = true;
                    } else if w == "port" {
                        in_generic = false;
                    }
                }
                SyntaxKind::PortConn => {
                    let ck = real_children(k);
                    let name_pos = ck.iter().position(|c| c.kind() == SyntaxKind::Name);
                    let assoc_name = name_pos.map(|i| self.text(&ck[i]));
                    let expr = ck
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(i, c)| Some(*i) != name_pos && is_expr_node(c.kind()))
                        .map(|(_, c)| c);
                    let Some(e) = expr else { continue };
                    let ex = self.lower_expr(e, &self.consts.clone());
                    match assoc_name {
                        Some(n) if in_generic => param_overrides.push((n, ex)),
                        Some(n) => conns.push(Conn::Named(n, ex)),
                        None => conns.push(Conn::Positional(ex)),
                    }
                }
                _ => {}
            }
        }
        Some(InstIr {
            module: name,
            name: String::new(), // label handled by elaboration naming
            param_overrides,
            conns,
        })
    }

    /// Lower a run of sequential statements into a Block.
    fn lower_seq_block(&mut self, n: &SyntaxNode) -> Stmt {
        let stmts: Vec<Stmt> = real_children(n)
            .iter()
            .filter(|k| is_stmt_node(k.kind()))
            .map(|k| self.lower_seq_stmt(k))
            .collect();
        if stmts.len() == 1 {
            stmts.into_iter().next().unwrap()
        } else {
            Stmt::Block(stmts)
        }
    }

    /// Lower one sequential statement, wrapped in a `Stmt::Spanned` with its
    /// source span (drives line/branch coverage).
    fn lower_seq_stmt(&mut self, s: &SyntaxNode) -> Stmt {
        let span = s.span();
        let inner = self.lower_seq_stmt_impl(s);
        Stmt::Spanned {
            span,
            inner: Box::new(inner),
        }
    }

    fn lower_seq_stmt_impl(&mut self, s: &SyntaxNode) -> Stmt {
        match s.kind() {
            SyntaxKind::SeqAssign => {
                let kids = real_children(s);
                let lhs = kids.iter().find(|k| {
                    matches!(
                        k.kind(),
                        SyntaxKind::Name | SyntaxKind::Call | SyntaxKind::PartSelect
                    )
                });
                let rhs = kids.iter().rev().find(|k| is_expr_node(k.kind()));
                match (lhs, rhs) {
                    (Some(l), Some(r)) => Stmt::Assign {
                        lhs: self.lower_expr(l, &self.consts.clone()),
                        rhs: self.lower_expr(r, &self.consts.clone()),
                        nba: true, // VHDL signal assignment is delta-delayed
                    },
                    _ => Stmt::Null,
                }
            }
            SyntaxKind::VarAssign => {
                let kids = real_children(s);
                let lhs = kids.iter().find(|k| {
                    k.kind() == SyntaxKind::Name
                        || k.kind() == SyntaxKind::Call
                        || k.kind() == SyntaxKind::PartSelect
                });
                let rhs = kids.iter().rev().find(|k| is_expr_node(k.kind()));
                match (lhs, rhs) {
                    (Some(l), Some(r)) => Stmt::Assign {
                        lhs: self.lower_expr(l, &self.consts.clone()),
                        rhs: self.lower_expr(r, &self.consts.clone()),
                        nba: false,
                    },
                    _ => Stmt::Null,
                }
            }
            SyntaxKind::If => self.lower_if(s),
            SyntaxKind::Case => self.lower_case(s),
            SyntaxKind::For => self.lower_for(s),
            SyntaxKind::LoopStmt => self.lower_loop(s),
            SyntaxKind::Wait => self.lower_wait(s),
            SyntaxKind::NullStmt => Stmt::Null,
            SyntaxKind::AssertStmt => {
                // Covers both `assert cond [report msg] [severity lvl]` and
                // `report msg [severity lvl]` (the parser uses one node kind).
                let kids = real_children(s);
                let first_kw = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_default();
                let severity = kids
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .find(|w| matches!(w.as_str(), "note" | "warning" | "error" | "failure"))
                    .unwrap_or_default();
                let mut exprs: Vec<Expr> = kids
                    .iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, &self.consts.clone()))
                    .collect();
                if !severity.is_empty() {
                    exprs.push(Expr::Lit(string_value(&severity)));
                }
                Stmt::SysCall {
                    name: if first_kw == "report" {
                        "report"
                    } else {
                        "assert"
                    }
                    .to_string(),
                    args: exprs,
                }
            }
            SyntaxKind::ExitStmt => Stmt::Null, // loop exit unsupported in IR yet; note it
            SyntaxKind::CallStmt => {
                let kids = real_children(s);
                let name = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k))
                    .map(|n| n.rsplit('.').next().unwrap_or(n.as_str()).to_string());
                let args: Vec<Expr> = kids
                    .iter()
                    .filter(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name)
                    .map(|k| self.lower_expr(k, &self.consts.clone()))
                    .collect();
                Stmt::SysCall {
                    name: name.unwrap_or_default(),
                    args,
                }
            }
            _ => Stmt::Null,
        }
    }

    fn lower_if(&mut self, s: &SyntaxNode) -> Stmt {
        // Children: cond expr, then stmts..., then for each `elsif`: Ident,
        // cond expr, stmts...; an `else`: Ident + stmts....
        let kids = real_children(s);
        let mut branches: Vec<(Option<Expr>, Vec<&SyntaxNode>)> = Vec::new();
        let mut cur_cond: Option<Expr> = None;
        let mut cur_stmts: Vec<&SyntaxNode> = Vec::new();
        let mut pending_cond = false;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident => {
                    let w = self.kw(k);
                    if (w == "elsif" || w == "else") && cur_cond.is_some() {
                        branches.push((cur_cond.take(), std::mem::take(&mut cur_stmts)));
                        pending_cond = w == "elsif";
                    }
                }
                _ if is_expr_node(k.kind()) && pending_cond => {
                    cur_cond = Some(self.lower_expr(k, &self.consts.clone()));
                    pending_cond = false;
                }
                _ if is_expr_node(k.kind()) && cur_cond.is_none() => {
                    cur_cond = Some(self.lower_expr(k, &self.consts.clone()));
                }
                _ if is_stmt_node(k.kind()) => cur_stmts.push(k),
                _ => {}
            }
        }
        branches.push((cur_cond, cur_stmts));
        // Fold into nested If from the inside out.
        let mut result = Stmt::Null;
        for (cond, stmts) in branches.into_iter().rev() {
            let body = {
                let ss: Vec<Stmt> = stmts.iter().map(|k| self.lower_seq_stmt(k)).collect();
                if ss.len() == 1 {
                    ss.into_iter().next().unwrap()
                } else {
                    Stmt::Block(ss)
                }
            };
            result = Stmt::If {
                cond: cond.unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1))),
                then: Box::new(body),
                els: Some(Box::new(result)),
            };
        }
        result
    }

    fn lower_case(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        let expr = kids.iter().find(|k| is_expr_node(k.kind()));
        let expr = expr
            .map(|k| self.lower_expr(k, &self.consts.clone()))
            .unwrap_or_else(|| Expr::Lit(Value::x(1)));
        let mut items: Vec<(Vec<Expr>, Stmt)> = Vec::new();
        let mut default = None;
        for item in kids.iter().filter(|k| k.kind() == SyntaxKind::CaseItem) {
            let ik = real_children(item);
            let is_others = ik.iter().any(|k| k.kind() == SyntaxKind::Others);
            let conds: Vec<Expr> = ik
                .iter()
                .filter(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Others)
                .map(|k| self.lower_expr(k, &self.consts.clone()))
                .collect();
            let body = self.lower_seq_block(item);
            if is_others {
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

    fn lower_for(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // for <var> in <range> loop <body> end loop
        let var = kids
            .iter()
            .find(|k| {
                k.kind() == SyntaxKind::Ident
                    && self.kw(k) != "for"
                    && self.kw(k) != "in"
                    && self.kw(k) != "loop"
                    && self.kw(k) != "end"
            })
            .map(|k| self.text(k));
        let range = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range || k.kind() == SyntaxKind::Attr);
        let Some(var) = var else {
            self.err(s.span(), "malformed for loop");
            return Stmt::Null;
        };
        let Some(var_id) = self.sig_id(&var) else {
            self.err(
                s.span(),
                format!("for-loop variable '{}' not declared", var),
            );
            return Stmt::Null;
        };
        // Range bounds.
        let (lo, hi, downto) = match range.map(|r| r.kind()) {
            Some(SyntaxKind::Range) => {
                let rk = real_children(range.unwrap());
                let exprs: Vec<&SyntaxNode> =
                    rk.iter().filter(|k| is_expr_node(k.kind())).collect();
                let dir = rk
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_else(|| "to".to_string());
                let a = exprs
                    .first()
                    .map(|k| self.lower_expr(k, &self.consts.clone()));
                let b = exprs
                    .get(1)
                    .map(|k| self.lower_expr(k, &self.consts.clone()));
                match (a, b) {
                    (Some(a), Some(b)) => {
                        let downto = dir == "downto";
                        if downto {
                            (b, a, true)
                        } else {
                            (a, b, false)
                        }
                    }
                    _ => (
                        Expr::Lit(Value::from_u64(32, 0)),
                        Expr::Lit(Value::from_u64(32, 0)),
                        false,
                    ),
                }
            }
            Some(SyntaxKind::Attr) => {
                // `name'range` on an array signal: 0 to depth-1.
                let ak = real_children(range.unwrap());
                let base = ak.iter().find(|k| k.kind() == SyntaxKind::Name);
                if let Some(b) = base {
                    let bn = self.text(b);
                    if let Some(id) = self.sig_id(&bn) {
                        if let Some(depth) = self.mems.get(&id).copied() {
                            let body = self.lower_seq_block(s);
                            return self.make_for(
                                var_id,
                                Expr::Lit(Value::from_u64(32, 0)),
                                Expr::Lit(Value::from_u64(32, depth as u64 - 1)),
                                false,
                                body,
                            );
                        }
                    }
                }
                (
                    Expr::Lit(Value::from_u64(32, 0)),
                    Expr::Lit(Value::from_u64(32, 0)),
                    false,
                )
            }
            _ => (
                Expr::Lit(Value::from_u64(32, 0)),
                Expr::Lit(Value::from_u64(32, 0)),
                false,
            ),
        };
        let body = self.lower_seq_block(s);
        self.make_for(var_id, lo, hi, downto, body)
    }

    fn make_for(&mut self, var_id: SigId, lo: Expr, hi: Expr, downto: bool, body: Stmt) -> Stmt {
        let v = || Expr::Sig(var_id);
        let one = Expr::Lit(Value::from_u64(32, 1));
        let init = Stmt::Assign {
            lhs: v(),
            rhs: lo,
            nba: false,
        };
        let cond = Expr::Bin {
            op: if downto { BinOp::Ge } else { BinOp::Le },
            l: Box::new(v()),
            r: Box::new(hi),
        };
        let step = Stmt::Assign {
            lhs: v(),
            rhs: Expr::Bin {
                op: if downto { BinOp::Sub } else { BinOp::Add },
                l: Box::new(v()),
                r: Box::new(one),
            },
            nba: false,
        };
        Stmt::For {
            init: Box::new(init),
            cond,
            step: Box::new(step),
            body: Box::new(body),
        }
    }

    fn lower_loop(&mut self, s: &SyntaxNode) -> Stmt {
        // `while cond loop body end loop` / `loop body end loop`
        let kids = real_children(s);
        let cond = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &self.consts.clone()))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
        let body = self.lower_seq_block(s);
        Stmt::For {
            init: Box::new(Stmt::Null),
            cond,
            step: Box::new(Stmt::Null),
            body: Box::new(body),
        }
    }

    fn lower_wait(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // wait [until cond] [on sens] [for time];
        let mut cond = None;
        let mut sens = None;
        let mut time_fs = None;
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if k.kind() == SyntaxKind::Ident {
                match self.kw(k).as_str() {
                    "until" => {
                        if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                            cond = Some(self.lower_expr(e, &self.consts.clone()));
                            i += 1;
                        }
                    }
                    "on" => {
                        sens = kids.get(i + 1).cloned();
                        i += 1;
                    }
                    "for" => {
                        // Physical literal: `10 ns`. The parser may merge number
                        // and unit into one Literal token, or leave the unit as
                        // a separate Ident sibling.
                        let num = kids.get(i + 1);
                        if let Some(n) = num.filter(|k| k.kind() == SyntaxKind::Literal) {
                            let t = self.text(n);
                            let (v, mult) = match t.split_once(char::is_whitespace) {
                                Some((a, b)) => (
                                    self.parse_int_literal(a.trim())
                                        .and_then(|v| v.to_u64())
                                        .unwrap_or(0),
                                    time_unit_fs(b.trim()),
                                ),
                                None => {
                                    let unit =
                                        kids.get(i + 2).filter(|k| k.kind() == SyntaxKind::Ident);
                                    (
                                        self.parse_int_literal(&t)
                                            .and_then(|v| v.to_u64())
                                            .unwrap_or(0),
                                        unit.map(|k| time_unit_fs(&self.kw(k)))
                                            .unwrap_or(1_000_000),
                                    )
                                }
                            };
                            time_fs = Some(v * mult);
                            i += 1;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        // `wait for 10 ns` → timed delay (takes precedence over a condition).
        if let Some(t) = time_fs {
            return Stmt::Delay {
                time_fs: t,
                stmt: Box::new(Stmt::Null),
            };
        }
        // `wait until rising_edge(clk)` → edge wait; `wait until expr` → any-edge on expr's signals.
        if let Some(c) = &cond {
            if let Some(e) = edges_from_cond(c, self) {
                return Stmt::Wait {
                    sens: e,
                    body: Box::new(Stmt::Null),
                };
            }
        }
        if let Some(sl) = sens.as_ref() {
            if sl.kind() == SyntaxKind::SensList {
                return Stmt::Wait {
                    sens: match self.lower_sens_list(sl) {
                        Sens::List(es) => es,
                        Sens::All => Vec::new(),
                    },
                    body: Box::new(Stmt::Null),
                };
            }
        }
        // Bare `wait;` — park forever.
        Stmt::Wait {
            sens: Vec::new(),
            body: Box::new(Stmt::Null),
        }
    }

    // ---- expressions ---------------------------------------------------------------

    fn lower_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        match e.kind() {
            SyntaxKind::Literal => self.lower_literal(e, env),
            SyntaxKind::ParenExpr => {
                let inner = real_children(e)
                    .into_iter()
                    .find(|k| is_expr_node(k.kind()));
                match inner {
                    Some(k) => self.lower_expr(&k, env),
                    None => Expr::Lit(Value::x(1)),
                }
            }
            SyntaxKind::Name => {
                let name = self.text(e);
                let base = name.rsplit('.').next().unwrap_or(name.as_str());
                let key = base.to_ascii_lowercase();
                if let Some(v) = env.get(&key).or_else(|| self.consts.get(&key)) {
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
                let exprs: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
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
                    Expr::Bin {
                        op,
                        l: Box::new(self.lower_expr(exprs[0], env)),
                        r: Box::new(self.lower_expr(exprs[1], env)),
                    }
                } else {
                    Expr::Lit(Value::x(1))
                }
            }
            SyntaxKind::UnaryExpr => {
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
                    Some(o) => Expr::Un {
                        op,
                        e: Box::new(self.lower_expr(o, env)),
                    },
                    None => Expr::Lit(Value::x(1)),
                }
            }
            SyntaxKind::Call => self.lower_call(e, env),
            SyntaxKind::PartSelect => {
                // name ( msb dir lsb ) — the base name is not a bound expr.
                let kids = real_children(e);
                let base_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
                let base = base_pos.map(|i| &kids[i]);
                let exprs: Vec<&SyntaxNode> = kids
                    .iter()
                    .enumerate()
                    .filter(|(i, k)| Some(*i) != base_pos && is_expr_node(k.kind()))
                    .map(|(_, k)| k)
                    .collect();
                if exprs.len() < 2 {
                    return Expr::Lit(Value::x(1));
                }
                let dir = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_else(|| "downto".to_string());
                let (msb, lsb) = if dir == "downto" {
                    (exprs[0], exprs[1])
                } else {
                    (exprs[1], exprs[0])
                };
                let base_e = match base {
                    Some(b) => self.lower_expr(b, env),
                    None => Expr::Lit(Value::x(1)),
                };
                Expr::Sel {
                    e: Box::new(base_e),
                    msb: Box::new(self.lower_expr(msb, env)),
                    lsb: Box::new(self.lower_expr(lsb, env)),
                }
            }
            SyntaxKind::Attr => {
                // name'attr — only 'length/'high/'low/'range supported statically.
                let kids = real_children(e);
                let base = kids.iter().find(|k| k.kind() == SyntaxKind::Name);
                let attr = kids
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Ident)
                    .last()
                    .map(|k| self.kw(k));
                let Some(attr) = attr else {
                    return Expr::Lit(Value::x(1));
                };
                let Some(b) = base else {
                    return Expr::Lit(Value::x(1));
                };
                let bn = self.text(b);
                match attr.as_str() {
                    "length" | "high" | "low" | "left" | "right" => {
                        if let Some(id) = self.sig_id(&bn) {
                            let w = 1; // width known only in module context; approximate below
                            let _ = id;
                            let _ = w;
                        }
                        Expr::Lit(Value::x(32)) // refined by caller when static
                    }
                    _ => {
                        self.warn(
                            e.span(),
                            format!("attribute '{}\'{}' not supported; X", bn, attr),
                        );
                        Expr::Lit(Value::x(1))
                    }
                }
            }
            SyntaxKind::Aggregate => self.lower_aggregate(e, env, 32),
            _ => {
                self.err(e.span(), format!("unsupported expression {:?}", e.kind()));
                Expr::Lit(Value::x(1))
            }
        }
    }

    fn lower_call(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        // `integer'image(x)` arrives as Call(Attr(Name integer, 'image), args).
        let attr = kids.iter().find(|k| k.kind() == SyntaxKind::Attr);
        if let Some(a) = attr {
            let ak = real_children(a);
            let attr_name = ak
                .iter()
                .filter(|k| k.kind() == SyntaxKind::Ident)
                .last()
                .map(|k| self.kw(k))
                .unwrap_or_default();
            if attr_name == "image" {
                if let Some(arg) = kids
                    .iter()
                    .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Attr)
                {
                    return Expr::ToString(Box::new(self.lower_expr(arg, env)));
                }
            }
        }
        // The callee is the first Name child (absent for attribute calls like
        // `integer'image(x)`, where kids are [Attr, args...]). Arguments are
        // every other expression child — plain names included (`rising_edge(clk)`).
        let name_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        let name = name_pos.map(|i| self.text(&kids[i])).unwrap_or_default();
        let fname = name
            .rsplit('.')
            .next()
            .unwrap_or(name.as_str())
            .to_ascii_lowercase();
        let args: Vec<Expr> = kids
            .iter()
            .enumerate()
            .filter(|(i, k)| {
                Some(*i) != name_pos && is_expr_node(k.kind()) && k.kind() != SyntaxKind::Attr
            })
            .map(|(_, k)| self.lower_expr(k, env))
            .collect();
        match fname.as_str() {
            // Indexed write/read of a signal: v(i) → Idx.
            _ if self.sig_id(&name).is_some() && args.len() == 1 => {
                let base = self.sig_id(&name).unwrap();
                Expr::Idx {
                    e: Box::new(Expr::Sig(base)),
                    i: Box::new(args.into_iter().next().unwrap()),
                }
            }
            "rising_edge" if args.len() == 1 => {
                Expr::Rising(Box::new(args.into_iter().next().unwrap()))
            }
            "falling_edge" if args.len() == 1 => {
                Expr::Falling(Box::new(args.into_iter().next().unwrap()))
            }
            "to_integer" | "conv_integer" if args.len() == 1 => {
                Expr::ToInt(Box::new(args.into_iter().next().unwrap()))
            }
            "to_unsigned" if args.len() == 2 => {
                let w = const_eval(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                Expr::Resize {
                    e: Box::new(args.into_iter().next().unwrap()),
                    width: w,
                    signed: false,
                }
            }
            "to_signed" if args.len() == 2 => {
                let w = const_eval(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                Expr::Resize {
                    e: Box::new(args.into_iter().next().unwrap()),
                    width: w,
                    signed: true,
                }
            }
            "resize" if args.len() == 2 => {
                let w = const_eval(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                let inner = &args[0];
                let signed = match inner {
                    Expr::Sig(s) => self.sig_signed.get(s).copied().unwrap_or(false),
                    Expr::Signed(_) => true,
                    Expr::Resize { signed, .. } => *signed,
                    _ => false,
                };
                Expr::Resize {
                    e: Box::new(args.into_iter().next().unwrap()),
                    width: w,
                    signed,
                }
            }
            // `signed(x)` keeps the bits but marks signed semantics.
            "signed" if args.len() == 1 => Expr::Signed(Box::new(args.into_iter().next().unwrap())),
            // Other type conversions are bit-identical.
            "std_logic_vector" | "std_ulogic_vector" | "unsigned" | "bit_vector" | "std_logic"
            | "bit" | "integer" | "natural" | "boolean" | "string" | "char" | "real"
                if args.len() == 1 =>
            {
                args.into_iter().next().unwrap()
            }
            _ => Expr::Call { name: fname, args },
        }
    }

    fn lower_aggregate(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
        width: usize,
    ) -> Expr {
        // `(others => x)` → replicate x; `(a, b, c)` → concat.
        let kids = real_children(e);
        if kids.iter().any(|k| k.kind() == SyntaxKind::Others) {
            let val = kids.iter().rev().find(|k| is_expr_node(k.kind()));
            return match val {
                Some(v) => {
                    let inner = self.lower_expr(v, env);
                    let iw = 1; // element width unknown statically; replicate by bits
                    let _ = iw;
                    Expr::Replicate {
                        count: Box::new(Expr::Lit(Value::from_u64(32, width as u64))),
                        e: Box::new(inner),
                    }
                }
                None => Expr::Lit(Value::x(width.max(1))),
            };
        }
        let parts: Vec<Expr> = kids
            .iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, env))
            .collect();
        Expr::Concat(parts)
    }

    fn lower_literal(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let t = self.text(e);
        // Physical literal: `100 ps` — the parser merges number + unit into
        // one Literal; the value is femtoseconds.
        if let Some((num, unit)) = t.split_once(char::is_whitespace) {
            if matches!(
                unit.trim().to_ascii_lowercase().as_str(),
                "fs" | "ps" | "ns" | "us" | "ms" | "sec" | "s" | "min" | "hr"
            ) {
                let v = self
                    .parse_int_literal(num.trim())
                    .and_then(|v| v.to_u64())
                    .unwrap_or(0);
                return Expr::Lit(Value::from_u64(32, v * time_unit_fs(unit.trim())));
            }
        }
        // Bit string literal: x"FF" / b"01" / o"7".
        if t.len() >= 2
            && matches!(t.as_bytes()[0], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
            && t.as_bytes()[1] == b'"'
        {
            let base = match t.as_bytes()[0].to_ascii_lowercase() {
                b'x' => 16,
                b'o' => 8,
                _ => 2,
            };
            let digits = t[2..t.len() - 1].replace('_', "");
            let digit_bits = match base {
                16 => 4,
                8 => 3,
                _ => 1,
            };
            let width = digits.len() * digit_bits;
            let mut bits = vec![VX; width.max(1)];
            for (i, c) in digits.chars().enumerate() {
                let from_lsb = (digits.len() - 1 - i) * digit_bits;
                match c.to_ascii_lowercase() {
                    'x' | '?' => {}
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
            return Expr::Lit(Value { bits });
        }
        if t.starts_with('\'') && t.ends_with('\'') && t.len() == 3 {
            // Character literal: std_logic value.
            let c = t.as_bytes()[1].to_ascii_lowercase();
            let b = match c {
                b'0' => V0,
                b'1' => V1,
                b'z' => VZ,
                _ => VX,
            };
            return Expr::Lit(Value { bits: vec![b] });
        }
        if t.starts_with('"') {
            let inner = t.trim_matches('"').replace("\"\"", "\"");
            // In VHDL a string of bit characters is a std_logic_vector value
            // (`"0000"`), not ASCII text — otherwise a plain string.
            if !inner.is_empty()
                && inner
                    .bytes()
                    .all(|c| matches!(c, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z' | b'-' | b'?'))
            {
                let mut bits = vec![VX; inner.len()];
                for (i, c) in inner.bytes().enumerate() {
                    bits[inner.len() - 1 - i] = match c.to_ascii_lowercase() {
                        b'0' => V0,
                        b'1' => V1,
                        b'z' => VZ,
                        _ => VX,
                    };
                }
                return Expr::Lit(Value { bits });
            }
            // String literal: first char in the high byte (MSB-first), so
            // `&`-concatenation and string_of read left to right.
            let bytes = inner.as_bytes();
            let n = bytes.len().max(1);
            let mut bits = vec![V0; n * 8];
            for (bi, &b) in bytes.iter().enumerate() {
                for j in 0..8 {
                    bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
                }
            }
            return Expr::Lit(Value { bits });
        }
        if let Some(v) = self.parse_int_literal(&t) {
            return Expr::Lit(v);
        }
        if let Ok(v) = t.parse::<f64>() {
            // VHDL real literal (contains '.' or exponent, no base '#').
            if t.contains('.') || t.contains('e') || t.contains('E') {
                return Expr::Real(v);
            }
            self.warn(e.span(), "real literals unsupported; X");
            return Expr::Lit(Value::x(32));
        }
        let _ = env;
        self.err(e.span(), format!("unsupported literal '{}'", t));
        Expr::Lit(Value::x(1))
    }

    /// Parse a VHDL integer literal: decimal (`1_000`) or based (`16#FF#E1`).
    fn parse_int_literal(&mut self, t: &str) -> Option<Value> {
        let t = t.trim();
        if let Some(h) = t.find('#') {
            let base: u32 = t[..h].parse().ok()?;
            let rest = &t[h + 1..];
            let end = rest.find('#')?;
            let digits = rest[..end].replace('_', "");
            let exp = rest[end + 1..]
                .strip_prefix(['e', 'E'])
                .and_then(|e| e.parse::<i32>().ok())
                .unwrap_or(0);
            let mut v = u128::from_str_radix(&digits, base).ok()?;
            if exp > 0 {
                v = v.checked_mul((base as u128).checked_pow(exp as u32)?)?;
            }
            let width = (128 - v.leading_zeros() as usize).max(1);
            return Some(value_from_u128(width, v));
        }
        let digits = t.replace('_', "");
        let v = digits.parse::<u128>().ok()?;
        // Plain decimal literals are VHDL integers: 32-bit, so `i + 1` does
        // not sign-extend the 1-bit literal `1` (based literals like 16#FF#
        // keep their minimal width and are usually bit patterns).
        Some(value_from_u128(32, v))
    }
}

fn is_stmt_node(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::SeqAssign
            | SyntaxKind::VarAssign
            | SyntaxKind::If
            | SyntaxKind::Case
            | SyntaxKind::For
            | SyntaxKind::LoopStmt
            | SyntaxKind::Wait
            | SyntaxKind::NullStmt
            | SyntaxKind::AssertStmt
            | SyntaxKind::ExitStmt
            | SyntaxKind::CallStmt
    )
}

fn bits_for(n: usize) -> usize {
    (usize::BITS - (n.max(1) - 1).leading_zeros()).max(1) as usize
}

/// VHDL time unit → femtoseconds.
fn time_unit_fs(unit: &str) -> u64 {
    match unit {
        "fs" => 1,
        "ps" => 1_000,
        "ns" => 1_000_000,
        "us" | "µs" => 1_000_000_000,
        "ms" => 1_000_000_000_000,
        "sec" | "s" => 1_000_000_000_000_000,
        "min" => 60_000_000_000_000_000,
        "hr" => 3_600_000_000_000_000_000,
        _ => 1_000_000,
    }
}

fn vhdl_binop(op: &str) -> Option<BinOp> {
    Some(match op {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "mod" | "rem" => BinOp::Mod,
        "=" => BinOp::Eq,
        "/=" => BinOp::Ne,
        "<" => BinOp::Lt,
        "<=" => BinOp::Le,
        ">" => BinOp::Gt,
        ">=" => BinOp::Ge,
        "and" => BinOp::BAnd,
        "or" => BinOp::BOr,
        "xor" => BinOp::BXor,
        "nand" => BinOp::BNand,
        "nor" => BinOp::BNor,
        "xnor" => BinOp::BXnor,
        "sll" | "rol" => BinOp::Shl,
        "srl" | "ror" => BinOp::Shr,
        "sra" => BinOp::Sar,
        "sla" => BinOp::Shl,
        _ => return None,
    })
}

fn vhdl_unop(op: &str) -> Option<UnOp> {
    Some(match op {
        "-" => UnOp::Neg,
        "+" => UnOp::Pos,
        "not" => UnOp::BNot,
        _ => return None,
    })
}

/// Extract edge sensitivities from a wait-until condition: rising_edge(s) →
/// Pos edge on s; anything else → Any edge on every signal in the condition.
fn edges_from_cond(c: &Expr, lx: &Lower) -> Option<Vec<Edge>> {
    match c {
        Expr::Rising(inner) | Expr::Falling(inner) => {
            if let Expr::Sig(s) = &**inner {
                return Some(vec![Edge {
                    sig: *s,
                    kind: if matches!(c, Expr::Rising(_)) {
                        EdgeKind::Pos
                    } else {
                        EdgeKind::Neg
                    },
                }]);
            }
            None
        }
        _ => {
            let mut sigs = Vec::new();
            c.collect_sigs(&mut sigs);
            let _ = lx;
            if sigs.is_empty() {
                None
            } else {
                Some(
                    sigs.iter()
                        .map(|s| Edge {
                            sig: *s,
                            kind: EdgeKind::Any,
                        })
                        .collect(),
                )
            }
        }
    }
}

/// String as a Value (first char in the high byte, matching `lower_literal`).
fn string_value(s: &str) -> Value {
    let bytes = s.as_bytes();
    let n = bytes.len().max(1);
    let mut bits = vec![V0; n * 8];
    for (bi, &b) in bytes.iter().enumerate() {
        for j in 0..8 {
            bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
        }
    }
    Value { bits }
}

/// Build a Value from a u128 with the minimal width that holds it.
fn value_from_u128(width: usize, v: u128) -> Value {
    let mut bits = vec![V0; width.max(1)];
    for i in 0..width.min(128) {
        bits[i] = if (v >> i) & 1 == 1 { V1 } else { V0 };
    }
    Value { bits }
}
