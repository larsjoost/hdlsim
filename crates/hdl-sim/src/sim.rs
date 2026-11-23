//! The simulation kernel: event-driven, delta-cycle based.
//!
//! Semantics (Verilog side first; VHDL lowers onto the same structures):
//! - Processes scheduled in an active queue; non-blocking assigns collect into
//!   an NBA list applied after the active queue drains (next delta).
//! - Continuous assigns re-evaluate to a fixpoint each delta.
//! - When nothing is active, time advances to the next scheduled event.
//! - `#delay` suspends a process via an explicit frame stack (owned clones-free:
//!   statements are moved out of frames with `mem::replace`), resumed by a
//!   time event.
//!
//! Invariants:
//! - No allocation in the steady-state event loop beyond queue pushes.
//! - Deterministic: fixed RNG seed, FIFO active queue, BTreeMap time queue.
//! - Never panics on design content; runtime errors are Message events.

use crate::events::{Observer, SimEvent};
use crate::rand::{self, Rng};
use hdl_ir::elab::{Flat, FlatSigId};

mod ckpt;
use hdl_ir::value::{Value, V0, V1, VDASH, VU, VX, VZ};

/// Pack a string as a Value: first char in the high byte, MSB-first — the same
/// layout as VHDL/Verilog string literals.
fn pack_str_value(s: &str) -> Value {
    let bytes = s.as_bytes();
    let n = bytes.len().max(1);
    let mut bits = vec![V0; n * 8];
    for (bi, &b) in bytes.iter().enumerate() {
        for j in 0..8 {
            bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
        }
    }
    Value { bits, real: false }
}

/// Inverse of [`pack_str_value`]: bytes of a packed string Value → String.
fn unpack_str_value(v: &Value) -> String {
    let n = v.bits.len() / 8;
    let mut s = String::new();
    for bi in 0..n {
        let mut b = 0u8;
        for j in 0..8 {
            if v.bits.get((n - 1 - bi) * 8 + j) == Some(&V1) {
                b |= 1 << j;
            }
        }
        s.push(b as char);
    }
    s
}

/// VHDL `to_hstring`/`to_ostring`/`to_bstring` of a value (MSB-first digits).
fn vhdl_radix_string(v: &Value, radix: u32) -> String {
    let bits_per = match radix {
        16 => 4,
        8 => 3,
        _ => 1,
    };
    let w = v.bits.len();
    let pad = (bits_per - (w % bits_per)) % bits_per;
    let mut out = String::new();
    let mut i = w + pad;
    while i > 0 {
        i -= bits_per;
        let mut digit = 0u32;
        let mut known = true;
        for j in 0..bits_per {
            let idx = i + j;
            let b = if idx < w { v.bits[idx] } else { V0 };
            if b == V1 {
                digit |= 1 << j;
            } else if b != V0 {
                known = false;
            }
        }
        if radix == 2 {
            out.push(if known {
                if digit == 1 {
                    '1'
                } else {
                    '0'
                }
            } else {
                'X'
            });
        } else if known {
            out.push(char::from_digit(digit, radix).unwrap().to_ascii_uppercase());
        } else {
            out.push('X');
        }
    }
    out
}
use hdl_ir::fmt_time;
use hdl_ir::{BinOp, Edge, EdgeKind, Expr, ProcKind, Sens, SigId, SigKind, Stmt, UnOp};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// Scheduled future event.
#[derive(Clone, Debug)]
enum Ev {
    /// Resume a suspended process at a time.
    Resume(u32),
    /// `wait until ... for T` timeout: pop the process's `WaitUntilCheck`
    /// frame and resume it — but only if the frame's wake token matches
    /// (`gen`); stale timeouts after an identical earlier wait are ignored.
    WaitTimeout(u32, u64),
    /// A delayed signal transaction (VHDL `s <= v after t;`) landing at a
    /// time: applied in that time's NBA region.
    Nba(FlatSigId, Value, Option<u32>),
    /// Delayed part-write (indexed/sliced target) landing at a time.
    NbaExpr(Expr, Value, Option<u32>),
}

/// One frame of a running process's explicit continuation stack.
/// Statements are *moved out* of block frames when executed (`mem::replace`
/// with `Null`), so the steady-state loop does not clone statement bodies.
#[derive(Clone, Debug)]
enum Frame {
    /// A statement list + program counter.
    Block { block: Vec<Stmt>, pc: usize },
    /// For-loop marker, sitting *below* the body/step block frames. When the
    /// frame above it finishes, the kernel runs `step` and re-tests `cond`.
    /// `after_body` distinguishes "body just finished, run step" from
    /// "step just finished, re-test cond".
    ForIter {
        cond: Expr,
        step: Box<Stmt>,
        body: Box<Stmt>,
        after_body: bool,
    },
    /// Repeat-loop marker (`repeat (n) body`): `remaining` is the count
    /// evaluated once at loop entry. Same protocol as `ForIter`.
    RepeatIter {
        remaining: i64,
        body: Box<Stmt>,
        after_body: bool,
    },
    /// `wait until` check, on top of the frames while the process is parked.
    /// On an edge wake the kernel re-evaluates `cond` (and re-parks on false);
    /// on a timeout wake the kernel pops the frame directly. `gen` is a wake
    /// token: each park increments a counter, and a timeout event only pops
    /// the frame whose `gen` matches — stale timeouts from an earlier wait
    /// (same pid, same deadline) are ignored.
    WaitUntilCheck {
        cond: Option<Expr>,
        edges: Vec<Edge>,
        deadline: u64,
        gen: u64,
    },
}

impl Frame {
    fn block(block: Vec<Stmt>) -> Frame {
        Frame::Block { block, pc: 0 }
    }
}

#[derive(Default)]
struct ProcRt {
    frames: Vec<Frame>,
}

enum Control {
    Done,
    Suspend(u64),
    /// Park the process until one of the listed signal edges occurs.
    Wait,
    /// `break;`: pop frames up to and including the innermost loop marker.
    Break,
    /// `continue;`: pop frames above the innermost loop marker and run its
    /// step / next iteration.
    Continue,
}

pub struct Simulator {
    flat: Flat,
    vals: Vec<Value>,
    prev: Vec<Value>,
    mems: Vec<Vec<Value>>, // per memory signal: mems[sig] if is_mem
    is_mem: Vec<bool>,
    active: VecDeque<u32>,
    nba: Vec<(FlatSigId, Value, Option<u32>)>,
    time_events: BTreeMap<u64, Vec<Ev>>,
    time: u64,
    proc_sens: Vec<Vec<(FlatSigId, EdgeKind)>>,
    proc_all: Vec<Vec<FlatSigId>>,
    proc_rt: Vec<ProcRt>,
    /// Function/procedure registry: name → indices into `flat.functions`.
    /// Overloads share a bare name, so lookups with several candidates
    /// resolve by formal count (`func_match`).
    funcs: HashMap<String, Vec<usize>>,
    observers: Vec<Box<dyn Observer>>,
    finished: bool,
    stopped: bool,
    started: bool,
    finish_notified: bool,
    /// STATUS argument of VHDL `std.env.finish(status)` (VHDL-2008). Verilog
    /// `$finish(n)` does not set this — its argument is a diagnostic level.
    finish_status: Option<i32>,
    /// Number of test failures seen (severity error/failure, `$error`/`$fatal`).
    failures: u64,
    /// Wake-token counter for `wait until` parks (see `Frame::WaitUntilCheck`).
    wait_gen: u64,
    /// `HDLSIM_TRACE=1` — stderr execution trace of wait/wake scheduling.
    trace_on: bool,
    monitor: Vec<Vec<Expr>>,
    monitor_last: Vec<Option<Vec<Value>>>,
    delta_guard: u32,
    /// Interpreter steps in the current process activation — runaway-loop
    /// budget (see `exec_frames`). Override: `HDLSIM_MAX_PROC_STEPS`.
    proc_steps: u64,
    max_proc_steps: u64,
    /// Nesting depth of user function/procedure calls — runaway-recursion
    /// guard (see `run_func`). Override: `HDLSIM_MAX_FUNC_DEPTH`.
    fn_depth: std::cell::Cell<usize>,
    max_func_depth: usize,
    /// Latched when `fn_depth` passes the budget: the name of the function
    /// at detection. The pump reports it and stops the simulation.
    fn_recursion: std::cell::RefCell<Option<String>>,
    /// Function/procedure calls in the current process activation — runaway
    /// guard for loops whose body only calls functions (the per-activation
    /// interpreter-step budget in `exec_frames` cannot see inside one
    /// statement). Override: `HDLSIM_MAX_FN_CALLS`.
    fn_calls: std::cell::Cell<u64>,
    max_fn_calls: u64,
    /// Latched when `fn_calls` passes the budget: (process origin, function
    /// name). The pump reports it and stops the simulation.
    fn_runaway: std::cell::RefCell<Option<(String, String)>>,
    /// Fast-path flag mirroring `fn_runaway.is_some()`: once latched, every
    /// function call unwinds with X immediately instead of continuing the
    /// runaway computation.
    fn_runaway_hit: std::cell::Cell<bool>,
    /// Function index at call-tree depth 1 (the root of the current call
    /// tree) — reported with the runaway latch to identify the entry point.
    cur_root_fn: std::cell::Cell<Option<usize>>,
    /// Active function call chain (indices, innermost last) — captured when
    /// the runaway latch fires so the report can show the recursion cycle.
    fn_stack: std::cell::RefCell<Vec<usize>>,
    /// Names of the call chain captured at latch time (`fn_stack` clone).
    fn_chain: std::cell::RefCell<Vec<String>>,
    /// Graceful memory guard (`HDLSIM_MAX_MEM_MB` in MiB): exceeding it
    /// stops the run with an actionable diagnostic instead of an OOM kill.
    max_mem_kb: Option<u64>,
    /// Throttle counter for the guard's `/proc` reads.
    mem_checks: std::cell::Cell<u64>,
    /// Stopped by an internal error guard (combinational loop, delta/step
    /// limit): the run must fail the process exit code, not exit 0.
    error_stopped: bool,
    /// Deferred NBA writes to non-scalar targets (part-selects, memories).
    nba_exprs: Vec<(Expr, Value, Option<u32>)>,
    /// Process currently executing (for driver tracing); `None` outside
    /// processes, `Some(u32::MAX)` while re-evaluating continuous assigns.
    cur_proc: Option<u32>,
    /// Per-signal last driver: (proc, time_fs). proc == u32::MAX = assign.
    last_driver: Vec<Option<(u32, u64)>>,
    /// "Parked at a wait" per process — O(1) park check and wake. The wait's
    /// wake edges live in `sens_index` while parked (see `park`).
    parked: Vec<bool>,
    /// Inverse of the processes' CURRENT sensitivity: signal → (proc, edge)
    /// entries woken by a change on it. Non-parked processes contribute their
    /// declared list; parked ones the edges of the wait they are parked on
    /// (see `park`). Scheduling a changed signal is O(fanout) instead of
    /// scanning every process's sensitivity list — the dominant cost on big
    /// designs.
    sens_index: Vec<Vec<(u32, EdgeKind)>>,
    /// Each process's entries currently in `sens_index` — `park` swaps them
    /// out for the parked wait's edges; wake swaps them back.
    cur_sens: Vec<Vec<(FlatSigId, EdgeKind)>>,
    /// "Already queued in `active`" per process — O(1) activate dedupe.
    in_active: Vec<bool>,
    /// RNG state for $random (Cell so expression evaluation can stay &self).
    rng: std::cell::Cell<u64>,
    /// Object memory signal for SV classes: class name → `__objmem_<Class>`
    /// (used for the object word width).
    objmem: HashMap<String, FlatSigId>,
    /// Live class objects: class name → one word per allocated object.
    /// RefCell so field writes work from &self (function bodies) too.
    objs: std::cell::RefCell<HashMap<String, Vec<Value>>>,
    /// std.textio read positions: file path → lines already consumed by
    /// `readline` (RefCell so `endfile` can peek from &self expression eval).
    textio_pos: std::cell::RefCell<HashMap<String, usize>>,
    /// Dynamically opened files: `$file:…` marker (or a literal-path file's
    /// path) → the actual filesystem path bound by `$textio_file_open`.
    open_files: HashMap<String, String>,
    /// Current object handle while executing a class method (`$this`).
    cur_obj: std::cell::Cell<u64>,
    /// Index of the function whose frame is currently executing (`None`
    /// outside functions) — lets dispatch detect a self-call.
    cur_fn: std::cell::Cell<Option<usize>>,
    /// External stop request (e.g. the live wave window's Stop button):
    /// checked once per event-loop iteration in `run`.
    stop_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// External-interrupt flag (SIGINT/SIGTERM via the CLI): when set, the
    /// pump dumps the current location and ends the run with `interrupt_rc`.
    /// Separate from `stop_flag`, whose set just finishes quietly (live
    /// viewer Stop button).
    interrupt_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Exit code for an interrupt stop (130 = shell convention for SIGINT).
    interrupt_rc: i32,
    /// Wall-clock budget for the whole run (`--max-wall`), checked in pump.
    wall_budget: Option<std::time::Duration>,
    /// First pump entry — anchors wall-budget and heartbeat measurements.
    wall_start: Option<std::time::Instant>,
    /// Last pump-visible time advance / heartbeat print (heartbeat only).
    last_advance: Option<std::time::Instant>,
    last_heartbeat: Option<std::time::Instant>,
    /// Set when the run ended via signal or wall budget: (reason, exit code).
    interrupt_info: Option<(String, i32)>,
    /// Most recently dispatched process — the between-events fallback for
    /// `where_am_i` (nothing is executing at the check point itself).
    last_proc: Option<u32>,
    /// Live fixpoint progress, shown by the heartbeat/`where_am_i` while the
    /// concurrent-assign sweep runs (cur_proc holds the u32::MAX sentinel):
    /// sweep number within the current fixpoint, and how many assigns
    /// changed in the previous sweep. A shrinking count = converging
    /// settling; a constant nonzero count = oscillating combinational loop.
    fix_sweep: u64,
    fix_changed: u64,
}

impl Simulator {
    /// Pre-allocation guard for [`Simulator::new`]: estimate each signal's
    /// footprint (a `Value` costs ~1 byte per bit; a memory repeats that per
    /// word) and refuse before touching the allocator when a single signal
    /// — or the total, when `HDLSIM_MAX_MEM_MB` is set — would exceed its
    /// cap. The error names the largest allocators so the design can be
    /// fixed: a width or depth in the GiB range is almost always a generic
    /// or range that did not elaborate (an unpropagated generic-map actual,
    /// a null range underflowing), and allocating it would otherwise abort
    /// or be OOM-killed (exit 137) before any RSS-based guard could run.
    ///
    /// Caps: `HDLSIM_MAX_SIGNAL_MB` per signal (default 4096 MiB — far above
    /// any legitimate unit-test or DUT sim, far below the runaway 2³²-width
    /// class); `HDLSIM_MAX_MEM_MB` (shared with the peak-RSS guard) for the
    /// total, only when set.
    pub fn alloc_report(flat: &Flat) -> Result<(), String> {
        const DEFAULT_MAX_SIGNAL_MB: u64 = 4096;
        let per_signal_limit = std::env::var("HDLSIM_MAX_SIGNAL_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_SIGNAL_MB)
            * 1024
            * 1024;
        // Shared with the peak-RSS guard's env contract: `HDLSIM_MAX_MEM_MB`
        // in MiB, unset or zero disables the total check.
        let total_limit = crate::meminfo::mem_limit_kb().map(|kb| kb * 1024);
        Self::alloc_report_with_caps(flat, per_signal_limit, total_limit)
    }

    /// `alloc_report` with explicit caps (bytes); the env-reading wrapper
    /// keeps this testable.
    fn alloc_report_with_caps(
        flat: &Flat,
        per_signal_limit: u64,
        total_limit: Option<u64>,
    ) -> Result<(), String> {
        let mut ests: Vec<(u64, usize)> = flat
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| {
                // A stored word costs its Value header (~24 bytes) plus 1
                // byte per bit — so even a 1-bit memory at 2³² words is
                // ~96 GiB of headers alone.
                let bytes_per_word = 24 + s.width.max(1) as u64;
                let words = s.mem.unwrap_or(0) as u64 + 1;
                (bytes_per_word.saturating_mul(words), i)
            })
            .collect();
        let total: u64 = ests.iter().map(|(e, _)| *e).sum();
        let over_signal = ests.iter().any(|(e, _)| *e > per_signal_limit);
        let over_total = total_limit.is_some_and(|l| total > l);
        if !over_signal && !over_total {
            return Ok(());
        }
        ests.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut report = String::new();
        if over_signal {
            report.push_str(&format!(
                "error: sim-init would allocate more than {} MB for a single signal \
                 (HDLSIM_MAX_SIGNAL_MB={}); largest allocators:\n",
                per_signal_limit / 1024 / 1024,
                per_signal_limit / 1024 / 1024
            ));
        } else {
            report.push_str(&format!(
                "error: sim-init would allocate ~{} MB (HDLSIM_MAX_MEM_MB={}); \
                 largest allocators:\n",
                total / 1024 / 1024,
                total_limit.unwrap() / 1024 / 1024
            ));
        }
        for (e, i) in ests.iter().take(8) {
            let s = &flat.signals[*i];
            report.push_str(&format!(
                "  {} : width {} bits, {} words, ~{} MB\n",
                s.name,
                s.width,
                s.mem.unwrap_or(0),
                (e / 1024 / 1024).max(1)
            ));
        }
        report.push_str(
            "a width or depth this large usually means a generic or range that did not \
             elaborate — check the generic map actuals of the instances above \
             (`hdlsim debug --dump` shows the elaborated widths); raise \
             HDLSIM_MAX_SIGNAL_MB / HDLSIM_MAX_MEM_MB only if the design is \
             genuinely this big\n",
        );
        Err(report)
    }

