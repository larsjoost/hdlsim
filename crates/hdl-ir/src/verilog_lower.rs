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

/// Lower one parsed file, with `time_unit_fs` taken from the file's
/// `` `timescale `` directive (femtoseconds per time unit).
pub fn lower_file_with_time(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    time_unit_fs: u64,
) -> Vec<ModuleIr> {
    let _ = file; // spans on nodes already carry the file id
    let mut lx = Lower {
        db,
        diags,
        time_unit_fs,
        names: HashMap::new(),
        sig_types: HashMap::new(),
        types: HashMap::new(),
        gconsts: HashMap::new(),
        packages: HashMap::new(),
        extra_signals: Vec::new(),
        sig_is_mem: HashSet::new(),
        extra_base: 0,
        in_function: false,
    };
    let mut modules = Vec::new();
    for child in real_children(root) {
        match child.kind() {
            SyntaxKind::Module => modules.push(lx.lower_module(&child)),
            SyntaxKind::Package => {
                let pkg = lx.lower_module(&child);
                lx.packages.insert(pkg.name.clone(), pkg);
            }
            SyntaxKind::Class => modules.push(lx.lower_class(&child)),
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
            | SyntaxKind::CondExpr
            | SyntaxKind::Call
            | SyntaxKind::Concat
            | SyntaxKind::Replicate
            | SyntaxKind::PartSelect
            | SyntaxKind::Index
            | SyntaxKind::Pattern
            | SyntaxKind::Allocator
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
    /// Signals allocated during statement lowering (e.g. foreach index vars).
    extra_signals: Vec<SignalIr>,
    /// module.signals.len() after the registration pre-pass — the id base of
    /// `extra_signals`.
    extra_base: usize,
    /// Signal ids that are memories/arrays.
    sig_is_mem: HashSet<SigId>,
    /// Inside a function body: `return e;` assigns to slot 0 (function name).
    in_function: bool,
}

/// A user-defined type (typedef, enum, struct, class).
#[derive(Clone, Debug)]
struct TypeDef {
    width: usize,
    signed: bool,
    real: bool,
    kind: TypeKind,
}

#[derive(Clone, Debug)]
enum TypeKind {
    Plain,
    /// Enum labels in declaration order (label → value).
    #[allow(dead_code)]
    Enum(Vec<(String, u64)>),
    /// Packed struct fields in declaration order: (name, width, lsb offset).
    Struct(Vec<(String, usize, usize)>),
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
        // The name map is per-module: without this, a declaration in the second
        // module of a file would be skipped as "already known" from the first.
        self.names.clear();
        self.sig_types.clear();
        self.sig_is_mem.clear();
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
                            .map_or(false, |t| t.real);
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
                                        .map_or(false, |t| t.signed),
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
                    // Look for `= expr` right after this Name.
                    if let Some(pos) = ek.iter().position(|x| std::ptr::eq(x, k)) {
                        if let Some(e) = ek.get(pos + 1).filter(|e| is_expr_node(e.kind())) {
                            if let Some(cv) = const_eval(&self.lower_expr(e, env), env) {
                                if let Some(n) = cv.to_u64() {
                                    v = n;
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
                    if sk.get(i).map_or(false, |k| k.kind() == SyntaxKind::Range) {
                        width = self.range_width(&sk[i], env);
                        i += 1;
                    }
                }
                while i < sk.len() && sk[i].kind() == SyntaxKind::Name {
                    fields.push((self.text(&sk[i]), width));
                    i += 1;
                    if sk.get(i).map_or(false, |k| k.kind() == SyntaxKind::Range) {
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
            self.types.insert(
                key,
                TypeDef {
                    width: total.max(1),
                    signed,
                    real: false,
                    kind: TypeKind::Struct(with_off),
                },
            );
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
                if star {
                    self.gconsts
                        .insert(param.name.clone(), param.default.clone());
                } else if Some(param.name.as_str()) == item_name.copied() {
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

    /// Lower a top-level `class` — full class support lands with the
    /// class/object slice; for now record a stub so `C c;` decls parse.
    fn lower_class(&mut self, c: &SyntaxNode) -> ModuleIr {
        let kids = real_children(c);
        let name = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .unwrap_or_else(|| "<anon_class>".to_string());
        self.err(
            c.span(),
            format!("class '{}' is parsed but not yet supported", name),
        );
        ModuleIr {
            name,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Vec::new(),
            assigns: Vec::new(),
            processes: Vec::new(),
            functions: Vec::new(),
            instances: Vec::new(),
            time_unit_fs: self.time_unit_fs,
        }
    }

    /// Lower `'{ a: x, b: y, default: z }` against the struct type of the LHS:
    /// concat fields in declaration order (first field = MSB).
    fn lower_pattern(
        &mut self,
        pat: &SyntaxNode,
        lhs: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let pk = real_children(pat);
        // (label, expr) pairs in source order.
        let mut entries: Vec<(Option<String>, Expr)> = Vec::new();
        let mut i = 0;
        while i < pk.len() {
            let label = if pk[i].kind() == SyntaxKind::Name {
                let l = self.text(&pk[i]);
                i += 1;
                Some(l)
            } else {
                None
            };
            if i < pk.len() && is_expr_node(pk[i].kind()) {
                let e = self.lower_expr(&pk[i], env);
                entries.push((label, e));
                i += 1;
            } else {
                i += 1;
            }
        }
        // LHS struct type.
        let lhs_name = if lhs.kind() == SyntaxKind::Name {
            self.text(lhs)
        } else {
            return None;
        };
        let id = self.sig_id(&lhs_name)?;
        let tn = self.sig_types.get(&id)?.clone();
        let td = self.types.get(&tn)?.clone();
        let TypeKind::Struct(fields) = td.kind else {
            // Non-struct target: positional concat.
            return Some(Expr::Concat(entries.into_iter().map(|(_, e)| e).collect()));
        };
        let default = entries
            .iter()
            .find(|(l, _)| l.as_deref() == Some("default"))
            .map(|(_, e)| e.clone());
        let mut parts: Vec<Expr> = Vec::new();
        let mut pos = 0usize;
        for (fname, fw, _lsb) in &fields {
            let named = entries
                .iter()
                .find(|(l, _)| l.as_deref() == Some(fname.as_str()))
                .map(|(_, e)| e.clone());
            let e = named.or_else(|| {
                let p = entries
                    .iter()
                    .enumerate()
                    .find(|(pi, (l, _))| *pi >= pos && l.is_none());
                p.map(|(pi, (_, e))| {
                    pos = pi + 1;
                    e.clone()
                })
            });
            parts.push(match e {
                Some(e) => e,
                None => default.clone().unwrap_or_else(|| Expr::Lit(Value::x(*fw))),
            });
        }
        Some(Expr::Concat(parts))
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
        if let Some(r) = kids
            .iter()
            .enumerate()
            .find(|(i, k)| k.kind() == SyntaxKind::Range && name_pos.map_or(true, |np| *i < np))
            .map(|(_, k)| k)
        {
            return self.range_width(r, env);
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
            if let Some(v) = const_eval(&exprs[0], env).and_then(|v| v.to_u64()) {
                // `mem [255:0]` style single bound, or assoc `[string]` (non-const).
                return v as usize + 1;
            }
            return 0; // assoc array key type, e.g. `[string]`
        }
        if let (Some(a), Some(b)) = (const_eval(&exprs[0], env), const_eval(&exprs[1], env)) {
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
                // Already handled in the pre-pass / param pass; re-lowering a Decl
                // for its init value:
                if item.kind() == SyntaxKind::Decl {
                    self.lower_decl_init(module, item, env);
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
                // Optional memory dimension after the name: `reg [7:0] mem [0:255]`.
                if kids
                    .get(i + 1)
                    .map_or(false, |k| k.kind() == SyntaxKind::Range)
                {
                    let depth = self.range_depth(&kids[i + 1], env);
                    if let Some(id) = self.sig_id(&name) {
                        module.signals[id as usize].mem = Some(depth);
                    }
                    i += 2;
                    continue;
                }
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

    /// Lower one statement, wrapped in a `Stmt::Spanned` with its source span
    /// (drives line/branch coverage). Recursive calls go through this wrapper,
    /// so every nested statement is spanned too.
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
                                            };
                                        }
                                    }
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
                        if dk.get(i + 1).map_or(false, |k| self.text(k) == "=") {
                            if let Some(e) = dk.get(i + 2).filter(|e| is_expr_node(e.kind())) {
                                inits.push(Stmt::Assign {
                                    lhs: Expr::Sig(self.sig_id(&self.text(k)).unwrap()),
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
                    inits.pop().unwrap()
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
            // `return expr;` in a function: assign to the function-name slot 0.
            SyntaxKind::ReturnStmt if self.in_function => {
                let kids = real_children(s);
                let e = kids.iter().find(|k| is_expr_node(k.kind()));
                Stmt::Assign {
                    lhs: Expr::Sig(0),
                    rhs: e
                        .map(|e| self.lower_expr(e, env))
                        .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0))),
                    nba: false,
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
                let cond = kids
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .map(|c| self.lower_expr(c, env))
                    .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
                if stmts.len() >= 3 {
                    Stmt::For {
                        init: Box::new(self.lower_stmt(stmts[0], env)),
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

    fn lower_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        match e.kind() {
            SyntaxKind::Literal => self.lower_literal(e),
            SyntaxKind::Name => {
                let name = self.text(e);
                if let Some(v) = env.get(&name).or_else(|| self.gconsts.get(&name)) {
                    return Expr::Lit(v.clone());
                }
                // Struct field select: `p.a` where p is a packed-struct signal.
                if let Some(dot) = name.find('.') {
                    let root = &name[..dot];
                    if let Some(id) = self.sig_id(root) {
                        if let Some(tn) = self.sig_types.get(&id).cloned() {
                            if let Some(td) = self.types.get(&tn).cloned() {
                                if let TypeKind::Struct(fields) = &td.kind {
                                    let field = &name[dot + 1..];
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
                // `q.size()` on an array → size query with the array as arg.
                if let Some(dot) = name.rfind('.') {
                    let (base, method) = (&name[..dot], &name[dot + 1..]);
                    if matches!(method, "size" | "num") {
                        if let Some(id) = self.sig_id(base) {
                            if self.sig_is_mem.contains(&id) {
                                return Expr::Call {
                                    name: "$array_size".to_string(),
                                    args: vec![Expr::Sig(id)],
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
                Expr::Call { name, args }
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
            SyntaxKind::Pattern => Expr::Concat(
                real_children(e)
                    .into_iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(&k, env))
                    .collect(),
            ),
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
            return Expr::Lit(Value { bits });
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

/// Bit width implied by a Verilog type keyword, if any.
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
        Expr::Rising(_) | Expr::Falling(_) | Expr::ToInt(_) | Expr::ToString(_) => None,
        Expr::Lit(v) => Some(v.clone()),
        Expr::Real(x) => Some(Value::from_f64(*x)),
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
        Expr::Resize { e, width, signed } => {
            let v = const_eval(e, env)?;
            if *signed && v.width() < *width {
                let mut bits = vec![if v.bit(v.width() - 1) == V1 { V1 } else { V0 }; *width];
                for i in 0..v.width() {
                    bits[i] = v.bit(i);
                }
                Some(Value { bits })
            } else {
                Some(v.resize(*width))
            }
        }
        Expr::Signed(e) => const_eval(e, env),
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
