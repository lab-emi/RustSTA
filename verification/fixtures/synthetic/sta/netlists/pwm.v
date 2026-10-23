module pwm(
  input wire clk,
  input wire rst_n,
  input wire [3:0] duty,
  output wire out
);
  wire \phase[0] ;
  wire \phase[1] ;
  wire \phase[2] ;
  wire \phase[3] ;
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
  wtf_nand2b_1 g0 (.A_N(\phase[3] ), .B(duty[3]), .Y(n0));
  wtf_nor2b_1 g1 (.A(\phase[2] ), .B_N(duty[2]), .Y(n1));
  wtf_nand2b_2 g2 (.A_N(\phase[1] ), .B(duty[1]), .Y(n2));
  wtf_nand2_4 g3 (.A(duty[0]), .B(n16), .Y(n3));
  wtf_nor2b_4 g4 (.A(duty[1]), .B_N(\phase[1] ), .Y(n4));
  wtf_a21oi_4 g5 (.A1(n2), .A2(n3), .B1(n4), .Y(n5));
  wtf_nand2b_1 g6 (.A_N(duty[2]), .B(\phase[2] ), .Y(n6));
  wtf_o21ai_4 g7 (.A1(n1), .A2(n5), .B1(n6), .Y(n7));
  wtf_nor2b_1 g8 (.A(duty[3]), .B_N(\phase[3] ), .Y(n8));
  wtf_a21oi_4 g9 (.A1(n0), .A2(n7), .B1(n8), .Y(n9));
  wtf_and2_1 g10 (.A(\phase[0] ), .B(\phase[1] ), .X(n10));
  wtf_nor2_1 g11 (.A(\phase[0] ), .B(\phase[1] ), .Y(n11));
  wtf_nor2_1 g12 (.A(n10), .B(n11), .Y(n12));
  wtf_and3_1 g13 (.A(\phase[0] ), .B(\phase[1] ), .C(\phase[2] ), .X(n13));
  wtf_xor2_1 g14 (.A(\phase[2] ), .B(n10), .X(n14));
  wtf_xor2_2 g15 (.A(\phase[3] ), .B(n13), .X(n15));
  wtf_dffs_1 r0 (.D(\phase[0] ), .CLK(clk), .SET_B(rst_n), .Q(n16), .Q_N(\phase[0] ));
  wtf_dffrq_1 r1 (.D(n12), .CLK(clk), .RESET_B(rst_n), .Q(\phase[1] ));
  wtf_dffrq_1 r2 (.D(n14), .CLK(clk), .RESET_B(rst_n), .Q(\phase[2] ));
  wtf_dffrq_1 r3 (.D(n15), .CLK(clk), .RESET_B(rst_n), .Q(\phase[3] ));
  assign out = n9;
endmodule
