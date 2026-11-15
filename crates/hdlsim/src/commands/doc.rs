//! `hdlsim doc` \u{2014} generate cross-linked, self-contained HTML documentation
//! for the compiled design: an index of all units plus one page per
//! module/entity with parameter/port/instance tables and syntax-highlighted
//! source. Instances link to the page of the instantiated unit.

use super::{build_manager, extract_common, remap_into_bundle, Command};
use hdl_ir::doc::generate_docs;
use hdl_ir::Design;
use std::path::PathBuf;

pub struct Doc;

impl Command for Doc {
    fn name(&self) -> &'static str {
        "doc"
    }
    fn about(&self) -> &'static str {
        "generate cross-linked HTML docs: hdlsim doc [files...] [--out DIR] (default html-docs/)"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let mut out = PathBuf::from("html-docs");
        let mut files: Vec<PathBuf> = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--out" | "-o" => {
                    i += 1;
                    out = PathBuf::from(rest.get(i).ok_or("--out needs a directory argument")?);
                }
                other => files.push(PathBuf::from(other)),
            }
            i += 1;
        }
        let (mut mgr, root) = build_manager(&opts)?;
        if files.is_empty() {
            return Err("no input files (or pass files to document)".to_string());
        }
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            return Ok(1);
        }
        let all_files = mgr.compiled_files();
        let summary = generate_docs(&design, &all_files, &out)?;
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
