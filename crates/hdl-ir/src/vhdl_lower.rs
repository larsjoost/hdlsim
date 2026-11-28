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
//!
//! Layout: this file holds `lower_file`, the `Lower` core impl (`new`, the
//! entity-map snapshot/install and context-merge methods, diagnostics and
//! text helpers), and module-level helpers. The design-unit IR types live in
//! `units`, the `Lower` struct itself (its fields are the module's real
//! interface — each submodule's `//!` header states which fields it owns) in
//! `env`, the per-file unit passes in `passes`, and the tests in `tests`.
//! The lowering itself lives in per-responsibility submodules:
//! `context`-free helpers in `scalar`, config/binding in `config`,
//! architecture selection in `variant`, object/record naming in `objects`,
//! type decls in `types`, widths in `width`, entities in `entity`, packages
//! in `package`, architectures/concurrent in `arch`, object decls in
//! `decls`, subprograms in `subprogram` (slot machinery in `slots`),
//! processes in `process`, sequential statements in `seq` (loops/waits in
//! `loops`), selected-name chains in `chains`, aggregates in `aggregate`,
//! expressions/attributes in `expr`/`attrs`/`dump`.

use crate::ir::*;
use crate::levenshtein_bounded;
use crate::value::{BinOp, UnOp, Value, V0, V1, VDASH, VH, VL, VU, VW, VX, VZ};
use crate::verilog_lower::const_eval_f_with_pkgs;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::{Diagnostic, Severity};
use hdl_syntax::kinds::SyntaxKind;
use hdl_syntax::source::{FileId, SourceDb, Span};
use serde::Serialize;

/// Capacity of a simulated std.textio `line` buffer, in bytes.
pub const TEXTIO_LINE_CAP: usize = 1024;

/// std.textio procedure names that lower to simulated `$textio_*` syscalls
/// (statement position). Their arguments may be plain names (`output`, `l`).
pub(crate) const TEXTIO_PROCS: &[&str] = &[
    "write",
    "read",
    "readline",
    "writeline",
    "endfile",
    "file_open",
    "file_close",
];
use std::collections::HashMap;
use std::rc::Rc;

mod aggregate;
mod context;
use context::use_clause_units;
pub use context::{
    context_libraries, context_unit_libs, context_units, declared_unit_names,
    package_inst_target_units,
};
mod arch;
mod attrs;
mod chains;
mod char_lits;
mod compare;
mod config;
mod decls;
mod dump;
mod env;
use env::Lower;
mod entity;
mod expr;
mod loops;
mod objects;
mod package;
mod passes;
mod proc_vars;
mod process;
mod process_sel;
mod scalar;
mod seq;
mod sigmaps;
pub use sigmaps::{ProtType, SigMaps, TypeInfo};
mod slots;
mod subprogram;
mod types;
mod units;
pub use units::{Binding, BlockCfg, ConfigIr, Lowered, ObjClass, ObjDims, PackageIr, PendingArch};
use units::{ConcOut, VariantSpec, WaveElem};
mod variant;
mod width;
pub(crate) use scalar::*;
use variant::{apply_default_archs, arch_entity};
pub use variant::{lower_entity_variant, lower_pending_architectures, stem_key};

// The lowerer's entry points thread the accumulated design-wide maps
// (packages, configs, entity roots/maps…) from file to file; a params
// struct would just relocate the same fields without hiding any.
#[allow(clippy::too_many_arguments)]
pub fn lower_file(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    _time_unit_fs: u64,
    pkgs: &HashMap<String, Rc<PackageIr>>,
    pkg_funcs: &std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
    cfgs: &HashMap<String, Rc<ConfigIr>>,
) -> Lowered {
    let _ = file;
    let mut lx = Lower::for_file(root, db, diags, pkgs, pkg_funcs, cfgs);
    let mut modules: Vec<ModuleIr> = Vec::new();
    // Pass 0: packages and package instantiations, before anything that may
    // `use` them (see `lower_pass0_packages`).
    let mut packages = passes::lower_pass0_packages(&mut lx, root);
    // Re-apply the context clauses now that same-file packages (plain and
    // instantiated) are in `pkgs`.
    lx.apply_context_clauses(root);
    // Pass 1: entities (interfaces).
    let (entity_roots, entities) = passes::lower_pass1_entities(&mut lx, root, &mut modules);
    // Configurations first, so `configuration work.cfg` instantiations and
    // `use configuration` bindings resolve regardless of declaration order.
    let (configurations, config_defaults) = passes::lower_config_decls(&mut lx, root);
    // Pass 2: architectures (bodies); ones whose entity is not declared in
    // this file are deferred (`pending`) — see `lower_pass2_architectures`.
    let mut pending: Vec<PendingArch> = Vec::new();
    let last_arch =
        passes::lower_pass2_architectures(&mut lx, db, root, &entities, &mut modules, &mut pending);
    apply_default_archs(&mut modules, &entities, &last_arch, &config_defaults, None);
    // Package instantiations from declarative parts (registered on the fly).
    packages.append(&mut lx.inst_packages);
    Lowered {
        modules,
        packages,
        configurations,
        pending,
        entity_roots,
        entity_maps: lx.entity_maps.clone(),
        entity_maps_by_dir: lx.entity_maps_by_dir.clone(),
        obj_dims: lx.obj_dims.clone(),
    }
}

