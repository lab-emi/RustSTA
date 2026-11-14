// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Library units (`STA-REQ-0018`): every library is converted to the internal units with its own
//! units, so the same library written in two unit systems times identically, bit for bit, and
//! libraries that state different units share a corner; SDC values without `set_units` are read
//! in the units of the corner's first library, a library that disagrees with it is warning
//! `D0103`, and first libraries of two corners that make SDC values ambiguous are error `D0103`.

use std::sync::Arc;

use rusteda_core::diag::Severity;
use rusteda_core::job::{Ctx, Diag};
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::verilog::netlist::read_structural;
use rusteda_formats::{liberty, spef};
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::delay::spef::ParasiticInput;
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::limits::table::LimitsData;
use ruststa::engine::store::from_analysis::build_store_from_analysis_with_ctx;
use ruststa::engine::store::layout::{StoreMode, StorePrecision};
use ruststa::engine::types::{An, NodeId, Tr};

/// A unit system of the test libraries and SDC files. Values are given in the small units (ps,
/// fF, ohm, nW); a system with the large units (ns, pF, kohm) writes them divided by 1000, in the
/// shortest text that reads back as that quotient.
#[derive(Clone, Copy, PartialEq)]
enum System {
    /// `1ns`, `(1, pf)`, `1kohm`, `1nW`, `1mA`.
    NsPf,
    /// `1ps`, `(1, ff)`, `1ohm`, `1mW`, `1uA`.
    PsFf,
}

