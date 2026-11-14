//! # hdl-lib
//!
//! Library manager: named logical libraries mapped to directories, automatic
//! dependency search across multiple libraries, on-demand compilation, and an
//! in-session compile cache.
//!
//! Pipeline position: between `hdl-syntax` (parsing) and `hdl-ir` (elaboration).
//! It owns the `SourceDb`, accumulates a `Design` by compiling files on demand,
//! and plugs into elaboration via [`hdl_ir::elab::UnitResolver`].
//!
//! Extension seam: [`providers`] — a library is anything implementing
//! `trait UnitProvider` (filesystem dir, built-in `std`/`ieee`; later zip or
//! remote providers). Resolution code never changes when a provider is added.
//!
//! Invariants:
//! - Resolution order is deterministic: providers are searched in the order
//!   they were added (`work` first by convention). Every miss reports everything
//!   that was searched.
//! - Each source file is compiled at most once per session (the `compiled`
//!   cache), so dependency cycles surface as "not found"/cycle errors instead
//!   of re-parsing.
//! - Diagnostics accumulate in one `Diagnostics` list with source spans.

pub mod builtin;
pub mod bundle;
pub mod providers;

use hdl_ir::elab::UnitResolver;
use hdl_ir::verilog_lower;
use hdl_ir::Design;
use hdl_syntax::diagnostic::{Diagnostic, Diagnostics};
use hdl_syntax::language::Language;
use hdl_syntax::source::SourceDb;
use hdl_syntax::verilog::{parser, preproc};
use providers::{DirProvider, UnitProvider};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The library manager: providers (search path) + compile pipeline + cache.
pub struct LibraryManager {
    providers: Vec<Box<dyn UnitProvider>>,
    /// Built-in `std`/`ieee` providers, always searched *after* user providers
    /// so user mappings override them.
    builtins: Vec<Box<dyn UnitProvider>>,
    incdirs: Vec<PathBuf>,
    db: SourceDb,
    diags: Diagnostics,
    /// unit name → candidate source files, in provider/search order.
    index: HashMap<String, Vec<PathBuf>>,
    indexed: bool,
    /// Canonical paths already compiled this session.
    compiled: HashSet<PathBuf>,
}

impl LibraryManager {
    pub fn new() -> LibraryManager {
        LibraryManager {
            providers: Vec::new(),
            builtins: builtin::stdlib_providers(),
            incdirs: Vec::new(),
            db: SourceDb::new(),
            diags: Diagnostics::new(),
            index: HashMap::new(),
            indexed: false,
            compiled: HashSet::new(),
        }
    }

    /// Add a provider at the end of the search path.
    pub fn add_provider(&mut self, p: Box<dyn UnitProvider>) {
        self.providers.push(p);
        self.indexed = false;
    }

    /// Add a named filesystem library directory (Verilog + VHDL sources).
    pub fn add_library_dir(&mut self, name: &str, path: &Path) {
        self.add_provider(Box::new(DirProvider::mixed(name, path)));
    }

    /// Add an include search directory for `` `include ``.
    pub fn add_incdir(&mut self, path: PathBuf) {
        self.incdirs.push(path);
    }

    /// Library names in search order (for diagnostics): user providers first,
    /// then the built-in `std`/`ieee`.
    pub fn library_names(&self) -> Vec<&str> {
        self.providers
            .iter()
            .chain(self.builtins.iter())
            .map(|p| p.name())
            .collect()
    }

    pub fn db(&self) -> &SourceDb {
        &self.db
    }

    pub fn diags(&self) -> &Diagnostics {
        &self.diags
    }

    pub fn diags_mut(&mut self) -> &mut Diagnostics {
        &mut self.diags
    }

    pub fn take_db(&mut self) -> SourceDb {
        std::mem::take(&mut self.db)
    }

    pub fn take_diags(&mut self) -> Diagnostics {
        std::mem::take(&mut self.diags)
    }