    pub fn new(flat: Flat) -> Simulator {
        let n = flat.signals.len();
        let np = flat.procs.len();
        let objmem: HashMap<String, FlatSigId> = flat
            .signals
            .iter()
            .enumerate()
            .filter(|(_, s)| s.name.starts_with("__objmem_"))
            .map(|(i, s)| (s.name["__objmem_".len()..].to_string(), i as FlatSigId))
            .collect();
        let mut sim = Simulator {
            vals: flat
                .signals
                .iter()
                .map(|s| s.init.clone().unwrap_or_else(|| Value::x(s.width.max(1))))
                .collect(),
            prev: vec![Value::x(1); n],
            mems: flat
                .signals
                .iter()
                .map(|s| match s.mem {
                    // A memory with an initializer: element 0 is the most
                    // significant part of the flat init value (aggregates pack
                    // parts[0] at the top).
                    Some(depth) => match &s.init {
                        Some(v) if v.width() >= s.width.max(1) * depth => {
                            let w = s.width.max(1);
                            let total = v.width();
                            (0..depth)
                                .map(|i| v.slice(total - 1 - i * w, total - (i + 1) * w))
                                .collect()
                        }
                        _ => vec![Value::x(s.width.max(1)); depth],
                    },
                    None => Vec::new(),
                })
                .collect(),
            is_mem: flat.signals.iter().map(|s| s.mem.is_some()).collect(),
            flat,
            active: VecDeque::new(),
            nba: Vec::new(),
            cur_proc: None,
            last_driver: vec![None; n],
            time_events: BTreeMap::new(),
            time: 0,
            proc_sens: Vec::new(),
            proc_all: Vec::new(),
            proc_rt: Vec::new(),
            funcs: HashMap::new(),
            observers: Vec::new(),
            finished: false,
            stopped: false,
            started: false,
            finish_notified: false,
            finish_status: None,
            failures: 0,
            wait_gen: 0,
            trace_on: std::env::var("HDLSIM_TRACE").is_ok_and(|v| v == "1"),
            rng: std::cell::Cell::new(0x243F6A8885A308D3),
            objmem,
            objs: std::cell::RefCell::new(HashMap::new()),
            textio_pos: std::cell::RefCell::new(HashMap::new()),
            open_files: HashMap::new(),
            cur_obj: std::cell::Cell::new(u64::MAX),
            cur_fn: std::cell::Cell::new(None),
            stop_flag: None,
            interrupt_flag: None,
            interrupt_rc: 130,
            wall_budget: None,
            wall_start: None,
            last_advance: None,
            last_heartbeat: None,
            interrupt_info: None,
            last_proc: None,
            fix_sweep: 0,
            fix_changed: 0,
            monitor: Vec::new(),
            monitor_last: Vec::new(),
            delta_guard: 0,
            proc_steps: 0,
            max_proc_steps: std::env::var("HDLSIM_MAX_PROC_STEPS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100_000_000),
            max_mem_kb: crate::meminfo::mem_limit_kb(),
            mem_checks: std::cell::Cell::new(0),
            fn_depth: std::cell::Cell::new(0),
            max_func_depth: std::env::var("HDLSIM_MAX_FUNC_DEPTH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(200),
            fn_recursion: std::cell::RefCell::new(None),
            fn_calls: std::cell::Cell::new(0),
            max_fn_calls: std::env::var("HDLSIM_MAX_FN_CALLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100_000_000),
            fn_runaway: std::cell::RefCell::new(None),
            fn_runaway_hit: std::cell::Cell::new(false),
            cur_root_fn: std::cell::Cell::new(None),
            fn_stack: std::cell::RefCell::new(Vec::new()),
            fn_chain: std::cell::RefCell::new(Vec::new()),
            error_stopped: false,
            nba_exprs: Vec::new(),
            parked: vec![false; np],
            sens_index: vec![Vec::new(); n],
            cur_sens: Vec::new(),
            in_active: vec![false; np],
        };
        sim.prev = sim.vals.clone();
        for (i, f) in sim.flat.functions.iter().enumerate() {
            let cands = sim.funcs.entry(f.name.to_ascii_lowercase()).or_default();
            // Stub shape: an interface-only spec declaration lowered without
            // a body (`Stmt::Null` or an empty block). Elaboration already
            // merges a spec stub with its same-signature body; what can
            // still coexist here are GENUINE overloads sharing a formal
            // count (`conv_float(integer)` vs `conv_float(unsigned)`), which
            // width dispatch separates at the call site. A body replaces a
            // stub of the same formal count; a stub never displaces a body.
            let is_stub = |j: usize| match &*sim.flat.functions[j].body {
                Stmt::Null => true,
                Stmt::Block(ss) => ss.is_empty(),
                _ => false,
            };
            let new_is_stub = is_stub(i);
            if new_is_stub
                && cands
                    .iter()
                    .any(|&j| sim.flat.functions[j].arity == f.arity && !is_stub(j))
            {
                continue;
            }
            match cands
                .iter()
                .position(|&j| sim.flat.functions[j].arity == f.arity && is_stub(j))
            {
                Some(pos) => cands[pos] = i,
                None => cands.push(i),
            }
        }
        sim.proc_rt = (0..sim.flat.procs.len())
            .map(|_| ProcRt::default())
            .collect();
        for (pid, p) in sim.flat.procs.iter().enumerate() {
            let list: Vec<(FlatSigId, EdgeKind)> = match &p.sens {
                Sens::List(es) => {
                    // Keep `proc_all` index-aligned with `procs`.
                    sim.proc_all.push(Vec::new());
                    es.iter().map(|e| (e.sig, e.kind)).collect()
                }
                Sens::All => {
                    let mut sigs = Vec::new();
                    p.body.collect_sigs(&mut sigs);
                    sigs.sort_unstable();
                    sigs.dedup();
                    sim.proc_all
                        .push(sigs.iter().map(|s| *s as FlatSigId).collect());
                    sigs.iter()
                        .map(|s| (*s as FlatSigId, EdgeKind::Any))
                        .collect()
                }
            };
            // Non-parked processes wake on their declared sensitivity.
            for (s, ek) in &list {
                sim.sens_index[*s as usize].push((pid as u32, *ek));
            }
            sim.cur_sens.push(list.clone());
            sim.proc_sens.push(list);
        }
        sim
    }

    /// Install an external stop flag (e.g. the live wave window's Stop
    /// button): when set, `run` finishes at the next loop iteration.
    pub fn set_stop_flag(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.stop_flag = Some(flag);
    }

    /// Install an external interrupt flag (SIGINT/SIGTERM): when set, the run
    /// stops with a location dump (see [`Self::where_am_i`]) and exits with
    /// `rc` — an agent (or human) staring at a hung run gets file:line of
    /// the active process and the function chain instead of a bare kill.
    pub fn set_interrupt_flag(
        &mut self,
        flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
        rc: i32,
    ) {
        self.interrupt_flag = Some(flag);
        self.interrupt_rc = rc;
    }

    /// Wall-clock budget for the whole run (`--max-wall`): on expiry the run
    /// stops like an interrupt, with exit code 124 (timeout convention).
    pub fn set_wall_budget(&mut self, budget: std::time::Duration) {
        self.wall_budget = Some(budget);
    }

    /// (reason, exit code) when the run ended via signal or wall budget.
    pub fn interrupt_info(&self) -> Option<(String, i32)> {
        self.interrupt_info.clone()
    }

    /// Cheap gate for hot loops (process bodies, fixpoint sweeps, event
    /// batches): break and let the pump report the location.
    pub fn stop_requested(&self) -> bool {
        if let Some(f) = &self.interrupt_flag {
            if f.load(std::sync::atomic::Ordering::Relaxed) {
                return true;
            }
        }
        if let Some(f) = &self.stop_flag {
            if f.load(std::sync::atomic::Ordering::Relaxed) {
                return true;
            }
        }
        if let Some(budget) = self.wall_budget {
            if self.wall_start.map_or(false, |s| s.elapsed() >= budget) {
                return true;
            }
        }
        false
    }

    pub fn add_observer(&mut self, o: Box<dyn Observer>) {
        self.observers.push(o);
    }

    pub fn flat(&self) -> &Flat {
        &self.flat
    }

    pub fn time(&self) -> u64 {
        self.time
    }

    // ---- introspection (debug API) ---------------------------------------------
    // These let external tools (and `hdlsim debug`) inspect the running design
    // without the kernel changing: find signals, read values, dump memories.

    /// Number of test failures so far (severity error/failure, `$error`/`$fatal`).
    /// CLI runners exit non-zero when this is > 0 after `run`.
    pub fn failures(&self) -> u64 {
        self.failures
    }

    /// True when the run was stopped by an internal error guard
    /// (combinational loop, delta-cycle/process-step limit): exit nonzero.
    pub fn error_stopped(&self) -> bool {
        self.error_stopped
    }

    /// STATUS argument passed to VHDL `std.env.finish(status)` (VHDL-2008),
    /// or `None` when the simulation ended by bare `finish`, `$finish`, or
    /// running out of events. CLI runners may use it as the process exit code.
    pub fn finish_status(&self) -> Option<i32> {
        self.finish_status
    }

    /// Find a signal by id (`"#57"`), full hierarchical name (`tb.dut.q`), or
    /// leaf name (`q`). Leaf match returns the first hit.
    pub fn signal_id(&self, name: &str) -> Option<FlatSigId> {
        if let Some(num) = name.strip_prefix('#') {
            return num
                .parse::<FlatSigId>()
                .ok()
                .filter(|i| (*i as usize) < self.flat.signals.len());
        }
        if let Some(i) = self.flat.signals.iter().position(|s| s.name == name) {
            return Some(i as FlatSigId);
        }
        self.flat
            .signals
            .iter()
            .position(|s| s.name.rsplit('.').next() == Some(name))
            .map(|i| i as FlatSigId)
    }

    /// Last driver of `sig` as `(time_fs, driver label)` — a process origin
    /// like `always @(posedge clk)`, or `"continuous assign"` for `assign`.
    /// `None` when the signal never changed value.
    pub fn driver_of(&self, sig: FlatSigId) -> Option<(u64, &str)> {
        let (p, t) = self.last_driver.get(sig as usize).copied().flatten()?;
        let name = if p == u32::MAX {
            "continuous assign"
        } else {
            self.flat
                .procs
                .get(p as usize)
                .map(|pr| pr.origin.as_str())
                .unwrap_or("<unknown process>")
        };
        Some((t, name))
    }

    pub fn signal_name(&self, id: FlatSigId) -> &str {
        &self.flat.signals[id as usize].name
    }

    pub fn signal_width(&self, id: FlatSigId) -> usize {
        self.flat.signals[id as usize].width
    }

    pub fn signal_value(&self, id: FlatSigId) -> Value {
        self.vals[id as usize].clone()
    }

    pub fn signal_is_mem(&self, id: FlatSigId) -> bool {
        self.is_mem[id as usize]
    }

    /// Number of words in a memory signal (0 for plain signals).
    pub fn mem_len(&self, id: FlatSigId) -> usize {
        if self.is_mem[id as usize] {
            self.mems[id as usize].len()
        } else {
            0
        }
    }

    pub fn mem_word(&self, id: FlatSigId, idx: usize) -> Option<Value> {
        self.mems[id as usize].get(idx).cloned()
    }

    pub fn proc_count(&self) -> usize {
        self.flat.procs.len()
    }

    pub fn proc_origin(&self, id: u32) -> &str {
        &self.flat.procs[id as usize].origin
    }

    /// Human-readable listing of the elaborated design: signals (with memory
    /// depth and init value), processes (with origin and sensitivity), and
    /// continuous assigns. Useful as a first step when debugging a design.
    pub fn dump_design(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "design: {} signals, {} processes, {} assigns, {} functions",
            self.flat.signals.len(),
            self.flat.procs.len(),
            self.flat.assigns.len(),
            self.flat.functions.len()
        );
        for (i, s) in self.flat.signals.iter().enumerate() {
            let mem = match s.mem {
                Some(d) => format!(" mem[0..{}]", d),
                None => String::new(),
            };
            let init = s
                .init
                .as_ref()
                .map(|v| format!(" init={}", crate::debug::fmt_value(v)))
                .unwrap_or_default();
            let sgn = if s.signed { " signed" } else { "" };
            let _ = writeln!(
                out,
                "  sig #{:<3} {:<40} w={:<3}{}{}{}",
                i, s.name, s.width, mem, sgn, init
            );
        }
        for (i, p) in self.flat.procs.iter().enumerate() {
            let sens = match &p.sens {
                Sens::List(es) => es
                    .iter()
                    .map(|e| format!("{:?}#{}", e.kind, e.sig))
                    .collect::<Vec<_>>()
                    .join(", "),
                Sens::All => "all".to_string(),
            };
            let _ = writeln!(
                out,
                "  proc #{:<3} {:?} sens=[{}] ({})",
                i, p.kind, sens, p.origin
            );
        }
        // Function and procedure bodies in slot space: slot 0 is the return
        // value; the Debug rendering of the body makes lowered calls, slot
        // reads and index expressions visible without a rebuild.
        for (i, f) in self.flat.functions.iter().enumerate() {
            let slots = f
                .slots
                .iter()
                .enumerate()
                .map(|(i, w)| format!("{}:w{}", if i == 0 { "ret" } else { "arg" }, w))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                out,
                "  func #{:<3} {:<30} arity={} slots=[{}] body={:#?}",
                i, f.name, f.arity, slots, f.body
            );
        }
        out
    }

    fn emit(&mut self, ev: SimEvent) {
        for o in &mut self.observers {
            o.on_event(&ev);
        }
    }

    /// Execution trace line (`HDLSIM_TRACE=1` on stderr): wait/wake events,
    /// signal wakes, and timeouts — for debugging scheduler semantics without
    /// a wave viewer.
    fn trace(&self, msg: impl std::fmt::Display) {
        if self.trace_on {
            eprintln!("TRACE t={}fs: {}", self.time, msg);
        }
    }

    /// Record a test failure (emits [`SimEvent::TestFailed`]).
    fn fail(&mut self, text: String) {
        self.failures += 1;
        let time_fs = self.time;
        self.emit(SimEvent::TestFailed { text, time_fs });
    }

    /// Report a latched function-recursion stop (latched by
    /// `run_func`/`run_procedure`, which only unwind with X): emits the
    /// diagnostic and flags the run as error-stopped. Returns true when a
    /// latch was pending. Checked at the top of the pump loop *and* before
    /// the no-events exit — the latch is set while the last processes run,
    /// and the loop can otherwise end without reporting it.
    fn report_fn_recursion(&mut self) -> bool {
        if let Some(name) = self.fn_recursion.take() {
            let root = self
                .cur_root_fn
                .get()
                .map(|i| self.flat.functions[i].name.clone())
                .unwrap_or_else(|| "?".to_string());
            // Tail of the chain captured at latch time; a repeating suffix
            // is the overload-dispatch cycle.
            let tail: Vec<String> = self
                .fn_chain
                .borrow()
                .iter()
                .rev()
                .take(14)
                .rev()
                .cloned()
                .collect();
            self.emit(SimEvent::Message {
                text: format!(
                    "error: function call depth exceeded ({} nested calls) at t={} — probable infinite recursion in function '{name}' (call tree rooted at '{root}', chain tail: {}); simulation stopped\n",
                    self.max_func_depth,
                    fmt_time(self.time),
                    tail.join(" > ")
                ),
                time_fs: self.time,
            });
            self.stopped = true;
            self.error_stopped = true;
            return true;
        }
        false
    }

    /// Snapshot the active call chain (names, innermost last) for the
    /// latch reports — this reveals the recursion cycle or the misfit
    /// dispatch. Called when either latch fires.
    fn capture_fn_chain(&self) {
        let stack = self.fn_stack.borrow().clone();
        *self.fn_chain.borrow_mut() = stack
            .into_iter()
            .map(|i| self.flat.functions[i].name.clone())
            .collect();
    }

    /// Report the function-call runaway latch (see `fn_calls`). Same shape
    /// as [`Self::report_fn_recursion`], but for bounded-depth loops whose
    /// body only calls functions.
    fn report_fn_runaway(&mut self) -> bool {
        if let Some((origin, name)) = self.fn_runaway.take() {
            let root = self
                .cur_root_fn
                .get()
                .map(|i| self.flat.functions[i].name.clone())
                .unwrap_or_else(|| "?".to_string());
            let chain = self.fn_chain.borrow().clone();
            // Show the tail of the chain; a repeating suffix is the cycle.
            let tail: Vec<String> = chain.iter().rev().take(14).rev().cloned().collect();
            self.emit(SimEvent::Message {
                text: format!(
                    "error: function call limit exceeded ({} calls in one process activation) at t={} — process '{origin}' loops while calling function '{name}' (call tree rooted at '{root}', chain tail: {}); simulation stopped\n",
                    self.max_fn_calls,
                    fmt_time(self.time),
                    tail.join(" > ")
                ),
                time_fs: self.time,
            });
            self.stopped = true;
            self.error_stopped = true;
            return true;
        }
        false
    }

    /// Graceful memory guard (`HDLSIM_MAX_MEM_MB`): over the limit, stop
    /// with an actionable diagnostic instead of being OOM-killed (exit 137,
    /// no output). Throttled to every 128th call — the `/proc` read is not
    /// free in the hot loop. Returns false when the run must stop.
    fn check_mem(&mut self) -> bool {
        let Some(limit_kb) = self.max_mem_kb else {
            return true;
        };
        let n = self.mem_checks.get() + 1;
        self.mem_checks.set(n);
        if n > 1 && n % 128 != 0 {
            return true;
        }
        let Some(peak_kb) = crate::meminfo::peak_rss_kb() else {
            return true;
        };
        if peak_kb <= limit_kb {
            return true;
        }
        self.emit(SimEvent::Message {
            text: crate::meminfo::limit_message("simulate", peak_kb, limit_kb),
            time_fs: self.time,
        });
        self.stopped = true;
        self.error_stopped = true;
        false
    }

    // ---- main loop ------------------------------------------------------------

    /// Write the full dynamic simulator state to `path` (own binary format
    /// `HDLCKPT1`) — checkpoint/restore, see [`Simulator::restore_checkpoint`].
    pub fn save_checkpoint(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        std::fs::write(path, self.save_checkpoint_bytes())
    }

    /// In-memory checkpoint (reverse debug history, tests): same format as
    /// [`Simulator::save_checkpoint`], returned as bytes.
    pub fn save_checkpoint_bytes(&mut self) -> Vec<u8> {
        // A checkpoint is meant to be resumed: a hard `--max-time` stop sets
        // `stopped`, which would make the restored run exit immediately.
        // (`finished` via $finish is kept — that state is genuinely terminal.)
        self.stopped = false;
        let mut buf = Vec::new();
        ckpt::save(self, &mut buf);
        buf
    }

    /// Rebuild a simulator from `flat` plus a checkpoint written by
    /// [`Simulator::save_checkpoint`]. The re-elaborated design must match the
    /// checkpointed one (fingerprint check) or this returns an error.
    pub fn restore_checkpoint(flat: Flat, path: &std::path::Path) -> Result<Simulator, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        Simulator::restore_checkpoint_bytes(flat, &bytes)
    }

    /// In-memory restore: see [`Simulator::restore_checkpoint`].
    pub fn restore_checkpoint_bytes(flat: Flat, bytes: &[u8]) -> Result<Simulator, String> {
        ckpt::load(flat, bytes)
    }

    /// Run until `$finish`, until `max_time_fs`, or until no events remain.
    pub fn run(&mut self, max_time_fs: u64) {
        self.ensure_started();
        self.pump(max_time_fs, false);
    }

    /// Incremental run: advance until `target_fs`, `$finish`, or no events
    /// remain — whichever comes first. Safe to call repeatedly (used by
    /// `hdlsim do` for `run 100ns` steps); reaching the target just pauses,
    /// it does not stop the simulation.
    pub fn run_until(&mut self, target_fs: u64) {
        self.ensure_started();
        let target = target_fs.max(self.time);
        self.pump(target, true);
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        self.emit(SimEvent::TimeAdvance { time_fs: 0 });
        // Schedule initial processes, plus `always` blocks that wait on no
        // explicit event list: bare `always` (empty list) and `always @*`
        // both execute once at t=0 per the LRM.
        let start_now: Vec<u32> = self
            .flat
            .procs
            .iter()
            .enumerate()
            .filter_map(|(pid, p)| {
                let start = match (&p.kind, &p.sens) {
                    (ProcKind::Initial, _) => true,
                    (ProcKind::Always, Sens::All) => true,
                    (ProcKind::Always, Sens::List(es)) => es.is_empty() || p.initial,
                };
                start.then_some(pid as u32)
            })
            .collect();
        for pid in start_now {
            self.activate(pid);
        }
        self.eval_assigns_fixpoint();
    }

    /// Human-readable "where is the simulator right now": current time, the
    /// active process origin (file:line), and the live function call chain.
    /// Shared by the interrupt dump and the progress heartbeat.
    fn where_am_i(&self) -> String {
        let proc_desc = match self.cur_proc {
            Some(p) if (p as usize) < self.flat.procs.len() => {
                format!("in {}", self.flat.procs[p as usize].origin)
            }
            // eval_assigns_fixpoint runs assigns under a sentinel proc id.
            Some(_) if self.fix_sweep > 0 => format!(
                "in concurrent assign sweep #{}, {} of {} assigns changed in the last pass",
                self.fix_sweep,
                self.fix_changed,
                self.flat.assigns.len()
            ),
            Some(_) => "in concurrent assign sweep".to_string(),
            // Between events: nothing is executing right now — name the most
            // recently dispatched process instead.
            None => match self.last_proc {
                Some(p) if (p as usize) < self.flat.procs.len() => {
                    format!("after {}", self.flat.procs[p as usize].origin)
                }
                _ => "before the first process ran".to_string(),
            },
        };
        let stack = self.fn_stack.borrow();
        let chain = if stack.is_empty() {
            String::new()
        } else {
            format!(
                ", in function {}",
                stack
                    .iter()
                    .rev()
                    .take(8)
                    .rev()
                    .map(|i| self.flat.functions[*i].name.clone())
                    .collect::<Vec<_>>()
                    .join(" > ")
            )
        };
        format!("t={}, {}{}", fmt_time(self.time), proc_desc, chain)
    }

    /// End the run from the interrupt flag or the wall budget: emit the
    /// location dump as a transcript message and record the exit code.
    fn stop_for_interrupt(&mut self, reason: &str, rc: i32) {
        self.emit(SimEvent::Message {
            text: format!(
                "error: simulation {} at {} — simulation stopped\n",
                reason,
                self.where_am_i()
            ),
            time_fs: self.time,
        });
        self.interrupt_info = Some((reason.to_string(), rc));
        self.stopped = true;
    }

    /// Periodic "still running" note on stderr for runs that grind without
    /// time advance (`HDLSIM_PROGRESS_SEC=<seconds>`, 0/absent = off). Shows
    /// the same location as the interrupt dump, so a hung or slow run
    /// announces where it is instead of silently burning CPU.
    fn maybe_heartbeat(&mut self) {
        let interval = progress_interval();
        if interval.is_zero() {
            return;
        }
        let now = std::time::Instant::now();
        let start = *self.wall_start.get_or_insert(now);
        if now.duration_since(start) < interval {
            return;
        }
        let report = self.last_heartbeat.unwrap_or(start);
        let advance = self.last_advance.unwrap_or(start);
        if now.duration_since(report) >= interval && now.duration_since(advance) >= interval {
            eprintln!(
                "hdlsim: still running ({:.0} s without time advance) at {}",
                now.duration_since(advance).as_secs_f64(),
                self.where_am_i()
            );
            self.last_heartbeat = Some(now);
        }
    }

