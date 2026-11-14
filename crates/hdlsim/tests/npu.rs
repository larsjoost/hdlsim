//! End-to-end test: the NPU example in `examples/npu-vhdl` — the 8x8 matvec
//! accelerator (unit test) and the RV32I cpu driving it over MMIO
//! (integration test), resolved across two libraries via hdlsim.ini
//! (work = the npu example dir, rv32i = ../riscv-vhdl/rtl).

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

fn run_tb(top: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/npu-vhdl");
    let mut mgr = LibraryManager::new();
    mgr.load_config(&dir.join("hdlsim.ini"))
        .expect("hdlsim.ini loads");
    let mut design = hdl_ir::Design::default();
    let flat = elab::elaborate_with(&mut design, top, &mut mgr).unwrap();
    if top == "tb_cpu_npu" {
        assert!(
            design.modules.iter().any(|m| m.name == "cpu"),
            "cpu was pulled from the rv32i library"
        );
    }
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(100_000_000_000); // 100 us in fs
    let log = msgs.lock().unwrap();
    log.join("")
}

#[test]
fn npu_testbenches_pass_via_libraries() {
    for top in ["tb_npu", "tb_cpu_npu"] {
        let log = run_tb(top);
        assert!(
            log.contains("PASS") && !log.contains("FAIL"),
            "{}:\n{}",
            top,
            log
        );
    }
}
