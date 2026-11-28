//! Bounded model checking over the elaborated IR (`hdl_ir::elab::Flat`), backed
//! by the own CDCL solver in [`crate::sat`]. Powers `hdlsim prove`.
//!
//! Model: each step t in `0..=steps` is one clock edge — every edge-triggered
//! (`Sens::List`) process fires once per step; `always @*` processes and
//! continuous assigns are combinational and re-evaluated per step. Top-level
//! input ports and undriven signals are free inputs (fresh CNF vars per step).
//! State signals are the ones written by clocked processes or initial blocks;
//! step 0 is constrained by declaration init values / initial blocks (otherwise
//! the initial state is nondeterministic — sound for safety).
//!
//! Properties: `assert property (@(posedge clk) P)` / `ante |-> cons` /
//! `ante |=> cons` (Verilog SVA-lite) and VHDL `assert cond` inside processes
//! lower to `sva_assert`/`assert` system calls; they are extracted into
//! `Property { guards, check, delay }` and a violation at step t is
//! `(∧ guards(t)) ∧ ¬check(t + delay)`. The BMC formula asserts the OR of all
//! violations; SAT gives a counterexample trace, UNSAT means "no violation
//! within `steps` steps".
//!
//! 2-valued encoding (no X/Z): X/Z literal bits encode as 0, `rising_edge` is
//! constant true and `falling_edge` constant false (every step is an edge),
//! division by zero yields 0. Supported statements: blocks, if/case, blocking
//! and non-blocking assigns (incl. part-select/concat LHS), for/repeat with
//! constant trip counts, user function calls (inlined). Unsupported (clear
//! `Err`, never a wrong answer): memories, non-constant indices/selects,
//! `#delay`/`@wait` in executed processes, system tasks other than the property
//! asserts, dynamic loop bounds.
//!
//! Invariants:
//! - Every signal is classified exactly once as state, combinational, or input.
//! - Combinational drivers form an acyclic graph (checked; cycles are an error).
//! - A signal has at most one clocked driver and at most one comb driver.
//! - Never panics on design content; all failures are `Err(String)`.

use crate::sat::{is_neg, neg, var_of, Lit};
use hdl_ir::elab::{Flat, FlatSigId};
use hdl_ir::{Expr, SigKind};
mod bitvec;
mod builder;
mod classify;
mod const_fold;
mod encode;
mod exec;
mod exec_expr;
mod expr_util;
mod property;
mod stmt_scan;

use bitvec::or_bits;

use classify::classify;
use encode::Encoder;
use exec::Exec;
use property::extract_properties;

pub use property::Property;

/// Outcome of a BMC run.
#[derive(Clone, Debug)]
pub enum ProveStatus {
    /// No property violated within the bound (not a proof of safety).
    Safe,
    /// Property index (into [`ProveResult::properties`]) fails at step.
    Violation(usize, usize),
}

/// Result of [`prove`].
#[derive(Clone, Debug)]
pub struct ProveResult {
    pub status: ProveStatus,
    pub steps: usize,
    /// One origin string per property.
    pub properties: Vec<String>,
    /// Counterexample (only on violation): per step, `(signal name, value)`
    /// for all input and state signals, sorted by name.
    pub trace: Vec<Vec<(String, u64)>>,
}

/// Run bounded model checking on an elaborated design for `steps` clock edges.
pub fn prove(flat: &Flat, steps: usize) -> Result<ProveResult, String> {
    let cls = classify(flat)?;
    let props = extract_properties(flat, &cls)?;
    if props.is_empty() {
        return Err(
            "prove: no `assert property` / `assert` found in the design to check".to_string(),
        );
    }
    let mut enc = Encoder::new(flat, cls, steps)?;
    for t in 0..=steps {
        enc.encode_comb(t)?;
        if t == 0 {
            enc.encode_init()?;
        }
        if t < steps {
            enc.encode_transition(t)?;
        }
    }
    let violations = encode_violations(&mut enc, &props, steps)?;
    // Assert the disjunction of all violations: SAT = some violation holds.
    let mut bad = enc.b.false_lit();
    for (_, _, v) in &violations {
        bad = enc.b.or_lit(bad, *v);
    }
    enc.b.clause(&[bad]);
    let properties: Vec<String> = props.iter().map(|p| p.origin.clone()).collect();
    let model = match enc.b.s.solve() {
        // UNSAT: no violation within the bound.
        None => return Ok(safe_result(properties, steps)),
        Some(m) => m,
    };
    let lit_val = |l: Lit| -> bool { model[var_of(l)] != is_neg(l) };
    let mut violated = (0usize, 0usize);
    for (pi, t, v) in &violations {
        if lit_val(*v) {
            violated = (*pi, *t);
            break;
        }
    }
    let trace = build_cex_trace(&enc, &lit_val, steps);
    Ok(ProveResult {
        status: ProveStatus::Violation(violated.0, violated.1),
        steps,
        properties,
        trace,
    })
}

