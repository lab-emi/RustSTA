module sync_edge(
  input wire clk,
  input wire rst_n,
  input wire async_i,
  output wire rise_o,
  output wire fall_o,
  output wire level_o
);
  wire \sync_q[0] ;
  wire \sync_q[1] ;
  wire \sync_q[2] ;
  wire n0;
  wire n1;
  wtf_nor2b_4 g0 (.A(\sync_q[2] ), .B_N(\sync_q[1] ), .Y(n0));
  wtf_nor2b_4 g1 (.A(\sync_q[1] ), .B_N(\sync_q[2] ), .Y(n1));
  wtf_dffrq_1 r0 (.D(async_i), .CLK(clk), .RESET_B(rst_n), .Q(\sync_q[0] ));
  wtf_dffrq_1 r1 (.D(\sync_q[0] ), .CLK(clk), .RESET_B(rst_n), .Q(\sync_q[1] ));
  wtf_dffrq_1 r2 (.D(\sync_q[1] ), .CLK(clk), .RESET_B(rst_n), .Q(\sync_q[2] ));
  assign rise_o = n0;
  assign fall_o = n1;
  assign level_o = \sync_q[1] ;
endmodule
