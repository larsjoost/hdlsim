//! End-to-end test for `hdlsim lint`: findings -> exit 1; `--fix` rewrites the
//! fixable ones in place and leaves the unfixable ones -> still exit 1; clean
//! file -> exit 0.

use hdlsim::commands::dispatch;

#[test]
fn lint_reports_and_fix() {
    let dir = std::env::temp_dir().join(format!("hdlsim_lint_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // trailing whitespace (fixable) + case without default (not fixable)
    let messy = "module m;  \nalways @(*) begin\ncase (x)\n0: y = 1;\nendcase\nend\nendmodule\n";
    let f = dir.join("m.v");
    std::fs::write(&f, messy).unwrap();
    let p = f.to_string_lossy().to_string();

    // plain lint: exit 1 (findings exist), file unchanged
    assert_eq!(dispatch(&["lint".to_string(), p.clone()]).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), messy);

    // --fix: trailing ws removed, case finding remains -> still exit 1
    assert_eq!(
        dispatch(&["lint".to_string(), "--fix".to_string(), p.clone()]).unwrap(),
        1
    );
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(
        !fixed.contains("  \n"),
        "trailing ws not fixed: {:?}",
        fixed
    );
    assert!(fixed.contains("module m;\n"));

    // clean file: exit 0
    let clean = dir.join("ok.v");
    std::fs::write(&clean, "module m;\nalways @(*) begin\ncase (x)\n0: y = 1;\ndefault: y = 0;\nendcase\nend\nendmodule\n").unwrap();
    assert_eq!(
        dispatch(&["lint".to_string(), clean.to_string_lossy().to_string()]).unwrap(),
        0
    );

    // usage error
    assert!(dispatch(&["lint".to_string()]).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