/// The result for an UNSAT solve: no property violated within the bound.
fn safe_result(properties: Vec<String>, steps: usize) -> ProveResult {
    ProveResult {
        status: ProveStatus::Safe,
        steps,
        properties,
        trace: Vec::new(),
    }
}

/// Violation literals per (property, trigger step): the guards' conjunction
/// at step t AND-NOT the check at step t + delay.
fn encode_violations(
    enc: &mut Encoder,
    props: &[Property],
    steps: usize,
) -> Result<Vec<(usize, usize, Lit)>, String> {
    let mut violations: Vec<(usize, usize, Lit)> = Vec::new();
    for (pi, p) in props.iter().enumerate() {
        let last = steps.saturating_sub(p.delay);
        for t in 0..=last {
            let g = {
                let mut ex = Exec::at(enc, t);
                let mut g = ex.enc.b.true_lit();
                for guard in &p.guards {
                    let v = ex.eval(guard)?;
                    let gt = or_bits(&mut ex.enc.b, &v.bits);
                    g = ex.enc.b.and_lit(g, gt);
                }
                g
            };
            let chk_bits = {
                let mut ex = Exec::at(enc, t + p.delay);
                ex.eval(&p.check)?.bits
            };
            let bad = {
                let chk_t = or_bits(&mut enc.b, &chk_bits);
                enc.b.and_lit(g, neg(chk_t))
            };
            violations.push((pi, t, bad));
        }
    }
    Ok(violations)
}

/// Counterexample trace over inputs + state signals, sorted by name.
fn build_cex_trace(
    enc: &Encoder,
    lit_val: &impl Fn(Lit) -> bool,
    steps: usize,
) -> Vec<Vec<(String, u64)>> {
    let mut traced: Vec<FlatSigId> = enc.cls.input.clone();
    traced.extend(enc.cls.state.iter().copied());
    traced.sort_by_key(|s| enc.flat.signals[*s as usize].name.clone());
    let mut trace = Vec::with_capacity(steps + 1);
    for t in 0..=steps {
        let mut row: Vec<(String, u64)> = Vec::new();
        for s in &traced {
            let name = enc.flat.signals[*s as usize].name.clone();
            let mut v: u64 = 0;
            for (i, &l) in enc.lits[t][*s as usize].iter().enumerate() {
                if i < 64 && lit_val(l) {
                    v |= 1 << i;
                }
            }
            row.push((name, v));
        }
        trace.push(row);
    }
    trace
}

