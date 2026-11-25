//! Live wave window (`--live [--port N]`) — a built-in `std::net` HTTP server
//! streaming the simulation over Server-Sent Events while it runs.
//!
//! - `GET /` serves a self-contained HTML page (canvas traces + transcript +
//!   Stop button) — no external assets, works offline.
//! - `GET /events` is the SSE stream; each client replays from t=0 with its own
//!   cursor, so late-joining viewers see the full history.
//! - `POST /stop` asks the simulator to finish (via the shared stop flag).
//!
//! The [`LiveSink`] observer feeds a [`LiveBus`]; the server threads only read
//! from it, so the kernel is never blocked by a slow viewer.

use super::{bits_msb, json_str, WaveSink};
use hdl_ir::elab::Flat;
use hdl_ir::SigKind;
use hdl_sim::events::SimEvent;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

struct Inner {
    /// SSE payloads (JSON, without the `data: ` prefix), in arrival order.
    events: Mutex<Vec<String>>,
    cond: Condvar,
    /// Signal table as JSON, written once when the sink is created.
    sigs: Mutex<String>,
    closed: AtomicBool,
    stop: Arc<AtomicBool>,
    /// Total events dropped by the history cap — reported to clients in
    /// bounded increments so they can label the reconstructed waves.
    dropped: std::sync::atomic::AtomicU64,
}

/// Shared bus between the simulator (producer) and the HTTP server (consumers).
#[derive(Clone)]
pub struct LiveBus {
    inner: Arc<Inner>,
}

