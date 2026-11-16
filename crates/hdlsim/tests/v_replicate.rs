//! Regression test for Verilog replication `{N{expr}}`: the Replicate node
//! must own both closing braces, so a replication used directly as a binary
//! operator operand (`a ^ {4{b}}`) does not corrupt the operator token.

use hdlsim::commands::dispatch;

const TB: &str = r#"`timescale 1ns/1ps
module tb_replicate;
    reg [3:0] a = 4'b0101;
    reg b = 1'b1;
    wire [3:0] x, y, z;
    assign x = {4{b}};       // replication alone
    assign y = a ^ {4{b}};   // replication as binary-op operand (regression)
    assign z = {2{a[1:0]}} & 4'b1100;
    initial begin
        #1;
        if (x !== 4'b1111) $error("FAIL replicate x=%b", x);
        else if (y !== (4'b0101 ^ 4'b1111)) $error("FAIL binop+replicate y=%b", y);
        else if (z !== 4'b0100) $error("FAIL concat-replicate z=%b", z);
        else $display("PASS replicate");
        $finish;
    end
endmodule
"#;

#[test]
fn replication_in_binary_operators() {
    std::env::set_var("HDLSIM_NO_CACHE", "1");
    let dir = std::env::temp_dir().join(format!("hdlsim_repl_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tb_replicate.v");
    std::fs::write(&f, TB).unwrap();
    let code = dispatch(&[
        "run".to_string(),
        "--top".to_string(),
        "tb_replicate".to_string(),
        f.to_string_lossy().to_string(),
    ])
    .unwrap();
    assert_eq!(code, 0, "replication tb must pass (exit 0)");
    let _ = std::fs::remove_dir_all(&dir);
}
