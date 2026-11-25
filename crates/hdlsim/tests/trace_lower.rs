//! End-to-end test for `--trace-lower`: the lower-side diagnosis trail
//! must name the dims source for every dimension-suffix attribute
//! resolution and every `sig_dims` population from an inline constraint,
//! and stay completely silent without the flag.

use std::process::Command;

fn run(extra: &[&str]) -> (String, String) {
    let fx = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/vhdl_len_dim_tb.vhd"
    );
    let out = Command::new(env!("CARGO_BIN_EXE_hdlsim"))
        .args(["run", "--no-cache", "--top", "len_dim_tb"])
        .args(extra)
        .arg(fx)
        .output()
        .expect("hdlsim runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn trace_lower_names_the_dims_source_and_stays_silent_without_the_flag() {
    let (out_on, err_on) = run(&["--trace-lower"]);
    assert!(out_on.contains("ROWS=2"), "fixture passes: {out_on}");
    // Resolution: which dims source answered, and with what value.
    assert!(
        err_on.contains("[lower] plane_s'length(2): sig_dims -> 2"),
        "resolution names the source: {err_on}"
    );
    assert!(
        err_on.contains("sig_dims[0] = [(0, 5), (0, 2)] via inline constraint (plane_s)"),
        "population is traced: {err_on}"
    );
    assert!(
        err_on.contains("[lower] g_grid'length(1): obj_dims -> 2"),
        "object-dims resolution is traced: {err_on}"
    );

    // Off state: no trace output at all.
    let (_out_off, err_off) = run(&[]);
    assert!(
        !err_off.contains("[lower]"),
        "silent without the flag: {err_off}"
    );
}