    fn pump(&mut self, max_time_fs: u64, soft: bool) {
        // Delta cycles since the last time advance: a design whose processes
        // or assigns keep re-triggering without time moving would otherwise
        // loop forever (time-based limits like --max-time cannot see it).
        let mut delta_count: u64 = 0;
        loop {
            if self.finished || self.stopped {
                break;
            }
            // Function recursion latched while evaluating expressions (the
            // guard itself only unwinds with X — reporting needs &mut).
            if self.report_fn_recursion() {
                break;
            }
            if self.report_fn_runaway() {
                break;
            }
            delta_count += 1;
            if delta_count > 10_000 {
                self.emit(SimEvent::Message {
                    text: format!(
                        "error: delta-cycle limit exceeded ({} iterations without time advance) at t={} — the design keeps producing zero-delay events (e.g. `wait for 0 ns` in a loop, or processes re-triggering each other with no delay); simulation stopped\n",
                        delta_count, fmt_time(self.time)
                    ),
                    time_fs: self.time,
                });
                self.stopped = true;
                self.error_stopped = true;
                break;
            }
            let flag_set = self
                .interrupt_flag
                .as_ref()
                .map_or(false, |f| f.load(std::sync::atomic::Ordering::Relaxed));
            if flag_set {
                self.stop_for_interrupt("interrupt received", self.interrupt_rc);
                break;
            }
            if let Some(budget) = self.wall_budget {
                let start = *self.wall_start.get_or_insert_with(std::time::Instant::now);
                if start.elapsed() >= budget {
                    // 124: the exit code GNU timeout uses when it kills.
                    self.stop_for_interrupt(
                        &format!("wall-time budget of {}s exceeded", budget.as_secs_f64()),
                        124,
                    );
                    break;
                }
            }
            let quiet_set = self
                .stop_flag
                .as_ref()
                .map_or(false, |f| f.load(std::sync::atomic::Ordering::Relaxed));
            if quiet_set {
                self.finished = true;
                break;
            }
            self.maybe_heartbeat();
            let mut drain_guard = 0u64;
            while let Some(pid) = self.active.pop_front() {
                self.in_active[pid as usize] = false;
                drain_guard += 1;
                if drain_guard > 1_000_000 {
                    self.emit(SimEvent::Message {
                        text: format!(
                            "error: zero-delay process loop detected at t={} — the design keeps re-running a process with no time passing; rerun with HDLSIM_TRACE=1 to see the activation cycle; simulation stopped\n",
                            fmt_time(self.time)
                        ),
                        time_fs: self.time,
                    });
                    self.stopped = true;
                    self.error_stopped = true;
                    break;
                }
                // A single pass over a large activation batch can run for
                // minutes — report progress from inside it too.
                if drain_guard % 4096 == 0 {
                    self.maybe_heartbeat();
                }
                self.run_proc(pid);
                if self.finished || self.stopped {
                    break;
                }
            }
            if self.finished || self.stopped {
                break;
            }
            if !self.nba.is_empty() || !self.nba_exprs.is_empty() {
                let nba = std::mem::take(&mut self.nba);
                for (sig, v, drv) in nba {
                    if self.stop_requested() {
                        break;
                    }
                    self.cur_proc = drv;
                    self.set_sig(sig, v);
                }
                let exprs = std::mem::take(&mut self.nba_exprs);
                for (lhs, v, drv) in exprs {
                    if self.stop_requested() {
                        break;
                    }
                    self.cur_proc = drv;
                    self.assign_to(&lhs, v, None);
                }
                self.cur_proc = None;
                continue;
            }
            if self.eval_assigns_fixpoint() {
                continue; // assigns scheduled processes into active
            }
            self.check_monitors();
            if !self.active.is_empty() {
                continue;
            }
            // Advance time to the next event.
            let next = self.time_events.keys().next().copied();
            match next {
                Some(t) if t <= max_time_fs => {
                    // Only a real advance resets the delta budget: same-time
                    // event batches (delta cycles, `wait for 0 ns` loops) must
                    // keep counting, or a process that keeps scheduling events
                    // at one timestamp would spin forever.
                    if t > self.time {
                        delta_count = 0;
                        self.last_advance = Some(std::time::Instant::now());
                    }
                    self.time = t;
                    self.emit(SimEvent::TimeAdvance { time_fs: t });
                    if !self.check_mem() {
                        break;
                    }
                    let evs = self.time_events.remove(&t).unwrap_or_default();
                    for ev in evs {
                        match ev {
                            Ev::Resume(pid) => self.activate(pid),
                            Ev::WaitTimeout(pid, gen) => self.wait_timeout(pid, gen),
                            Ev::Nba(sig, v, drv) => self.nba.push((sig, v, drv)),
                            Ev::NbaExpr(lhs, v, drv) => self.nba_exprs.push((lhs, v, drv)),
                        }
                    }
                }
                _ => {
                    // A recursion-guard latch set while the last processes
                    // ran must be reported here: the loop-top check would
                    // never run again once the event queue is empty.
                    let latched = self.report_fn_recursion() || self.report_fn_runaway();
                    if !latched {
                        if soft {
                            break; // pause at the target; simulation stays resumable
                        }
                        self.emit(SimEvent::Stop { time_fs: self.time });
                        self.stopped = true;
                    }
                    break;
                }
            }
        }
        if self.finished {
            self.emit(SimEvent::Finish { time_fs: self.time });
        }
        if (self.finished || self.stopped) && !self.finish_notified {
            self.finish_notified = true;
            for o in &mut self.observers {
                o.on_finish();
            }
        }
    }

    // ---- process execution ------------------------------------------------------

    /// Queue a process to run, at most once: never if it is already queued or
    /// parked. Without this, several sensitivity signals changing in one delta
    /// would queue the process several times and re-run its body repeatedly.
    fn activate(&mut self, pid: u32) {
        if self.in_active[pid as usize] || self.parked[pid as usize] {
            self.trace(format!(
                "activate {} skipped (in active={}, parked={})",
                self.flat.procs[pid as usize].origin,
                self.in_active[pid as usize],
                self.parked[pid as usize]
            ));
            return;
        }
        self.trace(format!("activate {}", self.flat.procs[pid as usize].origin));
        self.in_active[pid as usize] = true;
        self.active.push_back(pid);
    }

    /// Park a process on an event list, at most once per process. The
    /// process's `sens_index` entries swap from its declared sensitivity to
    /// the wait's edges — while parked, only those can wake it.
    fn park(&mut self, pid: u32, edges: Vec<(FlatSigId, EdgeKind)>) {
        let i = pid as usize;
        if self.parked[i] {
            self.trace(format!(
                "{}: park SKIPPED, already parked",
                self.flat.procs[pid as usize].origin
            ));
            return;
        }
        self.trace(format!(
            "{}: park on {:?}",
            self.flat.procs[pid as usize].origin, edges
        ));
        let old = std::mem::replace(&mut self.cur_sens[i], edges.clone());
        for (s, _ek) in &old {
            self.sens_index[*s as usize].retain(|(q, _)| *q != pid);
        }
        for (s, ek) in &edges {
            self.sens_index[*s as usize].push((pid, *ek));
        }
        self.parked[i] = true;
    }

    /// Release a process from its wait, swapping its `sens_index` entries back
    /// to the declared sensitivity. Called before re-activation.
    fn unpark(&mut self, pid: u32) {
        let i = pid as usize;
        if !self.parked[i] {
            return;
        }
        self.parked[i] = false;
        let old = std::mem::take(&mut self.cur_sens[i]);
        for (s, _ek) in &old {
            self.sens_index[*s as usize].retain(|(q, _)| *q != pid);
        }
        for (s, ek) in &self.proc_sens[i] {
            self.sens_index[*s as usize].push((pid, *ek));
        }
        self.cur_sens[i] = self.proc_sens[i].clone();
    }

    /// A `wait until ... for T` timeout expired. Pop the process's check
    /// frame and resume it — regardless of the condition (LRM 8.1). If the
    /// wake token does not match, the process already resumed from an earlier
    /// identical wait (or finished): ignore the stale event.
    fn wait_timeout(&mut self, pid: u32, gen: u64) {
        let matches = matches!(
            self.proc_rt[pid as usize].frames.last(),
            Some(Frame::WaitUntilCheck { gen: g, .. }) if *g == gen
        );
        if !matches {
            return;
        }
        self.trace(format!(
            "{}: wait until timeout fired",
            self.flat.procs[pid as usize].origin
        ));
        self.proc_rt[pid as usize].frames.pop();
        self.unpark(pid);
        self.activate(pid);
    }

    fn run_proc(&mut self, pid: u32) {
        self.proc_steps = 0; // per-activation budget (see exec_frames)
        self.fn_calls.set(0); // per-activation function-call budget
        self.last_proc = Some(pid);
        self.emit(SimEvent::ProcActive {
            proc: pid,
            active: true,
            time_fs: self.time,
        });
        self.cur_proc = Some(pid);
        {
            let rt = &mut self.proc_rt[pid as usize];
            if rt.frames.is_empty() {
                rt.frames
                    .push(Frame::block(vec![self.flat.procs[pid as usize]
                        .body
                        .clone()]));
            }
        }
        loop {
            match self.exec_frames(pid) {
                // Break/Continue are consumed inside exec_frames; if one ever
                // escapes (no enclosing loop), treat it as process end.
                Control::Done | Control::Break | Control::Continue => {
                    if self.flat.procs[pid as usize].kind == ProcKind::Always {
                        // `always @(sens) stmt` ≡ `forever begin @(sens); stmt end`:
                        // after the body completes, park until the next event.
                        // A bare `always` (no event list) re-runs immediately.
                        match &self.flat.procs[pid as usize].sens {
                            Sens::List(es) if !es.is_empty() => {
                                let edges =
                                    es.iter().map(|e| (e.sig as FlatSigId, e.kind)).collect();
                                self.park(pid, edges);
                            }
                            Sens::All => {
                                let sigs = std::mem::take(&mut self.proc_all[pid as usize]);
                                let edges = sigs.iter().map(|s| (*s, EdgeKind::Any)).collect();
                                self.proc_all[pid as usize] = sigs;
                                self.park(pid, edges);
                            }
                            _ => self.activate(pid),
                        }
                    }
                    break;
                }
                Control::Suspend(at) => {
                    self.time_events
                        .entry(at)
                        .or_default()
                        .push(Ev::Resume(pid));
                    break;
                }
                Control::Wait => break, // parked (see self.parked) until an edge
            }
        }
        self.cur_proc = None;
        self.emit(SimEvent::ProcActive {
            proc: pid,
            active: false,
            time_fs: self.time,
        });
    }

    fn exec_frames(&mut self, pid: u32) -> Control {
        /// What to do next, decided by peeking at the top frame without holding
        /// a borrow across `exec_stmt`.
        enum Act {
            Exec(Stmt),
            ForStep,
            ForTest,
            RepeatTest,
            /// Top frame is a `wait until` check: a wake arrived (an edge —
            /// a timeout wake pops the frame first, in `wait_timeout`).
            WaitCheck,
        }
        loop {
            // Per-activation interpreter-step budget: a `loop`/`while` whose
            // body never waits would otherwise spin inside this loop forever —
            // no scheduler guard (delta limit, --max-time) can fire because
            // control never returns to `pump`. Modelsim reports the same
            // condition as "iteration limit". HDLSIM_MAX_PROC_STEPS lowers
            // the budget (tests, CI); read once at construction.
            self.proc_steps += 1;
            if self.proc_steps > self.max_proc_steps {
                let origin = self.flat.procs[pid as usize].origin.clone();
                let budget = if self.max_proc_steps % 1_000_000 == 0 {
                    format!("{}M", self.max_proc_steps / 1_000_000)
                } else {
                    format!("{}", self.max_proc_steps)
                };
                self.emit(SimEvent::Message {
                    text: format!(
                        "error: process step limit exceeded ({} interpreter steps in one activation) at t={} — process '{origin}' loops without waiting (e.g. `loop`/`while` with no `wait`); simulation stopped\n",
                        budget,
                        fmt_time(self.time)
                    ),
                    time_fs: self.time,
                });
                self.stopped = true;
                self.error_stopped = true;
                return Control::Done;
            }
            let n = self.proc_rt[pid as usize].frames.len();
            if n == 0 {
                return Control::Done;
            }
            let action = {
                let rt = &mut self.proc_rt[pid as usize];
                match &mut rt.frames[n - 1] {
                    Frame::Block { block, pc } => {
                        if *pc >= block.len() {
                            rt.frames.pop();
                            continue;
                        }
                        let s = std::mem::replace(&mut block[*pc], Stmt::Null);
                        *pc += 1;
                        Act::Exec(s)
                    }
                    Frame::ForIter { after_body, .. } => {
                        if *after_body {
                            *after_body = false;
                            Act::ForStep
                        } else {
                            Act::ForTest
                        }
                    }
                    Frame::RepeatIter {
                        remaining,
                        after_body,
                        ..
                    } => {
                        if *after_body {
                            *remaining -= 1;
                            *after_body = false;
                        }
                        Act::RepeatTest
                    }
                    Frame::WaitUntilCheck { .. } => Act::WaitCheck,
                }
            };
            match action {
                Act::Exec(s) => match self.exec_stmt(pid, s) {
                    Control::Done => {
                        // `$finish`/`std.env.finish` stop the process here —
                        // statements after the call in this activation do not
                        // run. Clear the frames so the process does not
                        // resume into the rest of its body.
                        if self.finished || self.stopped {
                            self.proc_rt[pid as usize].frames.clear();
                            return Control::Done;
                        }
                        continue;
                    }
                    Control::Suspend(at) => return Control::Suspend(at),
                    Control::Wait => return Control::Wait,
                    Control::Break => {
                        let rt = &mut self.proc_rt[pid as usize];
                        while let Some(f) = rt.frames.last() {
                            let is_loop =
                                matches!(f, Frame::ForIter { .. } | Frame::RepeatIter { .. });
                            rt.frames.pop();
                            if is_loop {
                                break;
                            }
                        }
                        continue;
                    }
                    Control::Continue => {
                        let rt = &mut self.proc_rt[pid as usize];
                        let mut i = rt.frames.len();
                        while i > 0 {
                            i -= 1;
                            match &mut rt.frames[i] {
                                Frame::ForIter { after_body, .. }
                                | Frame::RepeatIter { after_body, .. } => {
                                    *after_body = true;
                                    break;
                                }
                                _ => {}
                            }
                        }
                        rt.frames.truncate(i + 1);
                        continue;
                    }
                },
                Act::ForStep => {
                    // Run the loop step in its own block frame above the marker.
                    let step = match &self.proc_rt[pid as usize].frames[n - 1] {
                        Frame::ForIter { step, .. } => (**step).clone(),
                        _ => unreachable!(),
                    };
                    self.proc_rt[pid as usize]
                        .frames
                        .push(Frame::block(vec![step]));
                }
                Act::RepeatTest => {
                    let (remaining, body) = match &self.proc_rt[pid as usize].frames[n - 1] {
                        Frame::RepeatIter {
                            remaining, body, ..
                        } => (*remaining, (**body).clone()),
                        _ => unreachable!(),
                    };
                    let rt = &mut self.proc_rt[pid as usize];
                    if remaining > 0 {
                        if let Frame::RepeatIter { after_body, .. } = &mut rt.frames[n - 1] {
                            *after_body = true;
                        }
                        rt.frames.push(Frame::block(vec![body]));
                    } else {
                        rt.frames.pop();
                    }
                }
                Act::ForTest => {
                    let (cond, body) = match &self.proc_rt[pid as usize].frames[n - 1] {
                        Frame::ForIter { cond, body, .. } => (cond.clone(), (**body).clone()),
                        _ => unreachable!(),
                    };
                    let c = self.eval_expr(&cond, None);
                    let rt = &mut self.proc_rt[pid as usize];
                    if truthy(&c) {
                        if let Frame::ForIter { after_body, .. } = &mut rt.frames[n - 1] {
                            *after_body = true;
                        }
                        rt.frames.push(Frame::block(vec![body]));
                    } else {
                        rt.frames.pop();
                    }
                }
                Act::WaitCheck => {
                    // Reached only on an edge wake — a timeout wake pops the
                    // frame before resuming (see `wait_timeout`). LRM 8.1:
                    // re-check the condition; proceed on true/absent, re-park
                    // on false. The original timeout event stays queued and
                    // still fires if the re-park lasts until the deadline.
                    let (proceed, edges) = match self.proc_rt[pid as usize].frames.last() {
                        Some(Frame::WaitUntilCheck { cond, edges, .. }) => (
                            match cond {
                                None => true,
                                Some(c) => truthy(&self.eval_expr(&c, None)),
                            },
                            edges.clone(),
                        ),
                        _ => (true, Vec::new()),
                    };
                    self.trace(format!(
                        "{}: wake, cond {} -> {}",
                        self.flat.procs[pid as usize].origin,
                        if proceed { "true" } else { "false" },
                        if proceed { "proceed" } else { "re-park" }
                    ));
                    if proceed {
                        self.proc_rt[pid as usize].frames.pop();
                        continue;
                    }
                    let parked: Vec<(FlatSigId, EdgeKind)> =
                        edges.iter().map(|e| (e.sig as FlatSigId, e.kind)).collect();
                    self.park(pid, parked);
                    return Control::Wait;
                }
            }
        }
    }

    fn exec_stmt(&mut self, pid: u32, stmt: Stmt) -> Control {
        match stmt {
            Stmt::Spanned { span, inner } => {
                self.emit(SimEvent::StmtExec {
                    span,
                    time_fs: self.time,
                });
                return self.exec_stmt(pid, *inner);
            }
            Stmt::Null => Control::Done,
            // `return` outside a function body (shouldn't happen) — ignore.
            Stmt::Return(_) => Control::Done,
            Stmt::Block(ss) => {
                self.proc_rt[pid as usize].frames.push(Frame::block(ss));
                Control::Done
            }
            Stmt::If { cond, then, els } => {
                let c = self.eval_expr(&cond, None);
                let branch = if truthy(&c) {
                    then
                } else {
                    els.unwrap_or_else(|| Box::new(Stmt::Null))
                };
                self.proc_rt[pid as usize]
                    .frames
                    .push(Frame::block(vec![*branch]));
                Control::Done
            }
            Stmt::Case {
                expr,
                items,
                default,
                matching,
            } => {
                let v = self.eval_expr(&expr, None);
                let mut chosen: Option<Stmt> = None;
                for (conds, body) in &items {
                    for c in conds {
                        let cv = self.eval_expr(c, None);
                        // Case choices match bit-literally after zero-extending
                        // both sides to the wider width (X in the expression
                        // matches nothing); in a matching case (`case?`) choice
                        // bits of '-' are wildcards.
                        let hit = if matching {
                            v.matches_case(&cv)
                        } else {
                            let w = v.width().max(cv.width()).max(1);
                            v.resize(w).bits == cv.resize(w).bits
                        };
                        if hit {
                            chosen = Some(body.clone());
                            break;
                        }
                    }
                    if chosen.is_some() {
                        break;
                    }
                }
                if chosen.is_none() {
                    chosen = default.map(|d| *d);
                }
                if let Some(b) = chosen {
                    self.proc_rt[pid as usize]
                        .frames
                        .push(Frame::block(vec![b]));
                }
                Control::Done
            }
            Stmt::AssignDelayed { lhs, rhs, delay } => {
                // VHDL `s <= v after t;`: evaluate now, schedule the update at
                // now + t on the time wheel without suspending the process.
                let v = self.eval_expr(&rhs, None);
                let t = self.eval_expr(&delay, None).to_u64().unwrap_or(0);
                if t == 0 {
                    if let Some(sig) = lhs_sig(&lhs) {
                        let w = self.flat.signals[sig as usize].width;
                        let v = self.convert_for_target(&lhs, &rhs, v, None);
                        self.nba.push((sig, v.resize(w.max(1)), self.cur_proc));
                    } else {
                        let lhs = self.concretize_target(&lhs);
                        self.nba_apply_later(lhs, v);
                    }
                } else if let Some(sig) = lhs_sig(&lhs) {
                    let w = self.flat.signals[sig as usize].width;
                    let v = self.convert_for_target(&lhs, &rhs, v, None);
                    self.time_events
                        .entry(self.time + t)
                        .or_default()
                        .push(Ev::Nba(sig, v.resize(w.max(1)), self.cur_proc));
                } else {
                    let lhs = self.concretize_target(&lhs);
                    self.time_events
                        .entry(self.time + t)
                        .or_default()
                        .push(Ev::NbaExpr(lhs, v, self.cur_proc));
                }
                Control::Done
            }
            Stmt::Assign { lhs, rhs, nba } => {
                let v = self.eval_expr(&rhs, None);
                if nba {
                    if let Some(sig) = lhs_sig(&lhs) {
                        let cur = self.vals[sig as usize].clone();
                        let w = self.flat.signals[sig as usize].width;
                        let v = self.convert_for_target(&lhs, &rhs, v, None);
                        self.nba.push((sig, v.resize(w.max(1)), self.cur_proc));
                        let _ = cur;
                    } else {
                        // Memory/part write with NBA: capture the target
                        // address now, apply in the NBA region.
                        let lhs = self.concretize_target(&lhs);
                        self.nba_apply_later(lhs, v);
                    }
                } else {
                    let v = self.convert_for_target(&lhs, &rhs, v, None);
                    self.assign_to(&lhs, v, None);
                }
                Control::Done
            }
            Stmt::Delay { time_fs, stmt } => {
                self.proc_rt[pid as usize]
                    .frames
                    .push(Frame::block(vec![*stmt]));
                Control::Suspend(self.time + time_fs)
            }
            Stmt::Wait { sens, body } => {
                let edges: Vec<(FlatSigId, EdgeKind)> =
                    sens.iter().map(|e| (e.sig as FlatSigId, e.kind)).collect();
                self.proc_rt[pid as usize]
                    .frames
                    .push(Frame::block(vec![*body]));
                self.park(pid, edges);
                Control::Wait
            }
            Stmt::WaitUntil {
                cond,
                sens,
                timeout,
            } => {
                // LRM 8.1: the timeout expression is evaluated once, when the
                // wait executes. `None` → no timeout (park until an edge).
                let deadline = match timeout {
                    Some(t) => {
                        let v = self.eval_expr(&t, None);
                        self.time.saturating_add(v.to_u64().unwrap_or(0))
                    }
                    None => u64::MAX,
                };
                self.wait_gen += 1;
                let gen = self.wait_gen;
                self.proc_rt[pid as usize]
                    .frames
                    .push(Frame::WaitUntilCheck {
                        cond: cond.clone(),
                        edges: sens.clone(),
                        deadline,
                        gen,
                    });
                let edges: Vec<(FlatSigId, EdgeKind)> =
                    sens.iter().map(|e| (e.sig as FlatSigId, e.kind)).collect();
                self.trace(format!(
                    "{}: wait until ({} edges, deadline {})",
                    self.flat.procs[pid as usize].origin,
                    edges.len(),
                    if deadline == u64::MAX {
                        "none".to_string()
                    } else {
                        format!("{deadline}")
                    }
                ));
                if !edges.is_empty() {
                    self.park(pid, edges);
                }
                if deadline != u64::MAX {
                    self.time_events
                        .entry(deadline)
                        .or_default()
                        .push(Ev::WaitTimeout(pid, gen));
                }
                Control::Wait
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                // Push the loop marker, then run init above it. The marker
                // drives step/re-test each time the body frame finishes, so
                // delays and event waits inside the body suspend correctly.
                self.proc_rt[pid as usize].frames.push(Frame::ForIter {
                    cond,
                    step,
                    body,
                    after_body: false,
                });
                self.exec_stmt(pid, *init)
            }
            Stmt::Repeat { count, body } => {
                let v = self.eval_expr(&count, None);
                let remaining = v.to_u64().map(|u| u as i64).unwrap_or(0);
                self.proc_rt[pid as usize].frames.push(Frame::RepeatIter {
                    remaining,
                    body,
                    after_body: false,
                });
                Control::Done
            }
            Stmt::Break => Control::Break,
            Stmt::Continue => Control::Continue,
            Stmt::ExprStmt(e) => {
                self.eval_expr(&e, None);
                Control::Done
            }
            Stmt::SysCall { name, args } => {
                // Process context: process variables are flat signals, no frame.
                self.sys_call(&name, args, None);
                Control::Done
            }
        }
    }

