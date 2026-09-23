//! Issue #349: an incrementally-maintained RFC 6962 tree, replacing
//! `merkle::mth`/`path`/`subproof`'s from-scratch-every-call recomputation
//! with O(log n) append and O(log n) root/proof lookups over persisted
//! perfect-subtree node hashes. Shares `merkle`'s `leaf_hash`/`node_hash`/
//! `split_point` so the tree shape can never drift from `merkle`'s own —
//! see this crate's exhaustive equivalence tests for the actual correctness
//! bar.
//!
//! Design/tradeoff notes live in `docs/projects/backend-server/architecture/settlement.md`, not here.

use crate::merkle::{leaf_hash, node_hash, split_point};
use std::collections::HashMap;

/// An incrementally-built RFC 6962 tree: every "perfect" (power-of-two,
/// leaf-aligned) subtree hash ever completed is kept, keyed by
/// `(level, index)` — `level` 0 is leaf hashes, and a node at `(level,
/// index)` covers leaves `[index * 2^level, (index+1) * 2^level)`. Any
/// root or proof for any tree size up to the current one is reconstructed
/// from these in O(log n), never by rehashing raw leaves.
#[derive(Default, Clone)]
pub struct IncrementalMerkleTree {
    /// `frontier[level]` holds a completed subtree hash still awaiting its
    /// pair at that level (RFC 6962's append-only "carry" — see
    /// `docs/projects/backend-server/architecture/settlement.md`).
    frontier: Vec<Option<[u8; 32]>>,
    nodes: HashMap<(u32, u64), [u8; 32]>,
    size: u64,
}

impl IncrementalMerkleTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> u64 {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Rebuilds a tree from a full ordered leaf-input list — the fallback
    /// path when an in-memory tree can't be trusted to be caught up (mirrors
    /// `PostgresSettlementProvider::leaf_cache`'s own fallback posture).
    pub fn from_leaves<T: AsRef<[u8]>>(leaves: &[T]) -> Self {
        let mut tree = Self::new();
        for leaf in leaves {
            tree.append(leaf.as_ref());
        }
        tree
    }

    /// Appends one leaf, updating O(log n) frontier/node entries.
    pub fn append(&mut self, leaf_data: &[u8]) {
        let start_size = self.size;
        let mut level: u32 = 0;
        let mut h = leaf_hash(leaf_data);
        self.nodes.insert((0, start_size), h);
        while (level as usize) < self.frontier.len() && self.frontier[level as usize].is_some() {
            let left = self.frontier[level as usize].take().unwrap();
            h = node_hash(&left, &h);
            level += 1;
            self.nodes.insert((level, start_size >> level), h);
        }
        if (level as usize) == self.frontier.len() {
            self.frontier.push(Some(h));
        } else {
            self.frontier[level as usize] = Some(h);
        }
        self.size += 1;
    }

    /// `MTH(D[start:start+size])`, using stored perfect-subtree nodes
    /// wherever the range aligns to one, recursing (RFC 6962's own
    /// `split_point`) only over the parts that don't. O(log size) lookups.
    fn range_hash(&self, start: u64, size: u64) -> [u8; 32] {
        if size == 1 {
            return *self
                .nodes
                .get(&(0, start))
                .expect("leaf must already be appended");
        }
        if size.is_power_of_two() && start.is_multiple_of(size) {
            if let Some(h) = self.nodes.get(&(size.trailing_zeros(), start / size)) {
                return *h;
            }
        }
        let k = split_point(size as usize) as u64;
        let left = self.range_hash(start, k);
        let right = self.range_hash(start + k, size - k);
        node_hash(&left, &right)
    }

    /// `MTH` at the given `tree_size` — `None` if `tree_size` exceeds how
    /// many leaves have been appended so far.
    pub fn root(&self, tree_size: u64) -> Option<[u8; 32]> {
        if tree_size > self.size {
            return None;
        }
        if tree_size == 0 {
            return Some(crate::merkle::empty_root());
        }
        Some(self.range_hash(0, tree_size))
    }

    fn path(&self, m: u64, start: u64, size: u64) -> Vec<[u8; 32]> {
        if size <= 1 {
            return Vec::new();
        }
        let k = split_point(size as usize) as u64;
        if m < k {
            let mut proof = self.path(m, start, k);
            proof.push(self.range_hash(start + k, size - k));
            proof
        } else {
            let mut proof = self.path(m - k, start + k, size - k);
            proof.push(self.range_hash(start, k));
            proof
        }
    }

