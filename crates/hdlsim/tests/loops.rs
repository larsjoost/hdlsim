//! Loop-construct semantics: while / repeat / forever / do-while must all
//! actually execute in the kernel (regression test: these parsed but were
//! silently dropped by the lowerer).

use hdlsim::commands::dispatch;

const TB: &str = r#"`timescale 1ns/1ps
module tb_loops;
    reg clk = 0;
    always #5 clk = ~clk;
    integer r = 0, w = 0, d = 0, f = 0, i;
    initial begin
        // repeat: 4 iterations
        repeat (4) r = r + 1;
        // while: until 5
        while (w < 5) w = w + 1;
        // do-while: runs at least once
        d = 0;
        do d = d + 1; while (d < 3);
        // forever: break after 3
        forever begin
            f = f + 1;
            if (f == 3) break;
        end
        if (r == 4 && w == 5 && d == 3 && f == 3)
            $display("PASS loops r=%0d w=%0d d=%0d f=%0d", r, w, d, f);
        else
            $error("FAIL loops r=%0d w=%0d d=%0d f=%0d", r, w, d, f);
        $finish;
    end
endmodule
"#;

#[test]
fn while_repeat_forever_do_while_execute() {
    std::env::set_var("HDLSIM_NO_CACHE", "1");
    let dir = std::env::temp_dir().join(format!("hdlsim_loops_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tb_loops.v");
    std::fs::write(&f, TB).unwrap();
    let code = dispatch(&[
        "run".to_string(),
        "--top".to_string(),
        "tb_loops".to_string(),
        f.to_string_lossy().to_string(),
    ])
    .unwrap();
    assert_eq!(code, 0, "loops tb must pass (exit 0)");
    let _ = std::fs::remove_dir_all(&dir);
}
