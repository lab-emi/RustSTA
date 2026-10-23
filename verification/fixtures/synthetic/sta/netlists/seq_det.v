module seq_det(
  input wire clk,
  input wire rst_n,
  input wire din,
  output wire hit
);
  wire \state_q[0] ;
  wire \state_q[1] ;
  wire n0;
  wire n1;
  wire n2;
  wtf_and2_2 g0 (.A(\state_q[0] ), .B(\state_q[1] ), .X(n0));
  wtf_nor2b_4 g1 (.A(\state_q[0] ), .B_N(\state_q[1] ), .Y(n1));
  wtf_mux2_4 g2 (.A0(\state_q[0] ), .A1(n1), .S(din), .X(n2));
  wtf_dffrq_1 r0 (.D(din), .CLK(clk), .RESET_B(rst_n), .Q(\state_q[0] ));
  wtf_dffrq_1 r1 (.D(n2), .CLK(clk), .RESET_B(rst_n), .Q(\state_q[1] ));
  assign hit = n0;
endmodule