    /// RFC 6962 inclusion (audit) path for leaf `leaf_index` at `tree_size` —
    /// `merkle::inclusion_proof`'s O(log n) equivalent.
    pub fn inclusion_proof(
        &self,
        leaf_index: u64,
        tree_size: u64,
    ) -> Result<Vec<[u8; 32]>, String> {
        if tree_size > self.size {
            return Err(format!(
                "tree_size {tree_size} exceeds {} appended leaves",
                self.size
            ));
        }
        if leaf_index >= tree_size {
            return Err(format!(
                "leaf_index {leaf_index} out of range for tree size {tree_size}"
            ));
        }
        Ok(self.path(leaf_index, 0, tree_size))
    }

    fn subproof(&self, m: u64, start: u64, size: u64, b: bool) -> Vec<[u8; 32]> {
        if m == size {
            if b {
                Vec::new()
            } else {
                vec![self.range_hash(start, size)]
            }
        } else {
            let k = split_point(size as usize) as u64;
            if m <= k {
                let mut proof = self.subproof(m, start, k, b);
                proof.push(self.range_hash(start + k, size - k));
                proof
            } else {
                let mut proof = self.subproof(m - k, start + k, size - k, false);
                proof.push(self.range_hash(start, k));
                proof
            }
        }
    }

    /// RFC 6962 consistency proof from `first` to `second` leaves —
    /// `merkle::consistency_proof`'s O(log n) equivalent.
    pub fn consistency_proof(&self, first: u64, second: u64) -> Result<Vec<[u8; 32]>, String> {
        if second > self.size {
            return Err(format!(
                "second {second} exceeds {} appended leaves",
                self.size
            ));
        }
        if first > second {
            return Err(format!("first {first} exceeds tree size {second}"));
        }
        if first == 0 || first == second {
            return Ok(Vec::new());
        }
        Ok(self.subproof(first, 0, second, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merkle;

    fn leaves(n: usize) -> Vec<Vec<u8>> {
        (0..n).map(|i| vec![i as u8, 0xCD]).collect()
    }

    /// The core bar: every root/inclusion/consistency proof
    /// this incremental tree produces must exactly match the from-scratch
    /// `merkle` implementation, for every size and every valid index/pair.
    #[test]
    fn matches_from_scratch_merkle_exhaustively() {
        let max = 40usize;
        let all_leaves = leaves(max);
        let mut tree = IncrementalMerkleTree::new();

        for size in 1..=max {
            tree.append(&all_leaves[size - 1]);

            let want_root = merkle::mth(&all_leaves[..size]);
            assert_eq!(
                tree.root(size as u64).unwrap(),
                want_root,
                "root mismatch at size {size}"
            );

            for first in 0..=size {
                let want = merkle::consistency_proof(first, &all_leaves[..size]).unwrap();
                let got = tree.consistency_proof(first as u64, size as u64).unwrap();
                assert_eq!(
                    got, want,
                    "consistency proof mismatch first={first} size={size}"
                );
            }

            for index in 0..size {
                let want = merkle::inclusion_proof(index, &all_leaves[..size]).unwrap();
                let got = tree.inclusion_proof(index as u64, size as u64).unwrap();
                assert_eq!(
                    got, want,
                    "inclusion proof mismatch index={index} size={size}"
                );
            }
        }
    }

    /// Same equivalence, but built via `from_leaves` in one shot rather
    /// than incrementally — the two construction paths must agree.
    #[test]
    fn from_leaves_matches_incremental_append() {
        let all_leaves = leaves(15);
        let mut incremental = IncrementalMerkleTree::new();
        for leaf in &all_leaves {
            incremental.append(leaf);
        }
        let bulk = IncrementalMerkleTree::from_leaves(&all_leaves);
        for size in 1..=all_leaves.len() as u64 {
            assert_eq!(incremental.root(size), bulk.root(size));
        }
    }

    #[test]
    fn root_of_empty_tree_is_empty_root() {
        let tree = IncrementalMerkleTree::new();
        assert_eq!(tree.root(0).unwrap(), merkle::empty_root());
        assert!(tree.root(1).is_none());
    }

    #[test]
    fn inclusion_proof_rejects_out_of_range_index() {
        let tree = IncrementalMerkleTree::from_leaves(&leaves(5));
        assert!(tree.inclusion_proof(5, 5).is_err());
        assert!(tree.inclusion_proof(0, 6).is_err());
    }

    #[test]
    fn proofs_still_verify_against_root_via_merkle_verifiers() {
        let all_leaves = leaves(11);
        let tree = IncrementalMerkleTree::from_leaves(&all_leaves);
        for size in 1..=all_leaves.len() as u64 {
            let root = tree.root(size).unwrap();
            for index in 0..size {
                let proof = tree.inclusion_proof(index, size).unwrap();
                assert!(merkle::verify_inclusion_proof(
                    &all_leaves[index as usize],
                    index as usize,
                    size as usize,
                    &proof,
                    &root
                ));
            }
        }
    }
}
