module set_neg(
  input wire clk,
  input wire set,
  input wire rst_n,
  input wire [1:0] d,
  output wire [1:0] q,
  output wire [1:0] qn
);
  wire \q[0] ;
  wire \q[1] ;
  wire \qn[0] ;
  wire \qn[1] ;
  wire n0;
  wire n1;
  wire n2;
  wire n3;
  wtf_inv_1 g0 (.A(clk), .Y(n0));
  wtf_inv_1 g1 (.A(set), .Y(n1));
  wtf_xnor2_1 g2 (.A(d[0]), .B(\q[0] ), .Y(n2));
  wtf_xor2_1 g3 (.A(d[1]), .B(\q[1] ), .X(n3));
  wtf_dffs_1 r0 (.D(d[0]), .CLK(clk), .SET_B(n1), .Q(\q[0] ));
  wtf_dffs_1 r1 (.D(d[1]), .CLK(clk), .SET_B(n1), .Q(\q[1] ));
  wtf_dffr_1 r2 (.D(n2), .CLK(n0), .RESET_B(rst_n), .Q_N(\qn[0] ));
  wtf_dffrq_1 r3 (.D(n3), .CLK(n0), .RESET_B(rst_n), .Q(\qn[1] ));
  assign q[0] = \q[0] ;
  assign q[1] = \q[1] ;
  assign qn[0] = \qn[0] ;
  assign qn[1] = \qn[1] ;
endmodule
