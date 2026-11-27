//! Self-contained interactive HTML/SVG wave viewer (`--wave out.html`).
//!
//! The sink records the `SimEvent` stream compactly; on `finish` it writes one
//! `.html` file with the trace data embedded as JSON plus inline CSS/JS — no
//! network, no external assets, works offline in any browser. Features:
//! signal search + visibility toggles, digital traces for 1-bit signals, hex
//! buses, analog polylines for `real` signals, time axis, wheel zoom + drag pan,
//! cursor with per-signal readout, and a transcript pane for `$display`/`report`
//! messages.

use super::{bits_msb, json_str, WaveSink};
use hdl_ir::elab::Flat;
use hdl_ir::value::Value;
use hdl_ir::SigKind;
use hdl_sim::events::SimEvent;
use std::io::Write;
use std::path::PathBuf;

/// One recorded value change: (time_fs, signal id, bits MSB-first).
struct Rec {
    t: u64,
    s: u32,
    v: String,
}

pub struct HtmlSink {
    file: Option<std::fs::File>,
    path: PathBuf,
    /// (name, width, is_real) per signal, captured from the flat design.
    sigs: Vec<(String, usize, bool)>,
    /// Initial values at t=0 plus every recorded change.
    recs: Vec<Rec>,
    /// Transcript lines: (time_fs, text).
    msgs: Vec<(u64, String)>,
    time: u64,
    max_time: u64,
}

impl HtmlSink {
    pub fn new(file: std::fs::File, path: PathBuf, flat: &Flat) -> HtmlSink {
        let sigs: Vec<(String, usize, bool)> = flat
            .signals
            .iter()
            .map(|s| (s.name.clone(), s.width.max(1), s.kind == SigKind::Real))
            .collect();
        let mut sink = HtmlSink {
            file: Some(file),
            path,
            sigs,
            recs: Vec::new(),
            msgs: Vec::new(),
            time: 0,
            max_time: 0,
        };
        // Seed t=0 values so even never-toggled signals render.
        for (i, s) in flat.signals.iter().enumerate() {
            let v = s.init.clone().unwrap_or_else(|| Value::x(s.width.max(1)));
            sink.recs.push(Rec {
                t: 0,
                s: i as u32,
                v: fmt_rec(&v, s.kind == SigKind::Real),
            });
        }
        sink
    }

    fn write_html(&mut self, flat: &Flat) {
        let mut data = String::from("{\"sigs\":[");
        for (i, (name, width, real)) in self.sigs.iter().enumerate() {
            if i > 0 {
                data.push(',');
            }
            data.push_str(&format!(
                "{{\"id\":{},\"name\":{},\"w\":{},\"real\":{}}}",
                i,
                json_str(name),
                width,
                real
            ));
        }
        data.push_str("],\"events\":[");
        for (i, r) in self.recs.iter().enumerate() {
            if i > 0 {
                data.push(',');
            }
            data.push_str(&format!("[{},{},{}]", r.t, r.s, json_str(&r.v)));
        }
        data.push_str("],\"msgs\":[");
        for (i, (t, m)) in self.msgs.iter().enumerate() {
            if i > 0 {
                data.push(',');
            }
            data.push_str(&format!("[{},{}]", t, json_str(m)));
        }
        data.push_str(&format!("],\"tmax\":{}}}", self.max_time.max(1)));

        let html = PAGE.replace("/*__DATA__*/", &format!("const DATA = {};", data));
        let file = self.file.take();
        if let Some(mut f) = file {
            if let Err(e) = f.write_all(html.as_bytes()) {
                log::warn!("hdlsim: cannot write {}: {}", self.path.display(), e);
            }
        }
        let _ = flat;
    }
}

