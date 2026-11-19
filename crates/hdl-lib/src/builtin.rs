//! Built-in standard libraries (`std`, `ieee`) — the VHDL sources in this
//! crate's `stdlib/` tree, embedded into the binary with `include_str!` and
//! parsed through the normal parse/lower pipeline like any other library, so
//! no on-disk stdlib is needed (installed binaries always have `std`/`ieee`).
//!
//! `$HDLSIM_STDLIB` overrides the embedded tree with a custom on-disk one.
//! The providers are appended *last* in the library search path, so a user
//! mapping like `-L ieee=./my_ieee` wins over the built-ins.

use crate::providers::{DirProvider, EmbeddedProvider, UnitProvider};
use std::path::PathBuf;

/// The embedded stdlib: `(unit name, source)` per library.
fn embedded_stdlib() -> Vec<EmbeddedProvider> {
    vec![
        EmbeddedProvider::new(
            "std",
            &[
                ("standard", include_str!("../stdlib/std/standard.vhd")),
                ("env", include_str!("../stdlib/std/env.vhd")),
                ("textio", include_str!("../stdlib/std/textio.vhd")),
            ],
        ),
        EmbeddedProvider::new(
            "ieee",
            &[
                (
                    "std_logic_1164",
                    include_str!("../stdlib/ieee/std_logic_1164.vhd"),
                ),
                (
                    "numeric_std",
                    include_str!("../stdlib/ieee/numeric_std.vhd"),
                ),
                (
                    "std_logic_arith",
                    include_str!("../stdlib/ieee/std_logic_arith.vhd"),
                ),
                (
                    "std_logic_signed",
                    include_str!("../stdlib/ieee/std_logic_signed.vhd"),
                ),
                (
                    "std_logic_unsigned",
                    include_str!("../stdlib/ieee/std_logic_unsigned.vhd"),
                ),
                (
                    "std_logic_textio",
                    include_str!("../stdlib/ieee/std_logic_textio.vhd"),
                ),
                ("fixed_pkg", include_str!("../stdlib/ieee/fixed_pkg.vhd")),
                ("float_pkg", include_str!("../stdlib/ieee/float_pkg.vhd")),
                (
                    "fixed_float_types",
                    include_str!("../stdlib/ieee/fixed_float_types.vhd"),
                ),
                (
                    "fixed_generic_pkg",
                    include_str!("../stdlib/ieee/fixed_generic_pkg.vhd"),
                ),
                (
                    "float_generic_pkg",
                    include_str!("../stdlib/ieee/float_generic_pkg.vhd"),
                ),
                ("math_real", include_str!("../stdlib/ieee/math_real.vhd")),
                (
                    "math_complex",
                    include_str!("../stdlib/ieee/math_complex.vhd"),
                ),
                (
                    "numeric_bit",
                    include_str!("../stdlib/ieee/numeric_bit.vhd"),
                ),
                (
                    "numeric_bit_unsigned",
                    include_str!("../stdlib/ieee/numeric_bit_unsigned.vhd"),
                ),
                (
                    "numeric_std_unsigned",
                    include_str!("../stdlib/ieee/numeric_std_unsigned.vhd"),
                ),
                (
                    "ieee_std_context",
                    include_str!("../stdlib/ieee/std_context.vhd"),
                ),
            ],
        ),
        // Pre-2008 name of the same packages (`library ieee_proposed`).
        EmbeddedProvider::new(
            "ieee_proposed",
            &[
                ("fixed_pkg", include_str!("../stdlib/ieee/fixed_pkg.vhd")),
                ("float_pkg", include_str!("../stdlib/ieee/float_pkg.vhd")),
                (
                    "fixed_float_types",
                    include_str!("../stdlib/ieee/fixed_float_types.vhd"),
                ),
                (
                    "fixed_generic_pkg",
                    include_str!("../stdlib/ieee/fixed_generic_pkg.vhd"),
                ),
                (
                    "float_generic_pkg",
                    include_str!("../stdlib/ieee/float_generic_pkg.vhd"),
                ),
                (
                    "standard_additions",
                    include_str!("../stdlib/ieee_proposed/standard_additions.vhd"),
                ),
            ],
        ),
    ]
}

