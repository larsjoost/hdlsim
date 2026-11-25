//! `hdlsim ir` — dump the elaborated IR of one top as pretty JSON.
//!
//! The IR is the artifact the kernel executes (see `hdl-ir`): flat signals,
//! processes, continuous assigns, and function bodies. The dump matches the
//! numbering the simulator traces use (`Sig(4635)`, dispatch function
//! indices), so it doubles as a diagnosis tool: `hdlsim ir --top t f.vhd >
//! ir.json`, then search the same names/indices the traces report.

use super::{build_manager, parse_with, remap_into_bundle, Command, CommonArgs};
use anyhow::Context as _;
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "dump the elaborated IR of a top as pretty JSON: hdlsim ir --top t file.vhd")]
struct IrArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Top unit to elaborate (entity/module name). Omitted when the
    /// candidates are unambiguous (one module, or one entity and its
    /// architectures).
    #[arg(long)]
    top: Option<String>,
    /// Write the JSON here instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Source files to compile.
    files: Vec<PathBuf>,
}

pub struct Ir;

impl Command for Ir {
    fn name(&self) -> &'static str {
        "ir"
    }
    fn about(&self) -> &'static str {
        "dump the elaborated IR of a top as pretty JSON: hdlsim ir --top t file.vhd"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<IrArgs>("ir", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        if a.files.is_empty() {
            return Err(anyhow::anyhow!("no input files"));
        }
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = a
            .files
            .iter()
            .map(|f| remap_into_bundle(f, &root))
            .collect();
        let mut design = Design::default();
        let compile_phase = crate::phase::Phase::start("compile");
        mgr.compile_files(&mut design, &files);
        compile_phase.done(format!("{} modules", design.modules.len()));
        let diags = mgr.diags();
        if !diags.is_empty() {
            super::emit_diags(diags, mgr.db());
        }
        if diags.has_errors() {
            println!("hdlsim ir: {} error(s)", diags.items.len());
            return Ok(1);
        }
        // One top candidate: the top is unambiguous. VHDL compiles an entity
        // and its architecture as two modules named `e` and `e/arch`, so
        // candidates collapse to the entity root before counting.
        let top = match &a.top {
            Some(t) => t.clone(),
            None => {
                let mut roots: Vec<&str> = design
                    .modules
                    .iter()
                    .map(|m| m.name.split('/').next().unwrap_or(&m.name))
                    .collect();
                roots.sort_unstable();
                roots.dedup();
                match roots.as_slice() {
                    [one] => (*one).to_string(),
                    _ => anyhow::bail!(
                        "no --top given and {} candidate units: {}; pass --top <name>",
                        roots.len(),
                        roots.join(", ")
                    ),
                }
            }
        };
        let elab_phase = crate::phase::Phase::start("elaborate");
        let flat = elab::elaborate_with(&mut design, &top, &mut mgr)
            .with_context(|| format!("elaborating '{top}'"))?;
        elab_phase.done(format!(
            "{} signals, {} processes, {} functions",
            flat.signals.len(),
            flat.procs.len(),
            flat.functions.len()
        ));
        // `--dump-maps` wraps the flat IR with the lower-side diagnosis
        // maps — one JSON document either way, so consumers never have to
        // split concatenated documents.
        let json = if crate::commands::run_options().dump_maps {
            maps_wrapped_json(&mgr, &design, &flat)?
        } else {
            serde_json::to_string_pretty(&flat)?
        };
        if let Some(out) = &a.out {
            std::fs::write(out, json).with_context(|| format!("writing {}", out.display()))?;
            println!("hdlsim ir: wrote {}", out.display());
        } else {
            println!("{json}");
        }
        Ok(0)
    }
}

/// The `--dump-maps` view: the flat IR wrapped with the lower-side maps
/// that previously existed only inside the lowerer — per-module signal
/// dims (architecture-local signals included; ids module-relative, as
/// believed at the end of lowering), per-entity signal maps, per-file
/// object dims (constants/generics), and parse-cache provenance. All keys
/// BTreeMap-sorted for deterministic output.
fn maps_wrapped_json(
    mgr: &hdl_lib::LibraryManager,
    design: &Design,
    flat: &elab::Flat,
) -> anyhow::Result<String> {
    let ro = crate::commands::run_options();
    let files: std::collections::BTreeMap<String, serde_json::Value> = mgr
        .file_obj_dims()
        .iter()
        .map(|(p, m)| {
            (
                p.display().to_string(),
                serde_json::json!({ "obj_dims": m }),
            )
        })
        .collect();
    let entities: std::collections::BTreeMap<String, serde_json::Value> = mgr
        .entity_maps()
        .iter()
        .map(|(name, m)| (name.clone(), m.debug_json()))
        .collect();
    let modules: std::collections::BTreeMap<String, serde_json::Value> = design
        .modules
        .iter()
        .filter_map(|m| {
            m.sig_dims_debug
                .as_ref()
                .map(|d| (m.name.clone(), serde_json::json!({ "sig_dims": d })))
        })
        .collect();
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ir": flat,
        "maps": {
            "cache": { "no_cache": ro.no_cache, "hits": mgr.cache_hits() },
            "entities": entities,
            "modules": modules,
            "files": files,
        },
    }))?)
}
