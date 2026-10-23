// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Gate-level netlists of synthetic designs (`verification/fixtures/synthetic/sta/netlists/`).
//!
//! The corpus netlists were synthesized from hand-written synthetic RTL with the `wt_full`
//! library ([`crate::liberty::Variant::Full`]) for delay at medium effort; every cell they
//! instantiate is a cell of that library. `LANES_K1` is one lane of the multiply-accumulate
//! datapath of the backend benchmark, synthesized with default options. The files are checked in;
//! `cargo xtask fixtures` does not regenerate them.

/// A flat structural Verilog netlist with one module, `top`.
#[derive(Debug, Clone, Copy)]
pub struct Netlist {
    /// Design name, also the file stem.
    pub name: &'static str,
    /// The module of the netlist.
    pub top: &'static str,
    /// Verilog text.
    pub text: &'static str,
}

macro_rules! netlist {
    ($name:literal) => {
        Netlist {
            name: $name,
            top: $name,
            text: include_str!(concat!(
                "../../../../verification/fixtures/synthetic/sta/netlists/",
                $name,
                ".v"
            )),
        }
    };
}

/// The corpus: small control and datapath blocks up to a pipelined processor core (`rv_core`).
pub const CORPUS: &[Netlist] = &[
    netlist!("counter"),
    netlist!("shift_reg"),
    netlist!("seq_det"),
    netlist!("fifo"),
    netlist!("alu"),
    netlist!("prio_enc"),
    netlist!("rr_arb"),
    netlist!("crc8"),
    netlist!("lfsr"),
    netlist!("gray"),
    netlist!("sync_edge"),
    netlist!("mult"),
    netlist!("barrel"),
    netlist!("regfile"),
    netlist!("uart_tx"),
    netlist!("pipeline"),
    netlist!("bin2bcd"),
    netlist!("debounce"),
    netlist!("pwm"),
    netlist!("latches"),
    netlist!("set_neg"),
    netlist!("div_const"),
    netlist!("sat_add"),
    netlist!("decoder"),
    netlist!("subroutines"),
    netlist!("mem2d"),
    netlist!("skid"),
    netlist!("old_style"),
    netlist!("top_hier"),
    netlist!("rv_core"),
    netlist!("zero_repl"),
];

/// One lane of the benchmark datapath: module `lanes` with ports `clk`, `a[15:0]`, `b[15:0]`,
/// `c[15:0]`, `y[31:0]` and `gt[0:0]`; a 16-by-16-bit multiply-accumulate into a 32-bit
/// register and a comparison of its upper half.
pub const LANES_K1: Netlist = Netlist {
    name: "lanes_k1",
    top: "lanes",
    text: include_str!("../../../../verification/fixtures/synthetic/sta/netlists/lanes_k1.v"),
};

/// The corpus netlist named `name`.
pub fn corpus(name: &str) -> Option<&'static Netlist> {
    CORPUS.iter().find(|n| n.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_netlist_defines_its_top() {
        for n in CORPUS.iter().chain(std::iter::once(&LANES_K1)) {
            assert!(
                n.text.starts_with(&format!("module {}(", n.top)),
                "{}: first module is not {}",
                n.name,
                n.top
            );
            assert_eq!(n.text.matches("\nendmodule").count(), 1, "{}", n.name);
        }
    }
}
