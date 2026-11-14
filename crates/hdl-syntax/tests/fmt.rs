//! Formatter integration test: `hdl_syntax::fmt` on the repo's real example
//! files must be idempotent and must still parse without errors afterwards.

use hdl_syntax::diagnostic::Severity;
use hdl_syntax::language::Language;

#[test]
fn fmt_examples_idempotent_and_parse_clean() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let files = [
        "counter/tb_counter.v",
        "counter/counter.v",
        "fp-vhdl/tb_fp.vhd",
        "fp-vhdl/fp_mul.vhd",
        "fp-vhdl/fp_add.vhd",
        "vhdl-counter/tb_counter.vhd",
    ];
    let mut n = 0;
    for rel in files {
        let path = std::path::Path::new(root).join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));
        let lang = Language::from_path(&path).unwrap();
        let (once, diags) = hdl_syntax::fmt::format_source(lang, &text);
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{}: lex errors",
            rel
        );
        let (twice, _) = hdl_syntax::fmt::format_source(lang, &once);
        assert_eq!(once, twice, "{} not idempotent", rel);
        // Formatted output must still parse without errors.
        let mut db = hdl_syntax::source::SourceDb::new();
        let id = db.add_file(rel, &once).unwrap();
        let diags2 = match lang {
            Language::Verilog => hdl_syntax::verilog::parser::parse_file(id, db.file(id)).diags,
            Language::Vhdl => hdl_syntax::vhdl::parser::parse_file(id, db.file(id)).diags,
        };
        assert!(
            !diags2.iter().any(|d| d.severity == Severity::Error),
            "{}: parse errors after fmt: {:?}",
            rel,
            diags2.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        n += 1;
    }
    assert_eq!(n, files.len());
}
