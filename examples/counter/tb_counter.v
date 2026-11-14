// Self-checking testbench for counter: drives clk/rst, checks q every cycle.
`timescale 1ns/1ps
module tb_counter;
    reg clk = 0;
    reg rst = 1;
    wire [7:0] q;

    counter dut (.clk(clk), .rst(rst), .q(q));

    always #5 clk = ~clk;

    integer errors = 0;
    integer i;

    initial begin
        // Hold reset for two clocks.
        @(posedge clk);
        @(posedge clk);
        rst = 0;
        for (i = 0; i < 10; i = i + 1) begin
            @(posedge clk);
            #1; // let the NBA update land before sampling
            if (q !== (i + 1)) begin
                $display("FAIL: after %0d clocks q=%h expected %0d", i + 1, q, i + 1);
                errors = errors + 1;
            end
        end
        if (errors == 0)
            $display("PASS: counter counted 1..10 correctly (q=%h)", q);
        else
            $display("FAIL: %0d errors", errors);
        $finish;
    end
endmodule