    /// Freeze the index/bound expressions of an assignment target to their
    /// values *now*, so a deferred (NBA-region) write uses the address that was
    /// current when the assignment executed — not whatever the index signals
    /// hold by the time the NBA region runs.
    fn concretize_target(&mut self, lhs: &Expr) -> Expr {
        match lhs {
            Expr::Lit(v) => Expr::Lit(v.clone()),
            Expr::Real(x) => Expr::Real(*x),
            Expr::Sig(s) => Expr::Sig(*s),
            Expr::SigAttr { sig, attr } => Expr::SigAttr {
                sig: *sig,
                attr: *attr,
            },
            Expr::Idx { e, i } => {
                let a = self.eval_expr(i, None);
                Expr::Idx {
                    e: Box::new(self.concretize_target(e)),
                    i: Box::new(Expr::Lit(a)),
                }
            }
            Expr::Sel { e, msb, lsb } => {
                let m = self.eval_expr(msb, None);
                let l = self.eval_expr(lsb, None);
                Expr::Sel {
                    e: Box::new(self.concretize_target(e)),
                    msb: Box::new(Expr::Lit(m)),
                    lsb: Box::new(Expr::Lit(l)),
                }
            }
            Expr::Bin { op, l, r } => Expr::Bin {
                op: *op,
                l: Box::new(self.concretize_target(l)),
                r: Box::new(self.concretize_target(r)),
            },
            Expr::Un { op, e } => Expr::Un {
                op: *op,
                e: Box::new(self.concretize_target(e)),
            },
            Expr::Cond { c, t, e } => Expr::Cond {
                c: Box::new(self.concretize_target(c)),
                t: Box::new(self.concretize_target(t)),
                e: Box::new(self.concretize_target(e)),
            },
            Expr::Call { name, args } => Expr::Call {
                name: name.clone(),
                args: args.iter().map(|a| self.concretize_target(a)).collect(),
            },
            Expr::Concat(es) => {
                Expr::Concat(es.iter().map(|e| self.concretize_target(e)).collect())
            }
            Expr::Replicate { count, e } => Expr::Replicate {
                count: Box::new(Expr::Lit(self.eval_expr(count, None))),
                e: Box::new(self.concretize_target(e)),
            },
            Expr::Rising(e) => Expr::Rising(Box::new(self.concretize_target(e))),
            Expr::Prev(e) => Expr::Prev(Box::new(self.concretize_target(e))),
            Expr::Falling(e) => Expr::Falling(Box::new(self.concretize_target(e))),
            Expr::ToInt(e) => Expr::ToInt(Box::new(self.concretize_target(e))),
            Expr::ToString(e) => Expr::ToString(Box::new(self.concretize_target(e))),
            Expr::Resize { e, width, signed } => Expr::Resize {
                e: Box::new(self.concretize_target(e)),
                width: *width,
                signed: *signed,
            },
            Expr::Signed(e) => Expr::Signed(Box::new(self.concretize_target(e))),
        }
    }

    fn nba_apply_later(&mut self, lhs: Expr, v: Value) {
        // Part-select/concat/memory NBA: wrap as a synthetic one-shot assign
        // executed in the NBA region via a synthetic process-less path. Phase 1:
        // apply immediately if the target is a plain signal; else defer via a
        // queued closure is not possible — so store as (lhs, v) in a side list.
        self.nba_exprs.push((lhs, v, self.cur_proc));
    }

    // ---- system tasks -------------------------------------------------------------

    /// Dispatch a system call (builtin task/procedure or user subprogram).
    /// `locals` is the subprogram frame when called from one: frame slots
    /// (`Expr::Sig(i)` with `i < locals.len()`) shadow real signals, matching
    /// `eval_expr`.
    fn sys_call(&mut self, name: &str, args: Vec<Expr>, mut locals: Option<&mut Vec<Value>>) {
        match name {
            // `arr = new[n]` — resize a dynamic array / queue.
            "$array_new" => {
                if let (Some(Expr::Sig(sig)), Some(n)) = (args.first(), args.get(1)) {
                    let n = self.eval_expr(n, locals.as_deref()).to_u64().unwrap_or(0) as usize;
                    let sig = *sig as FlatSigId;
                    let w = self.flat.signals[sig as usize].width;
                    if self.mem_grow_ok(sig as usize, n.saturating_sub(1)) {
                        self.mems[sig as usize].resize(n, Value::x(w.max(1)));
                        self.vals[sig as usize] = Value::from_u64(32, n as u64);
                    }
                }
            }
            // Array method statement: `q.push_back(x)` — lowered to
            // `$array_push_back` with the array as args[0].
            m if m.starts_with("$array_") && m != "$array_new" => {
                let method = &m["$array_".len()..];
                if let Some(Expr::Sig(sig)) = args.first() {
                    let sig = *sig as FlatSigId;
                    if self.is_mem[sig as usize] {
                        let w = self.flat.signals[sig as usize].width.max(1);
                        let margs: Vec<Value> = args[1..]
                            .iter()
                            .map(|a| self.eval_expr(a, locals.as_deref()).resize(w))
                            .collect();
                        match method {
                            "push_back" => {
                                if let Some(v) = margs.first() {
                                    self.mems[sig as usize].push(v.clone());
                                }
                            }
                            "push_front" => {
                                if let Some(v) = margs.first() {
                                    self.mems[sig as usize].insert(0, v.clone());
                                }
                            }
                            "pop_back" => {
                                self.mems[sig as usize].pop();
                            }
                            "pop_front" => {
                                if !self.mems[sig as usize].is_empty() {
                                    self.mems[sig as usize].remove(0);
                                }
                            }
                            _ => {}
                        }
                        let n = self.mems[sig as usize].len();
                        self.vals[sig as usize] = Value::from_u64(32, n as u64);
                    }
                }
            }
            // SV class constructor: `c = new C(args)` — push an object word,
            // store its handle in `c`, run `C::new` if defined.
            m if m.starts_with("$new::") => {
                let cn = &m["$new::".len()..];
                let w = self
                    .objmem
                    .get(cn)
                    .map(|ms| self.flat.signals[*ms as usize].width.max(1))
                    .unwrap_or(64);
                let handle = {
                    let mut objs = self.objs.borrow_mut();
                    let words = objs.entry(cn.to_string()).or_default();
                    words.push(Value::x(w));
                    (words.len() - 1) as u64
                };
                if let Some(Expr::Sig(sig)) = args.first() {
                    let sig = *sig as FlatSigId;
                    let sw = self.flat.signals[sig as usize].width.max(1);
                    self.vals[sig as usize] = Value::from_u64(64, handle).resize(sw);
                }
                let fname = format!("{}::new", cn);
                if let Some(fi) = self.func_match(&fname, args.get(1..).unwrap_or(&[]), None) {
                    let saved = self.cur_obj.replace(handle);
                    self.run_func(fi, &args[1..], None);
                    self.cur_obj.set(saved);
                }
            }
            // Void method call as a statement: `c.do(args);`
            m if m.starts_with("$callmethod::") => {
                let rest = &m["$callmethod::".len()..]; // `C::m`
                if let Some(fi) =
                    self.func_match(rest, args.get(1..).unwrap_or(&[]), locals.as_deref())
                {
                    let obj = args
                        .first()
                        .map(|a| {
                            self.eval_expr(a, locals.as_deref())
                                .to_u64()
                                .unwrap_or(u64::MAX)
                        })
                        .unwrap_or(u64::MAX);
                    let saved = self.cur_obj.replace(obj);
                    self.run_func(fi, &args[1..], None);
                    self.cur_obj.set(saved);
                }
            }
            "display" | "write" => {
                let text = self.fmt_args(&args, locals.as_deref());
                let text = if name == "display" {
                    format!("{}\n", text)
                } else {
                    text
                };
                self.emit(SimEvent::Message {
                    text,
                    time_fs: self.time,
                });
            }
            "monitor" => {
                self.monitor.push(args);
                self.monitor_last.push(None);
                let text = self.fmt_args(
                    &self.monitor[self.monitor.len() - 1].clone(),
                    locals.as_deref(),
                );
                self.emit(SimEvent::Message {
                    text: format!("{}\n", text),
                    time_fs: self.time,
                });
            }
            "assert" => {
                let ok = args
                    .first()
                    .map(|a| truthy(&self.eval_expr(a, locals.as_deref())))
                    .unwrap_or(true);
                if !ok {
                    let msg = args
                        .get(1)
                        .map(|a| string_of(&self.eval_expr(a, locals.as_deref())))
                        .unwrap_or_default();
                    let severity = args
                        .get(2)
                        .map(|a| string_of(&self.eval_expr(a, locals.as_deref())))
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    self.emit(SimEvent::Message {
                        text: format!("assertion failed at t={}: {}\n", self.time, msg),
                        time_fs: self.time,
                    });
                    if matches!(severity.as_str(), "error" | "failure") {
                        self.fail(format!("assertion failed at t={}: {}", self.time, msg));
                    }
                    if severity == "failure" {
                        self.emit(SimEvent::Message {
                            text: format!(
                                "error: severity failure at t={}; simulation aborted\n",
                                self.time
                            ),
                            time_fs: self.time,
                        });
                        self.stopped = true;
                    }
                }
            }
            "report" => {
                let msg = args
                    .first()
                    .map(|a| string_of(&self.eval_expr(a, locals.as_deref())))
                    .unwrap_or_default();
                let severity = args
                    .get(1)
                    .map(|a| string_of(&self.eval_expr(a, locals.as_deref())))
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                self.emit(SimEvent::Message {
                    text: format!("{}{}\n", if msg.is_empty() { "" } else { ": " }, msg),
                    time_fs: self.time,
                });
                if matches!(severity.as_str(), "error" | "failure") {
                    self.fail(format!("report severity {}: {}", severity, msg));
                }
                if severity == "failure" {
                    self.emit(SimEvent::Message {
                        text: format!(
                            "error: severity failure at t={}; simulation aborted\n",
                            self.time
                        ),
                        time_fs: self.time,
                    });
                    self.stopped = true;
                }
            }
            "sva_assert" => {
                // SVA-lite assertion check: a false condition is always a
                // failed test (unlike VHDL's severity-gated `assert` above).
                let ok = args
                    .first()
                    .map(|e| truthy(&self.eval_expr(e, locals.as_deref())))
                    .unwrap_or(false);
                if !ok {
                    let text = match args.first() {
                        Some(e) => format!("{:?}", e),
                        None => String::new(),
                    };
                    self.emit(SimEvent::Message {
                        text: format!("assertion failed at t={}: {}\n", self.time, text),
                        time_fs: self.time,
                    });
                    self.fail(format!("sva assertion failed at t={}: {}", self.time, text));
                }
            }
            "error" | "fatal" => {
                // Verilog $error / $fatal — a failed test.
                let text = self.fmt_args(&args, locals.as_deref());
                self.emit(SimEvent::Message {
                    text: format!("{}: {}\n", name, text),
                    time_fs: self.time,
                });
                self.fail(format!("${} at t={}: {}", name, self.time, text));
                if name == "fatal" {
                    self.stopped = true;
                }
            }
            "warning" | "info" => {
                let text = self.fmt_args(&args, locals.as_deref());
                self.emit(SimEvent::Message {
                    text: format!("{}: {}\n", name, text),
                    time_fs: self.time,
                });
            }
            "finish" => {
                self.finished = true;
            }
            "$env_finish" => {
                // VHDL std.env.finish([status]): stop the simulation; the
                // optional STATUS argument is reported via `finish_status()`
                // so the CLI can use it as the process exit code.
                self.finished = true;
                if let Some(a) = args.first() {
                    // 32-bit two's complement (VHDL integer) → i32.
                    let raw = self.eval_expr(a, locals.as_deref()).to_u64().unwrap_or(0) as u32;
                    self.finish_status = Some(raw as i32);
                }
            }
            "stop" => {
                self.stopped = true;
            }
            "readmemh" | "readmemb" => {
                if args.len() >= 2 {
                    if let (Expr::Lit(fname), Expr::Sig(mem)) = (&args[0], &args[1]) {
                        let fname = string_of(fname);
                        self.read_mem_file(mem, &fname, name == "readmemb");
                    }
                }
            }
            "rand_solve" => {
                // $rand_solve("spec", "name", lvalue, ...): solve the spec with
                // the kernel RNG, then assign each binding to its lvalue.
                let spec = args
                    .first()
                    .map(|a| string_of(&self.eval_expr(a, locals.as_deref())))
                    .unwrap_or_default();
                match self.rand_solve(&spec, rand::DEFAULT_SOLVE_ATTEMPTS) {
                    Ok(bindings) => {
                        let mut i = 1;
                        while i + 1 < args.len() {
                            let name = string_of(&self.eval_expr(&args[i], locals.as_deref()));
                            match bindings.iter().find(|(n, _)| *n == name) {
                                Some((_, v)) => {
                                    self.assign_to(
                                        &args[i + 1],
                                        Value::from_u64(32, *v as u32 as u64),
                                        None,
                                    );
                                }
                                None => self.emit(SimEvent::Message {
                                    text: format!(
                                        "warning: $rand_solve: '{}' is not in the spec at t={}\n",
                                        name, self.time
                                    ),
                                    time_fs: self.time,
                                }),
                            }
                            i += 2;
                        }
                        if (args.len().saturating_sub(1)) % 2 != 0 {
                            self.emit(SimEvent::Message {
                                text: format!(
                                    "warning: $rand_solve: dangling argument at t={}\n",
                                    self.time
                                ),
                                time_fs: self.time,
                            });
                        }
                    }
                    Err(msg) => {
                        self.emit(SimEvent::Message {
                            text: format!("warning: $rand_solve at t={}: {}\n", self.time, msg),
                            time_fs: self.time,
                        });
                    }
                }
            }
            // std.textio: `write(l, v)` appends the formatted value to the
            // line buffer (right-aligned: first char in the highest used
            // byte). The buffer may be a subprogram frame slot (a
            // procedure-local `variable l : line`) — those grow to fit,
            // matching VHDL's unconstrained line/string semantics.
            "$textio_write" => {
                if let (Some(target), Some(v)) = (args.first(), args.get(1)) {
                    let text = self.fmt_text_value(&self.eval_expr(v, locals.as_deref()));
                    let is_slot = matches!(target, Expr::Sig(s) if (*s as usize)
                        < locals.as_deref().map_or(0, Vec::len));
                    let (w, add) = if is_slot {
                        // Dynamic line: always take the full text.
                        (usize::MAX, text.as_bytes().len())
                    } else {
                        let w = self
                            .width_of_target(target, locals.as_deref())
                            .unwrap_or(1)
                            .max(1);
                        (w, text.as_bytes().len().min(w / 8))
                    };
                    if add == 0 {
                        return;
                    }
                    let shift = add * 8;
                    let old = self.eval_expr(target, locals.as_deref()).bits;
                    // Freshly declared lines hold X bits — treat those as
                    // empty so the appended text is not prefixed with X bytes.
                    let buf_w = if is_slot {
                        old.len().max(shift).max(8)
                    } else {
                        w
                    };
                    let mut nb = vec![V0; buf_w];
                    for i in 0..old.len() {
                        if i + shift < buf_w && (old[i] == V0 || old[i] == V1) {
                            nb[i + shift] = old[i];
                        }
                    }
                    for (bi, &b) in text.as_bytes()[..add].iter().enumerate() {
                        for j in 0..8 {
                            nb[(add - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
                        }
                    }
                    self.store_value(
                        target,
                        Value {
                            bits: nb,
                            real: false,
                        },
                        locals.as_deref_mut(),
                    );
                }
            }
            // `writeline(f, l)` — print the line (stdout) or append to the file.
            "$textio_writeline" => {
                let mut path = args
                    .first()
                    .map(|a| string_of(&self.eval_expr(a, locals.as_deref())))
                    .unwrap_or_else(|| "stdout".to_string());
                if let Some(p) = self.open_files.get(&path) {
                    path = p.clone();
                }
                let text = match args.get(1) {
                    Some(e) => string_of(&self.eval_expr(e, locals.as_deref())),
                    None => String::new(),
                };
                if path == "stdout" || path.is_empty() {
                    self.emit(SimEvent::Message {
                        text: format!("{}\n", text),
                        time_fs: self.time,
                    });
                } else {
                    use std::io::Write;
                    match std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                    {
                        Ok(mut f) => {
                            let _ = f.write_all(format!("{}\n", text).as_bytes());
                        }
                        Err(_) => self.emit(SimEvent::Message {
                            text: format!(
                                "error: cannot write textio file '{}' at t={}\n",
                                path, self.time
                            ),
                            time_fs: self.time,
                        }),
                    }
                }
            }
            // `file_open([status,] f, path, mode)` — bind the path to the
            // file's marker (path-less declarations lower to a unique
            // `$file:…` constant) or to a literal-path declaration's path.
            // `write_mode` (file_open_kind 1) truncates, matching VHDL open
            // semantics; read/append keep existing content.
            "$textio_file_open" => {
                let mut it = args.iter();
                let status = if args.len() >= 4 { it.next() } else { None };
                if let (Some(ftarget), Some(p)) = (it.next(), it.next()) {
                    let fkey = string_of(&self.eval_expr(ftarget, locals.as_deref()));
                    let path = string_of(&self.eval_expr(p, locals.as_deref()));
                    let mode = match it.next() {
                        Some(m) => self.eval_expr(m, locals.as_deref()).to_u64(),
                        None => None,
                    };
                    if mode == Some(1) {
                        let _ = std::fs::File::create(&path);
                    }
                    if !fkey.is_empty() {
                        self.open_files.insert(fkey, path);
                    }
                    if let Some(st) = status {
                        // file_open_status: open_ok = 0.
                        self.store_value(st, Value::from_u64(32, 0), locals.as_deref_mut());
                    }
                }
            }
            // `file_close(f)` — drop the binding; writes are flushed per
            // writeline, so there is no buffered handle to close.
            "$textio_file_close" => {
                if let Some(ftarget) = args.first() {
                    let fkey = string_of(&self.eval_expr(ftarget, locals.as_deref()));
                    self.open_files.remove(&fkey);
                }
            }
            // `readline(f, l)` — read the next line of the file into `l`.
            "$textio_readline" => {
                let mut path = args
                    .first()
                    .map(|a| string_of(&self.eval_expr(a, locals.as_deref())))
                    .unwrap_or_else(|| "stdin".to_string());
                if let Some(p) = self.open_files.get(&path) {
                    path = p.clone();
                }
                let line = self.textio_read_line(&path);
                if let (Some(target), Some(text)) = (args.get(1), line) {
                    let w = match target {
                        Expr::Sig(sig) => {
                            let i = *sig as usize;
                            if i < locals.as_deref().map_or(0, Vec::len) {
                                locals.as_deref().map(|l| l[i].width()).unwrap_or(1)
                            } else {
                                self.flat.signals[i].width
                            }
                        }
                        _ => 1,
                    }
                    .max(1);
                    self.store_value(
                        target,
                        pack_str_value(&text).resize(w),
                        locals.as_deref_mut(),
                    );
                }
            }
            // `read(l, v)` — parse the first whitespace-delimited token of the
            // line into `v` (decimal integer, or string for wide targets).
            // Both targets may be subprogram frame slots.
            "$textio_read" => {
                if let (Some(ltarget), Some(vtarget)) = (args.first(), args.get(1)) {
                    if matches!(ltarget, Expr::Sig(_)) && matches!(vtarget, Expr::Sig(_)) {
                        let content = string_of(&self.eval_expr(ltarget, locals.as_deref()));
                        let (tok, rest) = match content.find(char::is_whitespace) {
                            Some(i) => (
                                content[..i].to_string(),
                                content[i..].trim_start().to_string(),
                            ),
                            None => (content.clone(), String::new()),
                        };
                        let lw = self
                            .width_of_target(ltarget, locals.as_deref())
                            .unwrap_or(1)
                            .max(1);
                        self.store_value(
                            ltarget,
                            pack_str_value(&rest).resize(lw),
                            locals.as_deref_mut(),
                        );
                        let vw = self
                            .width_of_target(vtarget, locals.as_deref())
                            .unwrap_or(1)
                            .max(1);
                        let v = match tok.parse::<u64>() {
                            Ok(n) => Value::from_u64(vw, n),
                            Err(_) => match tok.parse::<i64>() {
                                Ok(n) => Value::from_u64(vw, n as u64),
                                // Not a number: read it as a string into a
                                // byte-multiple target.
                                Err(_) if vw % 8 == 0 => pack_str_value(&tok).resize(vw),
                                Err(_) => Value::x(vw),
                            },
                        };
                        self.store_value(vtarget, v, locals.as_deref_mut());
                    }
                }
            }
            _ => {
                // User procedure / SV task called as a statement, if
                // registered: run with full side effects (syscalls, signal
                // assignment) — not the pure function path.
                if let Some(fi) = self.func_match(name, &args, locals.as_deref()) {
                    self.run_procedure(fi, &args, locals.as_deref());
                } else {
                    self.emit(SimEvent::Message {
                        text: format!(
                            "warning: unsupported system task ${} at t={}",
                            name, self.time
                        ),
                        time_fs: self.time,
                    });
                }
            }
        }
    }

    /// Store into a statement target: a subprogram frame slot when the
    /// `Expr::Sig` id indexes the frame, otherwise the real signal.
    fn store_value(&mut self, e: &Expr, v: Value, locals: Option<&mut Vec<Value>>) {
        let Expr::Sig(s) = e else {
            return;
        };
        let i = *s as usize;
        if let Some(l) = locals {
            if i < l.len() {
                // Frame slots hold unconstrained values (string/line/file
                // paths): grow to fit instead of truncating to the declared
                // slot width.
                let w = l[i].width().max(v.width()).max(1);
                l[i] = v.resize(w);
                return;
            }
        }
        if let Some(sig) = self.flat.signals.get(i) {
            self.vals[i] = v.resize(sig.width.max(1));
        }
    }

    /// Width of a statement target: frame slot or real signal.
    fn width_of_target(&self, e: &Expr, locals: Option<&Vec<Value>>) -> Option<usize> {
        let Expr::Sig(s) = e else {
            return None;
        };
        let i = *s as usize;
        if i < locals.map_or(0, Vec::len) {
            Some(locals.unwrap()[i].width())
        } else {
            Some(self.flat.signals.get(i)?.width)
        }
    }

    /// Read the next line of a textio file (`stdin` marker → process stdin),
    /// advancing the stored read position. None at end of file.
    fn textio_read_line(&self, path: &str) -> Option<String> {
        if path == "stdin" {
            use std::io::BufRead;
            let mut lock = std::io::stdin().lock();
            let mut s = String::new();
            let n = lock.read_line(&mut s).ok()?;
            if n == 0 {
                return None;
            }
            return Some(s.trim_end_matches(['\n', '\r']).to_string());
        }
        let mut pos = self.textio_pos.borrow_mut();
        let n = pos.entry(path.to_string()).or_insert(0);
        let text = std::fs::read_to_string(path).ok()?;
        let line = text.lines().nth(*n).map(|l| l.to_string());
        if line.is_some() {
            *n += 1;
        }
        line
    }

    /// Format a value for textio `write(l, v)`: byte-multiple all-printable →
    /// the string itself; ≤32 bits → decimal; wider → hex.
    fn fmt_text_value(&self, v: &Value) -> String {
        let w = v.width();
        if w > 0 && w % 8 == 0 && w <= 8 * 1024 {
            let bytes: Vec<u8> = v
                .bits
                .chunks(8)
                .rev()
                .map(|c| {
                    let mut b = 0u8;
                    for (j, &x) in c.iter().enumerate() {
                        if x == V1 {
                            b |= 1 << j;
                        }
                    }
                    b
                })
                .collect();
            if bytes
                .iter()
                .all(|&b| b != 0 && (b.is_ascii_graphic() || b == b' '))
            {
                return String::from_utf8_lossy(&bytes).into_owned();
            }
        }
        match v.to_u64() {
            Some(n) if w <= 32 => n.to_string(),
            Some(n) => format!("0x{:x}", n),
            None => "X".to_string(),
        }
    }

    fn read_mem_file(&mut self, mem: &SigId, fname: &str, binary: bool) {
        let Ok(text) = std::fs::read_to_string(fname) else {
            self.emit(SimEvent::Message {
                text: format!("error: cannot read memory file '{}'", fname),
                time_fs: self.time,
            });
            return;
        };
        let sig = *mem as usize;
        let width = self.flat.signals[sig].width.max(1);
        for tok in text.split_whitespace() {
            let t = tok.trim_start_matches('@');
            if tok.starts_with('@') {
                continue; // address directive: we fill sequentially for simplicity
            }
            let v = if binary {
                hdl_ir::verilog_lower::parse_int_literal(&format!("'b{}", t.replace('_', "")))
            } else {
                hdl_ir::verilog_lower::parse_int_literal(&format!(
                    "{}'h{}",
                    width,
                    t.replace('_', "")
                ))
            };
            if let Some(v) = v {
                self.mems[sig].push(v.resize(width));
            }
        }
    }

    fn fmt_args(&mut self, args: &[Expr], locals: Option<&Vec<Value>>) -> String {
        let mut out = String::new();
        let mut argi = 0;
        let fmt = match args.first() {
            Some(Expr::Lit(v)) if string_of(v).contains('%') || args.len() > 1 => {
                argi = 1;
                string_of(v)
            }
            Some(Expr::Lit(v)) => {
                argi = 1;
                string_of(v)
            }
            _ => String::new(),
        };
        let mut chars = fmt.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                // Skip flags/width modifiers: %0d, %5h, %-10s …
                while matches!(chars.peek(), Some(d) if d.is_ascii_digit() || *d == '.' || *d == '-')
                {
                    chars.next();
                }
                let spec = chars.next().unwrap_or('\n');
                match spec {
                    '%' => out.push('%'),
                    'h' | 'x' | 'd' | 'b' | 'o' | 'c' | 's' | 't' => {
                        if argi < args.len() {
                            let v = self.eval_expr(&args[argi], locals);
                            argi += 1;
                            match spec {
                                's' => out.push_str(&string_of(&v)),
                                't' => out.push_str(&v.display("d")),
                                _ => out.push_str(&v.display(&spec.to_string())),
                            }
                        }
                    }
                    'f' | 'e' | 'g' => {
                        if argi < args.len() {
                            let e = &args[argi];
                            let v = self.eval_expr(e, None);
                            argi += 1;
                            let f = if self.expr_real(e, None) {
                                v.to_f64()
                            } else {
                                v.to_u64().map(|n| n as f64)
                            };
                            match (spec, f) {
                                ('f', Some(x)) => out.push_str(&format!("{:.6}", x)),
                                ('e', Some(x)) => out.push_str(&format!("{:e}", x)),
                                ('g', Some(x)) => out.push_str(&format!("{}", x)),
                                _ => out.push('x'),
                            }
                        }
                    }
                    'm' => out.push_str("<hier>"),
                    _ => {
                        out.push('%');
                        out.push(spec);
                    }
                }
            } else {
                out.push(c);
            }
        }
        // Trailing args without format specifiers.
        while argi < args.len() {
            let v = self.eval_expr(&args[argi], None);
            argi += 1;
            out.push_str(&v.display("h"));
        }
        out
    }

