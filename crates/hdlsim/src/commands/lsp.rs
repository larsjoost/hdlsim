//! `hdlsim lsp` — Language Server Protocol server over stdio (phase 2 of
//! docs/plan.md).
//!
//! Slice 1: JSON-RPC base protocol (Content-Length framing), `initialize` /
//! `shutdown` / `exit`, full-sync `textDocument/didOpen|didChange|didClose`,
//! and `textDocument/publishDiagnostics` straight from the hdl-syntax lexers
//! and parsers — so the editor shows the same errors as `hdlsim check`.
//! Positions are 0-based line + UTF-16 code-unit column (LSP default).

use super::Command;
use crate::json::Json;
use hdl_syntax::diagnostic::{Diagnostic, Severity};
use hdl_syntax::language::Language;
use hdl_syntax::source::{SourceDb, Span};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub struct Lsp;

impl Command for Lsp {
    fn name(&self) -> &'static str {
        "lsp"
    }
    fn about(&self) -> &'static str {
        "run an LSP server on stdio (initialize + live parse diagnostics)"
    }
    fn run(&self, _args: &[String]) -> Result<i32, String> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut r = stdin.lock();
        let mut w = stdout.lock();
        Ok(serve(&mut r, &mut w))
    }
}

/// Server state: open documents (`uri` -> full text) plus lifecycle flags.
pub struct Server {
    docs: HashMap<String, String>,
    shutdown_seen: bool,
    /// Set by the `exit` notification; `serve` stops and reports the code.
    pub exit_requested: bool,
}

impl Server {
    pub fn new() -> Server {
        Server {
            docs: HashMap::new(),
            shutdown_seen: false,
            exit_requested: false,
        }
    }

    /// Handle one JSON-RPC message; returns the messages to send back
    /// (responses and notifications), in order.
    pub fn handle(&mut self, msg: &Json) -> Vec<Json> {
        let method = msg.get("method").and_then(|m| m.as_str());
        let id = msg.get("id").cloned();
        match method {
            Some("initialize") => vec![response(
                id,
                Json::obj(vec![
                    (
                        "serverInfo",
                        Json::obj(vec![
                            ("name", Json::str("hdlsim")),
                            ("version", Json::str(env!("CARGO_PKG_VERSION"))),
                        ]),
                    ),
                    (
                        "capabilities",
                        Json::obj(vec![(
                            "textDocumentSync",
                            Json::obj(vec![
                                ("openClose", Json::Bool(true)),
                                // 1 = TextDocumentSyncKind.Full
                                ("change", Json::num(1)),
                            ]),
                        )]),
                    ),
                ]),
            )],
            Some("shutdown") => {
                self.shutdown_seen = true;
                match id {
                    Some(id) => vec![response(Some(id), Json::Null)],
                    None => vec![],
                }
            }
            Some("exit") => {
                self.exit_requested = true;
                vec![]
            }
            Some("textDocument/didOpen") => {
                let td = msg.get("params").and_then(|p| p.get("textDocument"));
                let uri = td.and_then(|t| t.get("uri")).and_then(|u| u.as_str());
                let text = td.and_then(|t| t.get("text")).and_then(|t| t.as_str());
                match (uri, text) {
                    (Some(uri), Some(text)) => {
                        self.docs.insert(uri.to_string(), text.to_string());
                        vec![self.publish(uri)]
                    }
                    _ => vec![],
                }
            }
            Some("textDocument/didChange") => {
                let uri = msg
                    .get("params")
                    .and_then(|p| p.get("textDocument"))
                    .and_then(|t| t.get("uri"))
                    .and_then(|u| u.as_str());
                // Full sync: the last content change carries the whole text.
                let text = msg
                    .get("params")
                    .and_then(|p| p.get("contentChanges"))
                    .and_then(|c| c.as_arr())
                    .and_then(|a| a.last())
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str());
                match (uri, text) {
                    (Some(uri), Some(text)) if self.docs.contains_key(uri) => {
                        self.docs.insert(uri.to_string(), text.to_string());
                        vec![self.publish(uri)]
                    }
                    _ => vec![],
                }
            }
            Some("textDocument/didClose") => {
                let uri = msg
                    .get("params")
                    .and_then(|p| p.get("textDocument"))
                    .and_then(|t| t.get("uri"))
                    .and_then(|u| u.as_str());
                match uri {
                    Some(uri) => {
                        self.docs.remove(uri);
                        // Clear the squiggles in the editor.
                        vec![publish_diagnostics(uri, Vec::new())]
                    }
                    None => vec![],
                }
            }
            Some("initialized") | Some("textDocument/didSave") | Some("$/cancelRequest") => vec![],
            Some(other) => match id {
                Some(id) => vec![error_response(
                    id,
                    -32601,
                    &format!("method not found: {}", other),
                )],
                None => vec![],
            },
            None => vec![], // a response from the client; ignore
        }
    }

    fn publish(&self, uri: &str) -> Json {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let diags = match uri_to_path(uri) {
            Some(path) => parse_doc(&path, &text)
                .iter()
                .map(|d| to_lsp_diag(&text, d))
                .collect(),
            None => Vec::new(),
        };
        publish_diagnostics(uri, diags)
    }
}

