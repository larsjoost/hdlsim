//! Cross-linked HTML documentation generated from a compiled design and its
//! source files (`hdlsim doc`). Pages are self-contained (inline CSS), source
//! is syntax-highlighted from the lexer token stream, and instances link to
//! the page of the instantiated unit when that unit is part of the design.

use crate::ir::{Design, Dir, ModuleIr};
use hdl_syntax::language::Language;
use hdl_syntax::source::{FileId, SourceFile};
use hdl_syntax::token::{Token, TokenKind};
use hdl_syntax::{verilog, vhdl};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One documented unit (module/entity) and where its page lives.
#[derive(Clone, Debug)]
pub struct DocUnit {
    pub name: String,
    pub lang: Language,
    pub file: String,
    pub line: u32,
    pub page: String,
}

/// Result of [`generate_docs`].
#[derive(Debug)]
pub struct DocSummary {
    pub out_dir: PathBuf,
    pub units: Vec<DocUnit>,
    pub pages: Vec<PathBuf>,
}

struct FileInfo {
    path: PathBuf,
    lang: Language,
    text: String,
    /// (unit name, 1-based declaration line) for every `module`/`entity`.
    decls: Vec<(String, u32)>,
}

/// Escape the five HTML-significant characters.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn token_class(k: TokenKind) -> &'static str {
    match k {
        TokenKind::Ident => "id",
        TokenKind::IntLiteral | TokenKind::RealLiteral => "num",
        TokenKind::StrLiteral | TokenKind::CharLiteral => "str",
        TokenKind::SysTask => "sys",
        TokenKind::Directive => "dir",
        TokenKind::Comment => "com",
        TokenKind::Punct => "pun",
        TokenKind::Error => "err",
        TokenKind::Whitespace | TokenKind::Eof => "",
    }
}

/// Syntax-highlight source text: one `<span>` per non-whitespace token.
pub fn highlight(lang: Language, text: &str) -> String {
    let file = SourceFile::new("<doc>", text);
    let (tokens, _diags) = match lang {
        Language::Verilog => verilog::lexer::lex(FileId(0), &file),
        Language::Vhdl => vhdl::lexer::lex(FileId(0), &file),
    };
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    for t in &tokens {
        if t.kind == TokenKind::Eof {
            continue;
        }
        let txt = &text[t.span.start as usize..t.span.end as usize];
        let class = token_class(t.kind);
        if class.is_empty() {
            out.push_str(&html_escape(txt));
        } else {
            out.push_str("<span class=\"");
            out.push_str(class);
            out.push_str("\">");
            out.push_str(&html_escape(txt));
            out.push_str("</span>");
        }
    }
    out
}

fn lex_source(lang: Language, file: &SourceFile) -> Vec<Token> {
    match lang {
        Language::Verilog => verilog::lexer::lex(FileId(0), file).0,
        Language::Vhdl => vhdl::lexer::lex(FileId(0), file).0,
    }
}

/// Read and lex one source file, collecting its unit declarations.
fn load_file(path: &Path) -> Option<FileInfo> {
    let lang = Language::from_path(path)?;
    let text = fs::read_to_string(path).ok()?;
    let file = SourceFile::new(path.display().to_string(), text.clone());
    let tokens = lex_source(lang, &file);
    let kw = match lang {
        Language::Verilog => "module",
        Language::Vhdl => "entity",
    };
    let mut decls = Vec::new();
    for i in 0..tokens.len() {
        let t = &tokens[i];
        if t.kind != TokenKind::Ident {
            continue;
        }
        let txt = &text[t.span.start as usize..t.span.end as usize];
        let is_kw = match lang {
            Language::Verilog => txt == kw,
            Language::Vhdl => txt.eq_ignore_ascii_case(kw),
        };
        if !is_kw {
            continue;
        }
        let mut j = i + 1;
        while j < tokens.len() && tokens[j].kind == TokenKind::Whitespace {
            j += 1;
        }
        if j < tokens.len() && tokens[j].kind == TokenKind::Ident {
            let n = &tokens[j];
            let name = text[n.span.start as usize..n.span.end as usize].to_string();
            decls.push((name, file.line_col(n.span.start).0));
        }
    }
    Some(FileInfo {
        path: path.to_path_buf(),
        lang,
        text,
        decls,
    })
}

