module tb_fail_error;
    initial begin
        $error("FAIL: verilog $error fixture");
        $finish;
    end
endmodule
