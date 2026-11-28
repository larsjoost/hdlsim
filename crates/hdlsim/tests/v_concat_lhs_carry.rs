//! Regression test for concatenation-LHS assignment context (LRM 11.4.14 /
//! 11.6): `{carry, sum} = a + b` evaluates the RHS in the context of the LHS
//! width, so 8-bit operands add at 9 bits and bit 8 is the carry. The carry
//! used to come out inverted (bit 0 of an 8-bit evaluation): 100 + 55 yielded
//! carry 1 and a sum of 300 yielded carry 0.

use hdlsim::commands::dispatch;

const TB: &str = r#"`timescale 1ns/1ps
module tb_concat_carry;
    reg [7:0] a, b;
    reg c_blk;   reg [7:0] s_blk;
    reg c_nba0;  reg [7:0] s_nba0;
    reg c_nba1;  reg [7:0] s_nba1;
    reg c_mul;   reg [7:0] s_mul;
    reg c_sgn;   reg [7:0] s_sgn;
    reg c_low;   reg [3:0] s_low;
    reg signed [7:0] sa, sb;
    wire c_cnt;  wire [7:0] s_cnt;

    // Continuous-assign form of the same construct.
    assign {c_cnt, s_cnt} = a + b;

    initial begin
        // Blocking: 100 + 55 = 155 = 9'b0_1001_1011 — carry 0.
        a = 8'd100; b = 8'd55;
        {c_blk, s_blk} = a + b;
        // Blocking: 200 + 100 = 300 = 9'b1_0010_1100 — carry 1, sum 44.
        a = 8'd200; b = 8'd100;
        {c_blk, s_blk} = a + b;
        // Nonblocking: each RHS is evaluated when the statement executes.
        a = 8'd100; b = 8'd55;
        {c_nba0, s_nba0} <= a + b;
        a = 8'd200; b = 8'd100;
        {c_nba1, s_nba1} <= a + b;
        // Multiply: 16 * 16 = 256 = 9'b1_0000_0000 — carry 1, sum 0.
        a = 8'd16; b = 8'd16;
        {c_mul, s_mul} = a * b;
        // Signed operands: -2 + 0 stays a signed add extended to 9 bits —
        // 9'b1_1111_1110 — carry 1, sum 8'hFE. The LHS width sets the size;
        // the operand types still set the signedness.
        sa = -2; sb = 0;
        {c_sgn, s_sgn} = signed'(sa) + signed'(sb);
        // Narrow LHS (5-bit target from 8-bit operands): the RHS's own
        // self-determined size (8) wins, so the add stays 8-bit and the
        // 1-bit part takes bit 4 of 155 = 1.
        a = 8'd100; b = 8'd55;
        {c_low, s_low} = a + b;
        // Leave a + b at the carry-producing values for the wire check.
        a = 8'd200; b = 8'd100;
        #1;
        if (c_blk !== 1'b1 || s_blk !== 8'd44)
            $error("FAIL blocking carry c=%b s=%d", c_blk, s_blk);
        else if (c_nba0 !== 1'b0 || s_nba0 !== 8'd155)
            $error("FAIL nba no-carry c=%b s=%d", c_nba0, s_nba0);
        else if (c_nba1 !== 1'b1 || s_nba1 !== 8'd44)
            $error("FAIL nba carry c=%b s=%d", c_nba1, s_nba1);
        else if (c_mul !== 1'b1 || s_mul !== 8'd0)
            $error("FAIL multiply carry c=%b s=%d", c_mul, s_mul);
        else if (c_sgn !== 1'b1 || s_sgn !== 8'hFE)
            $error("FAIL signed c=%b s=%h", c_sgn, s_sgn);
        else if (c_low !== 1'b1 || s_low !== 4'b1011)
            $error("FAIL narrow-lhs c=%b s=%b", c_low, s_low);
        else if (c_cnt !== 1'b1 || s_cnt !== 8'd44)
            $error("FAIL continuous c=%b s=%d", c_cnt, s_cnt);
        else
            $display("PASS concat carry");
        $finish;
    end
endmodule
"#;

#[test]
fn concat_lhs_add_context_carry() {
    // Library use never enables the parse cache (that is CLI-only, --no-cache);
    let dir = std::env::temp_dir().join(format!("hdlsim_concat_carry_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tb_concat_carry.v");
    std::fs::write(&f, TB).unwrap();
    let code = dispatch(&[
        "run".to_string(),
        "--top".to_string(),
        "tb_concat_carry".to_string(),
        f.to_string_lossy().to_string(),
    ])
    .unwrap();
    assert_eq!(code, 0, "concat-LHS carry tb must pass (exit 0)");
    let _ = std::fs::remove_dir_all(&dir);
}
