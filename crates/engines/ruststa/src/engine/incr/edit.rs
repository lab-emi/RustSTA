// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Low-level edits of a [`TimingSession`] by netlist id (`owner: E11`; `STA-REQ-0451`).
//!
//! Each edit is one or a few [`GateNetlist`] operations; the objects it touches are marked dirty
//! before and after it, and a batch of edits is followed by one incremental update. The named
//! edits of the suite's edit files are in [`named`](super::named).

use std::fmt;

use rustc_hash::FxHashSet;
use rusteda_compute::ComputeError;
use rusteda_core::job::Diag;
use rusteda_core::netlist::{CellId, Conn, GateNetlist, InstId, NetId, NetlistError, PinId};
use rusteda_workspace::edits::EditsError;

use super::collect_instance_nets;
use super::cone::{IncrementalStats, SessionNodeKey};
use super::session::TimingSession;
use super::verify::BitwiseMismatch;

/// A netlist edit by id applicable to a [`TimingSession`] (`STA-REQ-0451`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEdit {
    /// Swaps or resizes `inst` to `new_cell` with matching pin names (`GateNetlist::swap_cell`).
    SwapCell { inst: InstId, new_cell: CellId },
    /// Inserts a buffer instance driving `loads` on a new net (`GateNetlist::insert_buffer`).
    InsertBuffer {
        net: NetId,
        loads: Vec<PinId>,
        buf_name: String,
        buf_cell: CellId,
        in_pin: String,
        out_pin: String,
        new_net_name: Option<String>,
    },
    /// Removes a buffer instance and merges its output loads back onto its input net.
    RemoveBuffer {
        buf_inst: InstId,
        in_pin: String,
        out_pin: String,
    },
    /// Connects `pin_name` of `inst` to `net` (`GateNetlist::connect`).
    ConnectPin {
        inst: InstId,
        pin_name: String,
        net: NetId,
    },
    /// Disconnects `pin_name` of `inst` from its current net (`GateNetlist::disconnect`).
    DisconnectPin { inst: InstId, pin_name: String },
    /// Moves `pin_name` of `inst` from its current net (if any) to `new_net`.
    MovePin {
        inst: InstId,
        pin_name: String,
        new_net: NetId,
    },
    /// Adds an unconnected instance of `cell` named `name` (`GateNetlist::add_instance`).
    AddInstance { name: String, cell: CellId },
    /// Removes `inst` and disconnects all of its pins (`GateNetlist::remove_instance`).
    RemoveInstance { inst: InstId },
    /// Adds an unconnected net (`GateNetlist::add_net`).
    AddNet { name: Option<String> },
    /// Removes an unconnected net (`GateNetlist::remove_net`).
    RemoveNet { net: NetId },
}

/// Error returned by [`TimingSession`] construction or edit operations.
#[derive(Debug)]
pub enum SessionError {
    /// A netlist operation failed; the netlist is unchanged by it.
    Netlist(NetlistError),
    /// The analysis or an engine check rejected the input, with coded diagnostics.
    Diag(Vec<Diag>),
    /// The compute backend failed.
    Compute(ComputeError),
    /// No instance has the name.
    UnknownInstance(String),
    /// No library cell has the name.
    UnknownCell(String),
    /// No net has the name.
    UnknownNet(String),
    /// The instance has no such pin.
    UnknownPin { inst: String, pin: String },
    /// An edit of an edit file failed (`W03nn`).
    Edit(EditsError),
    /// The incremental state differs from a full analysis.
    BitwiseMismatch(BitwiseMismatch),
}

