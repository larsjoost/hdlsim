//! End-to-end test: the RV32I CPU example in `examples/riscv-vhdl` — every
//! unit testbench (alu, decode, regfile) and every program testbench
//! (fib, sum 1..10, jal/jalr subroutine) must report PASS through hdlsim.
//!
//! The example is split into two libraries (`hdlsim.ini`: `rv32i = rtl`,
//! `work = tb`); this test deliberately passes *no* source files and lets
//! elaboration resolve the top testbench and all dependencies (cpu ->
//! alu/decode/regfile/imem/dmem) across the libraries on demand.

use hdl_ir::elab;
use hdl_lib::LibraryManager;
use hdl_sim::{Observer, SimEvent, Simulator};
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

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/riscv-vhdl")
}

fn run_tb(top: &str) -> String {
    let mut mgr = LibraryManager::new();
    mgr.load_config(&example_dir().join("hdlsim.ini"))
        .expect("hdlsim.ini loads");
    let mut design = hdl_ir::Design::default();
    // No explicit files: resolution must come from the libraries alone.
    let flat = elab::elaborate_with(&mut design, top, &mut mgr).unwrap();
    assert!(
        design.modules.iter().any(|m| m.name == top),
        "top '{}' was resolved from the libraries (no files given)",
        top
    );
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(10_000_000_000); // 10 us in fs
    let log = msgs.lock().unwrap();
    log.join("")
}

#[test]
fn riscv_all_testbenches_pass_via_libraries() {
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