/// Run the server loop: read framed messages, dispatch, write responses.
/// Returns the process exit code (0 after `shutdown`+`exit`, 1 otherwise).
pub fn serve(r: &mut impl BufRead, w: &mut impl Write) -> i32 {
    let mut srv = Server::new();
    loop {
        let body = match read_message(r) {
            Ok(Some(b)) => b,
            Ok(None) => break, // clean EOF
            Err(_) => break,   // broken pipe / io error: just stop
        };
        match Json::parse(&body) {
            Ok(msg) => {
                for out in srv.handle(&msg) {
                    let _ = write_message(w, &out.to_string());
                }
                let _ = w.flush();
                if srv.exit_requested {
                    break;
                }
            }
            Err(e) => {
                let _ = write_message(
                    w,
                    &error_response(Json::Null, -32700, &format!("parse error: {}", e)).to_string(),
                );
                let _ = w.flush();
            }
        }
    }
    if srv.shutdown_seen {
        0
    } else {
        1
    }
}

/// Read one LSP base-protocol frame (`Content-Length` header + JSON body).
/// `Ok(None)` on clean EOF before any header byte.
pub fn read_message(r: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some(v) = line
            .split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .map(|(_, v)| v)
        {
            len = Some(v.trim().parse().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "bad Content-Length")
            })?);
        }
        // Other headers (Content-Type, ...) are ignored.
    }
    let mut buf = vec![0u8; len.unwrap()];
    r.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Write one LSP base-protocol frame.
pub fn write_message(w: &mut impl Write, payload: &str) -> std::io::Result<()> {
    write!(w, "Content-Length: {}\r\n\r\n{}", payload.len(), payload)
}

fn response(id: Option<Json>, result: Json) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", id.unwrap_or(Json::Null)),
        ("result", result),
    ])
}

fn error_response(id: Json, code: i64, message: &str) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", id),
        (
            "error",
            Json::obj(vec![
                ("code", Json::num(code)),
                ("message", Json::str(message)),
            ]),
        ),
    ])
}

fn publish_diagnostics(uri: &str, diags: Vec<Json>) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("method", Json::str("textDocument/publishDiagnostics")),
        (
            "params",
            Json::obj(vec![
                ("uri", Json::str(uri)),
                ("diagnostics", Json::Arr(diags)),
            ]),
        ),
    ])
}

/// `file:///path/to/x.v` -> `/path/to/x.v`, with percent-decoding.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode(rest)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = match std::str::from_utf8(&bytes[i + 1..i + 3]) {
                Ok(h) => h,
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                    continue;
                }
            };
            match u8::from_str_radix(hex, 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                    continue;
                }
                Err(_) => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lex + parse one document (raw text, so spans align with the editor buffer).
