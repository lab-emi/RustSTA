// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Shared helpers for the RustSTA suites: a synthetic library whose tables are linear in their
//! variables (so that interpolation and extrapolation are exact) and analysis runners.

#![allow(dead_code)]

use std::fmt::Write as _;

use rusteda_formats::liberty::{Library, read_text};

/// Linear delay model of a synthetic arc: `d0 + ks * slew + kc * load` and output transition
/// `s0 + mc * load`.
#[derive(Debug, Clone, Copy)]
pub struct Lin {
    pub d0: f64,
    pub ks: f64,
    pub kc: f64,
    pub s0: f64,
    pub mc: f64,
}

impl Lin {
    pub fn delay(&self, slew: f64, load: f64) -> f64 {
        self.d0 + self.ks * slew + self.kc * load
    }
    pub fn slew(&self, load: f64) -> f64 {
        self.s0 + self.mc * load
    }
}

/// Arc models of the synthetic library (rise and fall differ so that transitions matter).
pub const INV_R: Lin = Lin {
    d0: 0.10,
    ks: 0.20,
    kc: 2.0,
    s0: 0.05,
    mc: 1.5,
};
pub const INV_F: Lin = Lin {
    d0: 0.08,
    ks: 0.15,
    kc: 1.5,
    s0: 0.04,
    mc: 1.2,
};
pub const BUF_R: Lin = Lin {
    d0: 0.20,
    ks: 0.10,
    kc: 1.0,
    s0: 0.06,
    mc: 1.0,
};
pub const BUF_F: Lin = Lin {
    d0: 0.18,
    ks: 0.10,
    kc: 1.2,
    s0: 0.05,
    mc: 1.1,
};
pub const XOR_R: Lin = Lin {
    d0: 0.30,
    ks: 0.25,
    kc: 2.5,
    s0: 0.07,
    mc: 1.8,
};
pub const XOR_F: Lin = Lin {
    d0: 0.28,
    ks: 0.20,
    kc: 2.2,
    s0: 0.06,
    mc: 1.6,
};
pub const CKQ_R: Lin = Lin {
    d0: 0.40,
    ks: 0.0,
    kc: 1.0,
    s0: 0.08,
    mc: 1.0,
};
pub const CKQ_F: Lin = Lin {
    d0: 0.35,
    ks: 0.0,
    kc: 0.9,
    s0: 0.07,
    mc: 1.0,
};
pub const DQ_R: Lin = Lin {
    d0: 0.25,
    ks: 0.10,
    kc: 1.0,
    s0: 0.06,
    mc: 1.0,
};
pub const DQ_F: Lin = Lin {
    d0: 0.22,
    ks: 0.10,
    kc: 1.0,
    s0: 0.05,
    mc: 1.0,
};
/// Input capacitance of every input pin (pF).
pub const CIN: f64 = 0.01;
/// Setup and hold times (constant tables).
pub const SETUP: f64 = 0.10;
pub const HOLD: f64 = 0.05;

fn table(out: &mut String, kind: &str, v: impl Fn(f64, f64) -> f64) {
    // Two points per axis make a bilinear table exactly linear everywhere.
    let _ = writeln!(out, "        {kind} (t2) {{");
    let _ = writeln!(
        out,
        "          values (\"{:.9}, {:.9}\", \"{:.9}, {:.9}\");",
        v(0.0, 0.0),
        v(0.0, 1.0),
        v(1.0, 0.0),
        v(1.0, 1.0)
    );
    let _ = writeln!(out, "        }}");
}

fn arc(out: &mut String, related: &str, sense: &str, ty: &str, r: Lin, f: Lin) {
    let _ = writeln!(out, "      timing () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        timing_sense : {sense};");
    let _ = writeln!(out, "        timing_type : {ty};");
    table(out, "cell_rise", |s, c| r.delay(s, c));
    table(out, "rise_transition", |_, c| r.slew(c));
    table(out, "cell_fall", |s, c| f.delay(s, c));
    table(out, "fall_transition", |_, c| f.slew(c));
    let _ = writeln!(out, "      }}");
}

fn check_arc(out: &mut String, related: &str, ty: &str, v: f64) {
    let _ = writeln!(out, "      timing () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        timing_type : {ty};");
    let _ = writeln!(
        out,
        "        rise_constraint (scalar) {{ values (\"{v}\"); }}"
    );
    let _ = writeln!(
        out,
        "        fall_constraint (scalar) {{ values (\"{v}\"); }}"
    );
    let _ = writeln!(out, "      }}");
}

