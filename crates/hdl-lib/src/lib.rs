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

/// Heartbeat interval for the compile watchdog (`--progress-sec <s>`):
/// when > 0, `compile_files` spawns a watchdog that reports a compile
/// stage (parsing, dependency resolution, lowering) that outlives the
/// interval. Same knob as the simulator's heartbeat (`hdl-sim`):
/// one setting means "report long-running operations". Armed explicitly
/// by the CLI — environment variables have no spell check.
static PROGRESS_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Scanned candidates for one unit, grouped as (declaration files, body
/// files) in scan order — bodies must lower after their declaration.
type UnitCandidates = HashMap<String, (Vec<(usize, PathBuf)>, Vec<(usize, PathBuf)>)>;

/// Arm the compile watchdog heartbeat. Library users call this instead of
/// setting an environment variable; the CLI maps `--progress-sec` here.
pub fn set_progress_interval(d: std::time::Duration) {
    PROGRESS_MS.store(
        u64::try_from(d.as_millis()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn progress_interval() -> std::time::Duration {
    std::time::Duration::from_millis(PROGRESS_MS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Shared "what is the pipeline doing right now" cell plus a printer thread.
/// A single compile stage can legitimately run for minutes (one package body
/// lowering thousands of constants) with no code point to check a clock —
/// the thread observes the shared cell from the outside instead.
struct CompileWatch {
    shared: std::sync::Arc<std::sync::Mutex<(String, std::time::Instant)>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CompileWatch {
    fn start() -> Option<Self> {
        let interval = progress_interval();
        if interval.is_zero() {
            return None;
        }
        let shared = std::sync::Arc::new(std::sync::Mutex::new((
            String::new(),
            std::time::Instant::now(),
        )));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sh = shared.clone();
        let st = stop.clone();
        let handle = std::thread::spawn(move || {
            while !st.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(interval);
                if st.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let g = sh.lock().unwrap_or_else(|e| e.into_inner());
                if g.0.is_empty() {
                    continue;
                }
                let secs = g.1.elapsed().as_secs_f64();
                if secs >= interval.as_secs_f64() {
                    eprintln!("hdlsim: still {} for {secs:.0} s", g.0);
                }
            }
        });
        Some(Self {
            shared,
            stop,
            handle: Some(handle),
        })
    }

    fn mark(&self, what: String) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        *s = (what, std::time::Instant::now());
    }

    fn end(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

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
    /// Filesystem providers whose units were parse-scanned into `index` by
    /// `resolve_in_library` (one-time, per provider).
    lib_scanned: HashSet<usize>,
    /// Whether filesystem providers have been fully scanned (parsed) yet.
    dir_scanned: bool,
    /// Print indexing/resolution progress to stderr (`--progress`).
    progress: bool,
    /// Watchdog for long compile stages (`--progress-sec`): a thread
    /// printing "still <stage> for Ns" when one stage outlives the interval.
    watch: Option<CompileWatch>,
    /// Canonical paths already compiled this session.
    compiled: HashSet<PathBuf>,
    /// Persistent parse cache (`.hdlsim-cache/`); None unless enabled.
    cache: Option<cache::Cache>,
    /// Parse-cache hits this session.
    cache_hits: u64,
    /// VHDL architectures whose entity was not in the same file; re-bound
    /// design-wide once all known files are lowered (see `compile_files`).
    pending_archs: Vec<hdl_ir::vhdl_lower::PendingArch>,
    /// Entity name → root of the file that declared it, so pending
    /// architectures can inherit the entity's context clauses (LRM §13.3).
    entity_roots: HashMap<String, hdl_syntax::cst::SyntaxNode>,
    /// Entity name → signal maps + entity-declarative items, so pending
    /// architectures (entity in another file) see the entity's scope.
    entity_maps: HashMap<String, hdl_ir::vhdl_lower::SigMaps>,
    /// (source directory, lowercase entity name) → signal maps; disambiguates
    /// same-named entities from different directories (sibling binding).
    entity_maps_by_dir: HashMap<(String, String), hdl_ir::vhdl_lower::SigMaps>,
    /// Per-file lower-side object dims (constant/generic name → array
    /// dims [(low, size), …]), retained for `--dump-maps` diagnosis dumps.
    file_obj_dims: Vec<(PathBuf, hdl_ir::vhdl_lower::ObjDims)>,
    /// `use`d library units that failed to resolve while compiling a file:
    /// (unit name, resolve error). Warned about at the end of `compile_files`
    /// if still unresolved (a later explicit file may still provide them).
    unresolved_units: Vec<(String, String)>,
    /// (library, entity name) → index in `design.modules` of the module that
    /// library compiled for that entity. Architectures bind to their own
    /// library's entity when several libraries declare the same name.
    entity_module_of: HashMap<(String, String), usize>,
    /// (canonical source directory, entity name) → module index. Whole-tree
    /// libraries (`work = .`) hold same-named entities from several
    /// directories; sibling files (same dir) belong together.
    entity_module_by_dir: HashMap<(String, String), usize>,
    /// (canonical dir, file-stem key, entity name) → module index. Pins an
    /// architecture file to its own entity when several same-named entities
    /// share one directory (`reg-b.vhd` ↔ `reg.vhd`), see [`stem_key`].
    entity_module_by_stem: HashMap<(String, String, String), usize>,
    /// A `compile_files` batch is running: context-clause resolves triggered
    /// by one file must not bind cross-file architectures yet — the
    /// provenance maps only cover the files compiled so far. Binding waits
    /// for the batch's final pass.
    batch: bool,
    /// design-modules index → source file that lowered it. Variant
    /// re-lowering starts from the bound module's own file so same-named
    /// entities from other files (stem-index twins) cannot be picked.
    module_source: HashMap<usize, PathBuf>,
    /// Project roots searched on a resolve miss (cwd, dirs of input files):
    /// the "out of the box" fallback when no library maps the unit.
    search_roots: Vec<PathBuf>,
    /// Lazily built unit → file index over `search_roots` (built on first miss).
    search_index: Option<HashMap<String, Vec<(PathBuf, bool)>>>,
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
            lib_scanned: HashSet::new(),
            dir_scanned: false,
            progress: false,
            watch: None,
            compiled: HashSet::new(),
            cache: None,
            cache_hits: 0,
            pending_archs: Vec::new(),
            entity_roots: HashMap::new(),
            entity_maps: HashMap::new(),
            entity_maps_by_dir: HashMap::new(),
            file_obj_dims: Vec::new(),
            entity_module_of: HashMap::new(),
            entity_module_by_dir: HashMap::new(),
            entity_module_by_stem: HashMap::new(),
            batch: false,
            module_source: HashMap::new(),
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
        if !self.search_roots.contains(&p) {
            self.search_roots.push(p);
        }
    }

    /// Build the fallback index over the search roots (once), then look up
    /// `name`. Returns the file defining the unit, if the project has it.
    /// All project files defining `name`, declaration files first (a unit's
    /// declaration file wins over a file that only holds its body — compiling
    /// a body before its declaration lowers it without the declaration's items
    /// in scope). Duplicate unit names across files are all returned, matching
    /// the library-index behavior.
    fn search_project(&mut self, name: &str) -> Vec<PathBuf> {
        if self.search_index.is_none() {
            let mut idx: HashMap<String, Vec<(PathBuf, bool)>> = HashMap::new();
            for root in self.search_roots.clone() {
                for (unit, path, is_body) in providers::walk_sources_tagged(&root) {
                    let e = idx.entry(unit.to_ascii_lowercase()).or_default();
                    if is_body {
                        e.push((path, true));
                    } else {
                        e.insert(0, (path, false));
                    }
                }
            }
            self.search_index = Some(idx);
        }
        self.search_index
            .as_ref()
            .and_then(|i| i.get(&name.to_ascii_lowercase()))
            .map(|v| v.iter().map(|(p, _)| p.clone()).collect())
            .unwrap_or_default()
    }

    /// Enable the persistent parse cache in `c`.
    pub fn enable_cache(&mut self, c: cache::Cache) {
        self.cache = Some(c);
    }

    /// Enable the cache from explicit options (`--no-cache`,
    /// `--cache-dir`); library users opt in via
    /// [`LibraryManager::enable_cache`] with a hand-built [`cache::Cache`].
    pub fn use_cache_options(&mut self, no_cache: bool, dir: Option<PathBuf>) {
        self.cache = cache::Cache::from_options(no_cache, dir);
    }

    /// Parse-cache hits this session.
    pub fn cache_hits(&self) -> u64 {
        self.cache_hits
    }

    /// Per-entity lower-side signal maps retained during compilation
    /// (`SigId`s are module-relative); consumed by `hdlsim ir --dump-maps`.
    pub fn entity_maps(&self) -> &HashMap<String, hdl_ir::vhdl_lower::SigMaps> {
        &self.entity_maps
    }

    /// Per-file lower-side object dims (constant/generic name → array dims)
    /// retained during compilation; consumed by `hdlsim ir --dump-maps`.
    pub fn file_obj_dims(&self) -> &[(PathBuf, hdl_ir::vhdl_lower::ObjDims)] {
        &self.file_obj_dims
    }

    /// Add a provider at the end of the search path.
    pub fn add_provider(&mut self, p: Box<dyn UnitProvider>) {
        self.providers.push(p);
        self.indexed = false;
    }

    /// Add a named filesystem library directory (Verilog + VHDL sources).
    pub fn add_library_dir(&mut self, name: &str, path: &Path) {
        self.add_provider(Box::new(DirProvider::mixed(name, path)));
        self.lib_scanned.clear();
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

    /// Unit names of a filesystem provider's files, going through the
    /// persistent unit-index cache when enabled (see [`Self::units_in_file_cached`]).
    /// In-memory providers (no files) scan directly as before.
    fn scan_provider_tagged(&self, p: &dyn UnitProvider) -> Vec<(String, PathBuf, bool)> {
        let files = p.source_files();
        if files.is_empty() {
            return p.scan_tagged();
        }
        let mut out = Vec::new();
        for f in files {
            for (u, b) in self.units_in_file_cached(&f) {
                out.push((u, f.clone(), b));
            }
        }
        out
    }

    /// Design units of one file via the unit-index cache when enabled: the
    /// scan result is content-addressed, so the full-parse scan of a library
    /// directory is paid once per file ever — shared across managers and
    /// processes (the corpus compiles ~100 groups against the same core
    /// libraries; without the cache each group re-parses them for indexing).
    fn units_in_file_cached(&self, path: &Path) -> Vec<(String, bool)> {
        let Some(c) = self.cache.as_ref() else {
            return providers::units_in_file_tagged(path);
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let h = cache::hash64(&text);
        if let Some(units) = c.get_units(path, h) {
            return units;
        }
        let lang = match Language::from_path(path) {
            Some(l) => l,
            None => return Vec::new(),
        };
        let units = providers::units_in_text_tagged(lang, path, &text);
        c.put_units(path, h, &units);
        units
    }

    /// Fully scan (parse) the filesystem providers once, on a lookup miss —
    /// needed when a unit's name differs from its file name (multi-unit files).
    /// Providers already content-scanned via `resolve_in_library`
    /// (`lib_scanned`) are skipped and their results are shared, so the same
    /// library is never parsed twice in one session.
    fn full_scan_dir_providers(&mut self) {
        if self.dir_scanned {
            return;
        }
        self.dir_scanned = true;
        let scanned: Vec<(usize, String, PathBuf, bool)> = self
            .all_providers()
            .enumerate()
            .filter(|(_, p)| !p.source_files().is_empty()) // filesystem providers
            .filter(|(i, _)| !self.lib_scanned.contains(i))
            .flat_map(|(i, p)| {
                if self.progress {
                    eprintln!("  scanning library '{}' for unit names…", p.name());
                }
                self.scan_provider_tagged(p.as_ref())
                    .into_iter()
                    .map(move |(u, f, b)| (i, u, f, b))
            })
            .collect();
        // Declaration files first: candidates are compiled in order, and a
        // package body must be lowered after its declaration is registered.
        let mut grouped: UnitCandidates = HashMap::new();
        for (i, unit, file, is_body) in scanned {
            let e = grouped.entry(unit.to_ascii_lowercase()).or_default();
            if is_body {
                e.1.push((i, file));
            } else {
                e.0.push((i, file));
            }
        }
        for (unit, (mut decls, mut bodies)) in grouped {
            decls.append(&mut bodies);
            // Extend, don't replace: `ensure_index` already put the in-memory
            // (embedded stdlib) candidates under this unit name.
            self.index.entry(unit).or_default().extend(decls);
        }
        // Everything content-scanned here also counts as scanned for the
        // per-library path.
        let n = self.all_providers().count();
        for i in 0..n {
            self.lib_scanned.insert(i);
        }
    }

    /// Parse-scan one filesystem provider's files for unit names and add the
    /// results to `index` (declarations before bodies). The per-library
    /// variant of `full_scan_dir_providers`, used by `resolve_in_library` so
    /// `use lib.unit` can look inside multi-unit files of just `lib`.
    fn scan_provider_into_index(&mut self, pi: usize) {
        // Collect the scan under an immutable borrow, then index mutably.
        let scanned: Vec<(String, PathBuf, bool)> = {
            let p = self
                .all_providers()
                .nth(pi)
                .expect("pi indexes into the same providers list");
            if p.source_files().is_empty() {
                Vec::new() // in-memory providers are indexed eagerly already
            } else {
                if self.progress {
                    eprintln!("  scanning library '{}' for unit names…", p.name());
                }
                self.scan_provider_tagged(p.as_ref())
            }
        };
        // Declarations first per unit: candidates are compiled in order, and
        // a package body must be lowered after its declaration is registered
        // (same invariant as `full_scan_dir_providers`).
        let mut grouped: UnitCandidates = HashMap::new();
        for (unit, file, is_body) in scanned {
            let e = grouped.entry(unit.to_ascii_lowercase()).or_default();
            if is_body {
                e.1.push((pi, file));
            } else {
                e.0.push((pi, file));
            }
        }
        for (unit, (mut decls, mut bodies)) in grouped {
            decls.append(&mut bodies);
            self.index.entry(unit).or_default().extend(decls);
        }
        self.lib_scanned.insert(pi);
    }

    /// Resolve `use lib.unit...` inside the named library (LRM 11.2): try
    /// that library's own files first — stem candidates, in-memory index
    /// entries, then a one-time content scan of the library — so units in
    /// other libraries (vendor trees mapped under different names, built-ins)
    /// cannot shadow it. Returns true when the unit is on the design after
    /// the scoped attempt; false means the caller should fall back to the
    /// global search (the library is unknown, or lacks the unit).
    fn resolve_in_library(
        &mut self,
        design: &mut Design,
        name: &str,
        lname: &str,
        lib: &str,
    ) -> Result<bool, String> {
        let Some(pi) = self
            .all_providers()
            .position(|p| p.name().eq_ignore_ascii_case(lib))
        else {
            return Ok(false);
        };
        self.ensure_stem_index();
        let stem_cands: Vec<(usize, PathBuf)> = self
            .stem_index
            .get(lname)
            .map(|v| v.iter().filter(|(i, _)| *i == pi).cloned().collect())
            .unwrap_or_default();
        if !stem_cands.is_empty() && self.try_candidates(design, name, lname, stem_cands)? {
            return Ok(true);
        }
        // In-memory (built-in) providers are indexed eagerly; directory
        // providers land here after their content scan below.
        self.ensure_index();
        let idx_cands: Vec<(usize, PathBuf)> = self
            .index
            .get(lname)
            .map(|v| v.iter().filter(|(i, _)| *i == pi).cloned().collect())
            .unwrap_or_default();
        if !idx_cands.is_empty() && self.try_candidates(design, name, lname, idx_cands)? {
            return Ok(true);
        }
        // Last scoped chance: a unit whose name differs from its file name
        // (multi-unit files) — parse-scan this library once.
        if !self.lib_scanned.contains(&pi) {
            self.scan_provider_into_index(pi);
            let scan_cands: Vec<(usize, PathBuf)> = self
                .index
                .get(lname)
                .map(|v| v.iter().filter(|(i, _)| *i == pi).cloned().collect())
                .unwrap_or_default();
            if !scan_cands.is_empty() && self.try_candidates(design, name, lname, scan_cands)? {
                return Ok(true);
            }
        }
        Ok(false)
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
            if let Some(w) = &self.watch {
                w.mark(format!("resolving context clauses of {}", path.display()));
            }
            let id = self
                .db
                .add_file(path.display().to_string(), text.to_string())
                .map_err(|e| e.to_string())?;
            // Parse through the persistent cache when enabled, so the
            // compile pass below hits it instead of re-parsing the file.
            let (root, _parse_diags, _) =
                match cached_parse(self.cache.as_ref(), path, id, text, &mut self.cache_hits) {
                    Some(t) => t,
                    None => {
                        let parsed = hdl_syntax::vhdl::parser::parse_file(id, self.db.file(id));
                        store_parse(
                            self.cache.as_ref(),
                            path,
                            text,
                            &parsed.root,
                            &parsed.diags,
                            None,
                        );
                        (parsed.root, parsed.diags, None)
                    }
                };
            // `use lib.all;` (library-wide) pulls in every unit of that library.
            // `work` is skipped: it usually indexes the whole tree recursively,
            // and bulk-pulling every unit destabilizes compile order when unit
            // names collide across files (units still resolve by name on demand).
            let ctx_libs = hdl_ir::vhdl_lower::context_libraries(&self.db, &root);
            for lib in &ctx_libs {
                if lib == "work" {
                    continue;
                }
                for unit in self.library_unit_names(lib) {
                    if !design.packages.contains_key(&unit) {
                        // The units of `lib` live in `lib` — resolve there
                        // directly (falls back to the global search when the
                        // library lacks the unit file).
                        let resolved = self.resolve_in_library(design, &unit, &unit, lib)?;
                        if !resolved {
                            if let Err(e) = self.resolve(design, &unit) {
                                if !self.unresolved_units.iter().any(|(u, _)| *u == unit) {
                                    self.unresolved_units.push((unit, e));
                                }
                            }
                        }
                    }
                }
            }
            // Package instantiation targets anywhere in the file (incl. nested
            // in declarative parts) resolve library-only — see resolve_opts.
            // Units the file declares itself are skipped (pass 0 lowers them,
            // and resolving them externally could pull in a same-named unit
            // from another file).
            let inst_units = hdl_ir::vhdl_lower::package_inst_target_units(&self.db, &root);
            let local_units = hdl_ir::vhdl_lower::declared_unit_names(&self.db, &root);
            for unit in &inst_units {
                if local_units.contains(unit) {
                    continue;
                }
                if let Err(e) = self.resolve_opts(design, unit, false) {
                    if !self.unresolved_units.iter().any(|(u, _)| u == unit) {
                        self.unresolved_units.push((unit.clone(), e));
                    }
                }
            }
            for (unit, lib) in hdl_ir::vhdl_lower::context_unit_libs(&self.db, &root) {
                if inst_units.contains(&unit) {
                    continue;
                }
                // LRM 11.2: `use lib.unit` names the unit in library `lib` —
                // resolve inside `lib` first so units in other libraries
                // (vendor trees mapped under different names) cannot shadow
                // it. Falls back to the global search when `lib` is unknown
                // or lacks the unit.
                let mut resolved = false;
                if let Some(lib) = &lib {
                    resolved = self.resolve_in_library(design, &unit, &unit, lib)?;
                }
                if !resolved {
                    // Resolve even when a same-named unit is already on the
                    // design: duplicate units across files must ALL be
                    // compiled so their items merge (try_one skips
                    // already-compiled candidates).
                    if let Err(e) = self.resolve(design, &unit) {
                        if !self.unresolved_units.iter().any(|(u, _)| *u == unit) {
                            self.unresolved_units.push((unit, e));
                        }
                    }
                }
            }
        }
        let cache = self.cache.as_ref();
        if let Some(w) = &self.watch {
            w.mark(format!("lowering {}", path.display()));
        }
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
            &mut self.entity_roots,
            &mut self.entity_maps,
            &mut self.entity_maps_by_dir,
            &mut self.file_obj_dims,
            &mut self.entity_module_by_dir,
            &mut self.entity_module_by_stem,
            &mut self.module_source,
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
        // Cross-file architecture binding defers to the final pass: with the
        // batch still running, a file's context-clause resolves must not bind
        // pending architectures against incomplete provenance (later files
        // may declare another same-named entity).
        self.batch = true;
        let watch = CompileWatch::start();
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
            if let Some(w) = &watch {
                w.mark(format!("compiling {}", p.display()));
            }
            if let Err(e) = self.compile_file(design, p) {
                self.diags.push(Diagnostic::error("E0501", None, e));
            }
        }
        self.batch = false;
        self.retry_pending_architectures(design, true);
        self.warn_unresolved_context_units(design);
        if let Some(mut w) = watch {
            w.end();
        }
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
            &design.pkg_funcs,
            &design.configurations,
            &mut design.modules,
            pending,
            emit_errors,
            // Mid-batch retries (context-clause resolves) must not bind
            // against incomplete provenance; only the final batch pass and
            // single-file compile paths bind eagerly.
            !self.batch,
            &self.entity_roots,
            &self.entity_maps,
            &self.entity_maps_by_dir,
            &self.entity_module_of,
            &self.entity_module_by_dir,
            &self.entity_module_by_stem,
        );
        self.diags.extend(lower_diags);
        self.pending_archs = still;
    }

    /// Stamp pending architectures whose source file is `path` with the
    /// library that just compiled it (`UnitProvider::name()`), so the binding
    /// step can pick the entity from that library.
    fn stamp_pending_libs(&mut self, path: &Path, lib: &str) {
        for p in self.pending_archs.iter_mut() {
            if p.lib.is_some() {
                continue;
            }
            let fid = p.root.span().file;
            let from_here = {
                let f = self.db.file(fid);
                Path::new(f.name()) == path
            };
            if from_here {
                p.lib = Some(lib.to_string());
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
    /// VHDL `generic map` variant: re-lower the entity (and its architectures,
    /// including ones split into sibling `*_behavior.vhd`/`*_rtl.vhd` files)
    /// with the constant overrides applied, registered as `<entity>#<k=v,...>`
    /// modules, so elaboration binds the child's generics (generate ranges,
    /// port widths, ...). `unit` may be `entity` or `entity/arch`.
    fn resolve_variant(
        &mut self,
        design: &mut Design,
        unit: &str,
        params: &HashMap<String, hdl_ir::Value>,
        bound: usize,
    ) -> Result<Option<String>, String> {
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
        if design.modules.iter().any(|m| m.name == want) {
            return Ok(Some(want));
        }
        self.ensure_stem_index();
        let mut cands: Vec<PathBuf> = self
            .stem_index
            .get(&lname)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        if cands.is_empty() {
            self.ensure_index();
            cands = self
                .index
                .get(&lname)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(_, p)| p)
                .collect();
        }
        if cands.is_empty() {
            cands = self.search_project(&lname);
        } else {
            // The stem index keys raw file stems: a sibling architecture file
            // (`foo_rtl.vhd` for `foo_entity.vhd`) never matches the unit name,
            // so a stem-index hit alone would re-lower the variant entity
            // without its architectures (an empty module). Merge the unit-name
            // search index — extra candidates are skipped by the unit check
            // below, and the entity file itself comes from `module_source`.
            for p in self.search_project(&lname) {
                if !cands.contains(&p) {
                    cands.push(p);
                }
            }
        }
        // Classify the candidates: the entity's declaration file plus the
        // sibling files holding its architectures (this codebase splits
        // `*_entity.vhd` / `*_behavior.vhd` into separate files).
        //
        // The entity file is the source file of the module the instantiation
        // actually bound to (`bound`) when known: whole-tree libraries hold
        // same-named entities from several files, and stem order would pick
        // an unrelated twin (whose ports/generics differ). The stem-index
        // scan remains the fallback for modules without provenance (e.g.
        // previously created variants).
        let mut ent_file: Option<(PathBuf, String)> = None;
        let mut arch_files: Vec<(PathBuf, String)> = Vec::new();
        if let Some(bpath) = self.module_source.get(&bound) {
            if let Ok(text) = std::fs::read_to_string(bpath) {
                let units = crate::providers::vhdl_units_in_file(bpath, &text);
                if units
                    .iter()
                    .any(|(n, b)| n.eq_ignore_ascii_case(&lname) && !*b)
                {
                    ent_file = Some((bpath.clone(), text));
                }
            }
        }
        for path in cands {
            // VHDL variants only — a Verilog `#(...)` override keeps the
            // default-parameter behavior.
            if !matches!(
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .as_deref(),
                Some("vhd") | Some("vhdl")
            ) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let units = crate::providers::vhdl_units_in_file(&path, &text);
            if !units.iter().any(|(n, _)| n.eq_ignore_ascii_case(&lname)) {
                continue;
            }
            // `(name, false)` = entity declaration, `(name, true)` = an
            // architecture of it (see vhdl_units_in_file).
            let has_ent = units
                .iter()
                .any(|(n, b)| n.eq_ignore_ascii_case(&lname) && !*b);
            let has_arch = units
                .iter()
                .any(|(n, b)| n.eq_ignore_ascii_case(&lname) && *b);
            if has_ent {
                if ent_file.is_none() {
                    ent_file = Some((path, text));
                }
            } else if has_arch {
                arch_files.push((path, text));
            }
        }
        let Some((epath, etext)) = ent_file else {
            return Ok(None);
        };
        let Ok(eid) = self.db.add_file(epath.display().to_string(), etext) else {
            return Ok(None);
        };
        let eparsed = hdl_syntax::vhdl::parser::parse_file(eid, self.db.file(eid));
        let mut arch_roots: Vec<hdl_syntax::cst::SyntaxNode> = Vec::new();
        let edir = epath.parent();
        // Canonicalized directory of the entity file: CLI paths ("tb.vhd") and
        // search results ("./tb.vhd") yield different raw parents ("" vs "."),
        // which would wrongly fail the sibling check below.
        let edir_c = std::fs::canonicalize(&epath)
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .or_else(|| edir.map(|d| d.to_path_buf()));
        for (apath, atext) in arch_files {
            // Sibling files only: a same-named entity in another directory
            // must not bind its architectures to this one.
            let adir = std::fs::canonicalize(&apath)
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .or_else(|| apath.parent().map(|d| d.to_path_buf()));
            if adir != edir_c {
                continue;
            }
            let Ok(aid) = self.db.add_file(apath.display().to_string(), atext) else {
                continue;
            };
            let aparsed = hdl_syntax::vhdl::parser::parse_file(aid, self.db.file(aid));
            arch_roots.push(aparsed.root);
        }
        let mut lower_diags = Vec::new();
        let modules = hdl_ir::vhdl_lower::lower_entity_variant(
            &self.db,
            &mut lower_diags,
            &design.packages,
            &design.pkg_funcs,
            &design.configurations,
            &eparsed.root,
            &arch_roots,
            &lname,
            params,
            &suffix,
        );
        self.diags.extend(lower_diags);
        if modules.is_empty() {
            return Ok(None);
        }
        design.modules.extend(modules);
        if design.modules.iter().any(|m| m.name == want) {
            return Ok(Some(want));
        }
        if arch.is_none() && design.modules.iter().any(|m| m.name == vbase) {
            return Ok(Some(vbase));
        }
        Ok(None)
    }

    fn resolve(&mut self, design: &mut Design, name: &str) -> Result<bool, String> {
        self.resolve_opts(design, name, true)
    }
}

impl LibraryManager {
    /// `resolve` with control over the project-search fallback. Package
    /// instantiation targets (`package p is new lib.g ...`) are resolved
    /// library-only: a project-wide search would pull in unrelated same-named
    /// units from the source tree (e.g. other testbenches' fixtures).
    fn resolve_opts(
        &mut self,
        design: &mut Design,
        name: &str,
        allow_project_search: bool,
    ) -> Result<bool, String> {
        // VHDL instantiation module names are `entity/arch` (the IR naming);
        // the units to load are the entity's and its architectures' files, so
        // look up the entity part only. Success still requires the full
        // module on the design.
        let lname = name.to_ascii_lowercase();
        let (base, want_arch) = match lname.split_once('/') {
            Some((b, a)) => (b.to_string(), Some(a.to_string())),
            None => (lname.clone(), None),
        };
        // Staged lookup: cheap file-stem index (no parsing) → in-memory
        // built-ins → full parse scan of directory providers → project search.
        self.ensure_stem_index();
        if let Some(cands) = self.stem_index.get(&base).cloned() {
            if self.progress {
                eprintln!("  resolving '{name}' via library index (file-name match)");
            }
            if self.try_candidates(design, &base, &base, cands)? {
                return Ok(true);
            }
        }
        self.ensure_index();
        let candidates = self.index.get(&base).cloned();
        if candidates.is_none() {
            // Not in the stem index or the built-ins: fully scan (parse) the
            // directory providers once — needed when a unit's name differs
            // from its file name (multi-unit files).
            self.full_scan_dir_providers();
        }
        let candidates = self.index.get(&base).cloned();
        if candidates.is_none() {
            // Out-of-the-box fallback: search the project roots (cwd, input
            // file dirs) for a file defining this unit.
            let paths = if allow_project_search {
                self.search_project(&base)
            } else {
                Vec::new()
            };
            if !paths.is_empty() {
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
                            paths[0].display()
                        ),
                    ));
                }
                for path in &paths {
                    let text = std::fs::read_to_string(path)
                        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
                    self.compile_text(design, path, &text)?;
                }
                self.retry_pending_architectures(design, false);
                if self.unit_available(design, &base, want_arch.as_deref()) {
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
            return self.try_candidates(design, &base, &base, cands);
        }
        Ok(false)
    }
}

impl LibraryManager {
    /// True when the requested unit is on the design: a package with that
    /// name, a module named `base` (entity without architecture selected), or
    /// — for `entity/arch` requests — the bound `entity/arch` module.
    fn unit_available(&self, design: &Design, base: &str, want_arch: Option<&str>) -> bool {
        if design.packages.contains_key(base) {
            return true;
        }
        match want_arch {
            Some(a) => {
                let want = format!("{base}/{a}");
                design.modules.iter().any(|m| m.name == want)
                    || design.modules.iter().any(|m| m.name == base)
            }
            None => design.modules.iter().any(|m| m.name == base),
        }
    }

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
        // Already compiled (possibly as another duplicate candidate): skip
        // without re-reading the file.
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if self.compiled.contains(&canon) {
            return Ok(design
                .modules
                .iter()
                .any(|m| m.name.eq_ignore_ascii_case(name))
                || design.packages.contains_key(lname));
        }
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
        let lib = self
            .all_providers()
            .nth(pi)
            .map(|p| p.name().to_string())
            .unwrap_or_else(|| "work".to_string());
        let first_new_module = design.modules.len();
        self.compile_text(design, &path, &text)?;
        // Record which design module this provider compiled for each entity
        // it declares (library-keyed; the directory-keyed twin is recorded in
        // compile_one for every compile path), and stamp this file's pending
        // architectures with the library. The binding step prefers the
        // sibling-directory entity, then same-library.
        for (i, m) in design.modules.iter().enumerate().skip(first_new_module) {
            self.entity_module_of
                .insert((lib.to_ascii_lowercase(), m.name.to_ascii_lowercase()), i);
            self.module_source.insert(i, path.clone());
        }
        self.stamp_pending_libs(&path, &lib);
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
// One file's lower-and-register step threads every accumulator the
// library manager keeps (entity roots/maps by name and by dir, module
// slots, obj dims, module sources, the Verilog scope); grouping them
// into a struct is the follow-up if this grows again.
#[allow(clippy::too_many_arguments)]
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
    entity_roots: &mut HashMap<String, hdl_syntax::cst::SyntaxNode>,
    entity_maps: &mut HashMap<String, hdl_ir::vhdl_lower::SigMaps>,
    entity_maps_by_dir: &mut HashMap<(String, String), hdl_ir::vhdl_lower::SigMaps>,
    file_obj_dims: &mut Vec<(PathBuf, hdl_ir::vhdl_lower::ObjDims)>,
    entity_module_by_dir: &mut HashMap<(String, String), usize>,
    entity_module_by_stem: &mut HashMap<(String, String, String), usize>,
    module_source: &mut HashMap<usize, PathBuf>,
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
                &design.pkg_funcs,
                &design.configurations,
            );
            diags.extend(lower_diags);
            // Record which module each entity in this file produced, keyed by
            // the file's source directory — sibling architectures bind to
            // their own directory's entity when several libraries (or a
            // whole-tree `work` library) declare same-named entities. The
            // LAST same-named entity in a directory wins: an architecture
            // file compiled after several same-named entities belongs to the
            // entity closest before it (first-wins would bind it to an
            // unrelated earlier twin).
            if let Some(dir) = std::fs::canonicalize(path)
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
            {
                let base = design.modules.len();
                for (i, m) in lowered.modules.iter().enumerate() {
                    let lname = m.name.to_ascii_lowercase();
                    entity_module_by_dir.insert((dir.clone(), lname.clone()), base + i);
                    // Sibling-file convention: `reg-b.vhd` binds to `reg.vhd`
                    // even when several entities named `reg` share the dir.
                    let fname = std::path::Path::new(path)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_default();
                    entity_module_by_stem.insert(
                        (dir.clone(), hdl_ir::vhdl_lower::stem_key(&fname), lname),
                        base + i,
                    );
                }
            }
            let mbase = design.modules.len();
            let new_modules = lowered.modules.len();
            design.modules.extend(lowered.modules);
            // Stamp each new module with its source file so variant
            // re-lowering (generic-map overrides at elaboration) re-lowers
            // the bound module's own entity file, not a stem-index twin.
            for i in 0..new_modules {
                module_source.insert(mbase + i, path.to_path_buf());
            }
            pending_archs.extend(lowered.pending);
            for (name, root) in lowered.entity_roots {
                entity_roots.insert(name, root);
            }
            for (name, m) in lowered.entity_maps {
                entity_maps.insert(name, m);
            }
            for ((dir, name), m) in lowered.entity_maps_by_dir {
                entity_maps_by_dir.insert((dir, name), m);
            }
            file_obj_dims.push((path.to_path_buf(), lowered.obj_dims));
            for c in lowered.configurations {
                design
                    .configurations
                    .insert(c.name.to_ascii_lowercase(), std::rc::Rc::new(c));
            }
            for p in lowered.packages {
                // Keep the design-wide const-eval fallback (`pkg_funcs`) in
                // sync with `design.packages`: the delta to the entry's
                // functions is `p.functions` in both the insert and the
                // merge case. Building it here — once per package arrival —
                // replaces a per-file rebuild of the whole package closure
                // in the lowerer, which was quadratic in design size.
                {
                    let funcs = std::rc::Rc::make_mut(&mut design.pkg_funcs);
                    for f in &p.functions {
                        funcs
                            .entry(f.name.to_ascii_lowercase())
                            .or_default()
                            .push(std::rc::Rc::new(f.clone()));
                    }
                }
                // `package p` and `package body p` arrive as two entries with
                // the same name — merge their items instead of last-wins.
                match design.packages.entry(p.name.clone()) {
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        let e = std::rc::Rc::make_mut(e.get_mut());
                        e.types.extend(p.types);
                        e.consts.extend(p.consts);
                        e.functions.extend(p.functions);
                        e.rec_field_tys.extend(p.rec_field_tys);
                        for (n, pt) in p.prot_tys {
                            // Body entry (state fields + initializers) wins
                            // over the declaration's.
                            match e.prot_tys.iter_mut().find(|(en, _)| *en == n) {
                                Some(entry) => entry.1 = pt,
                                None => e.prot_tys.push((n, pt)),
                            }
                        }
                        e.sig_types.extend(p.sig_types);
                        e.sig_widths.extend(p.sig_widths);
                        e.const_tys.extend(p.const_tys);
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
    fn package_body_file_does_not_shadow_declaration_when_resolving() {
        let dir = std::env::temp_dir().join(format!("hdlsim_bodydecl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The body file sorts BEFORE the declaration file: a naive first-wins
        // index would compile the body first, without the declaration in scope.
        std::fs::write(
            dir.join("a_my_pkg-body.vhd"),
            "package body my_pkg is\n  constant C : natural := WIDTH;\nend package body;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("z_my_pkg.vhd"),
            "package my_pkg is\n  constant WIDTH : natural := 8;\nend package;\n",
        )
        .unwrap();
        let mut mgr = LibraryManager::new();
        mgr.add_search_root(&dir);
        let mut design = Design::default();
        assert!(mgr.resolve(&mut design, "my_pkg").unwrap());
        let rendered = mgr.diags().render_all(mgr.db());
        assert!(
            !mgr.diags().has_errors(),
            "body compiled before decl: {}",
            rendered
        );
        // The project-search hit must name the declaration file, not the body.
        assert!(rendered.contains("z_my_pkg.vhd"), "{}", rendered);
        assert!(!rendered.contains("a_my_pkg-body.vhd"), "{}", rendered);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_declaration_applies_via_context_reference() {
        let dir = std::env::temp_dir().join(format!("hdlsim_ctxdecl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("my_pkg.vhd"),
            "package my_pkg is\n  constant WIDTH : natural := 8;\nend package;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("my_context.vhd"),
            "context my_context is\n  use work.my_pkg.all;\nend context;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("top.vhd"),
            "context work.my_context;\nentity top is\n  port (q : out natural);\nend entity;\narchitecture rtl of top is\nbegin\n  q <= WIDTH;\nend architecture;\n",
        )
        .unwrap();
        let mut mgr = LibraryManager::new();
        mgr.add_library_dir("work", &dir);
        let mut design = Design::default();
        let paths: Vec<PathBuf> = ["my_pkg.vhd", "my_context.vhd", "top.vhd"]
            .iter()
            .map(|f| dir.join(f))
            .collect();
        mgr.compile_files(&mut design, &paths);
        let rendered = mgr.diags().render_all(mgr.db());
        assert!(!mgr.diags().has_errors(), "{}", rendered);
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
        // Cold cache: the VHDL file is parsed once by the context-clause
        // pre-pass (which stores it) and its compile pass then hits the
        // cache; the Verilog file has no pre-pass and parses fresh.
        assert_eq!(m1.cache_hits(), 1);
        assert!(!m1.diags().has_errors());

        // Second manager (warm cache): same sources, parse skipped.
        let mut m2 = LibraryManager::new();
        m2.enable_cache(cache::Cache::open(cdir.clone()).unwrap());
        let mut d2 = Design::default();
        m2.compile_file(&mut d2, &dir.join("counter.v")).unwrap();
        m2.compile_file(&mut d2, &dir.join("tb.vhd")).unwrap();
        // Warm cache: counter.v's compile pass hits once; tb.vhd's
        // context-clause pre-pass and its compile pass both hit.
        assert_eq!(m2.cache_hits(), 3, "both files should hit the cache");
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
