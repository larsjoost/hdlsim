//! Unit providers — the extension seam for library sources.
//!
//! A provider answers "which design units do you have, and in which file?".
//! Filesystem directories, the built-in `std`/`ieee` libraries, and a persistent
//! cache are all providers in one list; resolution code never changes when a
//! provider is added.

use hdl_syntax::kinds::SyntaxKind;
use hdl_syntax::language::Language;
use hdl_syntax::source::SourceDb;
use hdl_syntax::verilog::{parser, preproc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A source of design units (one logical library).
pub trait UnitProvider {
    /// Library name, e.g. `work`, `ieee`.
    fn name(&self) -> &str;
    /// Scan the provider once; returns `(unit name, source file)` pairs.
    /// Called at most once per provider (the manager caches the result).
    fn scan(&self) -> Vec<(String, PathBuf)>;
    /// Read a unit's source text. Default: read the file `scan` reported.
    /// In-memory providers (e.g. the embedded stdlib) override this; their
    /// `scan` paths are labels, not real files.
    fn read(&self, unit: &str, file: &Path) -> Option<String> {
        let _ = unit;
        std::fs::read_to_string(file).ok()
    }
}

/// A library whose sources are embedded in the binary (`include_str!`).
pub struct EmbeddedProvider {
    name: String,
    /// `(unit name, source text)` pairs.
    units: &'static [(&'static str, &'static str)],
}

impl EmbeddedProvider {
    pub fn new(name: &str, units: &'static [(&'static str, &'static str)]) -> EmbeddedProvider {
        EmbeddedProvider {
            name: name.to_string(),
            units,
        }
    }
}

impl UnitProvider for EmbeddedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn scan(&self) -> Vec<(String, PathBuf)> {
        self.units
            .iter()
            .map(|(u, _)| {
                (
                    u.to_string(),
                    PathBuf::from(format!("<builtin {}>/{}.vhd", self.name, u)),
                )
            })
            .collect()
    }

    fn read(&self, unit: &str, _file: &Path) -> Option<String> {
        self.units
            .iter()
            .find(|(u, _)| *u == unit)
            .map(|(_, s)| (*s).to_string())
    }
}

/// A filesystem directory searched recursively for source files.
pub struct DirProvider {
    name: String,
    path: PathBuf,
    extensions: Vec<&'static str>,
}

impl DirProvider {
    /// A Verilog library directory (`*.v`, `*.sv`).
    pub fn verilog(name: &str, path: &Path) -> DirProvider {
        DirProvider {
            name: name.to_string(),
            path: path.to_path_buf(),
            extensions: vec!["v", "sv"],
        }
    }

    /// A VHDL library directory (`*.vhd`, `*.vhdl`) — indexed for entity names
    /// once the VHDL front-end lands; the scan itself is extension-driven.
    pub fn vhdl(name: &str, path: &Path) -> DirProvider {
        DirProvider {
            name: name.to_string(),
            path: path.to_path_buf(),
            extensions: vec!["vhd", "vhdl", "v", "sv"],
        }
    }

    /// A mixed-language library directory (`*.v`, `*.sv`, `*.vhd`, `*.vhdl`)
    /// — what `-L name=path` and `hdlsim.ini` mappings create.
    pub fn mixed(name: &str, path: &Path) -> DirProvider {
        DirProvider {
            name: name.to_string(),
            path: path.to_path_buf(),
            extensions: vec!["v", "sv", "vhd", "vhdl"],
        }
    }
}

impl UnitProvider for DirProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn scan(&self) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        walk(&self.path, &self.extensions, &mut out);
        out
    }
}

/// Directory names never searched for sources (VCS dirs, build output, caches).
fn is_skip_dir(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "target" | "node_modules")
}

fn walk_paths(
    dir: &Path,
    exts: &[&str],
    out: &mut Vec<PathBuf>,
    on_dir: &mut dyn FnMut(&Path, usize),
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort(); // deterministic scan order
    for p in entries {
        if p.is_dir() {
            if p.file_name()
                .map_or(false, |n| is_skip_dir(&n.to_string_lossy()))
            {
                continue;
            }
            walk_paths(&p, exts, out, on_dir);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .map_or(false, |e| exts.contains(&e))
        {
            out.push(p);
        }
    }
    on_dir(dir, out.len());
}

/// Walk `dir` recursively for source file paths of any known extension
/// (skipping hidden dirs, `target`, `node_modules`) without parsing them;
/// `on_dir` fires after each directory with (dir, files found so far), so
/// callers can render progress.
pub fn walk_source_files_with(dir: &Path, on_dir: impl FnMut(&Path, usize)) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut on_dir = on_dir;
    walk_paths(dir, &["v", "sv", "vhd", "vhdl"], &mut out, &mut on_dir);
    out
}

/// Walk `dir` recursively for source file paths (no parsing) — for callers
/// that only need the files, not their design units.
pub fn walk_source_files(dir: &Path) -> Vec<PathBuf> {
    walk_source_files_with(dir, |_, _| {})
}

fn walk(dir: &Path, exts: &[&str], out: &mut Vec<(String, PathBuf)>) {
    let files = walk_source_files_with(dir, |_, _| {});
    for p in files {
        if p.extension()
            .and_then(|e| e.to_str())
            .map_or(false, |e| exts.contains(&e))
        {
            for unit in units_in_file(&p) {
                out.push((unit, p.clone()));
            }
        }
    }
}

/// Walk `dir` recursively for source files of any known extension (skipping
/// hidden dirs, `target`, `node_modules`); returns `(unit, file)` pairs.
pub fn walk_sources(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    walk(dir, &["v", "sv", "vhd", "vhdl"], &mut out);
    out
}