    /// Load a library config file (`hdlsim.ini`, modelsim.ini-style):
    /// a `[Library]` section with `name = path` lines, or bare `name = path`
    /// lines. Paths are relative to the config file's directory.
    pub fn load_config(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for (name, libpath) in parse_library_config(&text, base) {
            self.add_library_dir(&name, &libpath);
        }
        Ok(())
    }

    /// All providers as `(library name, [(unit, file)])` — used by
    /// project-file generation to describe the full library contents.
    pub fn scan_libraries(&self) -> Vec<(String, Vec<(String, PathBuf)>)> {
        self.providers
            .iter()
            .chain(self.builtins.iter())
            .map(|p| (p.name().to_string(), p.scan()))
            .collect()
    }

    /// Build the unit-name → files index by scanning all providers (once):
    /// user providers first, built-ins last.
    pub fn ensure_index(&mut self) {
        if self.indexed {
            return;
        }
        for p in self.providers.iter().chain(self.builtins.iter()) {
            for (unit, file) in p.scan() {
                self.index.entry(unit).or_default().push(file);
            }
        }
        self.indexed = true;
    }

    /// Compile one source file into `design` (at most once per session).
    pub fn compile_file(&mut self, design: &mut Design, path: &Path) -> Result<(), String> {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !self.compiled.insert(canon) {
            return Ok(()); // already compiled this session
        }
        compile_one(path, &self.incdirs, &mut self.db, &mut self.diags, design)
    }

    /// Compile a list of explicit files (e.g. the CLI's file arguments).
    pub fn compile_files(&mut self, design: &mut Design, paths: &[PathBuf]) {
        for p in paths {
            if let Err(e) = self.compile_file(design, p) {
                self.diags.push(Diagnostic::error("E0501", None, e));
            }
        }
    }
}

impl Default for LibraryManager {
    fn default() -> Self {
        Self::new()
    }
}