    fn check_monitors(&mut self) {
        for i in 0..self.monitor.len() {
            let args = self.monitor[i].clone();
            let vals: Vec<Value> = args.iter().map(|a| self.eval_expr(a, None)).collect();
            if self.monitor_last[i].as_ref() != Some(&vals) {
                self.monitor_last[i] = Some(vals);
                let text = self.fmt_args(&args, None);
                self.emit(SimEvent::Message {
                    text: format!("{}\n", text),
                    time_fs: self.time,
                });
            }
        }
    }

    // ---- continuous assigns ---------------------------------------------------------

    /// Evaluate all continuous assigns until stable. Returns true if any signal
    /// changed (which may have scheduled processes).
    fn eval_assigns_fixpoint(&mut self) -> bool {
        let mut any = false;
        self.cur_proc = Some(u32::MAX);
        self.fn_calls.set(0); // per-fixpoint call budget (see run_func)
        for sweep in 0..10_000 {
            self.delta_guard += 1;
            self.fix_sweep = sweep as u64 + 1;
            self.maybe_heartbeat();
            let mut changed = false;
            let mut n_changed = 0u64;
            let n_assigns = self.flat.assigns.len();
            for ai in 0..n_assigns {
                let a = self.flat.assigns[ai].clone();
                let t0 = std::time::Instant::now();
                let rhs = self.eval_expr(&a.rhs, None);
                let rhs = self.convert_for_target(&a.lhs, &a.rhs, rhs, None);
                let dt = t0.elapsed();
                if std::env::var_os("HDLSIM_DBG_SLOWASSIGN").is_some() && dt.as_millis() > 500 {
                    eprintln!(
                        "DBG slow assign#{ai} took {:.1}s calls={} lhs {:.140?} rhs {:.140?}",
                        dt.as_secs_f64(),
                        self.fn_calls.get(),
                        a.lhs,
                        a.rhs
                    );
                }
                if self.assign_to(&a.lhs, rhs, None) {
                    changed = true;
                    n_changed += 1;
                }
                // A non-converging fixpoint whose assigns call heavy
                // functions would grind through all 10_000 sweeps before
                // the pump can report the latched runaway-call budget —
                // surface it mid-sweep (a check per assign is cheap).
                if ai % 64 == 0 {
                    self.fix_changed = n_changed;
                    if self.report_fn_runaway() {
                        self.cur_proc = None;
                        return any;
                    }
                    // Same for an external interrupt: report it at the pump
                    // level (with the location) instead of grinding on. Keep
                    // the sentinel cur_proc so the dump names the sweep.
                    if self.stop_requested() {
                        return any;
                    }
                    self.maybe_heartbeat();
                }
            }
            self.fix_changed = n_changed;
            any |= changed;
            if !changed {
                self.cur_proc = None;
                return any;
            }
        }
        self.cur_proc = None;
        self.emit(SimEvent::Message {
            text: format!(
                "error: combinational loop detected at t={} — continuous assignments never settle (a signal's value feeds back into its own RHS with no clock in between); simulation stopped\\n",
                fmt_time(self.time)
            ),
            time_fs: self.time,
        });
        // Unconverged continuous assigns will oscillate every delta — an
        // unbounded failure would otherwise spin the delta loop forever
        // without time ever advancing (so --max-time could not stop it).
        self.stopped = true;
        self.error_stopped = true;
        any
    }

    // ---- signal update + scheduling ---------------------------------------------------

    /// Returns true if the signal actually changed.
    fn set_sig(&mut self, sig: FlatSigId, v: Value) -> bool {
        let old = self.vals[sig as usize].clone();
        let new = v;
        if old == new {
            return false;
        }
        self.prev[sig as usize] = old.clone();
        self.vals[sig as usize] = new.clone();
        self.emit(SimEvent::SignalChange {
            sig,
            old,
            new: new.clone(),
            time_fs: self.time,
        });
        if let Some(p) = self.cur_proc {
            if self.last_driver[sig as usize].map(|(q, _)| q) != Some(p) {
                self.emit(SimEvent::Driver {
                    sig,
                    proc: p,
                    time_fs: self.time,
                });
            }
            self.last_driver[sig as usize] = Some((p, self.time));
        }
        self.schedule_sensitive(sig, &self.prev[sig as usize].clone(), &new);
        true
    }

    /// Wake processes sensitive to a memory (array) signal after an element
    /// write. Memory words live in `mems`, not `vals`, so element writes
    /// bypass `set_sig` — but a VHDL event on any element is still an event
    /// on the signal (LRM 1076.4 sensitivity sets use the longest static
    /// prefix), so a process parked on the array must wake. Only a value
    /// change wakes, matching the scalar path.
    fn wake_mem_watchers(&mut self, sig: usize, a: usize, old_elem: Value) {
        let new_elem = self.mems[sig][a].clone();
        if old_elem != new_elem {
            self.schedule_sensitive(sig as FlatSigId, &old_elem, &new_elem);
        }
    }

    fn schedule_sensitive(&mut self, sig: FlatSigId, old: &Value, new: &Value) {
        // Lowering-internal signals (`__for_i` loop variables, `__pN_var_*`
        // variable mirrors) are not VHDL signals: writing them must never
        // wake a process. A (ALL) process's collect_sigs sensitivity
        // includes them — the loop reads its own variable, the body reads
        // its variables — so an unfiltered wake lets a process wake on its
        // own loop/variable writes: an infinite zero-delay loop (observed
        // as a (ALL) tb stimulus process re-running on every `__for_i`
        // iteration write). Flat names are instance-qualified
        // (`inst.__p0_var_x`), so test the last path segment. VHDL
        // identifiers cannot begin with `__`, so the prefix is
        // unambiguous.
        if self.flat.signals[sig as usize]
            .name
            .rsplit('.')
            .next()
            .map_or(false, |n| n.starts_with("__"))
        {
            return;
        }
        let edge_of = |ek: &EdgeKind| match ek {
            EdgeKind::Pos => new.bit(0) == V1 && old.bit(0) != V1,
            EdgeKind::Neg => new.bit(0) == V0 && old.bit(0) != V0,
            EdgeKind::Any => true,
        };
        // Fanout-driven wake: only processes whose CURRENT sensitivity (declared
        // list, or the wait they are parked on) includes `sig` are considered —
        // O(fanout) instead of scanning every process per write. Collect first
        // (borrows self.sens_index), then wake.
        let mut to_wake: Vec<u32> = Vec::new();
        for (pid, ek) in self.sens_index[sig as usize].iter() {
            if edge_of(ek) {
                to_wake.push(*pid);
            }
        }
        for pid in to_wake {
            if self.parked[pid as usize] {
                self.trace(format!(
                    "{}: woken by edge on signal {sig} ({})",
                    self.flat.procs[pid as usize].origin, self.flat.signals[sig as usize].name
                ));
            }
            self.unpark(pid);
            self.activate(pid);
        }
    }

    // ---- assignment targets -------------------------------------------------------------