impl WaveSink for HtmlSink {
    fn on_event(&mut self, ev: &SimEvent, flat: &Flat) {
        match ev {
            SimEvent::TimeAdvance { time_fs } => {
                self.time = *time_fs;
                if *time_fs > self.max_time {
                    self.max_time = *time_fs;
                }
            }
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                let real = flat
                    .signals
                    .get(*sig as usize)
                    .map(|s| s.kind == SigKind::Real)
                    .unwrap_or(false);
                self.recs.push(Rec {
                    t: *time_fs,
                    s: *sig,
                    v: fmt_rec(new, real),
                });
                if *time_fs > self.max_time {
                    self.max_time = *time_fs;
                }
            }
            SimEvent::Message { text, time_fs, .. } => {
                self.msgs.push((*time_fs, text.trim_end().to_string()));
            }
            _ => {}
        }
    }

    fn finish(&mut self, flat: &Flat) {
        self.write_html(flat);
    }
}

/// Recorded value as a string: bit string for digital signals, decimal for
/// real signals (the JS side parses it with parseFloat when sig.real).
fn fmt_rec(v: &Value, real: bool) -> String {
    if real {
        match v.to_f64() {
            Some(f) => format!("{}", f),
            None => "x".to_string(),
        }
    } else {
        bits_msb(v)
    }
}

