//! End-to-end test for `hdlsim regress`: two testbenches (one passing, one
//! failing), a JUnit XML file, and the exit code.

use hdlsim::commands::dispatch;

const TB_PASS: &str = r#"module tb_pass;
  initial begin
    $display("PASS");
    $finish;
  end
endmodule
"#;

const TB_FAIL: &str = r#"module tb_fail;
  initial begin
    $error("boom");
    $finish;
  end
endmodule
"#;

fn write(dir: &std::path::Path, name: &str, src: &str) {
    std::fs::write(dir.join(name), src).unwrap();
}

#[test]
fn regress_runs_tests_and_writes_junit() {
    // Keep the test hermetic: no on-disk cache in the repo working dir.
    std::env::set_var("HDLSIM_NO_CACHE", "1");
    let dir = std::env::temp_dir().join(format!("hdlsim_regress_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(&dir, "tb_pass.v", TB_PASS);
    write(&dir, "tb_fail.v", TB_FAIL);
    let junit = dir.join("junit.xml");

    let code = dispatch(&[
        "regress".to_string(),
        "--junit".to_string(),
        junit.to_string_lossy().to_string(),
        "-L".to_string(),
        format!("work={}", dir.display()),
        "tb_pass".to_string(),
        "tb_fail".to_string(),
    ])
    .unwrap();
    assert_eq!(code, 1, "one failing test must exit 1");
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains(r#"tests="2" failures="1""#), "{}", xml);
    assert!(xml.contains(r#"name="tb_pass""#), "{}", xml);
    assert!(xml.contains(r#"name="tb_fail""#), "{}", xml);
    assert!(
        xml.contains("<failure message=") && xml.contains("boom"),
        "{}",
        xml
    );

    // Passing-only run exits 0.
    let code = dispatch(&[
        "regress".to_string(),
        "-L".to_string(),
        format!("work={}", dir.display()),
        "tb_pass".to_string(),
    ])
    .unwrap();
    assert_eq!(code, 0);

    // --list file form: `name=top` lines.
    let list = dir.join("tests.txt");
    std::fs::write(&list, "# comment\n\nmy_pass=tb_pass\n").unwrap();
    let junit2 = dir.join("junit2.xml");
    let code = dispatch(&[
        "regress".to_string(),
        "--junit".to_string(),
        junit2.to_string_lossy().to_string(),
        "--list".to_string(),
        list.to_string_lossy().to_string(),
        "-L".to_string(),
        format!("work={}", dir.display()),
    ])
    .unwrap();
    assert_eq!(code, 0);
    let xml = std::fs::read_to_string(&junit2).unwrap();
    assert!(
        xml.contains(r#"name="my_pass""#) && xml.contains(r#"failures="0""#),
        "{}",
        xml
    );

    let _ = std::fs::remove_dir_all(&dir);
}