impl System {
    fn header(self) -> &'static str {
        match self {
            System::NsPf => {
                "  time_unit : \"1ns\";\n  capacitive_load_unit (1, pf);\n  \
                 pulling_resistance_unit : \"1kohm\";\n  leakage_power_unit : \"1nW\";\n  \
                 voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n"
            }
            System::PsFf => {
                "  time_unit : \"1ps\";\n  capacitive_load_unit (1, ff);\n  \
                 pulling_resistance_unit : \"1ohm\";\n  leakage_power_unit : \"1mW\";\n  \
                 voltage_unit : \"1V\";\n  current_unit : \"1uA\";\n"
            }
        }
    }

    /// A time (ps), capacitance (fF) or resistance (ohm) in this system's unit.
    fn v(self, small: f64) -> String {
        match self {
            System::NsPf => format!("{}", small / 1000.0),
            System::PsFf => format!("{small}"),
        }
    }

    /// A power (nW) in this system's unit.
    fn power(self, nw: f64) -> String {
        match self {
            System::NsPf => format!("{nw}"),
            System::PsFf => format!("{}", nw / 1e6),
        }
    }

    /// `"a, b"` of values in this system's unit.
    fn list(self, small: &[f64]) -> String {
        small
            .iter()
            .map(|&x| self.v(x))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A 2x2 table `values ("..", "..")` of an affine function of the input slew and the load.
fn table(sys: System, base: f64, per_slew: f64, per_load: f64) -> String {
    let slews = [20.0, 200.0];
    let loads = [2.0, 40.0];
    let rows: Vec<String> = slews
        .iter()
        .map(|s| {
            let row: Vec<f64> = loads
                .iter()
                .map(|c| base + per_slew * s + per_load * c)
                .collect();
            format!("\"{}\"", sys.list(&row))
        })
        .collect();
    format!("values ({});", rows.join(", "))
}

fn arc_tables(sys: System, delay: f64, slew: f64) -> String {
    format!(
        "cell_rise (delay_2x2) {{ {} }}\n        cell_fall (delay_2x2) {{ {} }}\n        \
         rise_transition (delay_2x2) {{ {} }}\n        fall_transition (delay_2x2) {{ {} }}",
        table(sys, delay, 0.125, 1.5),
        table(sys, delay + 5.0, 0.25, 1.25),
        table(sys, slew, 0.5, 3.5),
        table(sys, slew + 2.0, 0.375, 2.75)
    )
}

fn check_tables(sys: System, setup: f64) -> String {
    format!(
        "rise_constraint (check_2x2) {{ {} }}\n          fall_constraint (check_2x2) {{ {} }}",
        table(sys, setup, 0.25, 0.0),
        table(sys, setup + 7.0, 0.125, 0.0)
    )
}

/// The library header with the thresholds, templates, wire-load model and limits.
fn header(name: &str, sys: System) -> String {
    format!(
        "library ({name}) {{\n  delay_model : table_lookup;\n{units}  \
         input_threshold_pct_rise : 50;\n  input_threshold_pct_fall : 50;\n  \
         output_threshold_pct_rise : 50;\n  output_threshold_pct_fall : 50;\n  \
         slew_lower_threshold_pct_rise : 20;\n  slew_upper_threshold_pct_rise : 80;\n  \
         slew_lower_threshold_pct_fall : 20;\n  slew_upper_threshold_pct_fall : 80;\n  \
         default_max_transition : {max_tr};\n  default_fanout_load : 1;\n  \
         wire_load (wl) {{ resistance : {res}; capacitance : {cap}; area : 0; slope : 1.5; \
         fanout_length (1, 10); fanout_length (2, 18); }}\n  default_wire_load : wl;\n  \
         lu_table_template (delay_2x2) {{ variable_1 : input_net_transition; \
         variable_2 : total_output_net_capacitance; index_1 (\"{s}\"); index_2 (\"{c}\"); }}\n  \
         lu_table_template (check_2x2) {{ variable_1 : related_pin_transition; \
         variable_2 : constrained_pin_transition; index_1 (\"{s}\"); index_2 (\"{s}\"); }}\n",
        units = sys.header(),
        max_tr = sys.v(450.0),
        res = sys.v(12.5),
        cap = sys.v(0.75),
        s = sys.list(&[20.0, 200.0]),
        c = sys.list(&[2.0, 40.0]),
    )
}

/// Cells `BUF` and `DFF`.
fn std_cells(sys: System) -> String {
    format!(
        "  cell (BUF) {{ area : 2; cell_leakage_power : {leak};\n    \
         pin (A) {{ direction : input; capacitance : {cin}; }}\n    \
         pin (Y) {{ direction : output; function : \"A\"; max_capacitance : {cmax};\n      \
         timing () {{ related_pin : \"A\"; timing_sense : positive_unate;\n        {buf} }} }} }}\n  \
         cell (DFF) {{ area : 6; cell_leakage_power : {leak};\n    \
         ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; }}\n    \
         pin (CK) {{ direction : input; clock : true; capacitance : {cck}; \
         min_pulse_width_high : {mpw}; }}\n    \
         pin (D) {{ direction : input; capacitance : {cin};\n      \
         timing () {{ related_pin : \"CK\"; timing_type : setup_rising;\n          {setup} }}\n      \
         timing () {{ related_pin : \"CK\"; timing_type : hold_rising;\n          {hold} }} }}\n    \
         pin (Q) {{ direction : output; function : \"IQ\"; max_capacitance : {cmax};\n      \
         timing () {{ related_pin : \"CK\"; timing_type : rising_edge;\n        {dff} }} }} }}\n",
        leak = sys.power(3.5),
        cin = sys.v(1.75),
        cck = sys.v(2.25),
        cmax = sys.v(60.0),
        mpw = sys.v(95.0),
        buf = arc_tables(sys, 30.0, 15.0),
        dff = arc_tables(sys, 85.0, 22.0),
        setup = check_tables(sys, 40.0),
        hold = check_tables(sys, 12.0),
    )
}

/// Cell `MEM`, a macro with a clocked output and a constrained input.
fn macro_cell(sys: System) -> String {
    format!(
        "  cell (MEM) {{ area : 400; cell_leakage_power : {leak};\n    \
         pin (CLK) {{ direction : input; clock : true; capacitance : {cck}; }}\n    \
         pin (DIN) {{ direction : input; capacitance : {cin};\n      \
         timing () {{ related_pin : \"CLK\"; timing_type : setup_rising;\n          {setup} }}\n      \
         timing () {{ related_pin : \"CLK\"; timing_type : hold_rising;\n          {hold} }} }}\n    \
         pin (DOUT) {{ direction : output; max_capacitance : {cmax};\n      \
         timing () {{ related_pin : \"CLK\"; timing_type : rising_edge; \
         timing_sense : non_unate;\n        {arc} }} }} }}\n",
        leak = sys.power(1250.0),
        cck = sys.v(5.5),
        cin = sys.v(3.25),
        cmax = sys.v(80.0),
        setup = check_tables(sys, 120.0),
        hold = check_tables(sys, 30.0),
        arc = arc_tables(sys, 410.0, 18.0),
    )
}

fn read(text: &str, file: &str) -> Library {
    liberty::read_text(text, file).unwrap()
}

/// One library with the standard cells and the macro.
fn combined(sys: System) -> Library {
    let text = format!(
        "{}{}{}}}\n",
        header("units_lib", sys),
        std_cells(sys),
        macro_cell(sys)
    );
    read(&text, "units_lib.lib")
}

/// The standard-cell library and the macro library.
fn split(cells: System, mem: System) -> [Library; 2] {
    [
        read(
            &format!("{}{}}}\n", header("cells", cells), std_cells(cells)),
            "cells.lib",
        ),
        read(
            &format!("{}{}}}\n", header("mem", mem), macro_cell(mem)),
            "mem.lib",
        ),
    ]
}

const VERILOG: &str = "module top (clk, a, b, y, z);
  input clk, a, b;
  output y, z;
  wire n1, q, n2, m;
  BUF u1 (.A(a), .Y(n1));
  DFF r1 (.CK(clk), .D(n1), .Q(q));
  BUF u2 (.A(q), .Y(n2));
  MEM m1 (.CLK(clk), .DIN(n2), .DOUT(m));
  BUF u3 (.A(m), .Y(y));
  DFF r2 (.CK(clk), .D(b), .Q(z));
endmodule
";

fn netlist(libs: &[Library]) -> FrozenNetlist<'static> {
    let cell_pins = |name: &str| -> Option<Vec<(String, PinDir)>> {
        let cell = libs
            .iter()
            .flat_map(|l| &l.cells)
            .find(|c| c.name == name)?;
        Some(
            cell.pins
                .iter()
                .map(|p| {
                    let dir = match p.direction {
                        Direction::Input => PinDir::Input,
                        Direction::Output => PinDir::Output,
                        Direction::Inout => PinDir::Inout,
                        Direction::Internal => PinDir::Other,
                    };
                    (p.name.clone(), dir)
                })
                .collect(),
        )
    };
    read_structural(VERILOG, "top.v", &cell_pins)
        .unwrap()
        .freeze()
}

