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
use hdl_ir::value::{BinOp, UnOp, V1};
use hdl_ir::{Expr, SigKind};
mod bitvec;
mod builder;
mod classify;
mod encode;
mod exec;
mod exec_expr;
mod expr_util;
mod property;
mod stmt_scan;

use bitvec::or_bits;

use classify::classify;
use encode::Encoder;
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
    // Violation literals per (property, trigger step).
    let mut violations: Vec<(usize, usize, Lit)> = Vec::new();
    for (pi, p) in props.iter().enumerate() {
        let last = steps.saturating_sub(p.delay);
        for t in 0..=last {
            let g = {
                let mut ex = Exec::at(&mut enc, t);
                let mut g = ex.enc.b.true_lit();
                for guard in &p.guards {
                    let v = ex.eval(guard)?;
                    let gt = or_bits(&mut ex.enc.b, &v.bits);
                    g = ex.enc.b.and_lit(g, gt);
                }
                g
            };
            let chk_bits = {
                let mut ex = Exec::at(&mut enc, t + p.delay);
                ex.eval(&p.check)?.bits
            };
            let bad = {
                let chk_t = or_bits(&mut enc.b, &chk_bits);
                enc.b.and_lit(g, neg(chk_t))
            };
            violations.push((pi, t, bad));
        }
    }
    let mut bad = enc.b.false_lit();
    for (_, _, v) in &violations {
        bad = enc.b.or_lit(bad, *v);
    }
    enc.b.clause(&[bad]);
    let properties: Vec<String> = props.iter().map(|p| p.origin.clone()).collect();
    let model = match enc.b.s.solve() {
        None => {
            return Ok(ProveResult {
                status: ProveStatus::Safe,
                steps,
                properties,
                trace: Vec::new(),
            });
        }
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
    // Counterexample trace over inputs + state.
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
    Ok(ProveResult {
        status: ProveStatus::Violation(violated.0, violated.1),
        steps,
        properties,
        trace,
    })
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

use exec::Exec;

/// A compile-time constant value (used for indices / loop bounds).
struct CVal {
    bits: Vec<bool>,
    signed: bool,
}

impl<'e, 'a> Exec<'e, 'a> {
    // --- constant folding (no gates) for indices / loop bounds ---

    pub(in crate::bmc) fn const_u64(&self, e: &Expr) -> Option<u64> {
        let v = self.const_val(e)?;
        let mut acc: u64 = 0;
        for (i, &b) in v.bits.iter().enumerate() {
            if i < 64 && b {
                acc |= 1 << i;
            }
        }
        Some(acc)
    }

    fn const_val(&self, e: &Expr) -> Option<CVal> {
        match e {
            Expr::Lit(v) => Some(CVal {
                bits: v.bits.iter().map(|&b| b == V1).collect(),
                signed: false,
            }),
            Expr::Sig(s) => {
                let lits = if let Some(l) = &self.locals {
                    l.get(s).cloned()
                } else {
                    self.env.get(s).cloned()
                }?;
                let bits: Vec<bool> = lits
                    .iter()
                    .map(|&l| {
                        if l == self.enc.b.true_lit() {
                            Some(true)
                        } else if l == self.enc.b.false_lit() {
                            Some(false)
                        } else {
                            None
                        }
                    })
                    .collect::<Option<Vec<bool>>>()?;
                Some(CVal {
                    bits,
                    signed: self.enc.flat.signals[*s as usize].signed,
                })
            }
            Expr::Bin { op, l, r } => {
                let lv = self.const_val(l)?;
                let rv = self.const_val(r)?;
                let w = lv.bits.len().max(rv.bits.len()).max(1);
                let signed = lv.signed || rv.signed;
                let l2 = const_extend(&lv.bits, w, signed);
                let r2 = const_extend(&rv.bits, w, signed);
                let bits = const_binop(*op, &l2, &r2, w, signed)?;
                Some(CVal { bits, signed })
            }
            Expr::Un { op, e } => {
                let v = self.const_val(e)?;
                Some(CVal {
                    bits: const_unop(*op, &v.bits)?,
                    signed: false,
                })
            }
            Expr::Cond { c, t, e } => {
                let cv = self.const_val(c)?;
                if const_truthy(&cv.bits) {
                    self.const_val(t)
                } else {
                    self.const_val(e)
                }
            }
            Expr::Resize { e, width, signed } => {
                let v = self.const_val(e)?;
                Some(CVal {
                    bits: const_extend(&v.bits, *width, *signed),
                    signed: *signed,
                })
            }
            Expr::Signed(e) => {
                let mut v = self.const_val(e)?;
                v.signed = true;
                Some(v)
            }
            _ => None,
        }
    }
}

/// If all bits are constant literals, return them as bools.
pub(in crate::bmc) fn const_bits(bits: &[Lit], ex: &Exec) -> Option<Vec<bool>> {
    bits.iter()
        .map(|&l| {
            if l == ex.enc.b.true_lit() {
                Some(true)
            } else if l == ex.enc.b.false_lit() {
                Some(false)
            } else {
                None
            }
        })
        .collect()
}

pub(in crate::bmc) fn const_truthy(bits: &[bool]) -> bool {
    bits.iter().any(|&b| b)
}

fn const_extend(bits: &[bool], w: usize, signed: bool) -> Vec<bool> {
    let mut out = bits.to_vec();
    if out.len() > w {
        out.truncate(w);
    } else {
        let fill = signed && out.last().copied().unwrap_or(false);
        while out.len() < w {
            out.push(fill);
        }
    }
    out
}

fn mask(w: usize) -> u64 {
    if w >= 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    }
}

