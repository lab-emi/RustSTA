module decoder(
  input wire [7:0] opc,
  output wire [2:0] cls,
  output wire is_mem,
  output wire [7:0] onehot
);
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
  wtf_inv_1 g0 (.A(opc[0]), .Y(n0));
  wtf_inv_1 g1 (.A(opc[1]), .Y(n1));
  wtf_inv_1 g2 (.A(opc[4]), .Y(n2));
  wtf_inv_1 g3 (.A(opc[5]), .Y(n3));
  wtf_inv_1 g4 (.A(opc[7]), .Y(n4));
  wtf_nor2b_2 g5 (.A(opc[1]), .B_N(opc[0]), .Y(n5));
  wtf_or2_1 g6 (.A(opc[6]), .B(opc[7]), .X(n6));
  wtf_nand2_4 g7 (.A(opc[5]), .B(n5), .Y(n7));
  wtf_nand2b_1 g8 (.A_N(opc[5]), .B(opc[4]), .Y(n8));
  wtf_or2_1 g9 (.A(opc[4]), .B(opc[5]), .X(n9));
  wtf_a21oi_4 g10 (.A1(n7), .A2(n9), .B1(n6), .Y(n10));
  wtf_a221oi_4 g11 (.A1(n2), .A2(n3), .B1(n7), .B2(n8), .C1(n6), .Y(n11));
  wtf_inv_1 g12 (.A(n13), .Y(n12));
  wtf_nand3_4 g13 (.A(opc[0]), .B(opc[1]), .C(opc[2]), .Y(n13));
  wtf_nand3_2 g14 (.A(opc[3]), .B(opc[4]), .C(opc[5]), .Y(n14));
  wtf_o21a_4 g15 (.A1(n13), .A2(n14), .B1(opc[6]), .X(n15));
  wtf_nor2_4 g16 (.A(n4), .B(n15), .Y(n16));
  wtf_mux2_1 g17 (.A0(opc[6]), .A1(opc[5]), .S(opc[7]), .X(n17));
  wtf_nor3_1 g18 (.A(opc[0]), .B(opc[1]), .C(opc[2]), .Y(n18));
  wtf_nor2b_2 g19 (.A(opc[2]), .B_N(n5), .Y(n19));
  wtf_nor3_2 g20 (.A(opc[0]), .B(n1), .C(opc[2]), .Y(n20));
  wtf_nor3_2 g21 (.A(n0), .B(n1), .C(opc[2]), .Y(n21));
  wtf_and3_2 g22 (.A(n0), .B(n1), .C(opc[2]), .X(n22));
  wtf_and2_2 g23 (.A(opc[2]), .B(n5), .X(n23));
  wtf_and3_2 g24 (.A(n0), .B(opc[1]), .C(opc[2]), .X(n24));
  assign cls[0] = n10;
  assign cls[1] = n11;
  assign cls[2] = n16;
  assign is_mem = n17;
  assign onehot[0] = n18;
  assign onehot[1] = n19;
  assign onehot[2] = n20;
  assign onehot[3] = n21;
  assign onehot[4] = n22;
  assign onehot[5] = n23;
  assign onehot[6] = n24;
  assign onehot[7] = n12;
endmodule
