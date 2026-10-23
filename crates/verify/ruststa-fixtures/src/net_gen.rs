// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Seeded structural Verilog and `GateNetlist` generators (`net_gen`), re-exporting [`crate::r#gen`].

pub use crate::r#gen::{
    CellCatalog, CellSpec, InstSpec, Mutation, NetlistFixture, SplitMix64, apply_mutations,
    gen_chain, gen_clock_crossing, gen_clock_gated, gen_constant_tied, gen_dag, gen_fanout_tree,
    gen_high_fanout_5k, gen_latch_pipeline, gen_mutations, gen_scale_1m, gen_scale_500k,
    gen_scale_netlist, gen_tristate_bus, shrink_mutations, synthetic_netlists,
};
