//! Golden VCD test (milestone 9): simulating the counter example must produce
//! byte-for-byte the checked-in golden VCD (the kernel is deterministic).

use hdlsim::commands::dispatch;

#[test]
fn counter_vcd_matches_golden() {
    std::env::set_var("HDLSIM_NO_CACHE", "1");
    let dir = std::env::temp_dir().join(format!("hdlsim_golden_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ex = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/counter");
    let vcd = dir.join("out.vcd");
    let code = dispatch(&[
        "run".to_string(),
        "--top".to_string(),
        "tb_counter".to_string(),
        "--vcd".to_string(),
        vcd.to_string_lossy().to_string(),
        format!("{}/counter.v", ex),
        format!("{}/tb_counter.v", ex),
    ])
    .unwrap();
    assert_eq!(code, 0, "counter tb passes");
    let got = std::fs::read_to_string(&vcd).unwrap();
    let golden = include_str!("fixtures/counter_golden.vcd");
    assert_eq!(got, golden, "VCD differs from golden fixture");
    let _ = std::fs::remove_dir_all(&dir);
}
