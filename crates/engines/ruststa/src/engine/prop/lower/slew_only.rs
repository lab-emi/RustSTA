// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The incoming arcs that carry only their output transition (`STA-REQ-0117`): the delay
//! calculation ignores logic values, so the arcs that constants, case analysis and false `when`
//! conditions disable, the library transitions that a sense refinement removes from an enabled
//! arc, and the clock-to-output arcs that logic values disable or that no clock reaches all enter
//! the transition of their destination pin, without an arrival.

use rusteda_compute::timing::{CompiledLutBank, transition_bit};

use crate::engine::graph::{ArcKind, ArcSense, TimingGraph};
use crate::engine::input::NetlistView;
use crate::engine::input::libview::{Sense, TimingLib};
use crate::engine::types::{LibArc, NodeId, Tr};

/// The class of the cell arc `(inst, local_arc)` with the sense `sense` (`None`: the library
/// sense); a refined sense appends a class to `luts`.
pub(super) fn class_for(
    netview: &NetlistView<'_>,
    lib: &TimingLib,
    cell_arcs: &[Vec<LibArc>],
    luts: &mut CompiledLutBank<'_>,
    inst: u32,
    local_arc: LibArc,
    sense: Option<ArcSense>,
) -> u32 {
    let cell_row = netview.frozen().instances[inst as usize].cell as usize;
    let global_arc = cell_arcs[cell_row][local_arc.index()];
    let base_class = global_arc.0;
    let Some(sense) = sense else {
        return base_class;
    };
    let desired_sense = lib_sense(sense);
    let lib_prop = lib.arc(global_arc);
    if lib_prop.sense == desired_sense {
        base_class
    } else {
        let mut refined_prop = lib_prop.clone();
        refined_prop.sense = desired_sense;
        let mut refined_class = lib.lut_bank.classes[base_class as usize];
        refined_class.flags = refined_prop.transition_flags();
        let classes = luts.classes.to_mut();
        let idx = classes.len() as u32;
        classes.push(refined_class);
        idx
    }
}

fn lib_sense(sense: ArcSense) -> Sense {
    match sense {
        ArcSense::PositiveUnate => Sense::Positive,
        ArcSense::NegativeUnate => Sense::Negative,
        ArcSense::NonUnate => Sense::Non,
    }
}

/// The arcs that carry only a transition, as `(dst rank, src rank, csr_arc_to_graph_arc value,
/// class)` sorted: a graph arc `ai` maps to `ai`, a launch arc `li` to `graph.arcs().len() + li`.
/// Only arcs that rise in level are kept (the levels leave out those that would close a loop,
/// `T0149`).
pub(super) fn slew_only_arcs(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    cell_arcs: &[Vec<LibArc>],
    luts: &mut CompiledLutBank<'_>,
) -> Vec<(u32, u32, u32, u32)> {
    let g_arcs = graph.arcs();
    let n_graph_arcs = g_arcs.len() as u32;
    let rises = |src: NodeId, dst: NodeId| graph.level(src) < graph.level(dst);
    let mut out: Vec<(u32, u32, u32, u32)> = Vec::new();
    for (ai, g_arc) in g_arcs.iter().enumerate() {
        if !rises(g_arc.src, g_arc.dst) {
            continue;
        }
        let class = match (graph.arc_disable_reason(ai), g_arc.kind) {
            (Some(r), ArcKind::Net { .. }) if r.carries_transition() => lib.wire_class,
            (Some(r), kind) if r.carries_transition() => {
                let Some((inst, local_arc)) = kind.inst_and_lib_arc() else {
                    continue;
                };
                class_for(netview, lib, cell_arcs, luts, inst, local_arc, None)
            }
            (None, kind) => {
                // An enabled arc whose sense constant propagation refined: its library
                // transitions still enter the pin's transition.
                let Some((inst, local_arc)) = kind.inst_and_lib_arc() else {
                    continue;
                };
                let cell_row = netview.frozen().instances[inst as usize].cell as usize;
                let global_arc = cell_arcs[cell_row][local_arc.index()];
                if lib.arc(global_arc).sense == lib_sense(g_arc.sense) || kind.is_latch_data() {
                    continue;
                }
                class_for(netview, lib, cell_arcs, luts, inst, local_arc, None)
            }
            _ => continue,
        };
        out.push((
            graph.topo_rank(g_arc.dst),
            graph.topo_rank(g_arc.src),
            ai as u32,
            class,
        ));
    }
    for (li, l) in graph.launches().iter().enumerate() {
        if !graph.launch_carries_transition(li) || !rises(l.clock_pin, l.out_pin) {
            continue;
        }
        // A clock-to-output arc maps the active clock edge to both output transitions that have
        // tables.
        let cell_row = netview.frozen().instances[l.inst as usize].cell as usize;
        let global_arc = cell_arcs[cell_row][l.lib_arc.index()];
        let prop = lib.arc(global_arc);
        let mut class = lib.lut_bank.classes[global_arc.0 as usize];
        class.flags = Tr::ALL
            .iter()
            .filter(|to| prop.slew[to.index()] != crate::engine::delay::lut::NO_TABLE)
            .fold(0, |f, to| f | transition_bit(l.edge.index(), to.index()));
        if class.flags == 0 {
            continue;
        }
        let classes = luts.classes.to_mut();
        let idx = classes.len() as u32;
        classes.push(class);
        out.push((
            graph.topo_rank(l.out_pin),
            graph.topo_rank(l.clock_pin),
            n_graph_arcs + li as u32,
            idx,
        ));
    }
    out.sort_unstable();
    out
}
