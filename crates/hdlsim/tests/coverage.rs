//! End-to-end coverage test: compile a small Verilog design, simulate it with
//! a CoverageCollector, and check line/branch/toggle numbers and the report.

use hdl_ir::elab;
use hdl_sim::coverage::CoverageCollector;
use hdl_sim::{Observer, SimEvent, Simulator};
use hdlsim::compile::compile_paths;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

struct CovForward(Rc<RefCell<CoverageCollector>>);
impl Observer for CovForward {
    fn on_event(&mut self, ev: &SimEvent) {
        self.0.borrow_mut().on_event(ev);
    }
}

const TB: &str = r#"module tb;
  reg clk = 1'b0;
  reg [3:0] cnt = 4'd0;
  always #5 clk = ~clk;
  always @(posedge clk) begin
    if (cnt < 3)
      cnt <= cnt + 1;
    else
      cnt <= 4'd0;
  end
  initial begin
    #100;
    if (cnt == 4'd3) $display("PASS");
    else $error("FAIL: cnt=%0d", cnt);
    $finish;
  end
endmodule
"#;

#[test]
fn run_collects_line_branch_toggle_coverage() {
    let dir = std::env::temp_dir().join(format!("hdlsim_cov_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tb.v");
    std::fs::write(&f, TB).unwrap();

    let compiled = compile_paths(&[f.clone()]);
    assert!(!compiled.has_errors(), "{}", compiled.render_diags());
    let flat = elab::elaborate(&compiled.design, "tb").unwrap();
    let mut sim = Simulator::new(flat);
    let widths: Vec<usize> = sim.flat().signals.iter().map(|s| s.width).collect();
    let cov = Rc::new(RefCell::new(CoverageCollector::new(
        &compiled.design,
        &widths,
    )));
    sim.add_observer(Box::new(CovForward(cov.clone())));
    sim.run(1_000_000_000);

    let (lh, lt) = cov.borrow().line_coverage();
    let (bh, bt) = cov.borrow().branch_coverage();
    let (th, tt) = cov.borrow().toggle_coverage();
    // Every statement executed at least once except some branch arms.
    assert!(lh > 0 && lh <= lt, "line {}/{}", lh, lt);
    assert!(lt >= 5, "expected several statements, got {}", lt);
    assert!(
        bh > 0 && bh < bt,
        "branch {}/{} (if-else arms: some missed)",
        bh,
        bt
    );
    // clk toggles 0<->1 every 5 time units; cnt changes too.
    assert!(th > 0 && th <= tt, "toggle {}/{}", th, tt);

    let rep = cov.borrow().report(&compiled.db);
    assert!(rep.contains("coverage: line"), "{}", rep);
    assert!(rep.contains("branch"), "{}", rep);
    assert!(rep.contains("toggle"), "{}", rep);
    assert!(rep.contains("tb.v"), "{}", rep);

    // The never-taken arm must show up as a missed line in the report.
    assert!(rep.contains("missed lines"), "{}", rep);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = PathBuf::from(&f);
}
