module shift_reg(
  input wire clk,
  input wire rst_n,
  input wire [2:0] d,
  output wire [2:0] q
);
  wire \stage[1][0] ;
  wire \stage[1][1] ;
  wire \stage[1][2] ;
  wire \stage[2][0] ;
  wire \stage[2][1] ;
  wire \stage[2][2] ;
  wire \stage[3][0] ;
  wire \stage[3][1] ;
  wire \stage[3][2] ;
  wire \stage[4][0] ;
  wire \stage[4][1] ;
  wire \stage[4][2] ;
  wtf_dffrq_1 r0 (.D(d[0]), .CLK(clk), .RESET_B(rst_n), .Q(\stage[1][0] ));
  wtf_dffrq_1 r1 (.D(d[1]), .CLK(clk), .RESET_B(rst_n), .Q(\stage[1][1] ));
  wtf_dffrq_1 r2 (.D(d[2]), .CLK(clk), .RESET_B(rst_n), .Q(\stage[1][2] ));
  wtf_dffrq_1 r3 (.D(\stage[1][0] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[2][0] ));
  wtf_dffrq_1 r4 (.D(\stage[1][1] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[2][1] ));
  wtf_dffrq_1 r5 (.D(\stage[1][2] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[2][2] ));
  wtf_dffrq_1 r6 (.D(\stage[2][0] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[3][0] ));
  wtf_dffrq_1 r7 (.D(\stage[2][1] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[3][1] ));
  wtf_dffrq_1 r8 (.D(\stage[2][2] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[3][2] ));
  wtf_dffrq_1 r9 (.D(\stage[3][0] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[4][0] ));
  wtf_dffrq_1 r10 (.D(\stage[3][1] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[4][1] ));
  wtf_dffrq_1 r11 (.D(\stage[3][2] ), .CLK(clk), .RESET_B(rst_n), .Q(\stage[4][2] ));
  assign q[0] = \stage[4][0] ;
  assign q[1] = \stage[4][1] ;
  assign q[2] = \stage[4][2] ;
endmodule
