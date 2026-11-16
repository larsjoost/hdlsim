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
    Value { bits }
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
use hdl_ir::{BinOp, EdgeKind, Expr, ProcKind, Sens, SigId, SigKind, Stmt, UnOp};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// Scheduled future event.
#[derive(Clone, Debug)]
enum Ev {
    /// Resume a suspended process at a time.
    Resume(u32),
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
    funcs: HashMap<String, usize>,
    observers: Vec<Box<dyn Observer>>,
    finished: bool,
    stopped: bool,
    started: bool,
    finish_notified: bool,
    /// Number of test failures seen (severity error/failure, `$error`/`$fatal`).
    failures: u64,
    monitor: Vec<Vec<Expr>>,
    monitor_last: Vec<Option<Vec<Value>>>,
    delta_guard: u32,
    /// Deferred NBA writes to non-scalar targets (part-selects, memories).
    nba_exprs: Vec<(Expr, Value, Option<u32>)>,
    /// Process currently executing (for driver tracing); `None` outside
    /// processes, `Some(u32::MAX)` while re-evaluating continuous assigns.
    cur_proc: Option<u32>,
    /// Per-signal last driver: (proc, time_fs). proc == u32::MAX = assign.
    last_driver: Vec<Option<(u32, u64)>>,
    /// Processes parked at `@(...)` event waits: (proc, edges).
    waiting: Vec<(u32, Vec<(FlatSigId, EdgeKind)>)>,
    /// RNG state for $random (Cell so expression evaluation can stay &self).
    rng: std::cell::Cell<u64>,
    /// Object memory signal for SV classes: class name → `__objmem_<Class>`
    /// (used for the object word width).
    objmem: HashMap<String, FlatSigId>,
    /// Live class objects: class name → one word per allocated object.
    /// RefCell so field writes work from &self (function bodies) too.
    objs: std::cell::RefCell<HashMap<String, Vec<Value>>>,
    /// Current object handle while executing a class method (`$this`).
    cur_obj: std::cell::Cell<u64>,
    /// External stop request (e.g. the live wave window's Stop button):
    /// checked once per event-loop iteration in `run`.
    stop_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Simulator {
    pub fn new(flat: Flat) -> Simulator {
        let n = flat.signals.len();
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
                    Some(depth) => vec![Value::x(s.width.max(1)); depth],
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
            failures: 0,
            rng: std::cell::Cell::new(0x243F6A8885A308D3),
            objmem,
            objs: std::cell::RefCell::new(HashMap::new()),
            cur_obj: std::cell::Cell::new(u64::MAX),
            stop_flag: None,
            monitor: Vec::new(),
            monitor_last: Vec::new(),
            delta_guard: 0,
            nba_exprs: Vec::new(),
            waiting: Vec::new(),
        };
        sim.prev = sim.vals.clone();
        for (i, f) in sim.flat.functions.iter().enumerate() {
            sim.funcs.insert(f.name.clone(), i);
        }
        sim.proc_rt = (0..sim.flat.procs.len())
            .map(|_| ProcRt::default())
            .collect();
        for (i, p) in sim.flat.procs.iter().enumerate() {
            match &p.sens {
                Sens::List(es) => {
                    sim.proc_sens
                        .push(es.iter().map(|e| (e.sig, e.kind)).collect());
                    // Keep `proc_all` index-aligned with `procs`.
                    sim.proc_all.push(Vec::new());
                }
                Sens::All => {
                    let mut sigs = Vec::new();
                    p.body.collect_sigs(&mut sigs);
                    sigs.sort_unstable();
                    sigs.dedup();
                    sim.proc_sens.push(
                        sigs.iter()
                            .map(|s| (*s as FlatSigId, EdgeKind::Any))
                            .collect(),
                    );
                    sim.proc_all
                        .push(sigs.iter().map(|s| *s as FlatSigId).collect());
                }
            }
            let _ = i;
        }
        sim
    }

