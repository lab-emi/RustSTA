module skid(
  input wire clk,
  input wire rst_n,
  input wire in_valid,
  input wire [3:0] in_data,
  output wire in_ready,
  output wire out_valid,
  input wire out_ready,
  output wire [3:0] out_data
);
  wire full_q;
  wire valid_q;
  wire \buf_q[0] ;
  wire \buf_q[1] ;
  wire \buf_q[2] ;
  wire \buf_q[3] ;
  wire \data_q[0] ;
  wire \data_q[1] ;
  wire \data_q[2] ;
  wire \data_q[3] ;
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
  wire n21;
  wire n22;
  wire n23;
  wire n24;
  wtf_inv_1 g0 (.A(full_q), .Y(n0));
  wtf_nor2_4 g1 (.A(in_valid), .B(full_q), .Y(n1));
  wtf_nor2b_4 g2 (.A(out_ready), .B_N(valid_q), .Y(n2));
  wtf_nand2b_4 g3 (.A_N(out_ready), .B(valid_q), .Y(n3));
  wtf_nor2_1 g4 (.A(n1), .B(n3), .Y(n4));
  wtf_nor2_4 g5 (.A(full_q), .B(n2), .Y(n5));
  wtf_or3_1 g6 (.A(in_valid), .B(full_q), .C(n2), .X(n6));
  wtf_nor3_4 g7 (.A(full_q), .B(n1), .C(n3), .Y(n7));
  wtf_mux2i_4 g8 (.A0(\buf_q[0] ), .A1(in_data[0]), .S(n7), .Y(n8));
  wtf_mux2i_4 g9 (.A0(\buf_q[1] ), .A1(in_data[1]), .S(n7), .Y(n9));
  wtf_mux2i_4 g10 (.A0(\buf_q[2] ), .A1(in_data[2]), .S(n7), .Y(n10));
  wtf_mux2i_4 g11 (.A0(\buf_q[3] ), .A1(in_data[3]), .S(n7), .Y(n11));
  wtf_nor2_4 g12 (.A(n21), .B(n3), .Y(n12));
  wtf_nor2b_4 g13 (.A(n2), .B_N(full_q), .Y(n13));
  wtf_a221oi_4 g14 (.A1(in_data[0]), .A2(n5), .B1(\buf_q[0] ), .B2(n13), .C1(n12), .Y(n14));
  wtf_nor2_4 g15 (.A(n22), .B(n3), .Y(n15));
  wtf_a221oi_4 g16 (.A1(in_data[1]), .A2(n5), .B1(\buf_q[1] ), .B2(n13), .C1(n15), .Y(n16));
  wtf_nor2_4 g17 (.A(n23), .B(n3), .Y(n17));
  wtf_a221oi_4 g18 (.A1(in_data[2]), .A2(n5), .B1(\buf_q[2] ), .B2(n13), .C1(n17), .Y(n18));
  wtf_nor2_4 g19 (.A(n24), .B(n3), .Y(n19));
  wtf_a221oi_4 g20 (.A1(in_data[3]), .A2(n5), .B1(\buf_q[3] ), .B2(n13), .C1(n19), .Y(n20));
  wtf_dffrq_1 r0 (.D(n4), .CLK(clk), .RESET_B(rst_n), .Q(full_q));
  wtf_dffrq_1 r1 (.D(n6), .CLK(clk), .RESET_B(rst_n), .Q(valid_q));
  wtf_dff_1 r2 (.D(n8), .CLK(clk), .Q_N(\buf_q[0] ));
  wtf_dff_1 r3 (.D(n9), .CLK(clk), .Q_N(\buf_q[1] ));
  wtf_dff_1 r4 (.D(n10), .CLK(clk), .Q_N(\buf_q[2] ));
  wtf_dff_1 r5 (.D(n11), .CLK(clk), .Q_N(\buf_q[3] ));
  wtf_dff_1 r6 (.D(n14), .CLK(clk), .Q(n21), .Q_N(\data_q[0] ));
  wtf_dff_1 r7 (.D(n16), .CLK(clk), .Q(n22), .Q_N(\data_q[1] ));
  wtf_dff_1 r8 (.D(n18), .CLK(clk), .Q(n23), .Q_N(\data_q[2] ));
  wtf_dff_1 r9 (.D(n20), .CLK(clk), .Q(n24), .Q_N(\data_q[3] ));
  assign in_ready = n0;
  assign out_valid = valid_q;
  assign out_data[0] = \data_q[0] ;
  assign out_data[1] = \data_q[1] ;
  assign out_data[2] = \data_q[2] ;
  assign out_data[3] = \data_q[3] ;
endmodule
