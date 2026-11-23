//! End-to-end tests for real-valued (`real`) signals — the "analog waves"
//! slice: Verilog `real` and VHDL `real` variables, IEEE-754 arithmetic in the
//! kernel, integer<->real assignment conversion, `%f`/`%g` display formats,
//! and analog output in the wave sinks (VCD `r` values, HTML polyline data,
//! `.hwf` real flag, FST `FST_VT_VCD_REAL` vars).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hdlsim_analog_{}_{}", std::process::id(), name))
}

fn uniq(tag: &str) -> String {
    format!("{}_{}", tag, std::process::id())
}

const VERILOG_TB: &str = r#"module tb;
  real r, s;
  integer i;
  real y;
  assign y = r * 2.0;
  initial begin
    r = 1.5; s = 2.25;
    #10 r = r + s;
    #10 r = r * 2.0;
    #10 i = r;            // real -> integer rounds: 7.5 -> 8
    #10 $display("r=%f y=%g i=%0d", r, y, i);
    if (r == 7.5 && y == 15.0 && i == 8) $display("PASS");
    else $display("FAIL r=%f y=%g i=%0d", r, y, i);
    $finish;
  end
endmodule
"#;

const VHDL_TB: &str = r#"entity tb is end entity;
architecture sim of tb is
  signal r : real := 1.5;
  signal s : real := 0.0;
begin
  process
  begin
    wait for 10 ns;
    r <= r + 1.25;
    wait for 10 ns;
    s <= r * 2.0;
    wait for 10 ns;
    report "r=" & real'image(r) & " s=" & real'image(s);
    if r = 2.75 and s = 5.5 then report "PASS"; else report "FAIL"; end if;
    wait;
  end process;
end architecture;
"#;

#[test]
fn verilog_real_arithmetic_and_display() {
    let f = tmp(&format!("{}_tb.v", uniq("v")));
    std::fs::write(&f, VERILOG_TB).unwrap();
    let out = hdlsim()
        .args(["run", "--top", "tb", f.to_str().unwrap()])
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", stdout);
    assert!(stdout.contains("r=7.500000 y=15 i=8"), "{}", stdout);
    assert!(stdout.contains("PASS"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn vhdl_real_signals() {
    let f = tmp(&format!("{}_tb.vhd", uniq("h")));
    std::fs::write(&f, VHDL_TB).unwrap();
    let out = hdlsim()
        .args(["run", "--top", "tb", f.to_str().unwrap()])
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", stdout);
    assert!(stdout.contains("r=2.75 s=5.5"), "{}", stdout);
    assert!(stdout.contains("PASS"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn real_signals_in_all_wave_formats() {
    let tag = uniq("w");
    let f = tmp(&format!("{}_tb.v", tag));
    std::fs::write(&f, VERILOG_TB).unwrap();
    let vcd = tmp(&format!("{}_t.vcd", tag));
    let html = tmp(&format!("{}_t.html", tag));
    let hwf = tmp(&format!("{}_t.hwf", tag));
    let fst = tmp(&format!("{}_t.fst", tag));
    for p in [&vcd, &html, &hwf, &fst] {
        let _ = std::fs::remove_file(p);
    }
    let out = hdlsim()
        .args([
            "run",
            "--top",
            "tb",
            f.to_str().unwrap(),
            "--vcd",
            vcd.to_str().unwrap(),
            "--wave",
            html.to_str().unwrap(),
            "--hwf",
            hwf.to_str().unwrap(),
            "--fst",
            fst.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // VCD: real vars declared with `$var real`, changes as `r<value>`.
    let vcd_text = std::fs::read_to_string(&vcd).unwrap();
    assert!(vcd_text.contains("$var real 64"), "{}", vcd_text);
    assert!(
        vcd_text.contains("r7.5 "),
        "real change value missing: {}",
        vcd_text
    );
    assert!(
        vcd_text.contains("r15.0 "),
        "real change value missing: {}",
        vcd_text
    );

    // HTML viewer: real flag in the signal table + decimal event values.
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.contains("\"real\":true"), "real flag missing");
    assert!(page.contains("\"7.5\""), "decimal real event missing");
    assert!(
        page.contains("class: 'analog'") || page.contains(".analog"),
        "analog css missing"
    );

    // .hwf v2: real flag round-trips through read_hwf; values decode as f64.
    let hwf_bytes = std::fs::read(&hwf).unwrap();
    assert_eq!(&hwf_bytes[..8], b"HDLHWF02", "hwf v2 magic");
    let (flat, events) = hdl_wave::sinks::hwf::read_hwf(&hwf_bytes).unwrap();
    let real_sig = flat
        .signals
        .iter()
        .find(|s| s.name == "r")
        .expect("r signal");
    assert_eq!(real_sig.kind, hdl_ir::SigKind::Real);
    let r_changes: Vec<f64> = events
        .iter()
        .filter_map(|e| match e {
            hdl_sim::events::SimEvent::SignalChange { sig, new, .. }
                if flat.signals[*sig as usize].name == "r" =>
            {
                new.to_f64()
            }
            _ => None,
        })
        .collect();
    assert_eq!(r_changes, vec![1.5, 3.75, 7.5], "real change stream");

    // `hdlsim wave` on the .hwf renders real signals into the HTML viewer.
    let html2 = tmp(&format!("{}_t2.html", tag));
    let out = hdlsim()
        .args(["wave", hwf.to_str().unwrap(), "-o", html2.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let page2 = std::fs::read_to_string(&html2).unwrap();
    assert!(
        page2.contains("\"real\":true"),
        "real flag missing after replay"
    );
    assert!(page2.contains("\"7.5\""));

    // FST: real signals are FST_VT_VCD_REAL (geom width 0, 8-byte doubles);
    // read_fst reconstructs SigKind::Real + f64 values, and `wave` renders the
    // analog trace.
    let fst_bytes = std::fs::read(&fst).unwrap();
    let (flat_f, events_f) = hdl_wave::fst::read_fst(&fst_bytes).unwrap();
    let h = flat_f
        .signals
        .iter()
        .position(|s| s.name.ends_with(".r") || s.name == "r")
        .expect("r signal");
    assert_eq!(flat_f.signals[h].kind, hdl_ir::SigKind::Real);
    // t=0 value lives in the bits array (init); later changes are records.
    assert_eq!(flat_f.signals[h].init.as_ref().unwrap().to_f64(), Some(1.5));
    let r_reals: Vec<f64> = events_f
        .iter()
        .filter_map(|e| match e {
            hdl_sim::events::SimEvent::SignalChange { sig, new, .. } if *sig as usize == h => {
                new.to_f64()
            }
            _ => None,
        })
        .collect();
    assert_eq!(r_reals, vec![3.75, 7.5], "fst real change stream");
    let html3 = tmp(&format!("{}_t3.html", tag));
    let out = hdlsim()
        .args(["wave", fst.to_str().unwrap(), "-o", html3.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let page3 = std::fs::read_to_string(&html3).unwrap();
    assert!(
        page3.contains("\"real\":true"),
        "real flag after fst replay"
    );
    assert!(
        page3.contains("\"7.5\""),
        "decimal real value after fst replay"
    );

    for p in [&f, &vcd, &html, &hwf, &fst, &html2, &html3] {
        let _ = std::fs::remove_file(p);
    }
}
