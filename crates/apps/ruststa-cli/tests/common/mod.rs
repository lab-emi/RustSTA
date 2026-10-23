// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Shared helpers of the verification suites. Not every suite uses every helper.

use std::path::Path;

use rusteda_core::liberty::Direction;
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::{Library, read_text};
use rusteda_formats::sdc::{self, Env as SdcEnv, Limits as SdcLimits, Source as SdcSource};
use rusteda_formats::verilog::netlist::read_structural;
use rusteda_workspace::rbin::RbinWriter;
use rusteda_workspace::{Hash64, Invocation, JsonMap, Upstream, Workspace, rlib};
use ruststa_fixtures::liberty::{Variant, library};

#[allow(dead_code)]
pub fn lib(v: Variant) -> Library {
    read_text(&library(v), v.name()).expect("synthetic library")
}

/// A workspace in `dir/ws` whose `rev1` holds the Liberty library `lib_text` (cached under the
/// name `lib_file`, corner `default`), the structural netlist `verilog` and the compiled
/// constraints `sdc_text`, as `rusteda import` commits them.
#[allow(dead_code)]
pub fn netlist_workspace(
    dir: &Path,
    lib_text: &str,
    lib_file: &str,
    verilog: &str,
    sdc_text: &str,
) -> Workspace {
    corners_workspace(dir, &[("default", lib_text, lib_file)], verilog, sdc_text)
}

/// [`netlist_workspace`] with one library per corner: `(corner, lib_text, lib_file)`, in corner
/// order. The pins of the netlist's cells come from the first library.
#[allow(dead_code)]
pub fn corners_workspace(
    dir: &Path,
    corners: &[(&str, &str, &str)],
    verilog: &str,
    sdc_text: &str,
) -> Workspace {
    let inv =
        |command: &str| Invocation::new("ruststa", "0.2.0", command, vec!["ruststa".to_string()]);
    let ws = Workspace::init(dir.join("ws"), &inv("rusteda init"))
        .expect("init")
        .workspace;
    let mut entries = Vec::new();
    let mut first: Option<Library> = None;
    for &(corner, lib_text, lib_file) in corners {
        let lib = read_text(lib_text, lib_file).unwrap_or_else(|e| panic!("{lib_file}: {e}"));
        let cached = rlib::put(&ws, &lib, lib_text.as_bytes()).expect("rlib");
        entries.push(cached.entry(corner, lib_file));
        first.get_or_insert(lib);
    }
    let lib = first.expect("at least one corner");
    let frozen: FrozenNetlist<'static> = read_structural(verilog, "netlist.v", &|cell_name| {
        let cell = lib.cells.iter().find(|c| c.name == cell_name)?;
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
    })
    .expect("netlist")
    .freeze();
    let (sdc_model, _) = sdc::read(
        &[SdcSource::with_hash(
            "constraints.sdc",
            sdc_text,
            Hash64::of(sdc_text.as_bytes()).0,
        )],
        &SdcEnv::default(),
        &SdcLimits::default(),
    );
    let mut txn = ws
        .begin(
            ws.resolve_base("rev0").expect("rev0"),
            &inv("rusteda import"),
        )
        .expect("begin");
    for entry in entries {
        txn.add_library(entry).expect("library");
    }
    rusteda_workspace::netlist::add_to_txn(&mut txn, &frozen, Upstream::new()).expect("netlist");
    let sections = sdc_model.to_sections();
    let mut writer = RbinWriter::new("CONSTR", 1).expect("CONSTR writer");
    for sec in &sections {
        writer
            .section_bytes(u32::from(sec.id), sec.elem_size, sec.bytes.as_slice())
            .expect("section");
    }
    txn.add_rbin("constraints", "constraints.rbin", &writer, Upstream::new())
        .expect("constraints");
    txn.commit(JsonMap::new()).expect("commit rev1");
    ws
}