impl SessionError {
    /// The diagnostic code of the error, when it has one.
    #[must_use]
    pub fn code(&self) -> Option<String> {
        match self {
            Self::Diag(ds) => ds.first().map(|d| d.code.clone()),
            Self::Edit(e) => Some(e.code.as_str().to_string()),
            _ => None,
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Netlist(e) => write!(f, "netlist error: {e}"),
            Self::Diag(ds) => match ds.first() {
                Some(first) => write!(f, "{}: {}", first.code, first.message),
                None => write!(f, "timing analysis diagnostic error"),
            },
            Self::Compute(e) => write!(f, "compute error: {e}"),
            Self::UnknownInstance(name) => write!(f, "unknown instance '{name}'"),
            Self::UnknownCell(name) => write!(f, "unknown cell '{name}'"),
            Self::UnknownNet(name) => write!(f, "unknown net '{name}'"),
            Self::UnknownPin { inst, pin } => write!(f, "unknown pin '{pin}' on '{inst}'"),
            Self::Edit(e) => write!(f, "{}: {e}", e.code),
            Self::BitwiseMismatch(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<NetlistError> for SessionError {
    fn from(value: NetlistError) -> Self {
        Self::Netlist(value)
    }
}

impl From<ComputeError> for SessionError {
    fn from(value: ComputeError) -> Self {
        Self::Compute(value)
    }
}

impl From<BitwiseMismatch> for SessionError {
    fn from(value: BitwiseMismatch) -> Self {
        Self::BitwiseMismatch(value)
    }
}

impl From<EditsError> for SessionError {
    fn from(value: EditsError) -> Self {
        Self::Edit(value)
    }
}

/// The instances, nets and node keys that a batch of edits touched (`STA-REQ-0453`).
#[derive(Debug, Default)]
pub(crate) struct DirtyTracker {
    pub(crate) keys: FxHashSet<SessionNodeKey>,
    pub(crate) insts: FxHashSet<InstId>,
    pub(crate) nets: FxHashSet<NetId>,
}

impl DirtyTracker {
    pub(crate) fn mark_pin(&mut self, pin_id: PinId) {
        self.keys.insert(SessionNodeKey::Pin(pin_id));
    }

    pub(crate) fn mark_inst(&mut self, netlist: &GateNetlist, inst_id: InstId) {
        self.insts.insert(inst_id);
        if let Ok(inst) = netlist.instance(inst_id) {
            for &pin_id in &inst.connected {
                self.keys.insert(SessionNodeKey::Pin(pin_id));
                if let Ok(pin) = netlist.pin(pin_id)
                    && let Some(Conn::Net(net_id)) = pin.conn
                {
                    self.mark_net(netlist, net_id);
                }
            }
        }
    }

    pub(crate) fn mark_net(&mut self, netlist: &GateNetlist, net_id: NetId) {
        self.nets.insert(net_id);
        if let Ok(net) = netlist.net(net_id) {
            for &pin_id in &net.pins {
                self.keys.insert(SessionNodeKey::Pin(pin_id));
            }
        }
        let mut pb = 0u32;
        for port in netlist.ports() {
            for &bit_net in &port.bits {
                if bit_net == net_id {
                    self.keys.insert(SessionNodeKey::PortBit(pb));
                }
                pb += 1;
            }
        }
    }
}

impl TimingSession {
    /// Swaps or resizes `inst` to `new_cell` and incrementally updates timing (`STA-REQ-0451`).
    pub fn swap_cell(
        &mut self,
        inst: InstId,
        new_cell: CellId,
    ) -> Result<IncrementalStats, SessionError> {
        self.apply_edits(&[SessionEdit::SwapCell { inst, new_cell }])
    }

    /// Swaps or resizes `inst_name` to `new_cell_name` by name and incrementally updates timing
    /// (`STA-REQ-0451`).
    pub fn swap_cell_by_name(
        &mut self,
        inst_name: &str,
        new_cell_name: &str,
    ) -> Result<IncrementalStats, SessionError> {
        let inst = self
            .netlist
            .find_instance(inst_name)
            .ok_or_else(|| SessionError::UnknownInstance(inst_name.to_owned()))?;
        let new_cell = self.ensure_cell(new_cell_name)?;
        self.swap_cell(inst, new_cell)
    }

    /// Inserts a buffer driving `loads` on `net` and incrementally updates timing (`STA-REQ-0451`).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_buffer(
        &mut self,
        net: NetId,
        loads: &[PinId],
        buf_name: impl Into<String>,
        buf_cell: CellId,
        in_pin: &str,
        out_pin: &str,
        new_net_name: Option<String>,
    ) -> Result<(InstId, NetId, IncrementalStats), SessionError> {
        let buf_name = buf_name.into();
        let stats = self.apply_edits(&[SessionEdit::InsertBuffer {
            net,
            loads: loads.to_vec(),
            buf_name: buf_name.clone(),
            buf_cell,
            in_pin: in_pin.to_owned(),
            out_pin: out_pin.to_owned(),
            new_net_name,
        }])?;
        let buf_inst = self
            .netlist
            .find_instance(&buf_name)
            .ok_or(SessionError::UnknownInstance(buf_name))?;
        let out_pin_id = self.netlist.find_pin(buf_inst, out_pin)?;
        let Some(Conn::Net(new_net)) = self.netlist.pin(out_pin_id)?.conn else {
            return Err(SessionError::UnknownNet(String::from("<buffer_out>")));
        };
        Ok((buf_inst, new_net, stats))
    }

    /// Removes a buffer instance `buf_inst` and merges its output loads back onto its input net,
    /// then incrementally updates timing (`STA-REQ-0451`).
    pub fn remove_buffer(
        &mut self,
        buf_inst: InstId,
        in_pin: &str,
        out_pin: &str,
    ) -> Result<(NetId, IncrementalStats), SessionError> {
        let in_pin_id = self.netlist.find_pin(buf_inst, in_pin)?;
        let Some(Conn::Net(kept_net)) = self.netlist.pin(in_pin_id)?.conn else {
            return Err(SessionError::UnknownNet(String::from("<buffer_in>")));
        };
        let stats = self.apply_edits(&[SessionEdit::RemoveBuffer {
            buf_inst,
            in_pin: in_pin.to_owned(),
            out_pin: out_pin.to_owned(),
        }])?;
        Ok((kept_net, stats))
    }

    /// Connects `pin_name` of `inst` to `net` and incrementally updates timing (`STA-REQ-0451`).
    pub fn connect_pin(
        &mut self,
        inst: InstId,
        pin_name: &str,
        net: NetId,
    ) -> Result<(PinId, IncrementalStats), SessionError> {
        let stats = self.apply_edits(&[SessionEdit::ConnectPin {
            inst,
            pin_name: pin_name.to_owned(),
            net,
        }])?;
        let pin_id = self.netlist.find_pin(inst, pin_name)?;
        Ok((pin_id, stats))
    }

    /// Disconnects `pin_name` of `inst` and incrementally updates timing (`STA-REQ-0451`).
    pub fn disconnect_pin(
        &mut self,
        inst: InstId,
        pin_name: &str,
    ) -> Result<IncrementalStats, SessionError> {
        self.apply_edits(&[SessionEdit::DisconnectPin {
            inst,
            pin_name: pin_name.to_owned(),
        }])
    }

    /// Moves `pin_name` of `inst` onto `new_net` and incrementally updates timing (`STA-REQ-0451`).
    pub fn move_pin(
        &mut self,
        inst: InstId,
        pin_name: &str,
        new_net: NetId,
    ) -> Result<(PinId, IncrementalStats), SessionError> {
        let stats = self.apply_edits(&[SessionEdit::MovePin {
            inst,
            pin_name: pin_name.to_owned(),
            new_net,
        }])?;
        let pin_id = self.netlist.find_pin(inst, pin_name)?;
        Ok((pin_id, stats))
    }

    /// Adds an unconnected instance of `cell` and incrementally updates timing (`STA-REQ-0451`).
    pub fn add_instance(
        &mut self,
        name: impl Into<String>,
        cell: CellId,
    ) -> Result<(InstId, IncrementalStats), SessionError> {
        let name = name.into();
        let stats = self.apply_edits(&[SessionEdit::AddInstance {
            name: name.clone(),
            cell,
        }])?;
        let inst = self
            .netlist
            .find_instance(&name)
            .ok_or(SessionError::UnknownInstance(name))?;
        Ok((inst, stats))
    }

    /// Removes `inst` (disconnecting all of its pins) and incrementally updates timing
    /// (`STA-REQ-0451`).
    pub fn remove_instance(&mut self, inst: InstId) -> Result<IncrementalStats, SessionError> {
        self.apply_edits(&[SessionEdit::RemoveInstance { inst }])
    }

    /// Adds an unconnected net and incrementally updates timing (`STA-REQ-0451`).
    pub fn add_net(
        &mut self,
        name: Option<String>,
    ) -> Result<(NetId, IncrementalStats), SessionError> {
        let net = self.netlist.add_net(name.as_deref(), None)?;
        let mut tr = DirtyTracker::default();
        tr.mark_net(&self.netlist, net);
        let stats = self.recompute(&tr)?;
        Ok((net, stats))
    }

    /// Removes an unconnected net and incrementally updates timing (`STA-REQ-0451`).
    pub fn remove_net(&mut self, net: NetId) -> Result<IncrementalStats, SessionError> {
        self.apply_edits(&[SessionEdit::RemoveNet { net }])
    }

    /// Applies a batch of [`SessionEdit`]s, coalescing all dirty instances, nets, and pins into a
    /// single incremental update (`STA-REQ-0451`). When an edit fails, the edits before it stay
    /// applied, the timing is updated for them, and the error of the failed edit is returned.
    pub fn apply_edits(&mut self, edits: &[SessionEdit]) -> Result<IncrementalStats, SessionError> {
        let mut tr = DirtyTracker::default();
        let mut failure = None;
        for edit in edits {
            if let Err(e) = self.apply_one(edit, &mut tr) {
                failure = Some(e);
                break;
            }
        }
        if let Some(e) = failure {
            if !tr.keys.is_empty() || !tr.insts.is_empty() || !tr.nets.is_empty() {
                self.recompute(&tr)?;
            }
            return Err(e);
        }
        self.recompute(&tr)
    }

    fn apply_one(&mut self, edit: &SessionEdit, tr: &mut DirtyTracker) -> Result<(), SessionError> {
        match edit {
            SessionEdit::SwapCell { inst, new_cell } => {
                self.netlist.swap_cell(*inst, *new_cell)?;
                tr.mark_inst(&self.netlist, *inst);
            }
            SessionEdit::InsertBuffer {
                net,
                loads,
                buf_name,
                buf_cell,
                in_pin,
                out_pin,
                new_net_name,
            } => {
                let (buf_inst, new_net) = self.netlist.insert_buffer(
                    *net,
                    loads,
                    *buf_cell,
                    in_pin.as_str(),
                    out_pin.as_str(),
                    buf_name,
                    new_net_name.as_deref(),
                )?;
                for &lp in loads {
                    tr.mark_pin(lp);
                }
                tr.mark_inst(&self.netlist, buf_inst);
                tr.mark_net(&self.netlist, *net);
                tr.mark_net(&self.netlist, new_net);
            }
            SessionEdit::RemoveBuffer { buf_inst, .. } => {
                tr.mark_inst(&self.netlist, *buf_inst);
                let kept_net = self.netlist.remove_buffer(*buf_inst)?;
                tr.mark_net(&self.netlist, kept_net);
            }
            SessionEdit::ConnectPin {
                inst,
                pin_name,
                net,
            } => {
                let pin_id = self
                    .netlist
                    .connect(*inst, pin_name.as_str(), Conn::Net(*net))?;
                tr.mark_pin(pin_id);
                tr.mark_inst(&self.netlist, *inst);
                tr.mark_net(&self.netlist, *net);
            }
            SessionEdit::DisconnectPin { inst, pin_name } => {
                let pin_id = self.netlist.find_pin(*inst, pin_name.as_str())?;
                let old = self.netlist.disconnect(pin_id)?;
                tr.mark_pin(pin_id);
                if let Some(Conn::Net(old_net)) = old {
                    tr.mark_net(&self.netlist, old_net);
                }
                tr.mark_inst(&self.netlist, *inst);
            }
            SessionEdit::MovePin {
                inst,
                pin_name,
                new_net,
            } => {
                let pin_id = self.netlist.find_pin(*inst, pin_name.as_str())?;
                let old_net = match self.netlist.pin(pin_id)?.conn {
                    Some(Conn::Net(n)) => Some(n),
                    _ => None,
                };
                if old_net.is_some() {
                    self.netlist.move_pin(pin_id, Conn::Net(*new_net))?;
                } else {
                    self.netlist
                        .connect(*inst, pin_name.as_str(), Conn::Net(*new_net))?;
                }
                tr.mark_pin(pin_id);
                if let Some(old_net) = old_net {
                    tr.mark_net(&self.netlist, old_net);
                }
                tr.mark_net(&self.netlist, *new_net);
                tr.mark_inst(&self.netlist, *inst);
            }
            SessionEdit::AddInstance { name, cell } => {
                let inst = self.netlist.add_instance(name, *cell)?;
                tr.mark_inst(&self.netlist, inst);
            }
            SessionEdit::RemoveInstance { inst } => {
                let incident_nets = collect_instance_nets(&self.netlist, *inst);
                tr.mark_inst(&self.netlist, *inst);
                self.netlist.remove_instance(*inst)?;
                for net_id in incident_nets {
                    tr.mark_net(&self.netlist, net_id);
                }
            }
            SessionEdit::AddNet { name } => {
                let net = self.netlist.add_net(name.as_deref(), None)?;
                tr.mark_net(&self.netlist, net);
            }
            SessionEdit::RemoveNet { net } => {
                self.netlist.remove_net(*net)?;
                tr.nets.insert(*net);
            }
        }
        Ok(())
    }
}
