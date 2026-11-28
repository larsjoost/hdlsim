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
use crate::value::{BinOp, Value, V0, V1};
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::{Diagnostic, Severity};
use hdl_syntax::kinds::SyntaxKind;
use hdl_syntax::source::{FileId, SourceDb, Span};
use std::collections::{HashMap, HashSet};

mod ce_call;
mod ce_func;
mod ce_proc;
mod const_eval;
mod expr;
mod expr_name;
mod literals;
mod pattern;
mod proc;
mod stmt;
mod stmt_assign;
mod stmt_loop;
mod stream;

pub use const_eval::{const_eval, const_eval_f, const_eval_f_with_pkgs};
pub use literals::parse_int_literal;
pub use stream::stream_reverse;
// Path stability for the sibling const-eval modules (`ce_proc.rs` imports
// the interpreter core from `super::`).
pub(crate) use ce_func::{ce_stmt, CeFlow};
pub(crate) use const_eval::{ce_impl, CE_DEPTH, CE_MAX_DEPTH};

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
        gen_path: Vec::new(),
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
            | SyntaxKind::StreamConcat
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
            | SyntaxKind::DeassignStmt
            | SyntaxKind::Assign
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
    /// Generate blocks currently being unrolled, innermost last: one element
    /// per enclosing for-generate, `label[index]` (genvar-name fallback when
    /// the block is unlabeled). Processes and instances lowered inside get it
    /// as an instance-path suffix, so two unrolled lanes are distinguishable
    /// in the transcript and in signal names.
    gen_path: Vec<String>,
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
            gen_scope: self.gen_path.join("."),
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
                                    gen_scope: self.gen_path.join("."),
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
        // Block label `begin : lane_gen` when the body names one; the genvar
        // names the scope otherwise. It prefixes instance labels and process
        // scopes inside (`lane_gen[0].u_lane`), so parallel lanes are
        // distinguishable in the transcript and in signal names.
        let gen_label = match &body {
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
                var.clone()
            }
        });
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
            self.gen_path.push(format!("{gen_label}[{i}]"));
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
            self.gen_path.pop();
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
impl<'a> Lower<'a> {
    /// Const-evaluate with the global function table (package functions) so
    /// param defaults like `cc_pkg::idx_width(Depth)` resolve.
    fn ce(&self, e: &Expr, env: &HashMap<String, Value>) -> Option<Value> {
        const_eval_f(e, env, &self.funcs_all)
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