/// Constraints without `set_units`, written in `sys`.
fn sdc(sys: System) -> String {
    format!(
        "create_clock -name clk -period {period} [get_ports clk]\n\
         set_clock_uncertainty -setup {unc} [get_clocks clk]\n\
         set_clock_transition {ckt} [get_clocks clk]\n\
         set_input_delay -clock clk {din} [get_ports {{a b}}]\n\
         set_output_delay -clock clk {dout} [get_ports {{y z}}]\n\
         set_input_transition {tin} [get_ports {{a b}}]\n\
         set_load {load} [all_outputs]\n\
         set_max_transition {maxtr} [current_design]\n\
         set_max_capacitance {maxc} [current_design]\n",
        period = sys.v(1500.0),
        unc = sys.v(45.0),
        ckt = sys.v(35.0),
        din = sys.v(210.0),
        dout = sys.v(160.0),
        tin = sys.v(55.0),
        load = sys.v(6.5),
        maxtr = sys.v(400.0),
        maxc = sys.v(70.0),
    )
}

/// The extraction of the design's nets, in SPEF's own units.
fn extraction() -> Arc<spef::Spef> {
    let mut s = String::from(
        "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n\
         *T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 OHM\n",
    );
    let nets: [(&str, &str, &[&str], f64); 9] = [
        ("clk", "clk", &["r1:CK", "m1:CLK", "r2:CK"], 25.0),
        ("a", "a", &["u1:A"], 35.0),
        ("b", "b", &["r2:D"], 30.0),
        ("n1", "u1:Y", &["r1:D"], 60.0),
        ("q", "r1:Q", &["u2:A"], 45.0),
        ("n2", "u2:Y", &["m1:DIN"], 120.0),
        ("m", "m1:DOUT", &["u3:A"], 240.0),
        ("y", "u3:Y", &["y"], 80.0),
        ("z", "r2:Q", &["z"], 50.0),
    ];
    for (name, driver, sinks, ohm) in nets {
        let pin = |p: &str, dir: &str| {
            if p.contains(':') {
                format!("*I {p} {dir}\n")
            } else {
                format!("*P {p} {}\n", if dir == "O" { "I" } else { "O" })
            }
        };
        let mut conn = pin(driver, "O");
        let mut caps = format!("1 {driver} 0.002\n2 {name}:1 0.004\n");
        let mut res = format!("1 {driver} {name}:1 {ohm}\n");
        for (i, sink) in sinks.iter().enumerate() {
            conn += &pin(sink, "I");
            caps += &format!("{} {sink} 0.003\n", i + 3);
            res += &format!("{} {name}:1 {sink} {}\n", i + 2, ohm / 2.0);
        }
        let total = 0.006 + 0.003 * sinks.len() as f64;
        s += &format!("*D_NET {name} {total}\n*CONN\n{conn}*CAP\n{caps}*RES\n{res}*END\n");
    }
    Arc::new(spef::read(&s, spef::Limits::default()).unwrap())
}

