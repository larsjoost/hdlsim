module tb_fail_fatal_v;
    initial begin
        $fatal(1, "FAIL: verilog $fatal fixture");
    end
endmodule
