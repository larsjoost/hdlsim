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
    /// Like `scan`, but tags each unit with whether its file only contains the
    /// *body* of the unit (`package body p;`) rather than its declaration.
    /// Resolution prefers declaration files: a body compiled before its
    /// declaration is lowered without the declaration's items in scope.
    fn scan_tagged(&self) -> Vec<(String, PathBuf, bool)> {
        self.scan()
            .into_iter()
            .map(|(u, p)| (u, p, false))
            .collect()
    }
    /// Read a unit's source text. Default: read the file `scan` reported.
    /// In-memory providers (e.g. the embedded stdlib) override this; their
    /// `scan` paths are labels, not real files.
    fn read(&self, unit: &str, file: &Path) -> Option<String> {
        let _ = unit;
        std::fs::read_to_string(file).ok()
    }

    /// Source file paths without parsing them (for a cheap file-stem unit
    /// index). Default: none (in-memory providers are scanned directly).
    fn source_files(&self) -> Vec<PathBuf> {
        Vec::new()
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

    fn scan_tagged(&self) -> Vec<(String, PathBuf, bool)> {
        let mut out = Vec::new();
        walk_tagged(&self.path, &self.extensions, &mut out);
        out
    }

    fn source_files(&self) -> Vec<PathBuf> {
        walk_source_files(&self.path)
            .into_iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map_or(false, |e| self.extensions.contains(&e))
            })
            .collect()
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

fn walk_tagged(dir: &Path, exts: &[&str], out: &mut Vec<(String, PathBuf, bool)>) {
    let files = walk_source_files_with(dir, |_, _| {});
    for p in files {
        if p.extension()
            .and_then(|e| e.to_str())
            .map_or(false, |e| exts.contains(&e))
        {
            for (unit, is_body) in units_in_file_tagged(&p) {
                out.push((unit, p.clone(), is_body));
            }
        }
    }
}

/// Walk `dir` recursively for source files of any known extension (skipping
/// hidden dirs, `target`, `node_modules`); returns `(unit, file)` pairs.
pub fn walk_sources(dir: &Path) -> Vec<(String, PathBuf)> {
    walk_sources_tagged(dir)
        .into_iter()
        .map(|(u, p, _)| (u, p))
        .collect()
}

/// Like `walk_sources`, but each pair also says whether the file holds only a
/// unit *body* (e.g. `package body p;`) — declaration files win on conflicts.
pub fn walk_sources_tagged(dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    walk_tagged(dir, &["v", "sv", "vhd", "vhdl"], &mut out);
    out
}

/// Design-unit names defined by one source file. Verilog: `module` names.
/// VHDL architectures report under their entity's name, deduplicated — a file
/// holding `entity x` + `architecture rtl of x` provides the unit `x` once.
pub fn units_in_file(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (u, _) in units_in_file_tagged(path) {
        if !out.iter().any(|e| e == &u) {
            out.push(u);
        }
    }
    out
}

/// Like `units_in_file`, but each unit is tagged with whether its file holds
/// only the unit's *body* (`package body p;`) and not its declaration.
pub fn units_in_file_tagged(path: &Path) -> Vec<(String, bool)> {
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
        .map(|n| (db.span_text(n.span()).trim().to_string(), false))
        .collect()
}

/// Design-unit names defined by a VHDL file, tagged with body-ness: entities,
/// packages, and configurations (the name is the first identifier that isn't
/// a keyword); a `package body p;` tags `p` as a body-only file.
/// Architectures report their *entity's* name tagged as body-only: an
/// architecture is only meaningful together with its entity, and indexing it
/// there lets on-demand resolution compile `X_entity.vhd` + `X_rtl.vhd`
/// together (the common vendor file layout).
pub(crate) fn vhdl_units_in_file(path: &Path, text: &str) -> Vec<(String, bool)> {
    // Cheap line scan first: design-unit headers are line-oriented
    // (`entity foo is`, `package body p is`, `architecture rtl of e is`).
    // Parsing 5000+ files just to index unit names costs seconds; the scan
    // costs milliseconds. Files where the scan finds nothing (unusual
    // formatting, package instances) fall back to the full parse.
    let (scanned, ambiguous) = vhdl_units_line_scan(text);
    if !ambiguous && !scanned.is_empty() {
        return scanned;
    }
    let mut db = SourceDb::new();
    let Ok(id) = db.add_file(path.display().to_string(), text.to_string()) else {
        return Vec::new();
    };
    let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
    let mut out: Vec<(String, bool)> = parsed
        .root
        .children()
        .filter(|c| {
            matches!(
                c.kind(),
                SyntaxKind::Entity
                    | SyntaxKind::Package
                    | SyntaxKind::PackageInst
                    | SyntaxKind::ConfigDecl
                    | SyntaxKind::ContextDecl
            )
        })
        .filter_map(|u| {
            let is_body = u.kind() == SyntaxKind::Package
                && u.children().any(|c| {
                    c.kind() == SyntaxKind::Ident
                        && db.span_text(c.span()).eq_ignore_ascii_case("body")
                });
            u.children()
                .filter(|c| c.kind() == SyntaxKind::Ident)
                .find(|n| {
                    !matches!(
                        db.span_text(n.span()).to_ascii_lowercase().as_str(),
                        "entity" | "package" | "body" | "configuration" | "context"
                    )
                })
                .map(|n| (db.span_text(n.span()).trim().to_string(), is_body))
        })
        .collect();
    // Architectures: `ARCHITECTURE rtl OF ent IS` — index under `ent`, tagged
    // body-only so the entity's declaration file sorts first.
    for a in parsed
        .root
        .children()
        .filter(|c| c.kind() == SyntaxKind::Architecture)
    {
        let idents: Vec<String> = a
            .children()
            .filter(|c| c.kind() == SyntaxKind::Ident)
            .map(|n| db.span_text(n.span()).trim().to_ascii_lowercase())
            .collect();
        // [arch_name, "of", entity_name] — take the ident after "of".
        if let Some(pos) = idents.iter().position(|s| s == "of") {
            if let Some(ent) = idents.get(pos + 1) {
                if !ent.is_empty() {
                    out.push((ent.clone(), true));
                }
            }
        }
    }
    out
}

