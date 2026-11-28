//! Library-scoping helpers for unit resolution: attributing a source file
//! to the mapped library directory it lives in, and classifying files by
//! whether they declare packages (compile-order partitioning).

use crate::providers::UnitProvider;
use std::path::{Path, PathBuf};

/// Name of the mapped library directory that contains `path` (longest
/// directory prefix wins for nested mappings), if any. `work` inside a unit
/// means this library (LRM 1076-2008 §13.2) — without the translation,
/// `use work.unit` in a unit compiled into library `math` falls through to
/// the global name match, where a vendor library declaring a same-named
/// unit (indexed earlier in the ini) silently shadows the design's own copy.
pub(super) fn owning_library(providers: &[Box<dyn UnitProvider>], path: &Path) -> Option<String> {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    providers
        .iter()
        .filter_map(|p| {
            let dir = p.dir()?;
            let canon_dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
            if canon.starts_with(&canon_dir) {
                Some((canon_dir.components().count(), p.name().to_string()))
            } else {
                None
            }
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, name)| name)
}

/// Whether a source file is a package *declaration* (VHDL) or declares a
/// `package` (Verilog/SV) — `compile_files` lowers these first so dependent
/// files see the package's items regardless of argument order.
pub(super) fn is_package_source(ext: Option<&str>, text: &str) -> bool {
    match ext {
        Some("v") | Some("sv") => text.lines().any(|l| l.trim_start().starts_with("package ")),
        Some("vhd") | Some("vhdl") => text.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("package ") && !t.starts_with("package body")
        }),
        _ => false,
    }
}

/// Partition CLI paths: package-declaring files first (`is_package_source`),
/// so dependents see the package's items regardless of argument order.
pub(super) fn package_files_first(paths: &[PathBuf]) -> (Vec<&PathBuf>, Vec<&PathBuf>) {
    let mut pkgs = Vec::new();
    let mut rest = Vec::new();
    for p in paths {
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        let text = std::fs::read_to_string(p).unwrap_or_default();
        if is_package_source(ext.as_deref(), &text) {
            pkgs.push(p);
        } else {
            rest.push(p);
        }
    }
    (pkgs, rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::DirProvider;
    use std::fs;

    #[test]
    fn owning_library_prefers_the_deepest_mapping() {
        let base = std::env::temp_dir().join(format!("hdlsim_scope_{}", std::process::id()));
        let nested = base.join("inner");
        fs::create_dir_all(&nested).expect("create dirs");
        let file = nested.join("u.vhd");
        fs::write(&file, "package p is end package p;\n").expect("write unit");
        let outer = DirProvider::vhdl("outer", &base);
        let inner = DirProvider::vhdl("inner", &nested);
        let providers: Vec<Box<dyn UnitProvider>> = vec![Box::new(outer), Box::new(inner)];
        assert_eq!(
            owning_library(&providers, &file).as_deref(),
            Some("inner"),
            "the deepest mapping wins"
        );
        let outside = base.parent().unwrap().join("elsewhere.vhd");
        assert_eq!(owning_library(&providers, &outside), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn package_detection_by_extension() {
        assert!(is_package_source(Some("vhd"), "package p is\nend p;\n"));
        assert!(!is_package_source(
            Some("vhd"),
            "package body p is\nend p;\n"
        ));
        assert!(is_package_source(Some("v"), "package p;\nendpackage\n"));
        assert!(!is_package_source(Some("v"), "module m;\nendmodule\n"));
        assert!(!is_package_source(Some("c"), "package p;"));
        assert!(!is_package_source(None, "package p;"));
    }
}
