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
pub mod cache;
pub mod doctor;
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
    /// unit name → (provider index, source file) candidates, in search order.
    /// The index counts into `providers` then `builtins` (see `all_providers`).
    /// Only in-memory (built-in) providers are scanned eagerly; filesystem
    /// providers land here via `full_scan_dir_providers` on a lookup miss.
    index: HashMap<String, Vec<(usize, PathBuf)>>,
    indexed: bool,
    /// Cheap unit index from file stems (unit name == file name, the common
    /// case) over filesystem providers — built without parsing any file.
    stem_index: HashMap<String, Vec<(usize, PathBuf)>>,
    stem_indexed: bool,
    /// Whether filesystem providers have been fully scanned (parsed) yet.
    dir_scanned: bool,
    /// Print indexing/resolution progress to stderr (`--progress`).
    progress: bool,
    /// Canonical paths already compiled this session.
    compiled: HashSet<PathBuf>,
    /// Persistent parse cache (`.hdlsim-cache/`); None unless enabled.
    cache: Option<cache::Cache>,
    /// Parse-cache hits this session.
    cache_hits: u64,
    /// VHDL architectures whose entity was not in the same file; re-bound
    /// design-wide once all known files are lowered (see `compile_files`).
    pending_archs: Vec<hdl_ir::vhdl_lower::PendingArch>,
    /// `use`d library units that failed to resolve while compiling a file:
    /// (unit name, resolve error). Warned about at the end of `compile_files`
    /// if still unresolved (a later explicit file may still provide them).
    unresolved_units: Vec<(String, String)>,
    /// Project roots searched on a resolve miss (cwd, dirs of input files):
    /// the "out of the box" fallback when no library maps the unit.
    search_roots: Vec<PathBuf>,
    /// Lazily built unit → file index over `search_roots` (built on first miss).
    search_index: Option<HashMap<String, PathBuf>>,
    /// Units already reported as found-by-search (warn once per unit).
    search_warned: HashSet<String>,
    /// Cross-file Verilog library state: SV packages and their `pkg::`-qualified
    /// constants/functions from all Verilog files compiled so far.
    verilog_scope: verilog_lower::VerilogScope,
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
            stem_index: HashMap::new(),
            stem_indexed: false,
            dir_scanned: false,
            progress: false,
            compiled: HashSet::new(),
            cache: None,
            cache_hits: 0,
            pending_archs: Vec::new(),
            unresolved_units: Vec::new(),
            search_roots: Vec::new(),
            search_index: None,
            search_warned: HashSet::new(),
            verilog_scope: verilog_lower::VerilogScope::default(),
        }
    }

    /// Add a project root that is searched (recursively, once) when a design
    /// unit is not found in any registered library. Hidden directories,
    /// `target`, `node_modules` and `.hdlsim-cache` are skipped.
    pub fn add_search_root(&mut self, p: impl Into<PathBuf>) {
        let p = p.into();
        if !self.search_roots.iter().any(|r| *r == p) {
            self.search_roots.push(p);
        }
    }

    /// Build the fallback index over the search roots (once), then look up
    /// `name`. Returns the file defining the unit, if the project has it.
    fn search_project(&mut self, name: &str) -> Option<PathBuf> {
        if self.search_index.is_none() {
            let mut idx: HashMap<String, PathBuf> = HashMap::new();
            for root in self.search_roots.clone() {
                for (unit, path) in providers::walk_sources(&root) {
                    idx.entry(unit.to_ascii_lowercase())
                        .or_insert_with(|| path.clone());
                }
            }
            self.search_index = Some(idx);
        }
        self.search_index
            .as_ref()
            .and_then(|i| i.get(&name.to_ascii_lowercase()).cloned())
    }

    /// Enable the persistent parse cache in `c`.
    pub fn enable_cache(&mut self, c: cache::Cache) {
        self.cache = Some(c);
    }

    /// Enable the cache from the environment (default `.hdlsim-cache/`,
    /// `HDLSIM_CACHE_DIR` to override, `HDLSIM_NO_CACHE` to disable). Used by
    /// the CLI; library users opt in via [`LibraryManager::enable_cache`].
    pub fn use_env_cache(&mut self) {
        self.cache = cache::Cache::from_env();
    }

    /// Parse-cache hits this session.
    pub fn cache_hits(&self) -> u64 {
        self.cache_hits
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

    /// Add a "library root" (e.g. `$XILINX_VIVADO/data/vhdl/src`): each
    /// immediate subdirectory containing sources becomes a logical library
    /// named after the subdir.
    pub fn add_lib_root(&mut self, root: &Path) {
        for (name, dir) in providers::lib_root_libraries(root) {
            self.add_library_dir(&name, &dir);
        }
    }

    /// True if a provider (library) with this name is registered.
    pub fn has_library(&self, name: &str) -> bool {
        self.all_providers()
            .any(|p| p.name().eq_ignore_ascii_case(name))
    }

    /// All design-unit names provided by the library `lib` (for library-wide
    /// `use lib.all;` clauses).
    pub fn library_unit_names(&mut self, lib: &str) -> Vec<String> {
        self.ensure_index();
        let mut out: Vec<String> = Vec::new();
        for p in self.all_providers() {
            if p.name().eq_ignore_ascii_case(lib) {
                for (u, _) in p.scan() {
                    if !out.contains(&u) {
                        out.push(u);
                    }
                }
            }
        }
        out
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

    /// All providers in search order: user providers first, built-ins last.
    fn all_providers(&self) -> impl Iterator<Item = &Box<dyn UnitProvider>> {
        self.providers.iter().chain(self.builtins.iter())
    }

    /// Enable/disable `--progress` output on stderr.
    pub fn set_progress(&mut self, on: bool) {
        self.progress = on;
    }

    /// Build the unit-name → (provider, file) index by scanning all providers
    /// (once): user providers first, built-ins last. Filesystem providers are
    /// NOT parsed here — only in-memory (built-in) providers are scanned;
    /// directory providers are covered by the stem index and fully scanned
    /// (parsed) lazily on a lookup miss (`full_scan_dir_providers`).
    pub fn ensure_index(&mut self) {
        if self.indexed {
            return;
        }
        let scanned: Vec<(usize, String, PathBuf)> = self
            .all_providers()
            .enumerate()
            .filter(|(_, p)| p.source_files().is_empty()) // in-memory providers only
            .flat_map(|(i, p)| p.scan().into_iter().map(move |(u, f)| (i, u, f)))
            .collect();
        for (i, unit, file) in scanned {
            self.index
                .entry(unit.to_ascii_lowercase())
                .or_default()
                .push((i, file));
        }
        self.indexed = true;
    }

    /// Build the cheap file-stem index over filesystem providers (no parsing):
    /// `vcomponents.vhd` is assumed to define unit `vcomponents`.
    fn ensure_stem_index(&mut self) {
        if self.stem_indexed {
            return;
        }
        let mut per_provider: Vec<(usize, String, Vec<PathBuf>)> = Vec::new();
        for (i, p) in self.all_providers().enumerate() {
            let files = p.source_files();
            if !files.is_empty() {
                per_provider.push((i, p.name().to_string(), files));
            }
        }
        for (i, name, files) in per_provider {
            if self.progress {
                eprintln!(
                    "  indexing library '{name}': {} files (by name)",
                    files.len()
                );
            }
            for f in files {
                if let Some(stem) = f.file_stem().and_then(|s| s.to_str()) {
                    let stem = stem.to_ascii_lowercase();
                    if !stem.is_empty() {
                        self.stem_index.entry(stem).or_default().push((i, f));
                    }
                }
            }
        }
        self.stem_indexed = true;
    }

    /// Fully scan (parse) the filesystem providers once, on a lookup miss —
    /// needed when a unit's name differs from its file name (multi-unit files).
    fn full_scan_dir_providers(&mut self) {
        if self.dir_scanned {
            return;
        }
        self.dir_scanned = true;
        let scanned: Vec<(usize, String, PathBuf)> = self
            .all_providers()
            .enumerate()
            .filter(|(_, p)| !p.source_files().is_empty()) // filesystem providers
            .flat_map(|(i, p)| {
                if self.progress {
                    eprintln!(
                        "  scanning library '{}' for unit names (parsing)…",
                        p.name()
                    );
                }
                p.scan().into_iter().map(move |(u, f)| (i, u, f))
            })
            .collect();
        for (i, unit, file) in scanned {
            self.index
                .entry(unit.to_ascii_lowercase())
                .or_default()
                .push((i, file));
        }
    }

    /// The file that defines `unit` according to the library index, if any.
    pub fn unit_file(&mut self, unit: &str) -> Option<PathBuf> {
        let lunit = unit.to_ascii_lowercase();
        self.ensure_stem_index();
        if let Some(v) = self.stem_index.get(&lunit) {
            if let Some((_, p)) = v.first() {
                return Some(p.clone());
            }
        }
        self.ensure_index();
        if let Some(v) = self.index.get(&lunit) {
            if let Some((_, p)) = v.first() {
                return Some(p.clone());
            }
        }
        self.full_scan_dir_providers();
        self.index
            .get(&lunit)
            .and_then(|v| v.first().map(|(_, p)| p.clone()))
    }

    /// Compile one source file into `design` (at most once per session).
    pub fn compile_file(&mut self, design: &mut Design, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        self.compile_text(design, path, &text)
    }

    /// Compile source text already in memory (e.g. from an embedded provider).
    pub fn compile_text(
        &mut self,
        design: &mut Design,
        path: &Path,
        text: &str,
    ) -> Result<(), String> {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !self.compiled.insert(canon) {
            return Ok(()); // already compiled this session
        }
        // VHDL: resolve the units named in context clauses (`use lib.pkg.all;`)
        // first, so their packages are on the design and visible while lowering.
        if Language::from_path(path) == Some(Language::Vhdl) {
            let id = self
                .db
                .add_file(path.display().to_string(), text.to_string())
                .map_err(|e| e.to_string())?;
            let parsed = hdl_syntax::vhdl::parser::parse_file(id, self.db.file(id));
            // `use lib.all;` (library-wide) pulls in every unit of that library.
            // `work` is skipped: it usually indexes the whole tree recursively,
            // and bulk-pulling every unit destabilizes compile order when unit
            // names collide across files (units still resolve by name on demand).
            let ctx_libs = hdl_ir::vhdl_lower::context_libraries(&self.db, &parsed.root);
            for lib in &ctx_libs {
                if lib == "work" {
                    continue;
                }
                for unit in self.library_unit_names(lib) {
                    if !design.packages.contains_key(&unit) {
                        if let Err(e) = self.resolve(design, &unit) {
                            if !self.unresolved_units.iter().any(|(u, _)| *u == unit) {
                                self.unresolved_units.push((unit, e));
                            }
                        }
                    }
                }
            }
            for unit in hdl_ir::vhdl_lower::context_units(&self.db, &parsed.root) {
                // A 2-part `use X.all;` where X names a (non-work) library was
                // handled above.
                if unit != "work" && ctx_libs.contains(&unit) && self.has_library(&unit) {
                    continue;
                }
                if !design.packages.contains_key(&unit) {
                    if let Err(e) = self.resolve(design, &unit) {
                        if !self.unresolved_units.iter().any(|(u, _)| *u == unit) {
                            self.unresolved_units.push((unit, e));
                        }
                    }
                }
            }
        }
        let cache = self.cache.as_ref();
        compile_one(
            path,
            text,
            &self.incdirs,
            &mut self.db,
            &mut self.diags,
            design,
            cache,
            &mut self.cache_hits,
            &mut self.pending_archs,
            &mut self.verilog_scope,
        )
    }

    /// Canonical paths of all files compiled this session, sorted — the
    /// transitive dependency closure of everything compiled so far (used by
    /// `hdlsim deps`).
    pub fn compiled_files(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.compiled.iter().cloned().collect();
        v.sort();
        v
    }

    /// Compile a list of explicit files (e.g. the CLI's file arguments).
    /// After all files are lowered, architectures whose entity lives in
    /// another file are bound design-wide (regardless of file order);
    /// architectures still naming an unknown entity produce E0401.
    pub fn compile_files(&mut self, design: &mut Design, paths: &[PathBuf]) {
        // Compile Verilog files that declare a `package` first, so `pkg::name`
        // references resolve no matter what order the files were given in.
        // Likewise VHDL package declarations before package bodies, so a body
        // in its own file sees the declaration's types/constants.
        let mut pkgs: Vec<&PathBuf> = Vec::new();
        let mut rest: Vec<&PathBuf> = Vec::new();
        for p in paths {
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            let text = std::fs::read_to_string(p).unwrap_or_default();
            let is_verilog_pkg = matches!(ext.as_deref(), Some("v") | Some("sv"))
                && text.lines().any(|l| l.trim_start().starts_with("package "));
            let is_vhdl_pkg_decl = matches!(ext.as_deref(), Some("vhd") | Some("vhdl"))
                && text.lines().any(|l| {
                    let t = l.trim_start();
                    t.starts_with("package ") && !t.starts_with("package body")
                });
            if is_verilog_pkg || is_vhdl_pkg_decl {
                pkgs.push(p);
            } else {
                rest.push(p);
            }
        }
        for p in pkgs.into_iter().chain(rest) {
            // The directory of an explicitly compiled file is a project root
            // for the out-of-the-box fallback search.
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    self.add_search_root(parent.to_path_buf());
                }
            }
            if let Err(e) = self.compile_file(design, p) {
                self.diags.push(Diagnostic::error("E0501", None, e));
            }
        }
        self.retry_pending_architectures(design, true);
        self.warn_unresolved_context_units(design);
    }

    /// Warn about `use`d library units that could not be resolved anywhere.
    /// A warning, not an error: the design may still elaborate fine if the
    /// unit is never actually referenced.
    fn warn_unresolved_context_units(&mut self, design: &Design) {
        let pending = std::mem::take(&mut self.unresolved_units);
        for (unit, msg) in pending {
            // A file compiled later in the list may have provided it.
            if design.packages.contains_key(&unit) {
                continue;
            }
            self.diags.push(Diagnostic::warning(
                "W0501",
                None,
                format!("`use` of unresolved library unit '{unit}' has no effect: {msg}"),
            ));
        }
    }

    /// Re-bind deferred architectures against the design-wide module list.
    /// With `emit_errors`, still-unknown entities produce E0401; otherwise the
    /// architectures stay pending (e.g. while elaboration resolves more units).
    fn retry_pending_architectures(&mut self, design: &mut Design, emit_errors: bool) {
        if self.pending_archs.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_archs);
        let mut lower_diags = Vec::new();
        let still = hdl_ir::vhdl_lower::lower_pending_architectures(
            &self.db,
            &mut lower_diags,
            &design.packages,
            &design.configurations,
            &mut design.modules,
            pending,
            emit_errors,
        );
        self.diags.extend(lower_diags);
        self.pending_archs = still;
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
        // Staged lookup: cheap file-stem index (no parsing) → in-memory
        // built-ins → full parse scan of directory providers → project search.
        self.ensure_stem_index();
        let lname = name.to_ascii_lowercase();
        if let Some(cands) = self.stem_index.get(&lname).cloned() {
            if self.progress {
                eprintln!("  resolving '{name}' via library index (file-name match)");
            }
            if self.try_candidates(design, name, &lname, cands)? {
                return Ok(true);
            }
        }
        self.ensure_index();
        let candidates = self.index.get(&lname).cloned();
        if candidates.is_none() {
            // Not in the stem index or the built-ins: fully scan (parse) the
            // directory providers once — needed when a unit's name differs
            // from its file name (multi-unit files).
            self.full_scan_dir_providers();
        }
        let candidates = self.index.get(&lname).cloned();
        if candidates.is_none() {
            // Out-of-the-box fallback: search the project roots (cwd, input
            // file dirs) for a file defining this unit.
            if let Some(path) = self.search_project(name) {
                if self.search_warned.insert(name.to_ascii_lowercase()) {
                    let root = self
                        .search_roots
                        .first()
                        .map(|r| r.display().to_string())
                        .unwrap_or_else(|| ".".to_string());
                    self.diags.push(Diagnostic::warning(
                        "W0502",
                        None,
                        format!(
                            "design unit '{name}' not in any library; found by searching the project ({root}) at {} — run `hdlsim init` to write hdlsim.ini so it is indexed directly next time",
                            path.display()
                        ),
                    ));
                }
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
                self.compile_text(design, &path, &text)?;
                self.retry_pending_architectures(design, false);
                if design.modules.iter().any(|m| m.name == name)
                    || design.packages.contains_key(&name.to_ascii_lowercase())
                {
                    return Ok(true);
                }
                return Ok(false);
            }
            let searched = self.library_names().join(", ");
            let roots = self
                .search_roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "design unit '{}' not found (searched libraries: {}; project roots: {}) — run `hdlsim init` to index the project into hdlsim.ini, or add `-L name=path`",
                name,
                if searched.is_empty() {
                    "<none>".to_string()
                } else {
                    searched
                },
                if roots.is_empty() { "<none>".to_string() } else { roots }
            ));
        }
        if let Some(cands) = candidates {
            return self.try_candidates(design, name, &lname, cands);
        }
        Ok(false)
    }
}

