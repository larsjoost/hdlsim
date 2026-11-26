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
use std::collections::{HashMap, HashSet};

mod ce_proc;

/// Lower one parsed file into modules plus diagnostics, with the LRM default
/// time unit of 1 ns.
pub fn lower_file(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
) -> Vec<ModuleIr> {
    lower_file_with_time(file, root, db, diags, 1_000_000) // 1 ns in fs
}

/// Cross-file Verilog library state: packages and their `pkg::`-qualified
/// constants/functions, shared by all files compiled into the same library so
/// `cc_pkg::idx_width(...)` in one file resolves against `cc_pkg.sv`.
#[derive(Default)]
pub struct VerilogScope {
    pub packages: HashMap<String, ModuleIr>,
    pub gconsts: HashMap<String, Value>,
    pub funcs_all: HashMap<String, Vec<FuncIr>>,
    /// `pkg::Type` typedefs of previously compiled files, so a later file's
    /// `import pkg::*;` can resolve imported struct/enum types (field selects).
    pub types: HashMap<String, TypeDef>,
}

/// Lower one parsed file, with `time_unit_fs` taken from the file's
/// `` `timescale `` directive (femtoseconds per time unit).
pub fn lower_file_with_time(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    time_unit_fs: u64,
) -> Vec<ModuleIr> {
    lower_file_with_time_scoped(
        file,
        root,
        db,
        diags,
        time_unit_fs,
        &mut VerilogScope::default(),
    )
}

/// Like [`lower_file_with_time`], but seeds the lowerer from `scope` (packages
/// and qualified constants/functions of previously compiled files) and writes
/// back the `pkg::`-qualified entries discovered in this file.
pub fn lower_file_with_time_scoped(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    time_unit_fs: u64,
    scope: &mut VerilogScope,
) -> Vec<ModuleIr> {
    let _ = file; // spans on nodes already carry the file id
    let mut lx = Lower {
        db,
        diags,
        time_unit_fs,
        names: HashMap::new(),
        sig_types: HashMap::new(),
        types: scope.types.clone(),
        gconsts: scope.gconsts.clone(),
        packages: scope.packages.clone(),
        funcs_all: scope.funcs_all.clone(),
        extra_signals: Vec::new(),
        sig_is_mem: HashSet::new(),
        extra_base: 0,
        in_function: false,
        classes: HashMap::new(),
        class_mems: HashMap::new(),
        cur_class: None,
        inst_conns: HashMap::new(),
    };
    // Classes first (modules may reference them), then packages, then modules.
    let children = real_children(root);
    for child in &children {
        if child.kind() == SyntaxKind::Class {
            lx.lower_class(child);
        }
    }
    let mut modules = Vec::new();
    // Packages first (in file order): their params (`pkg::NAME` in gconsts)
    // and functions (`pkg::fn` in funcs_all) must be visible when modules
    // that appear earlier in the file reference them (e.g. `cc_pkg::idx_width`).
    for child in &children {
        if child.kind() == SyntaxKind::Package {
            let pkg = lx.lower_module(child);
            for f in &pkg.functions {
                lx.funcs_all
                    .entry(format!("{}::{}", pkg.name, f.name).to_ascii_lowercase())
                    .or_default()
                    .push(f.clone());
            }
            lx.packages.insert(pkg.name.clone(), pkg);
        }
    }
    // Bare-name aliases for package functions, so calls inside package
    // function bodies (`cnt_width` calling `idx_width`) const-evaluate.
    let pkg_funcs: Vec<(String, FuncIr)> = lx
        .funcs_all
        .iter()
        .filter(|(k, _)| k.contains("::"))
        .flat_map(|(k, v)| {
            let bare = k
                .rsplit("::")
                .next()
                .expect("rsplit always yields at least one item")
                .to_string();
            v.iter().map(move |f| (bare.clone(), f.clone()))
        })
        .collect();
    for (k, v) in pkg_funcs {
        let cands = lx.funcs_all.entry(k).or_default();
        if !cands.iter().any(|c| c.name == v.name && c.args == v.args) {
            cands.push(v);
        }
    }
    for child in children {
        if child.kind() == SyntaxKind::Module {
            let m = lx.lower_module(&child);
            for f in &m.functions {
                lx.funcs_all
                    .entry(f.name.to_ascii_lowercase())
                    .or_default()
                    .push(f.clone());
            }
            modules.push(m);
        }
    }
    // Write back only qualified names (`pkg::x`, `module::param`) so a plain
    // localparam of one file cannot leak into another file's scope.
    scope.packages.extend(lx.packages.drain());
    for (k, v) in lx.gconsts.drain() {
        if k.contains("::") {
            scope.gconsts.insert(k, v);
        }
    }
    for (k, v) in lx.funcs_all.drain() {
        if k.contains("::") {
            scope.funcs_all.insert(k, v);
        }
    }
    for (k, v) in lx.types.drain() {
        if k.contains("::") {
            scope.types.insert(k, v);
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
            | SyntaxKind::CondExpr
            | SyntaxKind::Call
            | SyntaxKind::Concat
            | SyntaxKind::Replicate
            | SyntaxKind::PartSelect
            | SyntaxKind::IndexedPartSelect
            | SyntaxKind::Index
            | SyntaxKind::Pattern
            | SyntaxKind::Allocator
            | SyntaxKind::Cast
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
            | SyntaxKind::AssertProperty
            | SyntaxKind::AssumeProperty
            | SyntaxKind::CoverProperty
            | SyntaxKind::Typedef
            | SyntaxKind::Import
            | SyntaxKind::Class
            | SyntaxKind::Package
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
            | SyntaxKind::Wait
            | SyntaxKind::For
            | SyntaxKind::While
            | SyntaxKind::Repeat
            | SyntaxKind::Forever
            | SyntaxKind::DoWhile
            | SyntaxKind::BreakStmt
            | SyntaxKind::ContinueStmt
            | SyntaxKind::SysCall
            | SyntaxKind::SvaStmt
            | SyntaxKind::Foreach
            | SyntaxKind::ReturnStmt
            | SyntaxKind::Decl
    )
}

struct Lower<'a> {
    db: &'a SourceDb,
    diags: &'a mut Vec<Diagnostic>,
    time_unit_fs: u64,
    /// Current scope's name → signal id map (module signals, or function slots).
    names: HashMap<String, SigId>,
    /// Signal id → typedef name (for struct field selects), current module.
    sig_types: HashMap<SigId, String>,
    /// User typedefs: name → type info (`pkg::name` entries included).
    types: HashMap<String, TypeDef>,
    /// Global constants: enum labels, package params (`pkg::NAME`), imported names.
    gconsts: HashMap<String, Value>,
    /// Lowered SV packages by name.
    packages: HashMap<String, ModuleIr>,
    /// All lowered functions (package functions keyed `pkg::name`), for
    /// const-evaluating calls like `cc_pkg::idx_width(Depth)` in param defaults.
    funcs_all: HashMap<String, Vec<FuncIr>>,
    /// Signals allocated during statement lowering (e.g. foreach index vars).
    extra_signals: Vec<SignalIr>,
    /// module.signals.len() after the registration pre-pass — the id base of
    /// `extra_signals`.
    extra_base: usize,
    /// Signal ids that are memories/arrays.
    sig_is_mem: HashSet<SigId>,
    /// Inside a function body: `return e;` assigns to slot 0 (function name).
    in_function: bool,
    /// Lowered SV classes by name.
    classes: HashMap<String, ClassInfo>,
    /// Per-module: class name → object-memory signal (one word per object).
    class_mems: HashMap<String, SigId>,
    /// Class whose method body is being lowered (for `this`/bare fields).
    cur_class: Option<String>,
    /// Current module's instances: instance name → port connections, for
    /// hierarchical reads `inst.port`.
    inst_conns: HashMap<String, Vec<Conn>>,
}

/// A user-defined type (typedef, enum, struct, class).
#[derive(Clone, Debug)]
pub struct TypeDef {
    width: usize,
    signed: bool,
    real: bool,
    kind: TypeKind,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    Plain,
    /// Enum labels in declaration order (label → value).
    #[allow(dead_code)]
    Enum(Vec<(String, u64)>),
    /// Packed struct fields in declaration order: (name, width, lsb offset).
    Struct(Vec<(String, usize, usize)>),
    /// Class type: instances are 64-bit handles; the class name keys `classes`.
    Class(String),
}

