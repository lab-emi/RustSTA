module old_style(
  input wire clk,
  input wire reset,
  input wire [3:0] a,
  input wire [3:0] b,
  input wire sel,
  output wire [3:0] y,
  output wire [3:0] q
);
  wire \q[0] ;
  wire \q[1] ;
  wire \q[2] ;
  wire \q[3] ;
  wire n0;
  wire n1;
  wire n2;
  wire n3;
  wire n4;
  wire n5;
  wire n6;
  wire n7;
  wire n8;
  wire n9;
  wire n10;
  wire n11;
  wire n12;
  wtf_mux2_1 g0 (.A0(b[0]), .A1(a[0]), .S(sel), .X(n0));
  wtf_mux2_1 g1 (.A0(b[1]), .A1(a[1]), .S(sel), .X(n1));
  wtf_mux2_1 g2 (.A0(b[2]), .A1(a[2]), .S(sel), .X(n2));
  wtf_mux2_1 g3 (.A0(b[3]), .A1(a[3]), .S(sel), .X(n3));
  wtf_and2_4 g4 (.A(a[0]), .B(b[0]), .X(n4));
  wtf_xnor2_4 g5 (.A(sel), .B(n4), .Y(n5));
  wtf_nor2_4 g6 (.A(reset), .B(n5), .Y(n6));
  wtf_nor2_1 g7 (.A(reset), .B(n10), .Y(n7));
  wtf_nor2_1 g8 (.A(reset), .B(n11), .Y(n8));
  wtf_nor2_1 g9 (.A(reset), .B(n12), .Y(n9));
  wtf_dff_1 r0 (.D(n6), .CLK(clk), .Q(\q[0] ), .Q_N(n10));
  wtf_dff_1 r1 (.D(n7), .CLK(clk), .Q(\q[1] ), .Q_N(n11));
  wtf_dff_1 r2 (.D(n8), .CLK(clk), .Q(\q[2] ), .Q_N(n12));
  wtf_dff_1 r3 (.D(n9), .CLK(clk), .Q(\q[3] ));
  assign y[0] = n0;
  assign y[1] = n1;
  assign y[2] = n2;
  assign y[3] = n3;
  assign q[0] = \q[0] ;
  assign q[1] = \q[1] ;
  assign q[2] = \q[2] ;
  assign q[3] = \q[3] ;
endmodule
