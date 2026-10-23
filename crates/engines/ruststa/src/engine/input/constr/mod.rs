// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The constraint compiler: the revision's constraints resolved against its netlist (owner: E2,
//! then E12).
//!
//! The constraints table (CONSTR v1) holds the SDC commands in evaluation order with unresolved
//! object queries. The compiler resolves every query against the names of the frozen netlist
//! (glob, `-regexp`, `-nocase`, `-hierarchical`, `-of_objects`, `-filter`, `all_*`,
//! `current_design`), converts values with each corner's library units, and builds the tables of
//! the analysis: clocks, input and output delays, loads, transitions, driving cells, clock
//! attributes, case analysis, disabled timing, exception object sets, wire-load selection, path
//! groups and the recorded design-rule limits. The import diagnostics stored in the table are
//! replayed. Work item E2 compiles the Phase 2a commands; E12 adds derates, generated and
//! propagated clocks, `-through` object sets and `-reference_pin` in Phase 2b.

mod compile;
mod compile_ext;
pub mod index;
pub mod model;
pub mod pattern;
pub mod query;

use rustc_hash::FxHashMap;
use rusteda_core::diag::Diag;
use rusteda_core::liberty::Library;
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::sdc::{ConstrSection, Env, Limits, Sdc, Source};

pub use compile::{compile_sdc, compile_sdc_lenient};
pub use index::DesignIndex;
pub use model::*;

use crate::engine::types::{An, NodeId, Tr};

