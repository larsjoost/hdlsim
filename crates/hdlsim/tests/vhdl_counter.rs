//! End-to-end test: compile and simulate `examples/vhdl-counter` (VHDL) through
//! the public library API and check that the self-checking testbench reports
//! PASS and finishes on its own via `std.env.finish`.

use hdl_ir::elab;
use hdl_sim::{Observer, SimEvent, Simulator};
use hdlsim::compile::compile_paths;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Collects `report`/`$display` messages from the observer bus.
struct Messages(Arc<Mutex<Vec<String>>>);

impl Observer for Messages {
    fn on_event(&mut self, ev: &SimEvent) {
        if let SimEvent::Message { text, .. } = ev {
            self.0.lock().unwrap().push(text.clone());
        }
    }
}

fn compile_vhdl_counter() -> hdlsim::compile::Compiled {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/vhdl-counter");
    let files = vec![dir.join("counter.vhd"), dir.join("tb_counter.vhd")];
    let compiled = compile_paths(&files);
    assert!(!compiled.has_errors(), "{}", compiled.render_diags());
    compiled
}

#[test]
fn vhdl_counter_testbench_passes() {
    let compiled = compile_vhdl_counter();
    let flat = elab::elaborate(&compiled.design, "tb_counter").unwrap();
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(1_000_000_000); // 1 us in fs
    let log = msgs.lock().unwrap().join("");
    assert!(log.contains("PASS"), "transcript was:\n{}", log);
    assert!(!log.contains("FAIL"), "transcript was:\n{}", log);
    assert!(
        log.contains("VHDL sim finished"),
        "transcript was:\n{}",
        log
    );
}

#[test]
fn vhdl_counter_vcd_trace_is_written() {
    let compiled = compile_vhdl_counter();
    let flat = elab::elaborate(&compiled.design, "tb_counter").unwrap();
    let mut sim = Simulator::new(flat);
    let out = std::env::temp_dir().join("hdlsim_vhdl_counter_test.vcd");
    let file = std::fs::File::create(&out).unwrap();
    let sink = hdl_wave::sinks::vcd::VcdSink::new(file, sim.flat());
    sim.add_observer(Box::new(hdl_wave::SinkAdapter {
        sink,
        flat: sim.flat().clone(),
    }));
    sim.run(1_000_000_000);
    let content = std::fs::read_to_string(&out).unwrap();
    assert!(content.contains("$enddefinitions"), "vcd header missing");
    assert!(
        content.contains("$var wire 8"),
        "8-bit q missing: {}",
        &content[..200]
    );
    assert!(
        content.matches('\n').count() > 20,
        "vcd too short — clock never toggled"
    );
    let _ = std::fs::remove_file(&out);
}
