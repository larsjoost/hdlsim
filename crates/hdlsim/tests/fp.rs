//! End-to-end test: the fp-vhdl example in `examples/fp-vhdl` — hand-written
//! IEEE-754 fp32 adder and multiplier in VHDL, exercised by a testbench
//! resolved from the library (work = the fp example dir via hdlsim.ini).

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
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fp-vhdl");
    let mut mgr = LibraryManager::new();
    mgr.load_config(&dir.join("hdlsim.ini"))
        .expect("hdlsim.ini loads");
    let mut design = hdl_ir::Design::default();
    let flat = elab::elaborate_with(&mut design, top, &mut mgr).unwrap();
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(100_000_000_000); // 100 us in fs
    let log = msgs.lock().unwrap();
    log.join("")
}

#[test]
fn fp_testbench_passes_via_library() {
    let log = run_tb("tb_fp");
    assert!(
        log.contains("PASS: fp-vhdl add/mul all 10 cases correct") && !log.contains("FAIL"),
        "tb_fp:\n{}",
        log
    );
}