/// Wrap `top` in a synthetic module `__hdlsim_prove_top` whose ports are
/// fresh top-level wires (they become the free BMC inputs). After calling
/// this, elaborate `__hdlsim_prove_top` (with a [`hdl_ir::elab::UnitResolver`]
/// if libraries are involved) and run [`prove`] on the result.
pub fn wrap_top(design: &mut hdl_ir::Design, top: &str) -> Result<(), String> {
    use hdl_ir::ir::{Conn, InstIr, ModuleIr, SignalIr};
    let mi = design
        .modules
        .iter()
        .position(|m| m.name == top)
        .ok_or_else(|| format!("prove: top module '{}' not found", top))?;
    let m = &design.modules[mi];
    let mut signals: Vec<SignalIr> = Vec::new();
    let mut conns: Vec<Conn> = Vec::new();
    for port in &m.ports {
        let ps = &m.signals[port.sig as usize];
        signals.push(SignalIr {
            name: port.name.clone(),
            width: ps.width,
            kind: SigKind::Wire,
            init: None,
            mem: None,
            signed: ps.signed,
        });
        conns.push(Conn::Named(
            port.name.clone(),
            Expr::Sig((signals.len() - 1) as u32),
        ));
    }
    let wrapper = ModuleIr {
        name: "__hdlsim_prove_top".to_string(),
        params: Vec::new(),
        ports: Vec::new(),
        signals,
        assigns: Vec::new(),
        processes: Vec::new(),
        functions: Vec::new(),
        instances: vec![InstIr {
            module: top.to_string(),
            name: "dut".to_string(),
            param_overrides: Vec::new(),
            conns,
            config: None,
        }],
        time_unit_fs: m.time_unit_fs,
        is_vhdl: m.is_vhdl,
        // The DUT's own real-field layout drives its signals' flat ranges.
        real_ranges: m.real_ranges.clone(),
        sig_dims_debug: None,
    };
    design.modules.push(wrapper);
    Ok(())
}

