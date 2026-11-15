//! Parser/lexer/formatter/linter fuzz: mutated and random inputs must never
//! panic — every failure mode is a Diagnostic. Deterministic (fixed-seed
//! xorshift), so failures reproduce exactly.

use hdl_syntax::cst::{decode_tree, encode_tree};
use hdl_syntax::language::Language;
use hdl_syntax::source::SourceDb;
use hdl_syntax::{fmt, lint, verilog, vhdl};

/// xorshift64* — tiny deterministic PRNG, std-only.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const VERILOG_SEED: &str = "module m (input wire clk, input wire [7:0] d, output reg [7:0] q);\n    always @(posedge clk) begin\n        if (d > 8'd10) q <= d; else q <= 8'h00;\n    end\n    assert property (@(posedge clk) d |=> q);\nendmodule\nmodule tb;\n    reg clk = 0; wire [7:0] q;\n    m dut (.clk(clk), .d(8'd3), .q(q));\n    always #5 clk = ~clk;\n    initial begin #20 $finish; end\nendmodule\n";
const VHDL_SEED: &str = "library ieee;\nuse ieee.std_logic_1164.all;\n\nentity m is\n    port (clk : in std_logic; q : out std_logic_vector(7 downto 0));\nend entity;\n\narchitecture rtl of m is\n    signal cnt : unsigned(7 downto 0) := x\"00\";\nbegin\n    process(clk)\n    begin\n        if rising_edge(clk) then\n            cnt <= cnt + 1;\n        end if;\n    end process;\n    q <= std_logic_vector(cnt);\nend architecture;\n";

/// Interesting byte soup: token-ish characters that stress lexer paths.
const SOUP: &[u8] = b"abcXYZ_09 \t\n\r\"'`$#@/\\*+-<>=!&|^~%?:;,.()[]{}'bhd0123456789\n";

fn mutate(rng: &mut Rng, src: &str) -> String {
    let mut b = src.as_bytes().to_vec();
    let ops = 1 + rng.below(8);
    for _ in 0..ops {
        if b.is_empty() {
            b.push(SOUP[rng.below(SOUP.len())]);
            continue;
        }
        match rng.below(5) {
            0 => {
                // flip a byte
                let i = rng.below(b.len());
                b[i] = SOUP[rng.below(SOUP.len())];
            }
            1 => {
                // insert a soup byte
                let i = rng.below(b.len() + 1);
                b.insert(i, SOUP[rng.below(SOUP.len())]);
            }
            2 => {
                // delete a slice
                let i = rng.below(b.len());
                let n = (1 + rng.below(16)).min(b.len() - i);
                b.drain(i..i + n);
            }
            3 => {
                // duplicate a slice
                let i = rng.below(b.len());
                let n = (1 + rng.below(16)).min(b.len() - i);
                let slice = b[i..i + n].to_vec();
                let j = rng.below(b.len() + 1);
                b.splice(j..j, slice);
            }
            _ => {
                // splice in a random soup run
                let i = rng.below(b.len() + 1);
                let n = 1 + rng.below(24);
                let run: Vec<u8> = (0..n).map(|_| SOUP[rng.below(SOUP.len())]).collect();
                b.splice(i..i, run);
            }
        }
    }
    // Keep it valid UTF-8 (the lexers expect text): lossy-convert.
    String::from_utf8_lossy(&b).into_owned()
}

fn parse(
    lang: Language,
    text: &str,
) -> (hdl_syntax::source::SourceDb, hdl_syntax::cst::SyntaxNode) {
    let mut db = SourceDb::new();
    let id = db.add_file("fuzz.v", text).unwrap();
    let root = match lang {
        Language::Verilog => verilog::parser::parse_file(id, db.file(id)).root,
        Language::Vhdl => vhdl::parser::parse_file(id, db.file(id)).root,
    };
    (db, root)
}

#[test]
fn fuzz_mutated_seeds_never_panic() {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    for i in 0..3000 {
        let (lang, seed) = if i % 2 == 0 {
            (Language::Verilog, VERILOG_SEED)
        } else {
            (Language::Vhdl, VHDL_SEED)
        };
        let text = mutate(&mut rng, seed);
        let (db, root) = parse(lang, &text);
        // CST encode/decode roundtrip must not panic and must decode.
        let bytes = encode_tree(&root);
        let back = decode_tree(hdl_syntax::source::FileId(0), &bytes).expect("decode");
        drop(back);
        // Formatter and linter must not panic either.
        let (out, _diags) = fmt::format_source(lang, &text);
        assert!(
            out.len() <= text.len() + 4096,
            "formatter blew up at iter {}",
            i
        );
        let mut db2 = SourceDb::new();
        let lints = lint::lint_source(lang, &mut db2, "fuzz.v", &text);
        let (_fixed, _) = lint::apply_fixes(&text, &lints);
        drop(db);
    }
}

#[test]
fn fuzz_pure_soup_never_panics() {
    let mut rng = Rng(0xDEADBEEFCAFEF00D);
    for i in 0..2000 {
        let n = rng.below(400);
        let text: String = (0..n)
            .map(|_| SOUP[rng.below(SOUP.len())] as char)
            .collect();
        let lang = if i % 2 == 0 {
            Language::Verilog
        } else {
            Language::Vhdl
        };
        let (_db, root) = parse(lang, &text);
        let bytes = encode_tree(&root);
        let _ = decode_tree(hdl_syntax::source::FileId(0), &bytes);
        let _ = fmt::format_source(lang, &text);
        let mut db2 = SourceDb::new();
        let lints = lint::lint_source(lang, &mut db2, "fuzz.v", &text);
        let _ = lint::apply_fixes(&text, &lints);
    }
}

#[test]
fn fuzz_diagnostics_render_without_panic() {
    let mut rng = Rng(0x1234567890ABCDEF);
    for i in 0..500 {
        let text = mutate(&mut rng, if i % 2 == 0 { VERILOG_SEED } else { VHDL_SEED });
        let lang = if i % 2 == 0 {
            Language::Verilog
        } else {
            Language::Vhdl
        };
        let mut db = SourceDb::new();
        let id = db.add_file("fuzz.v", &text).unwrap();
        let diags = match lang {
            Language::Verilog => verilog::parser::parse_file(id, db.file(id)).diags,
            Language::Vhdl => vhdl::parser::parse_file(id, db.file(id)).diags,
        };
        // Render every diagnostic (rustc-style) — spans must be in-bounds.
        for d in &diags {
            let _ = d.render(&db);
        }
        let mut ds = hdl_syntax::diagnostic::Diagnostics::new();
        ds.extend(diags.iter().cloned());
        let _ = ds.render_all(&db);
    }
}
