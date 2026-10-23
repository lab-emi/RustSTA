// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suite `sta-inputs`: timing types (STA-REQ-0011) and net arcs (STA-REQ-0111) on a library with
//! constant tables, so that every expected value follows by hand.

use rusteda_formats::liberty::{Library, read_text};
use ruststa::prop::{EARLY, LATE};
use ruststa::{Analysis, Backend, Source, StaOptions};

fn timing(related: &str, ty: &str, sense: Option<&str>, rise: f64, fall: f64) -> String {
    let sense = sense.map_or(String::new(), |s| format!("timing_sense : {s}; "));
    format!(
        "timing () {{ related_pin : \"{related}\"; timing_type : {ty}; {sense}\
         cell_rise (scalar) {{ values (\"{rise}\"); }} rise_transition (scalar) {{ values (\"0.05\"); }} \
         cell_fall (scalar) {{ values (\"{fall}\"); }} fall_transition (scalar) {{ values (\"0.04\"); }} }}"
    )
}

fn check(related: &str, ty: &str, v: f64) -> String {
    format!(
        "timing () {{ related_pin : \"{related}\"; timing_type : {ty}; \
         rise_constraint (scalar) {{ values (\"{v}\"); }} fall_constraint (scalar) {{ values (\"{v}\"); }} }}"
    )
}

fn library() -> Library {
    let text = format!(
        "library (tt) {{ time_unit : \"1ns\"; capacitive_load_unit (1, pf);\n\
         cell (RBUF) {{ pin (A) {{ direction : input; capacitance : 0.01; }}\n\
           pin (Y) {{ direction : output; function : \"A\"; {} }} }}\n\
         cell (FBUF) {{ pin (A) {{ direction : input; capacitance : 0.01; }}\n\
           pin (Y) {{ direction : output; function : \"A\"; {} }} }}\n\
         cell (DFFR) {{ ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; }}\n\
           pin (D) {{ direction : input; capacitance : 0.01; {} {} }}\n\
           pin (RN) {{ direction : input; capacitance : 0.01; {} {} }}\n\
           pin (CK) {{ direction : input; clock : true; capacitance : 0.01;\n\
             timing () {{ related_pin : \"CK\"; timing_type : min_pulse_width; }} }}\n\
           pin (Q) {{ direction : output; function : \"IQ\"; {} {} }} }}\n\
         }}\n",
        timing("A", "combinational_rise", Some("positive_unate"), 0.2, 0.3),
        timing("A", "combinational_fall", Some("positive_unate"), 0.2, 0.3),
        check("CK", "setup_rising", 0.1),
        check("CK", "hold_rising", 0.05),
        check("CK", "recovery_rising", 0.15),
        check("CK", "removal_rising", 0.07),
        timing("CK", "rising_edge", Some("non_unate"), 0.4, 0.4),
        timing("RN", "clear", Some("positive_unate"), 1.0, 1.0),
    );
    read_text(&text, "tt.lib").expect("library")
}

const NETLIST: &str = "module m (input clk, input a, input rn, output y, output z, output q);\n\
    RBUF u1 (.A(a), .Y(y));\n FBUF u2 (.A(a), .Y(z));\n\
    DFFR r (.D(a), .CK(clk), .RN(rn), .Q(q));\nendmodule\n";

const SDC: &str = "create_clock -name clk -period 2 [get_ports clk]\n\
    set_input_delay -clock clk 0.5 [get_ports a]\nset_input_delay -clock clk 0.5 [get_ports rn]\n\
    set_input_transition 0.03 [get_ports a]\n";

fn analysis() -> Analysis {
    let opts = StaOptions {
        netlist: Source::Texts(vec![("n.v".into(), NETLIST.into())]),
        top: None,
        sdc: SDC.into(),
        paths: 1,
        path_to: vec![],
        backend: Backend::Cpu,
        sdc_name: String::new(),
    };
    ruststa::run(&opts, &[library()]).expect("analysis")
}

fn node(a: &Analysis, name: &str) -> u32 {
    a.graph
        .nodes
        .iter()
        .position(|n| n.name == name)
        .expect(name) as u32
}

const EPS: f64 = 1e-12;

// STA-REQ-0011: `combinational_rise` arcs produce only output rises, `combinational_fall` arcs
// only output falls.
#[test]
fn single_transition_arcs() {
    let a = analysis();
    let (u1, u2) = (node(&a, "u1/Y"), node(&a, "u2/Y"));
    assert!(a.values.present(u1, 0, 0) && !a.values.present(u1, 0, 1));
    assert!(!a.values.present(u2, 0, 0) && a.values.present(u2, 0, 1));
    let late = a.values.arr[a.values.idx(u1, 0, 0, LATE)];
    assert!((late - 0.7).abs() < EPS, "{late}");
    let late = a.values.arr[a.values.idx(u2, 0, 1, LATE)];
    assert!((late - 0.8).abs() < EPS, "{late}");
}

// STA-REQ-0011: recovery and removal act as setup and hold checks of the asynchronous pin.
#[test]
fn recovery_and_removal() {
    let a = analysis();
    let rn = node(&a, "r/RN");
    let s = a.endpoints.setup[&rn].slack;
    let h = a.endpoints.hold[&rn].slack;
    assert!((s - (2.0 - 0.15 - 0.5)).abs() < EPS, "recovery slack {s}");
    assert!((h - (0.5 - 0.07)).abs() < EPS, "removal slack {h}");
}

// STA-REQ-0011: `clear` arcs are not propagated; the output is launched by the clock only.
#[test]
fn clear_arcs_not_propagated() {
    let a = analysis();
    let q = node(&a, "r/Q");
    for tr in 0..2 {
        let late = a.values.arr[a.values.idx(q, 0, tr, LATE)];
        assert!((late - 0.4).abs() < EPS, "Q {tr}: {late}");
    }
}

// STA-REQ-0011: other timing types are ignored with a note naming the cell.
#[test]
fn other_types_noted() {
    let a = analysis();
    let c = a.model.cells.iter().find(|c| c.name == "DFFR").unwrap();
    assert!(
        c.notes.iter().any(|n| n.contains("min_pulse_width")),
        "{:?}",
        c.notes
    );
}

// STA-REQ-0111: net arcs have zero delay and copy the driver's transition to every load.
#[test]
fn net_arcs_copy_arrival_and_transition() {
    let a = analysis();
    let port = node(&a, "a");
    for load in ["u1/A", "u2/A", "r/D"] {
        let v = node(&a, load);
        for tr in 0..2 {
            for an in [EARLY, LATE] {
                let (i, j) = (a.values.idx(port, 0, tr, an), a.values.idx(v, 0, tr, an));
                assert_eq!(a.values.arr[i], a.values.arr[j], "{load} arrival");
                assert_eq!(a.values.slew[i], a.values.slew[j], "{load} transition");
            }
        }
        assert!((a.values.slew[a.values.idx(v, 0, 0, LATE)] - 0.03).abs() < EPS);
    }
}
