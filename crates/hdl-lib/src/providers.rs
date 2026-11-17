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

fn walk(dir: &Path, exts: &[&str], out: &mut Vec<(String, PathBuf)>) {
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
            walk(&p, exts, out);
        } else if p
            .extension()
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