impl LiveBus {
    pub fn new(flat: &Flat) -> LiveBus {
        let mut sigs = String::from("[");
        for (i, s) in flat.signals.iter().enumerate() {
            if i > 0 {
                sigs.push(',');
            }
            sigs.push_str(&format!(
                "{{\"id\":{},\"name\":{},\"w\":{},\"real\":{}}}",
                i,
                json_str(&s.name),
                s.width.max(1),
                s.kind == SigKind::Real
            ));
        }
        sigs.push(']');
        LiveBus {
            inner: Arc::new(Inner {
                events: Mutex::new(Vec::new()),
                cond: Condvar::new(),
                sigs: Mutex::new(sigs),
                closed: AtomicBool::new(false),
                stop: Arc::new(AtomicBool::new(false)),
                dropped: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    /// Set when the simulation has ended; SSE handlers drain and exit.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::Relaxed);
        self.inner.cond.notify_all();
    }

    /// Shared stop flag — wire the same `Arc<AtomicBool>` into
    /// `Simulator::set_stop_flag` so the Stop button ends the run.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.inner.stop.clone()
    }

    fn push(&self, payload: String) {
        const CAP: usize = 500_000;
        const TRUNC_EVERY: u64 = 100_000;
        let mut ev = self.inner.events.lock().unwrap();
        ev.push(payload);
        // Cap history so long sims do not grow memory without bound.
        if ev.len() > CAP {
            let drop = ev.len() - CAP;
            ev.drain(..drop);
            // Mark the trim so viewers know waves reconstructed after this
            // point begin at the first surviving event (a clock must not be
            // read as flat from t=0). Report at most one marker per 100k
            // dropped events — one per push would flood the stream.
            let prev = self
                .inner
                .dropped
                .fetch_add(drop as u64, std::sync::atomic::Ordering::Relaxed);
            let total = prev + drop as u64;
            if prev / TRUNC_EVERY != total / TRUNC_EVERY {
                ev.push(format!("{{\"e\":\"trunc\",\"n\":{total}}}"));
            }
        }
        drop(ev);
        self.inner.cond.notify_all();
    }
}

/// Observer that forwards sim events onto the live bus as JSON lines.
pub struct LiveSink {
    bus: LiveBus,
}

impl LiveSink {
    pub fn new(bus: LiveBus) -> LiveSink {
        LiveSink { bus }
    }
}

impl WaveSink for LiveSink {
    fn on_event(&mut self, ev: &SimEvent, flat: &Flat) {
        let line = match ev {
            SimEvent::TimeAdvance { time_fs } => format!("{{\"e\":\"t\",\"t\":{}}}", time_fs),
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                let real = flat
                    .signals
                    .get(*sig as usize)
                    .map(|s| s.kind == SigKind::Real)
                    .unwrap_or(false);
                let v = if real {
                    match new.to_f64() {
                        Some(f) => format!("{}", f),
                        None => "x".to_string(),
                    }
                } else {
                    bits_msb(new)
                };
                format!(
                    "{{\"e\":\"s\",\"t\":{},\"s\":{},\"v\":{}}}",
                    time_fs,
                    sig,
                    json_str(&v)
                )
            }
            SimEvent::Message { text, time_fs, .. } => format!(
                "{{\"e\":\"m\",\"t\":{},\"m\":{}}}",
                time_fs,
                json_str(text.trim_end())
            ),
            SimEvent::TestFailed { text, time_fs, .. } => format!(
                "{{\"e\":\"m\",\"t\":{},\"m\":{},\"fail\":true}}",
                time_fs,
                json_str(text.trim_end())
            ),
            SimEvent::Finish { time_fs } => format!("{{\"e\":\"f\",\"t\":{}}}", time_fs),
            SimEvent::Stop { time_fs } => format!("{{\"e\":\"x\",\"t\":{}}}", time_fs),
            SimEvent::ProcActive { .. } => return,
            SimEvent::StmtExec { .. } => return, // coverage-only, not a wave event
            SimEvent::Driver { .. } => return,   // surfaced via debug --driver
        };
        self.bus.push(line);
    }
}

/// Spawn the HTTP/SSE server on `127.0.0.1:port`. Returns the bound port and a
/// handle; call `bus.close()` when the simulation ends.
pub fn spawn_live_server(
    bus: LiveBus,
    port: u16,
) -> std::io::Result<(u16, std::thread::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let bound = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let bus2 = bus.clone();
    let handle = std::thread::spawn(move || serve(listener, bus2));
    Ok((bound, handle))
}

fn serve(listener: TcpListener, bus: LiveBus) {
    // Accept until the process exits; `bus.close()` only ends the SSE
    // streams (they drain remaining events), so the final waves stay
    // viewable in the browser after the simulation has finished.
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let bus = bus.clone();
                std::thread::spawn(move || {
                    let _ = handle_conn(stream, bus);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 16 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().next()?.to_string();
    Some(line)
}

fn respond(mut stream: TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        content_type,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn handle_conn(mut stream: TcpStream, bus: LiveBus) -> std::io::Result<()> {
    let Some(req) = read_request(&mut stream) else {
        return Ok(());
    };
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");
    match (method, path) {
        ("GET", "/") => {
            respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                PAGE.as_bytes(),
            );
            Ok(())
        }
        ("GET", "/signals") => {
            let sigs = bus.inner.sigs.lock().unwrap().clone();
            respond(stream, "200 OK", "application/json", sigs.as_bytes());
            Ok(())
        }
        ("GET", "/events") => sse_stream(stream, bus),
        ("POST", "/stop") => {
            bus.inner.stop.store(true, Ordering::Relaxed);
            respond(stream, "200 OK", "text/plain", b"stopped");
            Ok(())
        }
        _ => {
            respond(stream, "404 Not Found", "text/plain", b"not found");
            Ok(())
        }
    }
}

/// SSE stream: replay history from the client's cursor, then follow live.
fn sse_stream(mut stream: TcpStream, bus: LiveBus) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    stream.write_all(head.as_bytes())?;
    let mut cursor = 0usize;
    loop {
        let (batch, closed) = {
            let ev = bus.inner.events.lock().unwrap();
            // Re-base cursor if history was trimmed.
            let base = ev.len().saturating_sub(500_000);
            if cursor < base {
                cursor = base;
            }
            let batch: Vec<String> = ev[cursor..].to_vec();
            let new_cursor = ev.len();
            let closed = bus.inner.closed.load(Ordering::Relaxed);
            drop(ev);
            cursor = new_cursor;
            (batch, closed)
        };
        for line in &batch {
            stream.write_all(format!("data: {}\n\n", line).as_bytes())?;
        }
        stream.flush()?;
        if closed && batch.is_empty() {
            return Ok(());
        }
        // Wait for more events (or shutdown) with a timeout so we stay responsive.
        let ev = bus.inner.events.lock().unwrap();
        let _ = bus
            .inner
            .cond
            .wait_timeout(ev, Duration::from_millis(250))
            .unwrap();
    }
}

/// The live viewer page.
const PAGE: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>hdlsim live</title>
<style>
  body { margin:0; background:#14171c; color:#d7dde6; font:13px/1.4 Menlo,Consolas,monospace; }
  header { display:flex; gap:8px; align-items:center; padding:6px 12px; background:#1c2129; border-bottom:1px solid #2b323d; }
  header b { color:#4da3ff; }
  header span { color:#8a94a3; }
  button { background:#14171c; border:1px solid #2b323d; color:#d7dde6; padding:3px 10px; border-radius:4px; cursor:pointer; }
  button:hover { border-color:#4da3ff; }
  /* One scroller for names AND waves so they always stay in sync. */
  #wrap { height:calc(100vh - 120px); }
  #scroller { height:100%; overflow-y:auto; }
  #inner { display:flex; align-items:flex-start; }
  #names { width:280px; flex:none; background:#1c2129; border-right:1px solid #2b323d; }
  #names .row { height:29px; line-height:26px; padding:0 8px; white-space:nowrap; overflow:hidden; box-sizing:border-box; }
  #names .v { color:#4da3ff; display:inline-block; min-width:90px; }
  canvas { flex:1; min-width:0; display:block; background:#14171c; cursor:grab; }
  #log { height:80px; overflow-y:auto; border-top:1px solid #2b323d; background:#1c2129; padding:4px 12px; white-space:pre-wrap; color:#8a94a3; }
</style>
</head>
<body>
<header><b>hdlsim</b> live <span id="time"></span> <span id="note"></span>
  <button onclick="zoomCenter(0.8)">+</button>
  <button onclick="zoomCenter(1.25)">&minus;</button>
  <button onclick="fit()">fit</button>
  <button onclick="follow()">follow</button>
  <button onclick="stop()">stop simulation</button>
</header>
<div id="wrap"><div id="scroller"><div id="inner"><div id="names"></div><canvas id="cv"></canvas></div></div></div>
<div id="log"></div>
<script>
let sigs = [], bySig = [], tmax = 0, t0 = 0, span = 1, latest = 0, pin = true, fitted = false;
const PADX = 10, PADY = 6, ROW = 24, GAP = 5, PITCH = ROW + GAP;
fetch('/signals').then(r => r.json()).then(j => {
  sigs = j; bySig = j.map(() => []);
  const names = document.getElementById('names');
  let html = '';
  j.forEach((s, i) => { html += '<div class="row"><span class="v" id="v'+i+'"></span>'
    + s.w + ' ' + s.name.replace(/</g,'&lt;') + '</div>'; });
  names.innerHTML = html;
  document.getElementById('cv').height = PADY*2 + j.length*PITCH;
  draw();
});
const es = new EventSource('/events');
let dirty = false;
function schedule() { if (!dirty) { dirty = true; requestAnimationFrame(() => { dirty = false; draw(); }); } }
es.onmessage = e => {
  const d = JSON.parse(e.data);
  if (d.e === 't') {
    latest = Math.max(latest, d.t); if (d.t > tmax) tmax = d.t;
    document.getElementById('time').textContent = fmt(d.t);
    // Auto-fit once the first real timestamp arrives: a 1 fs default
    // window would show nothing until the user pressed fit.
    if (!fitted && d.t > 0) { fitted = true; span = Math.max(d.t, 1); t0 = 0; }
  }
  else if (d.e === 's') { bySig[d.s].push([d.t, d.v]); if (d.t > tmax) tmax = d.t; if (d.t > latest) latest = d.t; }
  else if (d.e === 'm') { logline('@' + fmt(d.t) + '  ' + d.m); }
  else if (d.e === 'trunc') {
    logline('note: history truncated — ' + d.n + ' early event(s) dropped; waves before that are incomplete');
    document.getElementById('note').textContent = 'history truncated';
  }
  else if (d.e === 'f' || d.e === 'x') { document.getElementById('time').textContent = fmt(d.t) + (d.e==='f' ? ' (finished)' : ' (stopped)'); }
  schedule();
};
function logline(s) { const l = document.getElementById('log'); l.textContent += s + '\n'; l.scrollTop = l.scrollHeight; }
function fmt(t) { if (t>=1e15) return (t/1e15).toFixed(2)+'s'; if (t>=1e12) return (t/1e12).toFixed(2)+'ms';
  if (t>=1e9) return (t/1e9).toFixed(2)+'us'; if (t>=1e6) return (t/1e6).toFixed(2)+'ns';
  if (t>=1e3) return (t/1e3).toFixed(2)+'ps'; return t+'fs'; }
function hex(v) { if (/[xz]/.test(v)) return /^[xz]+$/.test(v) ? v[0] : 'x'; return '0x' + BigInt('0b'+v).toString(16); }
function valAt(s, t) { const evs = bySig[s]; let lo=0, hi=evs.length-1, ans=evs.length?evs[0][1]:'x';
  while (lo<=hi){const m=(lo+hi)>>1; if(evs[m][0]<=t){ans=evs[m][1];lo=m+1;}else hi=m-1;} return ans; }
const cv = document.getElementById('cv'), ctx = cv.getContext('2d');
const scroller = document.getElementById('scroller');
function clampView() {
  const smax = Math.max(tmax, 1);
  span = Math.max(1, Math.min(span, smax));
  if (pin && tmax) t0 = Math.max(0, tmax - span);
  t0 = Math.max(0, Math.min(t0, Math.max(0, tmax - span)));
}
function draw() {
  const W = cv.clientWidth, H = scroller.clientHeight;
  if (!W) return;
  if (cv.width !== W) { cv.width = W; }
  clampView();
  ctx.fillStyle = '#14171c'; ctx.fillRect(0, 0, W, cv.height);
  if (!tmax) return;
  const x = t => PADX + (t - t0) / span * (W - 2*PADX);
  // Only visible rows: with tens of thousands of signals, a full-table
  // sweep (and a value lookup per signal) per frame swamps the page.
  const st = scroller.scrollTop;
  const first = Math.max(0, Math.floor((st - PADY) / PITCH));
  const last = Math.min(sigs.length, Math.ceil((st + H) / PITCH) + 1);
  for (let i = first; i < last; i++) {
    const s = sigs[i], evs = bySig[i], y = PADY + i*PITCH;
    if (s.real) drawReal(i, evs, y, x);
    else if (s.w === 1) drawBit(evs, y, x);
    else drawBus(i, evs, y, x);
    const el = document.getElementById('v'+i);
    if (el) el.textContent = s.real ? valAt(i, latest)
      : (s.w === 1 ? valAt(i, latest).slice(-1) : hex(valAt(i, latest)));
  }
}
function drawReal(i, evs, y, x) {
  let lo = Infinity, hi = -Infinity;
  for (const [t, v] of evs) { const f = parseFloat(v); if (!isNaN(f)) { if (f<lo) lo=f; if (f>hi) hi=f; } }
  if (!isFinite(lo)) { lo = 0; hi = 1; }
  if (hi - lo < 1e-12) { const m=(hi+lo)/2; lo=m-0.5; hi=m+0.5; }
  const yOf = f => y + (ROW-5) - (f-lo)/(hi-lo)*(ROW-5);
  ctx.strokeStyle = '#5fd38a'; ctx.lineWidth = 1.4; ctx.beginPath();
  let started = false;
  for (const [t, v] of evs) {
    const f = parseFloat(v); if (isNaN(f)) continue;
    if (t < t0) continue;
    if (t > t0 + span) break;
    const xx = x(t);
    if (!started) { ctx.moveTo(xx, yOf(f)); started = true; } else ctx.lineTo(xx, yOf(f));
  }
  if (started) ctx.lineTo(x(t0+span), yOf(parseFloat(valAt(i, t0+span))));
  ctx.stroke();
}
function drawBit(evs, y, x) {
  const HI = y, LO = y + ROW - 5, XB = y + ROW - 9;
  // Level segments within the window; events at the same timestamp
  // (delta-cycle transitions) become separate segments.
  const segs = [];
  let segT = null, segB = null;
  for (const [t, v] of evs) {
    if (t > t0 + span) break;
    const b = v[v.length-1];
    if (segB === null) { segT = Math.max(t, t0); segB = b; }
    else if (b !== segB) { segs.push([segT, t, segB]); segT = t; segB = b; }
  }
  if (segB !== null) segs.push([segT, t0 + span, segB]);
  // X/Z bands: a distinct marker, never a valid low level.
  ctx.fillStyle = '#b06ab3';
  for (const [ta, tb, b] of segs) if (b !== '0' && b !== '1') {
    let x0 = x(ta), x1 = x(tb); if (x1 - x0 < 2) x1 = x0 + 2;
    ctx.fillRect(x0, XB, x1 - x0, 4);
  }
  // Level lines; a zero-width segment widens to 1px so a pulse that fits
  // entirely inside one timestamp stays visible when zoomed out.
  ctx.strokeStyle = '#4da3ff'; ctx.lineWidth = 1.5; ctx.beginPath();
  let plv = null;
  for (const [ta, tb, b] of segs) {
    if (b !== '0' && b !== '1') continue;
    const lv = b === '1' ? HI : LO;
    let x0 = x(ta), x1 = x(tb);
    if (x1 - x0 < 1) x1 = x0 + 1;
    if (plv === null) ctx.moveTo(x0, lv);
    else { ctx.lineTo(x0, plv); ctx.lineTo(x0, lv); }
    ctx.lineTo(x1, lv);
    plv = lv;
  }
  ctx.stroke();
}
function drawBus(i, evs, y, x) {
  let segT = t0, segV = valAt(i, t0);
  const flush = tEnd => { const x0=x(segT), x1=x(tEnd); if (x1-x0<1) return;
    ctx.fillStyle='#2a3140'; ctx.fillRect(x0, y, x1-x0, ROW-5);
    ctx.strokeStyle='#c9a24d'; ctx.strokeRect(x0, y, x1-x0, ROW-5);
    ctx.fillStyle = /[xz]/.test(segV) ? '#b06ab3' : '#c9a24d';
    ctx.font = '10px monospace'; ctx.textAlign='center'; ctx.fillText(hex(segV), (x0+x1)/2, y+ROW/2+3); };
  for (const [t, v] of evs) { if (t <= t0) { segV = v; continue; } if (t >= t0+span) break; if (v !== segV) { flush(t); segT = t; segV = v; } }
  flush(t0 + span);
}
function zoomCenter(f) {
  const W = cv.clientWidth, af = 0.5;
  const at = t0 + af*span;
  span = Math.max(1, Math.min(Math.max(tmax, 1), span*f));
  t0 = at - af*span;
  pin = !!tmax && t0 + span >= tmax;
  draw();
}
function fit() { span = Math.max(tmax, 1); t0 = 0; pin = true; draw(); }
function follow() { pin = !pin; if (pin) t0 = Math.max(0, tmax - span); draw(); }
function stop() { fetch('/stop', { method: 'POST' }); }
// Wheel zooms around the cursor position; drag pans. Pinning to the live
// edge only resumes automatically when the view is already at the end.
cv.onwheel = e => {
  e.preventDefault();
  const W = cv.clientWidth, af = (e.offsetX - PADX) / (W - 2*PADX);
  const at = t0 + af*span;
  span = Math.max(1, Math.min(Math.max(tmax, 1), span * (e.deltaY > 0 ? 1.25 : 0.8)));
  t0 = at - af*span;
  pin = !!tmax && t0 + span >= tmax;
  draw();
};
let drag = null;
cv.onmousedown = e => { drag = [e.clientX, t0]; };
window.onmousemove = e => {
  if (!drag) return;
  const W = cv.clientWidth;
  t0 = drag[1] - (e.clientX - drag[0]) / (W - 2*PADX) * span;
  pin = false;
  draw();
};
window.onmouseup = () => { drag = null; };
setInterval(draw, 200);
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::elab::{FlatSig, Scope};
    use hdl_ir::ir::SigKind;
    use hdl_ir::Value;

    fn flat1() -> Flat {
        Flat {
            signals: vec![FlatSig {
                name: "tb.clk".into(),
                width: 1,
                kind: SigKind::Wire,
                init: Some(Value::from_u64(1, 0)),
                mem: None,
                scope: 0,
                signed: false,
                real_ranges: Vec::new(),
            }],
            procs: vec![],
            assigns: vec![],
            functions: vec![],
            scopes: vec![Scope {
                name: "tb".into(),
                parent: None,
            }],
            top_scope: 0,
        }
    }

    #[test]
    fn live_sink_emits_json_events() {
        let flat = flat1();
        let bus = LiveBus::new(&flat);
        let mut sink = LiveSink::new(bus.clone());
        sink.on_event(
            &SimEvent::SignalChange {
                sig: 0,
                old: Value::from_u64(1, 0),
                new: Value::from_u64(1, 1),
                time_fs: 1_000_000,
            },
            &flat,
        );
        sink.on_event(
            &SimEvent::Message {
                text: "hi\n".into(),
                time_fs: 2_000_000,
                span: None,
            },
            &flat,
        );
        let ev = bus.inner.events.lock().unwrap();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0], "{\"e\":\"s\",\"t\":1000000,\"s\":0,\"v\":\"1\"}");
        assert_eq!(ev[1], "{\"e\":\"m\",\"t\":2000000,\"m\":\"hi\"}");
    }

    #[test]
    fn live_bus_marks_trimmed_history() {
        let flat = flat1();
        let bus = LiveBus::new(&flat);
        // Cross the history cap by >100k events: the stream must carry a
        // trunc marker so viewers do not read the surviving waves as
        // "constant from t=0" (clocks would show a wrong phase/duty).
        for i in 0..600_005u64 {
            bus.push(format!("{{\"e\":\"t\",\"t\":{i}}}"));
        }
        let ev = bus.inner.events.lock().unwrap();
        assert_eq!(ev.len(), 500_000);
        assert!(!ev[0].contains("\"e\":\"trunc\""));
        // The marker is not necessarily last (later pushes append after it).
        assert!(
            ev.iter().any(|l| l.contains("\"e\":\"trunc\"")),
            "no trunc marker in {} events",
            ev.len()
        );
    }

    #[test]
    fn live_page_has_zoom_pan_and_scroll_sync() {
        for marker in [
            "zoomCenter(",
            "cv.onmousedown",
            "id=\"scroller\"",
            "'trunc'",
        ] {
            assert!(PAGE.contains(marker), "page missing {marker}");
        }
    }

    #[test]
    fn live_server_serves_page_and_events() {
        use std::io::{Read, Write};
        let flat = flat1();
        let bus = LiveBus::new(&flat);
        let (port, _handle) = spawn_live_server(bus.clone(), 0).unwrap();
        // push one event before connecting so /events has history
        bus.push("{\"e\":\"t\",\"t\":42}".to_string());
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("200 OK"), "{}", &text[..100]);
        assert!(text.contains("hdlsim live"), "page missing");

        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET /events HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        s.set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        // SSE is a stream: keep reading until the history line shows up.
        let mut text = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !text.contains("data: ") && std::time::Instant::now() < deadline {
            let mut buf = [0u8; 4096];
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => {}
            }
        }
        assert!(
            text.contains("text/event-stream"),
            "{}",
            &text[..text.len().min(200)]
        );
        assert!(
            text.contains("data: {\"e\":\"t\",\"t\":42}"),
            "history missing: {}",
            &text[..text.len().min(300)]
        );
        bus.close();
    }
}
