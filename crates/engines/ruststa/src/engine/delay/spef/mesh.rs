// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Resistor loops on the nodal conductance system (STA-REQ-0730).
//!
//! With the driver node grounded, the conductance matrix of the contracted network is
//! `G = T + A D A^T`: `T` that of a spanning tree, `A` the incidence of the `k` loop-closing
//! resistors (chords) and `D` their conductances. `T^-1 b` is one postorder and one preorder
//! pass over the tree. By the Woodbury identity, `G^-1 c = T^-1 (c - A y)` with
//! `(D^-1 + A^T T^-1 A) y = A^T T^-1 c`; the `k x k` matrix is symmetric positive definite (a
//! resistance matrix of the loops) and is factored once by Cholesky for all four capacitance
//! vectors. Every solution is checked against Kirchhoff's current law in f64 and refined.
//! Work `O(k * nodes + k^3)`, scratch `O(k^2 + nodes)`, both bounded by the caller's budget;
//! no paths or cycles are enumerated.
use super::error;
use super::tree::Budget;
use rusteda_core::diag::Diag;

/// A resistor outside the spanning tree, between contracted nodes `a` and `b`.
pub(super) struct Chord {
    pub a: usize,
    pub b: usize,
    pub kohm: f64,
}

/// A breadth-first spanning tree of the contracted network, rooted at the driver.
pub(super) struct Tree<'a> {
    pub root: usize,
    /// Contracted nodes in breadth-first order (root first).
    pub order: &'a [usize],
    pub parent: &'a [usize],
    /// Resistance of the edge to the parent, kOhm.
    pub resistance: &'a [f64],
}

pub(super) struct Solved {
    /// First moments (Elmore delays) per contracted node and capacitance vector; ns.
    pub delays: Vec<[f64; 4]>,
    /// Driving-point admittance magnitudes (A, B, C) per capacitance vector.
    pub moments: [[f64; 3]; 4],
}

/// Relative Kirchhoff residual accepted after refinement (of the total capacitance).
const RESIDUAL: f64 = 1e-9;
/// Refinement steps after the first solve.
const REFINE: usize = 2;

impl Tree<'_> {
    /// `x <- T^-1 x` with the root grounded: subtree sums, then potentials along the tree.
    fn solve(&self, x: &mut [f64]) {
        for &i in self.order.iter().rev() {
            if i != self.root {
                let p = self.parent[i];
                x[p] += x[i];
            }
        }
        x[self.root] = 0.0;
        for &i in self.order.iter().skip(1) {
            x[i] = x[self.parent[i]] + self.resistance[i] * x[i];
        }
    }
}

/// Lower Cholesky factor of a symmetric positive definite row-major `k x k` matrix, in place.
fn cholesky(s: &mut [f64], k: usize) -> bool {
    for j in 0..k {
        let mut d = s[j * k + j];
        for p in 0..j {
            d -= s[j * k + p] * s[j * k + p];
        }
        if !(d > 0.0 && d.is_finite()) {
            return false;
        }
        let d = d.sqrt();
        s[j * k + j] = d;
        for i in j + 1..k {
            let mut v = s[i * k + j];
            for p in 0..j {
                v -= s[i * k + p] * s[j * k + p];
            }
            s[i * k + j] = v / d;
        }
    }
    true
}

/// Solves `L L^T y = y` in place with the factor of [`cholesky`].
fn cholesky_solve(l: &[f64], k: usize, y: &mut [f64]) {
    for i in 0..k {
        let mut v = y[i];
        for p in 0..i {
            v -= l[i * k + p] * y[p];
        }
        y[i] = v / l[i * k + i];
    }
    for i in (0..k).rev() {
        let mut v = y[i];
        for p in i + 1..k {
            v -= l[p * k + i] * y[p];
        }
        y[i] = v / l[i * k + i];
    }
}

/// `G^-1 c` by the Woodbury identity, written to `x`; `y` is chord scratch.
fn apply(tree: &Tree<'_>, chords: &[Chord], l: &[f64], c: &[f64], x: &mut [f64], y: &mut [f64]) {
    x.copy_from_slice(c);
    tree.solve(x);
    for (yi, ch) in y.iter_mut().zip(chords) {
        *yi = x[ch.a] - x[ch.b];
    }
    cholesky_solve(l, chords.len(), y);
    x.copy_from_slice(c);
    for (yi, ch) in y.iter().zip(chords) {
        x[ch.a] -= yi;
        x[ch.b] += yi;
    }
    tree.solve(x);
}

/// Injected minus drained current per contracted node for potentials `x` (`c - G x`).
fn residual(tree: &Tree<'_>, chords: &[Chord], c: &[f64], x: &[f64], r: &mut [f64]) {
    r.copy_from_slice(c);
    for &i in tree.order.iter().skip(1) {
        let p = tree.parent[i];
        let flow = (x[i] - x[p]) / tree.resistance[i];
        r[i] -= flow;
        r[p] += flow;
    }
    for ch in chords {
        let flow = (x[ch.a] - x[ch.b]) / ch.kohm;
        r[ch.a] -= flow;
        r[ch.b] += flow;
    }
    r[tree.root] = 0.0;
}