/// Every value that a corner of an analysis reports, as bits: per node the transitions, the worst
/// arrivals and the setup and hold slacks, then the corner summary.
fn values(analysis: &EngineAnalysis<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for corner in &analysis.corners {
        for node in 0..analysis.graph.num_nodes() {
            let node = NodeId(node as u32);
            let mut row = format!("{} {}", corner.corner_name, node.0);
            for tr in Tr::ALL {
                for an in An::ALL {
                    let arrival = corner.worst_arrival(node, tr, an);
                    row += &format!(
                        " {:x} {:?}",
                        corner.slew(node, tr, an).to_bits(),
                        arrival.map(|(tag, a)| (tag, a.to_bits()))
                    );
                }
            }
            row += &format!(
                " {:?} {:?}",
                corner.setup_slack(node).map(f64::to_bits),
                corner.hold_slack(node).map(f64::to_bits)
            );
            out.push(row);
        }
        out.push(format!("{:?}", corner.summary()));
    }
    out
}

/// The values, the limit table, the timing table and the diagnostics of an analysis of the
/// corners `corners` (name, libraries), with the design's extraction when `extracted`.
struct Run {
    values: Vec<String>,
    limits: Vec<u8>,
    store: Vec<u8>,
    diags: Vec<Diag>,
}

fn analyze(corners: &[(&str, Vec<Library>)], sdc_text: &str, extracted: bool) -> Run {
    let frozen = netlist(&corners[0].1);
    let views: Vec<CornerLibs<'_>> = corners
        .iter()
        .map(|(name, libs)| CornerLibs::new(name, libs))
        .collect();
    let owned: Vec<(String, Vec<Library>)> = corners
        .iter()
        .map(|(name, libs)| (name.to_string(), libs.clone()))
        .collect();
    let constraints =
        CompiledConstraints::from_sdc_str(sdc_text, "top.sdc", &frozen, &views).unwrap();
    let options = AnalysisOptions {
        parasitics: if extracted {
            corners
                .iter()
                .map(|(name, _)| Arc::new(ParasiticInput::new(*name, extraction())))
                .collect()
        } else {
            Vec::new()
        },
        ..AnalysisOptions::default()
    };
    let analysis = EngineAnalysis::analyze(&frozen, &views, constraints, &options, Ctx::NONE)
        .unwrap_or_else(|d| panic!("{d:?}"));
    let store = build_store_from_analysis_with_ctx(
        &analysis,
        None,
        &owned,
        "cpu",
        StorePrecision::F64,
        StoreMode::Full,
        100,
        Ctx::NONE,
    );
    Run {
        values: values(&analysis),
        limits: LimitsData::from_analysis(&analysis, 1000)
            .to_bytes()
            .unwrap(),
        store: store.to_bytes().unwrap(),
        diags: analysis.diags.clone(),
    }
}