fn const_binop(op: BinOp, a: &[bool], b: &[bool], w: usize, signed: bool) -> Option<Vec<bool>> {
    let to_u = |v: &[bool]| -> u64 {
        let mut x = 0u64;
        for (i, &bit) in v.iter().enumerate() {
            if i < 64 && bit {
                x |= 1 << i;
            }
        }
        x
    };
    let to_i = |v: &[bool]| -> i128 {
        let mut x: i128 = 0;
        for (i, &bit) in v.iter().enumerate() {
            if bit {
                x |= 1 << i;
            }
        }
        if signed && w < 128 && v.get(w - 1).copied().unwrap_or(false) {
            x |= -1i128 << w;
        }
        x
    };
    let bits_of = |v: u64| -> Vec<bool> { (0..w).map(|i| (v >> i) & 1 == 1).collect() };
    let r = match op {
        BinOp::Add => to_u(a).wrapping_add(to_u(b)),
        BinOp::Sub => to_u(a).wrapping_sub(to_u(b)),
        BinOp::Mul => to_u(a).wrapping_mul(to_u(b)),
        BinOp::Div => to_u(a).checked_div(to_u(b)).unwrap_or(0),
        BinOp::Mod => {
            let (x, y) = (to_u(a), to_u(b));
            if y == 0 {
                0
            } else {
                x % y
            }
        }
        BinOp::Shl => to_u(a).wrapping_shl(to_u(b) as u32),
        BinOp::Shr => to_u(a).wrapping_shr(to_u(b) as u32),
        BinOp::Sar => {
            let sh = (to_u(b) as u32) % 64;
            if signed {
                ((to_i(a) >> sh) as u64) & mask(w)
            } else {
                to_u(a) >> sh
            }
        }
        _ => {
            return Some(match op {
                BinOp::Lt => vec![if signed {
                    to_i(a) < to_i(b)
                } else {
                    to_u(a) < to_u(b)
                }],
                BinOp::Le => vec![if signed {
                    to_i(a) <= to_i(b)
                } else {
                    to_u(a) <= to_u(b)
                }],
                BinOp::Gt => vec![if signed {
                    to_i(a) > to_i(b)
                } else {
                    to_u(a) > to_u(b)
                }],
                BinOp::Ge => vec![if signed {
                    to_i(a) >= to_i(b)
                } else {
                    to_u(a) >= to_u(b)
                }],
                BinOp::Eq | BinOp::EqStrict => vec![a == b],
                BinOp::Ne | BinOp::NeStrict => vec![a != b],
                BinOp::BAnd => (0..w).map(|i| a[i] && b[i]).collect(),
                BinOp::BOr => (0..w).map(|i| a[i] || b[i]).collect(),
                BinOp::BXor => (0..w).map(|i| a[i] != b[i]).collect(),
                BinOp::BNand => (0..w).map(|i| !(a[i] && b[i])).collect(),
                BinOp::BNor => (0..w).map(|i| !(a[i] || b[i])).collect(),
                BinOp::BXnor => (0..w).map(|i| a[i] == b[i]).collect(),
                BinOp::LogAnd => vec![to_u(a) != 0 && to_u(b) != 0],
                BinOp::LogOr => vec![to_u(a) != 0 || to_u(b) != 0],
                _ => return None,
            });
        }
    };
    Some(bits_of(r))
}

fn const_unop(op: UnOp, a: &[bool]) -> Option<Vec<bool>> {
    let w = a.len().max(1);
    Some(match op {
        UnOp::Not => vec![!a.iter().any(|&b| b)],
        UnOp::BNot => a.iter().map(|&b| !b).collect(),
        UnOp::Neg => {
            let mut x = 0u64;
            for (i, &b) in a.iter().enumerate() {
                if i < 64 && b {
                    x |= 1 << i;
                }
            }
            (0..w).map(|i| (x.wrapping_neg() >> i) & 1 == 1).collect()
        }
        UnOp::Pos => a.to_vec(),
        UnOp::Abs => {
            // two's-complement absolute value
            let sign = a.last().copied().unwrap_or(false);
            if sign {
                let mut x = 0u64;
                for (i, &b) in a.iter().enumerate() {
                    if i < 64 && b {
                        x |= 1 << i;
                    }
                }
                (0..a.len())
                    .map(|i| (x.wrapping_neg() >> i) & 1 == 1)
                    .collect()
            } else {
                a.to_vec()
            }
        }
        UnOp::RedAnd => vec![a.iter().all(|&b| b)],
        UnOp::RedOr => vec![a.iter().any(|&b| b)],
        UnOp::RedXor => vec![a.iter().fold(false, |acc, &b| acc != b)],
        UnOp::RedNand => vec![!a.iter().all(|&b| b)],
        UnOp::RedNor => vec![!a.iter().any(|&b| b)],
        UnOp::RedXnor => vec![!a.iter().fold(false, |acc, &b| acc != b)],
    })
}

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
