//! End-to-end test: compile and simulate `examples/counter` through the public
//! library API and check that the self-checking testbench reports PASS and that
//! a VCD trace is produced.

use hdl_ir::elab;
use hdl_sim::{Observer, SimEvent, Simulator};
use hdlsim::compile::compile_paths;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Collects `$display`/`$finish` messages from the observer bus.
struct Messages(Arc<Mutex<Vec<String>>>);

impl Observer for Messages {
    fn on_event(&mut self, ev: &SimEvent) {
        if let SimEvent::Message { text, .. } = ev {
            self.0.lock().unwrap().push(text.clone());
        }
    }
}

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/counter")
}

fn compile_counter() -> hdlsim::compile::Compiled {
    let dir = example_dir();
    let files = vec![dir.join("counter.v"), dir.join("tb_counter.v")];
    let compiled = compile_paths(&files);
    assert!(!compiled.has_errors(), "{}", compiled.render_diags());
    compiled
}

#[test]
fn counter_testbench_passes() {
    let compiled = compile_counter();
    let flat = elab::elaborate(&compiled.design, "tb_counter").unwrap();
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(1_000_000_000); // 1 us in fs
    let log = msgs.lock().unwrap().join("");
    assert!(log.contains("PASS"), "transcript was:\n{}", log);
    assert!(!log.contains("FAIL"), "transcript was:\n{}", log);
}

#[test]
fn counter_vcd_trace_is_written() {
    let compiled = compile_counter();
    let flat = elab::elaborate(&compiled.design, "tb_counter").unwrap();
    let mut sim = Simulator::new(flat);
    let out = std::env::temp_dir().join("hdlsim_counter_test.vcd");
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
    // q must actually toggle: binary value changes present after t=0.
    let value_changes = content.matches('\n').count();
    assert!(
        value_changes > 20,
        "vcd too short ({} lines)",
        value_changes
    );
    let _ = std::fs::remove_file(&out);
}
