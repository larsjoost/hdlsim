//! End-to-end test for `hdlsim fmt`: --check detects an unformatted file and
//! exits 1, -w rewrites it, and a second --check passes (idempotent).

use hdlsim::commands::dispatch;

#[test]
fn fmt_check_write_check() {
    let dir = std::env::temp_dir().join(format!("hdlsim_fmt_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let messy = "module m;\nwire [3:0] x;\nassign y=~x;\nendmodule\n";
    let want = "module m;\n  wire [3:0] x;\n  assign y = ~x;\nendmodule\n";
    let f = dir.join("m.v");
    std::fs::write(&f, messy).unwrap();
    let p = f.to_string_lossy().to_string();

    // --check on unformatted file -> exit 1
    let code = dispatch(&["fmt".to_string(), "--check".to_string(), p.clone()]).unwrap();
    assert_eq!(code, 1);
    // -w rewrites
    let code = dispatch(&["fmt".to_string(), "-w".to_string(), p.clone()]).unwrap();
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), want);
    // --check now passes -> exit 0
    let code = dispatch(&["fmt".to_string(), "--check".to_string(), p.clone()]).unwrap();
    assert_eq!(code, 0);
    // usage errors
    assert!(dispatch(&["fmt".to_string()]).is_err());
    assert!(dispatch(&[
        "fmt".to_string(),
        "--check".to_string(),
        "-w".to_string(),
        p.clone()
    ])
    .is_err());
    // unknown extension
    let bad = dir.join("x.txt");
    std::fs::write(&bad, "hi").unwrap();
    assert!(dispatch(&["fmt".to_string(), bad.to_string_lossy().to_string()]).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_vhdl_write() {
    let dir = std::env::temp_dir().join(format!("hdlsim_fmt_test_vhd_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let messy = "entity e is end entity;\narchitecture r of e is begin\ny<=a;\nend architecture;\n";
    let want =
        "entity e is end entity;\narchitecture r of e is begin\n  y <= a;\nend architecture;\n";
    let f = dir.join("e.vhd");
    std::fs::write(&f, messy).unwrap();
    let p = f.to_string_lossy().to_string();
    let code = dispatch(&["fmt".to_string(), "-w".to_string(), p.clone()]).unwrap();
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), want);
    let _ = std::fs::remove_dir_all(&dir);
}
