//! `hdlsim lsp` — Language Server Protocol server over stdio (phase 2 of
//! docs/plan.md).
//!
//! Slice 1: JSON-RPC base protocol (Content-Length framing), `initialize` /
//! `shutdown` / `exit`, full-sync `textDocument/didOpen|didChange|didClose`,
//! and `textDocument/publishDiagnostics` straight from the hdl-syntax lexers
//! and parsers — so the editor shows the same errors as `hdlsim check`.
//! Positions are 0-based line + UTF-16 code-unit column (LSP default).

use super::Command;
use crate::json::{Json, JsonCtor};
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::{Diagnostic, Severity};
use hdl_syntax::kinds::SyntaxKind;
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
        "editor integration over stdio: live diagnostics, completion, hover, go-to-definition"
    }
    fn run(&self, _args: &[String]) -> anyhow::Result<i32> {
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

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
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
                        Json::obj(vec![
                            (
                                "textDocumentSync",
                                Json::obj(vec![
                                    ("openClose", Json::Bool(true)),
                                    // 1 = TextDocumentSyncKind.Full
                                    ("change", Json::num(1)),
                                ]),
                            ),
                            ("hoverProvider", Json::Bool(true)),
                            ("definitionProvider", Json::Bool(true)),
                            (
                                "completionProvider",
                                Json::obj(vec![(
                                    "triggerCharacters",
                                    Json::Array(vec![Json::str(".")]),
                                )]),
                            ),
                        ]),
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
                    .and_then(|c| c.as_array())
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
            Some("textDocument/hover") | Some("textDocument/definition") => {
                let is_hover = method == Some("textDocument/hover");
                let uri = msg
                    .get("params")
                    .and_then(|p| p.get("textDocument"))
                    .and_then(|t| t.get("uri"))
                    .and_then(|u| u.as_str());
                let line = msg
                    .get("params")
                    .and_then(|p| p.get("position"))
                    .and_then(|p| p.get("line"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as u32;
                let character = msg
                    .get("params")
                    .and_then(|p| p.get("position"))
                    .and_then(|p| p.get("character"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as u32;
                let result = match uri.and_then(|u| self.docs.get(u).map(|t| (u, t))) {
                    Some((uri, text)) => {
                        let off = position_to_offset(text, line, character)
                            .unwrap_or(0)
                            .min(text.len().saturating_sub(1) as u32);
                        match uri_to_path(uri).and_then(|p| parse_root(&p, text)) {
                            Some((db, root, _diags)) => {
                                if is_hover {
                                    match hover_at(&db, &root, off) {
                                        Some(v) => Json::obj(vec![(
                                            "contents",
                                            Json::obj(vec![
                                                ("kind", Json::str("markdown")),
                                                ("value", Json::str(v)),
                                            ]),
                                        )]),
                                        None => Json::Null,
                                    }
                                } else {
                                    match definition_at(&db, &root, off) {
                                        Some(span) => location_json(uri, text, span),
                                        None => Json::Null,
                                    }
                                }
                            }
                            None => Json::Null,
                        }
                    }
                    None => Json::Null,
                };
                match id {
                    Some(id) => vec![response(Some(id), result)],
                    None => vec![],
                }
            }
            Some("textDocument/completion") => {
                let uri = msg
                    .get("params")
                    .and_then(|p| p.get("textDocument"))
                    .and_then(|t| t.get("uri"))
                    .and_then(|u| u.as_str());
                let line = msg
                    .get("params")
                    .and_then(|p| p.get("position"))
                    .and_then(|p| p.get("line"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as u32;
                let character = msg
                    .get("params")
                    .and_then(|p| p.get("position"))
                    .and_then(|p| p.get("character"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as u32;
                let result = match uri.and_then(|u| self.docs.get(u).map(|t| (u, t))) {
                    Some((uri, text)) => {
                        let off = position_to_offset(text, line, character)
                            .unwrap_or(0)
                            .min(text.len() as u32) as usize;
                        let prefix = ident_prefix(text, off);
                        let items = match uri_to_path(uri) {
                            Some(path) => match Language::from_path(&path) {
                                Some(lang) => completion_items(lang, &path, text, &prefix),
                                None => Vec::new(),
                            },
                            None => Vec::new(),
                        };
                        Json::obj(vec![
                            ("isIncomplete", Json::Bool(false)),
                            ("items", Json::Array(items)),
                        ])
                    }
                    None => Json::Null,
                };
                match id {
                    Some(id) => vec![response(Some(id), result)],
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
                ("diagnostics", Json::Array(diags)),
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
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lex + parse one document (raw text, so spans align with the editor buffer).
fn parse_doc(path: &std::path::Path, text: &str) -> Vec<Diagnostic> {
    match parse_root(path, text) {
        Some((_db, _root, diags)) => diags,
        None => Vec::new(),
    }
}

/// Identifier characters being typed at `off` (the prefix up to the cursor).
fn ident_prefix(text: &str, off: usize) -> String {
    let b = text.as_bytes();
    let end = off.min(b.len());
    let mut start = end;
    while start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
        start -= 1;
    }
    text[start..end].to_string()
}

const VERILOG_KEYWORDS: &[&str] = &[
    "always",
    "and",
    "assign",
    "begin",
    "buf",
    "bufif0",
    "bufif1",
    "case",
    "casex",
    "casez",
    "default",
    "end",
    "endcase",
    "endfunction",
    "endgenerate",
    "endmodule",
    "endtask",
    "else",
    "for",
    "forever",
    "fork",
    "function",
    "generate",
    "genvar",
    "if",
    "inout",
    "input",
    "integer",
    "join",
    "localparam",
    "module",
    "nand",
    "negedge",
    "nor",
    "not",
    "notif0",
    "notif1",
    "or",
    "output",
    "parameter",
    "posedge",
    "pulldown",
    "pullup",
    "real",
    "reg",
    "repeat",
    "signed",
    "supply0",
    "supply1",
    "task",
    "time",
    "tri",
    "tri0",
    "tri1",
    "unsigned",
    "wait",
    "wand",
    "while",
    "wire",
    "wor",
    "xnor",
    "xor",
];

const VHDL_KEYWORDS: &[&str] = &[
    "abs",
    "after",
    "alias",
    "all",
    "and",
    "architecture",
    "array",
    "assert",
    "attribute",
    "begin",
    "block",
    "body",
    "buffer",
    "bus",
    "case",
    "component",
    "configuration",
    "constant",
    "disconnect",
    "downto",
    "else",
    "elsif",
    "end",
    "entity",
    "exit",
    "file",
    "for",
    "function",
    "generate",
    "generic",
    "group",
    "guarded",
    "if",
    "impure",
    "in",
    "inertial",
    "inout",
    "is",
    "label",
    "library",
    "linkage",
    "loop",
    "map",
    "mod",
    "nand",
    "new",
    "next",
    "nor",
    "not",
    "null",
    "of",
    "on",
    "open",
    "or",
    "others",
    "out",
    "package",
    "port",
    "postponed",
    "procedure",
    "process",
    "pure",
    "range",
    "record",
    "register",
    "reject",
    "rem",
    "report",
    "return",
    "rol",
    "ror",
    "select",
    "severity",
    "signal",
    "shared",
    "sla",
    "sll",
    "sra",
    "srl",
    "subtype",
    "then",
    "to",
    "transport",
    "type",
    "unaffected",
    "units",
    "until",
    "use",
    "variable",
    "wait",
    "when",
    "while",
    "with",
    "xnor",
    "xor",
];

/// Build completion items: language keywords (kind 14) plus every identifier
/// already used in the document (kind 6), filtered by the typed prefix
/// (case-insensitive), deduped, sorted. Keywords win over identifiers.
fn completion_items(lang: Language, path: &std::path::Path, text: &str, prefix: &str) -> Vec<Json> {
    let kws: &[&str] = match lang {
        Language::Verilog => VERILOG_KEYWORDS,
        Language::Vhdl => VHDL_KEYWORDS,
    };
    let mut kinds: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for k in kws {
        kinds.insert(k.to_string(), 14); // CompletionItemKind.Keyword
    }
    if let Some((db, root, _)) = parse_root(path, text) {
        for d in root.descendants() {
            if d.kind() == SyntaxKind::Ident {
                kinds.entry(d.text(&db).to_string()).or_insert(6); // Variable
            }
        }
    }
    let pfx = prefix.to_ascii_lowercase();
    let mut items: Vec<(String, i64)> = kinds
        .into_iter()
        .filter(|(label, _)| pfx.is_empty() || label.to_ascii_lowercase().starts_with(&pfx))
        .collect();
    items.sort();
    items
        .into_iter()
        .map(|(label, kind)| {
            Json::obj(vec![("label", Json::str(label)), ("kind", Json::num(kind))])
        })
        .collect()
}

/// Parse a document and return the db, CST root, and diagnostics.
fn parse_root(
    path: &std::path::Path,
    text: &str,
) -> Option<(SourceDb, SyntaxNode, Vec<Diagnostic>)> {
    let mut db = SourceDb::new();
    let id = db.add_file(path.display().to_string(), text).ok()?;
    let (root, diags) = match Language::from_path(path)? {
        Language::Verilog => {
            let p = hdl_syntax::verilog::parser::parse_file(id, db.file(id));
            (p.root, p.diags)
        }
        Language::Vhdl => {
            let p = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
            (p.root, p.diags)
        }
    };
    Some((db, root, diags))
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

/// LSP `Position` (0-based line, UTF-16 column) -> byte offset. Inverse of
/// [`offset_to_position`].
pub fn position_to_offset(text: &str, line: u32, character: u32) -> Option<u32> {
    let mut cur = 0u32;
    let mut start = 0usize;
    for (i, ch) in text.char_indices() {
        if ch == '\n' {
            if cur == line {
                break;
            }
            cur += 1;
            start = i + 1;
        }
    }
    if cur != line {
        return None;
    }
    let mut col = 0u32;
    for (j, c) in text[start..].char_indices() {
        if col >= character {
            return Some((start + j) as u32);
        }
        col += c.len_utf16() as u32;
    }
    Some(text.len() as u32)
}

/// Deepest non-trivia leaf token containing byte offset `off`.
fn leaf_at(root: &SyntaxNode, off: u32) -> Option<SyntaxNode> {
    for d in root.descendants() {
        if d.children().next().is_some() {
            continue; // not a leaf
        }
        match d.kind() {
            SyntaxKind::Whitespace | SyntaxKind::Comment | SyntaxKind::Eof => continue,
            _ => {}
        }
        let sp = d.span();
        if sp.start > off {
            break;
        }
        if sp.start <= off && off < sp.end {
            return Some(d);
        }
    }
    None
}

/// Hover text (markdown) for the token at byte offset `off`.
fn hover_at(db: &SourceDb, root: &SyntaxNode, off: u32) -> Option<String> {
    let leaf = leaf_at(root, off)?;
    let tok = leaf.text(db);
    // Nearest non-root ancestor gives the syntactic context.
    let mut ctx = leaf.parent();
    let mut ctx_kind = SyntaxKind::Root;
    let mut snippet = String::new();
    while let Some(n) = ctx {
        if n.kind() == SyntaxKind::Root || n.kind() == SyntaxKind::Name {
            ctx = n.parent();
            continue;
        }
        ctx_kind = n.kind();
        let t = n.text(db);
        let first = t.lines().next().unwrap_or("").trim();
        snippet = if first.len() > 80 {
            format!("{}...", &first[..80])
        } else {
            first.to_string()
        };
        break;
    }
    Some(format!(
        "`{}` — `{:?}`\n\n```\n{}\n```",
        tok, ctx_kind, snippet
    ))
}

/// Keywords after which an identifier is a declaration name.
const DECL_KEYWORDS: &[&str] = &[
    "module",
    "macromodule",
    "entity",
    "architecture",
    "process",
    "function",
    "task",
    "procedure",
    "package",
    "signal",
    "variable",
    "constant",
    "generic",
    "port",
    "type",
    "subtype",
    "component",
    "wire",
    "reg",
    "logic",
    "integer",
    "real",
    "genvar",
    "input",
    "output",
    "inout",
    "parameter",
    "localparam",
];

/// Go-to-definition: if the token at `off` is an identifier, find its
/// declaration in the same document (an `Ident` leaf preceded by a declaration
/// keyword). Returns the declaration span.
fn definition_at(db: &SourceDb, root: &SyntaxNode, off: u32) -> Option<Span> {
    let leaf = leaf_at(root, off)?;
    if leaf.kind() != SyntaxKind::Ident {
        return None;
    }
    let name = leaf.text(db);
    if DECL_KEYWORDS.contains(&name.to_ascii_lowercase().as_str()) {
        return Some(leaf.span()); // cursor on the keyword itself
    }
    let mut prev: Option<String> = None;
    for d in root.descendants() {
        if d.children().next().is_some() {
            continue;
        }
        match d.kind() {
            SyntaxKind::Whitespace | SyntaxKind::Comment | SyntaxKind::Eof => continue,
            _ => {}
        }
        if d.kind() == SyntaxKind::Ident && d.text(db) == name {
            if let Some(p) = &prev {
                if DECL_KEYWORDS.contains(&p.to_ascii_lowercase().as_str()) {
                    return Some(d.span());
                }
            }
        }
        prev = Some(d.text(db).to_string());
    }
    None
}

fn location_json(uri: &str, text: &str, span: Span) -> Json {
    let pos = |off: u32| {
        let (l, c) = offset_to_position(text, off);
        Json::obj(vec![("line", Json::num(l)), ("character", Json::num(c))])
    };
    Json::obj(vec![
        ("uri", Json::str(uri)),
        (
            "range",
            Json::obj(vec![
                ("start", pos(span.start)),
                ("end", pos(span.end.max(span.start))),
            ]),
        ),
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
            .as_array()
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
            .as_array()
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
            .as_array()
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
            .as_array()
            .unwrap();
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
            r#"{"jsonrpc":"2.0","id":9,"method":"textDocument/semanticTokens"}"#,
        ));
        assert_eq!(
            out[0].get("error").unwrap().get("code"),
            Some(&Json::num(-32601))
        );
    }

    #[test]
    fn position_offset_roundtrip_utf16() {
        let text = "ab\n\u{1f600}x\nlast";
        for off in [0usize, 1, 2, 3, 7, 8, 9, 12, 13] {
            let (l, c) = offset_to_position(text, off as u32);
            assert_eq!(
                position_to_offset(text, l, c),
                Some(off as u32),
                "off {}",
                off
            );
        }
        assert_eq!(position_to_offset(text, 2, 0), Some(9));
        assert_eq!(position_to_offset(text, 99, 0), None);
    }

    #[test]
    fn hover_and_definition_requests() {
        let mut srv = Server::new();
        let uri = "file:///tmp/def.v";
        let text = "module m;\nwire d;\nwire q;\ninitial q = d;\nendmodule\n";
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","text":"{}"}}}}}}"#,
            uri,
            text.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        );
        srv.handle(&msg(&open));
        // hover over `m` in `module m;` (line 0, char 7)
        let hov = format!(
            r#"{{"jsonrpc":"2.0","id":10,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{}"}},"position":{{"line":0,"character":7}}}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&hov));
        let value = out[0]
            .get("result")
            .and_then(|r| r.get("contents"))
            .and_then(|c| c.get("value"))
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(value.contains("`m`"), "{}", value);
        assert!(value.contains("Module"), "{}", value);
        // definition of `d` used at line 3, char 12 -> declared at line 1, char 5
        let def = format!(
            r#"{{"jsonrpc":"2.0","id":11,"method":"textDocument/definition","params":{{"textDocument":{{"uri":"{}"}},"position":{{"line":3,"character":12}}}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&def));
        let range = out[0].get("result").and_then(|r| r.get("range")).unwrap();
        assert_eq!(range.get("start").unwrap().get("line"), Some(&Json::num(1)));
        assert_eq!(
            range.get("start").unwrap().get("character"),
            Some(&Json::num(5))
        );
        assert_eq!(
            out[0]
                .get("result")
                .unwrap()
                .get("uri")
                .and_then(|u| u.as_str()),
            Some(uri)
        );
    }

    #[test]
    fn initialize_advertises_completion() {
        let mut srv = Server::new();
        let out = srv.handle(&msg(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        ));
        let caps = out[0].get("result").unwrap().get("capabilities").unwrap();
        let cp = caps.get("completionProvider").unwrap();
        assert_eq!(
            cp.get("triggerCharacters").unwrap().as_array().unwrap()[0],
            Json::str(".")
        );
    }

    #[test]
    fn completion_offers_keywords_and_doc_idents_filtered_by_prefix() {
        let mut srv = Server::new();
        let uri = "file:///tmp/tb.v";
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","text":"module tb;\n  wire count_sig;\n  al\nendmodule\n"}}}}}}"#,
            uri
        );
        srv.handle(&msg(&open));
        // Cursor right after "al" on line 2 (0-based): prefix "al".
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":7,"method":"textDocument/completion","params":{{"textDocument":{{"uri":"{}"}},"position":{{"line":2,"character":4}}}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&req));
        let items = out[0]
            .get("result")
            .unwrap()
            .get("items")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i.get("label").unwrap().as_str().unwrap())
            .collect();
        assert!(labels.contains(&"always"), "{:?}", labels);
        assert!(!labels.contains(&"module"), "prefix filter: {:?}", labels);
        // Empty prefix returns everything, keywords + doc idents, deduped.
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":8,"method":"textDocument/completion","params":{{"textDocument":{{"uri":"{}"}},"position":{{"line":0,"character":0}}}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&req));
        let items = out[0]
            .get("result")
            .unwrap()
            .get("items")
            .unwrap()
            .as_array()
            .unwrap();
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i.get("label").unwrap().as_str().unwrap())
            .collect();
        assert!(labels.contains(&"module"), "{:?}", labels);
        assert!(labels.contains(&"count_sig"), "{:?}", labels);
        assert!(labels.contains(&"tb"), "{:?}", labels);
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted.to_vec(), "items sorted");
        let mut dedup = labels.clone();
        dedup.dedup();
        assert_eq!(labels.len(), dedup.len(), "no duplicates: {:?}", labels);
    }

    #[test]
    fn completion_is_case_insensitive_for_vhdl() {
        let mut srv = Server::new();
        let uri = "file:///tmp/tb.vhd";
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","text":"entity tb is\nend entity;\nSIG\n"}}}}}}"#,
            uri
        );
        srv.handle(&msg(&open));
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":9,"method":"textDocument/completion","params":{{"textDocument":{{"uri":"{}"}},"position":{{"line":2,"character":3}}}}}}"#,
            uri
        );
        let out = srv.handle(&msg(&req));
        let items = out[0]
            .get("result")
            .unwrap()
            .get("items")
            .unwrap()
            .as_array()
            .unwrap();
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i.get("label").unwrap().as_str().unwrap())
            .collect();
        assert!(
            labels.contains(&"signal"),
            "prefix SIG matches signal: {:?}",
            labels
        );
        assert!(!labels.contains(&"entity"), "prefix filter: {:?}", labels);
    }

    #[test]
    fn initialize_advertises_hover_and_definition() {
        let mut srv = Server::new();
        let out = srv.handle(&msg(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        ));
        let caps = out[0].get("result").unwrap().get("capabilities").unwrap();
        assert_eq!(caps.get("hoverProvider"), Some(&Json::Bool(true)));
        assert_eq!(caps.get("definitionProvider"), Some(&Json::Bool(true)));
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
