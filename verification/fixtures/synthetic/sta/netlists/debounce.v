module debounce(
  input wire clk,
  input wire rst_n,
  input wire btn,
  output wire level,
  output wire pressed
);
  wire \cnt[0] ;
  wire \cnt[1] ;
  wire \cnt[2] ;
  wire last;
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
  wtf_inv_1 g0 (.A(level), .Y(n0));
  wtf_inv_1 g1 (.A(\cnt[0] ), .Y(n1));
  wtf_xnor2_1 g2 (.A(btn), .B(last), .Y(n2));
  wtf_xor2_1 g3 (.A(btn), .B(last), .X(n3));
  wtf_nand2_2 g4 (.A(level), .B(n3), .Y(n4));
  wtf_nand2_2 g5 (.A(\cnt[0] ), .B(\cnt[2] ), .Y(n5));
  wtf_nor2_4 g6 (.A(\cnt[1] ), .B(n5), .Y(n6));
  wtf_nand2_4 g7 (.A(btn), .B(n6), .Y(n7));
  wtf_o21ai_2 g8 (.A1(\cnt[1] ), .A2(n5), .B1(level), .Y(n8));
  wtf_a21o_4 g9 (.A1(n7), .A2(n8), .B1(n3), .X(n9));
  wtf_nand2_4 g10 (.A(n4), .B(n9), .Y(n10));
  wtf_and4_1 g11 (.A(btn), .B(n0), .C(last), .D(n6), .X(n11));
  wtf_o21a_1 g12 (.A1(n1), .A2(n6), .B1(n2), .X(n12));
  wtf_a21oi_1 g13 (.A1(\cnt[0] ), .A2(n5), .B1(\cnt[1] ), .Y(n13));
  wtf_a21o_2 g14 (.A1(\cnt[0] ), .A2(\cnt[1] ), .B1(n3), .X(n14));
  wtf_nor2_2 g15 (.A(n13), .B(n14), .Y(n15));
  wtf_and3_1 g16 (.A(\cnt[0] ), .B(\cnt[1] ), .C(\cnt[2] ), .X(n16));
  wtf_a21oi_1 g17 (.A1(\cnt[0] ), .A2(\cnt[1] ), .B1(\cnt[2] ), .Y(n17));
  wtf_nor3_1 g18 (.A(n3), .B(n16), .C(n17), .Y(n18));
  wtf_dffrq_1 r0 (.D(n10), .CLK(clk), .RESET_B(rst_n), .Q(level));
  wtf_dffrq_1 r1 (.D(n11), .CLK(clk), .RESET_B(rst_n), .Q(pressed));
  wtf_dffrq_1 r2 (.D(n12), .CLK(clk), .RESET_B(rst_n), .Q(\cnt[0] ));
  wtf_dffrq_1 r3 (.D(n15), .CLK(clk), .RESET_B(rst_n), .Q(\cnt[1] ));
  wtf_dffrq_1 r4 (.D(n18), .CLK(clk), .RESET_B(rst_n), .Q(\cnt[2] ));
  wtf_dffrq_1 r5 (.D(btn), .CLK(clk), .RESET_B(rst_n), .Q(last));
endmodule