fn input(out: &mut String, p: &str, clock: bool) {
    let ck = if clock { " clock : true;" } else { "" };
    let _ = writeln!(
        out,
        "    pin ({p}) {{ direction : input; capacitance : {CIN};{ck} }}"
    );
}

/// Liberty text of the linear library.
pub fn linear_library() -> String {
    let mut o = String::from(
        "library (lin) {\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n  lu_table_template (t2) {\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }\n",
    );
    // Combinational cells.
    for (name, ins, func, sense, r, f) in [
        ("INV", vec!["A"], "!A", "negative_unate", INV_R, INV_F),
        ("BUF", vec!["A"], "A", "positive_unate", BUF_R, BUF_F),
        (
            "AND2",
            vec!["A", "B"],
            "A&B",
            "positive_unate",
            BUF_R,
            BUF_F,
        ),
        ("XOR2", vec!["A", "B"], "A^B", "non_unate", XOR_R, XOR_F),
    ] {
        let _ = writeln!(o, "  cell ({name}) {{\n    area : 1;");
        for i in &ins {
            input(&mut o, i, false);
        }
        let _ = writeln!(
            o,
            "    pin (Y) {{\n      direction : output;\n      function : \"{func}\";"
        );
        for i in &ins {
            arc(&mut o, i, sense, "combinational", r, f);
        }
        let _ = writeln!(o, "    }}\n  }}");
    }
    // Flip-flops (rising and falling clock) and a positive-level latch.
    for (name, ck, edge, group) in [
        (
            "DFF",
            "CK",
            "rising",
            "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; }",
        ),
        (
            "DFFN",
            "CKN",
            "falling",
            "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"!CKN\"; }",
        ),
    ] {
        let _ = writeln!(o, "  cell ({name}) {{\n    area : 4;\n    {group}");
        let _ = writeln!(
            o,
            "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        check_arc(&mut o, ck, &format!("setup_{edge}"), SETUP);
        check_arc(&mut o, ck, &format!("hold_{edge}"), HOLD);
        let _ = writeln!(o, "    }}");
        input(&mut o, ck, true);
        let _ = writeln!(
            o,
            "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
        );
        arc(
            &mut o,
            ck,
            "non_unate",
            &format!("{edge}_edge"),
            CKQ_R,
            CKQ_F,
        );
        let _ = writeln!(o, "    }}\n  }}");
    }
    let _ = writeln!(
        o,
        "  cell (LAT) {{\n    area : 3;\n    latch (IQ, IQN) {{ data_in : \"D\"; enable : \"G\"; }}"
    );
    let _ = writeln!(
        o,
        "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
    );
    check_arc(&mut o, "G", "setup_falling", SETUP);
    check_arc(&mut o, "G", "hold_falling", HOLD);
    let _ = writeln!(o, "    }}");
    input(&mut o, "G", true);
    let _ = writeln!(
        o,
        "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
    );
    arc(&mut o, "D", "positive_unate", "combinational", DQ_R, DQ_F);
    arc(&mut o, "G", "non_unate", "rising_edge", CKQ_R, CKQ_F);
    let _ = writeln!(o, "    }}\n  }}");
    o.push_str("}\n");
    o
}

pub fn lib() -> Library {
    read_text(&linear_library(), "lin.lib").expect("linear library")
}

/// Runs RustSTA on a netlist text.
pub fn sta(netlist: &str, sdc: &str) -> ruststa::Report {
    let opts = ruststa::StaOptions {
        netlist: ruststa::Source::Texts(vec![("n.v".into(), netlist.into())]),
        top: None,
        sdc: sdc.into(),
        paths: 3,
        path_to: vec![],
        backend: ruststa::Backend::Cpu,
        sdc_name: String::new(),
    };
    ruststa::analyze(&opts, &[lib()]).expect("analysis")
}

/// Runs the independent checker against a RustSTA report.
pub fn check(netlist: &str, sdc: &str, rep: &ruststa::Report) -> ruststa_check::CheckStaReport {
    let opts = ruststa_check::CheckStaOptions {
        netlist: ruststa_check::Netlist::Texts(vec![("n.v".into(), netlist.into())]),
        top: None,
        sdc: sdc.into(),
        tolerance: 1e-9,
    };
    ruststa_check::check(&opts, &[lib()], &serde_json::to_value(rep).expect("json"))
}

/// Setup and hold slack of an endpoint pin in a report.
pub fn slack(rep: &ruststa::Report, pin: &str) -> (Option<f64>, Option<f64>) {
    rep.endpoints
        .iter()
        .find(|e| e.pin == pin)
        .map_or((None, None), |e| (e.setup_slack, e.hold_slack))
}
