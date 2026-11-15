//! `hdlsim doc` must emit a self-contained HTML docs tree (index + one page
//! per unit) for both languages, with port tables and cross-links.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn outdir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("hdlsim_doc_{}_{}_{}", std::process::id(), tag, n));
    d
}

#[test]
fn doc_verilog_counter() {
    let out = outdir("v");
    let counter = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/counter/counter.v"
    );
    let tb = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/counter/tb_counter.v"
    );
    let r = hdlsim()
        .args(["doc", counter, tb, "--out"])
        .arg(&out)
        .output()
        .expect("runs");
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let index = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(index.contains("href=\"counter.html\""), "{}", index);
    assert!(index.contains("href=\"tb_counter.html\""), "{}", index);
    let page = std::fs::read_to_string(out.join("counter.html")).unwrap();
    assert!(page.contains("<h1>counter</h1>"), "{}", page);
    assert!(page.contains("<td>clk</td>"), "{}", page);
    // tb instantiates counter -> cross-link on the tb page
    let tbpage = std::fs::read_to_string(out.join("tb_counter.html")).unwrap();
    assert!(
        tbpage.contains("<a href=\"counter.html\">counter</a>"),
        "{}",
        tbpage
    );
    // source is embedded and HTML-escaped
    assert!(page.contains("<pre class=\"src\">"), "{}", page);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn doc_vhdl_counter() {
    let out = outdir("vhd");
    let counter = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vhdl-counter/counter.vhd"
    );
    let tb = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/vhdl-counter/tb_counter.vhd"
    );
    let r = hdlsim()
        .args(["doc", counter, tb, "-o"])
        .arg(&out)
        .output()
        .expect("runs");
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let index = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(index.contains("counter.html"), "{}", index);
    assert!(index.contains("vhdl"), "{}", index);
    let page = std::fs::read_to_string(out.join("counter.html")).unwrap();
    assert!(page.contains("<h1>counter</h1>"), "{}", page);
    assert!(page.contains("<td>clk</td>"), "{}", page);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn doc_requires_files() {
    let r = hdlsim().args(["doc"]).output().expect("runs");
    assert!(!r.status.success());
}
