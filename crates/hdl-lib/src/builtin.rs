//! Built-in standard libraries (`std`, `ieee`) as real VHDL source files on
//! disk — NOT compiled into the binary. The tree lives in this crate at
//! `stdlib/{std,ieee}/*.vhd` and is located at runtime by [`stdlib_dir`].
//!
//! Search order:
//! 1. `$HDLSIM_STDLIB` (explicit override),
//! 2. `<dir of the hdlsim executable>/stdlib` and
//!    `<exe>/../share/hdlsim/stdlib` (installed layout),
//! 3. the crate's source tree (`CARGO_MANIFEST_DIR/stdlib`) — covers
//!    `cargo build` / `cargo test` / `cargo install --path` from a checkout.
//!
//! The providers are appended *last* in the library search path, so a user
//! mapping like `-L ieee=./my_ieee` overrides the built-ins. Kernel-primitive
//! semantics (integer, std_logic, arithmetic) remain fast paths in the
//! lowerer; these files make the standard names resolve through the normal
//! parse/lower pipeline and are what `project`/tooling export sees.

use crate::providers::{DirProvider, UnitProvider};
use std::path::PathBuf;

/// Locate the on-disk stdlib root (containing `std/` and `ieee/` subdirs).
pub fn stdlib_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("HDLSIM_STDLIB") {
        let p = PathBuf::from(d);
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [
                dir.join("stdlib"),
                dir.join("share/hdlsim/stdlib"),
                dir.join("../share/hdlsim/stdlib"),
            ] {
                if cand.is_dir() {
                    return Some(cand);
                }
            }
        }
    }
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("stdlib");
    if src.is_dir() {
        return Some(src);
    }
    None
}

/// The built-in library providers (`std`, then `ieee`), empty if the stdlib
/// tree cannot be located.
pub fn stdlib_providers() -> Vec<Box<dyn UnitProvider>> {
    let mut out: Vec<Box<dyn UnitProvider>> = Vec::new();
    let Some(root) = stdlib_dir() else {
        return out;
    };
    for name in ["std", "ieee"] {
        let dir = root.join(name);
        if dir.is_dir() {
            out.push(Box::new(DirProvider::vhdl(name, &dir)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_syntax::source::SourceDb;
    use hdl_syntax::vhdl::parser;

    #[test]
    fn stdlib_dir_is_found_in_checkout() {
        let dir = stdlib_dir().expect("stdlib dir must resolve in a source checkout");
        assert!(dir.join("std/standard.vhd").is_file(), "{}", dir.display());
        assert!(dir.join("ieee/std_logic_1164.vhd").is_file());
        assert!(dir.join("ieee/numeric_std.vhd").is_file());
    }

    #[test]
    fn stdlib_providers_scan_expected_units() {
        let providers = stdlib_providers();
        let names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
        assert_eq!(names, vec!["std", "ieee"]);
        let units: Vec<String> = providers
            .iter()
            .flat_map(|p| p.scan().into_iter().map(|(u, _)| u))
            .collect();
        assert!(units.contains(&"standard".to_string()), "{:?}", units);
        assert!(units.contains(&"std_logic_1164".to_string()), "{:?}", units);
        assert!(units.contains(&"numeric_std".to_string()), "{:?}", units);
    }

    #[test]
    fn every_stdlib_file_parses_without_errors() {
        let root = stdlib_dir().unwrap();
        let mut count = 0;
        for lib in ["std", "ieee"] {
            for entry in std::fs::read_dir(root.join(lib)).unwrap().flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("vhd") {
                    continue;
                }
                count += 1;
                let text = std::fs::read_to_string(&path).unwrap();
                let mut db = SourceDb::new();
                let id = db.add_file(path.display().to_string(), text).unwrap();
                let parsed = parser::parse_file(id, db.file(id));
                let errs: Vec<_> = parsed
                    .diags
                    .iter()
                    .filter(|d| d.severity == hdl_syntax::diagnostic::Severity::Error)
                    .collect();
                assert!(
                    errs.is_empty(),
                    "{}:\n{}",
                    path.display(),
                    errs.iter()
                        .map(|d| d.render(&db))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
        assert!(
            count >= 3,
            "expected at least 3 stdlib files, got {}",
            count
        );
    }
}