/// The built-in library providers (`std`, then `ieee`): the custom
/// `$HDLSIM_STDLIB` tree if set and valid, otherwise the embedded stdlib.
pub fn stdlib_providers() -> Vec<Box<dyn UnitProvider>> {
    stdlib_providers_from(std::env::var("HDLSIM_STDLIB").ok().map(PathBuf::from))
}

/// Providers for an optional custom stdlib root (); falls back
/// to the embedded tree when unset, invalid, or empty.
fn stdlib_providers_from(custom: Option<PathBuf>) -> Vec<Box<dyn UnitProvider>> {
    if let Some(root) = custom {
        if root.is_dir() {
            let mut out: Vec<Box<dyn UnitProvider>> = Vec::new();
            for name in ["std", "ieee", "ieee_proposed"] {
                let dir = root.join(name);
                if dir.is_dir() {
                    out.push(Box::new(DirProvider::vhdl(name, &dir)));
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    embedded_stdlib()
        .into_iter()
        .map(|p| Box::new(p) as Box<dyn UnitProvider>)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_syntax::source::SourceDb;
    use hdl_syntax::vhdl::parser;

    #[test]
    fn embedded_stdlib_scans_expected_units() {
        // No env var set: the embedded tree is used, no disk access needed.
        let providers = stdlib_providers();
        let names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
        assert_eq!(names, vec!["std", "ieee", "ieee_proposed"]);
        let units: Vec<String> = providers
            .iter()
            .flat_map(|p| p.scan().into_iter().map(|(u, _)| u))
            .collect();
        for u in [
            "standard",
            "env",
            "textio",
            "std_logic_1164",
            "numeric_std",
            "std_logic_arith",
            "std_logic_signed",
            "std_logic_unsigned",
            "std_logic_textio",
            "fixed_pkg",
            "float_pkg",
            "fixed_float_types",
            "fixed_generic_pkg",
            "float_generic_pkg",
        ] {
            assert!(units.contains(&u.to_string()), "{:?}", units);
        }
    }

    #[test]
    fn embedded_stdlib_reads_and_parses_without_errors() {
        let providers = stdlib_providers();
        let mut count = 0;
        for p in &providers {
            for (unit, file) in p.scan() {
                let text = p.read(&unit, &file).expect("embedded source reads");
                count += 1;
                let mut db = SourceDb::new();
                let id = db.add_file(file.display().to_string(), text).unwrap();
                let parsed = parser::parse_file(id, db.file(id));
                let errs: Vec<_> = parsed
                    .diags
                    .iter()
                    .filter(|d| d.severity == hdl_syntax::diagnostic::Severity::Error)
                    .collect();
                assert!(
                    errs.is_empty(),
                    "{}:
{}",
                    unit,
                    errs.iter().map(|d| d.render(&db)).collect::<Vec<_>>().join(
                        "
"
                    )
                );
            }
        }
        assert!(
            count >= 8,
            "expected at least 8 stdlib units, got {}",
            count
        );
    }

    #[test]
    fn custom_stdlib_root_overrides_embedded() {
        let dir = std::env::temp_dir().join(format!("hdlsim_stdlib_it_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ieee")).unwrap();
        std::fs::write(
            dir.join("ieee/std_logic_1164.vhd"),
            "package std_logic_1164 is end;",
        )
        .unwrap();
        let providers = stdlib_providers_from(Some(dir.clone()));
        let names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
        assert_eq!(names, vec!["ieee"]);
        let units = providers[0].scan();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].0, "std_logic_1164");
        assert!(units[0].1.starts_with(&dir));
        // Unset/invalid -> embedded tree.
        assert_eq!(stdlib_providers_from(None).len(), 3);
        assert_eq!(
            stdlib_providers_from(Some(PathBuf::from("/nonexistent"))).len(),
            3
        );
    }
}
