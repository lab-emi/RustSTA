// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Atomic import and dependency invalidation (RCLI-REQ-0070, STA-REQ-0700).
use rusteda_workspace::{Invocation, RbinWriter, Upstream, Workspace};
use serde_json::Value;
use std::{
    fs,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
fn run(args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_rusteda"))
        .args(["--format", "json"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{:?}", out))
}
#[test]
fn spef_requires_corner_limits_and_invalidates_old_timing() {
    static ID: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "spef-import-{}-{}",
        std::process::id(),
        ID.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let ws_path = dir.join("ws");
    let w = ws_path.to_str().unwrap();
    let lib = dir.join("cells.lib");
    let net = dir.join("top.v");
    let spef = dir.join("top.spef");
    fs::write(&lib, "library (l) { time_unit : \"1ns\"; capacitive_load_unit(1,pf); cell (BUF) { pin(A) {direction: input;} pin(Y) {direction: output; function: \"A\";} } }").unwrap();
    fs::write(
        &net,
        "module top(input a, output y); BUF u(.A(a),.Y(y)); endmodule\n",
    )
    .unwrap();
    fs::write(&spef,"*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n*T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 OHM\n*D_NET a 0\n*CONN\n*P a I\n*I u:A I\n*RES\n1 a u:A 2\n*END\n").unwrap();
    assert_eq!(run(&["init", "-w", w])["new_rev"], "rev0");
    assert_eq!(
        run(&[
            "import",
            "-w",
            w,
            "--base-rev",
            "rev0",
            "--lib",
            lib.to_str().unwrap(),
            "--corner",
            "tt",
            "--netlist",
            net.to_str().unwrap()
        ])["new_rev"],
        "rev1"
    );
    // Deliberately stage an old timing table without a parasitic dependency.
    {
        let ws = Workspace::open(&ws_path).unwrap();
        let base = ws.resolve_base("rev1").unwrap();
        let inv = Invocation::new("test", "0.1.0", "seed", ["seed"]);
        let mut tx = ws.begin(base, &inv).unwrap();
        let mut writer = RbinWriter::new("STA", 1).unwrap();
        writer.section_bytes(1, 1, &b"old"[..]).unwrap();
        tx.add_rbin("sta", "sta.rbin", &writer, Upstream::new())
            .unwrap();
        tx.commit(Default::default()).unwrap();
    }
    let imported = run(&[
        "import",
        "-w",
        w,
        "--base-rev",
        "rev2",
        "--spef",
        spef.to_str().unwrap(),
        "--spef-corner",
        "tt",
    ]);
    assert_eq!(imported["ok"], true, "{imported}");
    assert!(imported["tables"].get("sta").is_none());
    assert!(imported["tables"].get("parasitics").is_some());
    let conflict = run(&[
        "import",
        "-w",
        w,
        "--base-rev",
        "rev2",
        "--spef",
        spef.to_str().unwrap(),
        "--spef-corner",
        "tt",
    ]);
    assert_eq!(conflict["error"]["code"], "REVISION_CONFLICT");
    fs::write(&spef, "*NOT_SUPPORTED\n").unwrap();
    let bad = run(&[
        "import",
        "-w",
        w,
        "--base-rev",
        "rev3",
        "--spef",
        spef.to_str().unwrap(),
        "--spef-corner",
        "tt",
    ]);
    assert_eq!(bad["ok"], false);
    assert_eq!(run(&["show", "-w", w])["rev"], "rev3");
    fs::remove_dir_all(&dir).unwrap();
}

// RCLI-REQ-0073: gzip is a bounded transport; canonical table bytes do not depend on it.
#[test]
fn compressed_sources_are_bounded_and_fail_atomically() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("spef-compressed-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let ws_path = dir.join("ws");
    let w = ws_path.to_str().unwrap();
    let lib = dir.join("cells.lib");
    let net = dir.join("top.v");
    let plain = dir.join("input.spef");
    let compressed = dir.join("input.spef.gz");
    fs::write(&lib,"library (l) { time_unit : \"1ns\"; capacitive_load_unit(1,pf); cell(BUF){pin(A){direction:input;}pin(Y){direction:output;function:\"A\";}}}").unwrap();
    fs::write(
        &net,
        "module top(input a,output y);BUF u(.A(a),.Y(y));endmodule\n",
    )
    .unwrap();
    let source = "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n*T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 OHM\n*D_NET a 0\n*CONN\n*P a I\n*I u:A I\n*RES\n1 a u:A 2\n*END\n";
    fs::write(&plain, source).unwrap();
    let gzip = |bytes: &[u8]| {
        let mut g = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        g.write_all(bytes).unwrap();
        g.finish().unwrap()
    };
    fs::write(&compressed, gzip(source.as_bytes())).unwrap();
    assert_eq!(run(&["init", "-w", w])["new_rev"], "rev0");
    assert_eq!(
        run(&[
            "import",
            "-w",
            w,
            "--base-rev",
            "rev0",
            "--lib",
            lib.to_str().unwrap(),
            "--corner",
            "tt",
            "--netlist",
            net.to_str().unwrap()
        ])["new_rev"],
        "rev1"
    );
    let a = run(&[
        "import",
        "-w",
        w,
        "--base-rev",
        "rev1",
        "--spef",
        plain.to_str().unwrap(),
        "--spef-corner",
        "tt",
    ]);
    assert_eq!(a["ok"], true, "{a}");
    let b = run(&[
        "import",
        "-w",
        w,
        "--base-rev",
        "rev2",
        "--spef",
        compressed.to_str().unwrap(),
        "--spef-corner",
        "tt",
    ]);
    assert_eq!(b["ok"], true, "{b}");
    assert_eq!(b["metrics"]["spef_compression"], "gzip");
    let ws = Workspace::open(&ws_path).unwrap();
    let a = ws.revision(ws.resolve_query("rev2").unwrap()).unwrap();
    let b = ws.revision(ws.resolve_query("rev3").unwrap()).unwrap();
    let x = a.open_rbin("parasitics", "PARASIT", 1..=1).unwrap();
    let y = b.open_rbin("parasitics", "PARASIT", 1..=1).unwrap();
    assert_eq!(x.section_bytes(1).unwrap(), y.section_bytes(1).unwrap());
    drop((x, y, a, b));
    let mut corrupt = gzip(source.as_bytes());
    let end = corrupt.len() - 8;
    corrupt[end] ^= 1;
    fs::write(&compressed, corrupt).unwrap();
    let bad = run(&[
        "import",
        "-w",
        w,
        "--base-rev",
        "rev3",
        "--spef",
        compressed.to_str().unwrap(),
        "--spef-corner",
        "tt",
    ]);
    assert_eq!(bad["ok"], false);
    fs::write(&compressed, gzip(&vec![b'x'; 2 << 20])).unwrap();
    let bad = run(&[
        "import",
        "-w",
        w,
        "--base-rev",
        "rev3",
        "--spef",
        compressed.to_str().unwrap(),
        "--spef-corner",
        "tt",
        "--max-spef-mib",
        "1",
    ]);
    assert_eq!(bad["ok"], false);
    assert!(bad["error"]["message"].as_str().unwrap().contains("T0072"));
    #[cfg(unix)]
    {
        let link = dir.join("input-link.spef");
        std::os::unix::fs::symlink(&plain, &link).unwrap();
        let bad = run(&[
            "import",
            "-w",
            w,
            "--base-rev",
            "rev3",
            "--spef",
            link.to_str().unwrap(),
            "--spef-corner",
            "tt",
        ]);
        assert_eq!(bad["ok"], false);
    }
    assert_eq!(run(&["show", "-w", w])["rev"], "rev3");
    fs::remove_dir_all(dir).unwrap();
}