impl CompiledConstraints {
    /// Compiles `sdc` against `netlist` and `corners`, returning `Err(CompileError)` if any
    /// [`rusteda_core::diag::Severity::Error`] diagnostic is emitted.
    pub fn compile(
        sdc: &Sdc,
        netlist: &FrozenNetlist,
        corners: &[CornerLibs<'_>],
    ) -> Result<Self, CompileError> {
        compile_sdc(sdc, netlist, corners)
    }

    /// Compiles `sdc` against `netlist` and a single corner named `"default"` backed by `libs`.
    pub fn compile_single_corner(
        sdc: &Sdc,
        netlist: &FrozenNetlist,
        libs: &[Library],
    ) -> Result<Self, CompileError> {
        let corners = [CornerLibs::new("default", libs)];
        compile_sdc(sdc, netlist, &corners)
    }

    /// Compiles `sdc` against `netlist` and `corners` leniently, returning the compiled tables and
    /// all diagnostics even when error-severity diagnostics are present.
    #[must_use]
    pub fn compile_lenient(sdc: &Sdc, netlist: &FrozenNetlist, corners: &[CornerLibs<'_>]) -> Self {
        compile_sdc_lenient(sdc, netlist, corners)
    }

    /// Decodes an [`Sdc`] from the 13 `CONSTR v1` binary sections and compiles it against
    /// `netlist` and `corners`.
    pub fn from_sections(
        sections: &[ConstrSection],
        netlist: &FrozenNetlist,
        corners: &[CornerLibs<'_>],
    ) -> Result<Self, CompileError> {
        let sdc = Sdc::from_sections(sections).map_err(|e| {
            let d = Diag::error("D0009", format!("CONSTR v1 section decode error: {e}"));
            CompileError {
                diags: vec![d.clone()],
                partial: Box::new(Self {
                    diags: vec![d],
                    ..Self::default()
                }),
            }
        })?;
        compile_sdc(&sdc, netlist, corners)
    }

    /// Decodes an [`Sdc`] from the 13 `CONSTR v1` binary sections and compiles it against
    /// `netlist` and a single corner backed by `libs`.
    pub fn from_sections_single_corner(
        sections: &[ConstrSection],
        netlist: &FrozenNetlist,
        libs: &[Library],
    ) -> Result<Self, CompileError> {
        let corners = [CornerLibs::new("default", libs)];
        Self::from_sections(sections, netlist, &corners)
    }

    /// Evaluates an in-memory SDC script named `path` and compiles it against `netlist` and
    /// `corners`.
    pub fn from_sdc_str(
        text: &str,
        path: &str,
        netlist: &FrozenNetlist,
        corners: &[CornerLibs<'_>],
    ) -> Result<Self, CompileError> {
        let sources = [Source::new(path, text)];
        let (sdc, _) = rusteda_formats::sdc::read(&sources, &Env::default(), &Limits::default());
        compile_sdc(&sdc, netlist, corners)
    }

    /// Evaluates an SDC [`Source`] and compiles it against `netlist` and `corners`.
    pub fn from_sdc_source(
        source: &Source,
        env: &Env,
        limits: &Limits,
        netlist: &FrozenNetlist,
        corners: &[CornerLibs<'_>],
    ) -> Result<Self, CompileError> {
        let (sdc, _) = rusteda_formats::sdc::read(std::slice::from_ref(source), env, limits);
        compile_sdc(&sdc, netlist, corners)
    }

    /// Converts this [`CompiledConstraints`] to the legacy [`crate::constraints::Constraints`]
    /// representation using the node numbering of `graph`.
    #[must_use]
    pub fn to_legacy_constraints(
        &self,
        netlist: &FrozenNetlist,
        graph: &crate::graph::Graph,
    ) -> crate::constraints::Constraints {
        to_legacy_constraints(self, netlist, graph)
    }
}

/// Converts [`CompiledConstraints`] to the legacy [`crate::constraints::Constraints`] representation
/// using the node numbering of `graph` (matched by canonical node name against `netlist`).
#[must_use]
pub fn to_legacy_constraints(
    compiled: &CompiledConstraints,
    netlist: &FrozenNetlist,
    graph: &crate::graph::Graph,
) -> crate::constraints::Constraints {
    let index = DesignIndex::build(netlist, &[]);
    let graph_by_name: FxHashMap<&str, u32> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.name.as_str(), i as u32))
        .collect();
    let map_node = |nid: NodeId| -> u32 {
        let name = index.node_name(nid);
        graph_by_name.get(name).copied().unwrap_or(nid.0)
    };

    let clocks = compiled
        .clocks
        .iter()
        .map(|ck| {
            let mut sources: Vec<u32> = ck.sources.iter().map(|&s| map_node(s)).collect();
            sources.sort_unstable();
            sources.dedup();
            crate::constraints::Clock {
                name: ck.name.clone(),
                period: ck.period_ns,
                edges: ck.waveform_ns,
                sources,
                latency: [
                    ck.network_latency_ns[An::Early as usize][Tr::Rise as usize],
                    ck.network_latency_ns[An::Late as usize][Tr::Rise as usize],
                ],
                transition: ck.transition_ns[An::Late as usize][Tr::Rise as usize],
                uncertainty_setup: ck.uncertainty_setup_ns[Tr::Rise as usize],
                uncertainty_hold: ck.uncertainty_hold_ns[Tr::Rise as usize],
            }
        })
        .collect();

    let map_delays = |delays: &[CompiledIoDelay]| -> Vec<crate::constraints::PortDelay> {
        let mut out: Vec<crate::constraints::PortDelay> = delays
            .iter()
            .filter_map(|d| {
                let clock = d.clock?.0;
                let early = d.delay_ns[An::Early as usize][Tr::Rise as usize]
                    .or(d.delay_ns[An::Early as usize][Tr::Fall as usize]);
                let late = d.delay_ns[An::Late as usize][Tr::Rise as usize]
                    .or(d.delay_ns[An::Late as usize][Tr::Fall as usize]);
                let e = early.or(late).unwrap_or(0.0);
                let l = late.or(early).unwrap_or(0.0);
                Some(crate::constraints::PortDelay {
                    node: map_node(d.node),
                    clock,
                    edge: d.clock_edge as usize,
                    value: [e, l],
                })
            })
            .collect();
        out.sort_by_key(|d| (d.node, d.clock, d.edge));
        out
    };

    let mut port_load = FxHashMap::default();
    for (&nid, pl) in &compiled.port_loads {
        port_load.insert(map_node(nid), pl.load_pf);
    }

    let mut input_transition = FxHashMap::default();
    for (&nid, it) in &compiled.input_transitions {
        input_transition.insert(map_node(nid), it.min_max_pair());
    }

    crate::constraints::Constraints {
        clocks,
        input_delays: map_delays(&compiled.input_delays),
        output_delays: map_delays(&compiled.output_delays),
        port_load,
        input_transition,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusteda_core::netlist::PinDir;
    use rusteda_formats::verilog;

    fn parse_top(src: &str) -> FrozenNetlist<'static> {
        let cell_pins = |cell: &str| -> Option<Vec<(String, PinDir)>> {
            match cell {
                "DFF_X1" => Some(vec![
                    ("D".into(), PinDir::Input),
                    ("CK".into(), PinDir::Input),
                    ("Q".into(), PinDir::Output),
                    ("QN".into(), PinDir::Output),
                ]),
                _ => None,
            }
        };
        let gn =
            verilog::netlist::read_structural(src, "<test>", &cell_pins).expect("valid verilog");
        gn.freeze()
    }

    #[test]
    fn basic_compile_and_sections_roundtrip() {
        let netlist = parse_top(
            r#"
            module top (clk, d, q);
              input clk, d;
              output q;
              DFF_X1 r1 (.D(d), .CK(clk), .Q(q));
            endmodule
            "#,
        );
        let sdc_src = r#"
            current_design top
            create_clock -name clk -period 2.0 [get_ports clk]
            set_input_delay -clock clk 0.25 [get_ports d]
            set_output_delay -clock clk 0.35 [get_ports q]
            set_load 0.02 [get_ports q]
            set_input_transition 0.05 [get_ports d]
        "#;
        let (sdc, diags) = rusteda_formats::sdc::read_str(sdc_src);
        assert!(diags.is_empty(), "{diags:?}");

        let compiled = CompiledConstraints::compile(&sdc, &netlist, &[]).expect("compile ok");
        assert_eq!(compiled.current_design.as_deref(), Some("top"));
        assert_eq!(compiled.clocks.len(), 1);
        assert_eq!(compiled.clocks[0].name, "clk");
        assert_eq!(compiled.clocks[0].period_ns, 2.0);
        assert_eq!(compiled.input_delays.len(), 1);
        assert_eq!(compiled.output_delays.len(), 1);
        assert_eq!(compiled.port_loads.len(), 1);
        assert_eq!(compiled.input_transitions.len(), 1);

        let sections = sdc.to_sections();
        let from_sec =
            CompiledConstraints::from_sections(&sections, &netlist, &[]).expect("from_sections ok");
        assert_eq!(compiled.clocks, from_sec.clocks);
        assert_eq!(compiled.input_delays, from_sec.input_delays);
        assert_eq!(compiled.output_delays, from_sec.output_delays);
    }
}