fn parse_doc(path: &std::path::Path, text: &str) -> Vec<Diagnostic> {
    let mut db = SourceDb::new();
    let Ok(id) = db.add_file(path.display().to_string(), text) else {
        return Vec::new();
    };
    match Language::from_path(path) {
        Some(Language::Verilog) => hdl_syntax::verilog::parser::parse_file(id, db.file(id)).diags,
        Some(Language::Vhdl) => hdl_syntax::vhdl::parser::parse_file(id, db.file(id)).diags,
        None => Vec::new(),
    }
}

/// LSP position: 0-based line, 0-based UTF-16 code-unit column.
pub fn offset_to_position(text: &str, offset: u32) -> (u32, u32) {
    let off = (offset as usize).min(text.len());
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, ch) in text.char_indices() {
        if i >= off {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as u32;
        }
    }
    (line, col)
}

fn to_lsp_diag(text: &str, d: &Diagnostic) -> Json {
    let span: Option<Span> = d.span;
    let (s, e) = span.map(|sp| (sp.start, sp.end)).unwrap_or((0, 0));
    let (sl, sc) = offset_to_position(text, s);
    let (el, ec) = offset_to_position(text, e.max(s));
    let severity = match d.severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Note => 4,
    };
    Json::obj(vec![
        (
            "range",
            Json::obj(vec![
                (
                    "start",
                    Json::obj(vec![("line", Json::num(sl)), ("character", Json::num(sc))]),
                ),
                (
                    "end",
                    Json::obj(vec![("line", Json::num(el)), ("character", Json::num(ec))]),
                ),
            ]),
        ),
        ("severity", Json::num(severity)),
        ("code", Json::str(d.code)),
        ("source", Json::str("hdlsim")),
        ("message", Json::str(d.message.clone())),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn msg(s: &str) -> Json {
        Json::parse(s).unwrap()
    }

    #[test]
    fn framing_roundtrip() {
        let mut buf = Vec::new();
        write_message(&mut buf, "{\"jsonrpc\":\"2.0\"}").unwrap();
        write_message(&mut buf, "hello \u{e9}").unwrap();
        let mut cur = Cursor::new(buf);
        assert_eq!(
            read_message(&mut cur).unwrap().as_deref(),
            Some("{\"jsonrpc\":\"2.0\"}")
        );
        assert_eq!(
            read_message(&mut cur).unwrap().as_deref(),
            Some("hello \u{e9}")
        );
        assert_eq!(read_message(&mut cur).unwrap(), None);
    }

    #[test]
    fn positions_are_utf16() {
        let text = "ab\n\u{e9}x\n";
        assert_eq!(offset_to_position(text, 0), (0, 0));
        assert_eq!(offset_to_position(text, 2), (0, 2));
        assert_eq!(offset_to_position(text, 3), (1, 0)); // after '\n'
        assert_eq!(offset_to_position(text, 4), (1, 1)); // é = 1 utf16 unit
        assert_eq!(offset_to_position(text, 6), (1, 2));
        let text = "a\u{1f600}b"; // emoji = 2 utf16 units
        assert_eq!(offset_to_position(text, 5), (0, 3));
    }

    #[test]
    fn uri_decoding() {
        assert_eq!(
            uri_to_path("file:///tmp/a%20b/x.v").unwrap(),
            PathBuf::from("/tmp/a b/x.v")
        );
        assert_eq!(
            uri_to_path("file:///x.vhd").unwrap(),
            PathBuf::from("/x.vhd")
        );
        assert!(uri_to_path("https://x").is_none());
    }

    #[test]
    fn initialize_advertises_full_sync() {
        let mut srv = Server::new();
        let out = srv.handle(&msg(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        ));
        assert_eq!(out.len(), 1);
        let caps = out[0].get("result").unwrap().get("capabilities").unwrap();
        assert_eq!(
            caps.get("textDocumentSync").unwrap().get("change"),
            Some(&Json::num(1))
        );
        assert_eq!(out[0].get("id"), Some(&Json::num(1)));
    }

    #[test]
    fn didopen_publishes_parse_errors_and_change_clears_them() {
        let mut srv = Server::new();
        let uri = "file:///tmp/tb.v";
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","text":"module tb;\n  wire x\nendmodule\n"}}}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&open));
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].get("method").and_then(|m| m.as_str()),
            Some("textDocument/publishDiagnostics")
        );
        let diags = out[0]
            .get("params")
            .unwrap()
            .get("diagnostics")
            .unwrap()
            .as_arr()
            .unwrap();
        assert!(!diags.is_empty(), "expected a parse error");
        assert_eq!(diags[0].get("severity"), Some(&Json::num(1)));
        assert_eq!(
            diags[0].get("source").and_then(|s| s.as_str()),
            Some("hdlsim")
        );
        // The missing `;` after `  wire x` is reported at the next token,
        // `endmodule` on 0-based line 2.
        let line = diags[0]
            .get("range")
            .unwrap()
            .get("start")
            .unwrap()
            .get("line")
            .unwrap()
            .as_i64()
            .unwrap();
        assert_eq!(line, 2);

        // Fix the file: change to valid Verilog -> empty diagnostics.
        let change = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{}"}},"contentChanges":[{{"text":"module tb;\nendmodule\n"}}]}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&change));
        let diags = out[0]
            .get("params")
            .unwrap()
            .get("diagnostics")
            .unwrap()
            .as_arr()
            .unwrap();
        assert!(diags.is_empty());

        // Close: diagnostics cleared, doc forgotten.
        let close = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didClose","params":{{"textDocument":{{"uri":"{}"}}}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&close));
        assert!(out[0]
            .get("params")
            .unwrap()
            .get("diagnostics")
            .unwrap()
            .as_arr()
            .unwrap()
            .is_empty());
        assert!(!srv.docs.contains_key(uri));
    }

    #[test]
    fn vhdl_diagnostics_flow() {
        let mut srv = Server::new();
        let uri = "file:///tmp/t.vhd";
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","text":"entity e is end"}}}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&open));
        let diags = out[0]
            .get("params")
            .unwrap()
            .get("diagnostics")
            .unwrap()
            .as_arr()
            .unwrap()
            .clone();
        assert!(!diags.is_empty());
    }

    #[test]
    fn shutdown_exit_and_unknown_method() {
        let mut srv = Server::new();
        let out = srv.handle(&msg(r#"{"jsonrpc":"2.0","id":7,"method":"shutdown"}"#));
        assert_eq!(out[0].get("result"), Some(&Json::Null));
        assert!(srv.shutdown_seen);
        srv.handle(&msg(r#"{"jsonrpc":"2.0","method":"exit"}"#));
        assert!(srv.exit_requested);
        let out = srv.handle(&msg(
            r#"{"jsonrpc":"2.0","id":9,"method":"textDocument/hover"}"#,
        ));
        assert_eq!(
            out[0].get("error").unwrap().get("code"),
            Some(&Json::num(-32601))
        );
    }

    #[test]
    fn serve_full_session_exit_codes() {
        // shutdown + exit -> 0
        let mut frames = Vec::new();
        for m in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
            r#"{"jsonrpc":"2.0","method":"exit"}"#,
        ] {
            write_message(&mut frames, m).unwrap();
        }
        let mut cur = Cursor::new(frames);
        let mut out = Vec::new();
        assert_eq!(serve(&mut cur, &mut out), 0);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"result\""));
        assert!(text.contains("serverInfo"));

        // EOF without shutdown -> 1
        let mut cur = Cursor::new(Vec::new());
        let mut out = Vec::new();
        assert_eq!(serve(&mut cur, &mut out), 1);
    }
}
