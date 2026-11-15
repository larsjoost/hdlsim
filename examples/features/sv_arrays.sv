// Dynamic arrays (new[]), queues (push/pop/size), assoc arrays, foreach.
module tb;
  int da[];
  int q[$];
  int aa[string];
  int sum;
  initial begin
    da = new[3];
    da[0] = 1; da[1] = 2; da[2] = 3;
    q.push_back(10); q.push_back(20);
    aa["k"] = 99;
    sum = 0;
    foreach (da[i]) sum = sum + da[i];
    if (da[2] == 3 && q[1] == 20 && q.size() == 2 && aa["k"] == 99 && sum == 6) begin
      q.pop_back();
      if (q.size() == 1) $display("PASS"); else $display("FAIL pop");
    end else $display("FAIL");
    $finish;
  end
endmodule