fn d0103(diags: &[Diag]) -> Vec<&Diag> {
    diags.iter().filter(|d| d.code == "D0103").collect()
}

// STA-REQ-0018: the same library and constraints written in ns/pF/kohm/nW and in ps/fF/ohm/mW
// give the same arrivals, transitions, slacks, limit checks and timing table, bit for bit, with
// the lumped wire-load model and with extracted parasitics.
#[test]
fn same_library_in_two_unit_systems_times_identically() {
    for extracted in [false, true] {
        let ns = analyze(
            &[("tt", vec![combined(System::NsPf)])],
            &sdc(System::NsPf),
            extracted,
        );
        let ps = analyze(
            &[("tt", vec![combined(System::PsFf)])],
            &sdc(System::PsFf),
            extracted,
        );
        // The design is timed: endpoints have slacks, so the comparison is not vacuous.
        assert!(
            ns.values.iter().filter(|r| r.contains("Some(")).count() > 10,
            "{extracted}"
        );
        assert_eq!(ns.values, ps.values, "extracted {extracted}");
        assert_eq!(ns.limits, ps.limits, "extracted {extracted}");
        assert_eq!(ns.store, ps.store, "extracted {extracted}");
        assert!(d0103(&ns.diags).is_empty() && d0103(&ps.diags).is_empty());
    }
}

// STA-REQ-0018: a corner whose cell library states ns/pF/kohm/nW and whose macro library states
// ps/fF/ohm/mW times as the corner with both libraries in ns/pF, with warning D0103 naming both
// libraries and every differing unit; SDC values are read in the units of the first library, in
// either order of the libraries.
#[test]
fn mixed_unit_corner_times_like_one_unit_system() {
    for extracted in [false, true] {
        let reference = analyze(
            &[("tt", split(System::NsPf, System::NsPf).to_vec())],
            &sdc(System::NsPf),
            extracted,
        );
        assert!(d0103(&reference.diags).is_empty());
        let mixed = analyze(
            &[("tt", split(System::NsPf, System::PsFf).to_vec())],
            &sdc(System::NsPf),
            extracted,
        );
        assert_eq!(mixed.values, reference.values, "extracted {extracted}");
        assert_eq!(mixed.limits, reference.limits, "extracted {extracted}");
        let warnings = d0103(&mixed.diags);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let w = warnings[0];
        assert_eq!(w.severity, Severity::Warning);
        for part in [
            "corner 'tt' libraries 'cells' and 'mem' disagree on units",
            "time_unit '1ns' vs '1ps'",
            "capacitive_load_unit '(1,pf)' vs '(1,ff)'",
            "pulling_resistance_unit '1kohm' vs '1ohm'",
            "leakage_power_unit '1nW' vs '1mW'",
            "current_unit '1mA' vs '1uA'",
            "read in the units of the first library 'cells'",
        ] {
            assert!(w.message.contains(part), "{part}: {}", w.message);
        }
        assert!(!w.message.contains("voltage_unit"), "{}", w.message);

        // With the macro library first, SDC values are read in its ps and fF.
        let [cells, mem] = split(System::NsPf, System::PsFf);
        let swapped = analyze(&[("tt", vec![mem, cells])], &sdc(System::PsFf), extracted);
        let [cells, mem] = split(System::NsPf, System::NsPf);
        let swapped_reference = analyze(&[("tt", vec![mem, cells])], &sdc(System::NsPf), extracted);
        assert_eq!(
            swapped.values, swapped_reference.values,
            "extracted {extracted}"
        );
        assert_eq!(
            swapped.limits, swapped_reference.limits,
            "extracted {extracted}"
        );
    }
}

fn compile(corners: &[(&str, Vec<Library>)], sdc_text: &str) -> Vec<Diag> {
    let frozen = netlist(&corners[0].1);
    let views: Vec<CornerLibs<'_>> = corners
        .iter()
        .map(|(name, libs)| CornerLibs::new(name, libs))
        .collect();
    match CompiledConstraints::from_sdc_str(sdc_text, "top.sdc", &frozen, &views) {
        Ok(c) => c.diags,
        Err(e) => e.diags,
    }
}

