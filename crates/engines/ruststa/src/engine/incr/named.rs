// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The named edits of the suite's edit files (`rusteda.edits.v1`) on a timing session (`owner:
//! E11`; `STA-REQ-0603`, `STA-REQ-0604`).
//!
//! The netlist operations of an edit are those of the shared edits module
//! ([`rusteda_workspace::edits::apply`]), so that the netlist a tool commits after accepting
//! edits is the netlist the session timed. Before an edit is applied, the session checks what the
//! shared module cannot, because it needs the cell libraries: a resize keeps the function of the
//! instance, a buffer edit names a non-inverting buffer, and a pin swap exchanges pins the
//! function is symmetric in. A rejected edit leaves the netlist unchanged.

use rusteda_core::job::Diag;
use rusteda_core::liberty::{Cell, Direction, Func};
use rusteda_workspace::edits::{self, Edit};

use super::cone::IncrementalStats;
use super::edit::{DirtyTracker, SessionError};
use super::session::TimingSession;
use super::{find_lib_cell, revision_cell_pins};

/// Code of a resize to a cell with another function (`STA-REQ-0604`).
pub const CODE_RESIZE_FUNCTION: &str = "T0102";
/// Code of a buffer edit on a cell that is not a non-inverting buffer (`STA-REQ-0604`).
pub const CODE_NOT_A_BUFFER: &str = "T0103";
/// Code of a pin swap of pins that the function is not symmetric in (`STA-REQ-0604`).
pub const CODE_NOT_SYMMETRIC: &str = "T0104";
/// Code of a cell that no library of the session has (`STA-REQ-0604`).
pub const CODE_UNKNOWN_CELL: &str = "T0105";

impl TimingSession {
    /// Applies the named edits `edits` in order and updates the timing once (`STA-REQ-0603`).
    /// When an edit is rejected, the edits before it stay applied, the timing is updated for
    /// them, and the error names the position of the rejected edit.
    pub fn apply_named_edits(&mut self, edits: &[Edit]) -> Result<IncrementalStats, SessionError> {
        let mut tr = DirtyTracker::default();
        let mut failure = None;
        for (k, edit) in edits.iter().enumerate() {
            if let Err(e) = self.apply_named(edit, &mut tr) {
                failure = Some(at_index(e, k));
                break;
            }
        }
        if let Some(e) = failure {
            if !tr.insts.is_empty() || !tr.nets.is_empty() || !tr.keys.is_empty() {
                self.recompute(&tr)?;
            }
            return Err(e);
        }
        self.recompute(&tr)
    }

    /// Checks `edit` against the libraries and applies it to the netlist, marking what it
    /// changed; timing is not updated.
    fn apply_named(&mut self, edit: &Edit, tr: &mut DirtyTracker) -> Result<(), SessionError> {
        self.check_function(edit)?;
        let corner_libs = &self.corner_libs;
        let applied = edits::apply(&mut self.netlist, edit, &|name| {
            revision_cell_pins(corner_libs, name)
        })?;
        for &inst in &applied.instances {
            tr.mark_inst(&self.netlist, inst);
        }
        for &net in &applied.nets {
            tr.mark_net(&self.netlist, net);
        }
        Ok(())
    }

    /// The library cell of the first corner named `name`, or error `T0105`.
    fn lib_cell(&self, name: &str) -> Result<&Cell, SessionError> {
        find_lib_cell(&self.corner_libs[0].1, name).ok_or_else(|| {
            reject(
                CODE_UNKNOWN_CELL,
                format!("cell '{name}' is in no library of the first corner"),
            )
        })
    }

    /// The library cell of instance `instance`, when the instance exists.
    fn instance_cell(&self, instance: &str) -> Result<Option<&Cell>, SessionError> {
        let Some(inst) = self.netlist.find_instance(instance) else {
            return Ok(None);
        };
        let cell = self.netlist.instance(inst)?.cell;
        let name = self
            .netlist
            .symbols()
            .resolve(self.netlist.cell(cell)?.name);
        self.lib_cell(name).map(Some)
    }

    /// The checks of `edit` that need the libraries (`STA-REQ-0604`). An edit whose instance
    /// does not exist passes them, and the shared module rejects it.
    fn check_function(&self, edit: &Edit) -> Result<(), SessionError> {
        match edit {
            Edit::Resize { instance, cell } => {
                let new = self.lib_cell(cell)?;
                if let Some(old) = self.instance_cell(instance)?
                    && !same_function(old, new)
                {
                    return Err(reject(
                        CODE_RESIZE_FUNCTION,
                        format!(
                            "cell '{cell}' does not have the function of '{}', the cell of \
                             instance '{instance}'",
                            old.name
                        ),
                    ));
                }
            }
            Edit::InsertBuffer { cell, .. } => {
                let c = self.lib_cell(cell)?;
                if !is_buffer(c) {
                    return Err(reject(
                        CODE_NOT_A_BUFFER,
                        format!("cell '{cell}' is not a non-inverting buffer"),
                    ));
                }
            }
            Edit::RemoveBuffer { instance } => {
                if let Some(c) = self.instance_cell(instance)?
                    && !is_buffer(c)
                {
                    return Err(reject(
                        CODE_NOT_A_BUFFER,
                        format!(
                            "instance '{instance}' of cell '{}' is not a non-inverting buffer",
                            c.name
                        ),
                    ));
                }
            }
            Edit::SwapPins {
                instance,
                pin_a,
                pin_b,
            } => {
                if let Some(c) = self.instance_cell(instance)?
                    && pin_a != pin_b
                    && c.pin(pin_a).is_some()
                    && c.pin(pin_b).is_some()
                    && !symmetric(c, pin_a, pin_b)
                {
                    return Err(reject(
                        CODE_NOT_SYMMETRIC,
                        format!(
                            "the function of cell '{}' is not symmetric in pins '{pin_a}' and \
                             '{pin_b}'",
                            c.name
                        ),
                    ));
                }
            }
            Edit::Clone { .. } => {}
        }
        Ok(())
    }
}