fn page_name(name: &str, used: &mut HashMap<String, usize>) -> String {
    let mut base: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if base.is_empty() {
        base.push_str("unit");
    }
    let n = used.entry(base.clone()).or_insert(0);
    *n += 1;
    if *n == 1 {
        format!("{}.html", base)
    } else {
        format!("{}-{}.html", base, *n)
    }
}

fn dir_str(d: Dir) -> &'static str {
    match d {
        Dir::In => "in",
        Dir::Out => "out",
        Dir::Inout => "inout",
    }
}

const CSS: &str = "body{font-family:ui-monospace,Menlo,Consolas,monospace;margin:2em;background:#1e1e2e;color:#cdd6f4}
a{color:#89b4fa}h1{border-bottom:1px solid #45475a;padding-bottom:.2em}
table{border-collapse:collapse;margin:1em 0}td,th{border:1px solid #45475a;padding:.25em .8em;text-align:left}
th{background:#313244}.meta{color:#a6adc8}pre.src{background:#181825;border:1px solid #45475a;padding:1em;overflow:auto}
.id{color:#cdd6f4}.num{color:#fab387}.str{color:#a6e3a1}.sys{color:#f9e2af}.dir{color:#f38ba8}.com{color:#6c7086;font-style:italic}.pun{color:#9399b2}.err{color:#f38ba8;text-decoration:underline}";

fn shell(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{}</title><style>{}</style></head><body>{}</body></html>\n",
        html_escape(title),
        CSS,
        body
    )
}

fn index_page(design: &Design, units: &[DocUnit]) -> String {
    let mut body = String::from("<h1>hdlsim design documentation</h1>\n<p class=\"meta\">");
    body.push_str(&format!("{} unit(s)</p>\n<table>\n<tr><th>Unit</th><th>Language</th><th>Source</th><th>Ports</th><th>Params</th><th>Instances</th></tr>\n", units.len()));
    let mut rows: Vec<(String, &DocUnit)> =
        units.iter().map(|u| (u.name.to_lowercase(), u)).collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, u) in rows {
        let m = design.modules.iter().find(|m| m.name == u.name).unwrap();
        body.push_str(&format!(
            "<tr><td><a href=\"{}\">{}</a></td><td>{}</td><td>{}:{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            html_escape(&u.page),
            html_escape(&u.name),
            u.lang.name(),
            html_escape(&u.file),
            u.line,
            m.ports.len(),
            m.params.len(),
            m.instances.len()
        ));
    }
    body.push_str("</table>\n");
    shell("hdlsim docs", &body)
}

fn module_page(
    m: &ModuleIr,
    u: &DocUnit,
    page_of: &HashMap<String, String>,
    src_html: &str,
) -> String {
    let mut body = format!(
        "<h1>{}</h1>\n<p class=\"meta\">{} &middot; {}:{} &middot; <a href=\"index.html\">index</a></p>\n",
        html_escape(&m.name),
        u.lang.name(),
        html_escape(&u.file),
        u.line
    );
    body.push_str("<h2>Parameters</h2>\n");
    if m.params.is_empty() {
        body.push_str("<p class=\"meta\">none</p>\n");
    } else {
        body.push_str("<table><tr><th>Name</th><th>Default</th></tr>\n");
        for p in &m.params {
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td></tr>\n",
                html_escape(&p.name),
                html_escape(&p.default.display("d"))
            ));
        }
        body.push_str("</table>\n");
    }
    body.push_str("<h2>Ports</h2>\n");
    if m.ports.is_empty() {
        body.push_str("<p class=\"meta\">none</p>\n");
    } else {
        body.push_str("<table><tr><th>Name</th><th>Direction</th><th>Width</th></tr>\n");
        for p in &m.ports {
            let width = m.signals.get(p.sig as usize).map(|s| s.width).unwrap_or(0);
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                html_escape(&p.name),
                dir_str(p.dir),
                width
            ));
        }
        body.push_str("</table>\n");
    }
    body.push_str("<h2>Instances</h2>\n");
    if m.instances.is_empty() {
        body.push_str("<p class=\"meta\">none</p>\n");
    } else {
        body.push_str("<table><tr><th>Instance</th><th>Unit</th></tr>\n");
        for inst in &m.instances {
            let unit_cell = match page_of.get(&inst.module.to_lowercase()) {
                Some(page) => format!(
                    "<a href=\"{}\">{}</a>",
                    html_escape(page),
                    html_escape(&inst.module)
                ),
                None => html_escape(&inst.module),
            };
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td></tr>\n",
                html_escape(&inst.name),
                unit_cell
            ));
        }
        body.push_str("</table>\n");
    }
    body.push_str("<h2>Source</h2>\n<pre class=\"src\">");
    body.push_str(src_html);
    body.push_str("</pre>\n");
    shell(&format!("hdlsim docs \u{2014} {}", m.name), &body)
}

