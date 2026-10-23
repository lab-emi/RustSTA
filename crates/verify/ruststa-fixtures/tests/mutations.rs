// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tests for the pin-compatible netlist mutation generator, deterministic replay, and delta-debugging
//! shrinker.

use ruststa_fixtures::net_gen::{
    Mutation, apply_mutations, gen_dag, gen_mutations, shrink_mutations,
};

#[test]
fn mutation_replay_is_deterministic_and_produces_valid_netlist() {
    let base = gen_dag(0x2026, 20, 2);
    let m1 = gen_mutations(0x9999, 50);
    let m2 = gen_mutations(0x9999, 50);
    assert_eq!(m1, m2);

    let out1 = apply_mutations(&base, &m1);
    let out2 = apply_mutations(&base, &m2);
    assert_eq!(out1.verilog, out2.verilog);

    let nl = out1.to_gate_netlist();
    assert!(nl.instance_count() >= base.gate_count());
}

#[test]
fn shrinker_reduces_planted_100_step_failure_below_20_steps() {
    let base = gen_dag(0x1234, 16, 2);
    let gate_idx = base
        .instances
        .iter()
        .position(|inst| inst.conns.len() == 3)
        .expect("at least one 2-input gate");
    let mut seq = gen_mutations(0x5678, 100);
    // Plant a 2-step failure trigger at steps 37 and 63 that random steps never overwrite.
    for m in &mut seq {
        if let Mutation::SwapCell { inst_idx, .. } = m
            && *inst_idx % base.instances.len() == gate_idx
        {
            *inst_idx += 1;
        }
    }
    seq[37] = Mutation::SwapCell {
        inst_idx: gate_idx,
        new_cell: "wt_xnor2_1".into(),
    };
    seq[63] = Mutation::InsertBuffer {
        inst_idx: 5,
        buf_cell: "wt_inv_2".into(),
    };

    let fails = |candidate: &[Mutation]| -> bool {
        let mutated = apply_mutations(&base, candidate);
        let has_xnor = mutated
            .instances
            .get(gate_idx)
            .is_some_and(|inst| inst.cell_name == "wt_xnor2_1");
        let has_inv2_buf = mutated
            .instances
            .iter()
            .any(|inst| inst.inst_name.starts_with("mut_buf_") && inst.cell_name == "wt_inv_2");
        has_xnor && has_inv2_buf
    };

    assert!(fails(&seq));
    let shrunk = shrink_mutations(&seq, fails);
    assert!(
        shrunk.len() <= 20,
        "expected shrunk sequence <= 20 steps, got {}",
        shrunk.len()
    );
    assert!(fails(&shrunk));
}
