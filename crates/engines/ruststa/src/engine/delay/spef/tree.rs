// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Iterative RC network reduction, linear space/work on trees; no path enumeration
//! (STA-REQ-0704). Resistor loops are solved on the nodal conductance system (STA-REQ-0730) and
//! components without a resistive path to the driver are joined to it (STA-REQ-0731).
use super::error;
use super::mesh;
use rusteda_compute::rc::Pi;
use rusteda_core::diag::Diag;

pub(super) struct Reduced {
    pub delays: Vec<[f64; 4]>,
    pub pi: [Pi; 4],
    /// Components joined to the driver (STA-REQ-0731), each as ascending local node indices,
    /// ordered by their smallest index.
    pub detached: Vec<Vec<u32>>,
    /// Independent resistor loops solved on the conductance system (STA-REQ-0730); 0 on a tree.
    pub loops: usize,
    /// Resistors whose two ends are joined by zero resistance; they carry no current.
    pub shorted: usize,
}
use rusteda_formats::spef::Net;

/// Scratch and work budget of one net's loop solve (STA-REQ-0730).
#[derive(Debug, Clone, Copy)]
pub(super) struct Budget {
    pub bytes: u64,
    pub work: u64,
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], rank: &mut [u8], a: usize, b: usize) {
    let a = find(parent, a);
    let b = find(parent, b);
    if a != b {
        let (a, b) = if rank[a] > rank[b] || rank[a] == rank[b] && a < b {
            (a, b)
        } else {
            (b, a)
        };
        parent[b] = a;
        if rank[a] == rank[b] {
            rank[a] += 1;
        }
    }
}

/// The components of the whole network (any resistance) that do not contain the driver.
fn detached_components(net: &Net, driver: usize) -> Vec<Vec<u32>> {
    let n = net.nodes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut rank = vec![0u8; n];
    for edge in &net.resistors {
        union(&mut parent, &mut rank, edge.a as usize, edge.b as usize);
    }
    let main = find(&mut parent, driver);
    let mut first = vec![usize::MAX; n];
    let mut components: Vec<Vec<u32>> = Vec::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        if r == main {
            continue;
        }
        if first[r] == usize::MAX {
            first[r] = components.len();
            components.push(Vec::new());
        }
        components[first[r]].push(i as u32);
    }
    components
}

/// Driving-point pi from the admittance magnitudes (A, B, C) (STA-REQ-0720).
fn pi_from_moments(a: f64, b: f64, c: f64) -> Result<Pi, Diag> {
    let pi = if b == 0.0 || c == 0.0 {
        Pi {
            near: a,
            ..Pi::default()
        }
    } else {
        let far = b / c * b;
        let near = a - far;
        let ratio = c / b;
        if near < -1e-12 * a || !far.is_finite() {
            return Err(error("nonpassive RC moment reduction"));
        }
        Pi {
            near: near.max(0.0),
            far,
            resistance: ratio / b * ratio,
        }
    };
    pi.validate().map_err(|e| error(e.to_string()))?;
    Ok(pi)
}