/// Generate the HTML docs for `design` into `out_dir`. `files` are the source
/// files of the compilation (used for declaration lines and highlighting).
pub fn generate_docs(
    design: &Design,
    files: &[PathBuf],
    out_dir: &Path,
) -> Result<DocSummary, String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("cannot create {}: {}", out_dir.display(), e))?;
    let infos: Vec<FileInfo> = files.iter().filter_map(|p| load_file(p)).collect();
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut units = Vec::with_capacity(design.modules.len());
    let mut file_idx: Vec<Option<usize>> = Vec::with_capacity(design.modules.len());
    for m in &design.modules {
        let mut found: Option<(usize, u32)> = None;
        for (fi, info) in infos.iter().enumerate() {
            let hit = info.decls.iter().find(|(n, _)| match info.lang {
                Language::Verilog => n == &m.name,
                Language::Vhdl => n.eq_ignore_ascii_case(&m.name),
            });
            if let Some((_, line)) = hit {
                found = Some((fi, *line));
                break;
            }
        }
        let (lang, file_str, line, fi) = match found {
            Some((fi, line)) => (
                infos[fi].lang,
                infos[fi].path.display().to_string(),
                line,
                Some(fi),
            ),
            None => (Language::Verilog, String::from("?"), 0, None),
        };
        let page = page_name(&m.name, &mut used);
        units.push(DocUnit {
            name: m.name.clone(),
            lang,
            file: file_str,
            line,
            page: page.clone(),
        });
        file_idx.push(fi);
    }
    let page_of: HashMap<String, String> = units
        .iter()
        .map(|u| (u.name.to_lowercase(), u.page.clone()))
        .collect();
    let mut pages = Vec::new();
    for (i, m) in design.modules.iter().enumerate() {
        let u = &units[i];
        let src_html = match file_idx[i] {
            Some(fi) => highlight(infos[fi].lang, &infos[fi].text),
            None => String::new(),
        };
        let html = module_page(m, u, &page_of, &src_html);
        let path = out_dir.join(&u.page);
        fs::write(&path, html).map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
        pages.push(path);
    }
    let index = out_dir.join("index.html");
    fs::write(&index, index_page(design, &units))
        .map_err(|e| format!("cannot write {}: {}", index.display(), e))?;
    pages.push(index);
    Ok(DocSummary {
        out_dir: out_dir.to_path_buf(),
        units,
        pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{ParamIr, PortIr, SigKind, SignalIr};
    use crate::value::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!(
            "hdlsim_ir_doc_{}_{}_{}",
            std::process::id(),
            tag,
            n
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn counter_module() -> ModuleIr {
        ModuleIr {
            name: "counter".to_string(),
            params: vec![ParamIr {
                name: "WIDTH".to_string(),
                default: Value::from_u64(8, 8),
            }],
            ports: vec![
                PortIr {
                    name: "clk".to_string(),
                    dir: Dir::In,
                    sig: 0,
                },
                PortIr {
                    name: "q".to_string(),
                    dir: Dir::Out,
                    sig: 1,
                },
            ],
            signals: vec![
                SignalIr {
                    name: "clk".to_string(),
                    width: 1,
                    kind: SigKind::Input,
                    init: None,
                    mem: None,
                    signed: false,
                },
                SignalIr {
                    name: "q".to_string(),
                    width: 8,
                    kind: SigKind::Output,
                    init: None,
                    mem: None,
                    signed: false,
                },
            ],
            assigns: Vec::new(),
            processes: Vec::new(),
            functions: Vec::new(),
            instances: Vec::new(),
            time_unit_fs: 1_000,
        }
    }

    #[test]
    fn escape_covers_html_significant_chars() {
        assert_eq!(
            html_escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    #[test]
    fn highlight_wraps_tokens_and_escapes() {
        let html = highlight(Language::Verilog, "module m; // <c>\nendmodule\n");
        assert!(
            html.contains("<span class=\"id\">module</span>"),
            "{}",
            html
        );
        assert!(
            html.contains("<span class=\"com\">// &lt;c&gt;</span>"),
            "{}",
            html
        );
        assert!(!html.contains("<c>"), "{}", html);
        let vhdl_html = highlight(Language::Vhdl, "entity e is\nend entity; -- <x>\n");
        assert!(vhdl_html.contains("&lt;x&gt;"), "{}", vhdl_html);
    }

    #[test]
    fn generate_docs_writes_index_and_module_pages() {
        let d = tmpdir("gen");
        let v = d.join("counter.v");
        fs::write(
            &v,
            "module counter #(parameter WIDTH = 8) (input clk, output reg [WIDTH-1:0] q);\nendmodule\n",
        )
        .unwrap();
        let vhd = d.join("e.vhd");
        fs::write(&vhd, "entity ent is\nend entity;\n").unwrap();
        let mut design = Design::default();
        design.modules.push(counter_module());
        design.modules.push(ModuleIr {
            name: "ENT".to_string(),
            ..counter_module()
        });
        let out = d.join("html");
        let summary = generate_docs(&design, &[v.clone(), vhd.clone()], &out).unwrap();
        assert_eq!(summary.units.len(), 2);
        assert_eq!(summary.pages.len(), 3);
        let index = fs::read_to_string(out.join("index.html")).unwrap();
        assert!(index.contains("href=\"counter.html\""), "{}", index);
        assert!(index.contains("href=\"ENT.html\""), "{}", index);
        assert!(index.contains("counter.v:1"), "{}", index);
        assert!(index.contains("e.vhd:1"), "{}", index);
        let page = fs::read_to_string(out.join("counter.html")).unwrap();
        assert!(page.contains("<h1>counter</h1>"), "{}", page);
        assert!(
            page.contains("<td>clk</td><td>in</td><td>1</td>"),
            "{}",
            page
        );
        assert!(
            page.contains("<td>q</td><td>out</td><td>8</td>"),
            "{}",
            page
        );
        assert!(page.contains("<td>WIDTH</td><td>8</td>"), "{}", page);
        assert!(page.contains("module"), "{}", page);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn instance_links_to_known_unit_page() {
        let d = tmpdir("link");
        let v = d.join("top.v");
        fs::write(&v, "module top;\nendmodule\n").unwrap();
        let mut top = counter_module();
        top.name = "top".to_string();
        top.instances.push(crate::ir::InstIr {
            module: "counter".to_string(),
            name: "dut".to_string(),
            param_overrides: Vec::new(),
            conns: Vec::new(),
        });
        let mut design = Design::default();
        design.modules.push(top);
        design.modules.push(counter_module());
        let out = d.join("html");
        generate_docs(&design, &[v], &out).unwrap();
        let page = fs::read_to_string(out.join("top.html")).unwrap();
        assert!(
            page.contains("<a href=\"counter.html\">counter</a>"),
            "{}",
            page
        );
        let _ = fs::remove_dir_all(&d);
    }
}
