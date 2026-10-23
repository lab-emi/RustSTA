module latches(
  input wire en,
  input wire en_n,
  input wire [3:0] d,
  input wire sel,
  output wire [3:0] q,
  output wire [1:0] r
);
  wire \q[0] ;
  wire \q[1] ;
  wire \q[2] ;
  wire \q[3] ;
  wire \r[0] ;
  wire \r[1] ;
  wtf_dlh_1 r0 (.D(d[0]), .GATE(en), .Q(\q[0] ));
  wtf_dlh_1 r1 (.D(d[1]), .GATE(en), .Q(\q[1] ));
  wtf_dlh_1 r2 (.D(d[2]), .GATE(en), .Q(\q[2] ));
  wtf_dlh_1 r3 (.D(d[3]), .GATE(en), .Q(\q[3] ));
  wtf_dll_1 r4 (.D(d[0]), .GATE_N(en_n), .Q(\r[0] ));
  wtf_dll_1 r5 (.D(d[1]), .GATE_N(en_n), .Q(\r[1] ));
  assign q[0] = \q[0] ;
  assign q[1] = \q[1] ;
  assign q[2] = \q[2] ;
  assign q[3] = \q[3] ;
  assign r[0] = \r[0] ;
  assign r[1] = \r[1] ;
endmodule