/// The viewer page: data placeholder `/*__DATA__*/` is replaced with the JSON.
const PAGE: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>hdlsim waves</title>
<style>
  :root { --bg:#14171c; --panel:#1c2129; --line:#2b323d; --fg:#d7dde6; --dim:#8a94a3;
          --acc:#4da3ff; --bus:#c9a24d; --x:#b06ab3; }
  * { box-sizing:border-box; }
  body { margin:0; background:var(--bg); color:var(--fg); font:13px/1.4 "SF Mono",Menlo,Consolas,monospace; }
  header { display:flex; gap:12px; align-items:center; padding:8px 12px; background:var(--panel);
            border-bottom:1px solid var(--line); position:sticky; top:0; z-index:5; }
  header b { color:var(--acc); }
  header input { background:var(--bg); border:1px solid var(--line); color:var(--fg);
                 padding:4px 8px; border-radius:4px; width:260px; }
  header button { background:var(--bg); border:1px solid var(--line); color:var(--fg);
                  padding:4px 10px; border-radius:4px; cursor:pointer; }
  header button:hover { border-color:var(--acc); }
  #tmax { color:var(--dim); }
  main { display:flex; height:calc(100vh - 90px); }
  #list { width:300px; min-width:300px; overflow-y:auto; background:var(--panel);
          border-right:1px solid var(--line); }
  #list .row { display:flex; gap:6px; padding:3px 8px; cursor:pointer; white-space:nowrap; }
  #list .row:hover { background:#232a35; }
  #list .row.hidden { display:none; }
  #list .row .w { color:var(--dim); min-width:34px; text-align:right; }
  #list .row .v { color:var(--acc); min-width:90px; }
  #list .row input { margin:0; }
  #list .head { position:sticky; top:0; background:var(--panel); color:var(--dim);
                padding:4px 8px; border-bottom:1px solid var(--line); }
  #pane { flex:1; overflow:hidden; position:relative; }
  svg { width:100%; height:100%; display:block; }
  #transcript { height:88px; overflow-y:auto; border-top:1px solid var(--line);
                background:var(--panel); padding:4px 12px; white-space:pre-wrap; color:var(--dim); }
  .axis { stroke:var(--line); stroke-width:1; }
  .tlab { fill:var(--dim); font-size:10px; }
  .sig1 { stroke:var(--acc); stroke-width:1.6; fill:none; }
  .analog { stroke:#5fd38a; stroke-width:1.4; fill:none; }
  .busband { fill:#2a3140; stroke:var(--bus); stroke-width:1; }
  .buslab { fill:var(--bus); font-size:10px; text-anchor:middle; }
  .xlab { fill:var(--x); font-size:10px; text-anchor:middle; }
  .cursor { stroke:#e06c75; stroke-width:1; stroke-dasharray:3 3; }
</style>
</head>
<body>
<header>
  <b>hdlsim</b> waves
  <input id="q" placeholder="search signals…" oninput="filter()">
  <button onclick="fit()">fit</button>
  <button onclick="toggleAll(true)">all</button>
  <button onclick="toggleAll(false)">none</button>
  <span id="tmax"></span>
</header>
<main>
  <div id="list"><div class="head">signal <span style="float:right">value @ cursor</span></div></div>
  <div id="pane"><svg id="cv" preserveAspectRatio="none"></svg></div>
</main>
<div id="transcript"></div>
<script>
/*__DATA__*/
const ROW_H = 26, GAP = 6, LEFT = 0;
let sigs = DATA.sigs, events = DATA.events, msgs = DATA.msgs, tmax = DATA.tmax;
let show = sigs.map(() => true);
let view = { t0: 0, span: tmax || 1 };   // visible time window
let cursorT = null;
// index events per signal, sorted by t (they arrive in order)
let bySig = sigs.map(() => []);
for (const [t, s, v] of events) bySig[s].push([t, v]);

const listEl = document.getElementById('list');
const svg = document.getElementById('cv');
const pane = document.getElementById('pane');
document.getElementById('tmax').textContent = 't = 0 … ' + fmtT(tmax);

for (const m of msgs) {
  const d = document.createElement('div');
  d.textContent = '@' + fmtT(m[0]) + '  ' + m[1];
  document.getElementById('transcript').appendChild(d);
}

function fmtT(t) {
  if (t >= 1e15) return (t/1e15).toFixed(2) + 's';
  if (t >= 1e12) return (t/1e12).toFixed(2) + 'ms';
  if (t >= 1e9)  return (t/1e9).toFixed(2) + 'us';
  if (t >= 1e6)  return (t/1e6).toFixed(2) + 'ns';
  if (t >= 1e3)  return (t/1e3).toFixed(2) + 'ps';
  return t + 'fs';
}
function jsonBitsToHex(v) {  // v: MSB-first bit string
  if (/[xz]/.test(v)) return /^[xz]+$/.test(v) ? v[0].repeat(Math.min(4, v.length)) : 'x';
  let n = BigInt('0b' + v);
  return '0x' + n.toString(16);
}
function valAt(s, t) {
  const evs = bySig[s];
  let lo = 0, hi = evs.length - 1, ans = evs.length ? evs[0][1] : 'x';
  while (lo <= hi) { const mid = (lo + hi) >> 1; if (evs[mid][0] <= t) { ans = evs[mid][1]; lo = mid + 1; } else hi = mid - 1; }
  return ans;
}
function filter() {
  const q = document.getElementById('q').value.toLowerCase();
  for (const row of listEl.querySelectorAll('.row')) {
    const name = row.dataset.name.toLowerCase();
    row.classList.toggle('hidden', !(name.includes(q) && row.dataset.on === '1'));
  }
}
function toggleAll(on) {
  show = sigs.map(() => on);
  for (const row of listEl.querySelectorAll('.row')) row.dataset.on = on ? '1' : '0';
  filter(); draw();
}
function buildList() {
  listEl.querySelectorAll('.row').forEach(r => r.remove());
  sigs.forEach((s, i) => {
    const row = document.createElement('div');
    row.className = 'row'; row.dataset.name = s.name; row.dataset.on = show[i] ? '1' : '0';
    row.innerHTML = '<input type="checkbox" ' + (show[i] ? 'checked' : '') + '>' +
      '<span class="v" id="val' + i + '"></span><span class="w">' + s.w + '</span><span>' + esc(s.name) + '</span>';
    row.onclick = (e) => { if (e.target.tagName !== 'INPUT') { show[i] = !show[i]; row.querySelector('input').checked = show[i]; row.dataset.on = show[i] ? '1' : '0'; filter(); draw(); } };
    row.querySelector('input').onchange = () => { show[i] = row.querySelector('input').checked; row.dataset.on = show[i] ? '1' : '0'; filter(); draw(); };
    listEl.appendChild(row);
  });
  filter();
}
function esc(s) { return s.replace(/&/g, '&amp;').replace(/</g, '&lt;'); }

const NS = 'http://www.w3.org/2000/svg';
function el(tag, attrs) { const e = document.createElementNS(NS, tag); for (const k in attrs) e.setAttribute(k, attrs[k]); return e; }

function draw() {
  const W = pane.clientWidth, H = pane.clientHeight;
  svg.setAttribute('viewBox', '0 0 ' + W + ' ' + H);
  while (svg.firstChild) svg.removeChild(svg.firstChild);
  const rows = sigs.map((s, i) => i).filter(i => show[i]);
  const x = t => LEFT + (t - view.t0) / view.span * (W - 20) + 10;
  // axis
  const steps = 8;
  for (let k = 0; k <= steps; k++) {
    const t = view.t0 + view.span * k / steps;
    const xx = x(t);
    svg.appendChild(el('line', { x1: xx, y1: 0, x2: xx, y2: H, class: 'axis' }));
    const lab = el('text', { x: xx + 3, y: 12, class: 'tlab' });
    lab.textContent = fmtT(t); svg.appendChild(lab);
  }
  rows.forEach((si, r) => {
    const y = 18 + r * (ROW_H + GAP), s = sigs[si];
    const evs = bySig[si];
    if (s.real) {
      // analog trace: polyline, y auto-scaled to the signal's value range
      let lo = Infinity, hi = -Infinity;
      for (const [t, v] of evs) { const f = parseFloat(v); if (!isNaN(f)) { if (f < lo) lo = f; if (f > hi) hi = f; } }
      if (!isFinite(lo)) { lo = 0; hi = 1; }
      if (hi - lo < 1e-12) { const m = (hi + lo) / 2; lo = m - 0.5; hi = m + 0.5; }
      const yOf = f => y + (ROW_H - 6) - (f - lo) / (hi - lo) * (ROW_H - 6);
      let d = '';
      for (const [t, v] of evs) {
        const f = parseFloat(v); if (isNaN(f)) continue;
        d += (d ? ' L' : 'M') + x(t).toFixed(1) + ' ' + yOf(f).toFixed(1);
      }
      if (d) d += ' L' + x(view.t0 + view.span).toFixed(1) + ' ' + yOf(parseFloat(valAt(si, view.t0 + view.span))).toFixed(1);
      if (d) svg.appendChild(el('path', { d, class: 'analog' }));
      return;
    }
    if (s.w === 1) {
      let d = '', cur = null;
      for (const [t, v] of evs) {
        const xx = x(t), bit = v[v.length - 1];
        if (cur === null) { d += 'M' + xx + ' ' + (bit === '1' ? y : y + ROW_H - 6); }
        else if (bit !== cur) { d += ' V' + (bit === '1' ? y : y + ROW_H - 6) + ' H' + xx; }
        else d += ' H' + xx;
        cur = bit;
      }
      if (cur !== null) d += ' H' + x(view.t0 + view.span);
      svg.appendChild(el('path', { d, class: 'sig1' }));
    } else {
      // bus: band per constant segment + hex label
      let segStart = view.t0, segVal = valAt(si, view.t0);
      const flush = (tEnd) => {
        const x0 = x(segStart), x1 = x(tEnd);
        if (x1 - x0 < 1) return;
        svg.appendChild(el('rect', { x: x0, y: y, width: x1 - x0, height: ROW_H - 6, class: 'busband' }));
        const lab = el('text', { x: (x0 + x1) / 2, y: y + ROW_H / 2 + 3, class: /[xz]/.test(segVal) ? 'xlab' : 'buslab' });
        lab.textContent = jsonBitsToHex(segVal); svg.appendChild(lab);
      };
      for (const [t, v] of evs) {
        if (t <= view.t0) { segVal = v; continue; }
        if (t >= view.t0 + view.span) break;
        if (v !== segVal) { flush(t); segStart = t; segVal = v; }
      }
      flush(view.t0 + view.span);
    }
  });
  if (cursorT !== null) {
    svg.appendChild(el('line', { x1: x(cursorT), y1: 0, x2: x(cursorT), y2: H, class: 'cursor' }));
  }
  rows.forEach((si, r) => {
    const elv = document.getElementById('val' + si);
    if (elv) elv.textContent = cursorT === null ? '' : (sigs[si].real ? valAt(si, cursorT) : (sigs[si].w === 1 ? valAt(si, cursorT).slice(-1) : jsonBitsToHex(valAt(si, cursorT))));
  });
}
function fit() { view = { t0: 0, span: tmax || 1 }; draw(); }

let drag = null;
svg.onmousedown = e => { drag = { x: e.clientX, t0: view.t0, span: view.span }; };
window.onmouseup = () => { drag = null; };
window.onmousemove = e => {
  const r = svg.getBoundingClientRect();
  const W = pane.clientWidth;
  const t = view.t0 + (e.clientX - r.left - 10) / (W - 20) * view.span;
  cursorT = Math.max(view.t0, Math.min(view.t0 + view.span, t));
  if (drag) {
    const dt = (drag.x - e.clientX) / (W - 20) * drag.span;
    view.t0 = Math.max(0, Math.min(tmax - view.span, drag.t0 + dt));
  }
  draw();
};
svg.onmouseleave = () => { cursorT = null; draw(); };
svg.onwheel = e => {
  e.preventDefault();
  const r = svg.getBoundingClientRect(), W = pane.clientWidth;
  const anchor = view.t0 + (e.clientX - r.left - 10) / (W - 20) * view.span;
  const f = e.deltaY > 0 ? 1.25 : 0.8;
  view.span = Math.max(1, Math.min(tmax || 1, view.span * f));
  view.t0 = Math.max(0, Math.min(tmax - view.span, anchor - (anchor - view.t0) * f));
  draw();
};
window.onresize = draw;
buildList(); fit();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::Value;
    use hdl_sim::events::SimEvent;

    fn flat_with_two_sigs() -> Flat {
        use hdl_ir::elab::{FlatSig, Scope};
        use hdl_ir::ir::SigKind;
        Flat {
            signals: vec![
                FlatSig {
                    name: "tb.clk".into(),
                    width: 1,
                    kind: SigKind::Wire,
                    init: Some(Value::from_u64(1, 0)),
                    mem: None,
                    scope: 0,
                    signed: false,
                    real_ranges: Vec::new(),
                },
                FlatSig {
                    name: "tb.q".into(),
                    width: 8,
                    kind: SigKind::Wire,
                    init: Some(Value::from_u64(8, 0)),
                    mem: None,
                    scope: 0,
                    signed: false,
                    real_ranges: Vec::new(),
                },
            ],
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
    fn html_file_contains_signals_and_events() {
        let flat = flat_with_two_sigs();
        let path = std::env::temp_dir().join("hdlsim_html_sink_test.html");
        let file = std::fs::File::create(&path).unwrap();
        let mut sink = HtmlSink::new(file, path.clone(), &flat);
        sink.on_event(
            &SimEvent::SignalChange {
                sig: 0,
                old: Value::from_u64(1, 0),
                new: Value::from_u64(1, 1),
                time_fs: 5_000_000,
            },
            &flat,
        );
        sink.on_event(
            &SimEvent::Message {
                text: "PASS\n".into(),
                time_fs: 9_000_000,
                span: None,
                severity: None,
                scope: None,
            },
            &flat,
        );
        sink.finish(&flat);
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("tb.clk"), "signal name missing");
        assert!(html.contains("tb.q"), "signal name missing");
        assert!(
            html.contains("\\\"w\\\":8") || html.contains("\"w\":8"),
            "width missing"
        );
        assert!(html.contains("5000000"), "event time missing");
        assert!(html.contains("PASS"), "transcript missing");
        assert!(html.contains("const DATA = {"), "data block missing");
        assert!(!html.contains("/*__DATA__*/"), "placeholder not replaced");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn json_escaping_neutralizes_script_breakout() {
        assert_eq!(json_str("a</script>b"), "\"a\\u003c/script\\u003eb\"");
        assert_eq!(json_str("q\"\\\n"), "\"q\\\"\\\\\\n\"");
    }

    #[test]
    fn bits_msb_order() {
        assert_eq!(bits_msb(&Value::from_u64(4, 0b1010)), "1010");
        assert_eq!(bits_msb(&Value::x(3)), "xxx");
    }
}
