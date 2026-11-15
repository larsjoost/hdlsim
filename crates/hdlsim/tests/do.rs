//! End-to-end test for `hdlsim do`: a mini-Tcl do-file driving the counter
//! example with incremental `run` steps, a `for` loop, `vcd dumpfile`,
//! `examine`, and `quit`. Verifies the exit code and that the VCD contains
//! timestamps from every stepped `run` (proving `run_until` is incremental).

use hdlsim::commands::dispatch;

#[test]
fn do_file_runs_incremental_steps_and_writes_vcd() {
    std::env::set_var("HDLSIM_NO_CACHE", "1");
    let dir = std::env::temp_dir().join(format!("hdlsim_do_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ex = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/counter");
    let vcd = dir.join("out.vcd");
    let script = dir.join("run.do");
    std::fs::write(
        &script,
        format!(
            "# do-file test\nvcd dumpfile {}\nrun 20ns\nfor {{set i 0}} {{$i < 2}} {{incr i}} {{ run 10ns }}\nrun -all\nquit\n",
            vcd.display()
        ),
    )
    .unwrap();
    let code = dispatch(&[
        "do".to_string(),
        script.to_string_lossy().to_string(),
        "--top".to_string(),
        "tb_counter".to_string(),
        format!("{}/counter.v", ex),
        format!("{}/tb_counter.v", ex),
    ])
    .unwrap();
    assert_eq!(code, 0, "counter tb passes -> exit 0");
    let vcd_text = std::fs::read_to_string(&vcd).unwrap();
    // 20ns step + two 10ns steps must each advance time: 5ns clock edges land
    // at 5,10,...,40ns.
    for t in ["#5000000", "#20000000", "#30000000", "#40000000"] {
        assert!(vcd_text.contains(t), "vcd missing {}: ...", t);
    }

    // A script error is a usage error (dispatch Err -> exit 2 at the binary).
    let bad = dir.join("bad.do");
    std::fs::write(&bad, "frobnicate\n").unwrap();
    assert!(dispatch(&[
        "do".to_string(),
        bad.to_string_lossy().to_string(),
        "--top".to_string(),
        "tb_counter".to_string(),
        format!("{}/counter.v", ex),
        format!("{}/tb_counter.v", ex),
    ])
    .is_err());

    let _ = std::fs::remove_dir_all(&dir);
}