/// Solves the four first-moment systems `G tau = c` of a network with loops and returns the
/// moments and driving-point admittance magnitudes `(sum c, sum c*tau, sum c*tau^2)`.
pub(super) fn solve(
    tree: &Tree<'_>,
    chords: &[Chord],
    caps: &[[f64; 4]],
    budget: Budget,
    net: &str,
) -> Result<Solved, Diag> {
    let n = caps.len();
    let m = tree.order.len() as u64;
    let k = chords.len() as u64;
    let bytes = k
        .saturating_mul(k)
        .saturating_add(k)
        .saturating_add(n as u64 * 5)
        .saturating_mul(8);
    let work = m
        .saturating_mul(k.saturating_add(4 * (2 + 2 * REFINE as u64)))
        .saturating_mul(4)
        .saturating_add(k.saturating_mul(k).saturating_mul(k))
        .saturating_add(k.saturating_mul(k).saturating_mul(32));
    if bytes > budget.bytes || work > budget.work {
        return Err(error(format!(
            "RC loop budget exceeded in {net}: {k} independent loops over {m} nodes need \
             {bytes} bytes and {work} operations (limits {} and {}; raise --max-state-gib)",
            budget.bytes, budget.work
        )));
    }
    let k = chords.len();
    // The loop resistance matrix D^-1 + A^T T^-1 A, column by column.
    let mut s = vec![0.0; k * k];
    let mut x = vec![0.0; n];
    for (j, cj) in chords.iter().enumerate() {
        x.fill(0.0);
        x[cj.a] += 1.0;
        x[cj.b] -= 1.0;
        tree.solve(&mut x);
        for (i, ci) in chords.iter().enumerate() {
            s[i * k + j] = x[ci.a] - x[ci.b];
        }
        s[j * k + j] += cj.kohm;
    }
    for i in 0..k {
        for j in 0..i {
            let v = 0.5 * (s[i * k + j] + s[j * k + i]);
            s[i * k + j] = v;
            s[j * k + i] = v;
        }
    }
    if s.iter().any(|v| !v.is_finite()) || !cholesky(&mut s, k) {
        return Err(error(format!("singular RC loop system in {net}")));
    }
    let mut delays = vec![[0.0; 4]; n];
    let mut moments = [[0.0; 3]; 4];
    let mut c = vec![0.0; n];
    let mut r = vec![0.0; n];
    let mut dx = vec![0.0; n];
    let mut y = vec![0.0; k];
    for slot in 0..4 {
        for (dst, cap) in c.iter_mut().zip(caps) {
            *dst = cap[slot];
        }
        let total: f64 = tree.order.iter().map(|&i| c[i]).sum();
        if !total.is_finite() {
            return Err(error("RC capacitance sum overflow"));
        }
        apply(tree, chords, &s, &c, &mut x, &mut y);
        let limit = RESIDUAL * total;
        let mut worst = f64::INFINITY;
        for step in 0..=REFINE {
            residual(tree, chords, &c, &x, &mut r);
            worst = tree
                .order
                .iter()
                .map(|&i| r[i].abs())
                .fold(0.0_f64, |a, b| if b.is_nan() { f64::NAN } else { a.max(b) });
            if worst <= limit || step == REFINE || !worst.is_finite() {
                break;
            }
            apply(tree, chords, &s, &r, &mut dx, &mut y);
            for &i in tree.order {
                x[i] += dx[i];
            }
        }
        if worst.is_nan() || worst > limit {
            return Err(error(format!(
                "RC loop solve in {net} misses Kirchhoff's current law by {worst:e} pF \
                 (limit {limit:e})"
            )));
        }
        // First moments of a passive network are nonnegative; only rounding may cross zero.
        let largest = tree.order.iter().fold(0.0_f64, |a, &i| a.max(x[i]));
        let (mut a, mut b, mut cc) = (0.0, 0.0, 0.0);
        for &i in tree.order {
            let tau = x[i];
            if !tau.is_finite() || tau < -RESIDUAL * largest {
                return Err(error(format!("RC delay overflow in {net}")));
            }
            let tau = tau.max(0.0);
            delays[i][slot] = tau;
            a += c[i];
            b += c[i] * tau;
            cc += c[i] * tau * tau;
        }
        if ![a, b, cc].iter().all(|v| v.is_finite()) {
            return Err(error("RC admittance moment overflow"));
        }
        moments[slot] = [a, b, cc];
    }
    Ok(Solved { delays, moments })
}

#[cfg(test)]
mod tests {
    use super::*;

    // STA-REQ-0730: a chord parallel to a tree edge halves its resistance exactly.
    #[test]
    fn parallel_resistor_is_its_conductance_sum() {
        // root 0 - 1 (2 kOhm) - 2 (1 kOhm), plus a second 2 kOhm resistor 0 - 1.
        let order = [0, 1, 2];
        let parent = [0, 0, 1];
        let resistance = [0.0, 2.0, 1.0];
        let tree = Tree {
            root: 0,
            order: &order,
            parent: &parent,
            resistance: &resistance,
        };
        let chords = [Chord {
            a: 0,
            b: 1,
            kohm: 2.0,
        }];
        let caps = [[0.5; 4], [0.1; 4], [0.2; 4]];
        let budget = Budget {
            bytes: 1 << 20,
            work: 1 << 20,
        };
        let solved = solve(&tree, &chords, &caps, budget, "n").unwrap();
        let t1 = 1.0 * 0.3;
        let t2 = t1 + 1.0 * 0.2;
        assert!((solved.delays[1][0] - t1).abs() < 1e-15);
        assert!((solved.delays[2][0] - t2).abs() < 1e-15);
        assert_eq!(solved.delays[0][0], 0.0);
        let [a, b, c] = solved.moments[0];
        assert!((a - 0.8).abs() < 1e-15);
        assert!((b - (0.1 * t1 + 0.2 * t2)).abs() < 1e-15);
        assert!((c - (0.1 * t1 * t1 + 0.2 * t2 * t2)).abs() < 1e-15);
        let tight = Budget { bytes: 8, work: 8 };
        assert!(solve(&tree, &chords, &caps, tight, "n").is_err());
    }
}