/// Design-unit names defined by one source file. Verilog: `module` names.
/// VHDL files return no units until the VHDL front-end lands.
pub fn units_in_file(path: &Path) -> Vec<String> {
    let lang = match Language::from_path(path) {
        Some(l) => l,
        None => return Vec::new(),
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    if lang == Language::Vhdl {
        return vhdl_units_in_file(path, &text);
    }
    let mut db = SourceDb::new();
    let Ok(id) = db.add_file(path.display().to_string(), text) else {
        return Vec::new();
    };
    let mut macros = HashMap::new();
    let mut pdiags = Vec::new();
    let expanded = preproc::preprocess(id, db.file(id), &[], &mut macros, &mut pdiags);
    let id = db
        .add_file(path.display().to_string(), expanded)
        .unwrap_or(id);
    let parsed = parser::parse_file(id, db.file(id));
    parsed
        .root
        .children()
        .filter(|c| c.kind() == SyntaxKind::Module)
        .filter_map(|m| m.children().find(|c| c.kind() == SyntaxKind::Name))
        .map(|n| db.span_text(n.span()).trim().to_string())
        .collect()
}

/// Design-unit names defined by a VHDL file: entities, packages, and
/// configurations (the name is the first identifier that isn't a keyword).
fn vhdl_units_in_file(path: &Path, text: &str) -> Vec<String> {
    let mut db = SourceDb::new();
    let Ok(id) = db.add_file(path.display().to_string(), text.to_string()) else {
        return Vec::new();
    };
    let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
    parsed
        .root
        .children()
        .filter(|c| {
            matches!(
                c.kind(),
                SyntaxKind::Entity
                    | SyntaxKind::Package
                    | SyntaxKind::PackageInst
                    | SyntaxKind::ConfigDecl
            )
        })
        .filter_map(|u| {
            u.children()
                .filter(|c| c.kind() == SyntaxKind::Ident)
                .find(|n| {
                    !matches!(
                        db.span_text(n.span()).to_ascii_lowercase().as_str(),
                        "entity" | "package" | "body" | "configuration"
                    )
                })
                .map(|n| db.span_text(n.span()).trim().to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_source_files_skips_hidden_and_build_dirs() {
        let root =
            std::env::temp_dir().join(format!("hdlsim_walk_test_{}_{}", std::process::id(), "a"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("a.vhd"), "entity e is end;").unwrap();
        std::fs::write(root.join("sub/b.sv"), "module m; endmodule").unwrap();
        std::fs::write(root.join(".hidden/c.vhd"), "").unwrap();
        std::fs::write(root.join("target/d.vhd"), "").unwrap();
        std::fs::write(root.join("note.txt"), "").unwrap();

        let mut dirs_seen = Vec::new();
        let files = walk_source_files_with(&root, |dir, _n| dirs_seen.push(dir.to_path_buf()));
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.vhd".to_string(), "b.sv".to_string()]);
        assert!(dirs_seen.contains(&root));
        assert!(dirs_seen.contains(&root.join("sub")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lib_root_libraries_maps_subdirs_with_sources() {
        let root = std::env::temp_dir().join(format!("hdlsim_libroot_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("unisim")).unwrap();
        std::fs::create_dir_all(root.join("empty_lib")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("unisim/vcomponents.vhd"), "package v is end;").unwrap();
        std::fs::write(root.join(".hidden/x.vhd"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();

        let libs = lib_root_libraries(&root);
        assert_eq!(libs, vec![("unisim".to_string(), root.join("unisim"))]);
        assert_eq!(
            library_name_from_dir(&root.join("2nd-gen")),
            Some("lib_2nd_gen".to_string())
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Guess a logical library name from a directory name: lowercase, invalid
/// identifier chars become `_`, and a non-identifier start gets a `lib_`
/// prefix (e.g. `scint_scan_integrator` stays as-is, `2nd` -> `lib_2nd`).
/// Returns `None` for the project root or reserved/built-in names.
pub fn library_name_from_dir(dir: &Path) -> Option<String> {
    let raw = dir.file_name()?.to_string_lossy().to_ascii_lowercase();
    let mut name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        return None;
    }
    if !name
        .chars()
        .next()
        .map_or(false, |c| c.is_ascii_alphabetic() || c == '_')
    {
        name = format!("lib_{name}");
    }
    // Reserved / built-in names are not user libraries.
    if matches!(name.as_str(), "work" | "std" | "ieee" | "ieee_proposed") {
        return None;
    }
    Some(name)
}

/// A "library root" (e.g. `$XILINX_VIVADO/data/vhdl/src`): each immediate
/// subdirectory containing sources becomes a logical library named after the
/// subdir. Returns `(name, dir)` pairs in sorted order.
pub fn lib_root_libraries(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if !p.is_dir() {
            continue;
        }
        if p.file_name()
            .map_or(false, |n| is_skip_dir(&n.to_string_lossy()))
        {
            continue;
        }
        if walk_source_files(&p).is_empty() {
            continue;
        }
        if let Some(name) = library_name_from_dir(&p) {
            out.push((name, p));
        }
    }
    out
}

/// Xilinx vendor library roots from the environment: `HDLSIM_XILINX_LIBS`
/// (explicit root whose subdirs are libraries), else
/// `$XILINX_VIVADO/data/vhdl/src` and `$XILINX_HLS/data/vhdl/src` if those
/// directories exist.
pub fn xilinx_lib_roots_from_env() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(v) = std::env::var("HDLSIM_XILINX_LIBS") {
        if !v.is_empty() {
            roots.push(PathBuf::from(v));
        }
    }
    for var in ["XILINX_VIVADO", "XILINX_HLS"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                let p = Path::new(&v).join("data/vhdl/src");
                if p.is_dir() {
                    roots.push(p);
                }
            }
        }
    }
    roots
}
