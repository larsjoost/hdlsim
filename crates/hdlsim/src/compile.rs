//! Compile pipeline: source files → parsed → lowered → `Design`.
//!
//! Phase 1 handles Verilog files; VHDL files join once the VHDL front-end lands.
//! (The full library-manager version of this lives in `hdl-lib`; this is the
//! minimal single-shot path used by `run`/`test` until then.)

use hdl_ir::{verilog_lower, Design};
use hdl_syntax::diagnostic::{Diagnostic, Diagnostics};
use hdl_syntax::language::Language;
use hdl_syntax::source::SourceDb;
use hdl_syntax::verilog::{parser, preproc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// Compile the given files into one Design. Verilog files only (phase 1).
pub fn compile_paths(paths: &[PathBuf]) -> Compiled {
    let mut db = SourceDb::new();
    let mut diags = Diagnostics::new();
    let mut design = Design::default();
    for path in paths {
        if let Err(e) = compile_one(path, &mut db, &mut diags, &mut design) {
            diags.push(Diagnostic::error("E0501", None, e));
        }
    }
    Compiled { design, db, diags }
}

fn compile_one(
    path: &Path,
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
            // Preprocess (macros/includes), then register the *expanded* text so
            // spans line up with what the parser saw.
            let raw_id = db
                .add_file(path.display().to_string(), text.clone())
                .map_err(|e| e)?;
            let mut macros = HashMap::new();
            let mut pdiags = Vec::new();
            let expanded =
                preproc::preprocess(raw_id, db.file(raw_id), &[], &mut macros, &mut pdiags);
            diags.extend(pdiags);
            let id = db
                .add_file(path.display().to_string(), expanded)
                .map_err(|e| e)?;
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
            diags.push(Diagnostic::warning(
                "W0501",
                None,
                format!("VHDL front-end not wired yet; skipping {}", path.display()),
            ));
        }
    }
    Ok(())
}