fn reject(code: &'static str, message: String) -> SessionError {
    SessionError::Diag(vec![Diag::error(code, message)])
}

/// `e` with the position `k` of the edit in its list.
fn at_index(e: SessionError, k: usize) -> SessionError {
    match e {
        SessionError::Edit(err) => SessionError::Edit(err.within(&format!("[{k}]"))),
        SessionError::Diag(mut ds) => {
            for d in &mut ds {
                d.message = format!("edit {k}: {}", d.message);
            }
            SessionError::Diag(ds)
        }
        other => other,
    }
}

/// The input pins of `cell`, sorted by name.
fn input_names(cell: &Cell) -> Vec<&str> {
    let mut v: Vec<&str> = cell.inputs().map(|p| p.name.as_str()).collect();
    v.sort_unstable();
    v
}

/// Two functions are equal: equal truth tables over `vars` (at most 6), else equal trees.
fn same_func(a: Option<&Func>, b: Option<&Func>, vars: &[&str]) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => match (x.truth_table(vars), y.truth_table(vars)) {
            (Some(p), Some(q)) => p == q,
            _ => x == y,
        },
        _ => false,
    }
}

/// True when cells `a` and `b` have the same pins and the same function (`STA-REQ-0604`): the
/// same function and three-state function of every output pin (as truth tables over the input
/// pins), the same state function, the same sequential groups and state table, and the same
/// clock-gating role. A pin without a function makes the cells differ unless they are one cell.
pub fn same_function(a: &Cell, b: &Cell) -> bool {
    if a.name == b.name {
        return true;
    }
    fn pins(c: &Cell) -> Vec<(&str, Direction)> {
        let mut v: Vec<(&str, Direction)> = c
            .pins
            .iter()
            .map(|p| (p.name.as_str(), p.direction))
            .collect();
        v.sort_unstable_by(|x, y| x.0.cmp(y.0));
        v
    }
    if pins(a) != pins(b)
        || format!("{:?}", a.sequentials) != format!("{:?}", b.sequentials)
        || a.statetable != b.statetable
        || a.has_statetable != b.has_statetable
        || a.is_clock_gating != b.is_clock_gating
        || a.clock_gating_integrated_cell != b.clock_gating_integrated_cell
    {
        return false;
    }
    let vars = input_names(a);
    a.pins
        .iter()
        .filter(|p| matches!(p.direction, Direction::Output | Direction::Inout))
        .all(|pa| {
            let Some(pb) = b.pin(&pa.name) else {
                return false;
            };
            pa.function.is_some()
                && same_func(pa.function.as_ref(), pb.function.as_ref(), &vars)
                && same_func(
                    pa.three_state_function.as_ref(),
                    pb.three_state_function.as_ref(),
                    &vars,
                )
                && same_func(
                    pa.state_function.as_ref(),
                    pb.state_function.as_ref(),
                    &vars,
                )
        })
}

/// True when `cell` is a non-inverting buffer (`STA-REQ-0604`): one input and one output pin, the
/// output's function the input, no three-state function and no state.
pub fn is_buffer(cell: &Cell) -> bool {
    let (inputs, outputs): (Vec<_>, Vec<_>) = cell
        .pins
        .iter()
        .partition(|p| p.direction == Direction::Input);
    if cell.pins.len() != 2
        || inputs.len() != 1
        || outputs.len() != 1
        || outputs[0].direction != Direction::Output
        || !cell.sequentials.is_empty()
        || cell.has_statetable
        || cell.is_clock_gating
    {
        return false;
    }
    let vars = [inputs[0].name.as_str()];
    let identity = Func::Var(inputs[0].name.clone()).truth_table(&vars);
    let tt = outputs[0]
        .function
        .as_ref()
        .and_then(|f| f.truth_table(&vars));
    outputs[0].three_state_function.is_none() && tt.is_some() && tt == identity
}

/// True when the function of `cell` is symmetric in its input pins `a` and `b`
/// (`STA-REQ-0604`): exchanging them changes no function or three-state function of an output.
/// A cell with state, or an output without a function, is not symmetric.
pub fn symmetric(cell: &Cell, a: &str, b: &str) -> bool {
    let is_input = |n: &str| cell.pin(n).is_some_and(|p| p.direction == Direction::Input);
    if !is_input(a) || !is_input(b) || !cell.sequentials.is_empty() || cell.has_statetable {
        return false;
    }
    let vars = input_names(cell);
    if vars.len() > 6 {
        return false;
    }
    let swapped: Vec<&str> = vars
        .iter()
        .map(|&v| {
            if v == a {
                b
            } else if v == b {
                a
            } else {
                v
            }
        })
        .collect();
    let same = |f: Option<&Func>| match f {
        None => true,
        Some(f) => {
            f.truth_table(&vars).is_some() && f.truth_table(&vars) == f.truth_table(&swapped)
        }
    };
    cell.pins
        .iter()
        .filter(|p| matches!(p.direction, Direction::Output | Direction::Inout))
        .all(|p| {
            p.function.is_some()
                && same(p.function.as_ref())
                && same(p.three_state_function.as_ref())
        })
}
