// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! External nodal validation of extracted RC networks with resistor loops (`STA-REQ-0730`).
//!
//! Planted networks (a tree, a loop between branches, a loop through the driver, a parallel
//! resistor and a ladder with two loops) are bound through the engine's SPEF binding. The
//! external simulator then solves the circuit equations: two operating points of the resistor
//! network with injected currents give the first and second moments of every node (`G tau = C`
//! and `G m2 = C tau`, root grounded), from which the driving-point admittance and its pi
//! follow; a transient of the finite-ramp Thevenin driver into the full network gives the root
//! and sink crossings that the pi and the sink's single pole approximate (`STA-REQ-0729`).
//! Nothing here re-implements the reduction.
//!
//! Peak memory: five sequential circuits of at most twelve nodes, < 32 MiB each; no paths.
use rusteda_compute::rc::{self, Pi, Thresholds};
use rusteda_core::{
    liberty::Library,
    netlist::{Conn, GateNetlist, PinDir, PortDir},
};
use rusteda_formats::{liberty, spef};
use ruststa::engine::{
    delay::{
        driver::SENSITIVITY_STEP_PF,
        load::NetLoadOverrides,
        spef::Spef,
        wire::{DriverLoad, WireModel},
    },
    input::{NetlistView, libview::TimingLib},
    types::{An, NetRow, Tr},
};
use ruststa_fixtures::liberty::{Variant, library};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
};

fn lib() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| liberty::read_text(&library(Variant::Wt15), "synthetic.lib").unwrap())
}