// STA-REQ-0018: D0103 is an error only when the first libraries of two corners state different
// time or capacitance units and the SDC sets that unit with no set_units, so that one SDC value
// would mean two physical values; units that disagree inside a corner, or a resistance unit
// across corners, are warnings or nothing.
#[test]
fn d0103_error_only_for_ambiguous_sdc_values() {
    let ns = combined(System::NsPf);
    let ps = combined(System::PsFf);

    // The first libraries of the two corners disagree on time and capacitance.
    let diags = compile(
        &[("fast", vec![ns.clone()]), ("slow", vec![ps.clone()])],
        &sdc(System::NsPf),
    );
    let found = d0103(&diags);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].severity, Severity::Error);
    for part in [
        "the first libraries 'units_lib' of corner 'fast' and 'units_lib' of corner 'slow'",
        "time_unit '1ns' vs '1ps'",
        "capacitive_load_unit '(1,pf)' vs '(1,ff)'",
        "set_units",
    ] {
        assert!(
            found[0].message.contains(part),
            "{part}: {}",
            found[0].message
        );
    }
    assert!(!found[0].message.contains("pulling_resistance_unit"));

    // `set_units` for one quantity leaves the other one ambiguous.
    let time_only = format!("set_units -time ns\n{}", sdc(System::NsPf));
    let diags = compile(
        &[("fast", vec![ns.clone()]), ("slow", vec![ps.clone()])],
        &time_only,
    );
    let found = d0103(&diags);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].severity, Severity::Error);
    assert!(
        !found[0].message.contains("time_unit"),
        "{}",
        found[0].message
    );
    assert!(found[0].message.contains("capacitive_load_unit"));

    // With both units set, the corners may state any units.
    let both = format!("set_units -time ns -capacitance pF\n{}", sdc(System::NsPf));
    let diags = compile(
        &[("fast", vec![ns.clone()]), ("slow", vec![ps.clone()])],
        &both,
    );
    assert!(d0103(&diags).is_empty(), "{diags:?}");

    // A resistance unit alone (no SDC value of this engine is a resistance) is no error.
    let ohm = read(
        &format!(
            "{}{}{}}}\n",
            header("units_lib", System::NsPf),
            std_cells(System::NsPf),
            macro_cell(System::NsPf)
        )
        .replace("\"1kohm\"", "\"1ohm\""),
        "units_lib.lib",
    );
    let diags = compile(
        &[("fast", vec![ns.clone()]), ("slow", vec![ohm])],
        &sdc(System::NsPf),
    );
    assert!(d0103(&diags).is_empty(), "{diags:?}");

    // Inside a corner the disagreement is a warning per library and corner, never an error.
    let [cells, mem] = split(System::NsPf, System::PsFf);
    let corner = vec![cells, mem];
    let diags = compile(
        &[("fast", corner.clone()), ("slow", corner)],
        &sdc(System::NsPf),
    );
    let found = d0103(&diags);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|d| d.severity == Severity::Warning));
    assert!(
        found[0]
            .message
            .starts_with("corner 'fast' libraries 'cells' and 'mem'")
    );
    assert!(
        found[1]
            .message
            .starts_with("corner 'slow' libraries 'cells' and 'mem'")
    );

    // Libraries that differ only in units no SDC value is stated in (power and current, as the
    // IHP SG13G2 SRAM and its EZ130 cell library do) agree.
    let [cells, _] = split(System::NsPf, System::NsPf);
    let mem = read(
        &format!(
            "{}{}}}\n",
            header("mem", System::NsPf),
            macro_cell(System::NsPf)
        )
        .replace("\"1nW\"", "\"1mW\"")
        .replace("\"1mA\"", "\"1uA\""),
        "mem.lib",
    );
    let diags = compile(&[("tt", vec![cells, mem])], &sdc(System::NsPf));
    assert!(d0103(&diags).is_empty(), "{diags:?}");
}