/// A lowered SV class: fields (incl. inherited, parent-first) with bit
/// offsets in the per-class object word, methods, and raw constraint text.
#[derive(Clone, Debug)]
struct ClassInfo {
    name: String,
    fields: Vec<(String, usize, usize, bool)>, // name, width, lsb, rand
    width: usize,
    methods: Vec<FuncIr>,
    constraint_text: String,
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
        // First pass: params (so ports and later items can use them in ranges).
        let mut env: HashMap<String, Value> = HashMap::new();
        for item in &item_nodes {
            if item.kind() == SyntaxKind::ParamDecl {
                self.lower_param(&mut module, item, &env);
                for p in &module.params {
                    env.insert(p.name.clone(), p.default.clone());
                    self.gconsts
                        .insert(format!("{}::{}", module.name, p.name), p.default.clone());
                }
            }
        }
        // Imports and typedefs come before signal registration so user types
        // are known when declarations are scanned.
        let pkg_of = if m.kind() == SyntaxKind::Package {
            Some(module.name.clone())
        } else {
            None
        };
        for item in &item_nodes {
            match item.kind() {
                SyntaxKind::Import => self.lower_import(&mut module, item, &env),
                SyntaxKind::Typedef => self.lower_typedef(item, &env, pkg_of.as_deref()),
                // Bare enum declaration: `enum logic [2:0] { A, B } s;` —
                // register the labels like a typedef enum (module scope).
                SyntaxKind::Decl
                    if real_children(item)
                        .iter()
                        .any(|k| k.kind() == SyntaxKind::EnumType) =>
                {
                    self.lower_typedef(item, &env, pkg_of.as_deref());
                }
                _ => {}
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
        // Classes declared inside the module must be registered before the
        // signal pre-pass so class-typed declarations get their type.
        for item in &item_nodes {
            if item.kind() == SyntaxKind::Class {
                self.lower_class(item);
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
                SyntaxKind::Decl => {
                    let dk = real_children(item);
                    let width = self.decl_width(&dk, &env);
                    let is_real = dk
                        .iter()
                        .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "real")
                        || self
                            .decl_type_name(&dk)
                            .and_then(|t| self.types.get(&t))
                            .is_some_and(|t| t.real);
                    let tname = self.decl_type_name(&dk);
                    for (i, k) in dk.iter().enumerate() {
                        if k.kind() == SyntaxKind::Name {
                            let n = self.text(k);
                            if self.sig_id(&n).is_none() {
                                let id = module.signals.len() as SigId;
                                // Memory dimension after the name: `mem [0:255]`,
                                // dynamic `da []` / queue `q [$]`.
                                let mem = dk.get(i + 1).and_then(|r| {
                                    if r.kind() == SyntaxKind::Range {
                                        Some(self.range_depth(r, &env))
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
        self.extra_base = module.signals.len();
        // Allocate an object memory for each class-typed declaration in this
        // module, and expose class methods as `Class::method` functions.
        for item in &item_nodes {
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

    /// Parse a `struct packed { ... }` node into a struct `TypeDef` (fields with
    /// bit offsets, first field at the MSB).
    fn struct_type_def(&mut self, st: &SyntaxNode, env: &HashMap<String, Value>) -> TypeDef {
        let sk = real_children(st);
        let mut fields: Vec<(String, usize)> = Vec::new();
        let mut i = 0;
        while i < sk.len() {
            let mut width = 1usize;
            if sk[i].kind() == SyntaxKind::Ident {
                let t = self.text(&sk[i]);
                width = type_kw_width(&t)
                    .or_else(|| self.types.get(&t).map(|td| td.width))
                    .unwrap_or(1);
                i += 1;
                if sk.get(i).is_some_and(|k| k.kind() == SyntaxKind::Range) {
                    width = self.range_width(&sk[i], env);
                    i += 1;
                }
            }
            while i < sk.len() && sk[i].kind() == SyntaxKind::Name {
                fields.push((self.text(&sk[i]), width));
                i += 1;
                if sk.get(i).is_some_and(|k| k.kind() == SyntaxKind::Range) {
                    i += 1; // array field dimension
                }
            }
            // skip `;`
            i += 1;
        }
        let total: usize = fields.iter().map(|f| f.1).sum();
        let mut lsb = total;
        let mut with_off: Vec<(String, usize, usize)> = Vec::new();
        for (n, w) in &fields {
            lsb -= w;
            with_off.push((n.clone(), *w, lsb));
        }
        let signed = sk
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "signed");
        TypeDef {
            width: total.max(1),
            signed,
            real: false,
            kind: TypeKind::Struct(with_off),
        }
    }

    /// Lower a `typedef` into the type table; enum labels also become
    /// constants. `pkg` prefixes names when the typedef lives in a package.
    fn lower_typedef(
        &mut self,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
        pkg: Option<&str>,
    ) {
        let kids = real_children(item);
        let name = kids
            .iter()
            .rev()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k));
        let Some(name) = name else { return };
        let key = match pkg {
            Some(p) => format!("{}::{}", p, name),
            None => name.clone(),
        };
        let body = kids.iter().find(|k| {
            matches!(k.kind(), SyntaxKind::EnumType | SyntaxKind::StructType)
                || (k.kind() == SyntaxKind::Ident && self.types.contains_key(&self.text(k)))
                || matches!(k.kind(), SyntaxKind::Range)
        });
        // Determine the type body: EnumType / StructType node, or a base type
        // ident (+ optional range) for `typedef logic [7:0] word_t;`.
        let enum_node = kids.iter().find(|k| k.kind() == SyntaxKind::EnumType);
        let struct_node = kids.iter().find(|k| k.kind() == SyntaxKind::StructType);
        if let Some(en) = enum_node {
            let ek = real_children(en);
            // Optional base type + range.
            let mut width = 32usize;
            if let Some(r) = ek.iter().find(|k| k.kind() == SyntaxKind::Range) {
                width = self.range_width(r, env);
            } else if ek.iter().any(|k| k.kind() == SyntaxKind::Name) {
                if let Some(r) = ek.iter().find(|k| k.kind() == SyntaxKind::Range) {
                    width = self.range_width(r, env);
                }
            }
            let mut vals: Vec<(String, u64)> = Vec::new();
            let mut next = 0u64;
            for k in &ek {
                if k.kind() == SyntaxKind::Name && self.text(k) != "enum" {
                    // Could be the base type name; enum labels are Names too.
                    // Distinguish: labels are followed by `=`/`,`/`}`; the base
                    // type name is the first Name and is a type keyword or typedef.
                    let t = self.text(k);
                    if vals.is_empty()
                        && (type_kw_width(&t).is_some() || self.types.contains_key(&t))
                    {
                        continue;
                    }
                    let mut v = next;
                    // Look for `= expr` right after this Name: the child after
                    // the label must be the `=` punct, then the value expression.
                    if let Some(pos) = ek.iter().position(|x| std::ptr::eq(x, k)) {
                        let has_eq = ek.get(pos + 1).is_some_and(|c| {
                            c.kind() == SyntaxKind::Punct
                                && self.db.span_text(c.span()).trim() == "="
                        });
                        if has_eq {
                            if let Some(e) = ek[pos + 2..].iter().find(|e| is_expr_node(e.kind())) {
                                if let Some(cv) = const_eval(&self.lower_expr(e, env), env) {
                                    if let Some(n) = cv.to_u64() {
                                        v = n;
                                    }
                                }
                            }
                        }
                    }
                    next = v + 1;
                    vals.push((t, v));
                }
            }
            for (label, v) in &vals {
                let lv = Value::from_u64(width, *v);
                self.gconsts.insert(label.clone(), lv.clone());
                if let Some(p) = pkg {
                    self.gconsts.insert(format!("{}::{}", p, label), lv);
                }
            }
            self.types.insert(
                key,
                TypeDef {
                    width,
                    signed: false,
                    real: false,
                    kind: TypeKind::Enum(vals),
                },
            );
            return;
        }
        if let Some(st) = struct_node {
            let td = self.struct_type_def(st, env);
            self.types.insert(key, td);
            return;
        }
        let _ = body;
        // Plain typedef: base type keyword or existing typedef, optional range.
        let mut width = 1usize;
        let mut signed = false;
        let mut real = false;
        if let Some(r) = kids.iter().find(|k| k.kind() == SyntaxKind::Range) {
            width = self.range_width(r, env);
        }
        for k in &kids {
            if k.kind() == SyntaxKind::Ident {
                let t = self.text(k);
                match t.as_str() {
                    "signed" => signed = true,
                    "real" => {
                        real = true;
                        width = 64;
                    }
                    "typedef" => {}
                    _ => {
                        if width == 1 {
                            if let Some(w) = type_kw_width(&t) {
                                width = w;
                            } else if let Some(td) = self.types.get(&t) {
                                width = td.width;
                                signed = td.signed;
                                real = td.real;
                            }
                        }
                    }
                }
            }
        }
        self.types.insert(
            key,
            TypeDef {
                width,
                signed,
                real,
                kind: TypeKind::Plain,
            },
        );
    }

    /// `import p::*;` / `import p::x;` — merge package scope into the module:
    /// constants, typedefs, and functions (renamed to `p::f`).
    fn lower_import(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let _ = env;
        for name_node in real_children(item)
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
        {
            let text = self.text(name_node);
            let parts: Vec<&str> = text.split("::").collect();
            let (Some(pkg), item_name) = (parts.first(), parts.get(1)) else {
                continue;
            };
            let Some(p) = self.packages.get(*pkg).cloned() else {
                self.err(
                    name_node.span(),
                    format!("unknown package '{}' in import", pkg),
                );
                continue;
            };
            let star = item_name == Some(&"*");
            for param in &p.params {
                self.gconsts
                    .insert(format!("{}::{}", pkg, param.name), param.default.clone());
                // `*` imports every parameter unqualified; a named import
                // (`pkg::item`) aliases its own params unqualified — same
                // insertion either way.
                if star || Some(param.name.as_str()) == item_name.copied() {
                    self.gconsts
                        .insert(param.name.clone(), param.default.clone());
                }
            }
            for f in &p.functions {
                let qualified = format!("{}::{}", pkg, f.name);
                if star || Some(f.name.as_str()) == item_name.copied() {
                    let mut f2 = f.clone();
                    f2.name = qualified.clone();
                    module.functions.push(f2);
                }
            }
            // Typedefs and enum labels of the package.
            for (tname, td) in &self.types.clone() {
                if let Some(rest) = tname.strip_prefix(&format!("{}::", pkg)) {
                    if star || rest == *item_name.unwrap_or(&"") {
                        self.types.insert(rest.to_string(), td.clone());
                    }
                }
            }
            for (cname, cv) in &self.gconsts.clone() {
                if let Some(rest) = cname.strip_prefix(&format!("{}::", pkg)) {
                    if star || rest == *item_name.unwrap_or(&"") {
                        self.gconsts.insert(rest.to_string(), cv.clone());
                    }
                }
            }
        }
    }

    /// Lower a top-level `class ... endclass`: register the type, fields
    /// (inherited fields first), methods (named `Class::method`), and the raw
    /// constraint-block text used by `randomize()`.
    fn lower_class(&mut self, c: &SyntaxNode) {
        let kids = real_children(c);
        let mut names = kids.iter().filter(|k| k.kind() == SyntaxKind::Name);
        let name = names.next().map(|k| self.text(k)).unwrap_or_default();
        let parent = names.next().map(|k| self.text(k));
        if name.is_empty() {
            return;
        }
        // Fields: inherited (parent-first) then own declarations.
        let mut fields: Vec<(String, usize, usize, bool)> = Vec::new();
        if let Some(p) = &parent {
            if let Some(pi) = self.classes.get(p) {
                fields.extend(pi.fields.iter().cloned());
            }
        }
        let mut own: Vec<(String, usize, bool)> = Vec::new();
        let mut constraint_text = String::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::Decl => {
                    let dk = real_children(k);
                    let width = self.decl_width(&dk, &HashMap::new());
                    let is_rand = dk.iter().any(|d| {
                        d.kind() == SyntaxKind::Ident
                            && matches!(self.text(d).as_str(), "rand" | "randc")
                    });
                    for d in &dk {
                        if d.kind() == SyntaxKind::Name {
                            own.push((self.text(d), width, is_rand));
                        }
                    }
                }
                SyntaxKind::Constraint => {
                    let text = self.db.span_text(k.span());
                    // Strip the outer braces and the constraint name.
                    let inner = text
                        .trim_start_matches(|ch| ch != '{')
                        .trim_start_matches('{');
                    let inner = inner.trim_end_matches('}').trim();
                    if !constraint_text.is_empty() {
                        constraint_text.push_str(" ; ");
                    }
                    constraint_text.push_str(inner);
                }
                _ => {}
            }
        }
        let total: usize =
            fields.iter().map(|f| f.1).sum::<usize>() + own.iter().map(|f| f.1).sum::<usize>();
        let mut lsb = total;
        for (n, w, r) in &own {
            lsb -= w;
            fields.push((n.clone(), *w, lsb, *r));
        }
        // Register the class (fields only) before lowering methods so bare
        // field references inside methods resolve via `cur_class_field`.
        self.types.insert(
            name.clone(),
            TypeDef {
                width: 64, // handle
                signed: false,
                real: false,
                kind: TypeKind::Class(name.clone()),
            },
        );
        self.classes.insert(
            name.clone(),
            ClassInfo {
                name: name.clone(),
                fields,
                width: total.max(1),
                methods: Vec::new(),
                constraint_text,
            },
        );
        // Lower methods with the class context set (this / bare fields).
        let mut methods = Vec::new();
        for k in &kids {
            if k.kind() == SyntaxKind::Function || k.kind() == SyntaxKind::Task {
                let saved = self.cur_class.replace(name.clone());
                let f = self.lower_function_ir(k, &HashMap::new());
                self.cur_class = saved;
                methods.push(f);
            }
        }
        if let Some(ci) = self.classes.get_mut(&name) {
            ci.methods = methods;
        }
    }

    /// `glbl.GSR` / `glbl.GRESTORE` / `glbl.GTS`: the Xilinx `glbl` primitive is
    /// auto-instantiated by the tools, so a hierarchical read of it materializes
    /// a per-module 1-bit signal on first reference.
    fn glbl_sig(&mut self, field: &str) -> SigId {
        let name = format!("glbl.{}", field);
        if let Some(id) = self.names.get(&name) {
            return *id;
        }
        let id = (self.extra_base + self.extra_signals.len()) as SigId;
        self.extra_signals.push(SignalIr {
            name: name.clone(),
            width: 1,
            kind: SigKind::Wire,
            init: None,
            mem: None,
            signed: false,
        });
        self.names.insert(name, id);
        id
    }

    /// Allocate (once per module) the object-memory signal for class `cn`.
    fn class_mem_sig(&mut self, cn: &str) -> Option<SigId> {
        if let Some(id) = self.class_mems.get(cn) {
            return Some(*id);
        }
        let ci = self.classes.get(cn)?.clone();
        let id = (self.extra_base + self.extra_signals.len()) as SigId;
        self.extra_signals.push(SignalIr {
            name: format!("__objmem_{}", cn),
            width: ci.width,
            kind: SigKind::Wire,
            init: None,
            mem: Some(0), // dynamic: one word per object
            signed: false,
        });
        self.sig_is_mem.insert(id);
        self.class_mems.insert(cn.to_string(), id);
        Some(id)
    }

    /// `obj.field` where obj is a class-typed signal → `$getf::C::field(obj)`.
    /// Returns None if not a class field access.
    fn lower_field_read(&mut self, _root: &str, field: &str, root_id: SigId) -> Option<Expr> {
        let tn = self.sig_types.get(&root_id)?.clone();
        let cn = match self.types.get(&tn)?.kind {
            TypeKind::Class(ref cn) => cn.clone(),
            _ => return None,
        };
        if !self
            .classes
            .get(&cn)?
            .fields
            .iter()
            .any(|(n, _, _, _)| n == field)
        {
            return None;
        }
        Some(Expr::Call {
            name: self
                .getf_name(&cn, field)
                .unwrap_or_else(|| format!("$getf::{}::{}", cn, field)),
            args: vec![Expr::Sig(root_id)],
            shapes: Vec::new(),
        })
    }

    /// Object expression for `this` / `super` (inherited fields are merged
    /// into the same word, so both are the current object).
    fn this_expr(&self) -> Expr {
        Expr::Call {
            name: "$this".to_string(),
            args: vec![],
            shapes: Vec::new(),
        }
    }

    /// `$getf::C::field@lsb,width` — the field layout is encoded in the call
    /// name so the simulator can extract field bits without a side table.
    fn getf_name(&self, cn: &str, field: &str) -> Option<String> {
        let f = self
            .classes
            .get(cn)?
            .fields
            .iter()
            .find(|(n, _, _, _)| n == field)?;
        Some(format!("$getf::{}::{}@{},{}", cn, field, f.2, f.1))
    }

    /// `$setf::C::field@lsb,width` — see [`Lower::getf_name`].
    fn setf_name(&self, cn: &str, field: &str) -> Option<String> {
        let f = self
            .classes
            .get(cn)?
            .fields
            .iter()
            .find(|(n, _, _, _)| n == field)?;
        Some(format!("$setf::{}::{}@{},{}", cn, field, f.2, f.1))
    }

    /// `$randomize::C@x=lsb,w;y=lsb,w` — rand-field layout, same order as the
    /// domains emitted into the constraint spec.
    fn randomize_name(&self, cn: &str) -> String {
        let mut lay: Vec<String> = Vec::new();
        if let Some(ci) = self.classes.get(cn) {
            for (n, w, lsb, r) in &ci.fields {
                if *r {
                    lay.push(format!("{}={},{}", n, lsb, w));
                }
            }
        }
        format!("$randomize::{}@{}", cn, lay.join(";"))
    }

    /// Is `name` a field of the class whose method is being lowered?
    fn cur_class_field(&self, name: &str) -> Option<String> {
        let cn = self.cur_class.as_ref()?;
        let ci = self.classes.get(cn)?;
        if ci.fields.iter().any(|(n, _, _, _)| n == name) {
            Some(cn.clone())
        } else {
            None
        }
    }

    /// If `lhs` is a class field write (`c.x` or bare `x` inside a method),
    /// return (class, field, object-expr).
    fn class_field_lhs(&mut self, l: &SyntaxNode) -> Option<(String, String, Expr)> {
        if l.kind() == SyntaxKind::Name {
            let name = self.text(l);
            if let Some(dot) = name.find('.') {
                let root = &name[..dot];
                let field = &name[dot + 1..];
                if let Some(id) = self.sig_id(root) {
                    if let Some(tn) = self.sig_types.get(&id).cloned() {
                        if let Some(td) = self.types.get(&tn).cloned() {
                            if let TypeKind::Class(cn) = &td.kind {
                                if self.classes.get(cn).is_some_and(|ci| {
                                    ci.fields.iter().any(|(n, _, _, _)| n == field)
                                }) {
                                    return Some((cn.clone(), field.to_string(), Expr::Sig(id)));
                                }
                            }
                        }
                    }
                }
            } else if let Some(cn) = self.cur_class_field(&name) {
                return Some((cn, name, self.this_expr()));
            }
        }
        None
    }

    /// Constraint spec text for `randomize()`: rand field domains plus the
    /// class's constraint blocks.
    fn randomize_spec(&self, cn: &str) -> String {
        let mut spec = String::new();
        if let Some(ci) = self.classes.get(cn) {
            for (n, w, _, r) in &ci.fields {
                if *r {
                    let hi = if *w >= 64 { i64::MAX } else { (1i64 << w) - 1 };
                    spec.push_str(&format!("{}: 0..{}; ", n, hi));
                }
            }
            if !ci.constraint_text.is_empty() {
                spec.push_str(&ci.constraint_text);
            }
        }
        spec
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
            mem: None,
            signed: false,
        });
        module.ports.push(PortIr { name, dir, sig: id });
    }

    /// Width of a declaration: an explicit `[msb:lsb]` range wins, else the
    /// type keyword (`integer` = 32 bits, …), else 1.
    fn decl_width(&mut self, kids: &[SyntaxNode], env: &HashMap<String, Value>) -> usize {
        let name_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        // Packed dimensions multiply: `logic [A-1:0][B-1:0] x` is A*B wide.
        let packed: Vec<&SyntaxNode> = kids
            .iter()
            .enumerate()
            .filter(|(i, k)| k.kind() == SyntaxKind::Range && name_pos.is_none_or(|np| *i < np))
            .map(|(_, k)| k)
            .collect();
        if !packed.is_empty() {
            return packed.iter().map(|r| self.range_width(r, env)).product();
        }
        if let Some(t) = self.decl_type_name(kids) {
            if let Some(td) = self.types.get(&t) {
                return td.width;
            }
        }
        kids.iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .find_map(|k| type_kw_width(&self.text(k)))
            .unwrap_or(1)
    }

    /// The declared type name of a Decl node when it is a user type
    /// (`word_t w;` → `word_t`), else None.
    fn decl_type_name(&self, kids: &[SyntaxNode]) -> Option<String> {
        // Skip modifiers (var/const/rand/...), then the first Ident that is a
        // known user type (a type keyword like `int` is not in `types`).
        for k in kids {
            if k.kind() == SyntaxKind::Ident {
                let t = self.text(k);
                if self.types.contains_key(&t) {
                    return Some(t);
                }
            }
            if k.kind() == SyntaxKind::Name {
                break;
            }
        }
        None
    }

    /// Memory depth of a dimension after a name: `[0:255]` → 256,
    /// `[]` / `[$]` (dynamic array / queue) → 0 (grows on demand),
    /// `[n]` (assoc) → 0 as well (sparse).
    fn range_depth(&mut self, r: &SyntaxNode, env: &HashMap<String, Value>) -> usize {
        let kids = real_children(r);
        // Assoc-array key type (`aa[string]`) or queue `q[$]`: no depth.
        let nonpunct: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| k.kind() != SyntaxKind::Punct)
            .collect();
        if nonpunct.len() == 1 && nonpunct[0].kind() == SyntaxKind::Name {
            return 0;
        }
        let exprs: Vec<Expr> = kids
            .into_iter()
            .filter(|c| is_expr_node(c.kind()))
            .map(|c| self.lower_expr(&c, env))
            .collect();
        if exprs.is_empty() {
            return 0; // `[]` or `[$]`
        }
        if exprs.len() == 1 {
            if let Some(v) = self.ce(&exprs[0], env).and_then(|v| v.to_u64()) {
                // `mem [255:0]` style single bound, or assoc `[string]` (non-const).
                return v as usize + 1;
            }
            return 0; // assoc array key type, e.g. `[string]`
        }
        if let (Some(a), Some(b)) = (self.ce(&exprs[0], env), self.ce(&exprs[1], env)) {
            if let (Some(a), Some(b)) = (a.to_u64(), b.to_u64()) {
                return (a as i64 - b as i64).unsigned_abs() as usize + 1;
            }
        }
        0
    }

    fn range_width(&mut self, r: &SyntaxNode, env: &HashMap<String, Value>) -> usize {
        let exprs: Vec<Expr> = real_children(r)
            .into_iter()
            .filter(|c| is_expr_node(c.kind()))
            .map(|c| self.lower_expr(&c, env))
            .collect();
        if exprs.len() == 2 {
            if let (Some(a), Some(b)) = (self.ce(&exprs[0], env), self.ce(&exprs[1], env)) {
                if let (Some(a), Some(b)) = (a.to_u64(), b.to_u64()) {
                    return (a as i64 - b as i64).unsigned_abs() as usize + 1;
                }
            }
        } else if exprs.len() == 1 {
            if let Some(v) = self.ce(&exprs[0], env) {
                if let Some(w) = v.to_u64() {
                    return w as usize;
                }
            }
        }
        self.err(r.span(), "cannot evaluate range bounds to constants");
        1
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
        let edges = match &sens {
            Sens::List(es) => es.clone(),
            Sens::All => {
                self.err(
                    prop_node.span(),
                    "assert property needs an explicit `@(...)` clocking event",
                );
                return;
            }
        };
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
                return;
            }
        };
        module.processes.push(ProcessIr {
            kind: ProcKind::Always,
            sens,
            body,
            origin: format!("assert property {}", self.text(prop_node)),
            initial: false,
        });
    }

    fn lower_module_item(
        &mut self,
        module: &mut ModuleIr,
        item: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        match item.kind() {
            SyntaxKind::AssertProperty => self.lower_assert_property(module, item, env),
            SyntaxKind::AssumeProperty | SyntaxKind::CoverProperty => {
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
            SyntaxKind::Port | SyntaxKind::Decl | SyntaxKind::ParamDecl => {
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
            // Handled in the pre-pass.
            SyntaxKind::Typedef | SyntaxKind::Import => {}
            SyntaxKind::Class | SyntaxKind::Package => {}
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
                // Optional memory dimension after the name: `reg [7:0] mem [0:255]`.
                if kids
                    .get(i + 1)
                    .is_some_and(|k| k.kind() == SyntaxKind::Range)
                {
                    let depth = self.range_depth(&kids[i + 1], env);
                    if let Some(id) = self.sig_id(&name) {
                        module.signals[id as usize].mem = Some(depth);
                    }
                    i += 2;
                    continue;
                }
                // Optional `= expr` after the name.
                if kids.get(i + 1).is_some_and(|k| self.text(k) == "=") {
                    if let Some(e) = kids.get(i + 2) {
                        if e.kind() == SyntaxKind::Allocator {
                            // `C c = new(args);` — runtime constructor: emit a
                            // one-shot initial process performing the $new call.
                            let cn = self
                                .decl_type_name(&kids)
                                .and_then(|t| self.types.get(&t).cloned())
                                .and_then(|td| match td.kind {
                                    TypeKind::Class(cn) => Some(cn),
                                    _ => None,
                                });
                            if let (Some(cn), Some(id)) = (cn, self.sig_id(&name)) {
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
                                    initial: true,
                                });
                            }
                            i += 3;
                            continue;
                        }
                        if is_expr_node(e.kind()) {
                            let expr = self.lower_expr(e, env);
                            if let Some(id) = self.sig_id(&name) {
                                let w = module.signals[id as usize].width.max(1);
                                module.signals[id as usize].init =
                                    const_eval(&expr, env).map(|v| v.resize(w));
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
                    i += 3;
                    continue;
                }
                if let Some(e) = kids.get(i + 2) {
                    if is_expr_node(e.kind()) {
                        let expr = self.lower_expr(e, env);
                        let val = self.ce(&expr, env).unwrap_or_else(|| Value::x(32));
                        let pname = self.text(&kids[i]);
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
        // No sensitivity list at all (`always #5 ...`): an empty list means
        // "no event sensitivity — start once at t=0 and loop on the body".
        // `@*` lowers to Sens::All via the SensList arm below.
        let mut sens = Sens::List(Vec::new());
        let mut body = None;
        let mut origin = String::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::SensList => {
                    sens = self.lower_sens_list(k, env);
                    let kw = match kind {
                        ProcKind::Initial => "initial",
                        ProcKind::Always => "always",
                    };
                    origin = format!("{} @{}", kw, self.text(k));
                }
                SyntaxKind::Ident | SyntaxKind::Punct => {}
                _ => body = Some(k.clone()),
            }
        }
        let body = match body {
            Some(b) => self.lower_stmt(&b, env),
            None => Stmt::Null,
        };
        if origin.is_empty() {
            origin = match kind {
                ProcKind::Initial => "initial".to_string(),
                ProcKind::Always => "always".to_string(),
            };
        }
        module.processes.push(ProcessIr {
            kind,
            sens,
            body,
            origin,
            initial: false,
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
                    // `a`, `a[1:0]`, `a[i]` — the entry's base signal.
                    let base_sig = match &e {
                        Expr::Sig(id) => Some(*id),
                        Expr::Idx { e, .. } | Expr::Sel { e, .. } => match &**e {
                            Expr::Sig(id) => Some(*id),
                            _ => None,
                        },
                        _ => None,
                    };
                    match base_sig {
                        Some(id) => edges.push(Edge {
                            sig: id,
                            kind: pending,
                        }),
                        None => self.err(c.span(), "sensitivity list entry must be a signal"),
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

    /// Lower one statement, wrapped in a `Stmt::Spanned` with its source span
    /// (drives line/branch coverage). Recursive calls go through this wrapper,
    /// so every nested statement is spanned too.
    /// For-loop init statement. `for (int i = 0; ...)` arrives as a
    /// StmtAssign whose leading type keyword marks it as a declaration:
    /// register the loop variable, then emit its initializer assignment.
    fn lower_for_init(&mut self, st: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        if st.kind() == SyntaxKind::StmtAssign {
            let ik = real_children(st);
            let name_pos = ik.iter().position(|k| k.kind() == SyntaxKind::Name);
            let is_decl = name_pos.is_some_and(|np| {
                ik[..np].iter().any(|k| {
                    k.kind() == SyntaxKind::Ident && type_kw_width(&self.text(k)).is_some()
                })
            });
            if is_decl {
                let np = name_pos.expect("is_decl is only true when name_pos was found");
                let n = self.text(&ik[np]);
                if self.sig_id(&n).is_none() {
                    let id = (self.extra_base + self.extra_signals.len()) as SigId;
                    self.extra_signals.push(SignalIr {
                        name: n.clone(),
                        width: 32,
                        kind: SigKind::Wire,
                        init: None,
                        mem: None,
                        signed: false,
                    });
                    self.names.insert(n, id);
                }
                if ik.get(np + 1).is_some_and(|k| self.text(k) == "=") {
                    if let Some(e) = ik.get(np + 2).filter(|e| is_expr_node(e.kind())) {
                        return Stmt::Assign {
                            lhs: Expr::Sig(self.sig_id(&self.text(&ik[np])).expect(
                                "the signal was registered just above when it was missing",
                            )),
                            rhs: self.lower_expr(e, env),
                            nba: false,
                        };
                    }
                }
                return Stmt::Null;
            }
        }
        self.lower_stmt(st, env)
    }

    fn lower_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let span = s.span();
        let inner = self.lower_stmt_impl(s, env);
        Stmt::Spanned {
            span,
            inner: Box::new(inner),
        }
    }

    fn lower_stmt_impl(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        match s.kind() {
            SyntaxKind::SvaStmt => {
                // Statement-level SVA: `assert (e) [else s];` lowers to the
                // always-failing `sva_assert` check; assume/cover/expect are
                // parsed but not simulated.
                let kids = real_children(s);
                let kw = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.text(k))
                    .unwrap_or_default();
                if kw != "assert" {
                    self.diags.push(Diagnostic::warning(
                        "W0401",
                        Some(s.span()),
                        format!("`{}` statement is parsed but not simulated", kw),
                    ));
                    return Stmt::Null;
                }
                let expr = kids.iter().find(|k| is_expr_node(k.kind()));
                let els = kids.iter().find(|k| {
                    !is_expr_node(k.kind())
                        && !matches!(k.kind(), SyntaxKind::Ident | SyntaxKind::Punct)
                });
                match expr {
                    Some(e) => {
                        let e = self.lower_expr(e, env);
                        match els {
                            // `assert (e) else s;` == `if (!e) s;`
                            Some(st) => Stmt::If {
                                cond: Expr::Un {
                                    op: UnOp::Not,
                                    e: Box::new(e),
                                },
                                then: Box::new(self.lower_stmt(st, env)),
                                els: None,
                            },
                            None => Stmt::SysCall {
                                name: "sva_assert".to_string(),
                                args: vec![e],
                                shapes: Vec::new(),
                            },
                        }
                    }
                    None => {
                        self.err(s.span(), "malformed assert statement");
                        Stmt::Null
                    }
                }
            }
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
                        if i > 0
                            && body[i - 1].kind() == SyntaxKind::Punct
                            && self.text(body[i - 1]) == "<"
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
                            (Some(l), Some(r)) => {
                                // `obj.field = v` / `this.x = v` → $setf call
                                // (expression form so it also works inside
                                // functions, where only &self evaluation runs).
                                if let Some((cn, field, obj)) = self.class_field_lhs(l) {
                                    return Stmt::ExprStmt(Expr::Call {
                                        name: self
                                            .setf_name(&cn, &field)
                                            .unwrap_or_else(|| format!("$setf::{}::{}", cn, field)),
                                        args: vec![obj, self.lower_expr(r, env)],
                                        shapes: Vec::new(),
                                    });
                                }
                                // `c = new C(args)` → $new syscall (alloc + ctor).
                                if r.kind() == SyntaxKind::Allocator {
                                    let ak = real_children(r);
                                    let has_paren = ak.iter().any(|k| {
                                        k.kind() == SyntaxKind::Punct && self.text(k) == "("
                                    });
                                    if has_paren {
                                        let cn = ak
                                            .iter()
                                            .find(|k| k.kind() == SyntaxKind::Name)
                                            .map(|k| self.text(k))
                                            .or_else(|| self.cur_class.clone())
                                            .or_else(|| {
                                                // `c = new(...)`: class from the LHS type.
                                                if l.kind() == SyntaxKind::Name {
                                                    self.sig_id(&self.text(l)).and_then(|id| {
                                                        self.sig_types.get(&id).and_then(|t| {
                                                            self.types.get(t).and_then(|td| {
                                                                match &td.kind {
                                                                    TypeKind::Class(cn) => {
                                                                        Some(cn.clone())
                                                                    }
                                                                    _ => None,
                                                                }
                                                            })
                                                        })
                                                    })
                                                } else {
                                                    None
                                                }
                                            });
                                        if let Some(cn) = cn {
                                            let mut args = vec![self.lower_expr(l, env)];
                                            args.extend(
                                                ak.iter()
                                                    .filter(|k| {
                                                        is_expr_node(k.kind())
                                                            && k.kind() != SyntaxKind::Name
                                                    })
                                                    .map(|k| self.lower_expr(k, env)),
                                            );
                                            return Stmt::SysCall {
                                                name: format!("$new::{}", cn),
                                                args,
                                                shapes: Vec::new(),
                                            };
                                        }
                                    }
                                }
                                // `arr = new[n]` — dynamic array allocation.
                                if r.kind() == SyntaxKind::Allocator
                                    && real_children(r).iter().any(|k| {
                                        k.kind() == SyntaxKind::Punct && self.text(k) == "["
                                    })
                                {
                                    let size = real_children(r)
                                        .into_iter()
                                        .find(|k| is_expr_node(k.kind()))
                                        .map(|k| self.lower_expr(&k, env))
                                        .unwrap_or_else(|| Expr::Lit(Value::from_u64(32, 0)));
                                    return Stmt::SysCall {
                                        name: "$array_new".to_string(),
                                        args: vec![self.lower_expr(l, env), size],
                                        shapes: Vec::new(),
                                    };
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
                            _ => Stmt::Null,
                        }
                    }
                    None => match body.iter().find(|k| is_expr_node(k.kind())) {
                        Some(e) => {
                            // Class method call as a statement: `c.randomize();`, `c.do();`
                            if e.kind() == SyntaxKind::Call {
                                let ck = real_children(e);
                                let cname = ck
                                    .first()
                                    .map(|k| self.text(k).trim_start_matches('$').to_string())
                                    .unwrap_or_default();
                                if let Some(dot) = cname.rfind('.') {
                                    let base = &cname[..dot];
                                    let method = &cname[dot + 1..];
                                    if let Some(id) = self.sig_id(base) {
                                        if let Some(tn) = self.sig_types.get(&id).cloned() {
                                            if let Some(td) = self.types.get(&tn).cloned() {
                                                if let TypeKind::Class(cn) = &td.kind {
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
                                                        return Stmt::ExprStmt(Expr::Call {
                                                            name: self.randomize_name(cn),
                                                            args,
                                                            shapes: Vec::new(),
                                                        });
                                                    }
                                                    return Stmt::SysCall {
                                                        name: format!(
                                                            "$callmethod::{}::{}",
                                                            cn, method
                                                        ),
                                                        args,
                                                        shapes: Vec::new(),
                                                    };
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // Array method call as a statement: `q.push_back(x);`
                            if e.kind() == SyntaxKind::Call {
                                let ck = real_children(e);
                                let cname = ck
                                    .first()
                                    .map(|k| self.text(k).trim_start_matches('$').to_string())
                                    .unwrap_or_default();
                                if let Some(dot) = cname.find('.') {
                                    let base = &cname[..dot];
                                    let method = &cname[dot + 1..];
                                    if let Some(id) = self.sig_id(base) {
                                        if self.sig_is_mem.contains(&id) {
                                            let mut args = vec![Expr::Sig(id)];
                                            args.extend(
                                                ck[1..]
                                                    .iter()
                                                    .filter(|k| is_expr_node(k.kind()))
                                                    .map(|k| self.lower_expr(k, env)),
                                            );
                                            return Stmt::SysCall {
                                                name: format!("$array_{}", method),
                                                args,
                                                shapes: Vec::new(),
                                            };
                                        }
                                    }
                                }
                            }
                            // Bare task call without parens: `my_task;` — a
                            // name that is neither a signal nor a constant.
                            if e.kind() == SyntaxKind::Name {
                                let n = self.text(e);
                                if self.sig_id(&n).is_none()
                                    && !self.gconsts.contains_key(&n)
                                    && !self.gconsts.contains_key(&n.to_ascii_lowercase())
                                {
                                    return Stmt::ExprStmt(Expr::Call {
                                        name: n,
                                        args: Vec::new(),
                                        shapes: Vec::new(),
                                    });
                                }
                            }
                            Stmt::ExprStmt(self.lower_expr(e, env))
                        }
                        None => Stmt::Null,
                    },
                }
            }
            // Block-level declaration (`int x = 3;` inside begin/end):
            // allocate a hidden signal, then lower any init as an assignment.
            SyntaxKind::Decl => {
                let dk = real_children(s);
                let width = self.decl_width(&dk, env);
                let tname = self.decl_type_name(&dk);
                let mut inits = Vec::new();
                for (i, k) in dk.iter().enumerate() {
                    if k.kind() == SyntaxKind::Name {
                        let n = self.text(k);
                        if self.sig_id(&n).is_none() {
                            let id = (self.extra_base + self.extra_signals.len()) as SigId;
                            let mem = dk.get(i + 1).and_then(|r| {
                                if r.kind() == SyntaxKind::Range {
                                    Some(self.range_depth(r, env))
                                } else {
                                    None
                                }
                            });
                            self.extra_signals.push(SignalIr {
                                name: n.clone(),
                                width,
                                kind: SigKind::Wire,
                                init: None,
                                mem,
                                signed: false,
                            });
                            if mem.is_some() {
                                self.sig_is_mem.insert(id);
                            }
                            if let Some(t) = &tname {
                                self.sig_types.insert(id, t.clone());
                            }
                            self.names.insert(n, id);
                        }
                        // `= expr` init?
                        if dk.get(i + 1).is_some_and(|k| self.text(k) == "=") {
                            if let Some(e) = dk.get(i + 2).filter(|e| is_expr_node(e.kind())) {
                                inits.push(Stmt::Assign {
                                    lhs: Expr::Sig(self.sig_id(&self.text(k)).expect(
                                        "the signal was registered earlier in this declaration",
                                    )),
                                    rhs: self.lower_expr(e, env),
                                    nba: false,
                                });
                            }
                        }
                    }
                }
                if inits.is_empty() {
                    Stmt::Null
                } else if inits.len() == 1 {
                    inits.pop().expect("checked inits.len() == 1")
                } else {
                    Stmt::Block(inits)
                }
            }
            SyntaxKind::Foreach => {
                // `foreach (arr[i]) body` → for (i = 0; i < arr.size(); i++) body
                let kids = real_children(s);
                let hdr = kids.iter().find(|k| is_expr_node(k.kind())).cloned();
                let body_node = kids.iter().find(|k| is_stmt_node(k.kind())).cloned();
                let (arr_node, idx_name): (Option<SyntaxNode>, String) = match &hdr {
                    Some(h) if h.kind() == SyntaxKind::Index => {
                        let ik = real_children(h);
                        let exprs: Vec<&SyntaxNode> =
                            ik.iter().filter(|k| is_expr_node(k.kind())).collect();
                        let arr = exprs.first().map(|n| (*n).clone());
                        let idx = exprs
                            .get(1)
                            .filter(|n| n.kind() == SyntaxKind::Name)
                            .map(|n| self.text(n))
                            .unwrap_or_else(|| "i".to_string());
                        (arr, idx)
                    }
                    Some(h) => (Some(h.clone()), "i".to_string()),
                    None => (None, "i".to_string()),
                };
                let Some(arr_node) = arr_node else {
                    self.err(s.span(), "foreach needs an array");
                    return Stmt::Null;
                };
                let id = (self.extra_base + self.extra_signals.len()) as SigId;
                self.extra_signals.push(SignalIr {
                    name: format!("__foreach_{}", idx_name),
                    width: 32,
                    kind: SigKind::Wire,
                    init: None,
                    mem: None,
                    signed: false,
                });
                self.names.insert(idx_name, id);
                let arr = self.lower_expr(&arr_node, env);
                let idx = Expr::Sig(id);
                let body = body_node
                    .map(|b| Box::new(self.lower_stmt(&b, env)))
                    .unwrap_or_else(|| Box::new(Stmt::Null));
                Stmt::For {
                    init: Box::new(Stmt::Assign {
                        lhs: idx.clone(),
                        rhs: Expr::Lit(Value::from_u64(32, 0)),
                        nba: false,
                    }),
                    cond: Expr::Bin {
                        op: BinOp::Lt,
                        l: Box::new(idx.clone()),
                        r: Box::new(Expr::Call {
                            name: "$array_size".to_string(),
                            args: vec![arr],
                            shapes: Vec::new(),
                        }),
                    },
                    step: Box::new(Stmt::Assign {
                        lhs: idx.clone(),
                        rhs: Expr::Bin {
                            op: BinOp::Add,
                            l: Box::new(idx),
                            r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                        },
                        nba: false,
                    }),
                    body,
                }
            }
            // `return expr;` in a function: set the return slot and stop.
            SyntaxKind::ReturnStmt if self.in_function => {
                let kids = real_children(s);
                let e = kids.iter().find(|k| is_expr_node(k.kind()));
                Stmt::Return(Box::new(
                    e.map(|e| self.lower_expr(e, env))
                        .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0))),
                ))
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
            SyntaxKind::Wait => {
                let kids = real_children(s);
                let sens_node = kids.iter().find(|k| k.kind() == SyntaxKind::SensList);
                let body = kids
                    .iter()
                    .find(|k| is_stmt_node(k.kind()) && k.kind() != SyntaxKind::SensList);
                let edges = match sens_node.map(|sl| self.lower_sens_list(sl, env)) {
                    Some(Sens::List(es)) => es,
                    _ => Vec::new(),
                };
                let body = body
                    .map(|b| Box::new(self.lower_stmt(b, env)))
                    .unwrap_or_else(|| Box::new(Stmt::Null));
                Stmt::Wait { sens: edges, body }
            }
            SyntaxKind::For => {
                let kids = real_children(s);
                let stmts: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_stmt_node(k.kind())).collect();
                // Lower the init first: `for (int i = 0; ...)` declares `i`,
                // which the condition, step and body reference.
                let init = stmts
                    .first()
                    .map(|st| self.lower_for_init(st, env))
                    .unwrap_or(Stmt::Null);
                let cond = kids
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .map(|c| self.lower_expr(c, env))
                    .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
                if stmts.len() >= 3 {
                    Stmt::For {
                        init: Box::new(init),
                        cond,
                        step: Box::new(self.lower_stmt(stmts[1], env)),
                        body: Box::new(self.lower_stmt(stmts[2], env)),
                    }
                } else {
                    Stmt::Null
                }
            }
            SyntaxKind::While => {
                // Desugar `while (c) body` into a For loop with no init/step.
                let kids = real_children(s);
                let cond = kids
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .map(|c| self.lower_expr(c, env))
                    .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
                let body = kids
                    .iter()
                    .find(|k| is_stmt_node(k.kind()))
                    .map(|b| Box::new(self.lower_stmt(b, env)))
                    .unwrap_or_else(|| Box::new(Stmt::Null));
                Stmt::For {
                    init: Box::new(Stmt::Null),
                    cond,
                    step: Box::new(Stmt::Null),
                    body,
                }
            }
            SyntaxKind::Forever => {
                let kids = real_children(s);
                let body = kids
                    .iter()
                    .find(|k| is_stmt_node(k.kind()))
                    .map(|b| Box::new(self.lower_stmt(b, env)))
                    .unwrap_or_else(|| Box::new(Stmt::Null));
                Stmt::For {
                    init: Box::new(Stmt::Null),
                    cond: Expr::Lit(Value::from_u64(1, 1)),
                    step: Box::new(Stmt::Null),
                    body,
                }
            }
            SyntaxKind::DoWhile => {
                // `do body while (c)` = body; while (c) body;
                let kids = real_children(s);
                let stmts: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_stmt_node(k.kind())).collect();
                let cond = kids
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .map(|c| self.lower_expr(c, env))
                    .unwrap_or_else(|| Expr::Lit(Value::from_u64(0, 1)));
                match stmts.first() {
                    Some(b) => {
                        let body = self.lower_stmt(b, env);
                        Stmt::Block(vec![
                            body.clone(),
                            Stmt::For {
                                init: Box::new(Stmt::Null),
                                cond,
                                step: Box::new(Stmt::Null),
                                body: Box::new(body),
                            },
                        ])
                    }
                    None => Stmt::Null,
                }
            }
            SyntaxKind::Repeat => {
                let kids = real_children(s);
                let count = kids
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .map(|c| self.lower_expr(c, env))
                    .unwrap_or_else(|| Expr::Lit(Value::from_u64(0, 32)));
                let body = kids
                    .iter()
                    .find(|k| is_stmt_node(k.kind()))
                    .map(|b| Box::new(self.lower_stmt(b, env)))
                    .unwrap_or_else(|| Box::new(Stmt::Null));
                Stmt::Repeat { count, body }
            }
            SyntaxKind::BreakStmt => Stmt::Break,
            SyntaxKind::ContinueStmt => Stmt::Continue,
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
                Stmt::SysCall {
                    name,
                    args,
                    shapes: Vec::new(),
                }
            }
            _ => Stmt::Null,
        }
    }

    /// Lower `'{a: x, b: y}` against the LHS struct type: build a concat in
    /// field order (msb first). Missing fields fall back to positional
    /// entries, then `default:`, then X.
    fn lower_pattern(
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
        Some(Expr::Concat(parts))
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
            matching: false,
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
        let Some(mut i) = init_val else {
            self.err(g.span(), "generate for: init value not constant");
            return;
        };
        let step = &assigns[1];
        let body_node = body.expect("body.is_none() returned early above").clone();
        let mut iter = 0u32;
        loop {
            if iter > 1_000_000 {
                self.err(g.span(), "generate for: too many iterations (>1M)");
                return;
            }
            iter += 1;
            let mut env2 = env.clone();
            env2.insert(var.clone(), Value::from_u64(32, i));
            // Lower the step against the current iteration value so `i = i + 1`
            // const-evaluates.
            let step_expr = {
                let sk = real_children(step);
                let op_text: String = sk
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Punct)
                    .map(|k| self.text(k))
                    .collect();
                let exprs: Vec<&SyntaxNode> =
                    sk.iter().filter(|k| is_expr_node(k.kind())).collect();
                // Compound step `j += 2` / `j -= 1`: desugar to `j = j +/- 2`.
                if exprs.len() >= 2 && (op_text.contains("+=") || op_text.contains("-=")) {
                    let op = if op_text.contains("-=") {
                        BinOp::Sub
                    } else {
                        BinOp::Add
                    };
                    Some(Expr::Bin {
                        op,
                        l: Box::new(self.lower_expr(exprs[0], &env2)),
                        r: Box::new(self.lower_expr(exprs[exprs.len() - 1], &env2)),
                    })
                } else if !exprs.is_empty() && op_text.contains("++") {
                    Some(Expr::Bin {
                        op: BinOp::Add,
                        l: Box::new(self.lower_expr(exprs[0], &env2)),
                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                    })
                } else if !exprs.is_empty() && op_text.contains("--") {
                    Some(Expr::Bin {
                        op: BinOp::Sub,
                        l: Box::new(self.lower_expr(exprs[0], &env2)),
                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                    })
                } else {
                    exprs.last().map(|k| self.lower_expr(k, &env2))
                }
            };
            let go = cond_node
                .map(|c| self.lower_expr(c, &env2))
                .and_then(|e| const_eval(&e, &env2))
                .and_then(|v| v.to_u64())
                .is_some_and(|v| v != 0);
            if !go {
                break;
            }
            match body_node.kind() {
                SyntaxKind::Block | SyntaxKind::GenBlock => {
                    for k in real_children(&body_node) {
                        // Skip `begin`/`end` keywords, `:` and the block label.
                        if matches!(
                            k.kind(),
                            SyntaxKind::Ident | SyntaxKind::Punct | SyntaxKind::Name
                        ) {
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
                for k in real_children(b) {
                    // Skip `begin`/`end` keywords, `:` and the block label.
                    if matches!(
                        k.kind(),
                        SyntaxKind::Ident | SyntaxKind::Punct | SyntaxKind::Name
                    ) {
                        continue;
                    }
                    self.lower_module_item(module, &k.clone(), env);
                }
            } else {
                self.lower_module_item(module, b, env);
            }
        }
    }

    // ---- functions / instances -------------------------------------------------

    fn lower_function(
        &mut self,
        module: &mut ModuleIr,
        f: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let func = self.lower_function_ir(f, env);
        module.functions.push(func);
    }

    fn lower_function_ir(&mut self, f: &SyntaxNode, env: &HashMap<String, Value>) -> FuncIr {
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
        let mut args = Vec::new();
        // Slot 0 is the function name (the return value); args follow.
        let mut slots: Vec<(String, usize)> = vec![(name.clone(), ret_width.max(1))];
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
                .or_else(|| {
                    ak.iter()
                        .filter(|k| k.kind() == SyntaxKind::Ident)
                        .find_map(|k| type_kw_width(&self.text(k)))
                })
                .unwrap_or(1);
            args.push((aname.clone(), aw, String::new()));
            slots.push((aname, aw));
        }
        let mut locals = Vec::new();
        for d in &local_nodes {
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
        // Lower the body in slot space.
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
        for c in &kids[i..] {
            if c.kind() == SyntaxKind::PortConn {
                conns.push(self.lower_conn(c, env))
            }
        }
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
        module.instances.push(InstIr {
            module: module_name,
            name: inst_name,
            param_overrides,
            conns,
            config: None,
        });
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
            (Some(n), None) => Conn::Named(n, Expr::Lit(Value::x(1))),
            (None, None) => Conn::Positional(Expr::Lit(Value::x(1))),
        }
    }

    // ---- expressions ------------------------------------------------------------

    /// `lhs inside { a, b, [lo:hi], ... }` — OR chain of `lhs == item` and
    /// `lo <= lhs <= hi` range comparisons.
    fn lower_inside(&mut self, kids: &[SyntaxNode], env: &HashMap<String, Value>) -> Expr {
        let inside_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "inside")
            .unwrap_or(0);
        let Some(lhs_node) = kids[..inside_pos]
            .iter()
            .rev()
            .find(|k| is_expr_node(k.kind()))
        else {
            return Expr::Lit(Value::x(1));
        };
        let x = self.lower_expr(lhs_node, env);
        let rest = &kids[inside_pos + 1..];
        let mut terms: Vec<Expr> = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            if !is_expr_node(rest[i].kind()) {
                i += 1;
                continue;
            }
            let v = self.lower_expr(&rest[i], env);
            let is_range = rest
                .get(i + 1)
                .is_some_and(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
            if is_range {
                if let Some(hi_node) = rest[i + 2..].iter().find(|k| is_expr_node(k.kind())) {
                    let hi = self.lower_expr(hi_node, env);
                    terms.push(Expr::Bin {
                        op: BinOp::LogAnd,
                        l: Box::new(Expr::Bin {
                            op: BinOp::Ge,
                            l: Box::new(x.clone()),
                            r: Box::new(v),
                        }),
                        r: Box::new(Expr::Bin {
                            op: BinOp::Le,
                            l: Box::new(x.clone()),
                            r: Box::new(hi),
                        }),
                    });
                    // Skip past the range's hi expression.
                    if let Some(j) = rest[i + 2..].iter().position(|k| std::ptr::eq(k, hi_node)) {
                        i = i + 2 + j + 1;
                        continue;
                    }
                }
            } else {
                terms.push(Expr::Bin {
                    op: BinOp::Eq,
                    l: Box::new(x.clone()),
                    r: Box::new(v),
                });
            }
            i += 1;
        }
        let mut acc = Expr::Lit(Value::from_u64(1, 0));
        for t in terms {
            acc = Expr::Bin {
                op: BinOp::LogOr,
                l: Box::new(acc),
                r: Box::new(t),
            };
        }
        acc
    }

    fn lower_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        match e.kind() {
            SyntaxKind::Literal => self.lower_literal(e),
            SyntaxKind::Name => {
                let name = self.text(e);
                if name == "this" || name == "super" {
                    return self.this_expr();
                }
                if let Some(v) = env.get(&name).or_else(|| self.gconsts.get(&name)) {
                    return Expr::Lit(v.clone());
                }
                // Dotted name: struct field select, class field read, or a
                // hierarchical read of an instance port (`arb.data`).
                if let Some(dot) = name.find('.') {
                    let root = &name[..dot];
                    let field = &name[dot + 1..];
                    // Hierarchical read of the auto-instantiated `glbl` primitive.
                    if root.eq_ignore_ascii_case("glbl") {
                        return Expr::Sig(self.glbl_sig(field));
                    }
                    if self.sig_id(root).is_none() {
                        if let Some(conns) = self.inst_conns.get(root) {
                            if let Some(Conn::Named(_, ex)) = conns
                                .iter()
                                .find(|c| matches!(c, Conn::Named(n, _) if n == field))
                            {
                                return ex.clone();
                            }
                        }
                    }
                    if let Some(id) = self.sig_id(root) {
                        if let Some(tn) = self.sig_types.get(&id).cloned() {
                            if let Some(td) = self.types.get(&tn).cloned() {
                                match &td.kind {
                                    TypeKind::Struct(fields) => {
                                        if let Some((_, fw, lsb)) =
                                            fields.iter().find(|(n, _, _)| n == field)
                                        {
                                            return Expr::Sel {
                                                e: Box::new(Expr::Sig(id)),
                                                msb: Box::new(Expr::Lit(Value::from_u64(
                                                    32,
                                                    (lsb + fw - 1) as u64,
                                                ))),
                                                lsb: Box::new(Expr::Lit(Value::from_u64(
                                                    32,
                                                    *lsb as u64,
                                                ))),
                                            };
                                        }
                                    }
                                    TypeKind::Class(_) => {
                                        if let Some(x) = self.lower_field_read(root, field, id) {
                                            return x;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                let base = name.rsplit('.').next().unwrap_or(name.as_str());
                if let Some(v) = env.get(base).or_else(|| self.gconsts.get(base)) {
                    return Expr::Lit(v.clone());
                }
                match self.sig_id(base) {
                    Some(id) => Expr::Sig(id),
                    None => {
                        // Bare field of the enclosing class method: `x` ≡ `this.x`.
                        if let Some(cn) = self.cur_class_field(base) {
                            return Expr::Call {
                                name: self
                                    .getf_name(&cn, base)
                                    .unwrap_or_else(|| format!("$getf::{}::{}", cn, base)),
                                args: vec![self.this_expr()],
                                shapes: Vec::new(),
                            };
                        }
                        // A bare typedef name used as a value (passing a type to a
                        // `parameter type` port, `.rule_t(rule_t)`) — treat as 0.
                        if self.types.contains_key(base) {
                            return Expr::Lit(Value::from_u64(1, 0));
                        }
                        // A bare type keyword used as a value (`parameter type t = logic`)
                        // means the default type; treat as 0.
                        if matches!(
                            base,
                            "logic"
                                | "reg"
                                | "wire"
                                | "bit"
                                | "int"
                                | "integer"
                                | "longint"
                                | "shortint"
                                | "byte"
                                | "real"
                                | "time"
                                | "string"
                                | "chandle"
                                | "signed"
                                | "unsigned"
                        ) {
                            return Expr::Lit(Value::from_u64(1, 0));
                        }
                        self.err(e.span(), format!("unknown identifier '{}'", base));
                        Expr::Lit(Value::x(1))
                    }
                }
            }
            SyntaxKind::ParenExpr => {
                // Grouping only — lower the inner expression.
                let inner = real_children(e)
                    .into_iter()
                    .find(|k| is_expr_node(k.kind()));
                match inner {
                    Some(k) => self.lower_expr(&k, env),
                    None => Expr::Lit(Value::x(1)),
                }
            }
            SyntaxKind::Cast => {
                // `signed'(e)` / `unsigned'(e)` / `N'(e)` / `Type'(e)`: lower the
                // operand; a signed cast keeps the signedness hint, a type cast
                // resizes to the typedef's width when known.
                let kids = real_children(e);
                let target = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident || k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k));
                let is_signed = target.as_deref() == Some("signed");
                let inner = kids
                    .iter()
                    .rev()
                    .find(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, env))
                    .unwrap_or_else(|| Expr::Lit(Value::x(1)));
                if is_signed {
                    return Expr::Signed(Box::new(inner));
                }
                if let Some(t) = target {
                    if let Some(td) = self.types.get(&t.to_ascii_lowercase()).cloned() {
                        if td.width > 0 && !td.real {
                            return Expr::Resize {
                                e: Box::new(inner),
                                width: td.width,
                                signed: td.signed,
                            };
                        }
                    }
                }
                inner
            }
            SyntaxKind::BinExpr => {
                let kids = real_children(e);
                // `lhs inside { a, b, [lo:hi], ... }` — lower to an OR chain of
                // equality / range comparisons.
                if kids
                    .iter()
                    .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "inside")
                {
                    return self.lower_inside(&kids, env);
                }
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
                // `x++` / `x--` (postfix) and `++x` / `--x` (prefix) become
                // `x+1` / `x-1` (the assignment side effect is modeled by the
                // enclosing statement).
                if op_text == "++" || op_text == "--" {
                    let Some(o) = operand else {
                        return Expr::Lit(Value::x(1));
                    };
                    let base = self.lower_expr(o, env);
                    return Expr::Bin {
                        op: if op_text == "++" {
                            BinOp::Add
                        } else {
                            BinOp::Sub
                        },
                        l: Box::new(base),
                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                    };
                }
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
                // Method call on a class object: `c.get()` → `$method::C::get(c, ...)`.
                if let Some(dot) = name.rfind('.') {
                    let (base, method) = (&name[..dot], &name[dot + 1..]);
                    if let Some(id) = self.sig_id(base) {
                        if let Some(tn) = self.sig_types.get(&id).cloned() {
                            if let Some(td) = self.types.get(&tn).cloned() {
                                if let TypeKind::Class(cn) = &td.kind {
                                    let mut args = vec![Expr::Sig(id)];
                                    args.extend(
                                        kids[1..]
                                            .iter()
                                            .filter(|k| is_expr_node(k.kind()))
                                            .map(|k| self.lower_expr(k, env)),
                                    );
                                    if method == "randomize" {
                                        let spec = self.randomize_spec(cn);
                                        args.push(Expr::Lit(pack_str(&spec)));
                                        return Expr::Call {
                                            name: self.randomize_name(cn),
                                            args,
                                            shapes: Vec::new(),
                                        };
                                    }
                                    return Expr::Call {
                                        name: format!("$method::{}::{}", cn, method),
                                        args,
                                        shapes: Vec::new(),
                                    };
                                }
                            }
                        }
                    }
                }
                // `q.size()` on an array → size query with the array as arg.
                if let Some(dot) = name.rfind('.') {
                    let (base, method) = (&name[..dot], &name[dot + 1..]);
                    if matches!(method, "size" | "num") {
                        if let Some(id) = self.sig_id(base) {
                            if self.sig_is_mem.contains(&id) {
                                return Expr::Call {
                                    name: "$array_size".to_string(),
                                    args: vec![Expr::Sig(id)],
                                    shapes: Vec::new(),
                                };
                            }
                        }
                    }
                }
                let args = kids[1..]
                    .iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, env))
                    .collect();
                Expr::Call {
                    name,
                    args,
                    shapes: Vec::new(),
                }
            }
            SyntaxKind::Concat => Expr::Concat(
                real_children(e)
                    .into_iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(&k, env))
                    .collect(),
            ),
            // Assignment pattern `'{ a: x, b: y }`: lowered contextually in
            // assignments (struct field order); here, positional concat.
            // `Field:` label names are children too — skip them.
            SyntaxKind::Pattern => {
                let kids = real_children(e);
                let mut parts = Vec::new();
                for (i, k) in kids.iter().enumerate() {
                    if k.kind() == SyntaxKind::Name
                        && kids
                            .get(i + 1)
                            .is_some_and(|n| n.kind() == SyntaxKind::Punct && self.text(n) == ":")
                    {
                        continue;
                    }
                    if is_expr_node(k.kind()) {
                        parts.push(self.lower_expr(k, env));
                    }
                }
                Expr::Concat(parts)
            }
            // `new[n]` for dynamic arrays: handled in assignment context; as a
            // bare expression it is an error.
            SyntaxKind::Allocator => {
                self.err(
                    e.span(),
                    "`new` is only supported as a constructor call or array allocation in an assignment",
                );
                Expr::Lit(Value::x(32))
            }
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
            SyntaxKind::IndexedPartSelect => {
                // `base +: width` = `base[width-1 + base : base]`;
                // `base -: width` = `base[base : base - (width-1)]`.
                let kids = real_children(e);
                let exprs: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
                if exprs.len() == 2 {
                    let plus = e.children().any(|c| {
                        c.kind() == SyntaxKind::Punct && self.db.span_text(c.span()) == "+"
                    });
                    let one = Expr::Lit(parse_int_literal("1").unwrap_or(Value::x(32)));
                    let base = self.lower_expr(exprs[0], env);
                    let width = self.lower_expr(exprs[1], env);
                    let span = Expr::Bin {
                        op: BinOp::Sub,
                        l: Box::new(width),
                        r: Box::new(one),
                    };
                    if plus {
                        Expr::Sel {
                            e: Box::new(base.clone()),
                            msb: Box::new(Expr::Bin {
                                op: BinOp::Add,
                                l: Box::new(base.clone()),
                                r: Box::new(span),
                            }),
                            lsb: Box::new(base),
                        }
                    } else {
                        Expr::Sel {
                            e: Box::new(base.clone()),
                            msb: Box::new(base.clone()),
                            lsb: Box::new(Expr::Bin {
                                op: BinOp::Sub,
                                l: Box::new(base),
                                r: Box::new(span),
                            }),
                        }
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
            // String literal: first char in the high byte (MSB-first), matching
            // Value::concat part order and string_of's MSB-first reading.
            let inner = t.trim_matches('"');
            let bytes = inner.as_bytes();
            let n = bytes.len().max(1);
            let mut bits = vec![V0; n * 8];
            for (bi, &b) in bytes.iter().enumerate() {
                for j in 0..8 {
                    bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
                }
            }
            return Expr::Lit(Value { bits, real: false });
        }
        if let Some(f) = parse_real_literal(&t) {
            return Expr::Real(f);
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
        "**" => BinOp::Pow,
        "<<" | "<<<" => BinOp::Shl,
        ">>" => BinOp::Shr,
        ">>>" => BinOp::Sar,
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

/// Bit width implied by a Verilog type keyword, if any.
/// Pack a string as a Value (first char in the high byte), matching the
/// kernel's string-literal packing.
fn pack_str(s: &str) -> Value {
    let bytes = s.as_bytes();
    let n = bytes.len().max(1);
    let mut bits = vec![V0; n * 8];
    for (bi, &b) in bytes.iter().enumerate() {
        for j in 0..8 {
            bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
        }
    }
    Value { bits, real: false }
}

fn type_kw_width(t: &str) -> Option<usize> {
    Some(match t {
        "integer" | "time" | "int" => 32,
        "longint" => 64,
        "shortint" => 16,
        "byte" => 8,
        "real" => 64,
        "bit" | "logic" | "reg" | "wire" => 1,
        _ => return None,
    })
}

/// Parse a Verilog integer literal (`42`, `8'hFF`, `'b1010`, `4'd3`) into a
/// Value with per-bit X/Z for x/z/? digits. Returns None for non-integers.
/// Parse a Verilog real literal (`1.5`, `1e3`, `.5`, `2.0E-2`). Base-specified
/// literals (`8'hFF`) and plain integers are not real literals.
pub fn parse_real_literal(t: &str) -> Option<f64> {
    let t = t.trim();
    if t.contains('\'') {
        return None;
    }
    if !(t.contains('.') || t.contains('e') || t.contains('E')) {
        return None;
    }
    let c: String = t.chars().filter(|&c| c != '_').collect();
    c.parse::<f64>().ok()
}

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
    // Verilog unsized literals (plain decimal or `'b...` without a size) are
    // at least 32 bits wide; sized literals keep their declared size.
    let width = size.map(|s| s as usize).unwrap_or(32);
    if !digits
        .chars()
        .any(|c| matches!(c.to_ascii_lowercase(), 'x' | 'z' | '?'))
    {
        let v = u128::from_str_radix(&digits, base).ok()?;
        let mut bits = vec![V0; width.max(1)];
        for (i, b) in bits.iter_mut().enumerate().take(width.min(128)) {
            *b = if (v >> i) & 1 == 1 { V1 } else { V0 };
        }
        return Some(Value { bits, real: false });
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
    Some(Value { bits, real: false })
}

impl<'a> Lower<'a> {
    /// Const-evaluate with the global function table (package functions) so
    /// param defaults like `cc_pkg::idx_width(Depth)` resolve.
    fn ce(&self, e: &Expr, env: &HashMap<String, Value>) -> Option<Value> {
        const_eval_f(e, env, &self.funcs_all)
    }
}

/// Constant-evaluate an expression with an environment of named constants.
pub fn const_eval(e: &Expr, env: &HashMap<String, Value>) -> Option<Value> {
    let no_funcs: HashMap<String, Vec<FuncIr>> = HashMap::new();
    let no_fallback: HashMap<String, Vec<std::rc::Rc<FuncIr>>> = HashMap::new();
    ce_impl(e, env, &no_funcs, &no_fallback, None)
}

/// Like [`const_eval`], but user function bodies (`funcs`, e.g. VHDL package
/// functions visible through `use`) are const-evaluated for `Expr::Call` —
/// needed for constant initializers like `constant c : natural := f(1, 2);`.
pub fn const_eval_f(
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
) -> Option<Value> {
    let no_fallback: HashMap<String, Vec<std::rc::Rc<FuncIr>>> = HashMap::new();
    ce_impl(e, env, funcs, &no_fallback, None)
}

/// Like [`const_eval_f`], but a call no visible function (`funcs`) provides
/// falls back to `pkg_funcs` — the function bodies of all known packages.
/// VHDL visibility limits the directly merged table (`use p.f;` merges only
/// `f`), yet `f`'s body may legally call a package-internal sibling (visible
/// at the body's declaration site), so nested const-eval needs the wider
/// table to avoid poisoning the whole constant chain with X.
pub fn const_eval_f_with_pkgs(
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    pkg_funcs: &HashMap<String, Vec<std::rc::Rc<FuncIr>>>,
) -> Option<Value> {
    ce_impl(e, env, funcs, pkg_funcs, None)
}

thread_local! {
    /// Const-eval call depth: recursive or mutually recursive functions
    /// (e.g. OSVVM's MetaMatch) must abort instead of overflowing the stack.
    static CE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

const CE_MAX_DEPTH: u32 = 64;

/// Whether a const-evaluated expression is real-typed (mirrors the
/// simulator's `expr_real`): literals and arithmetic propagate realness.
fn ce_real(e: &Expr) -> bool {
    match e {
        Expr::Real(_) => true,
        Expr::Bin {
            op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod,
            l,
            r,
        } => ce_real(l) || ce_real(r),
        Expr::Bin { .. } => false,
        Expr::Un {
            op: UnOp::Neg | UnOp::Pos,
            e,
        } => ce_real(e),
        Expr::Un { .. } => false,
        Expr::Resize { e, .. } | Expr::Signed(e) => ce_real(e),
        Expr::Cond { c, t, e } => ce_real(c) || ce_real(t) || ce_real(e),
        _ => false,
    }
}

/// Const-eval one operand of a real-typed binop as f64.
fn ce_f64(v: &Value) -> Option<f64> {
    v.to_f64().or_else(|| v.to_u64().map(|n| n as f64))
}

/// `Some(slots)` while interpreting a function body: `Expr::Sig(i)` then
/// reads slot `i` (slot 0 = return value, then args, then locals) instead of
/// failing.
fn ce_impl(
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<std::rc::Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    match e {
        Expr::Rising(_) | Expr::Falling(_) | Expr::Prev(_) => None,
        // `to_integer(x)` in a constant initializer.
        Expr::ToInt(e) => {
            let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
            // Realness is a value property for slot reads: a formal/variable
            // slot holding a real (f64 bits, `real: true`) must round as a
            // number, not truncate its bit pattern to an integer (a w64 real
            // read via `to_u64` yields e.g. 0x1E1E1E1E for 1/17 — the low
            // half of the f64 bits).
            if ce_real(e) || v.real {
                let f = ce_f64(&v)?;
                return Some(Value::from_u64(32, f.round() as u64));
            }
            v.to_u64().map(|n| Value::from_u64(32, n))
        }
        // `T'image(x)` in a constant initializer (e.g. integer'image(42)).
        Expr::ToString(e) => {
            let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
            let s = if ce_real(e) || v.real {
                ce_f64(&v).map(|f| format!("{}", f))?
            } else {
                v.to_u64().map(|n| n.to_string())?
            };
            Some(crate::vhdl_lower::string_value(&s))
        }
        Expr::Lit(v) => Some(v.clone()),
        Expr::Real(x) => Some(Value::from_f64(*x)),
        Expr::Sig(i) => match slots {
            Some(s) => {
                while s.len() <= *i as usize {
                    s.push(Value::x(32));
                }
                Some(s[*i as usize].clone())
            }
            None => None,
        },
        Expr::Bin { op, l, r } => {
            let lv = ce_impl(l, env, funcs, extras, slots.as_deref_mut())?;
            let rv = ce_impl(r, env, funcs, extras, slots.as_deref_mut())?;
            // Realness is a value property for slot reads (see ToInt): a
            // `x * y` of two slot-held reals must multiply numbers, not the
            // f64 bit patterns.
            if ce_real(l) || ce_real(r) || lv.real || rv.real {
                let a = ce_f64(&lv);
                let b = ce_f64(&rv);
                match (a, b) {
                    (Some(x), Some(y)) => Some(match op {
                        BinOp::Add => Value::from_f64(x + y),
                        BinOp::Sub => Value::from_f64(x - y),
                        BinOp::Mul => Value::from_f64(x * y),
                        BinOp::Div => Value::from_f64(x / y),
                        BinOp::Mod => Value::from_f64(x % y),
                        BinOp::Pow => Value::from_f64(x.powf(y)),
                        BinOp::Lt => Value::from_u64(1, (x < y) as u64),
                        BinOp::Le => Value::from_u64(1, (x <= y) as u64),
                        BinOp::Gt => Value::from_u64(1, (x > y) as u64),
                        BinOp::Ge => Value::from_u64(1, (x >= y) as u64),
                        BinOp::Eq => Value::from_u64(1, (x == y) as u64),
                        BinOp::Ne => Value::from_u64(1, (x != y) as u64),
                        _ => return None,
                    }),
                    _ => None,
                }
            } else {
                // Const-eval convention (same as the BMC encoding): div/mod by
                // zero yields 0, so a default like `depth = N/width` with
                // `width=0` still folds instead of failing the range check.
                if matches!(op, BinOp::Div | BinOp::Mod) && rv.to_u64() == Some(0) {
                    return Some(Value::from_u64(1, 0));
                }
                Some(lv.binop(*op, &rv))
            }
        }
        Expr::Un { op, e } => {
            let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
            if ce_real(e) || v.real {
                return match (op, ce_f64(&v)) {
                    (UnOp::Neg, Some(x)) => Some(Value::from_f64(-x)),
                    (UnOp::Pos, Some(x)) => Some(Value::from_f64(x)),
                    (UnOp::Abs, Some(x)) => Some(Value::from_f64(x.abs())),
                    _ => None,
                };
            }
            Some(v.unop(*op))
        }
        Expr::Cond { c, t, e } => {
            if ce_impl(c, env, funcs, extras, slots.as_deref_mut())?.to_u64()? != 0 {
                ce_impl(t, env, funcs, extras, slots.as_deref_mut())
            } else {
                ce_impl(e, env, funcs, extras, slots.as_deref_mut())
            }
        }
        Expr::Concat(parts) => {
            let mut vals = Vec::with_capacity(parts.len());
            for p in parts {
                vals.push(ce_impl(p, env, funcs, extras, slots.as_deref_mut())?);
            }
            Some(Value::concat(&vals))
        }
        Expr::Replicate { count, e } => {
            let n = ce_impl(count, env, funcs, extras, slots.as_deref_mut())?.to_u64()? as usize;
            // A replication count beyond a few MB of bits is a misfolded
            // count, never a constant — abort the fold.
            if n > (1 << 21) {
                return None;
            }
            let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
            Some(Value::concat(&vec![v; n]))
        }
        Expr::Sel { e, msb, lsb } => {
            let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
            let msb = ce_impl(msb, env, funcs, extras, slots.as_deref_mut())?.to_u64()? as usize;
            let lsb = ce_impl(lsb, env, funcs, extras, slots.as_deref_mut())?.to_u64()? as usize;
            Some(v.slice(msb, lsb))
        }
        Expr::Idx { e, i } => {
            let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
            let i = ce_impl(i, env, funcs, extras, slots.as_deref_mut())?.to_u64()? as usize;
            Some(Value::from_bit(v.bit(i)))
        }
        Expr::Resize { e, width, signed } => {
            let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
            if *signed && v.width() < *width {
                let mut bits = vec![if v.bit(v.width() - 1) == V1 { V1 } else { V0 }; *width];
                bits[..v.width()].copy_from_slice(&v.bits);
                Some(Value { bits, real: false })
            } else {
                Some(v.resize(*width))
            }
        }
        Expr::Signed(e) => ce_impl(e, env, funcs, extras, slots.as_deref_mut()),
        Expr::SigAttr { sig, attr } => {
            // Function-local slots share the id space with flat signals; a
            // formal's `sig'length` / `'range` inside a const-evaluated
            // function body reads the bound value's width (same rule as the
            // runtime kernel). Signals proper stay unevaluated here — the
            // runtime resolves them after elaboration.
            let s = *sig as usize;
            let w = slots
                .as_deref()
                .and_then(|l| l.get(s))
                .map(|v| v.width().max(1))?;
            let v = match *attr {
                "length" => w,
                "high" | "left" => w.saturating_sub(1),
                _ => 0, // low | right
            };
            Some(Value::from_u64(32, v as u64))
        }
        Expr::Call { name, args, shapes } => {
            // System functions usable in constant expressions.
            // lower_expr strips the `$` from system tasks, so both spellings.
            if (name == "$clog2" || name == "clog2") && args.len() == 1 {
                let v = ce_impl(&args[0], env, funcs, extras, slots.as_deref_mut())?;
                let n = v.to_u64()?;
                return Some(Value::from_u64(32, (64 - n.leading_zeros()).max(1) as u64));
            }
            // `$const_idx(val, i, w)`: element `i` (MSB-first) of a flat array
            // value with element width `w` — array-constant indexing
            // (`C_VERSION(0)`, `const_arr(i).field`) and record-internal array
            // reads lower to this call. Same semantics as the simulator's
            // eval_call arm.
            if name == "$const_idx" && args.len() == 3 {
                let v = ce_impl(&args[0], env, funcs, extras, slots.as_deref_mut())?;
                let a =
                    ce_impl(&args[1], env, funcs, extras, slots.as_deref_mut())?.to_u64()? as usize;
                let w = ce_impl(&args[2], env, funcs, extras, slots.as_deref_mut())?
                    .to_u64()?
                    .max(1) as usize;
                let total = v.width();
                return Some(if total >= (a + 1) * w && a * w < total {
                    v.slice(total - 1 - a * w, total - (a + 1) * w)
                } else {
                    Value::x(w)
                });
            }
            // VHDL `type'val(x)` / `type'pos(x)` — numeric identity (the 'val
            // result width is applied by a Resize at the lower site).
            if (name == "$val" || name == "$pos") && args.len() == 1 {
                return ce_impl(&args[0], env, funcs, extras, slots.as_deref_mut());
            }
            // Overload resolution: same-name candidates may exist (VHDL
            // overloaded subprograms, e.g. `elog2(integer)` vs
            // `elog2(std_ulogic_vector)`). Prefer the candidate whose
            // formal count matches, then the one whose formal widths fit
            // the actual argument widths (exact match, or an
            // unconstrained/unknown formal width of 0 or 1). Without a
            // fitting candidate, the first same-arity one runs — matching
            // the simulator's runtime pick.
            // Interface-only stubs must never win a width tie against a real
            // body (a stub const-evals to X, which loops on forever — see
            // the matching ordering in the simulator's dispatch).
            // Directly visible candidates first; the design-wide package
            // fallback only fills a total miss — either/or, never merged.
            let is_stub_ir = |f: &FuncIr| match &f.body {
                Stmt::Null => true,
                Stmt::Block(ss) => ss.is_empty(),
                _ => false,
            };
            // Directly visible candidates first; the design-wide package
            // fallback only fills a total miss — either/or, never merged.
            let cands: Vec<&FuncIr> = match funcs.get(&name.to_ascii_lowercase()) {
                Some(c) => {
                    let mut v: Vec<&FuncIr> = c.iter().filter(|f| !is_stub_ir(f)).collect();
                    v.extend(c.iter().filter(|f| is_stub_ir(f)));
                    v
                }
                None => {
                    let c = extras.get(&name.to_ascii_lowercase())?;
                    let mut v: Vec<&FuncIr> = c
                        .iter()
                        .map(std::rc::Rc::as_ref)
                        .filter(|f| !is_stub_ir(f))
                        .collect();
                    v.extend(c.iter().map(std::rc::Rc::as_ref).filter(|f| is_stub_ir(f)));
                    v
                }
            };
            let mut argv = Vec::with_capacity(args.len());
            for a in args {
                argv.push(ce_impl(a, env, funcs, extras, slots.as_deref_mut())?);
            }
            // A call may omit trailing actuals whose formals carry interface
            // defaults (`w : natural := 8`); the real body qualifies, the
            // interface-only spec stub does not.
            let fits_defaults = |f: &FuncIr, n: usize| {
                f.args.len() > n
                    && f.defaults
                        .get(n..f.args.len())
                        .is_some_and(|t| t.iter().all(|d| d.is_some()))
                    && !matches!(&f.body, Stmt::Block(ss) if ss.is_empty())
                    && !matches!(f.body, Stmt::Null)
            };
            // Width fit is checked across ALL fitting candidates —
            // exact-arity and defaults-fit alike — exact widths before
            // wildcards: an unconstrained (w <= 1) formal must not absorb a
            // call an exact-width overload can take (a real-typed actual,
            // w64, must reach the real overload, not a same-name
            // std_ulogic_vector overload registered first).
            let width_ok = |f: &FuncIr, wild: bool| {
                f.args
                    .iter()
                    .zip(argv.iter())
                    .all(|((_, w, _), v)| *w == v.width() || (wild && *w <= 1))
            };
            let viable = |f: &FuncIr| f.args.len() == argv.len() || fits_defaults(f, argv.len());
            let f = cands
                .iter()
                .find(|f| viable(f) && width_ok(f, false))
                .or_else(|| cands.iter().find(|f| viable(f) && width_ok(f, true)))
                // No candidate matched every actual width: score the rest by
                // formal fit, best first — a `(record_t, boolean, natural, …)`
                // overload stays ahead of a `(natural, string, string, …)`
                // namesake when a record actual arrives, though neither
                // matched exactly (a scalar actual folds to a width the
                // formal does not declare). A same-arity interface stub must
                // never win this scored fit against a real body: its formals
                // carry fallback widths and its body const-evals to X, so a
                // real-body call whose actual is nearer the stub's fallback
                // width would fold the constant to X (`expand("abc", 5)`
                // scored the w32 stub over the w8 real `string` formal). When
                // any real body is viable, stubs drop out of the pool; an
                // unelaborated interface (stubs only) keeps the scored pick.
                .or_else(|| {
                    let has_real = cands.iter().any(|f| viable(f) && !is_stub_ir(f));
                    cands
                        .iter()
                        .filter(|f| viable(f) && (has_real == !is_stub_ir(f)))
                        .filter_map(|f| ce_overload_fit(f, &argv).map(|c| (c, f)))
                        .min_by_key(|(c, _)| *c)
                        .map(|(_, f)| f)
                })
                .or_else(|| cands.iter().find(|f| f.args.len() == argv.len()))
                .or_else(|| cands.iter().find(|f| fits_defaults(f, argv.len())))
                .or_else(|| cands.first())?;
            // Fill the omitted actuals from the recorded defaults.
            for (j, d) in f.defaults.iter().enumerate().skip(argv.len()) {
                match d {
                    Some(v) => {
                        let w = f.args.get(j).map(|a| a.1).unwrap_or(v.width()).max(1);
                        argv.push(v.clone().resize(w));
                    }
                    None => break,
                }
            }
            let depth = CE_DEPTH.with(|d| d.get());
            if depth >= CE_MAX_DEPTH {
                return None; // recursive const-eval: not constant
            }
            CE_DEPTH.with(|d| d.set(depth + 1));
            // Hidden shape binds fold against the caller's frame, filtered to
            // the picked candidate's shape-bound formals.
            let binds = ce_proc::eval_hidden_binds(&f.hidden, shapes, env, funcs, extras, slots)?;
            let r = const_eval_func(f, &argv, &binds, env, funcs, extras);
            CE_DEPTH.with(|d| d.set(depth));
            if super::debug::TRACE_CE.load(std::sync::atomic::Ordering::Relaxed)
                && (name.starts_with("get_")
                    || super::debug::TRACE_CE_ALL.load(std::sync::atomic::Ordering::Relaxed))
            {
                eprintln!(
                    "ce {name} arity={} argvw {:?} argv {:?} -> {:?} pickedw {:?} body {:.80?}",
                    f.args.len(),
                    argv.iter().map(|v| v.width()).collect::<Vec<_>>(),
                    argv.iter().map(|v| v.to_u64()).collect::<Vec<_>>(),
                    r.as_ref().map(|v| (v.width(), v.to_u64())),
                    f.args.iter().map(|a| a.1).collect::<Vec<_>>(),
                    f.body,
                );
            }
            r
        }
    }
}

/// Control flow out of a const-evaluated function body.
#[derive(PartialEq, Debug)]
enum CeFlow {
    Continue,
    Return,
    Abort,
    /// `break` / VHDL `exit`: ends the innermost enclosing loop; consumed by
    /// the loop, which resumes normal flow after itself.
    LoopBreak,
    /// `continue` / VHDL `next`: skips to the loop's step; consumed by the
    /// loop, which then runs the step and the next iteration.
    LoopNext,
}

/// Const-evaluate a user function body: slot 0 is the return value, then
/// args, then locals. Supports the statement subset that appears in pure
/// package functions (block/if/case/assign/for/return).
/// Score how well an overload's formals fit a call's actuals when no
/// candidate matched every width exactly (see the `Call` arm): lower fits
/// better, `None` disqualifies. A declared-numeric formal can never take a
/// multi-word array/record actual — widths beyond 64 bits are arrays or
/// records, not scalars — and other formals match by width distance, which
/// keeps a `(record_t, boolean, natural, …)` overload ahead of a
/// `(natural, string, string, …)` namesake when a record actual arrives.
/// Viability (exact arity or trailing defaults) is checked by the caller.
fn ce_overload_fit(f: &FuncIr, argv: &[Value]) -> Option<u64> {
    let mut cost = 0u64;
    for ((_, w, t), v) in f.args.iter().zip(argv.iter()) {
        let aw = v.width() as u64;
        if *w as u64 == aw {
            continue;
        }
        if matches!(
            t.as_str(),
            "natural" | "integer" | "positive" | "boolean" | "bit" | "character" | "real" | "time",
        ) {
            // A scalar formal cannot bind an array/record actual.
            if aw > 64 {
                return None;
            }
            cost += 1;
        } else {
            cost += aw.abs_diff(*w as u64);
        }
    }
    Some(cost)
}

fn const_eval_func(
    f: &FuncIr,
    argv: &[Value],
    binds: &[(usize, Value, Value)],
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<std::rc::Rc<FuncIr>>>,
) -> Option<Value> {
    let mut slots: Vec<Value> = vec![Value::x(f.ret_width.max(1))];
    slots.extend(argv.iter().cloned());
    // Locals ride in slot order; the hidden shape slots are locals entries
    // (32-bit, init-less), so extend first and then overwrite the two slots
    // of each shape-bound formal from the call's binds.
    slots.extend(
        f.locals
            .iter()
            .map(|l| l.init.clone().unwrap_or_else(|| Value::x(l.width.max(1)))),
    );
    ce_proc::fill_hidden_slots(&f.hidden, binds, &mut slots);
    // Locals with formal-dependent constraints (`string(1 to size)`)
    // registered with a fallback width: re-fold the recorded bit-width
    // expression now that the actuals are in the slots (shared with the
    // procedure dispatcher). Unfoldable — the body will fold to X on use.
    if !ce_proc::fold_dyn_locals(&f.dyn_locals, &mut slots, env, funcs, extras) {
        return None;
    }
    let mut flow = CeFlow::Continue;
    ce_stmt(&f.body, &mut slots, env, funcs, extras, &mut flow);
    if flow == CeFlow::Abort
        // A `break`/`continue` that outlives every loop has no value.
        || flow == CeFlow::LoopBreak
        || flow == CeFlow::LoopNext
    {
        return None;
    }
    Some(slots.into_iter().next().unwrap_or(Value::x(1)))
}

fn ce_stmt(
    s: &Stmt,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<std::rc::Rc<FuncIr>>>,
    flow: &mut CeFlow,
) {
    if *flow != CeFlow::Continue {
        return;
    }
    let ev = |e: &Expr, slots: &mut Vec<Value>| ce_impl(e, env, funcs, extras, Some(slots));
    match s {
        Stmt::Null => {}
        Stmt::Spanned { inner, .. } => ce_stmt(inner, slots, env, funcs, extras, flow),
        Stmt::Block(ss) => {
            for x in ss {
                ce_stmt(x, slots, env, funcs, extras, flow);
                if *flow != CeFlow::Continue {
                    return;
                }
            }
        }
        Stmt::If { cond, then, els } => match ev(cond, slots).and_then(|v| v.to_u64()) {
            // An unknown condition is not TRUE: VHDL runs the sequence only
            // on TRUE (LRM 1076 §10.8) and Verilog takes the else branch on
            // x/z (IEEE 1800 §4.1) — the runtime behaves the same, and the
            // loop arms already terminate on an unknown condition, so
            // const-eval mirrors that instead of aborting the fold.
            Some(0) | None => {
                if let Some(els) = els {
                    ce_stmt(els, slots, env, funcs, extras, flow);
                }
            }
            Some(_) => ce_stmt(then, slots, env, funcs, extras, flow),
        },
        Stmt::Assign { lhs, rhs, .. } => {
            let v = ev(rhs, slots);
            match (v, lhs) {
                (Some(v), Expr::Sig(i)) => {
                    while slots.len() <= *i as usize {
                        slots.push(Value::x(32));
                    }
                    slots[*i as usize] = v;
                }
                // `v(i) := x` / `v(hi downto lo) := x` on a local slot: splice
                // the rhs bits in at the index/slice offset, growing the slot
                // with X as needed (mirrors ce_impl's bit-index read of
                // Expr::Idx and msb/lsb read of Expr::Sel).
                (Some(v), Expr::Idx { e, i }) => {
                    let spliced = (|| {
                        let idx = ev(i, slots)?.to_u64()? as usize;
                        let Expr::Sig(s) = &**e else { return None };
                        let s = *s as usize;
                        while slots.len() <= s {
                            slots.push(Value::x(32));
                        }
                        let w = v.width().max(1);
                        // A bit index near 2^32 is a folded-negative index
                        // (`v(i - 1)` with i = 0) read as u64 — abort the
                        // fold instead of growing the slot by gigabytes.
                        if idx + w > (1 << 24) {
                            return None::<()>;
                        }
                        if slots[s].bits.len() < idx + w {
                            let mut bits = slots[s].bits.clone();
                            bits.resize(idx + w, VX);
                            slots[s] = Value { bits, real: false };
                        }
                        for b in 0..w {
                            slots[s].set_bit(idx + b, v.bit(b));
                        }
                        Some(())
                    })();
                    if spliced.is_none() {
                        *flow = CeFlow::Abort;
                    }
                }
                (Some(v), Expr::Sel { e, msb, lsb }) => {
                    let spliced = (|| {
                        let msb = ev(msb, slots)?.to_u64()? as usize;
                        let lsb = ev(lsb, slots)?.to_u64()? as usize;
                        // Walk Sel chains down to the base slot: a record
                        // field slice-assign (`result.description(1 TO n)`)
                        // is Sel-of-Sel; each inner field select's low bit
                        // offsets the splice target (record field offsets
                        // are static bit ranges).
                        let mut off = 0usize;
                        let mut node = &**e;
                        let s = loop {
                            match node {
                                Expr::Sig(s) => break *s as usize,
                                Expr::Sel {
                                    e: inner, msb, lsb, ..
                                } => {
                                    off += ev(lsb, slots)?.to_u64()? as usize;
                                    let _ = ev(msb, slots)?.to_u64()?;
                                    node = &**inner;
                                }
                                _ => return None::<()>,
                            }
                        };
                        while slots.len() <= s {
                            slots.push(Value::x(32));
                        }
                        let w = msb.saturating_sub(lsb) + 1;
                        // Guard the slot growth: a slice beyond a few MB is
                        // a misfolded index (a negative bound read as u64),
                        // never a real constant — abort the fold instead of
                        // gigabytes of X.
                        if w > (1 << 24) || lsb + off > (1 << 24) {
                            return None::<()>;
                        }
                        if slots[s].bits.len() < lsb + off + w {
                            let mut bits = slots[s].bits.clone();
                            bits.resize(lsb + off + w, VX);
                            slots[s] = Value { bits, real: false };
                        }
                        for b in 0..w {
                            slots[s].set_bit(lsb + off + b, v.bit(b));
                        }
                        Some(())
                    })();
                    if spliced.is_none() {
                        *flow = CeFlow::Abort;
                    }
                }
                _ => *flow = CeFlow::Abort,
            }
        }
        Stmt::Return(e) => match ev(e, slots) {
            Some(v) => {
                slots[0] = v;
                *flow = CeFlow::Return;
            }
            None => *flow = CeFlow::Abort,
        },
        Stmt::Break => *flow = CeFlow::LoopBreak,
        Stmt::Continue => *flow = CeFlow::LoopNext,
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => {
            ce_stmt(init, slots, env, funcs, extras, flow);
            let mut guard = 0u32;
            loop {
                if *flow != CeFlow::Continue {
                    return;
                }
                match ev(cond, slots).and_then(|v| v.to_u64()) {
                    Some(n) if n != 0 => {
                        ce_stmt(body, slots, env, funcs, extras, flow);
                        match *flow {
                            // The innermost loop consumes break/continue so an
                            // enclosing loop (or the function epilogue) sees
                            // normal flow; Return/Abort still propagate.
                            CeFlow::LoopBreak => {
                                *flow = CeFlow::Continue;
                                return;
                            }
                            CeFlow::LoopNext => *flow = CeFlow::Continue,
                            CeFlow::Continue => {}
                            CeFlow::Return | CeFlow::Abort => return,
                        }
                        ce_stmt(step, slots, env, funcs, extras, flow);
                        guard += 1;
                        if guard > 100_000 {
                            *flow = CeFlow::Abort;
                            return;
                        }
                    }
                    _ => return,
                }
            }
        }
        Stmt::Case {
            expr,
            items,
            default,
            ..
        } => {
            let v = match ev(expr, slots) {
                Some(v) => v,
                None => {
                    *flow = CeFlow::Abort;
                    return;
                }
            };
            for (choices, body) in items {
                for ch in choices {
                    if ev(ch, slots) == Some(v.clone()) {
                        ce_stmt(body, slots, env, funcs, extras, flow);
                        return;
                    }
                }
            }
            if let Some(d) = default {
                ce_stmt(d, slots, env, funcs, extras, flow);
            }
        }
        Stmt::SysCall { name, args, shapes } => {
            // VHDL `report`/`assert` inside a constant initializer: a note or
            // warning only annotates elaboration, so const-eval continues past
            // it (a `log2` that warns on non-power-of-2 input must still
            // produce a value). Error/failure — and a failing `assert`, whose
            // default severity is error (LRM 1076 §10.8) — mean the
            // initializer would fail at elaboration, so it folds to X. Other
            // syscalls (file I/O, $display) have no constant meaning.
            let severity = |i: usize, slots: &mut Vec<Value>| {
                args.get(i)
                    .and_then(|a| ev(a, slots))
                    .map(|v| {
                        v.bits
                            .chunks(8)
                            .rev()
                            .filter_map(|c| {
                                let mut b = 0u8;
                                for (j, &bit) in c.iter().enumerate() {
                                    if bit == V1 {
                                        b |= 1 << j;
                                    }
                                }
                                (b != 0).then_some(b as char)
                            })
                            .collect::<String>()
                            .to_ascii_lowercase()
                    })
                    .unwrap_or_default()
            };
            match name.as_str() {
                "report" => {
                    if matches!(severity(1, slots).as_str(), "error" | "failure") {
                        *flow = CeFlow::Abort;
                    }
                }
                "assert" => {
                    let ok = args
                        .first()
                        .and_then(|a| ev(a, slots))
                        .and_then(|v| v.to_u64())
                        .map(|n| n != 0)
                        .unwrap_or(true);
                    if !ok && !matches!(severity(2, slots).as_str(), "note" | "warning") {
                        *flow = CeFlow::Abort;
                    }
                }
                _ => {
                    // A user procedure called as a statement: run its body
                    // on a fresh frame and copy inout/out formals back (see
                    // `ce_proc`). Builtin syscalls and unresolvable names
                    // have no constant meaning and abort the fold.
                    if !ce_proc::ce_call_procedure(name, args, shapes, slots, env, funcs, extras) {
                        *flow = CeFlow::Abort;
                    }
                }
            }
        }
        _ => *flow = CeFlow::Abort,
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
    fn two_modules_in_one_file_do_not_share_names() {
        // Regression: the per-module name map used to leak from the first
        // module into the second, so a same-named decl in module 2 was
        // skipped and later references failed with "unknown identifier".
        let src = "module a (input wire x); endmodule\n\
                   module b;\n    reg x;\n    initial begin x = 1'b0; end\nendmodule\n";
        let (modules, _db, diags) = lower_src(src);
        assert!(
            diags
                .iter()
                .all(|d| d.severity != hdl_syntax::diagnostic::Severity::Error),
            "{:?}",
            diags
        );
        assert_eq!(modules.len(), 2);
        let b = &modules[1];
        assert_eq!(b.name, "b");
        assert!(b.signals.iter().any(|s| s.name == "x"), "{:?}", b.signals);
    }

    #[test]
    fn lowers_sva_lite_assert_property_and_immediate_assert() {
        let src = "module m (input wire clk, input wire a, output reg b);\n\
                   always @(posedge clk) b <= a;\n\
                   assert property (@(posedge clk) a |-> b);\n\
                   assert property (@(posedge clk) a |=> b);\n\
                   initial begin assert (a == 1'b0); end\n\
                   endmodule\n";
        let (modules, _db, diags) = lower_src(src);
        assert!(
            diags
                .iter()
                .all(|d| d.severity != hdl_syntax::diagnostic::Severity::Error),
            "{:?}",
            diags
        );
        let m = &modules[0];
        // two clocked properties + one always + one initial
        assert_eq!(
            m.processes.len(),
            4,
            "{:?}",
            m.processes.iter().map(|p| &p.origin).collect::<Vec<_>>()
        );
        let origins: Vec<&str> = m.processes.iter().map(|p| p.origin.as_str()).collect();
        assert!(
            origins
                .iter()
                .filter(|o| o.starts_with("assert property"))
                .count()
                == 2,
            "{:?}",
            origins
        );
        // the immediate assert lowered to a sva_assert syscall
        let init = m.processes.iter().find(|p| p.origin == "initial").unwrap();
        let s = format!("{:?}", init.body);
        assert!(s.contains("sva_assert"), "{}", s);
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
    fn lowers_common_cells_constructs() {
        // The constructs used by pulp-platform/common_cells: package functions
        // called as `pkg::fn(...)` in param defaults, gen-block localparams,
        // multi-dim ranges with `**`, struct patterns with field labels,
        // `parameter type`, for-loop inline declarations, gen-for compound steps.
        let src = "package cc_pkg;\n\
                   \x20 function automatic int unsigned idx_width(input int unsigned num_idx);\n\
                   \x20   if (num_idx > 32'd1) begin return unsigned'($clog2(num_idx)); end\n\
                   \x20   return 32'd1;\n\
                   \x20 endfunction\n\
                   \x20 function automatic int unsigned cnt_width(input int unsigned max_cnt);\n\
                   \x20   return idx_width(max_cnt + 1);\n\
                   \x20 endfunction\n\
                   \x20 localparam int unsigned LZC = 1;\n\
                   endpackage\n\
                   module m #(parameter int unsigned Depth = 8, parameter type rule_t = logic [7:0])\n\
                   \x20 (input logic clk, output logic [7:0] y);\n\
                   \x20 localparam int unsigned W = cc_pkg::idx_width(Depth);\n\
                   \x20 localparam int unsigned U = cc_pkg::cnt_width(Depth);\n\
                   \x20 if (Depth > 1) begin : g\n\
                   \x20   localparam int unsigned Lvls = unsigned'($clog2(Depth)) + 1;\n\
                   \x20   logic [Lvls-1:0][2**Lvls-2:0] sum;\n\
                   \x20 end\n\
                   \x20 logic [W-1:0] ptr;\n\
                   \x20 logic [cc_pkg::LZC-1:0] x;\n\
                   \x20 typedef struct packed { int unsigned a; int unsigned b; } seed_t;\n\
                   \x20 localparam seed_t [1:0] seeds = '{'{a: 32'd1, b: 32'd2}, '{a: 32'd3, b: 32'd4}};\n\
                   \x20 always_comb begin\n\
                   \x20   ptr = 0;\n\
                   \x20   for (int i = 0; i < W; i++) begin\n\
                   \x20     ptr += x[i];\n\
                   \x20   end\n\
                   \x20 end\n\
                   \x20 for (genvar j = 0; j < U; j += 2) begin : gj\n\
                   \x20   assign y[j] = 1'b0;\n\
                   \x20 end\n\
                   endmodule\n";
        let (modules, _db, diags) = lower_src(src);
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let m = &modules[0];
        let ptr = m.signals.iter().find(|s| s.name == "ptr").unwrap();
        // W = idx_width(8) = $clog2(8) = 4 with the bits-needed formula.
        assert_eq!(ptr.width, 4);
    }

    #[test]
    fn parse_real_literals() {
        assert_eq!(parse_real_literal("1.5"), Some(1.5));
        assert_eq!(parse_real_literal("2.0E-2"), Some(0.02));
        assert_eq!(parse_real_literal("1e3"), Some(1000.0));
        assert_eq!(parse_real_literal(".5"), Some(0.5));
        assert_eq!(parse_real_literal("42"), None); // plain integer
        assert_eq!(parse_real_literal("8'hFF"), None); // based literal
        assert_eq!(parse_real_literal("8'h1E"), None); // 'E' inside based literal
        assert_eq!(parse_real_literal("1_000.5"), Some(1000.5));
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

    #[test]
    fn inside_operator_lowers_to_comparison_chain() {
        let src = "module m(input [2:0] x, output y); assign y = x inside {[3'b000:3'b010], 3'b111}; endmodule\n";
        let (_m, _db, diags) = lower_src(src);
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == hdl_syntax::diagnostic::Severity::Error)
            .collect();
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn const_eval_of_recursive_function_aborts_instead_of_overflowing() {
        // A self-recursive function is not constant; const-eval must give up
        // (depth guard) rather than recurse until the stack overflows.
        let src = "module m;\nfunction automatic integer f(input integer x);\n  f = f(x) + 1;\nendfunction\nlocalparam integer P = f(1);\nendmodule\n";
        let (_m, _db, _diags) = lower_src(src); // returns at all = pass
    }

    #[test]
    fn const_eval_function_loop_consumes_break_and_keeps_prior_value() {
        // `break` inside a const-evaluated loop ends the loop; the value
        // accumulated before the break is the function result. Used to
        // abort the whole function (X-poisoning the localparam).
        let src = "package ce_pkg;\n\
                   function automatic integer sum_until(input integer limit);\n\
                     integer i;\n\
                     begin\n\
                       sum_until = 0;\n\
                       for (i = 0; i < limit; i = i + 1) begin\n\
                         if (i == 3) break;\n\
                         sum_until = sum_until + 1;\n\
                       end\n\
                     end\n\
                   endfunction\n\
                   endpackage\n\
                   module m;\n\
                   localparam integer P = ce_pkg::sum_until(10);\n\
                   endmodule\n";
        let (modules, _db, diags) = lower_src(src);
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let p = modules[0]
            .params
            .iter()
            .find(|p| p.name == "P")
            .expect("localparam P lowered");
        assert_eq!(p.default.to_u64(), Some(3), "{:?}", p.default);
    }

    #[test]
    fn const_eval_function_loop_consumes_continue_and_runs_step() {
        // `continue` inside a const-evaluated loop skips to the step, so the
        // loop still advances. Used to abort the whole function.
        let src = "package ce_pkg;\n\
                   function automatic integer count_odd(input integer limit);\n\
                     integer i;\n\
                     begin\n\
                       count_odd = 0;\n\
                       for (i = 0; i < limit; i = i + 1) begin\n\
                         if (i % 2 == 0) continue;\n\
                         count_odd = count_odd + 1;\n\
                       end\n\
                     end\n\
                   endfunction\n\
                   endpackage\n\
                   module m;\n\
                   localparam integer P = ce_pkg::count_odd(10);\n\
                   endmodule\n";
        let (modules, _db, diags) = lower_src(src);
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let p = modules[0]
            .params
            .iter()
            .find(|p| p.name == "P")
            .expect("localparam P lowered");
        assert_eq!(p.default.to_u64(), Some(5), "{:?}", p.default);
    }
}
