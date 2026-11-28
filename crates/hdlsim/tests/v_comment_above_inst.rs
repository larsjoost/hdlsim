//! Regression test for design-unit resolution with a comment directly above
//! an instantiation: `bump` attaches pending trivia to the node being
//! started, so the module-name `Name` node of an instantiation absorbed a
//! preceding `//` comment into its text and elaboration tried to resolve a
//! design unit literally named the comment (`error: design unit '// …'`).

use hdlsim::commands::dispatch;

const ADDER: &str = r#"module widen_add(input [7:0] x, input [7:0] y, output [7:0] z);
    assign z = x + y;
endmodule
"#;

const TOP: &str = r#"`timescale 1ns/1ps
module tb_comment_inst;
    reg [7:0] p, q;
    wire [7:0] r1, r2, r3;
    // comment directly above the instantiation
    widen_add u0(.x(p), .y(q), .z(r1));
    /* block comment directly above the instantiation */
    widen_add u1(.x(p), .y(q), .z(r2));
    // one more above a parameterless-shape instantiation
    widen_add u2(.x(q), .y(p), .z(r3));
    initial begin
        p = 8'd1; q = 8'd2;
        #1;
        if (r1 !== 8'd3 || r2 !== 8'd3 || r3 !== 8'd3)
            $error("FAIL r1=%d r2=%d r3=%d", r1, r2, r3);
        else
            $display("PASS comment above instantiation");
        $finish;
    end
endmodule
"#;

#[test]
fn comment_above_instantiation_resolves_the_unit() {
    // Library use never enables the parse cache (that is CLI-only, --no-cache);
    let dir = std::env::temp_dir().join(format!("hdlsim_comment_inst_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let top = dir.join("tb_comment_inst.v");
    let sub = dir.join("widen_add.v");
    std::fs::write(&top, TOP).unwrap();
    std::fs::write(&sub, ADDER).unwrap();
    let code = dispatch(&[
        "run".to_string(),
        "--top".to_string(),
        "tb_comment_inst".to_string(),
        top.to_string_lossy().to_string(),
        sub.to_string_lossy().to_string(),
    ])
    .unwrap();
    assert_eq!(code, 0, "comment-above-instantiation tb must pass (exit 0)");
    let _ = std::fs::remove_dir_all(&dir);
}
