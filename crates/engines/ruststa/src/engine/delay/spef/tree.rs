// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Iterative RC tree reduction, linear space/work; no path enumeration (STA-REQ-0704).
use super::error;
use rusteda_compute::rc::Pi;
use rusteda_core::diag::Diag;

pub(super) struct Reduced {
    pub delays: Vec<[f64; 4]>,
    pub pi: [Pi; 4],
}
use rusteda_formats::spef::Net;

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

pub(super) fn reduce(net: &Net, driver: u32, caps: &[[f64; 4]]) -> Result<Reduced, Diag> {
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
            let a = find(&mut parent, edge.a as usize);
            let b = find(&mut parent, edge.b as usize);
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
    for edge in &net.resistors {
        if edge.kohm == 0.0 {
            continue;
        }
        let a = roots[edge.a as usize];
        let b = roots[edge.b as usize];
        if a == b {
            return Err(error(format!("resistive loop in {}", net.name)));
        }
        children[a].push((b, edge.kohm));
        children[b].push((a, edge.kohm));
    }
    let mut tree_parent = vec![usize::MAX; n];
    let mut resistance = vec![0.0; n];
    let mut order = Vec::with_capacity(n);
    tree_parent[root] = root;
    order.push(root);
    let mut cursor = 0;
    while cursor < order.len() {
        let node = order[cursor];
        cursor += 1;
        let mut parent_edges = 0;
        for &(child, r) in &children[node] {
            if child == tree_parent[node] {
                parent_edges += 1;
                if parent_edges == 1 {
                    continue;
                }
            }
            if tree_parent[child] != usize::MAX {
                return Err(error(format!(
                    "RC mesh/parallel edge in {} is unsupported",
                    net.name
                )));
            }
            tree_parent[child] = node;
            resistance[child] = r;
            order.push(child);
        }
    }
    if roots.iter().any(|&r| tree_parent[r] == usize::MAX) {
        return Err(error(format!("disconnected RC network in {}", net.name)));
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
        let a = sums[root][k];
        let [b, c] = moments[root][k];
        pi[k] = if b == 0.0 || c == 0.0 {
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
        pi[k].validate().map_err(|e| error(e.to_string()))?;
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
    Ok(Reduced { delays, pi })
}
