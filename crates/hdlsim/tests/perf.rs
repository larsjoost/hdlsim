//! Perf gate (milestone 9 in docs/plan.md): a 10M-cycle counter testbench
//! must run in a few seconds in release. The big run is `#[ignore]`d and only
//! executes when HDLSIM_PERF=1 is set, e.g.:
//!
//!   HDLSIM_PERF=1 cargo test --release -p hdlsim --test perf -- --ignored
//!
//! A smaller always-on correctness check runs in normal CI.

use hdlsim::commands::dispatch;
use std::time::Instant;

const DUT: &str = r#"`timescale 1ns/1ps
module counter32(input clk, input rst, output reg [31:0] q);
    always @(posedge clk) begin
        if (rst) q <= 0; else q <= q + 1;
    end
endmodule
"#;

fn make_tb(dir: &std::path::Path, cycles: u32) -> std::path::PathBuf {
    let tb = dir.join("perf_tb.v");
    std::fs::write(
        &tb,
        format!(
            r#"`timescale 1ns/1ps
module perf_tb;
    reg clk = 0;
    reg rst = 1;
    wire [31:0] q;
    counter32 dut (.clk(clk), .rst(rst), .q(q));
    always #5 clk = ~clk;
    initial begin
        @(posedge clk); rst = 0;
        repeat ({}) @(posedge clk);
        if (q !== {}-1) $error("FAIL q=%h", q);
        else $display("PASS q=%h", q);
        $finish;
    end
endmodule
"#,
            cycles, cycles
        ),
    )
    .unwrap();
    tb
}

fn run_cycles(dir: &std::path::Path, cycles: u32) -> (i32, std::time::Duration) {
    let tb = make_tb(dir, cycles);
    let dut = dir.join("counter32.v");
    std::fs::write(&dut, DUT).unwrap();
    let t0 = Instant::now();
    let code = dispatch(&[
        "run".to_string(),
        "--top".to_string(),
        "perf_tb".to_string(),
        dut.to_string_lossy().to_string(),
        tb.to_string_lossy().to_string(),
    ])
    .unwrap();
    (code, t0.elapsed())
}

#[test]
fn perf_smoke_100k_cycles() {
    std::env::set_var("HDLSIM_NO_CACHE", "1");
    let dir = std::env::temp_dir().join(format!("hdlsim_perf_smoke_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (code, dur) = run_cycles(&dir, 100_000);
    assert_eq!(code, 0, "100k-cycle counter must pass");
    eprintln!("100k cycles in {:?} ({:?}/cycle)", dur, dur / 100_000);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "perf gate: run with HDLSIM_PERF=1 cargo test --release -p hdlsim --test perf -- --ignored"]
fn perf_gate_10m_cycles() {
    if std::env::var("HDLSIM_PERF").is_err() {
        eprintln!("skipping: set HDLSIM_PERF=1 to run the 10M-cycle gate");
        return;
    }
    std::env::set_var("HDLSIM_NO_CACHE", "1");
    let dir = std::env::temp_dir().join(format!("hdlsim_perf_gate_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (code, dur) = run_cycles(&dir, 10_000_000);
    assert_eq!(code, 0, "10M-cycle counter must pass");
    eprintln!(
        "10M cycles in {:?} ({:?} ns/cycle)",
        dur,
        dur.as_nanos() as u64 / 10_000_000
    );
    // Gate: plan target is "a few seconds" in release; allow 30s to keep CI
    // machines with variance green while still catching real regressions.
    assert!(dur.as_secs() < 30, "perf gate: 10M cycles took {:?}", dur);
    let _ = std::fs::remove_dir_all(&dir);
}
