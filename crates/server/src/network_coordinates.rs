//! Vivaldi network coordinates with a height term.
//!
//! Each node keeps one position in a small Euclidean space plus a height and an
//! error estimate. The distance between two positions plus both heights
//! approximates the round trip between the nodes, so a pair that never
//! measured each other can still be estimated with [`estimate_rtt_ms`].
//!
//! Coordinates are advisory. They are never an input to admission, pruning,
//! trust or version decisions; per-observer measured round trips remain the
//! ground truth. Values received from a peer are validated and every update
//! is damped, so a misbehaving peer can only move a node a bounded distance
//! per announce.

use serde::{Deserialize, Serialize};

/// Dimension of the Euclidean part of a coordinate.
pub const DIMENSIONS: usize = 3;
/// Error-estimate weighting constant (Vivaldi `ce`).
pub const CE: f64 = 0.25;
/// Position-step constant (Vivaldi `cc`).
pub const CC: f64 = 0.25;
/// Error estimate of a node that has not yet learned anything.
pub const INITIAL_ERROR: f64 = 1.0;
/// Smallest error estimate; keeps the weighting denominator positive.
pub const MIN_ERROR: f64 = 0.05;
/// Largest accepted or produced error estimate.
pub const MAX_ERROR: f64 = 1.5;
/// Round trips below this are treated as this many milliseconds.
pub const MIN_RTT_MS: f64 = 0.1;
/// Round trips above this are treated as this many milliseconds.
pub const MAX_RTT_MS: f64 = 10_000.0;
/// Smallest height in milliseconds.
pub const MIN_HEIGHT_MS: f64 = 0.01;
/// Largest accepted or produced height in milliseconds.
pub const MAX_HEIGHT_MS: f64 = 10_000.0;
/// Largest accepted or produced magnitude of a vector component.
pub const MAX_COMPONENT_MS: f64 = 10_000.0;
/// Largest displacement (vector or height) applied by one update, in milliseconds.
pub const MAX_STEP_MS: f64 = 50.0;

/// A node's own network coordinate, as published in announce exchanges and
/// the topology read model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Coordinate {
    pub vector: [f64; DIMENSIONS],
    pub height: f64,
    pub error: f64,
}

impl Default for Coordinate {
    fn default() -> Self {
        Self {
            vector: [0.0; DIMENSIONS],
            height: MIN_HEIGHT_MS,
            error: INITIAL_ERROR,
        }
    }
}

impl Coordinate {
    /// Whether every field is finite and inside the accepted ranges.
    pub fn is_valid(&self) -> bool {
        self.vector
            .iter()
            .all(|c| c.is_finite() && c.abs() <= MAX_COMPONENT_MS)
            && self.height.is_finite()
            && (0.0..=MAX_HEIGHT_MS).contains(&self.height)
            && self.error.is_finite()
            && self.error > 0.0
            && self.error <= MAX_ERROR
    }
}

/// Why an update was not applied. The local coordinate is left unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    InvalidRemote,
    InvalidRtt,
}

/// Estimated round trip in milliseconds between two nodes: Euclidean distance
/// between the vectors plus both heights. Symmetric, finite and non-negative
/// for valid coordinates.
pub fn estimate_rtt_ms(a: &Coordinate, b: &Coordinate) -> f64 {
    euclid(&a.vector, &b.vector) + (a.height + b.height)
}