impl LibraryManager {
    /// Try one (provider, file) candidate: compile it and check the unit is now
    /// on the design (module or package).
    fn try_one(
        &mut self,
        design: &mut Design,
        name: &str,
        lname: &str,
        pi: usize,
        path: PathBuf,
    ) -> Result<bool, String> {
        let text = self
            .all_providers()
            .nth(pi)
            .and_then(|p| p.read(lname, &path));
        let Some(text) = text else {
            return Ok(false);
        };
        if self.progress {
            eprintln!("  compiling {} for unit '{name}'", path.display());
        }
        self.compile_text(design, &path, &text)?;
        self.retry_pending_architectures(design, false);
        Ok(design
            .modules
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case(name))
            || design.packages.contains_key(lname))
    }

    fn try_candidates(
        &mut self,
        design: &mut Design,
        name: &str,
        lname: &str,
        cands: Vec<(usize, PathBuf)>,
    ) -> Result<bool, String> {
        // Compile ALL candidates (duplicate unit names across files): the last
        // package registration wins, matching the historical behavior.
        let mut found = false;
        for (pi, path) in cands {
            if self.try_one(design, name, lname, pi, path)? {
                found = true;
            }
        }
        Ok(found)
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

/// Try the persistent parse cache for `text` (as file `id`). On a hit, bumps
/// `cache_hits` and returns the decoded tree, diagnostics, and timescale.
fn cached_parse(
    cache: Option<&cache::Cache>,
    path: &Path,
    id: hdl_syntax::source::FileId,
    text: &str,
    cache_hits: &mut u64,
) -> Option<(hdl_syntax::cst::SyntaxNode, Vec<Diagnostic>, Option<String>)> {
    let c = cache?;
    let payload = c.get(path, cache::hash64(text))?;
    let mut r = cache::Cursor::new(&payload);
    let timescale = match r.u8()? {
        0 => None,
        1 => Some(String::from_utf8(r.bytes()?.to_vec()).ok()?),
        _ => return None,
    };
    let root = hdl_syntax::cst::decode_tree(id, r.bytes()?)?;
    let diags = hdl_syntax::diagnostic::decode_diags(id, r.rest())?;
    *cache_hits += 1;
    Some((root, diags, timescale))
}

/// Store a parse result in the persistent cache (best-effort).
fn store_parse(
    cache: Option<&cache::Cache>,
    path: &Path,
    text: &str,
    root: &hdl_syntax::cst::SyntaxNode,
    diags: &[Diagnostic],
    timescale: Option<&str>,
) {
    let Some(c) = cache else { return };
    let mut p = Vec::new();
    match timescale {
        None => cache::w_u8(&mut p, 0),
        Some(t) => {
            cache::w_u8(&mut p, 1);
            cache::w_bytes(&mut p, t.as_bytes());
        }
    }
    cache::w_bytes(&mut p, &hdl_syntax::cst::encode_tree(root));
    p.extend_from_slice(&hdl_syntax::diagnostic::encode_diags(diags));
    c.put(path, cache::hash64(text), &p);
}

/// Compile one source file into `design` (preprocess → parse → lower), using
/// the persistent parse cache when `cache` is Some. The cache key is the hash
/// of the text that was actually parsed (post-preprocessor for Verilog).
pub fn compile_one(
    path: &Path,
    text: &str,
    incdirs: &[PathBuf],
    db: &mut SourceDb,
    diags: &mut Diagnostics,
    design: &mut Design,
    cache: Option<&cache::Cache>,
    cache_hits: &mut u64,
    pending_archs: &mut Vec<hdl_ir::vhdl_lower::PendingArch>,
    verilog_scope: &mut verilog_lower::VerilogScope,
) -> Result<(), String> {
    let lang = Language::from_path(path)
        .ok_or_else(|| format!("unknown file extension: {}", path.display()))?;
    match lang {
        Language::Verilog => {
            let raw_id = db
                .add_file(path.display().to_string(), text.to_string())
                .map_err(|e| e.to_string())?;
            let mut macros = HashMap::new();
            let mut pdiags = Vec::new();
            let expanded =
                preproc::preprocess(raw_id, db.file(raw_id), incdirs, &mut macros, &mut pdiags);
            diags.extend(pdiags);
            let id = db
                .add_file(path.display().to_string(), expanded.clone())
                .map_err(|e| e.to_string())?;
            let (root, parse_diags, timescale) =
                match cached_parse(cache, path, id, &expanded, cache_hits) {
                    Some(t) => t,
                    None => {
                        let parsed = parser::parse_file(id, db.file(id));
                        store_parse(
                            cache,
                            path,
                            &expanded,
                            &parsed.root,
                            &parsed.diags,
                            parsed.timescale.as_deref(),
                        );
                        (parsed.root, parsed.diags, parsed.timescale)
                    }
                };
            diags.extend(parse_diags);
            let time_unit_fs = timescale
                .as_deref()
                .map(parser::timescale_to_fs)
                .unwrap_or(1_000_000); // LRM default: 1 ns
            let mut lower_diags = Vec::new();
            let modules = verilog_lower::lower_file_with_time_scoped(
                id,
                &root,
                db,
                &mut lower_diags,
                time_unit_fs,
                verilog_scope,
            );
            diags.extend(lower_diags);
            design.modules.extend(modules);
        }
        Language::Vhdl => {
            let id = db
                .add_file(path.display().to_string(), text.to_string())
                .map_err(|e| e.to_string())?;
            let (root, parse_diags) = match cached_parse(cache, path, id, text, cache_hits) {
                Some((root, diags, _ts)) => (root, diags),
                None => {
                    let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
                    store_parse(cache, path, text, &parsed.root, &parsed.diags, None);
                    (parsed.root, parsed.diags)
                }
            };
            diags.extend(parse_diags);
            let mut lower_diags = Vec::new();
            let lowered = hdl_ir::vhdl_lower::lower_file(
                id,
                &root,
                db,
                &mut lower_diags,
                1_000_000,
                &design.packages,
                &design.configurations,
            );
            diags.extend(lower_diags);
            design.modules.extend(lowered.modules);
            pending_archs.extend(lowered.pending);
            for c in lowered.configurations {
                design
                    .configurations
                    .insert(c.name.to_ascii_lowercase(), std::rc::Rc::new(c));
            }
            for p in lowered.packages {
                // `package p` and `package body p` arrive as two entries with
                // the same name — merge their items instead of last-wins.
                match design.packages.entry(p.name.clone()) {
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        let e = std::rc::Rc::make_mut(e.get_mut());
                        e.types.extend(p.types);
                        e.consts.extend(p.consts);
                        e.functions.extend(p.functions);
                        if e.generics.is_empty() {
                            e.generics = p.generics;
                        }
                        e.uses.extend(p.uses);
                        if e.decl.is_none() {
                            e.decl = p.decl;
                        }
                        if e.src.is_none() {
                            e.src = p.src;
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(std::rc::Rc::new(p));
                    }
                }
            }
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
    fn search_roots_resolve_units_outside_libraries_with_hint() {
        let dir = std::env::temp_dir().join(format!("hdlsim_search_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rtl")).unwrap();
        std::fs::write(
            dir.join("rtl/my_pkg.vhd"),
            "package my_pkg is\n  constant WIDTH : natural := 8;\nend package;\n",
        )
        .unwrap();
        let mut mgr = LibraryManager::new();
        // No libraries registered; the project root is only a search root.
        mgr.add_search_root(&dir);
        let mut design = Design::default();
        // Miss: found by searching the project roots, with a W0502 hint.
        assert!(mgr.resolve(&mut design, "my_pkg").unwrap());
        assert!(design.packages.contains_key("my_pkg"));
        let warned = mgr
            .diags()
            .items
            .iter()
            .any(|d| d.code == "W0502" && d.message.contains("hdlsim init"));
        assert!(warned, "{:?}", mgr.diags().items);
        // Total miss: error names libraries, roots, and the remedy.
        let err = mgr.resolve(&mut design, "nope").unwrap_err();
        assert!(
            err.contains("nope") && err.contains("hdlsim init"),
            "{}",
            err
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cross_file_architecture_binds_regardless_of_order() {
        let dir = std::env::temp_dir().join(format!("hdlsim_xfile_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let entity = dir.join("ent.vhd");
        let arch = dir.join("arch.vhd");
        std::fs::write(
            &entity,
            "entity top is\n    port (clk : in bit; q : out bit);\nend entity;\n",
        )
        .unwrap();
        std::fs::write(
            &arch,
            "architecture rtl of top is\n    signal s : bit;\nbegin\n    q <= s;\nend architecture;\n",
        )
        .unwrap();
        // Architecture file first or second: binding must be order-independent.
        for paths in [
            vec![arch.clone(), entity.clone()],
            vec![entity.clone(), arch.clone()],
        ] {
            let mut mgr = LibraryManager::new();
            let mut design = Design::default();
            mgr.compile_files(&mut design, &paths);
            assert!(
                !mgr.diags().has_errors(),
                "{:?}: {}",
                paths,
                mgr.diags().render_all(mgr.db())
            );
            let m = design
                .modules
                .iter()
                .find(|m| m.name == "top")
                .expect("entity lowered");
            assert!(
                m.signals.iter().any(|s| s.name == "s"),
                "architecture body attached: {:?}",
                m.signals
            );
            assert_eq!(m.assigns.len(), 1, "concurrent assign attached");
        }
        // A genuinely unknown entity name still errors, with a did-you-mean.
        std::fs::write(
            &arch,
            "architecture rtl of topp is\nbegin\nend architecture;\n",
        )
        .unwrap();
        let mut mgr = LibraryManager::new();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &[arch.clone(), entity.clone()]);
        let rendered = mgr.diags().render_all(mgr.db());
        assert!(
            rendered.contains("architecture for unknown entity 'topp'"),
            "{}",
            rendered
        );
        assert!(rendered.contains("did you mean 'top'?"), "{}", rendered);
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
        // The (embedded) stdlib indexes package units, and they resolve +
        // compile cleanly through the real parse+lower pipeline — no disk
        // tree needed.
        let mut design = Design::default();
        for unit in [
            "standard",
            "env",
            "std_logic_1164",
            "numeric_std",
            "std_logic_arith",
            "std_logic_signed",
            "std_logic_unsigned",
            "std_logic_textio",
        ] {
            // Packages compile but produce no module, so resolve may return
            // Ok(false); what matters is that compilation is error-free.
            assert!(mgr.resolve(&mut design, unit).is_ok(), "{}", unit);
        }
        assert!(
            !mgr.diags().has_errors(),
            "{}",
            mgr.diags().render_all(mgr.db())
        );
    }

    #[test]
    fn parse_cache_hits_across_managers() {
        let dir = std::env::temp_dir().join(format!("hdlsim_cache_it_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("counter.v"),
            "module counter(input clk, output reg [7:0] q); always @(posedge clk) q <= q + 1; endmodule\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("tb.vhd"),
            "entity tb is end entity;\narchitecture sim of tb is\n  signal q : bit;\nbegin\n  q <= '1';\nend;\n",
        )
        .unwrap();
        let cdir = dir.join(".hdlsim-cache");

        // First manager (cold cache): parses and populates the cache.
        let mut m1 = LibraryManager::new();
        m1.enable_cache(cache::Cache::open(cdir.clone()).unwrap());
        let mut d1 = Design::default();
        m1.compile_file(&mut d1, &dir.join("counter.v")).unwrap();
        m1.compile_file(&mut d1, &dir.join("tb.vhd")).unwrap();
        assert!(d1.modules.iter().any(|m| m.name == "counter"));
        assert_eq!(m1.cache_hits(), 0);
        assert!(!m1.diags().has_errors());

        // Second manager (warm cache): same sources, parse skipped.
        let mut m2 = LibraryManager::new();
        m2.enable_cache(cache::Cache::open(cdir.clone()).unwrap());
        let mut d2 = Design::default();
        m2.compile_file(&mut d2, &dir.join("counter.v")).unwrap();
        m2.compile_file(&mut d2, &dir.join("tb.vhd")).unwrap();
        assert_eq!(m2.cache_hits(), 2, "both files should hit the cache");
        assert!(d2.modules.iter().any(|m| m.name == "counter"));
        assert!(!m2.diags().has_errors());

        // Edited source → hash changes → miss, re-parse.
        std::fs::write(
            dir.join("counter.v"),
            "module counter(input clk, output reg [7:0] q); always @(posedge clk) q <= q; endmodule\n",
        )
        .unwrap();
        let mut m3 = LibraryManager::new();
        m3.enable_cache(cache::Cache::open(cdir).unwrap());
        let mut d3 = Design::default();
        m3.compile_file(&mut d3, &dir.join("counter.v")).unwrap();
        assert_eq!(m3.cache_hits(), 0, "edited file must miss the cache");
        assert!(d3.modules.iter().any(|m| m.name == "counter"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