    /// Cap for growing a memory on an indexed write (`HDLSIM_MAX_SIGNAL_MB`,
    /// shared with `alloc_report`'s per-signal estimate): a write at an index
    /// in the 2³² range is an underflowed/negative address from an
    /// unelaborated generic, and `resize` would abort the process (96 GiB
    /// class) before any RSS guard could run. Over the cap: report the
    /// signal, the index, and the declared depth, and stop the run.
    fn mem_grow_ok(&mut self, sig: usize, a: usize) -> bool {
        const DEFAULT_MAX_SIGNAL_MB: u64 = 4096;
        let cap_bytes = std::env::var("HDLSIM_MAX_SIGNAL_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_SIGNAL_MB)
            * 1024
            * 1024;
        // Same estimate as `alloc_report`: a stored word costs its Value
        // header (~24 bytes) plus 1 byte per bit — a 1-bit memory at 2³²
        // words is ~96 GiB of headers alone, so the cap must be on bytes,
        // not word count.
        let bytes = (24 + self.flat.signals[sig].width.max(1) as u64).saturating_mul(a as u64 + 1);
        if bytes <= cap_bytes {
            return true;
        }
        // Report the first violation only: a runaway loop writes thousands of
        // out-of-range indexes within one delta, and each report is a full
        // sentence.
        if !self.error_stopped {
            self.emit(SimEvent::Message {
                text: format!(
                    "error: memory write to '{}' at index {} would grow it past \
                     HDLSIM_MAX_SIGNAL_MB (declared depth {} words) — the index is \
                     probably an underflowed or negative address (an unelaborated \
                     generic or range); raise HDLSIM_MAX_SIGNAL_MB only if the \
                     design is genuinely this big",
                    self.flat.signals[sig].name,
                    a,
                    self.flat.signals[sig].mem.unwrap_or(0)
                ),
                time_fs: self.time,
            });
        }
        self.stopped = true;
        self.error_stopped = true;
        false
    }

    /// Assign `v` to the target of `lhs`. Returns true if a signal changed.
    fn assign_to(&mut self, lhs: &Expr, v: Value, locals: Option<&Vec<Value>>) -> bool {
        match lhs {
            Expr::Sig(s) => {
                let sig = *s as FlatSigId;
                if self.is_mem[sig as usize] {
                    return false; // bare memory assign not meaningful
                }
                let w = self.flat.signals[sig as usize].width.max(1);
                self.set_sig(sig, v.resize(w))
            }
            Expr::Idx { e, i } => {
                let addr = self.eval_expr(i, locals);
                let Some(a) = addr.to_u64() else { return false };
                match &**e {
                    Expr::Sig(s) if self.is_mem[*s as usize] => {
                        let sig = *s as usize;
                        let w = self.flat.signals[sig].width.max(1);
                        if (a as usize) < self.mems[sig].len() {
                            let old = self.mems[sig][a as usize].clone();
                            self.mems[sig][a as usize] = v.resize(w);
                            self.wake_mem_watchers(sig, a as usize, old);
                        } else if self.mem_grow_ok(sig, a as usize) {
                            let old = Value::x(w);
                            self.mems[sig].resize(a as usize + 1, Value::x(w));
                            self.mems[sig][a as usize] = v.resize(w);
                            self.wake_mem_watchers(sig, a as usize, old);
                        }
                        false // memory element: no scalar event phase 1
                    }
                    _ => {
                        // Bit write.
                        let sig = match &**e {
                            Expr::Sig(s) => *s as FlatSigId,
                            _ => return false,
                        };
                        let mut cur = self.vals[sig as usize].clone();
                        cur.set_bit(a as usize, v.bit(0));
                        self.set_sig(sig, cur)
                    }
                }
            }
            Expr::Sel { e, msb, lsb } => {
                let msb = self.eval_expr(msb, locals).to_u64().unwrap_or(0) as usize;
                let lsb = self.eval_expr(lsb, locals).to_u64().unwrap_or(0) as usize;
                // `arr(i).field <= v` — slice write into a memory element.
                if let Expr::Idx { e: ie, i } = &**e {
                    if let Expr::Sig(s) = &**ie {
                        let sig = *s as usize;
                        if self.is_mem[sig] {
                            let Some(a) = self.eval_expr(i, locals).to_u64() else {
                                return false;
                            };
                            let w = self.flat.signals[sig].width.max(1);
                            if (a as usize) >= self.mems[sig].len()
                                && !self.mem_grow_ok(sig, a as usize)
                            {
                                return false;
                            }
                            if (a as usize) >= self.mems[sig].len() {
                                self.mems[sig].resize(a as usize + 1, Value::x(w));
                            }
                            let old = self.mems[sig][a as usize].clone();
                            let mut cur = old.clone();
                            let v = v.resize(msb - lsb + 1);
                            for j in 0..=(msb - lsb) {
                                cur.set_bit(lsb + j, v.bit(j));
                            }
                            self.mems[sig][a as usize] = cur;
                            self.wake_mem_watchers(sig, a as usize, old);
                            return false; // memory element: no scalar event phase 1
                        }
                    }
                    return false;
                }
                let sig = match &**e {
                    Expr::Sig(s) => *s as FlatSigId,
                    _ => return false,
                };
                let mut cur = self.vals[sig as usize].clone();
                let v = v.resize(msb - lsb + 1);
                for j in 0..=(msb - lsb) {
                    cur.set_bit(lsb + j, v.bit(j));
                }
                self.set_sig(sig, cur)
            }
            Expr::Concat(parts) => {
                // Split v across parts: parts[0] takes the most-significant
                // slice, matching Value::concat.
                let widths: Vec<usize> = parts.iter().map(|p| self.expr_width(p, locals)).collect();
                let total: usize = widths.iter().sum::<usize>().max(1);
                let mut changed = false;
                let mut hi = total; // exclusive upper bound of the remaining slice
                for (p, w) in parts.iter().zip(widths.iter()) {
                    let lo = hi.saturating_sub(*w);
                    let part_val = v.slice((hi - 1).min(total - 1), lo);
                    changed |= self.assign_to(p, part_val, locals);
                    hi = lo;
                }
                changed
            }
            _ => false,
        }
    }

    /// Whether an expression is a signed-typed value (VHDL `signed`).
    fn expr_signed(&self, e: &Expr, locals: Option<&Vec<Value>>) -> bool {
        match e {
            Expr::Sig(s) => {
                let i = *s as usize;
                if let Some(l) = locals {
                    if i < l.len() {
                        return false; // function/variable slots are unsigned
                    }
                }
                i < self.flat.signals.len() && self.flat.signals[i].signed
            }
            Expr::Bin { op, l, r } => {
                // Shifts/rotates preserve the operand's signedness (the amount
                // is just a count).
                if matches!(
                    op,
                    BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rol | BinOp::Ror
                ) {
                    self.expr_signed(l, locals)
                } else {
                    self.expr_signed(l, locals) && self.expr_signed(r, locals)
                }
            }
            Expr::Un { e, .. } => self.expr_signed(e, locals),
            Expr::Resize { e, signed, .. } => *signed || self.expr_signed(e, locals),
            Expr::Signed(_) => true,
            _ => false,
        }
    }

    fn expr_width(&self, e: &Expr, locals: Option<&Vec<Value>>) -> usize {
        match e {
            Expr::Lit(v) => v.width(),
            Expr::Real(_) => 64,
            Expr::SigAttr { .. } => 32,
            Expr::Sig(s) => {
                if let Some(l) = locals {
                    if (*s as usize) < l.len() {
                        return l[*s as usize].width();
                    }
                }
                self.flat.signals[*s as usize].width.max(1)
            }
            Expr::Bin { l, r, .. } => self.expr_width(l, locals).max(self.expr_width(r, locals)),
            Expr::Un { e, .. } => self.expr_width(e, locals),
            Expr::Cond { t, e, .. } => self.expr_width(t, locals).max(self.expr_width(e, locals)),
            Expr::Concat(parts) => parts.iter().map(|p| self.expr_width(p, locals)).sum(),
            Expr::Sel { msb, lsb, .. } => {
                let a = self.eval_expr(msb, locals).to_u64().unwrap_or(1) as usize;
                let b = self.eval_expr(lsb, locals).to_u64().unwrap_or(0) as usize;
                a.abs_diff(b) + 1
            }
            Expr::Idx { .. } => 1,
            Expr::Call { name, args } => self
                .func_match(name, args, locals)
                .map(|i| self.flat.functions[i].ret_width)
                .unwrap_or(1),
            Expr::Replicate { e, .. } => self.expr_width(e, locals),
            Expr::Rising(_) | Expr::Falling(_) => 1,
            Expr::Prev(e) => self.expr_width(e, locals),
            Expr::ToInt(_) => 32,
            Expr::ToString(_) => 8 * 12, // decimal digits, upper bound
            Expr::Resize { width, .. } => *width,
            Expr::Signed(e) => self.expr_width(e, locals),
        }
    }

    // ---- expression evaluation -----------------------------------------------------------

    /// Whether an expression is real-typed (Verilog/VHDL `real`).
    fn expr_real(&self, e: &Expr, locals: Option<&Vec<Value>>) -> bool {
        match e {
            Expr::Real(_) => true,
            Expr::Sig(s) => {
                let i = *s as usize;
                if let Some(l) = locals {
                    if i < l.len() {
                        return false; // function/variable slots are not real
                    }
                }
                i < self.flat.signals.len() && self.flat.signals[i].kind == SigKind::Real
            }
            // Record-field select on a real-typed field (`r.re`): decode the
            // slice as IEEE-754 instead of an integer bit pattern. Constant
            // bounds only — the lowerer emits literals for record fields.
            Expr::Sel { e, msb, lsb } => {
                let (Expr::Sig(s), Some(m), Some(l)) = (
                    &**e,
                    self.const_bound(msb, locals),
                    self.const_bound(lsb, locals),
                ) else {
                    return false;
                };
                let i = *s as usize;
                if let Some(lc) = locals {
                    if i < lc.len() {
                        return false;
                    }
                }
                self.flat
                    .signals
                    .get(i)
                    .map(|s| s.real_ranges.iter().any(|&(lo, hi)| lo == l && hi == m))
                    .unwrap_or(false)
            }
            // Only arithmetic propagates realness; comparisons/logical ops
            // yield 1-bit results even on real operands.
            Expr::Bin { op, l, r } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    self.expr_real(l, locals) || self.expr_real(r, locals)
                }
                _ => false,
            },
            Expr::Un { op, e } => match op {
                UnOp::Neg | UnOp::Pos | UnOp::Abs => self.expr_real(e, locals),
                _ => false,
            },
            Expr::Resize { e, .. } | Expr::Signed(e) => self.expr_real(e, locals),
            Expr::Cond { c, t, e } => {
                self.expr_real(c, locals) || self.expr_real(t, locals) || self.expr_real(e, locals)
            }
            _ => false,
        }
    }

    /// Constant bound of a select (record-field bounds are literals from the
    /// lowerer); dynamic bounds conservatively report None.
    fn const_bound(&self, e: &Expr, locals: Option<&Vec<Value>>) -> Option<u32> {
        if matches!(e, Expr::Lit(_)) {
            return self.eval_expr(e, locals).to_u64().map(|v| v as u32);
        }
        None
    }

    /// One operand of a real-typed binop as f64: real operands decode their
    /// bit pattern, integer operands convert their integer value.
    fn operand_f64(&self, e: &Expr, v: &Value, locals: Option<&Vec<Value>>) -> Option<f64> {
        if self.expr_real(e, locals) || v.real {
            v.to_f64()
        } else {
            v.to_u64().map(|n| n as f64)
        }
    }

    /// IEEE-754 arithmetic/comparison for real-typed binops.
    fn eval_bin_real(
        &self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        lv: &Value,
        rv: &Value,
        locals: Option<&Vec<Value>>,
    ) -> Value {
        let a = self.operand_f64(l, lv, locals);
        let b = self.operand_f64(r, rv, locals);
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => match (a, b) {
                (Some(x), Some(y)) => Value::from_f64(match op {
                    BinOp::Add => x + y,
                    BinOp::Sub => x - y,
                    BinOp::Mul => x * y,
                    BinOp::Div => x / y,
                    _ => x % y,
                }),
                _ => Value::x(64),
            },
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => match (a, b) {
                (Some(x), Some(y)) => Value::from_u64(
                    1,
                    match op {
                        BinOp::Lt => x < y,
                        BinOp::Le => x <= y,
                        BinOp::Gt => x > y,
                        BinOp::Ge => x >= y,
                        BinOp::Eq => x == y,
                        _ => x != y,
                    } as u64,
                ),
                _ => Value::x(1),
            },
            _ => Value::x(64), // bitwise/shift ops on reals: undefined
        }
    }

    /// Convert an assigned value to the target signal's type: integer -> real
    /// (value becomes f64), real -> integer (round to nearest, per Verilog).
    fn convert_for_target(
        &self,
        lhs: &Expr,
        rhs: &Expr,
        v: Value,
        locals: Option<&Vec<Value>>,
    ) -> Value {
        if let Expr::Sig(s) = lhs {
            let i = *s as usize;
            if i < self.flat.signals.len() {
                let sig = &self.flat.signals[i];
                let target_real = sig.kind == SigKind::Real;
                let src_real = self.expr_real(rhs, locals);
                if target_real && !src_real {
                    return match v.to_u64() {
                        Some(n) => Value::from_f64(n as f64),
                        None => Value::x(64),
                    };
                }
                if !target_real && src_real {
                    let w = sig.width.max(1);
                    return match v.to_f64() {
                        Some(f) => Value::from_u64(w, (f.round() as i64) as u64),
                        None => Value::x(w),
                    };
                }
            }
        }
        v
    }

    fn eval_expr(&self, e: &Expr, locals: Option<&Vec<Value>>) -> Value {
        match e {
            Expr::Lit(v) => v.clone(),
            Expr::Real(x) => Value::from_f64(*x),
            Expr::SigAttr { sig, attr } => {
                let sig = *sig as usize;
                // Function-local slots share the id space with flat signals
                // (Expr::Sig resolves locals first); a formal's `sig'length`
                // must come from the bound value, not an unrelated flat
                // signal.
                if let Some(l) = locals {
                    if sig < l.len() {
                        let w = l[sig].width().max(1);
                        let v = match *attr {
                            "length" => w as u64,
                            "high" | "left" => (w as u64).saturating_sub(1),
                            _ => 0, // low | right
                        };
                        return Value::from_u64(32, v);
                    }
                }
                let w = self.flat.signals[sig].width.max(1);
                let n = self
                    .mems
                    .get(sig)
                    .filter(|m| !m.is_empty())
                    .map_or(w, |m| m.len().max(1));
                let v = match *attr {
                    "length" => n as u64,
                    "high" | "left" => (n as u64).saturating_sub(1),
                    _ => 0, // low | right
                };
                Value::from_u64(32, v)
            }
            Expr::Sig(s) => {
                let i = *s as usize;
                if let Some(l) = locals {
                    if i < l.len() {
                        return l[i].clone();
                    }
                }
                if i < self.vals.len() {
                    self.vals[i].clone()
                } else {
                    Value::x(1)
                }
            }
            Expr::Bin { op, l, r } => {
                let lv = self.eval_expr(l, locals);
                let rv = self.eval_expr(r, locals);
                // Real-typed expression: IEEE-754 arithmetic on f64 operands.
                // A value-level marker covers function frames, whose slots
                // have no type metadata (`expr_real` cannot see a real
                // formal/variable behind a slot id).
                if self.expr_real(l, locals) || self.expr_real(r, locals) || lv.real || rv.real {
                    return self.eval_bin_real(*op, l, r, &lv, &rv, locals);
                }
                // VHDL signed operands: sign-extend both to the wider width
                // before arithmetic/comparison (zero-extend is the default).
                if self.expr_signed(l, locals) || self.expr_signed(r, locals) {
                    // Shifts/rotates: the amount must not widen the result.
                    let w = if matches!(
                        op,
                        BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rol | BinOp::Ror
                    ) {
                        lv.width().max(1)
                    } else {
                        lv.width().max(rv.width()).max(1)
                    };
                    let l2 = sign_extend(&lv, w);
                    let r2 = sign_extend(&rv, w);
                    // Comparisons must be signed; arithmetic is two's
                    // complement so the unsigned binop is exact for it.
                    if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                        let to_i = |v: &Value| -> Option<i128> {
                            if !v.is_known() {
                                return None;
                            }
                            let mut x: i128 = 0;
                            for (i, &b) in v.bits.iter().enumerate() {
                                if b == V1 {
                                    x |= 1 << i;
                                }
                            }
                            if w < 128 && v.bit(w - 1) == V1 {
                                x |= -1i128 << w; // sign bit
                            }
                            Some(x)
                        };
                        return match (to_i(&l2), to_i(&r2)) {
                            (Some(a), Some(b)) => Value::from_u64(
                                1,
                                match op {
                                    BinOp::Lt => a < b,
                                    BinOp::Le => a <= b,
                                    BinOp::Gt => a > b,
                                    _ => a >= b,
                                } as u64,
                            ),
                            _ => Value::x(1),
                        };
                    }
                    return l2.binop(*op, &r2);
                }
                lv.binop(*op, &rv)
            }
            Expr::Rising(e) => {
                let v = self.eval_expr(e, locals);
                let i = match &**e {
                    Expr::Sig(s) => *s as usize,
                    _ => return Value::x(1),
                };
                let cur = v.bit(0);
                let prev = if i < self.prev.len() {
                    self.prev[i].bit(0)
                } else {
                    VX
                };
                Value::from_u64(1, (cur == V1 && prev == V0) as u64)
            }
            Expr::Falling(e) => {
                let v = self.eval_expr(e, locals);
                let i = match &**e {
                    Expr::Sig(s) => *s as usize,
                    _ => return Value::x(1),
                };
                let cur = v.bit(0);
                let prev = if i < self.prev.len() {
                    self.prev[i].bit(0)
                } else {
                    VX
                };
                Value::from_u64(1, (cur == V0 && prev == V1) as u64)
            }
            Expr::Prev(e) => {
                // `sig'last_value`: the value before the current delta.
                let v = self.eval_expr(e, locals);
                match &**e {
                    Expr::Sig(s) => {
                        let i = *s as usize;
                        if i < self.prev.len() {
                            self.prev[i].clone()
                        } else {
                            v
                        }
                    }
                    _ => v,
                }
            }
            Expr::ToInt(e) => {
                let v = self.eval_expr(e, locals);
                // Real operand (`integer(r)`, `to_integer(r)`): round to the
                // nearest integer, ties away from zero (VHDL LRM). The value
                // marker covers real formals/locals in function frames.
                if self.expr_real(e, locals) || v.real {
                    return match v.to_f64() {
                        Some(f) => Value::from_u64(32, (f.round() as i64) as u64),
                        None => Value::x(32),
                    };
                }
                match v.to_u64() {
                    Some(n) => {
                        // Signed operand (e.g. `to_integer(signed_val)`): decode
                        // as a signed value, keep the 32-bit two's complement.
                        if self.expr_signed(e, locals) {
                            let w = v.width();
                            let x = if w < 64 && (n >> (w - 1)) & 1 == 1 {
                                (n | (u64::MAX << w)) as i64
                            } else {
                                n as i64
                            };
                            Value::from_u64(32, x as u64)
                        } else {
                            Value::from_u64(32, n)
                        }
                    }
                    None => Value::x(32),
                }
            }
            Expr::ToString(e) => {
                let v = self.eval_expr(e, locals);
                let s = if self.expr_real(e, locals) {
                    match v.to_f64() {
                        Some(f) => format!("{}", f),
                        None => "x".to_string(),
                    }
                } else {
                    match v.to_u64() {
                        Some(n) => n.to_string(),
                        None => "x".to_string(),
                    }
                };
                pack_str_value(&s)
            }
            Expr::Un { op, e } => {
                let v = self.eval_expr(e, locals);
                if self.expr_real(e, locals) || v.real {
                    return match (op, v.to_f64()) {
                        (UnOp::Neg, Some(x)) => Value::from_f64(-x),
                        (UnOp::Pos, Some(x)) => Value::from_f64(x),
                        (UnOp::Abs, Some(x)) => Value::from_f64(x.abs()),
                        _ => Value::x(64),
                    };
                }
                v.unop(*op)
            }
            Expr::Cond { c, t, e } => {
                let cv = self.eval_expr(c, locals);
                if truthy(&cv) {
                    self.eval_expr(t, locals)
                } else {
                    self.eval_expr(e, locals)
                }
            }
            Expr::Concat(parts) => {
                let vals: Vec<Value> = parts.iter().map(|p| self.eval_expr(p, locals)).collect();
                Value::concat(&vals)
            }
            Expr::Replicate { count, e } => {
                let n = self.eval_expr(count, locals).to_u64().unwrap_or(0) as usize;
                let v = self.eval_expr(e, locals);
                Value::concat(&vec![v; n])
            }
            Expr::Resize { e, width, signed } => {
                let v = self.eval_expr(e, locals);
                if *signed && v.width() < *width {
                    sign_extend(&v, *width)
                } else {
                    v.resize(*width)
                }
            }
            Expr::Signed(e) => self.eval_expr(e, locals),
            Expr::Sel { e, msb, lsb } => {
                let v = self.eval_expr(e, locals);
                let msb = self.eval_expr(msb, locals).to_u64().unwrap_or(0) as usize;
                let lsb = self.eval_expr(lsb, locals).to_u64().unwrap_or(0) as usize;
                v.slice(msb, lsb)
            }
            Expr::Idx { e, i } => {
                let addr = self.eval_expr(i, locals);
                let Some(a) = addr.to_u64() else {
                    return Value::x(1);
                };
                match &**e {
                    Expr::Sig(s)
                        if (*s as usize) < self.is_mem.len() && self.is_mem[*s as usize] =>
                    {
                        let sig = *s as usize;
                        self.mems[sig]
                            .get(a as usize)
                            .cloned()
                            .unwrap_or_else(|| Value::x(self.flat.signals[sig].width.max(1)))
                    }
                    _ => {
                        let v = self.eval_expr(e, locals);
                        Value::from_bit(v.bit(a as usize))
                    }
                }
            }
            Expr::Call { name, args } => self.eval_call(name, args, locals),
        }
    }

    fn eval_call(&self, name: &str, args: &[Expr], locals: Option<&Vec<Value>>) -> Value {
        // System functions.
        match name {
            "clog2" => {
                let v = self.eval_expr(&args[0], locals);
                let n = v.to_u64().unwrap_or(1);
                return Value::from_u64(32, (64 - n.leading_zeros()).max(1) as u64);
            }
            // VHDL `type'val(x)` / `type'pos(x)` — numeric identity (width is
            // applied by a Resize at the lower site).
            "$val" | "$pos" if !args.is_empty() => return self.eval_expr(&args[0], locals),
            "unsigned" | "signed" => return self.eval_expr(&args[0], locals),
            // std.standard `now` — current simulation time in fs.
            "$now" => return Value::from_u64(64, self.time),
            // `$const_idx(val, i, w)`: element `i` (MSB-first) of a flat
            // constant array value with element width `w`.
            "$const_idx" if args.len() == 3 => {
                let v = self.eval_expr(&args[0], locals);
                let a = self.eval_expr(&args[1], locals).to_u64().unwrap_or(0) as usize;
                let w = self
                    .eval_expr(&args[2], locals)
                    .to_u64()
                    .unwrap_or(1)
                    .max(1) as usize;
                let total = v.width();
                return if total >= (a + 1) * w && a * w < total {
                    v.slice(total - 1 - a * w, total - (a + 1) * w)
                } else {
                    Value::x(w)
                };
            }
            // Array size: `$array_size(arr)` (foreach) or `q.size()`.
            "$array_size" => {
                if let Some(Expr::Sig(sig)) = args.first() {
                    let sig = *sig as FlatSigId;
                    let n = self.mems.get(sig as usize).map_or(0, |m| m.len());
                    return Value::from_u64(32, n as u64);
                }
                return Value::x(32);
            }

            // VHDL-2008 string functions: to_hstring/to_ostring/to_bstring give
            // radix digits (uppercase, 'X' for unknown nibbles); to_string of a
            // 1-bit value is its char, of a byte-multiple value its bytes.
            "to_hstring" | "to_ostring" | "to_bstring" | "to_string" if !args.is_empty() => {
                let v = self.eval_expr(&args[0], locals);
                let s = match name {
                    "to_hstring" => vhdl_radix_string(&v, 16),
                    "to_ostring" => vhdl_radix_string(&v, 8),
                    "to_bstring" => vhdl_radix_string(&v, 2),
                    _ => {
                        // VHDL to_string of a real: decimal; of a vector: one
                        // char per bit.
                        if self.expr_real(&args[0], locals) {
                            match v.to_f64() {
                                Some(f) => format!("{}", f),
                                None => "x".to_string(),
                            }
                        } else if v.width() == 1 {
                            match v.bits[0] {
                                V0 => "0".to_string(),
                                V1 => "1".to_string(),
                                VZ => "Z".to_string(),
                                VDASH => "-".to_string(),
                                _ => "X".to_string(),
                            }
                        } else {
                            v.bits
                                .iter()
                                .map(|&b| match b {
                                    V0 => '0',
                                    V1 => '1',
                                    VZ => 'Z',
                                    VDASH => '-',
                                    VU => 'U',
                                    _ => 'X',
                                })
                                .collect()
                        }
                    }
                };
                return pack_str_value(&s);
            }
            "time" | "realtime" => return Value::from_u64(64, self.time),
            // std.textio `endfile(f)`: peek at the file position (stdin: no EOF).
            "endfile" => {
                let path = args
                    .first()
                    .map(|a| string_of(&self.eval_expr(a, locals)))
                    .unwrap_or_default();
                if path == "stdin" {
                    return Value::from_u64(1, 0);
                }
                let pos = self.textio_pos.borrow().get(&path).copied().unwrap_or(0);
                let total = std::fs::read_to_string(&path)
                    .map(|t| t.lines().count())
                    .unwrap_or(0);
                return Value::from_u64(1, if pos >= total { 1 } else { 0 });
            }
            "random" | "urandom" => return Value::from_u64(32, self.next_random() as u64),
            "urandom_range" => {
                // $urandom_range(max) -> 0..=max, $urandom_range(min, max).
                let arg = |i: usize| self.eval_expr(&args[i], locals).to_u64().unwrap_or(0) as i64;
                let (lo, hi) = match args.len() {
                    1 => (0, arg(0)),
                    2 => (arg(0), arg(1)),
                    _ => return Value::x(32),
                };
                return match self.urandom_range(lo, hi) {
                    Some(v) => Value::from_u64(32, v as u32 as u64),
                    None => Value::x(32),
                };
            }
            _ => {}
        }
        // SV class support: `$this`, field reads, method calls.
        if name == "$this" {
            return Value::from_u64(64, self.cur_obj.get());
        }
        // Field write `obj.field = v` (expression form, works in functions too).
        if let Some(rest) = name.strip_prefix("$setf::") {
            let (cnf, lw) = rest.split_once('@').unwrap_or((rest, "0,1"));
            let cn = cnf.split("::").next().unwrap_or("");
            let mut it = lw.split(',');
            let lsb = it.next().and_then(|x| x.parse::<usize>().ok()).unwrap_or(0);
            let w = it
                .next()
                .and_then(|x| x.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            if args.len() >= 2 {
                let handle = self
                    .eval_expr(&args[0], locals)
                    .to_u64()
                    .unwrap_or(u64::MAX);
                let v = self.eval_expr(&args[1], locals).resize(w);
                self.write_field(cn, handle, lsb, w, &v);
            }
            return Value::from_u64(32, 1);
        }
        // `c.randomize()` — solve rand-field domains + constraints, write the
        // object's fields, return 1 on success / 0 on failure.
        if let Some(rest) = name.strip_prefix("$randomize::") {
            let (cn, lay) = rest.split_once('@').unwrap_or((rest, ""));
            let mut layout: HashMap<String, (usize, usize)> = HashMap::new();
            for ent in lay.split(';') {
                if let Some((n, lw)) = ent.split_once('=') {
                    let mut it = lw.split(',');
                    let lsb = it.next().and_then(|x| x.parse::<usize>().ok()).unwrap_or(0);
                    let w = it
                        .next()
                        .and_then(|x| x.parse::<usize>().ok())
                        .unwrap_or(1)
                        .max(1);
                    layout.insert(n.to_string(), (lsb, w));
                }
            }
            if args.len() >= 2 {
                let handle = self
                    .eval_expr(&args[0], locals)
                    .to_u64()
                    .unwrap_or(u64::MAX);
                let spec = unpack_str_value(&self.eval_expr(&args[1], locals));
                let sols = self.with_rng(|r| rand::solve_spec(r, &spec, 4096));
                match sols {
                    Ok(sols) => {
                        for (n, val) in sols {
                            if let Some(&(lsb, w)) = layout.get(&n) {
                                let v = Value::from_u64(w, val as u64).resize(w);
                                self.write_field(cn, handle, lsb, w, &v);
                            }
                        }
                        return Value::from_u64(32, 1);
                    }
                    Err(_) => return Value::from_u64(32, 0),
                }
            }
            return Value::from_u64(32, 0);
        }
        if let Some(rest) = name.strip_prefix("$getf::") {
            let (cnf, lw) = rest.split_once('@').unwrap_or((rest, "0,1"));
            let cn = cnf.split("::").next().unwrap_or("");
            let mut it = lw.split(',');
            let lsb = it.next().and_then(|x| x.parse::<usize>().ok()).unwrap_or(0);
            let w = it
                .next()
                .and_then(|x| x.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            let handle = args
                .first()
                .map(|a| self.eval_expr(a, locals).to_u64().unwrap_or(u64::MAX))
                .unwrap_or(u64::MAX);
            return self.read_field(cn, handle, lsb, w);
        }
        if let Some(rest) = name.strip_prefix("$method::") {
            // `c.m(args)` in expression position; funcs are keyed `C::m`.
            if let Some(fi) = self.func_match(rest, args.get(1..).unwrap_or(&[]), locals) {
                let obj = args
                    .first()
                    .map(|a| self.eval_expr(a, locals).to_u64().unwrap_or(u64::MAX))
                    .unwrap_or(u64::MAX);
                let saved = self.cur_obj.replace(obj);
                let out = self.run_func(fi, &args[1..], locals);
                self.cur_obj.set(saved);
                return out;
            }
            return Value::x(32);
        }
        if let Some(fi) = self.func_match(name, args, locals) {
            return self.run_func(fi, args, locals);
        }
        Value::x(32)
    }

    /// Execute function `fi` with the given argument expressions; returns
    /// slot 0 (the function-name slot, i.e. the return value). Shared by
    /// plain calls and class method calls.
    /// Resolve a name to one function. Overloads share a bare name. Among
    /// the candidates a call fits (exact formal count, or every omitted
    /// trailing formal carries an interface default), prefer the one whose
    /// provided-formal widths match the actual widths — width is the only
    /// type signal, and it is what separates `conv_float(integer)` from
    /// `conv_float(real)` (`real(x)` lowers to a w64 expression). Then an
    /// exact formal-count match (VHDL likewise prefers the overload that
    /// binds without default values), then any fitting candidate, then the
    /// first-registered one — a width-mismatched argument reads X rather
    /// than failing.
    fn func_match(&self, name: &str, args: &[Expr], locals: Option<&Vec<Value>>) -> Option<usize> {
        let nargs = args.len();
        // VHDL identifiers are case-insensitive (LRM 1076 §15.2); the table
        // is keyed lowercase so a `ROUND(signed)` body answers a `round(...)`
        // call site. The lowerer's table is keyed the same way.
        let cands = self.funcs.get(&name.to_ascii_lowercase())?;
        if cands.len() == 1 {
            return Some(cands[0]);
        }
        // Interface-only stubs (spec declarations whose body never landed —
        // e.g. the built-in ieee.math_real spec) must never win a width tie
        // against a real body: they execute as empty bodies returning X,
        // which callers loop on forever. Try real bodies first; stubs are a
        // last resort.
        let is_stub_fn = |i: usize| match &*self.flat.functions[i].body {
            Stmt::Null => true,
            Stmt::Block(ss) => ss.is_empty(),
            _ => false,
        };
        let mut ordered: Vec<usize> = cands.iter().copied().filter(|&i| !is_stub_fn(i)).collect();
        ordered.extend(cands.iter().copied().filter(|&i| is_stub_fn(i)));
        let cands = &ordered;
        let fits = |&i: &usize| {
            let f = &self.flat.functions[i];
            f.arity == nargs
                || (f.arity > nargs
                    && f.defaults
                        .get(nargs..f.arity)
                        .is_some_and(|t| t.iter().all(|d| d.is_some())))
        };
        // Width agreement on the provided actuals; formal width <= 1 is
        // unconstrained/unknown and fits anything. A Sig id resolves in the
        // caller's locals first (function slots), then global signals; an id
        // in neither must not panic — it counts as width-unknown.
        let arg_w = |a: &Expr| match a {
            Expr::Sig(s)
                if locals.map_or(true, |l| (*s as usize) >= l.len())
                    && (*s as usize) >= self.flat.signals.len() =>
            {
                0
            }
            _ => self.expr_width(a, locals),
        };
        // Exact width first: an unconstrained (w <= 1) formal is a wildcard
        // that must not absorb a call an exact-width overload can take —
        // `conv_float(real_arg)` must reach the w64 real overload, not the
        // w1 std_ulogic_vector overload that then recurses through
        // bit-identical type conversions.
        let width_exact = |&i: &usize| {
            let f = &self.flat.functions[i];
            args.iter()
                .enumerate()
                .all(|(j, a)| matches!(f.slots.get(j + 1), Some(&fw) if fw == arg_w(a)))
        };
        let width_wild = |&i: &usize| {
            let f = &self.flat.functions[i];
            args.iter()
                .enumerate()
                .all(|(j, a)| match f.slots.get(j + 1) {
                    Some(&fw) => fw == arg_w(a) || fw <= 1,
                    None => true,
                })
        };
        // A conversion-marked actual carries the declared type of the value
        // it wraps: `signed(data)` lowers to `Expr::Signed` (bit-identical),
        // the same signal VHDL overload resolution keys on. Same-width
        // unconstrained overloads (`signed`/`unsigned`/`std_ulogic_vector`,
        // all w1) are told apart only by the formal's declared type
        // (`FlatFunc::formal_types`) — widths tie, and picking a wrong one
        // lets the delegating overload bodies recurse forever
        // (`conv_float_phase(signed(data))` re-entering the
        // std_ulogic_vector overload). Prefer candidates whose formals match
        // every hint; fall back to the width-only rules when none does
        // (unknown formal types, or the hinted overload is not registered).
        let arg_hint = |a: &Expr| match a {
            Expr::Signed(_) | Expr::Resize { signed: true, .. } => Some("signed"),
            _ => None,
        };
        let type_ok = |&i: &usize| {
            let f = &self.flat.functions[i];
            args.iter().enumerate().all(|(j, a)| match arg_hint(a) {
                None => true,
                Some(h) => f
                    .formal_types
                    .get(j)
                    .map_or(true, |t| t.is_empty() || t == h),
            })
        };
        let pick_in = |pool: &[usize]| -> Option<usize> {
            pool.iter()
                .copied()
                .find(|i| width_exact(i))
                .or_else(|| pool.iter().copied().find(|i| width_wild(i)))
                .or_else(|| {
                    pool.iter()
                        .copied()
                        .find(|&i| self.flat.functions[i].arity == nargs)
                })
                .or_else(|| pool.iter().copied().find(fits))
        };
        let fitting: Vec<usize> = cands.iter().copied().filter(|i| fits(i)).collect();
        let typed: Vec<usize> = fitting.iter().copied().filter(|i| type_ok(i)).collect();
        let picked = pick_in(&typed)
            .or_else(|| pick_in(&fitting))
            .or_else(|| {
                cands
                    .iter()
                    .copied()
                    .find(|&i| self.flat.functions[i].arity == nargs)
            })
            .or_else(|| cands.iter().copied().find(fits))
            .unwrap_or(cands[0]);
        // A self-call that passes a formal through UNCHANGED — the body
        // lowered `f(unsigned(x))` to `f(Sig(x))` (bit-identical type
        // conversion) or `f(signed(x))` to `f(Signed(Sig(x)))` — receives
        // the same value every time and can never make progress: it
        // recurses to the depth guard. Overload families with same-width
        // unconstrained formals (`conv_float(signed)` vs
        // `conv_float(unsigned)`, both w1) hit exactly this. Dispatch to
        // the next fitting candidate instead of the executing function.
        let picked = if Some(picked) == self.cur_fn.get() {
            // A formal passed through a bare `Sig` or through the
            // bit-identical `Signed` marker reaches this function with the
            // same value every time — unwrap both to the slot id.
            let pass_through = match args {
                [Expr::Sig(s)] => Some(*s),
                [Expr::Signed(inner)] => match inner.as_ref() {
                    Expr::Sig(s) => Some(*s),
                    _ => None,
                },
                _ => None,
            };
            match pass_through {
                Some(s) if locals.map_or(false, |l| (s as usize) < l.len()) => {
                    let alt = |j: &usize| *j != picked && fits(j);
                    let alts: Vec<usize> = cands.iter().copied().filter(|j| alt(j)).collect();
                    let typed_alts: Vec<usize> =
                        alts.iter().copied().filter(|j| type_ok(j)).collect();
                    pick_in(&typed_alts)
                        .or_else(|| pick_in(&alts))
                        .unwrap_or(picked)
                }
                _ => picked,
            }
        } else {
            picked
        };
        if std::env::var_os("HDLSIM_TRACE_DISPATCH").is_some() {
            fn find_span(s: &Stmt) -> Option<(u32, u32)> {
                match s {
                    Stmt::Spanned { span, .. } => Some((span.file.0, span.start)),
                    Stmt::Block(ss) => ss.iter().find_map(find_span),
                    _ => None,
                }
            }
            let sp = find_span(&self.flat.functions[picked].body);
            let caller = self
                .cur_fn
                .get()
                .map(|i| self.flat.functions[i].name.clone())
                .unwrap_or_else(|| "<proc>".to_string());
            eprintln!(
                "dispatch {name} nargs={nargs} -> {} (caller {caller}, cands {:?}, sigs [{}], argw {:?}, span {:?})",
                picked,
                cands,
                cands.iter()
                    .map(|&i| {
                        let f = &self.flat.functions[i];
                        format!(
                            "{} ar{} ret{} {}",
                            f.name,
                            f.arity,
                            f.ret_width,
                            f.slots.iter()
                                .map(|w| w.to_string())
                                .collect::<Vec<_>>()
                                .join(",")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" | "),
                args.iter().map(|a| arg_w(a)).collect::<Vec<_>>(),
                sp,
            );
            // HDLSIM_TRACE_PICKED=1: dump the picked body (Debug) for the
            // first few picks per name — identifies which overload a hot
            // call site binds to without flooding multi-million-line logs.
            if std::env::var_os("HDLSIM_TRACE_PICKED").is_some() {
                use std::cell::RefCell;
                use std::collections::HashMap;
                thread_local! {
                    static PICKED: RefCell<HashMap<(String, usize), u32>> =
                        RefCell::new(HashMap::new());
                }
                let dump = PICKED.with(|m| {
                    let mut m = m.borrow_mut();
                    let n = m.entry((name.to_ascii_lowercase(), picked)).or_insert(0);
                    *n += 1;
                    *n <= 1
                });
                if dump {
                    let body = format!("{:?}", self.flat.functions[picked].body);
                    eprintln!(
                        "picked {name} #{picked} body {}",
                        &body[..body.len().min(900)]
                    );
                }
            }
        }
        Some(picked)
    }

    /// Execute function `fi` with the given argument expressions; returns
    /// slot 0 (the function-name slot, i.e. the return value). Shared by
    /// plain calls and class method calls.
    fn run_func(&self, fi: usize, args: &[Expr], locals: Option<&Vec<Value>>) -> Value {
        let f = &self.flat.functions[fi];
        // Runaway-loop budget: count every call made at runtime (not
        // const-eval/elaboration, which run with cur_proc = None). A loop
        // whose body only calls functions never returns to `exec_frames`,
        // so the interpreter-step budget there cannot see it — see
        // `report_fn_runaway`. Continuous assigns evaluate under the
        // fixpoint's u32::MAX sentinel; those calls are attributed to the
        // fixpoint itself.
        if let Some(pid) = self.cur_proc {
            let calls = self.fn_calls.get() + 1;
            self.fn_calls.set(calls);
            if calls > self.max_fn_calls && self.fn_runaway.borrow().is_none() {
                let origin = if pid == u32::MAX {
                    "<continuous-assign fixpoint>".to_string()
                } else {
                    self.flat.procs[pid as usize].origin.clone()
                };
                self.fn_runaway
                    .borrow_mut()
                    .get_or_insert((origin, f.name.clone()));
                self.fn_runaway_hit.set(true);
                self.capture_fn_chain();
            }
        }
        // Latched: unwind with X so the enclosing evaluation (fixpoint
        // sweep, process statement) reaches a latch check promptly instead
        // of grinding through the runaway computation.
        if self.fn_runaway_hit.get() {
            return Value::x(f.ret_width.max(1));
        }
        // Recursion guard: a self-recursive function (or a cycle of
        // mutually recursive overloads) would otherwise overflow the
        // stack. Latch the name and unwind with X; the pump reports the
        // latch and stops the run.
        let d = self.fn_depth.get() + 1;
        if d > self.max_func_depth {
            self.fn_recursion.borrow_mut().get_or_insert(f.name.clone());
            self.capture_fn_chain();
            return Value::x(f.ret_width.max(1));
        }
        self.fn_depth.set(d);
        self.fn_stack.borrow_mut().push(fi);
        if d == 1 {
            self.cur_root_fn.set(Some(fi));
        }
        // HDLSIM_TRACE_CALLS=1: log the roots of call trees (depth 0→1) —
        // identifies which process/assign drives a hot function without
        // flooding multi-million-call logs. Capped at 300 lines.
        if std::env::var_os("HDLSIM_TRACE_CALLS").is_some() && d == 1 {
            thread_local! {
                static ROOTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
            }
            let n = ROOTS.with(|c| {
                let n = c.get() + 1;
                c.set(n);
                n
            });
            if n <= 300 {
                let origin = match self.cur_proc {
                    None => "<none>".to_string(),
                    Some(u32::MAX) => "<assign-fixpoint>".to_string(),
                    Some(p) => self.flat.procs[p as usize].origin.clone(),
                };
                eprintln!("fnroot #{n} {} proc={origin} t={}fs", f.name, self.time);
            }
        }
        let mut lvals: Vec<Value> = Vec::with_capacity(f.slots.len());
        lvals.push(Value::x(f.ret_width)); // slot 0 = function name (return value)
        for (i, a) in args.iter().enumerate() {
            let w = f.slots.get(i + 1).copied().unwrap_or(1);
            lvals.push(self.eval_expr(a, locals).resize(w));
        }
        // Omitted trailing actuals take the formals' recorded interface
        // defaults (`w : natural := 8`); anything after the formals (locals)
        // starts X as before.
        while lvals.len() < (f.arity + 1).min(f.slots.len()) {
            let j = lvals.len() - 1;
            let w = f.slots.get(j + 1).copied().unwrap_or(1);
            let v = match f.defaults.get(j).and_then(|d| d.as_ref()) {
                Some(d) => d.clone().resize(w),
                None => Value::x(w),
            };
            lvals.push(v);
        }
        while lvals.len() < f.slots.len() {
            lvals.push(Value::x(f.slots[lvals.len()]));
        }
        // Functions with delays are not supported — execute recursively.
        let saved_fn = self.cur_fn.replace(Some(fi));
        let mut runner = FnRunner {
            sim: self,
            locals: &mut lvals,
            returned: false,
        };
        runner.exec(&f.body);
        let r = lvals[0].clone();
        self.cur_fn.set(saved_fn);
        self.fn_depth.set(d - 1);
        self.fn_stack.borrow_mut().pop();
        r
    }

    /// Run a user procedure called in statement position. Unlike `run_func`
    /// (expression position, pure), the body executes with `&mut self`, so
    /// syscalls (report/assert/finish/textio) and signal assignment take
    /// effect instead of being silently dropped.
    fn run_procedure(&mut self, fi: usize, args: &[Expr], caller: Option<&Vec<Value>>) {
        let f = self.flat.functions[fi].clone();
        // Same recursion guard as `run_func` (shared `fn_depth`, so function
        // ↔ procedure call chains count together). Procedure calls from a
        // process count toward the same runaway-call budget.
        let d = self.fn_depth.get() + 1;
        if d > self.max_func_depth {
            self.fn_recursion.borrow_mut().get_or_insert(f.name.clone());
            return;
        }
        if let Some(pid) = self.cur_proc {
            let calls = self.fn_calls.get() + 1;
            self.fn_calls.set(calls);
            if calls > self.max_fn_calls && self.fn_runaway.borrow().is_none() {
                let origin = if pid == u32::MAX {
                    "<continuous-assign fixpoint>".to_string()
                } else {
                    self.flat.procs[pid as usize].origin.clone()
                };
                self.fn_runaway
                    .borrow_mut()
                    .get_or_insert((origin, f.name.clone()));
                self.fn_runaway_hit.set(true);
                self.capture_fn_chain();
            }
        }
        if self.fn_runaway_hit.get() {
            return; // unwind (see run_func)
        }
        self.fn_depth.set(d);
        self.fn_stack.borrow_mut().push(fi);
        let mut lvals: Vec<Value> = Vec::with_capacity(f.slots.len());
        lvals.push(Value::x(f.ret_width.max(1))); // slot 0 = return slot (unused)
        for (i, a) in args.iter().enumerate() {
            let w = f.slots.get(i + 1).copied().unwrap_or(1);
            let v = self.eval_expr(a, caller);
            // An actual wider than the declared slot (unconstrained string
            // parameters) keeps its full width — the slot is a lower bound,
            // never a truncation.
            lvals.push(v.resize(w.max(v.width())));
        }
        while lvals.len() < f.slots.len() {
            lvals.push(Value::x(f.slots[lvals.len()]));
        }
        let saved_fn = self.cur_fn.replace(Some(fi));
        let mut runner = ProcRunner {
            sim: self,
            locals: &mut lvals,
            returned: false,
        };
        runner.exec(&f.body);
        self.cur_fn.set(saved_fn);
        self.fn_depth.set(d - 1);
        self.fn_stack.borrow_mut().pop();
    }

    /// Read field bits `[lsb..lsb+w)` of the object word for `handle` in
    /// class `cn` (null handle → all X).
    fn read_field(&self, cn: &str, handle: u64, lsb: usize, w: usize) -> Value {
        if handle == u64::MAX {
            return Value::x(w);
        }
        let objs = self.objs.borrow();
        if let Some(word) = objs.get(cn).and_then(|m| m.get(handle as usize)) {
            let mut bits = vec![VX; w];
            for (j, b) in bits.iter_mut().enumerate() {
                if let Some(&v) = word.bits.get(lsb + j) {
                    *b = v;
                }
            }
            return Value { bits, real: false };
        }
        Value::x(w)
    }

    /// Write field bits `[lsb..lsb+w)` of the object word for `handle`.
    fn write_field(&self, cn: &str, handle: u64, lsb: usize, w: usize, v: &Value) {
        if handle == u64::MAX {
            return;
        }
        let mut objs = self.objs.borrow_mut();
        if let Some(word) = objs.get_mut(cn).and_then(|m| m.get_mut(handle as usize)) {
            while word.bits.len() < lsb + w {
                word.bits.push(VX);
            }
            for j in 0..w {
                word.bits[lsb + j] = v.bits.get(j).copied().unwrap_or(VX);
            }
        }
    }

    /// Run `f` with the kernel RNG, persisting the advanced state. The state
    /// lives in `self.rng`, which checkpoints serialize, so stimulus drawn
    /// here stays reproducible across checkpoint/restore.
    fn with_rng<T>(&self, f: impl FnOnce(&mut Rng) -> T) -> T {
        let mut r = Rng::from_state(self.rng.get());
        let out = f(&mut r);
        self.rng.set(r.state());
        out
    }

    /// `$random`/`$urandom` value (xorshift64*), advancing the seed.
    pub fn next_random(&self) -> u32 {
        self.with_rng(|r| r.next_u32())
    }

    /// `$urandom` — same stream and values as `$random`, seeded via `--seed`.
    pub fn urandom(&self) -> u32 {
        self.next_random()
    }

    /// `$urandom_range(lo, hi)` — uniform in the inclusive range (`None` when
    /// `hi < lo`); rejection-sampled, so no modulo bias.
    pub fn urandom_range(&self, lo: i64, hi: i64) -> Option<i64> {
        self.with_rng(|r| r.range(lo, hi))
    }

    /// Solve a `$rand_solve` constraint spec with the kernel RNG; returns the
    /// variable bindings in declaration order. Advances the RNG, so a fixed
    /// `--seed` reproduces the same solution.
    pub fn rand_solve(&self, spec: &str, max_attempts: u32) -> Result<Vec<(String, i64)>, String> {
        self.with_rng(|r| rand::solve_spec(r, spec, max_attempts))
    }

    /// Current `$random` RNG state.
    pub fn seed(&self) -> u64 {
        self.rng.get()
    }

    /// Override the `$random` RNG seed (0 keeps the built-in default), e.g.
    /// from `--seed` for reproducible stimulus. Persisted in checkpoints.
    pub fn set_seed(&self, seed: u64) {
        self.rng
            .set(if seed == 0 { 0x243F6A8885A308D3 } else { seed });
    }
}

/// Helper: run function bodies (recursive, no delays) with a local slot space.
/// Pure: syscalls other than class-field/randomize and real-signal assignment
/// are dropped — statement-position subprogram calls use `ProcRunner` instead.
struct FnRunner<'a> {
    sim: &'a Simulator,
    locals: &'a mut Vec<Value>,
    /// Set when a `return` executed — stops the rest of the function body.
    returned: bool,
}

impl<'a> FnRunner<'a> {
    fn exec(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Spanned { inner, .. } => self.exec(inner),
            Stmt::Return(e) => {
                let v = self.sim.eval_expr(e, Some(self.locals));
                let w = self.locals[0].width().max(1);
                self.locals[0] = v.resize(w);
                self.returned = true;
            }
            Stmt::Block(ss) => {
                for s in ss {
                    self.exec(s);
                    if self.returned {
                        return;
                    }
                }
            }
            Stmt::If { cond, then, els } => {
                let c = self.sim.eval_expr(cond, Some(self.locals));
                if truthy(&c) {
                    self.exec(then);
                } else if let Some(e) = els {
                    self.exec(e);
                }
            }
            Stmt::Assign { lhs, rhs, .. } => {
                let v = self.sim.eval_expr(rhs, Some(self.locals));
                match lhs {
                    Expr::Sig(s) => {
                        let i = *s as usize;
                        if i < self.locals.len() {
                            let w = self.locals[i].width();
                            self.locals[i] = v.resize(w.max(1));
                        }
                    }
                    // `v(i) := x` / `v(hi downto lo) := x` on a local slot:
                    // splice the rhs bits in at the offset, growing with X.
                    Expr::Idx { e, i } => {
                        if let Expr::Sig(s) = &**e {
                            let idx = self.sim.eval_expr(i, Some(self.locals));
                            if let Some(a) = idx.to_u64() {
                                let s = *s as usize;
                                let a = a as usize;
                                if s < self.locals.len() {
                                    let w = v.width().max(1);
                                    if self.locals[s].bits.len() < a + w {
                                        let mut bits = self.locals[s].bits.clone();
                                        bits.resize(a + w, VX);
                                        self.locals[s] = Value { bits, real: false };
                                    }
                                    for b in 0..w {
                                        self.locals[s].set_bit(a + b, v.bit(b));
                                    }
                                }
                            }
                        }
                    }
                    Expr::Sel { e, msb, lsb } => {
                        if let Expr::Sig(s) = &**e {
                            let m = self.sim.eval_expr(msb, Some(self.locals));
                            let l = self.sim.eval_expr(lsb, Some(self.locals));
                            if let (Some(m), Some(l)) = (m.to_u64(), l.to_u64()) {
                                let s = *s as usize;
                                let (m, l) = (m as usize, l as usize);
                                if s < self.locals.len() {
                                    let w = m.saturating_sub(l) + 1;
                                    if self.locals[s].bits.len() < l + w {
                                        let mut bits = self.locals[s].bits.clone();
                                        bits.resize(l + w, VX);
                                        self.locals[s] = Value { bits, real: false };
                                    }
                                    for b in 0..w {
                                        self.locals[s].set_bit(l + b, v.bit(b));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Stmt::Case {
                expr,
                items,
                default,
                matching,
            } => {
                let v = self.sim.eval_expr(expr, Some(self.locals));
                for (conds, body) in items {
                    for c in conds {
                        let cv = self.sim.eval_expr(c, Some(self.locals));
                        let hit = if *matching {
                            v.matches_case(&cv)
                        } else {
                            eq_strict(&v, &cv)
                        };
                        if hit {
                            self.exec(body);
                            return;
                        }
                    }
                }
                if let Some(d) = default {
                    self.exec(d);
                }
            }
            Stmt::ExprStmt(e) => {
                self.sim.eval_expr(e, Some(self.locals));
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.exec(init);
                let mut guard = 0u32;
                loop {
                    let c = self.sim.eval_expr(cond, Some(self.locals));
                    if !truthy(&c) {
                        break;
                    }
                    self.exec(body);
                    if self.returned {
                        return;
                    }
                    self.exec(step);
                    guard += 1;
                    if guard % 1024 == 0 && self.sim.stop_requested() {
                        break;
                    }
                    if guard > 1_000_000 {
                        break;
                    }
                }
            }
            Stmt::SysCall { name, args } => {
                // Class field writes / randomize are &self-capable: run them
                // as calls. Other syscalls need &mut self — only reachable
                // here via `ProcRunner`-dispatched nested expression calls.
                if name.starts_with("$setf::") || name.starts_with("$randomize::") {
                    self.sim.eval_call(name, args, Some(self.locals));
                }
            }
            _ => {}
        }
    }
}

/// Helper: run procedure bodies (statement-position subprogram calls) with
/// full side effects. `FnRunner` holds `&Simulator`, so syscalls and signal
/// assignment would be dropped; this runner holds `&mut Simulator`.
/// `wait`/`#delay` inside a procedure still cannot suspend the caller (the
/// body runs inline within one `sys_call`), so those warn and continue.
struct ProcRunner<'a> {
    sim: &'a mut Simulator,
    locals: &'a mut Vec<Value>,
    /// Set when a `return` executed — stops the rest of the body.
    returned: bool,
}

impl<'a> ProcRunner<'a> {
    fn exec(&mut self, stmt: &Stmt) -> Control {
        match stmt {
            Stmt::Spanned { inner, .. } => self.exec(inner),
            Stmt::Return(e) => {
                let v = self.sim.eval_expr(e, Some(self.locals));
                let w = self.locals[0].width().max(1);
                self.locals[0] = v.resize(w);
                self.returned = true;
                Control::Done
            }
            Stmt::Block(ss) => {
                for s in ss {
                    match self.exec(s) {
                        Control::Done => {
                            if self.returned {
                                return Control::Done;
                            }
                        }
                        other => return other,
                    }
                }
                Control::Done
            }
            Stmt::If { cond, then, els } => {
                let c = self.sim.eval_expr(cond, Some(self.locals));
                if truthy(&c) {
                    self.exec(then)
                } else if let Some(e) = els {
                    self.exec(e)
                } else {
                    Control::Done
                }
            }
            Stmt::Assign { lhs, rhs, nba } => {
                let v = self.sim.eval_expr(rhs, Some(self.locals));
                self.assign(lhs, rhs, v, *nba);
                Control::Done
            }
            Stmt::AssignDelayed { lhs, rhs, delay } => {
                let v = self.sim.eval_expr(rhs, Some(self.locals));
                let t = self
                    .sim
                    .eval_expr(delay, Some(self.locals))
                    .to_u64()
                    .unwrap_or(0);
                if t == 0 {
                    self.assign(lhs, rhs, v, true);
                } else if let Some(sig) = lhs_sig(lhs) {
                    let w = self.sim.flat.signals[sig as usize].width;
                    let v = self.sim.convert_for_target(lhs, rhs, v, Some(self.locals));
                    self.sim
                        .time_events
                        .entry(self.sim.time + t)
                        .or_default()
                        .push(Ev::Nba(sig, v.resize(w.max(1)), self.sim.cur_proc));
                } else {
                    let lhs = self.sim.concretize_target(lhs);
                    self.sim
                        .time_events
                        .entry(self.sim.time + t)
                        .or_default()
                        .push(Ev::NbaExpr(lhs, v, self.sim.cur_proc));
                }
                Control::Done
            }
            Stmt::Case {
                expr,
                items,
                default,
                matching,
            } => {
                let v = self.sim.eval_expr(expr, Some(self.locals));
                for (conds, body) in items {
                    for c in conds {
                        let cv = self.sim.eval_expr(c, Some(self.locals));
                        let hit = if *matching {
                            v.matches_case(&cv)
                        } else {
                            eq_strict(&v, &cv)
                        };
                        if hit {
                            return self.exec(body);
                        }
                    }
                }
                if let Some(d) = default {
                    return self.exec(d);
                }
                Control::Done
            }
            Stmt::ExprStmt(e) => {
                self.sim.eval_expr(e, Some(self.locals));
                Control::Done
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.exec(init);
                let mut guard = 0u32;
                loop {
                    let c = self.sim.eval_expr(cond, Some(self.locals));
                    if !truthy(&c) {
                        break;
                    }
                    match self.exec(body) {
                        Control::Break => break,
                        Control::Continue => {}
                        Control::Done => {
                            if self.returned {
                                return Control::Done;
                            }
                        }
                        _ => {}
                    }
                    self.exec(step);
                    guard += 1;
                    if guard % 1024 == 0 && self.sim.stop_requested() {
                        break;
                    }
                    if guard > 1_000_000 {
                        break;
                    }
                }
                Control::Done
            }
            Stmt::Repeat { count, body } => {
                let n = self
                    .sim
                    .eval_expr(count, Some(self.locals))
                    .to_u64()
                    .unwrap_or(0);
                for _ in 0..n {
                    match self.exec(body) {
                        Control::Break => break,
                        Control::Done | Control::Continue => {
                            if self.returned {
                                return Control::Done;
                            }
                        }
                        _ => {}
                    }
                }
                Control::Done
            }
            Stmt::SysCall { name, args } => {
                self.sim
                    .sys_call(name, args.clone(), Some(&mut *self.locals));
                Control::Done
            }
            Stmt::Break => Control::Break,
            Stmt::Continue => Control::Continue,
            Stmt::Delay { .. } | Stmt::Wait { .. } | Stmt::WaitUntil { .. } => {
                // The body runs inline inside one `sys_call`, so a suspend
                // cannot return to the caller with its position saved. Warn
                // instead of silently ignoring the statement.
                self.sim.emit(SimEvent::Message {
                    text: format!(
                        "warning: wait/delay inside a procedure is not supported; continuing without suspending at t={}\n",
                        self.sim.time
                    ),
                    time_fs: self.sim.time,
                });
                Control::Done
            }
            Stmt::Null => Control::Done,
        }
    }

    /// Assign `v` to `lhs` in a procedure body: frame-slot writes for
    /// variables (direct, indexed, or sliced), real-signal writes for
    /// signals — blocking or NBA, mirroring the process executor.
    fn assign(&mut self, lhs: &Expr, rhs: &Expr, v: Value, nba: bool) {
        let base = match lhs {
            Expr::Sig(_) => Some(lhs),
            Expr::Idx { e, .. } | Expr::Sel { e, .. } => Some(&**e),
            _ => None,
        };
        if let Some(Expr::Sig(s)) = base {
            if (*s as usize) < self.locals.len() {
                self.assign_local(lhs, v);
                return;
            }
        }
        if nba {
            if let Some(sig) = lhs_sig(lhs) {
                let w = self.sim.flat.signals[sig as usize].width;
                let v = self.sim.convert_for_target(lhs, rhs, v, Some(self.locals));
                self.sim
                    .nba
                    .push((sig, v.resize(w.max(1)), self.sim.cur_proc));
            } else {
                let lhs = self.sim.concretize_target(lhs);
                self.sim.nba_apply_later(lhs, v);
            }
        } else {
            let v = self.sim.convert_for_target(lhs, rhs, v, Some(self.locals));
            self.sim.assign_to(lhs, v, Some(self.locals));
        }
    }

    /// Write a local slot: direct, indexed (`v(i) := x`), or sliced
    /// (`v(hi downto lo) := x`), mirroring `FnRunner`.
    fn assign_local(&mut self, lhs: &Expr, v: Value) {
        match lhs {
            Expr::Sig(s) => {
                let i = *s as usize;
                if i < self.locals.len() {
                    let w = self.locals[i].width();
                    self.locals[i] = v.resize(w.max(1));
                }
            }
            // `v(i) := x` / `v(hi downto lo) := x` on a local slot:
            // splice the rhs bits in at the offset, growing with X.
            Expr::Idx { e, i } => {
                if let Expr::Sig(s) = &**e {
                    let idx = self.sim.eval_expr(i, Some(self.locals));
                    if let Some(a) = idx.to_u64() {
                        let s = *s as usize;
                        let a = a as usize;
                        if s < self.locals.len() {
                            let w = v.width().max(1);
                            if self.locals[s].bits.len() < a + w {
                                let mut bits = self.locals[s].bits.clone();
                                bits.resize(a + w, VX);
                                self.locals[s] = Value { bits, real: false };
                            }
                            for b in 0..w {
                                self.locals[s].set_bit(a + b, v.bit(b));
                            }
                        }
                    }
                }
            }
            Expr::Sel { e, msb, lsb } => {
                if let Expr::Sig(s) = &**e {
                    let m = self.sim.eval_expr(msb, Some(self.locals));
                    let l = self.sim.eval_expr(lsb, Some(self.locals));
                    if let (Some(m), Some(l)) = (m.to_u64(), l.to_u64()) {
                        let s = *s as usize;
                        let (m, l) = (m as usize, l as usize);
                        if s < self.locals.len() {
                            let w = m.saturating_sub(l) + 1;
                            if self.locals[s].bits.len() < l + w {
                                let mut bits = self.locals[s].bits.clone();
                                bits.resize(l + w, VX);
                                self.locals[s] = Value { bits, real: false };
                            }
                            for b in 0..w {
                                self.locals[s].set_bit(l + b, v.bit(b));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Sign-extend `v` to `width` bits (VHDL signed semantics).
fn sign_extend(v: &Value, width: usize) -> Value {
    let w = width.max(v.width()).max(1);
    let mut bits = vec![V0; w];
    for i in 0..v.width().min(w) {
        bits[i] = v.bit(i);
    }
    let sign = v.bit(v.width().saturating_sub(1));
    if sign != V0 && sign != V1 {
        for i in v.width()..w {
            bits[i] = sign; // X/Z sign-extends as X/Z
        }
    } else if sign == V1 {
        for i in v.width()..w {
            bits[i] = V1;
        }
    }
    Value { bits, real: false }
}

/// HDLSIM_PROGRESS_SEC=<seconds> — set once per process; when > 0, the pump
/// prints a periodic "still running" heartbeat (see `maybe_heartbeat`).
fn progress_interval() -> std::time::Duration {
    static SECS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    let secs = *SECS.get_or_init(|| {
        std::env::var("HDLSIM_PROGRESS_SEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0)
    });
    std::time::Duration::from_secs_f64(secs.max(0.0))
}

fn truthy(v: &Value) -> bool {
    v.to_u64().map_or(false, |x| x != 0)
}

fn eq_strict(a: &Value, b: &Value) -> bool {
    a.binop(BinOp::EqStrict, b).to_u64() == Some(1)
}

fn lhs_sig(e: &Expr) -> Option<FlatSigId> {
    match e {
        Expr::Sig(s) => Some(*s),
        _ => None,
    }
}

fn string_of(v: &Value) -> String {
    // Interpret value bits as bytes, MSB chunk first (first char = high byte,
    // matching how string literals are packed and Value::concat orders parts).
    let mut bytes = Vec::new();
    for chunk in v.bits.chunks(8).rev() {
        let mut byte = 0u8;
        for (j, &b) in chunk.iter().enumerate() {
            if b == V1 {
                byte |= 1 << j;
            }
        }
        if byte != 0 {
            bytes.push(byte);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::elab::FlatSig;

    fn flat_with(widths_mems: &[(usize, Option<usize>)]) -> Flat {
        Flat {
            signals: widths_mems
                .iter()
                .map(|(w, d)| FlatSig {
                    name: "s".to_string(),
                    width: *w,
                    kind: SigKind::Wire,
                    init: None,
                    mem: *d,
                    scope: 0,
                    signed: false,
                    real_ranges: Vec::new(),
                })
                .collect(),
            procs: Vec::new(),
            assigns: Vec::new(),
            functions: Vec::new(),
            scopes: Vec::new(),
            top_scope: 0,
        }
    }

    #[test]
    fn alloc_report_allows_ordinary_signals() {
        let f = flat_with(&[(32, None), (8, Some(1024))]);
        assert!(Simulator::alloc_report_with_caps(&f, 4096 << 20, None).is_ok());
    }

    #[test]
    fn alloc_report_refuses_runaway_width_and_names_it() {
        // A 2^32-bit signal — the classic underflowed `0 to N-1` range.
        let f = flat_with(&[(32, None), (1usize << 32, None)]);
        let err = Simulator::alloc_report_with_caps(&f, 4096 << 20, None)
            .expect_err("runaway width must be refused");
        assert!(err.contains("largest allocators"), "{err}");
    }

    #[test]
    fn alloc_report_counts_value_headers_on_narrow_mems() {
        // 2^32 one-bit words cost ~96 GiB of Value headers alone — the cap
        // must be on bytes, not word count.
        let f = flat_with(&[(1, Some((1usize << 32) - 2))]);
        assert!(Simulator::alloc_report_with_caps(&f, 4096 << 20, None).is_err());
    }

    #[test]
    fn alloc_report_honors_total_cap() {
        let f = flat_with(&[(32, Some(1 << 20))]);
        assert!(Simulator::alloc_report_with_caps(&f, 4096 << 20, Some(1 << 20)).is_err());
    }
}