// ---------------------------------------------------------------------------
// Symbolic execution
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::Design;
    use hdl_syntax::diagnostic::Severity;
    use hdl_syntax::source::SourceDb;

    use hdl_ir::elab::elaborate;

    /// Parse + lower a Verilog source string into a Design.
    fn lower_top(src: &str, top: &str) -> Design {
        let mut db = SourceDb::new();
        let file = db.add_file("test.v", src).expect("add_file");
        let parsed = hdl_syntax::verilog::parser::parse_file(file, db.file(file));
        assert!(
            parsed.diags.iter().all(|d| d.severity != Severity::Error),
            "parse errors: {:?}",
            parsed.diags
        );
        let mut diags = Vec::new();
        let modules = hdl_ir::verilog_lower::lower_file(file, &parsed.root, &db, &mut diags);
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "lower errors: {:?}",
            diags
        );
        Design {
            modules,
            top: Some(top.to_string()),
            packages: Default::default(),
            pkg_funcs: Default::default(),
            configurations: Default::default(),
        }
    }

    /// Parse + lower + wrap + elaborate + prove a Verilog source string.
    fn prove_src(src: &str, top: &str, steps: usize) -> Result<ProveResult, String> {
        let mut design = lower_top(src, top);
        wrap_top(&mut design, top)?;
        let flat = elaborate(&design, "__hdlsim_prove_top")
            .map_err(|e| format!("elaboration error: {}", e))?;
        prove(&flat, steps)
    }

    #[test]
    fn prove_safe_register() {
        // q <= d; an 8-bit q is always < 256 → safe.
        let src = "module dut(input wire clk, input wire [7:0] d, output reg [7:0] q);\n\
                   always @(posedge clk) q <= d;\n\
                   assert property (@(posedge clk) q < 256);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 4).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }

    #[test]
    fn prove_finds_violation() {
        // q <= ~d, but the property claims q == d one cycle later → fails.
        let src = "module dut(input wire clk, input wire [7:0] d, output reg [7:0] q);\n\
                   always @(posedge clk) q <= ~d;\n\
                   assert property (@(posedge clk) d |=> q == d);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 4).expect("prove");
        match r.status {
            ProveStatus::Violation(pi, t) => {
                assert_eq!(pi, 0);
                assert!(t <= 3, "violation step {}", t);
                assert_eq!(r.trace.len(), 5);
                // In the counterexample, q at step 1 == ~d at step 0.
                let d0 = r.trace[0].iter().find(|(n, _)| n == "d").unwrap().1;
                let q1 = r.trace[1].iter().find(|(n, _)| n == "q").unwrap().1;
                assert_eq!(q1, (!d0) & 0xff, "cex: d0={} q1={}", d0, q1);
            }
            other => panic!("expected violation, got {:?}", other),
        }
    }

    #[test]
    fn prove_overlap_property_fails() {
        // b <= a; `a |-> b` must fail (b holds the old value at the same edge).
        let src = "module dut(input wire clk, input wire a, output reg b);\n\
                   always @(posedge clk) b <= a;\n\
                   assert property (@(posedge clk) a |-> b);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 3).expect("prove");
        assert!(
            matches!(r.status, ProveStatus::Violation(0, _)),
            "{:?}",
            r.status
        );
    }

    #[test]
    fn prove_counter_reaches_fifteen() {
        // 4-bit counter from 0: `cnt != 15` is violated exactly at step 15.
        let src = "module dut(input wire clk, output reg [3:0] cnt);\n\
                   initial cnt = 0;\n\
                   always @(posedge clk) cnt <= cnt + 1;\n\
                   assert property (@(posedge clk) cnt != 4'd15);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 20).expect("prove");
        match r.status {
            ProveStatus::Violation(0, t) => assert_eq!(t, 15, "cnt reaches 15 at step 15"),
            other => panic!("expected violation, got {:?}", other),
        }
    }

    #[test]
    fn prove_counter_bound_holds() {
        // Same counter: `cnt < 15` holds for steps 0..=14.
        let src = "module dut(input wire clk, output reg [3:0] cnt);\n\
                   initial cnt = 0;\n\
                   always @(posedge clk) cnt <= cnt + 1;\n\
                   assert property (@(posedge clk) cnt < 15);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 14).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }

    #[test]
    fn prove_comb_logic_safe() {
        // y = a & b; the property restates the assign → always safe.
        let src = "module dut(input wire clk, input wire [3:0] a, input wire [3:0] b, output wire [3:0] y);\n\
                   assign y = a & b;\n\
                   assert property (@(posedge clk) y == (a & b));\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 2).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }

    #[test]
    fn prove_comb_logic_violation() {
        // y = a & b but the property claims y == (a | b) → violated when
        // exactly one of a, b is nonzero.
        let src = "module dut(input wire clk, input wire [3:0] a, input wire [3:0] b, output wire [3:0] y);\n\
                   assign y = a & b;\n\
                   assert property (@(posedge clk) y == (a | b));\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 2).expect("prove");
        assert!(
            matches!(r.status, ProveStatus::Violation(0, 0)),
            "{:?}",
            r.status
        );
    }

    #[test]
    fn prove_rejects_memories() {
        let src = "module dut(input wire clk, input wire [3:0] d, output reg [7:0] q);\n\
                   reg [7:0] mem [0:15];\n\
                   always @(posedge clk) begin mem[0] <= d; q <= mem[0]; end\n\
                   assert property (@(posedge clk) q == 0);\n\
                   endmodule\n";
        assert!(prove_src(src, "dut", 2).unwrap_err().contains("memories"));
    }

    #[test]
    fn prove_rejects_delay() {
        let src = "module dut(input wire clk, output reg q);\n\
                   initial begin q = 0; #10 q = 1; end\n\
                   assert property (@(posedge clk) q == 0);\n\
                   endmodule\n";
        assert!(prove_src(src, "dut", 2).unwrap_err().contains("#delay"));
    }

    #[test]
    fn prove_arithmetic_and_case() {
        // 2-bit FSM-ish: state machine with case; assert state never 3.
        let src = "module dut(input wire clk, input wire rst, output reg [1:0] st);\n\
                   initial st = 0;\n\
                   always @(posedge clk) begin\n\
                     if (rst) st <= 0;\n\
                     else case (st)\n\
                       0: st <= 1;\n\
                       1: st <= 2;\n\
                       2: st <= 0;\n\
                       default: st <= 3;\n\
                     endcase\n\
                   end\n\
                   assert property (@(posedge clk) st != 2'd3);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 10).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }

    #[test]
    fn prove_async_reset() {
        // Async reset: st resets to 0 whenever rst=1; property st < 4 always
        // holds for a 2-bit reg → safe; also checks rst as a free input.
        let src = "module dut(input wire clk, input wire rst, output reg [1:0] st);\n\
                   always @(posedge clk or posedge rst) begin\n\
                     if (rst) st <= 0; else st <= st + 1;\n\
                   end\n\
                   assert property (@(posedge clk) st < 4);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 6).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }
}
