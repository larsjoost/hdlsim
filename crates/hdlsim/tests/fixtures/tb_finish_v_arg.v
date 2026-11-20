module tb_finish_v_arg;
    initial begin
        $display("PASS: verilog finish arg fixture");
        // LRM 1364: the $finish argument is a diagnostic level (0/1/2),
        // not an exit status — it must not change the process exit code.
        $finish(1);
    end
endmodule
