//! Compile pipeline wrapper: source files → `Design`, via `hdl-lib`'s
//! `LibraryManager`. The full library-aware path (config files, `-L` mappings,
//! on-demand dependency resolution) is built directly on `LibraryManager` in
//! `commands.rs`; this helper is the minimal single-shot path used when no
//! libraries are involved.

use hdl_ir::Design;
use hdl_lib::LibraryManager;
use hdl_syntax::diagnostic::Diagnostics;
use hdl_syntax::source::SourceDb;
use std::path::PathBuf;

pub struct Compiled {
    pub design: Design,
    pub db: SourceDb,
    pub diags: Diagnostics,
}

impl Compiled {
    pub fn has_errors(&self) -> bool {
        self.diags.has_errors()
    }

    pub fn render_diags(&self) -> String {
        self.diags.render_all(&self.db)
    }
}

/// Compile the given files into one Design (no library search).
pub fn compile_paths(paths: &[PathBuf]) -> Compiled {
    let mut mgr = LibraryManager::new();
    let mut design = Design::default();
    mgr.compile_files(&mut design, paths);
    Compiled {
        design,
        db: mgr.take_db(),
        diags: mgr.take_diags(),
    }
}
