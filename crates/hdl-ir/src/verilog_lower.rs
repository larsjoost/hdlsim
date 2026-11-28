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
use crate::value::{Value, V0, V1};
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
mod funcs;
mod generate;
mod inst;
mod items;
mod literals;
mod module;
mod pattern;
mod proc;
mod stmt;
mod stmt_assign;
mod stmt_loop;
mod stream;
mod types;

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