fn euclid(a: &[f64; DIMENSIONS], b: &[f64; DIMENSIONS]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f64>()
        .sqrt()
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic unit vector derived from `seed`, used when two positions coincide.
fn seeded_direction(seed: u64) -> [f64; DIMENSIONS] {
    let mut state = seed;
    loop {
        let mut v = [0.0; DIMENSIONS];
        for c in v.iter_mut() {
            *c = (splitmix64(&mut state) >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
        }
        let norm = v.iter().map(|c| c * c).sum::<f64>().sqrt();
        if norm > 1e-6 {
            return v.map(|c| c / norm);
        }
    }
}

/// Stable seed for a peer identifier (FNV-1a).
pub fn seed_for(peer: &str) -> u64 {
    peer.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// One Vivaldi step of `local` toward agreement with a measured round trip to
/// `remote`. `seed` picks the push direction when the positions coincide.
pub fn update(
    local: &Coordinate,
    remote: &Coordinate,
    rtt_ms: f64,
    seed: u64,
) -> Result<Coordinate, Rejected> {
    if !remote.is_valid() || !local.is_valid() {
        return Err(Rejected::InvalidRemote);
    }
    if !rtt_ms.is_finite() || rtt_ms <= 0.0 {
        return Err(Rejected::InvalidRtt);
    }
    let rtt = rtt_ms.clamp(MIN_RTT_MS, MAX_RTT_MS);

    let local_error = local.error.max(MIN_ERROR);
    let weight = local_error / (local_error + remote.error.max(MIN_ERROR));
    let dist = estimate_rtt_ms(local, remote);
    let sample_error = ((dist - rtt).abs() / rtt).min(MAX_ERROR);
    let error = (CE * weight * sample_error + local_error * (1.0 - CE * weight))
        .clamp(MIN_ERROR, MAX_ERROR);

    let force = (CC * weight * (rtt - dist)).clamp(-MAX_STEP_MS, MAX_STEP_MS);
    let mut diff = [0.0; DIMENSIONS];
    for (d, (l, r)) in diff.iter_mut().zip(local.vector.iter().zip(&remote.vector)) {
        *d = l - r;
    }
    let mag = diff.iter().map(|c| c * c).sum::<f64>().sqrt();
    let unit = if mag > 1e-9 {
        diff.map(|c| c / mag)
    } else {
        seeded_direction(seed)
    };
    let mut vector = local.vector;
    for (v, u) in vector.iter_mut().zip(unit) {
        *v = (*v + u * force).clamp(-MAX_COMPONENT_MS, MAX_COMPONENT_MS);
    }
    let height = (local.height + (local.height + remote.height) * force / dist.max(MIN_RTT_MS))
        .clamp(MIN_HEIGHT_MS, MAX_HEIGHT_MS);

    Ok(Coordinate {
        vector,
        height,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground-truth round trips: points in space plus per-node access height.
    fn truth(points: &[([f64; 3], f64)], i: usize, j: usize) -> f64 {
        euclid(&points[i].0, &points[j].0) + points[i].1 + points[j].1
    }

    fn points() -> Vec<([f64; 3], f64)> {
        vec![
            ([0.0, 0.0, 0.0], 2.0),
            ([40.0, 0.0, 0.0], 1.0),
            ([0.0, 60.0, 0.0], 3.0),
            ([40.0, 60.0, 5.0], 2.0),
            ([90.0, 20.0, 0.0], 1.0),
            ([-30.0, 30.0, 10.0], 4.0),
            ([20.0, -50.0, 0.0], 2.0),
            ([70.0, 80.0, 0.0], 1.0),
        ]
    }

    fn simulate(rounds: usize) -> (Vec<Coordinate>, Vec<([f64; 3], f64)>) {
        let pts = points();
        let n = pts.len();
        let mut coords = vec![Coordinate::default(); n];
        for _ in 0..rounds {
            for i in 0..n {
                for j in 0..n {
                    if i != j {
                        let remote = coords[j];
                        if let Ok(c) =
                            update(&coords[i], &remote, truth(&pts, i, j), (i * n + j) as u64)
                        {
                            coords[i] = c;
                        }
                    }
                }
            }
        }
        (coords, pts)
    }

    #[test]
    fn converges_on_a_synthetic_latency_matrix() {
        let (coords, pts) = simulate(300);
        let mut errs = Vec::new();
        for i in 0..pts.len() {
            for j in (i + 1)..pts.len() {
                let t = truth(&pts, i, j);
                errs.push((estimate_rtt_ms(&coords[i], &coords[j]) - t).abs() / t);
            }
        }
        errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = errs[errs.len() / 2];
        assert!(median < 0.1, "median relative error {median}");
        assert!(
            *errs.last().unwrap() < 0.4,
            "worst {}",
            errs.last().unwrap()
        );
        assert!(coords.iter().all(|c| c.is_valid()));
    }

    #[test]
    fn simulation_is_deterministic() {
        assert_eq!(simulate(20).0, simulate(20).0);
    }

    #[test]
    fn coincident_start_separates_along_a_seeded_direction() {
        let a = Coordinate::default();
        let one = update(&a, &a, 50.0, 7).unwrap();
        let again = update(&a, &a, 50.0, 7).unwrap();
        let other = update(&a, &a, 50.0, 8).unwrap();
        assert_eq!(one, again);
        assert_ne!(one.vector, other.vector);
        assert!(euclid(&one.vector, &a.vector) > 0.1);
    }

    #[test]
    fn rejects_non_finite_and_out_of_range_values() {
        let local = Coordinate::default();
        let mutations: [fn(&mut Coordinate); 9] = [
            |c| c.vector[0] = f64::NAN,
            |c| c.vector[1] = f64::INFINITY,
            |c| c.vector[2] = 1e12,
            |c| c.height = f64::NEG_INFINITY,
            |c| c.height = -1.0,
            |c| c.height = 1e9,
            |c| c.error = f64::NAN,
            |c| c.error = 0.0,
            |c| c.error = 100.0,
        ];
        for m in mutations {
            let mut c = Coordinate::default();
            m(&mut c);
            assert!(!c.is_valid());
            assert_eq!(update(&local, &c, 10.0, 1), Err(Rejected::InvalidRemote));
        }
        for rtt in [f64::NAN, f64::INFINITY, 0.0, -5.0] {
            assert_eq!(
                update(&local, &Coordinate::default(), rtt, 1),
                Err(Rejected::InvalidRtt)
            );
        }
    }

    #[test]
    fn a_wrong_outlier_moves_a_converged_node_only_a_bounded_step() {
        let (coords, pts) = simulate(300);
        let local = coords[0];
        let liar = Coordinate {
            vector: [9_000.0, 9_000.0, 9_000.0],
            height: 5_000.0,
            error: MIN_ERROR,
        };
        let next = update(&local, &liar, 1.0, 3).unwrap();
        let moved = euclid(&local.vector, &next.vector);
        assert!(moved <= MAX_STEP_MS + 1e-9, "moved {moved}");
        assert!(next.is_valid());

        let mut c = next;
        for _ in 0..200 {
            for (j, remote) in coords.iter().enumerate().skip(1) {
                if let Ok(n) = update(&c, remote, truth(&pts, 0, j), j as u64) {
                    c = n;
                }
            }
        }
        let t = truth(&pts, 0, 1);
        assert!((estimate_rtt_ms(&c, &coords[1]) - t).abs() / t < 0.25);
    }

    #[test]
    fn estimate_is_symmetric_finite_and_non_negative() {
        let (coords, _) = simulate(50);
        for a in &coords {
            for b in &coords {
                let e = estimate_rtt_ms(a, b);
                assert!(e.is_finite() && e >= 0.0);
                assert_eq!(e, estimate_rtt_ms(b, a));
            }
        }
    }
}