pub(super) fn reduce(
    net: &Net,
    driver: u32,
    caps: &[[f64; 4]],
    budget: Budget,
) -> Result<Reduced, Diag> {
    let n = net.nodes.len();
    if caps.len() != n
        || driver as usize >= n
        || caps.iter().flatten().any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err(error("invalid RC capacitance state"));
    }
    let mut parent: Vec<usize> = (0..n).collect();
    let mut rank = vec![0u8; n];
    for edge in &net.resistors {
        if edge.kohm == 0.0 {
            union(&mut parent, &mut rank, edge.a as usize, edge.b as usize);
        }
    }
    // STA-REQ-0731: a component without any resistor path to the driver joins it with zero
    // resistance. Nothing changes when every node reaches the driver.
    let detached = detached_components(net, driver as usize);
    let mut joined = vec![false; if detached.is_empty() { 0 } else { n }];
    for component in &detached {
        for &i in component {
            union(&mut parent, &mut rank, i as usize, driver as usize);
            joined[i as usize] = true;
        }
    }
    let roots: Vec<_> = (0..n).map(|i| find(&mut parent, i)).collect();
    let root = roots[driver as usize];
    let mut children = vec![Vec::new(); n];
    let mut sums = vec![[0.0; 4]; n];
    for (i, cap) in caps.iter().enumerate() {
        for k in 0..4 {
            sums[roots[i]][k] += cap[k];
            if !sums[roots[i]][k].is_finite() {
                return Err(error("RC capacitance sum overflow"));
            }
        }
    }
    let mut shorted = 0;
    for (e, edge) in net.resistors.iter().enumerate() {
        if edge.kohm == 0.0 {
            continue;
        }
        let a = roots[edge.a as usize];
        let b = roots[edge.b as usize];
        if a == b {
            // Both ends are one zero-resistance node: no current flows through it. Resistors
            // of joined components are part of that join, not shorted in the source.
            if joined.is_empty() || !joined[edge.a as usize] {
                shorted += 1;
            }
            continue;
        }
        children[a].push((b, edge.kohm, e));
        children[b].push((a, edge.kohm, e));
    }
    let mut tree_parent = vec![usize::MAX; n];
    let mut parent_edge = vec![usize::MAX; n];
    let mut resistance = vec![0.0; n];
    let mut order = Vec::with_capacity(n);
    tree_parent[root] = root;
    order.push(root);
    let mut cursor = 0;
    while cursor < order.len() {
        let node = order[cursor];
        cursor += 1;
        for &(child, r, e) in &children[node] {
            if e == parent_edge[node] || tree_parent[child] != usize::MAX {
                continue;
            }
            tree_parent[child] = node;
            parent_edge[child] = e;
            resistance[child] = r;
            order.push(child);
        }
    }
    if roots.iter().any(|&r| tree_parent[r] == usize::MAX) {
        return Err(error(format!("disconnected RC network in {}", net.name)));
    }
    // Resistors outside the breadth-first spanning tree close loops (STA-REQ-0730).
    let chords: Vec<mesh::Chord> =
        net.resistors
            .iter()
            .enumerate()
            .filter_map(|(e, edge)| {
                let a = roots[edge.a as usize];
                let b = roots[edge.b as usize];
                (edge.kohm != 0.0 && a != b && parent_edge[a] != e && parent_edge[b] != e)
                    .then_some(mesh::Chord {
                        a,
                        b,
                        kohm: edge.kohm,
                    })
            })
            .collect();
    if !chords.is_empty() {
        let tree = mesh::Tree {
            root,
            order: &order,
            parent: &tree_parent,
            resistance: &resistance,
        };
        let solved = mesh::solve(&tree, &chords, &sums, budget, &net.name)?;
        let mut pi = [Pi::default(); 4];
        for k in 0..4 {
            pi[k] = pi_from_moments(
                solved.moments[k][0],
                solved.moments[k][1],
                solved.moments[k][2],
            )?;
        }
        let delays = roots.iter().map(|&r| solved.delays[r]).collect();
        return Ok(Reduced {
            delays,
            pi,
            detached,
            loops: chords.len(),
            shorted,
        });
    }
    for &i in order.iter().rev() {
        if i != root {
            let p = tree_parent[i];
            for k in 0..4 {
                sums[p][k] += sums[i][k];
                if !sums[p][k].is_finite() {
                    return Err(error("RC subtree capacitance overflow"));
                }
            }
        }
    }
    // Magnitudes of admittance s^2 and s^3 coefficients, accumulated positively.
    let mut moments = vec![[[0.0; 2]; 4]; n];
    for &i in order.iter().rev() {
        if i == root {
            continue;
        }
        let p = tree_parent[i];
        let r = resistance[i];
        for k in 0..4 {
            let a = sums[i][k];
            let [b, c] = moments[i][k];
            moments[p][k][0] += b + r * a * a;
            moments[p][k][1] += c + 2.0 * r * a * b + r * r * a * a * a;
            if !moments[p][k].iter().all(|x| x.is_finite()) {
                return Err(error("RC admittance moment overflow"));
            }
        }
    }
    let mut pi = [Pi::default(); 4];
    for k in 0..4 {
        let [b, c] = moments[root][k];
        pi[k] = pi_from_moments(sums[root][k], b, c)?;
    }
    let mut delays = vec![[0.0; 4]; n];
    for &i in order.iter().skip(1) {
        for k in 0..4 {
            let v = delays[tree_parent[i]][k] + resistance[i] * sums[i][k];
            if !v.is_finite() {
                return Err(error(format!("RC delay overflow in {}", net.name)));
            }
            delays[i][k] = v;
        }
    }
    for i in 0..n {
        delays[i] = delays[roots[i]];
    }
    Ok(Reduced {
        delays,
        pi,
        detached,
        loops: 0,
        shorted,
    })
}