/// `u0` drives net `n` with the three sinks `s1/A`, `s2/A`, `s3/A`.
fn design() -> GateNetlist {
    let mut g = GateNetlist::new("top").unwrap();
    let buf = g
        .add_cell_type("wt_buf_1", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .unwrap();
    g.add_port("a", PortDir::Input, None).unwrap();
    for k in 1..=3 {
        g.add_port(&format!("y{k}"), PortDir::Output, None).unwrap();
    }
    let n = g.add_net(Some("n"), None).unwrap();
    let u = g.add_instance("u0", buf).unwrap();
    g.connect(u, "A", Conn::Net(g.find_net("a", None).unwrap()))
        .unwrap();
    g.connect(u, "Y", Conn::Net(n)).unwrap();
    for k in 1..=3 {
        let s = g.add_instance(&format!("s{k}"), buf).unwrap();
        g.connect(s, "A", Conn::Net(n)).unwrap();
        g.connect(
            s,
            "Y",
            Conn::Net(g.find_net(&format!("y{k}"), None).unwrap()),
        )
        .unwrap();
    }
    g
}

type Network = (
    Vec<(&'static str, f64)>,
    Vec<(&'static str, &'static str, f64)>,
);

fn tree() -> Network {
    (
        vec![("n:1", 0.004), ("n:2", 0.006), ("n:3", 0.005)],
        vec![
            ("u0:Y", "n:1", 0.3),
            ("n:1", "n:2", 0.8),
            ("n:2", "s1:A", 0.02),
            ("n:1", "n:3", 0.5),
            ("n:3", "s2:A", 0.02),
            ("n:2", "s3:A", 0.4),
        ],
    )
}

/// The planted networks; all but the tree have resistor loops.
fn networks() -> Vec<(&'static str, Network)> {
    let with = |extra: &[(&'static str, &'static str, f64)], caps: &[(&'static str, f64)]| {
        let (mut c, mut r) = tree();
        c.extend_from_slice(caps);
        r.extend_from_slice(extra);
        (c, r)
    };
    vec![
        ("tree", tree()),
        ("chord", with(&[("n:3", "n:2", 0.6)], &[])),
        ("root_loop", with(&[("u0:Y", "n:3", 0.9)], &[])),
        ("parallel", with(&[("n:1", "n:2", 0.8)], &[])),
        (
            "ladder",
            with(
                &[
                    ("n:3", "n:4", 0.3),
                    ("n:4", "n:5", 0.3),
                    ("n:5", "n:2", 0.3),
                    ("n:4", "s3:A", 0.2),
                    ("u0:Y", "n:5", 1.5),
                ],
                &[("n:4", 0.003), ("n:5", 0.003)],
            ),
        ),
    ]
}

fn source((caps, res): &Network) -> String {
    let mut s = String::from(
        "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n\
         *T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 KOHM\n\
         *D_NET a 0\n*CONN\n*P a I\n*I u0:A I\n*CAP\n1 a 0\n*RES\n1 a u0:A 0.001\n*END\n",
    );
    let total: f64 = caps.iter().map(|c| c.1).sum();
    s.push_str(&format!(
        "*D_NET n {total}\n*CONN\n*I u0:Y O\n*I s1:A I\n*I s2:A I\n*I s3:A I\n*CAP\n"
    ));
    for (i, (node, c)) in caps.iter().enumerate() {
        s.push_str(&format!("{} {node} {c}\n", i + 1));
    }
    s.push_str("*RES\n");
    for (i, (a, b, r)) in res.iter().enumerate() {
        s.push_str(&format!("{} {a} {b} {r}\n", i + 1));
    }
    s.push_str("*END\n");
    for k in 1..=3 {
        s.push_str(&format!(
            "*D_NET y{k} 0\n*CONN\n*I s{k}:Y O\n*P y{k} O\n*CAP\n1 y{k} 0\n*RES\n1 s{k}:Y y{k} 0.001\n*END\n"
        ));
    }
    s
}

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

/// Circuit node names (the simulator folds case): the driver pin is `root`, others lose the
/// delimiter.
fn node(name: &str) -> String {
    if name == "u0:Y" {
        "root".into()
    } else {
        name.replace(':', "_").to_lowercase()
    }
}

/// A linear NLDM law for the driver (delay and transition against load).
fn law(c: f64) -> (f64, f64) {
    (0.05 + 4.0 * c, 0.03 + 9.0 * c)
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

// STA-REQ-0730: on every planted network the bound first moments of the sinks and the
// driving-point pi equal those of the simulator's operating points (relative 1e-9), so loops
// are solved, not cut. With the Thevenin driver of STA-REQ-0728, the simulated root and sink
// crossings of the full network agree with the pi and single-pole model to within 0.1 % of the
// sink arrival on every network, as on the tree (measured: at most 0.08 ps, 1.6e-4); the
// first moments of the same network without its loop-closing resistors miss by 19 to 87 %.
#[test]
#[ignore = "requires RUSTSTA_NGSPICE and a fresh RUSTSTA_RC_MESH_WORKSPACE, with OS caps"]
fn ngspice_resistor_loops_match_nodal_solution() {
    let tool =
        PathBuf::from(std::env::var("RUSTSTA_NGSPICE").expect("external simulator required"));
    let root = PathBuf::from(
        std::env::var("RUSTSTA_RC_MESH_WORKSPACE").expect("evidence workspace required"),
    );
    fs::create_dir(&root).unwrap();
    let frozen = design().freeze();
    let view = NetlistView::new(&frozen, std::slice::from_ref(lib())).unwrap();
    let tl = TimingLib::from_corner("default", std::slice::from_ref(lib())).unwrap();
    let pin = tl
        .cell("wt_buf_1")
        .unwrap()
        .pins
        .iter()
        .find(|p| p.name == "A")
        .unwrap()
        .cap[0][0];
    let sinks = ["s1:A", "s2:A", "s3:A"];
    let net = (0..view.num_nets())
        .map(|i| NetRow(i as u32))
        .find(|&r| view.net_name(r).to_string() == "n")
        .unwrap();
    let sink_node = |name: &str| {
        (0..view.num_nodes())
            .map(|i| ruststa::engine::types::NodeId(i as u32))
            .find(|&id| view.node_name(id).to_string() == name.replace(':', "/"))
            .unwrap()
    };
    let t = trips();
    let mut metrics = String::new();
    for (name, network) in networks() {
        let parsed = spef::read(&source(&network), spef::Limits::default()).unwrap();
        let model = Spef::bind(&parsed, &view, &tl, &NetLoadOverrides::default(), 1 << 22).unwrap();
        assert_eq!(model.topology().mesh_nets > 0, name != "tree", "{name}");
        let Some(DriverLoad::Pi { c_near, c_far, r }) =
            model.physical_load(net, Tr::Rise, An::Early)
        else {
            panic!("pi absent")
        };
        let taus: Vec<f64> = sinks
            .iter()
            .map(|s| model.elmore(sink_node(s), Tr::Rise, An::Early).unwrap())
            .collect();
        // Node capacitances of the circuit: extracted plus the sinks' pin capacitance.
        let (caps, res) = &network;
        let mut cap: BTreeMap<String, f64> = BTreeMap::new();
        for (n, c) in caps {
            *cap.entry(node(n)).or_default() += c;
        }
        for s in sinks {
            *cap.entry(node(s)).or_default() += pin;
        }
        // Operating points: copy 1 has currents C (volts = first moments), copy 2 has currents
        // C * v(copy 1) (volts = second moments); both copies ground the driver node.
        let mut deck = String::from("RC moments\nVr1 m1_root 0 0\nVr2 m2_root 0 0\n");
        for (i, (a, b, kohm)) in res.iter().enumerate() {
            for copy in ["m1", "m2"] {
                deck.push_str(&format!(
                    "R{copy}_{i} {copy}_{} {copy}_{} {kohm}\n",
                    node(a),
                    node(b)
                ));
            }
        }
        for (i, (n, c)) in cap.iter().enumerate() {
            deck.push_str(&format!("I1_{i} 0 m1_{n} DC {c}\n"));
            deck.push_str(&format!("B2_{i} 0 m2_{n} I={c}*V(m1_{n})\n"));
        }
        deck.push_str(".control\nset numdgt=17\nop\n");
        for n in cap.keys() {
            deck.push_str(&format!(
                "let t_{n}=v(m1_{n})\nlet q_{n}=v(m2_{n})\nprint t_{n} q_{n}\n"
            ));
        }
        deck.push_str("quit\n.endc\n.end\n");
        let m = simulate(&tool, &root, &format!("{name}_op"), &deck);
        let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
        for (n, cn) in &cap {
            a += cn;
            if n != "root" {
                b += cn * m[&format!("t_{n}")];
                c += cn * m[&format!("q_{n}")];
            }
        }
        let far = b * b / c;
        let rpi = c * c / (b * b * b);
        let rel = |x: f64, y: f64| (x - y).abs() / y.abs().max(1e-30);
        let mut moment_err: f64 = 0.0;
        for (s, tau) in sinks.iter().zip(&taus) {
            moment_err = moment_err.max(rel(*tau, m[&format!("t_{}", node(s))]));
        }
        let pi_err = rel(c_far, far).max(rel(r, rpi)).max(rel(c_near + c_far, a));
        assert!(
            moment_err < 1e-9 && pi_err < 1e-9,
            "{name}: {moment_err} {pi_err}"
        );

        // Transient: the Thevenin driver of the bound pi into the full network.
        let pi = Pi {
            near: c_near,
            far: c_far,
            resistance: r,
        };
        let total = pi.total();
        let rd = (law(total + SENSITIVITY_STEP_PF).0 - law(total).0) / SENSITIVITY_STEP_PF
            * std::f64::consts::LN_2;
        let drive = rc::gate_one_step(pi, t, rd, law).unwrap();
        let stop = 0.1 + drive.duration + 30.0 * (rd * total + r * c_far + taus[2]);
        let mut deck = format!(
            "RC network transient\nVsrc src 0 PWL(0 0 0.1n 0 {end}n 1)\nRd src root {rd}k\n",
            end = 0.1 + drive.duration,
            rd = rd
        );
        for (i, (a, b, kohm)) in res.iter().enumerate() {
            deck.push_str(&format!("R{i} {} {} {kohm}k\n", node(a), node(b)));
        }
        for (i, (n, cn)) in cap.iter().enumerate() {
            deck.push_str(&format!("C{i} {n} 0 {cn}p\n"));
        }
        deck.push_str(&format!(
            ".control\nset numdgt=17\ntran {step}n {stop}n\nmeas tran troot WHEN v(root)=0.5 RISE=1\n",
            step = stop / 200_000.0
        ));
        for s in sinks {
            deck.push_str(&format!("meas tran t{0} WHEN v({0})=0.5 RISE=1\n", node(s)));
        }
        deck.push_str("quit\n.endc\n.end\n");
        let m = simulate(&tool, &root, &format!("{name}_tran"), &deck);
        let root_sim = m["troot"] * 1e9 - 0.1;
        let mut wire_err: f64 = 0.0;
        for (s, tau) in sinks.iter().zip(&taus) {
            let sim = m[&format!("t{}", node(s))] * 1e9 - 0.1;
            let (delay, _) = rc::wire_from_driver(pi, rd, drive.duration, *tau, t).unwrap();
            let model = drive.root_crossing + delay;
            let err = (model - sim).abs();
            wire_err = wire_err.max(err / sim);
            metrics.push_str(&format!(
                "{name} {s} tau_ns {tau:e} sink_sim_ns {sim:e} sink_model_ns {model:e} error_ns {err:e}\n"
            ));
        }
        metrics.push_str(&format!(
            "{name} moment_rel {moment_err:e} pi_rel {pi_err:e} root_sim_ns {root_sim:e} \
             root_model_ns {:e} sink_rel_max {wire_err:e}\n",
            drive.root_crossing
        ));
        assert!(wire_err < 1e-3, "{name}: {wire_err}");
        if name != "tree" {
            // The same network without its loop-closing resistors is a different circuit: its
            // first moments miss the simulated ones.
            let (tree_caps, tree_res) = tree();
            if caps.len() == tree_caps.len() {
                let cut = Spef::bind(
                    &spef::read(&source(&(tree_caps, tree_res)), spef::Limits::default()).unwrap(),
                    &view,
                    &tl,
                    &NetLoadOverrides::default(),
                    1 << 22,
                )
                .unwrap();
                let worst = sinks
                    .iter()
                    .zip(&taus)
                    .map(|(s, tau)| {
                        rel(cut.elmore(sink_node(s), Tr::Rise, An::Early).unwrap(), *tau)
                    })
                    .fold(0.0_f64, f64::max);
                metrics.push_str(&format!("{name} cut_moment_rel {worst:e}\n"));
                assert!(worst > 0.1, "{name}: {worst}");
            }
        }
    }
    fs::write(root.join("metrics.txt"), metrics).unwrap();
}
