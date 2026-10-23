module gray(
  input wire clk,
  input wire rst_n,
  input wire [4:0] bin_i,
  output wire [4:0] gray_o,
  output wire [4:0] bin_o,
  output wire [4:0] gcnt_o
);
  wire \b_q[0] ;
  wire \b_q[1] ;
  wire \b_q[2] ;
  wire \b_q[3] ;
  wire \b_q[4] ;
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
  wire n13;
  wire n14;
  wire n15;
  wire n16;
  wire n17;
  wire n18;
  wire n19;
  wire n20;
  wtf_inv_1 g0 (.A(\b_q[2] ), .Y(n0));
  wtf_xor2_1 g1 (.A(bin_i[0]), .B(bin_i[1]), .X(n1));
  wtf_xor2_1 g2 (.A(bin_i[1]), .B(bin_i[2]), .X(n2));
  wtf_xor2_1 g3 (.A(bin_i[2]), .B(bin_i[3]), .X(n3));
  wtf_xnor2_4 g4 (.A(bin_i[3]), .B(bin_i[4]), .Y(n4));
  wtf_inv_1 g5 (.A(n4), .Y(n5));
  wtf_xor2_4 g6 (.A(bin_i[2]), .B(n4), .X(n6));
  wtf_inv_1 g7 (.A(n6), .Y(n7));
  wtf_xor2_4 g8 (.A(bin_i[1]), .B(n6), .X(n8));
  wtf_xnor2_1 g9 (.A(bin_i[1]), .B(n6), .Y(n9));
  wtf_xnor2_4 g10 (.A(bin_i[0]), .B(n8), .Y(n10));
  wtf_nand2_1 g11 (.A(\b_q[0] ), .B(\b_q[1] ), .Y(n11));
  wtf_o21a_2 g12 (.A1(\b_q[0] ), .A2(\b_q[1] ), .B1(n11), .X(n12));
  wtf_xor2_1 g13 (.A(\b_q[1] ), .B(\b_q[2] ), .X(n13));
  wtf_xor2_1 g14 (.A(\b_q[2] ), .B(\b_q[3] ), .X(n14));
  wtf_xor2_1 g15 (.A(\b_q[3] ), .B(\b_q[4] ), .X(n15));
  wtf_and3_1 g16 (.A(\b_q[0] ), .B(\b_q[1] ), .C(\b_q[2] ), .X(n16));
  wtf_a21oi_2 g17 (.A1(n0), .A2(n11), .B1(n16), .Y(n17));
  wtf_and4_1 g18 (.A(\b_q[0] ), .B(\b_q[1] ), .C(\b_q[2] ), .D(\b_q[3] ), .X(n18));
  wtf_xor2_2 g19 (.A(\b_q[3] ), .B(n16), .X(n19));
  wtf_xor2_2 g20 (.A(\b_q[4] ), .B(n18), .X(n20));
  wtf_dffs_1 r0 (.D(\b_q[0] ), .CLK(clk), .SET_B(rst_n), .Q_N(\b_q[0] ));
  wtf_dffrq_1 r1 (.D(n12), .CLK(clk), .RESET_B(rst_n), .Q(\b_q[1] ));
  wtf_dffrq_1 r2 (.D(n17), .CLK(clk), .RESET_B(rst_n), .Q(\b_q[2] ));
  wtf_dffrq_1 r3 (.D(n19), .CLK(clk), .RESET_B(rst_n), .Q(\b_q[3] ));
  wtf_dffrq_1 r4 (.D(n20), .CLK(clk), .RESET_B(rst_n), .Q(\b_q[4] ));
  assign gray_o[0] = n1;
  assign gray_o[1] = n2;
  assign gray_o[2] = n3;
  assign gray_o[3] = n5;
  assign gray_o[4] = bin_i[4];
  assign bin_o[0] = n10;
  assign bin_o[1] = n9;
  assign bin_o[2] = n7;
  assign bin_o[3] = n5;
  assign bin_o[4] = bin_i[4];
  assign gcnt_o[0] = n12;
  assign gcnt_o[1] = n13;
  assign gcnt_o[2] = n14;
  assign gcnt_o[3] = n15;
  assign gcnt_o[4] = \b_q[4] ;
endmodule