/// Canonical source directory of the file a syntax node came from — the key
/// that groups sibling design units (entity + its architectures) together.
fn file_dir(db: &SourceDb, root: &SyntaxNode) -> Option<String> {
    let name = db.file(root.span().file).name().to_string();
    let parent = std::path::Path::new(&name).parent()?.to_path_buf();
    Some(
        std::fs::canonicalize(&parent)
            .map(|c| c.to_string_lossy().to_string())
            .unwrap_or_else(|_| parent.to_string_lossy().to_string()),
    )
}

/// Package declaration name: the first ident that is not a keyword —
/// `package NAME is …` and `package body NAME is …` both yield NAME (a naive
/// nth(1) would return "body" for package bodies).
fn package_decl_name(p: &SyntaxNode, db: &SourceDb) -> String {
    real_children(p)
        .iter()
        .filter(|k| k.kind() == SyntaxKind::Ident)
        .map(|k| db.span_text(k.span()).trim().to_ascii_lowercase())
        .find(|w| !matches!(w.as_str(), "package" | "body" | "is" | "end"))
        .unwrap_or_default()
}

impl<'a> Lower<'a> {
    fn new(db: &'a SourceDb, diags: &'a mut Vec<Diagnostic>) -> Lower<'a> {
        Lower {
            db,
            diags,
            consts: Rc::new(HashMap::new()),
            const_types: HashMap::new(),
            funcs: HashMap::new(),
            merged_all: std::collections::HashSet::new(),
            funcs_ce: std::rc::Rc::new(HashMap::new()),
            types: HashMap::new(),
            type_widths: HashMap::new(),
            type_elem: HashMap::new(),
            type_dims: HashMap::new(),
            names: HashMap::new(),
            mems: HashMap::new(),
            sig_signed: HashMap::new(),
            sig_class: HashMap::new(),
            arith_visible: false,
            sig_rec: HashMap::new(),
            pkg_sig_types: HashMap::new(),
            pkg_sig_widths: HashMap::new(),
            sig_elem_rec: HashMap::new(),
            sig_width: HashMap::new(),
            sig_dims: HashMap::new(),
            obj_dims: HashMap::new(),
            obj_asc: HashMap::new(),
            sig_asc: HashMap::new(),
            sig_elem: HashMap::new(),
            sig_dynw: HashMap::new(),
            agg_target_w: None,
            rec_field_dims: HashMap::new(),
            access_inner: HashMap::new(),
            rec_field_ty: HashMap::new(),
            phys_units: HashMap::new(),
            names_stack: Vec::new(),
            extra_signals: Vec::new(),
            extra_base: 0,
            inst_count: 0,
            uncon_ports: std::collections::HashSet::new(),
            fn_formals: std::collections::HashSet::new(),
            formal_dyn: HashMap::new(),
            aliases: HashMap::new(),
            alias_rec: HashMap::new(),
            suppress_err: 0,
            fn_ret_str: false,
            fn_scope: 0,
            pkgs: HashMap::new(),
            cfgs: HashMap::new(),
            file_root: None,
            inst_packages: Vec::new(),
            entity_maps: HashMap::new(),
            entity_maps_by_dir: HashMap::new(),
            entity_consts: HashMap::new(),
            entity_types: HashMap::new(),
            entity_obj_dims: HashMap::new(),
            entity_obj_asc: HashMap::new(),
            entity_aliases: HashMap::new(),
            entity_alias_rec: HashMap::new(),
            attr_decls: std::collections::HashSet::new(),
            variant: None,
            cur_entity: String::new(),
            cur_scope: String::new(),
            str_ctx: false,
            per_instance_names: std::collections::HashSet::new(),
            impure_fns: std::collections::HashSet::new(),
            prot_types: HashMap::new(),
            prot_body_ctx: None,
            sig_prot: HashMap::new(),
            pending_funcs: Vec::new(),
            file_seq: 0,
            gen_path: Vec::new(),
        }
    }

    /// A blank lowerer wired for one design file: built-in types registered,
    /// the design-wide package/config tables installed (lowercased keys,
    /// matching the `use`-clause lookups), and the file's context clauses
    /// applied.
    #[allow(clippy::too_many_arguments)]
    fn for_file(
        root: &SyntaxNode,
        db: &'a SourceDb,
        diags: &'a mut Vec<Diagnostic>,
        pkgs: &HashMap<String, Rc<PackageIr>>,
        pkg_funcs: &std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
        cfgs: &HashMap<String, Rc<ConfigIr>>,
    ) -> Lower<'a> {
        let mut lx = Lower::new(db, diags);
        lx.register_builtin_types();
        lx.file_root = Some(root.clone());
        lx.pkgs = pkgs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        lx.funcs_ce = pkg_funcs.clone();
        lx.cfgs = cfgs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        lx.apply_context_clauses(root);
        lx
    }

    /// Snapshot the current signal maps (called after an entity's ports are
    /// lowered) and clear them so the next entity starts fresh.
    fn snapshot_entity_maps(&mut self, entity: &str) {
        let maps = SigMaps {
            mems: self.mems.clone(),
            sig_signed: self.sig_signed.clone(),
            sig_class: self.sig_class.clone(),
            arith: self.arith_visible,
            sig_rec: self.sig_rec.clone(),
            sig_elem_rec: self.sig_elem_rec.clone(),
            sig_width: self.sig_width.clone(),
            sig_dims: self.sig_dims.clone(),
            sig_asc: self.sig_asc.clone(),
            sig_elem: self.sig_elem.clone(),
            uncon_ports: self.uncon_ports.clone(),
            consts: self.entity_consts.clone(),
            types: self.entity_types.clone(),
            obj_dims: self.entity_obj_dims.clone(),
            obj_asc: self.entity_obj_asc.clone(),
            aliases: self.entity_aliases.clone(),
            alias_rec: self.entity_alias_rec.clone(),
        };
        self.entity_maps.insert(entity.to_string(), maps.clone());
        // Dir-keyed twin: the entity's own source directory disambiguates
        // same-named entities from different libraries/directories.
        if let Some(dir) = self.file_root.as_ref().and_then(|r| file_dir(self.db, r)) {
            self.entity_maps_by_dir
                .insert((dir, entity.to_ascii_lowercase()), maps);
        }
        self.mems.clear();
        self.sig_signed.clear();
        self.sig_class.clear();
        self.sig_rec.clear();
        self.sig_elem_rec.clear();
        self.sig_width.clear();
        self.sig_dims.clear();
        self.sig_asc.clear();
        self.sig_elem.clear();
        self.uncon_ports.clear();
    }

    /// Install the signal maps of the entity whose architecture is being
    /// lowered (SigId is module-relative). The (directory, entity) entry wins:
    /// a sibling entity's snapshot is the correct scope even when another
    /// directory declared an entity with the same name first.
    fn install_entity_maps(&mut self, entity: &str) {
        let dir_key = self
            .file_root
            .as_ref()
            .and_then(|r| file_dir(self.db, r))
            .map(|d| (d, entity.to_ascii_lowercase()));
        let m = dir_key
            .as_ref()
            .and_then(|k| self.entity_maps_by_dir.get(k))
            .cloned()
            .or_else(|| self.entity_maps.get(entity).cloned())
            .unwrap_or_default();
        self.mems = m.mems;
        self.sig_signed = m.sig_signed;
        self.sig_class = m.sig_class;
        // OR, not assign: an arch file's own `use ieee.std_logic_arith`
        // must survive the entity's install (and vice versa).
        self.arith_visible |= m.arith;
        self.sig_rec = m.sig_rec;
        self.sig_elem_rec = m.sig_elem_rec;
        self.sig_width = m.sig_width;
        self.sig_dims = m.sig_dims;
        self.sig_asc = m.sig_asc;
        self.sig_elem = m.sig_elem;
        self.uncon_ports = m.uncon_ports;
        for (k, v) in m.consts {
            Rc::make_mut(&mut self.consts).entry(k).or_insert(v);
        }
        for (k, v) in m.types {
            self.types.entry(k).or_insert(v);
        }
        for (k, v) in m.obj_dims {
            self.obj_dims.entry(k).or_insert(v);
        }
        for (k, v) in m.obj_asc {
            self.obj_asc.entry(k).or_insert(v);
        }
    }

    /// Merge the packages named in the file's context clauses into scope
    /// (`use lib.pkg.all;` merges everything, `use lib.pkg.item;` just that item).
    fn apply_context_clauses(&mut self, root: &SyntaxNode) {
        for child in real_children(root) {
            if matches!(
                child.kind(),
                SyntaxKind::ContextClause | SyntaxKind::UseClause
            ) {
                self.merge_use_clause(&child);
            }
        }
        // The const-eval fallback (`funcs_ce`) is shared design-wide
        // (`Design::pkg_funcs`) — see the field docs — so nothing to
        // snapshot here: rebuilding it per context application re-cloned the
        // whole package closure (quadratic in design size).
    }

    /// Merge one `use lib.pkg.item;` / `context lib.pkg;` clause into scope.
    fn merge_use_clause(&mut self, u: &SyntaxNode) {
        for n in real_children(u) {
            if n.kind() == SyntaxKind::Name {
                let text = self.db.span_text(n.span()).trim();
                // Whitespace is free between tokens (`pkg . item` equals
                // `pkg.item`), so every dotted part is trimmed — the raw
                // span text keeps interior spacing (`pkg. item`) that would
                // otherwise miss the item key.
                let parts: Vec<&str> = text.split('.').map(|p| p.trim()).collect();
                // Two-part names are ambiguous: `lib.unit` (e.g.
                // `context vunit_lib.com_context;`) vs `unit.item` / `unit.all`.
                // If the second part names a known package, it is the unit.
                let (unit, item) = match parts.len() {
                    n if n >= 3 => (parts[n - 2], parts[n - 1]),
                    2 if parts[1].eq_ignore_ascii_case("all") => (parts[0], parts[1]),
                    2 if self.pkgs.contains_key(&parts[1].to_ascii_lowercase()) => {
                        (parts[1], "all")
                    }
                    2 => (parts[0], parts[1]),
                    _ => continue,
                };
                // std_logic_arith's mixed comparisons wrap the integer
                // operand (numeric_std's extend instead) — recorded for the
                // comparison gate in `wrap_mixed_compare`.
                if unit.eq_ignore_ascii_case("std_logic_arith") {
                    self.arith_visible = true;
                }
                if let Some(p) = self.pkgs.get(&unit.to_ascii_lowercase()).cloned() {
                    let item = item.to_ascii_lowercase();
                    self.merge_package(&p, if item == "all" { None } else { Some(&item) }, false);
                }
            }
        }
    }

    fn err(&mut self, span: Span, msg: impl Into<String>) {
        if self.suppress_err > 0 {
            return;
        }
        let msg = msg.into();
        self.diags.push(Diagnostic {
            code: "E0401",
            severity: Severity::Error,
            span: Some(span),
            message: msg,
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

    /// Index of the formal name of a named association (`formal => actual`),
    /// or `None` for a positional one. The lexer emits `=>` as two
    /// single-char puncts, so the arrow is the first adjacent `=` `>` pair.
    /// A top-level association expression never yields a bare `=` followed
    /// by `>` (binary ops parse into expr nodes; `>=` lexes in reverse
    /// order), so the first pair is the association arrow when present.
    fn assoc_formal(&self, ck: &[SyntaxNode]) -> Option<usize> {
        let arrow = ck.windows(2).position(|w| {
            w[0].kind() == SyntaxKind::Punct
                && self.text(&w[0]) == "="
                && w[1].kind() == SyntaxKind::Punct
                && self.text(&w[1]) == ">"
        })?;
        ck[..arrow]
            .iter()
            .rposition(|c| c.kind() == SyntaxKind::Name)
    }

    // ---- declarations ------------------------------------------------------------

    // ---- processes and statements ---------------------------------------------------

    // ---- expressions ---------------------------------------------------------------
}

/// String as a Value (first char in the high byte, matching `lower_literal`).
pub(crate) fn string_value(s: &str) -> Value {
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

#[cfg(test)]
mod tests;
