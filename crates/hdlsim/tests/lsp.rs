//! End-to-end test for `hdlsim lsp`: a full JSON-RPC session over in-memory
//! stdio — initialize, didOpen (broken Verilog), didChange (fixed), shutdown,
//! exit — checking the framed responses, including publishDiagnostics.

use hdlsim::commands::lsp::{read_message, serve, write_message};
use hdlsim::json::Json;
use std::io::Cursor;

fn frame(s: &str) -> Vec<u8> {
    let mut v = Vec::new();
    write_message(&mut v, s).unwrap();
    v
}

/// Split framed output back into JSON bodies.
fn bodies(out: &[u8]) -> Vec<Json> {
    let mut cur = Cursor::new(out.to_vec());
    let mut v = Vec::new();
    while let Some(b) = read_message(&mut cur).unwrap() {
        v.push(Json::parse(&b).unwrap());
    }
    v
}

#[test]
fn lsp_session_serves_diagnostics_and_exits_zero() {
    let uri = "file:///tmp/lsp_tb.v";
    let mut input = Vec::new();
    for m in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","method":"initialized"}"#,
        // Broken Verilog: missing `;` after `wire x`.
        &format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","languageId":"verilog","version":1,"text":"module tb;\n  wire x\nendmodule\n"}}}}}}"#,
            uri
        ),
        // Fixed Verilog.
        &format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{}","version":2}},"contentChanges":[{{"text":"module tb;\nendmodule\n"}}]}}}}"#,
            uri
        ),
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
        r#"{"jsonrpc":"2.0","method":"exit"}"#,
    ] {
        input.extend_from_slice(&frame(m));
    }

    let mut cur = Cursor::new(input);
    let mut out = Vec::new();
    let code = serve(&mut cur, &mut out);
    assert_eq!(code, 0, "shutdown+exit must exit 0");

    let msgs = bodies(&out);
    // initialize response
    let init = msgs
        .iter()
        .find(|m| m.get("id").and_then(|i| i.as_i64()) == Some(1))
        .expect("initialize response");
    assert!(init.get("result").is_some());
    assert_eq!(
        init.get("result")
            .unwrap()
            .get("serverInfo")
            .unwrap()
            .get("name")
            .and_then(|n| n.as_str()),
        Some("hdlsim")
    );
    // shutdown response
    assert!(msgs
        .iter()
        .any(|m| m.get("id").and_then(|i| i.as_i64()) == Some(2)
            && m.get("result") == Some(&Json::Null)));
    // two publishDiagnostics notifications: one with an error, one empty
    let diags: Vec<&Json> = msgs
        .iter()
        .filter(|m| {
            m.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics")
        })
        .collect();
    assert_eq!(diags.len(), 2);
    let first = diags[0]
        .get("params")
        .unwrap()
        .get("diagnostics")
        .unwrap()
        .as_arr()
        .unwrap();
    assert_eq!(first.len(), 1, "{}", diags[0]);
    assert_eq!(first[0].get("severity"), Some(&Json::num(1)));
    assert_eq!(
        first[0]
            .get("range")
            .unwrap()
            .get("start")
            .unwrap()
            .get("line"),
        Some(&Json::num(2))
    );
    assert!(diags[1]
        .get("params")
        .unwrap()
        .get("diagnostics")
        .unwrap()
        .as_arr()
        .unwrap()
        .is_empty());
}

#[test]
fn lsp_eof_without_shutdown_exits_one() {
    let mut cur = Cursor::new(Vec::new());
    let mut out = Vec::new();
    assert_eq!(serve(&mut cur, &mut out), 1);
}

#[test]
fn lsp_malformed_json_gets_parse_error() {
    let mut input = Vec::new();
    write_message(&mut input, "{not json").unwrap();
    input.extend_from_slice(&frame(r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#));
    input.extend_from_slice(&frame(r#"{"jsonrpc":"2.0","method":"exit"}"#));
    let mut cur = Cursor::new(input);
    let mut out = Vec::new();
    assert_eq!(serve(&mut cur, &mut out), 0);
    let msgs = bodies(&out);
    assert!(msgs.iter().any(|m| m
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_i64())
        == Some(-32700)));
}