    /// Install an external stop flag (e.g. the live wave window's Stop
    /// button): when set, `run` finishes at the next loop iteration.
    pub fn set_stop_flag(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.stop_flag = Some(flag);
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
        out
    }

    fn emit(&mut self, ev: SimEvent) {
        for o in &mut self.observers {
            o.on_event(&ev);
        }
    }

    /// Record a test failure (emits [`SimEvent::TestFailed`]).
    fn fail(&mut self, text: String) {
        self.failures += 1;
        let time_fs = self.time;
        self.emit(SimEvent::TestFailed { text, time_fs });
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

    fn pump(&mut self, max_time_fs: u64, soft: bool) {
        loop {
            if self.finished || self.stopped {
                break;
            }
            if let Some(f) = &self.stop_flag {
                if f.load(std::sync::atomic::Ordering::Relaxed) {
                    self.finished = true;
                    break;
                }
            }
            let mut drain_guard = 0u64;
            while let Some(pid) = self.active.pop_front() {
                drain_guard += 1;
                if drain_guard > 1_000_000 {
                    self.emit(SimEvent::Message {
                        text: format!("error: zero-delay process loop detected at t={}", self.time),
                        time_fs: self.time,
                    });
                    self.stopped = true;
                    break;
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
                    self.cur_proc = drv;
                    self.set_sig(sig, v);
                }
                let exprs = std::mem::take(&mut self.nba_exprs);
                for (lhs, v, drv) in exprs {
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
                    self.time = t;
                    self.emit(SimEvent::TimeAdvance { time_fs: t });
                    let evs = self.time_events.remove(&t).unwrap_or_default();
                    for ev in evs {
                        match ev {
                            Ev::Resume(pid) => self.activate(pid),
                        }
                    }
                }
                _ => {
                    if soft {
                        break; // pause at the target; simulation stays resumable
                    }
                    self.emit(SimEvent::Stop { time_fs: self.time });
                    self.stopped = true;
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
        if self.active.iter().any(|p| *p == pid) || self.waiting.iter().any(|(w, _)| *w == pid) {
            return;
        }
        self.active.push_back(pid);
    }

    /// Park a process on an event list, at most once per process.
    fn park(&mut self, pid: u32, edges: Vec<(FlatSigId, EdgeKind)>) {
        if self.waiting.iter().any(|(w, _)| *w == pid) {
            return;
        }
        self.waiting.push((pid, edges));
    }

    fn run_proc(&mut self, pid: u32) {
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
                Control::Wait => break, // parked in self.waiting until an edge
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
        }
        loop {
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
                }
            };
            match action {
                Act::Exec(s) => match self.exec_stmt(pid, s) {
                    Control::Done => continue,
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
                self.sys_call(&name, args);
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

    fn sys_call(&mut self, name: &str, args: Vec<Expr>) {
        match name {
            // `arr = new[n]` — resize a dynamic array / queue.
            "$array_new" => {
                if let (Some(Expr::Sig(sig)), Some(n)) = (args.first(), args.get(1)) {
                    let n = self.eval_expr(n, None).to_u64().unwrap_or(0) as usize;
                    let sig = *sig as FlatSigId;
                    let w = self.flat.signals[sig as usize].width;
                    self.mems[sig as usize].resize(n, Value::x(w.max(1)));
                    self.vals[sig as usize] = Value::from_u64(32, n as u64);
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
                            .map(|a| self.eval_expr(a, None).resize(w))
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
                if let Some(fi) = self.funcs.get(&fname).copied() {
                    let saved = self.cur_obj.replace(handle);
                    self.run_func(fi, &args[1..], None);
                    self.cur_obj.set(saved);
                }
            }
            // Void method call as a statement: `c.do(args);`
            m if m.starts_with("$callmethod::") => {
                let rest = &m["$callmethod::".len()..]; // `C::m`
                if let Some(fi) = self.funcs.get(rest).copied() {
                    let obj = args
                        .first()
                        .map(|a| self.eval_expr(a, None).to_u64().unwrap_or(u64::MAX))
                        .unwrap_or(u64::MAX);
                    let saved = self.cur_obj.replace(obj);
                    self.run_func(fi, &args[1..], None);
                    self.cur_obj.set(saved);
                }
            }
            "display" | "write" => {
                let text = self.fmt_args(&args);
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
                let text = self.fmt_args(&self.monitor[self.monitor.len() - 1].clone());
                self.emit(SimEvent::Message {
                    text: format!("{}\n", text),
                    time_fs: self.time,
                });
            }
            "assert" => {
                let ok = args
                    .first()
                    .map(|a| truthy(&self.eval_expr(a, None)))
                    .unwrap_or(true);
                if !ok {
                    let msg = args
                        .get(1)
                        .map(|a| string_of(&self.eval_expr(a, None)))
                        .unwrap_or_default();
                    let severity = args
                        .get(2)
                        .map(|a| string_of(&self.eval_expr(a, None)))
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
                    .map(|a| string_of(&self.eval_expr(a, None)))
                    .unwrap_or_default();
                let severity = args
                    .get(1)
                    .map(|a| string_of(&self.eval_expr(a, None)))
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
                    .map(|e| truthy(&self.eval_expr(e, None)))
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
                let text = self.fmt_args(&args);
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
                let text = self.fmt_args(&args);
                self.emit(SimEvent::Message {
                    text: format!("{}: {}\n", name, text),
                    time_fs: self.time,
                });
            }
            "finish" => {
                self.finished = true;
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
                    .map(|a| string_of(&self.eval_expr(a, None)))
                    .unwrap_or_default();
                match self.rand_solve(&spec, rand::DEFAULT_SOLVE_ATTEMPTS) {
                    Ok(bindings) => {
                        let mut i = 1;
                        while i + 1 < args.len() {
                            let name = string_of(&self.eval_expr(&args[i], None));
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
            _ => {
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

    fn fmt_args(&mut self, args: &[Expr]) -> String {
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
                            let v = self.eval_expr(&args[argi], None);
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
                let text = self.fmt_args(&args);
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
        for _ in 0..10_000 {
            self.delta_guard += 1;
            let mut changed = false;
            for ai in 0..self.flat.assigns.len() {
                let a = self.flat.assigns[ai].clone();
                let rhs = self.eval_expr(&a.rhs, None);
                let rhs = self.convert_for_target(&a.lhs, &a.rhs, rhs, None);
                if self.assign_to(&a.lhs, rhs, None) {
                    changed = true;
                }
            }
            any |= changed;
            if !changed {
                self.cur_proc = None;
                return any;
            }
        }
        self.cur_proc = None;
        self.emit(SimEvent::Message {
            text: format!("error: combinational loop detected at t={}", self.time),
            time_fs: self.time,
        });
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

    fn schedule_sensitive(&mut self, sig: FlatSigId, old: &Value, new: &Value) {
        let edge_of = |ek: &EdgeKind| match ek {
            EdgeKind::Pos => new.bit(0) == V1 && old.bit(0) != V1,
            EdgeKind::Neg => new.bit(0) == V0 && old.bit(0) != V0,
            EdgeKind::Any => true,
        };
        // Collect first (borrows self.proc_sens), then queue: a process runs at
        // most once per delta no matter how many of its signals changed.
        let mut to_activate: Vec<u32> = Vec::new();
        for (pid, sens) in self.proc_sens.iter().enumerate() {
            // Procs parked at an explicit `@(...)` are resumed via `waiting`.
            if self.waiting.iter().any(|(w, _)| *w == pid as u32) {
                continue;
            }
            if sens.iter().any(|(s, ek)| *s == sig && edge_of(ek)) {
                to_activate.push(pid as u32);
            }
        }
        for pid in to_activate {
            self.activate(pid);
        }
        // Processes parked at `@(...)` waits.
        let mut i = 0;
        while i < self.waiting.len() {
            let hit = self.waiting[i]
                .1
                .iter()
                .any(|(s, ek)| *s == sig && edge_of(ek));
            if hit {
                let (pid, _) = self.waiting.remove(i);
                self.activate(pid);
            } else {
                i += 1;
            }
        }
    }

    // ---- assignment targets -------------------------------------------------------------

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
                            self.mems[sig][a as usize] = v.resize(w);
                        } else {
                            self.mems[sig].resize(a as usize + 1, Value::x(w));
                            self.mems[sig][a as usize] = v.resize(w);
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
            Expr::Call { name, .. } => self
                .funcs
                .get(name)
                .map(|i| self.flat.functions[*i].ret_width)
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
            // Only arithmetic propagates realness; comparisons/logical ops
            // yield 1-bit results even on real operands.
            Expr::Bin { op, l, r } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    self.expr_real(l, locals) || self.expr_real(r, locals)
                }
                _ => false,
            },
            Expr::Un { op, e } => match op {
                UnOp::Neg | UnOp::Pos => self.expr_real(e, locals),
                _ => false,
            },
            Expr::Resize { e, .. } | Expr::Signed(e) => self.expr_real(e, locals),
            Expr::Cond { c, t, e } => {
                self.expr_real(c, locals) || self.expr_real(t, locals) || self.expr_real(e, locals)
            }
            _ => false,
        }
    }

    /// One operand of a real-typed binop as f64: real operands decode their
    /// bit pattern, integer operands convert their integer value.
    fn operand_f64(&self, e: &Expr, v: &Value, locals: Option<&Vec<Value>>) -> Option<f64> {
        if self.expr_real(e, locals) {
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
                if self.expr_real(l, locals) || self.expr_real(r, locals) {
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
                if self.expr_real(e, locals) {
                    return match (op, v.to_f64()) {
                        (UnOp::Neg, Some(x)) => Value::from_f64(-x),
                        (UnOp::Pos, Some(x)) => Value::from_f64(x),
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
                    Expr::Sig(s) if self.is_mem[*s as usize] => {
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
            "unsigned" | "signed" => return self.eval_expr(&args[0], locals),
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
                        // VHDL to_string of a vector: one char per bit.
                        if v.width() == 1 {
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
            if let Some(fi) = self.funcs.get(rest).copied() {
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
        if let Some(fi) = self.funcs.get(name).copied() {
            return self.run_func(fi, args, locals);
        }
        Value::x(32)
    }

    /// Execute function `fi` with the given argument expressions; returns
    /// slot 0 (the function-name slot, i.e. the return value). Shared by
    /// plain calls and class method calls.
    fn run_func(&self, fi: usize, args: &[Expr], locals: Option<&Vec<Value>>) -> Value {
        let f = &self.flat.functions[fi];
        let mut lvals: Vec<Value> = Vec::with_capacity(f.slots.len());
        lvals.push(Value::x(f.ret_width)); // slot 0 = function name (return value)
        for (i, a) in args.iter().enumerate() {
            let w = f.slots.get(i + 1).copied().unwrap_or(1);
            lvals.push(self.eval_expr(a, locals).resize(w));
        }
        while lvals.len() < f.slots.len() {
            lvals.push(Value::x(f.slots[lvals.len()]));
        }
        // Functions with delays are not supported — execute recursively.
        let mut runner = FnRunner {
            sim: self,
            locals: &mut lvals,
            returned: false,
        };
        runner.exec(&f.body);
        lvals[0].clone()
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
            return Value { bits };
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
                if let Expr::Sig(s) = lhs {
                    let i = *s as usize;
                    if i < self.locals.len() {
                        let w = self.locals[i].width();
                        self.locals[i] = v.resize(w.max(1));
                    }
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
            Stmt::SysCall { name, args } => {
                // Class field writes / randomize are &self-capable: run them
                // as calls. Other syscalls need &mut self (process context).
                if name.starts_with("$setf::") || name.starts_with("$randomize::") {
                    self.sim.eval_call(name, args, Some(self.locals));
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
    Value { bits }
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
