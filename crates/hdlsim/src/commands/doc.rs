//! `hdlsim doc` \u{2014} generate cross-linked, self-contained HTML documentation
//! for the compiled design: an index of all units plus one page per
//! module/entity with parameter/port/instance tables and syntax-highlighted
//! source. Instances link to the page of the instantiated unit.

use super::{build_manager, parse_with, remap_into_bundle, Command, CommonArgs};
use clap::Parser;
use hdl_ir::doc::generate_docs;
use hdl_ir::Design;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "cross-linked HTML documentation of entities and packages: hdlsim doc [files] [--out DIR]"
)]
struct DocArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Output directory (default: html-docs/).
    #[arg(long, short = 'o', default_value = "html-docs")]
    out: PathBuf,
    /// Source files to document.
    files: Vec<PathBuf>,
}

pub struct Doc;

impl Command for Doc {
    fn name(&self) -> &'static str {
        "doc"
    }
    fn about(&self) -> &'static str {
        "cross-linked HTML documentation of entities and packages: hdlsim doc [files] [--out DIR]"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<DocArgs>("doc", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let out = a.out;
        let (mut mgr, root) = build_manager(&opts)?;
        if a.files.is_empty() {
            return Err(anyhow::anyhow!(
                "no input files (or pass files to document)"
            ));
        }
        let files: Vec<PathBuf> = a
            .files
            .iter()
            .map(|f| remap_into_bundle(f, &root))
            .collect();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            return Ok(1);
        }
        let all_files = mgr.compiled_files();
        let summary = generate_docs(&design, &all_files, &out).map_err(anyhow::Error::msg)?;
        for u in &summary.units {
            println!("  {} ({}) -> {}", u.name, u.lang.name(), u.page);
        }
        println!(
            "hdlsim doc: wrote {} page(s) to {}",
            summary.pages.len(),
            out.display()
        );
        Ok(0)
    }
}
