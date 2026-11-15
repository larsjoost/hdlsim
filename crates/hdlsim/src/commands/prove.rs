//! `hdlsim prove` — bounded model checking with the own CDCL SAT solver
//! (`hdl_sim::bmc` + `hdl_sim::sat`). Compiles the files, elaborates `--top`
//! (wrapped so its ports are free inputs), and checks every
//! `assert property` / `assert` within `--bmc N` clock edges.
//!
//! Exit codes: 0 = no violation within the bound (safe up to N, not a proof),
//! 1 = a property is violated (counterexample trace printed), 2 = usage or
//! unsupported-construct error.

use super::{build_manager, extract_common, remap_into_bundle, Command};
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_sim::bmc::{prove, wrap_top, ProveStatus};
use std::path::PathBuf;

pub struct Prove;

impl Command for Prove {
    fn name(&self) -> &'static str {
        "prove"
    }
    fn about(&self) -> &'static str {
        "bounded model check a DUT: hdlsim prove --top dut [--bmc N] files..."
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut top: Option<String> = None;
        let mut bmc: usize = 10;
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--top" => {
                    top = Some(rest.get(i + 1).ok_or("--top needs a value")?.clone());
                    i += 2;
                }
                "--bmc" => {
                    bmc = rest
                        .get(i + 1)
                        .ok_or("--bmc needs a value")?
                        .parse()
                        .map_err(|_| "--bmc needs a number")?;
                    i += 2;
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{}'", other));
                }
                f => {
                    files.push(PathBuf::from(f));
                    i += 1;
                }
            }
        }
        if files.is_empty() && top.is_none() {
            return Err("no input files (or pass --top to resolve from libraries)".to_string());
        }
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            return Ok(1);
        }
        let top_name = match top {
            Some(t) => t,
            None => {
                if design.modules.len() == 1 {
                    design.modules[0].name.clone()
                } else {
                    let names: Vec<&str> = design.modules.iter().map(|m| m.name.as_str()).collect();
                    return Err(format!(
                        "multiple modules ({}); pick one with --top",
                        names.join(", ")
                    ));
                }
            }
        };
        wrap_top(&mut design, &top_name)?;
        let flat = elab::elaborate_with(&mut design, "__hdlsim_prove_top", &mut mgr)
            .map_err(|e| format!("elaboration error: {}", e))?;
        let r = prove(&flat, bmc)?;
        if !mgr.diags().is_empty() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
        }
        println!(
            "hdlsim prove: top '{}', BMC depth {}, {} propert{}",
            top_name,
            r.steps,
            r.properties.len(),
            if r.properties.len() == 1 { "y" } else { "ies" }
        );
        for (i, p) in r.properties.iter().enumerate() {
            println!("  [{}] {}", i, p);
        }
        match r.status {
            ProveStatus::Safe => {
                println!(
                    "hdlsim prove: SAFE — no violation within {} step{} (bounded result, not a proof)",
                    r.steps,
                    if r.steps == 1 { "" } else { "s" }
                );
                Ok(0)
            }
            ProveStatus::Violation(pi, t) => {
                println!(
                    "hdlsim prove: VIOLATION — property [{}] fails at step {}",
                    pi, t
                );
                println!("counterexample:");
                for (t, row) in r.trace.iter().enumerate() {
                    let cells: Vec<String> =
                        row.iter().map(|(n, v)| format!("{}={}", n, v)).collect();
                    println!("  step {:>3}: {}", t, cells.join("  "));
                }
                Ok(1)
            }
        }
    }
}
