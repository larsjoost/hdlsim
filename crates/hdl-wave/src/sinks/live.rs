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
                "{{\"id\":{},\"name\":{},\"w\":{}}}",
                i,
                json_str(&s.name),
                s.width.max(1)
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
        let mut ev = self.inner.events.lock().unwrap();
        ev.push(payload);
        // Cap history so long sims do not grow memory without bound.
        if ev.len() > 500_000 {
            let drop = ev.len() - 500_000;
            ev.drain(..drop);
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
    fn on_event(&mut self, ev: &SimEvent, _flat: &Flat) {
        let line = match ev {
            SimEvent::TimeAdvance { time_fs } => format!("{{\"e\":\"t\",\"t\":{}}}", time_fs),
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => format!(
                "{{\"e\":\"s\",\"t\":{},\"s\":{},\"v\":{}}}",
                time_fs,
                sig,
                json_str(&bits_msb(new))
            ),
            SimEvent::Message { text, time_fs } => format!(
                "{{\"e\":\"m\",\"t\":{},\"m\":{}}}",
                time_fs,
                json_str(text.trim_end())
            ),
            SimEvent::TestFailed { text, time_fs } => format!(
                "{{\"e\":\"m\",\"t\":{},\"m\":{},\"fail\":true}}",
                time_fs,
                json_str(text.trim_end())
            ),
            SimEvent::Finish { time_fs } => format!("{{\"e\":\"f\",\"t\":{}}}", time_fs),
            SimEvent::Stop { time_fs } => format!("{{\"e\":\"x\",\"t\":{}}}", time_fs),
            SimEvent::ProcActive { .. } => return,
            SimEvent::StmtExec { .. } => return, // coverage-only, not a wave event
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
  header { display:flex; gap:10px; align-items:center; padding:6px 12px; background:#1c2129; border-bottom:1px solid #2b323d; }
  header b { color:#4da3ff; }
  header span { color:#8a94a3; }
  button { background:#14171c; border:1px solid #2b323d; color:#d7dde6; padding:3px 10px; border-radius:4px; cursor:pointer; }
  button:hover { border-color:#4da3ff; }
  #wrap { display:flex; height:calc(100vh - 120px); }
  #list { width:280px; overflow-y:auto; background:#1c2129; border-right:1px solid #2b323d; padding:4px 0; }
  #list div { padding:2px 8px; white-space:nowrap; }
  #list .v { color:#4da3ff; display:inline-block; min-width:90px; }
  canvas { flex:1; background:#14171c; }
  #log { height:80px; overflow-y:auto; border-top:1px solid #2b323d; background:#1c2129; padding:4px 12px; white-space:pre-wrap; color:#8a94a3; }
</style>
</head>
<body>
<header><b>hdlsim</b> live <span id="time"></span>
  <button onclick="fit()">fit</button>
  <button onclick="stop()">stop simulation</button>
</header>
<div id="wrap"><div id="list"></div><canvas id="cv"></canvas></div>
<div id="log"></div>
<script>
let sigs = [], bySig = [], tmax = 0, t0 = 0, span = 1, latest = 0;
fetch('/signals').then(r => r.json()).then(j => { sigs = j; bySig = j.map(() => []);
  const list = document.getElementById('list');
  j.forEach((s, i) => { const d = document.createElement('div');
    d.innerHTML = '<span class="v" id="v'+i+'"></span>' + s.w + ' ' + s.name.replace(/</g,'&lt;'); list.appendChild(d); });
});
const es = new EventSource('/events');
es.onmessage = e => {
  const d = JSON.parse(e.data);
  if (d.e === 't') { latest = d.t; if (d.t > tmax) tmax = d.t; document.getElementById('time').textContent = fmt(d.t); }
  else if (d.e === 's') { bySig[d.s].push([d.t, d.v]); if (d.t > tmax) tmax = d.t; latest = Math.max(latest, d.t); }
  else if (d.e === 'm') { const l = document.getElementById('log'); l.textContent += '@' + fmt(d.t) + '  ' + d.m + '\n'; l.scrollTop = l.scrollHeight; }
  else if (d.e === 'f' || d.e === 'x') { document.getElementById('time').textContent = fmt(d.t) + (d.e==='f' ? ' (finished)' : ' (stopped)'); }
  draw();
};
function fmt(t) { if (t>=1e15) return (t/1e15).toFixed(2)+'s'; if (t>=1e12) return (t/1e12).toFixed(2)+'ms';
  if (t>=1e9) return (t/1e9).toFixed(2)+'us'; if (t>=1e6) return (t/1e6).toFixed(2)+'ns';
  if (t>=1e3) return (t/1e3).toFixed(2)+'ps'; return t+'fs'; }
function hex(v) { if (/[xz]/.test(v)) return /^[xz]+$/.test(v) ? v[0] : 'x'; return '0x' + BigInt('0b'+v).toString(16); }
function valAt(s, t) { const evs = bySig[s]; let lo=0, hi=evs.length-1, ans=evs.length?evs[0][1]:'x';
  while (lo<=hi){const m=(lo+hi)>>1; if(evs[m][0]<=t){ans=evs[m][1];lo=m+1;}else hi=m-1;} return ans; }
const cv = document.getElementById('cv'), ctx = cv.getContext('2d');
function draw() {
  const W = cv.clientWidth, H = cv.clientHeight;
  if (cv.width !== W) { cv.width = W; cv.height = H; }
  ctx.fillStyle = '#14171c'; ctx.fillRect(0, 0, W, H);
  if (!tmax) return;
  if (span > tmax) span = tmax;
  if (t0 + span > tmax && span < tmax) t0 = tmax - span;   // auto-follow
  const x = t => 10 + (t - t0) / span * (W - 20);
  const ROW = 24, GAP = 5;
  sigs.forEach((s, i) => {
    const y = 6 + i * (ROW + GAP); if (y > H) return;
    const evs = bySig[i];
    if (s.w === 1) {
      ctx.strokeStyle = '#4da3ff'; ctx.lineWidth = 1.5; ctx.beginPath();
      let cur = null;
      for (const [t, v] of evs) {
        if (t < t0) { cur = v; continue; }
        if (t > t0 + span) break;
        const xx = x(t), bit = v[v.length-1];
        if (cur === null) ctx.moveTo(xx, bit==='1' ? y : y+ROW-5);
        else if (bit !== cur) { ctx.lineTo(xx, cur==='1'?y:y+ROW-5); ctx.lineTo(xx, bit==='1'?y:y+ROW-5); }
        cur = bit;
      }
      if (cur !== null) ctx.lineTo(x(t0+span), cur==='1'?y:y+ROW-5);
      ctx.stroke();
    } else {
      let segT = t0, segV = valAt(i, t0);
      ctx.fillStyle = '#2a3140'; ctx.strokeStyle = '#c9a24d'; ctx.fillStyle = '#c9a24d';
      const flush = tEnd => { const x0=x(segT), x1=x(tEnd); if (x1-x0<1) return;
        ctx.fillStyle='#2a3140'; ctx.fillRect(x0, y, x1-x0, ROW-5);
        ctx.strokeStyle='#c9a24d'; ctx.strokeRect(x0, y, x1-x0, ROW-5);
        ctx.fillStyle = /[xz]/.test(segV) ? '#b06ab3' : '#c9a24d';
        ctx.font = '10px monospace'; ctx.textAlign='center'; ctx.fillText(hex(segV), (x0+x1)/2, y+ROW/2+3); };
      for (const [t, v] of evs) { if (t <= t0) { segV = v; continue; } if (t >= t0+span) break; if (v !== segV) { flush(t); segT = t; segV = v; } }
      flush(t0 + span);
    }
    const el = document.getElementById('v'+i);
    if (el) el.textContent = s.w === 1 ? valAt(i, latest).slice(-1) : hex(valAt(i, latest));
  });
}
function fit() { t0 = 0; span = tmax || 1; draw(); }
function stop() { fetch('/stop', { method: 'POST' }); }
cv.onwheel = e => { e.preventDefault(); const f = e.deltaY > 0 ? 1.25 : 0.8;
  span = Math.max(1, Math.min(tmax || 1, span * f)); draw(); };
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
            },
            &flat,
        );
        let ev = bus.inner.events.lock().unwrap();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0], "{\"e\":\"s\",\"t\":1000000,\"s\":0,\"v\":\"1\"}");
        assert_eq!(ev[1], "{\"e\":\"m\",\"t\":2000000,\"m\":\"hi\"}");
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