/// Unit names by line scan: `entity N is`, `package [body] N is`,
/// `configuration N of`, `context N is`, and `architecture A of E is`
/// (tagged body-only, keyed on the entity). `--` comments are stripped
/// first. Returns the names plus an `ambiguous` flag set when a
/// design-unit keyword line could not be confidently parsed; the caller
/// then falls back to the full parse so no unit is missed.
fn vhdl_units_line_scan(text: &str) -> (Vec<(String, bool)>, bool) {
    let mut out: Vec<(String, bool)> = Vec::new();
    // A design-unit keyword line the scan cannot confidently parse (unusual
    // formatting, escaped identifiers, multi-line headers). Any such line
    // makes the caller fall back to the full parse so no unit is missed.
    let mut ambiguous = false;
    let kw = |w: &str| {
        matches!(
            w,
            "entity" | "package" | "configuration" | "context" | "architecture"
        )
    };
    for raw in text.lines() {
        let line = match raw.find("--") {
            Some(p) => &raw[..p],
            None => raw,
        };
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(w0) = words.first().map(|w| w.to_ascii_lowercase()) else {
            continue;
        };
        if !kw(&w0) {
            continue;
        }
        if words.len() < 2 {
            ambiguous = true;
            continue;
        }
        let name_of = |i: usize| -> Option<String> {
            words
                .get(i)
                .map(|w| w.trim_matches(';').trim().to_string())
                .filter(|n| !n.is_empty())
        };
        match w0.as_str() {
            "entity" => match name_of(1) {
                Some(n) => out.push((n, false)),
                None => ambiguous = true,
            },
            "package" => {
                let is_body = words[1].eq_ignore_ascii_case("body");
                let name_pos = if is_body { 2 } else { 1 };
                match name_of(name_pos) {
                    // `package p is new …` (an instantiation) also defines a
                    // unit named `p`; only skip the word `instance` itself.
                    Some(n) if !n.eq_ignore_ascii_case("instance") => out.push((n, is_body)),
                    Some(_) => {}
                    None => ambiguous = true,
                }
            }
            "configuration" | "context" => match name_of(1) {
                Some(n) => out.push((n, false)),
                None => ambiguous = true,
            },
            "architecture" => {
                match words
                    .iter()
                    .position(|w| w.eq_ignore_ascii_case("of"))
                    .and_then(|p| name_of(p + 1))
                {
                    Some(e) => out.push((e, true)),
                    None => ambiguous = true,
                }
            }
            _ => {}
        }
    }
    (out, ambiguous)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_sources_tags_package_bodies() {
        let root = std::env::temp_dir().join(format!("hdlsim_tag_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // The body file sorts before the declaration file.
        std::fs::write(
            root.join("a_p-body.vhd"),
            "package body p is end package body;",
        )
        .unwrap();
        std::fs::write(root.join("z_p.vhd"), "package p is end package;").unwrap();
        let tagged = walk_sources_tagged(&root);
        let mut p_entries: Vec<_> = tagged.iter().filter(|(u, _, _)| u == "p").collect();
        p_entries.sort_by_key(|(_, _, b)| *b);
        assert_eq!(p_entries.len(), 2);
        assert!(
            !p_entries[0].2,
            "declaration file must be non-body: {:?}",
            p_entries[0]
        );
        assert!(
            p_entries[1].2,
            "body file must be tagged body: {:?}",
            p_entries[1]
        );
        assert!(p_entries[1].1.ends_with("a_p-body.vhd"));
        // Untagged view keeps both.
        assert_eq!(
            walk_sources(&root).iter().filter(|(u, _)| u == "p").count(),
            2
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn units_line_scan_matches_full_parse_and_flags_ambiguity() {
        // Normal formatting: the scan agrees with the full parse.
        let normal = "library ieee;\nuse ieee.std_logic_1164.all;\n-- entity fake is\nentity e is\n  port (c : in std_logic);\nend entity;\narchitecture rtl of e is\nbegin\nend architecture;\npackage p is\nend package;\npackage body p is\nend package body;\ncontext ctx is\nend context;\n";
        let (names, ambiguous) = vhdl_units_line_scan(normal);
        assert!(!ambiguous);
        assert!(names.contains(&("e".to_string(), false)));
        assert!(names.contains(&("e".to_string(), true)));
        assert!(names.contains(&("p".to_string(), false)));
        assert!(names.contains(&("p".to_string(), true)));
        assert!(names.contains(&("ctx".to_string(), false)));
        // A multi-line header the scan cannot parse must be flagged, so the
        // caller falls back to the full parse instead of missing the unit.
        let odd = "package p is\nend package;\nentity\ne2\nis\nend;\n";
        let (_, ambiguous) = vhdl_units_line_scan(odd);
        assert!(ambiguous, "multi-line entity header must be flagged");
        // Cross-check the normal case against the full parse result.
        let parsed = vhdl_units_in_file(std::path::Path::new("normal.vhd"), normal);
        assert_eq!(parsed, names, "line scan must match the full parse");
    }

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