/// On-demand dependency resolution for elaboration: find the unit in the
/// indexed libraries, compile its file, and report what was searched on a miss.
impl UnitResolver for LibraryManager {
    fn resolve(&mut self, design: &mut Design, name: &str) -> Result<bool, String> {
        self.ensure_index();
        let Some(candidates) = self.index.get(name).cloned() else {
            let searched = self.library_names().join(", ");
            return Err(format!(
                "design unit '{}' not found (searched libraries: {})",
                name,
                if searched.is_empty() {
                    "<none>".to_string()
                } else {
                    searched
                }
            ));
        };
        for path in candidates {
            self.compile_file(design, &path)?;
            if design.modules.iter().any(|m| m.name == name) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Parse library config text into `(name, path)` pairs. Accepts a `[Library]`
/// section (modelsim.ini style) and bare `name = path` lines; `#`/`;` comments.
pub fn parse_library_config(text: &str, base: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut in_library_section = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_library_section = line.eq_ignore_ascii_case("[library]");
            continue;
        }
        if let Some(eq) = line.find('=') {
            let name = line[..eq].trim();
            let value = line[eq + 1..].trim();
            // Only treat as a library mapping inside [Library] or before any
            // other section header.
            if in_library_section && !name.is_empty() && !value.is_empty() {
                out.push((name.to_string(), base.join(value)));
            }
        }
    }
    out
}

/// Compile one source file into `design` (preprocess → parse → lower).
/// Verilog now; VHDL joins when the VHDL front-end lands.
pub fn compile_one(
    path: &Path,
    incdirs: &[PathBuf],
    db: &mut SourceDb,
    diags: &mut Diagnostics,
    design: &mut Design,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let lang = Language::from_path(path)
        .ok_or_else(|| format!("unknown file extension: {}", path.display()))?;
    match lang {
        Language::Verilog => {
            let raw_id = db
                .add_file(path.display().to_string(), text)
                .map_err(|e| e.to_string())?;
            let mut macros = HashMap::new();
            let mut pdiags = Vec::new();
            let expanded =
                preproc::preprocess(raw_id, db.file(raw_id), incdirs, &mut macros, &mut pdiags);
            diags.extend(pdiags);
            let id = db
                .add_file(path.display().to_string(), expanded)
                .map_err(|e| e.to_string())?;
            let parsed = parser::parse_file(id, db.file(id));
            diags.extend(parsed.diags);
            let time_unit_fs = parsed
                .timescale
                .as_deref()
                .map(parser::timescale_to_fs)
                .unwrap_or(1_000_000); // LRM default: 1 ns
            let mut lower_diags = Vec::new();
            let modules = verilog_lower::lower_file_with_time(
                id,
                &parsed.root,
                db,
                &mut lower_diags,
                time_unit_fs,
            );
            diags.extend(lower_diags);
            design.modules.extend(modules);
        }
        Language::Vhdl => {
            let raw_id = db
                .add_file(path.display().to_string(), text)
                .map_err(|e| e.to_string())?;
            let parsed = hdl_syntax::vhdl::parser::parse_file(raw_id, db.file(raw_id));
            diags.extend(parsed.diags);
            let mut lower_diags = Vec::new();
            let modules = hdl_ir::vhdl_lower::lower_file(
                raw_id,
                &parsed.root,
                db,
                &mut lower_diags,
                1_000_000,
            );
            diags.extend(lower_diags);
            design.modules.extend(modules);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_library_config_sections() {
        let text = "# comment\n[Library]\nwork = .\nmylib = libs/mylib\n";
        let libs = parse_library_config(text, Path::new("/proj"));
        assert_eq!(
            libs,
            vec![
                ("work".to_string(), PathBuf::from("/proj/.")),
                ("mylib".to_string(), PathBuf::from("/proj/libs/mylib")),
            ]
        );
    }

    #[test]
    fn ignores_non_library_sections() {
        let text = "[Other]\nfoo = bar\n[Library]\nwork = .\n";
        let libs = parse_library_config(text, Path::new("."));
        assert_eq!(libs, vec![("work".to_string(), PathBuf::from("./."))]);
    }

    #[test]
    fn resolves_modules_from_library_dirs() {
        let dir = std::env::temp_dir().join(format!("hdlsim_lib_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("counter.v"),
            "module counter(input clk, output reg [7:0] q); always @(posedge clk) q <= q + 1; endmodule\n",
        )
        .unwrap();
        let mut mgr = LibraryManager::new();
        mgr.add_library_dir("work", &dir);
        let mut design = Design::default();
        // Nothing compiled yet: resolve must find + compile counter on demand.
        assert!(mgr.resolve(&mut design, "counter").unwrap());
        assert!(design.modules.iter().any(|m| m.name == "counter"));
        // Second resolve is a cache hit (no error, still true).
        assert!(mgr.resolve(&mut design, "counter").unwrap());
        // A miss reports the searched libraries.
        let err = mgr.resolve(&mut design, "nonexistent").unwrap_err();
        assert!(
            err.contains("nonexistent") && err.contains("work"),
            "{}",
            err
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_standard_libraries_are_wired_last() {
        let mut mgr = LibraryManager::new();
        let names = mgr.library_names();
        assert!(
            names.contains(&"std") && names.contains(&"ieee"),
            "{:?}",
            names
        );
        mgr.add_library_dir("work", Path::new("/nonexistent-work-dir"));
        // User providers come before the built-ins in search order.
        let names = mgr.library_names();
        assert_eq!(&names[..1], &["work"]);
        // The stdlib tree indexes package units, and they compile cleanly
        // through the real parse+lower pipeline.
        let dir = builtin::stdlib_dir().expect("stdlib dir in checkout");
        let mut design = Design::default();
        mgr.compile_file(&mut design, &dir.join("std/standard.vhd"))
            .unwrap();
        mgr.compile_file(&mut design, &dir.join("ieee/std_logic_1164.vhd"))
            .unwrap();
        mgr.compile_file(&mut design, &dir.join("ieee/numeric_std.vhd"))
            .unwrap();
        assert!(
            !mgr.diags().has_errors(),
            "{}",
            mgr.diags().render_all(mgr.db())
        );
    }
}
