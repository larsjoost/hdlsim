//! Generic-map variant resolution: re-lowering an entity and its
//! architectures with an instantiation's constant generics applied,
//! registered as `<entity>#<k=v,...>` modules so elaboration binds the
//! child's generics (generate ranges, port widths, ...).
//!
//! Source selection is provenance-first: the files the user actually
//! compiled (recorded per module in `module_source`) win over same-named
//! twins found by scanning library directories, whose outcome would
//! otherwise depend on directory-walk order.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::LibraryManager;
use hdl_ir::{Design, ModuleIr, Value};
use hdl_syntax::cst::SyntaxNode;

/// Everything needed to name and look up one variant of a unit.
struct VariantKey {
    /// Lowercase entity name without the suffix.
    lname: String,
    /// Lowercase architecture name, when the unit named one.
    arch: Option<String>,
    /// `#g=..,..` suffix (items sorted for a stable key).
    suffix: String,
    /// `<lname><suffix>`.
    vbase: String,
    /// `<vbase>[/<arch>]` — the module the caller wants.
    want: String,
}

/// Split a unit name and derive its variant module names for `params`.
fn variant_key(unit: &str, params: &HashMap<String, Value>) -> VariantKey {
    let (ent, arch) = match unit.split_once('/') {
        Some((e, a)) => (e.to_string(), Some(a.to_ascii_lowercase())),
        None => (unit.to_string(), None),
    };
    let lname = ent.to_ascii_lowercase();
    let mut items: Vec<(String, u64)> = params
        .iter()
        .map(|(k, v)| (k.clone(), v.to_u64().unwrap_or(0)))
        .collect();
    items.sort();
    let suffix = format!(
        "#{}",
        items
            .iter()
            .map(|(k, v)| format!("{k}={v:x}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let vbase = format!("{lname}{suffix}");
    let want = match &arch {
        Some(a) => format!("{vbase}/{a}"),
        None => vbase.clone(),
    };
    VariantKey {
        lname,
        arch,
        suffix,
        vbase,
        want,
    }
}

/// Canonical directory of `path` (canonicalized parent, else the raw
/// parent): CLI paths (`tb.vhd`) and search results (`./tb.vhd`) yield
/// different raw parents and must still compare equal.
fn canonical_dir(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path)
        .ok()
        .and_then(|c| c.parent().map(|d| d.to_path_buf()))
        .or_else(|| path.parent().map(|d| d.to_path_buf()))
}

/// Text of `path` when it declares an entity named `lname`.
fn declaring_entity_file(path: &Path, lname: &str) -> Option<(PathBuf, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let declares = crate::providers::vhdl_units_in_file(path, &text)
        .iter()
        .any(|(n, b)| n.eq_ignore_ascii_case(lname) && !*b);
    declares.then(|| (path.to_path_buf(), text))
}

impl LibraryManager {
    /// Candidate files that may declare the variant's entity or hold its
    /// architectures: file-stem index, then unit-name index, then the
    /// project search — merged, because the stem index keys raw file stems
    /// and a sibling architecture file (`foo_rtl.vhd` for `foo_entity.vhd`)
    /// never matches the unit name on its own.
    fn variant_candidates(&mut self, lname: &str) -> Vec<PathBuf> {
        self.ensure_stem_index();
        let mut cands: Vec<PathBuf> = self
            .stem_index
            .get(lname)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        if cands.is_empty() {
            self.ensure_index();
            cands = self
                .index
                .get(lname)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(_, p)| p)
                .collect();
        }
        if cands.is_empty() {
            return self.search_project(lname);
        }
        for p in self.search_project(lname) {
            if !cands.contains(&p) {
                cands.push(p);
            }
        }
        cands
    }

    /// The entity's declaration file, provenance-first: the file of the
    /// module the instantiation actually bound to (`bound`), else the file
    /// of a bare-name entity module — preferring the twin next to the bound
    /// file, since whole-tree libraries hold same-named entities from
    /// several files and stem order would pick an unrelated one (whose
    /// ports/generics differ). Returns `None` when nothing on the design
    /// carries provenance for `lname`.
    fn variant_entity_file(
        &self,
        design: &Design,
        bound: usize,
        lname: &str,
    ) -> Option<(PathBuf, String)> {
        if let Some(bpath) = self.module_source.get(&bound) {
            if let Some(found) = declaring_entity_file(bpath, lname) {
                return Some(found);
            }
        }
        // The bound module is the architecture module (`ent/arch`): its file
        // holds only the architecture, so the entity declaration lives in a
        // bare entity module's file — same provenance chain.
        let bound_dir = self
            .module_source
            .get(&bound)
            .and_then(|p| canonical_dir(p));
        let mut fallback: Option<(PathBuf, String)> = None;
        for (i, m) in design.modules.iter().enumerate() {
            if !m.name.eq_ignore_ascii_case(lname) {
                continue;
            }
            let Some(epath) = self.module_source.get(&i) else {
                continue;
            };
            let Some(found) = declaring_entity_file(epath, lname) else {
                continue;
            };
            if bound_dir.is_some() && canonical_dir(&found.0) == bound_dir {
                return Some(found);
            }
            if fallback.is_none() {
                fallback = Some(found);
            }
        }
        fallback
    }

    /// Parse the sibling architecture files (same directory as the entity
    /// declaration) into syntax roots for the variant lowering.
    fn variant_arch_roots(
        &mut self,
        epath: &Path,
        arch_files: &[(PathBuf, String)],
    ) -> Vec<SyntaxNode> {
        let edir_c = canonical_dir(epath);
        let mut roots = Vec::new();
        for (apath, atext) in arch_files {
            // Sibling files only: a same-named entity in another directory
            // must not bind its architectures to this one.
            if canonical_dir(apath) != edir_c {
                continue;
            }
            let Ok(aid) = self.db.add_file(apath.display().to_string(), atext.clone()) else {
                continue;
            };
            let aparsed = hdl_syntax::vhdl::parser::parse_file(aid, self.db.file(aid));
            roots.push(aparsed.root);
        }
        roots
    }

    /// Lower the entity file plus its architecture roots into variant
    /// modules named `<entity><suffix>[/<arch>]`.
    fn lower_variant(
        &mut self,
        design: &Design,
        epath: &Path,
        etext: String,
        arch_roots: Vec<SyntaxNode>,
        key: &VariantKey,
        params: &HashMap<String, Value>,
    ) -> Vec<ModuleIr> {
        let Ok(eid) = self.db.add_file(epath.display().to_string(), etext) else {
            return Vec::new();
        };
        let eparsed = hdl_syntax::vhdl::parser::parse_file(eid, self.db.file(eid));
        let mut lower_diags = Vec::new();
        let modules = hdl_ir::vhdl_lower::lower_entity_variant(
            &self.db,
            &mut lower_diags,
            &design.packages,
            &design.pkg_funcs,
            &design.configurations,
            &eparsed.root,
            &arch_roots,
            &key.lname,
            params,
            &key.suffix,
        );
        self.diags.extend(lower_diags);
        modules
    }
}

/// Split the candidates into the entity declaration file (only when
/// provenance found none — the first candidate wins, as before) and the
/// architecture files. VHDL variants only: a Verilog `#(...)` override
/// keeps the default-parameter behavior.
fn classify_candidates(
    cands: &[PathBuf],
    lname: &str,
    ent_file: &mut Option<(PathBuf, String)>,
) -> Vec<(PathBuf, String)> {
    let mut arch_files = Vec::new();
    for path in cands {
        if !matches!(
            path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .as_deref(),
            Some("vhd") | Some("vhdl")
        ) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let units = crate::providers::vhdl_units_in_file(path, &text);
        if !units.iter().any(|(n, _)| n.eq_ignore_ascii_case(lname)) {
            continue;
        }
        // `(name, false)` = entity declaration, `(name, true)` = an
        // architecture of it (see vhdl_units_in_file).
        let has_ent = units
            .iter()
            .any(|(n, b)| n.eq_ignore_ascii_case(lname) && !*b);
        if has_ent {
            if ent_file.is_none() {
                *ent_file = Some((path.clone(), text));
            }
        } else {
            arch_files.push((path.clone(), text));
        }
    }
    arch_files
}

/// Entry point behind `UnitResolver::resolve_variant`: build (or find) the
/// `<entity>#<k=v,...>[/<arch>]` variant modules for an instantiation whose
/// target module is `bound`, and return the variant module name.
pub(crate) fn resolve_variant(
    mgr: &mut LibraryManager,
    design: &mut Design,
    unit: &str,
    params: &HashMap<String, Value>,
    bound: usize,
) -> Result<Option<String>, String> {
    let key = variant_key(unit, params);
    if design.modules.iter().any(|m| m.name == key.want) {
        return Ok(Some(key.want));
    }
    let mut cands = mgr.variant_candidates(&key.lname);
    // The actually-bound architecture file must always be a candidate: the
    // project search may not reach it, and the sibling filter in
    // `variant_arch_roots` keeps it only when it belongs to the entity's
    // directory.
    if let Some(apath) = mgr.module_source.get(&bound) {
        if !cands.contains(apath) {
            cands.insert(0, apath.clone());
        }
    }
    let mut ent_file = mgr.variant_entity_file(design, bound, &key.lname);
    let arch_files = classify_candidates(&cands, &key.lname, &mut ent_file);
    let Some((epath, etext)) = ent_file else {
        return Ok(None);
    };
    let arch_roots = mgr.variant_arch_roots(&epath, &arch_files);
    let modules = mgr.lower_variant(design, &epath, etext, arch_roots, &key, params);
    if modules.is_empty() {
        return Ok(None);
    }
    design.modules.extend(modules);
    if design.modules.iter().any(|m| m.name == key.want) {
        return Ok(Some(key.want));
    }
    if key.arch.is_none() && design.modules.iter().any(|m| m.name == key.vbase) {
        return Ok(Some(key.vbase));
    }
    Ok(None)
}
