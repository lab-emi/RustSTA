// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! External nodal transient validation of the engine's pi-load driver rule (`STA-REQ-0726`,
//! `STA-REQ-0727`): the Thevenin resistance from the forward delay sensitivity over 1 fF and the
//! lumped regime below one thousandth of it. The simulator solves the circuit equations; nothing
//! here re-implements the charge solve.
//!
//! Peak memory: four sequential circuits of at most six nodes, < 32 MiB each; no paths.
use rusteda_compute::rc::{self, Pi, Thresholds};
use ruststa::engine::delay::driver::{LUMPED_RESISTANCE_RATIO, SENSITIVITY_STEP_PF, pi_response};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn measurements(text: &str) -> BTreeMap<String, f64> {
    text.lines()
        .filter_map(|l| {
            let (key, value) = l.split_once('=')?;
            Some((
                key.trim().to_string(),
                value.split_whitespace().next()?.parse().ok()?,
            ))
        })
        .collect()
}

fn simulate(tool: &Path, root: &Path, name: &str, circuit: &str) -> BTreeMap<String, f64> {
    let path = root.join(format!("{name}.cir"));
    let log = root.join(format!("{name}.log"));
    fs::write(&path, circuit).unwrap();
    let file = fs::File::create(&log).unwrap();
    let status = Command::new("timeout")
        .args(["--kill-after=2s", "35s"])
        .arg(tool)
        .arg("-b")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .status()
        .unwrap();
    assert!(status.success(), "simulator failed: {}", log.display());
    assert!(fs::metadata(&log).unwrap().len() < 1 << 20);
    measurements(&fs::read_to_string(log).unwrap())
}

/// A piecewise NLDM law with a breakpoint at 0.01 pF (delay slope 10 below, 4 above), so that
/// the forward 1 fF sensitivity differs from a central one near the breakpoint.
fn law(c: f64) -> (f64, f64) {
    let d = if c <= 0.01 {
        10.0 * c
    } else {
        0.1 + 4.0 * (c - 0.01)
    };
    (0.06 + d, 0.03 + 20.0 * c)
}

fn trips() -> Thresholds {
    Thresholds {
        lower: 0.2,
        upper: 0.8,
        input: 0.5,
        output: 0.5,
        derate: 1.0,
    }
}

// STA-REQ-0726: with the forward sensitivity over 1 fF (half of the step above the breakpoint
// for a total of 0.0095 pF), the charge solve still satisfies its circuit: the simulated root
// crossings, slew and end-of-ramp charge match within 1e-6 ns and 1e-8 pF.
// STA-REQ-0727: below one thousandth of that resistance the pi and its lumped total give the
// same driver waveform to within 1e-4 of the transition, so the table values at the total load
// (the characterization of a lumped load) replace the fitted waveform, whose transition differs
// from the table by more than 1 % at the same load.
#[test]
#[ignore = "requires RUSTSTA_NGSPICE and a fresh RUSTSTA_RC_DRIVER_WORKSPACE, with OS caps"]
fn ngspice_forward_sensitivity_and_lumped_regime() {
    let tool =
        PathBuf::from(std::env::var("RUSTSTA_NGSPICE").expect("external simulator required"));
    let root = PathBuf::from(
        std::env::var("RUSTSTA_RC_DRIVER_WORKSPACE").expect("evidence workspace required"),
    );
    fs::create_dir(&root).unwrap();
    let t = trips();
    let mut metrics = String::new();
    for (i, (near, far, ratio)) in [
        (0.0003, 0.0092, 40.0),
        (0.004, 0.0055, 200.0),
        (0.0003, 0.0092, 0.9),
        (0.004, 0.0055, 0.5),
    ]
    .into_iter()
    .enumerate()
    {
        let total: f64 = near + far;
        let res = (law(total + SENSITIVITY_STEP_PF).0 - law(total).0) / SENSITIVITY_STEP_PF
            * std::f64::consts::LN_2;
        let pi = Pi {
            near,
            far,
            resistance: ratio * LUMPED_RESISTANCE_RATIO * res,
        };
        // The circuit of the charge solve at this resistance, lumped or not.
        let r = rc::gate_detail_with_resistance(pi, t, res, law).unwrap();
        let (d, s, c) = pi_response(pi, t, law).unwrap();
        let lumped = ratio < 1.0;
        let stop = 0.1 + r.duration + 30.0 * (res * total + pi.resistance * far);
        let circuit = format!(
            "RC driver transient\nVsrc src 0 PWL(0 0 0.1n 0 {end}n 1)\nRd src root {rd}\n\
             Rpi root far {rp}\nCn root 0 {cn}p\nCf far 0 {cf}p\nRnom src nom {rd}\n\
             Cnom nom 0 {ce}p\nRlum src lum {rd}\nClum lum 0 {ct}p\n.control\nset numdgt=17\n\
             tran {step}n {stop}n\n\
             meas tran tlo WHEN v(root)=0.2 RISE=1\nmeas tran tmid WHEN v(root)=0.5 RISE=1\n\
             meas tran thi WHEN v(root)=0.8 RISE=1\nmeas tran llo WHEN v(lum)=0.2 RISE=1\n\
             meas tran lmid WHEN v(lum)=0.5 RISE=1\nmeas tran lhi WHEN v(lum)=0.8 RISE=1\n\
             meas tran vr FIND v(root) AT={end}n\nmeas tran vf FIND v(far) AT={end}n\n\
             meas tran vn FIND v(nom) AT={end}n\nquit\n.endc\n.end\n",
            end = 0.1 + r.duration,
            rd = res * 1000.0,
            rp = pi.resistance * 1000.0,
            cn = near,
            cf = far,
            ce = r.capacitance,
            ct = total,
            step = stop / 100_000.0,
        );
        let m = simulate(&tool, &root, &format!("case{i}"), &circuit);
        let crossing = (m["tmid"] * 1e9 - 0.1 - r.root_crossing).abs();
        let slew = ((m["thi"] - m["tlo"]) * 1e9 - r.slew).abs();
        let charge = (near * m["vr"] + far * m["vf"] - r.capacitance * m["vn"]).abs();
        let pi_vs_lumped = [
            (m["tlo"] - m["llo"]).abs(),
            (m["tmid"] - m["lmid"]).abs(),
            (m["thi"] - m["lhi"]).abs(),
        ]
        .into_iter()
        .fold(0.0_f64, f64::max)
            * 1e9;
        metrics.push_str(&format!(
            "case{i} lumped={lumped} crossing_error_ns {crossing:e} slew_error_ns {slew:e} \
             charge_error_pf {charge:e} pi_vs_lumped_ns {pi_vs_lumped:e} model_slew_ns {} \
             table_slew_ns {}\n",
            r.slew,
            law(total).1
        ));
        assert!(crossing < 1e-6 && slew < 1e-6 && charge < 1e-8, "case{i}");
        if lumped {
            assert_eq!((d, s, c), (law(total).0, law(total).1, total), "case{i}");
            assert!(pi_vs_lumped < 1e-4 * s, "case{i}: {pi_vs_lumped}");
            assert!((r.slew - s).abs() > 0.01 * s, "case{i}: {} {s}", r.slew);
        } else {
            for (x, y) in [(d, r.delay), (s, r.slew), (c, r.capacitance)] {
                assert!((x - y).abs() < 1e-12, "case{i}: {x} {y}");
            }
        }
    }
    fs::write(root.join("metrics.txt"), metrics).unwrap();
}
