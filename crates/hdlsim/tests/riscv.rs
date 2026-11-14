//! End-to-end test: the RV32I CPU example in `examples/riscv-vhdl` — every
//! unit testbench (alu, decode, regfile) and every program testbench
//! (fib, sum 1..10, jal/jalr subroutine) must report PASS through hdlsim.

use hdl_ir::elab;
use hdl_sim::{Observer, SimEvent, Simulator};
use hdlsim::compile::compile_paths;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Messages(Arc<Mutex<Vec<String>>>);

impl Observer for Messages {
    fn on_event(&mut self, ev: &SimEvent) {
        if let SimEvent::Message { text, .. } = ev {
            self.0.lock().unwrap().push(text.clone());
        }
    }
}

fn riscv_files() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/riscv-vhdl");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map_or(false, |e| e == "vhd"))
        .collect();
    files.sort();
    files
}

fn run_tb(top: &str) -> String {
    let compiled = compile_paths(&riscv_files());
    assert!(!compiled.has_errors(), "{}", compiled.render_diags());
    let flat = elab::elaborate(&compiled.design, top).unwrap();
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(10_000_000_000); // 10 us in fs
    let log = msgs.lock().unwrap();
    log.join("")
}

#[test]
fn riscv_all_testbenches_pass() {
    let tops = [
        "tb_alu",
        "tb_decode",
        "tb_regfile",
        "tb_cpu_fib",
        "tb_cpu_sum",
        "tb_cpu_jal",
    ];
    let mut failures = Vec::new();
    for top in tops {
        let log = run_tb(top);
        if !log.contains("PASS") || log.contains("FAIL") {
            failures.push(format!("{}:\n{}", top, log));
        }
    }
    assert!(
        failures.is_empty(),
        "failing testbenches:\n{}",
        failures.join("\n")
    );
}
