//! RFC 6962 (Certificate Transparency) Merkle Tree Hash (see
//! `docs/architecture/settlement.md`'s "What is decided (continued)"
//! section). This is a layered addition on top of `postgres.rs`'s existing
//! sequential hash chain (`prev_hash`/`entry_hash`), not a replacement for
//! it: the chain stays the cheap, no-network, O(1)-per-link integrity check
//! `avalon inspect-ledger` already walks end-to-end; this module computes a
//! single append-only Merkle tree over the *whole ledger's* `entry_hash`
//! values, ordered by `seq`, which is what makes succinct inclusion/
//! consistency proofs possible for a remote mirror —
//! something a plain hash chain alone can't give without transferring every
//! entry.
//!
//! Domain-separated per RFC 6962 §2.1, so a leaf hash can never collide
//! with an interior node hash:
//!
//! ```text
//! MTH({})       = SHA-256()
//! MTH({d(0)})   = SHA-256(0x00 || d(0))
//! MTH(D[n])     = SHA-256(0x01 || MTH(D[0:k]) || MTH(D[k:n]))   for n > 1,
//!                 k = the largest power of two strictly less than n
//! ```
//!
//! Computed on demand from stored `entry_hash` values, in Postgres — no new
//! storage, no embedded engine required to start. An incremental frontier/proof-cache
//! table remains a valid future optimization if recompute cost ever matters
//! at real ledger scale; it isn't required to close this ticket.

use sha2::{Digest, Sha256};

const LEAF_HASH_PREFIX: u8 = 0x00;
const NODE_HASH_PREFIX: u8 = 0x01;

/// RFC 6962's root hash for a zero-leaf tree: `SHA-256()`, the hash of the
/// empty string.
pub fn empty_root() -> [u8; 32] {
    Sha256::digest([]).into()
}

pub(crate) fn leaf_hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([LEAF_HASH_PREFIX]);
    hasher.update(data);
    hasher.finalize().into()
}

pub(crate) fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([NODE_HASH_PREFIX]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// The largest power of two strictly less than `n` (`n` must be >= 2) —
/// RFC 6962's left/right split point for an interior node covering `n`
/// leaves (§2.1: "k is the largest power of two smaller than n").
pub(crate) fn split_point(n: usize) -> usize {
    debug_assert!(n >= 2, "split_point is only defined for n >= 2");
    1usize << (usize::BITS - 1 - (n - 1).leading_zeros())
}

/// RFC 6962's Merkle Tree Hash (MTH) of `leaves`, in order — the whole
/// ledger's tree when `leaves` is every `entry_hash` up to some `tree_size`
/// (see [`mth_of_hex_hashes`], what `postgres.rs` actually calls). Each
/// element of `leaves` is one leaf's *input data*; this function applies
/// RFC 6962's domain-separated leaf hashing itself, so callers must not
/// pre-hash their inputs before passing them in.
pub fn mth<T: AsRef<[u8]>>(leaves: &[T]) -> [u8; 32] {
    match leaves.len() {
        0 => empty_root(),
        1 => leaf_hash(leaves[0].as_ref()),
        n => {
            let k = split_point(n);
            let left = mth(&leaves[..k]);
            let right = mth(&leaves[k..]);
            node_hash(&left, &right)
        }
    }
}

/// [`mth`] over a ledger's `entry_hash` values exactly as `postgres.rs`
/// stores them (lowercase hex text) — decodes each to its raw 32 bytes
/// first, so what actually gets leaf-hashed is `entry_hash`'s raw bytes
/// (matching issue #210's "domain-separated leaf hash `SHA256(0x00 ||
/// entry_hash_bytes)`"), not its hex text representation.
pub fn mth_of_hex_hashes(hashes: &[String]) -> Result<[u8; 32], String> {
    let mut leaves = Vec::with_capacity(hashes.len());
    for hash in hashes {
        leaves.push(hex::decode(hash).map_err(|e| format!("invalid hex hash `{hash}`: {e}"))?);
    }
    Ok(mth(&leaves))
}

fn decode_hex_leaves(hashes: &[String]) -> Result<Vec<Vec<u8>>, String> {
    let mut leaves = Vec::with_capacity(hashes.len());
    for hash in hashes {
        leaves.push(hex::decode(hash).map_err(|e| format!("invalid hex hash `{hash}`: {e}"))?);
    }
    Ok(leaves)
}

// ---------------------------------------------------------------------
// RFC 6962 §2.1.1 — Merkle audit paths (inclusion proofs)
// ---------------------------------------------------------------------
//
// PATH(m, {d(0)}) = {}
// PATH(m, D[n]) = PATH(m, D[0:k]) : MTH(D[k:n])      for m < k
// PATH(m, D[n]) = PATH(m - k, D[k:n]) : MTH(D[0:k])  for m >= k
//
// where k is the largest power of two strictly less than n (as in `mth`),
// and PATH(m, D[n]) is the audit path for leaf m in a tree of n leaves.
// Reproduced here verbatim from RFC 6962 text — this is not
// an approximation; it's the literal recursive definition, sharing `mth`'s
// `split_point`/`node_hash` so the tree structure a proof is generated
// against can never drift from the structure `mth` itself computes.

/// RFC 6962 `PATH(leaf_index, D[0:leaves.len()])` — the Merkle audit path
/// (list of sibling hashes, leaf-to-root order) proving `leaves[leaf_index]`
/// is included in the tree over the given `leaves`. `leaves` holds each
/// leaf's *input data*, not a pre-hashed value (same convention as [`mth`]).
///
/// Combined with [`verify_inclusion_proof`] and the root this tree
/// produces (`mth(leaves)`), this is everything a remote verifier needs —
/// it never has to see `leaves` itself.
pub fn inclusion_proof<T: AsRef<[u8]>>(
    leaf_index: usize,
    leaves: &[T],
) -> Result<Vec<[u8; 32]>, String> {
    let n = leaves.len();
    if leaf_index >= n {
        return Err(format!(
            "leaf_index {leaf_index} out of range for tree size {n}"
        ));
    }
    Ok(path(leaf_index, leaves))
}

fn path<T: AsRef<[u8]>>(m: usize, d: &[T]) -> Vec<[u8; 32]> {
    let n = d.len();
    if n <= 1 {
        return Vec::new();
    }
    let k = split_point(n);
    if m < k {
        let mut proof = path(m, &d[..k]);
        proof.push(mth(&d[k..]));
        proof
    } else {
        let mut proof = path(m - k, &d[k..]);
        proof.push(mth(&d[..k]));
        proof
    }
}

/// [`inclusion_proof`] over hex-encoded `entry_hash` values, matching
/// [`mth_of_hex_hashes`]'s decoding convention — what `server`'s
/// `GET /ledger/proof/inclusion` actually calls.
pub fn inclusion_proof_of_hex_hashes(
    leaf_index: usize,
    hashes: &[String],
) -> Result<Vec<[u8; 32]>, String> {
    let leaves = decode_hex_leaves(hashes)?;
    inclusion_proof(leaf_index, &leaves)
}

/// Reconstructs a Merkle root from a leaf hash, its index, the claimed tree
/// size, and an audit path — the direct inverse of [`path`]. `None` means
/// the proof is malformed (wrong length for the claimed shape); the caller
/// still must compare the returned hash against the expected root.
fn verify_path(m: usize, n: usize, leaf: [u8; 32], proof: &[[u8; 32]]) -> Option<[u8; 32]> {
    if n <= 1 {
        return if proof.is_empty() { Some(leaf) } else { None };
    }
    let k = split_point(n);
    let (last, rest) = proof.split_last()?;
    if m < k {
        let left = verify_path(m, k, leaf, rest)?;
        Some(node_hash(&left, last))
    } else {
        let right = verify_path(m - k, n - k, leaf, rest)?;
        Some(node_hash(last, &right))
    }
}

/// RFC 6962 inclusion-proof verification: does `proof` actually prove that
/// the leaf whose *input data* is `leaf_data` sits at `leaf_index` in a tree
/// of `tree_size` leaves whose root is `root`? This is what a mirror (or
/// this server itself, before ever returning a proof — see the
/// [`crate::postgres`] invariant that a wrong proof must never leave the
/// process) runs, needing nothing but the leaf data, the claimed
/// coordinates, the proof, and the root — never the rest of the tree.
pub fn verify_inclusion_proof(
    leaf_data: &[u8],
    leaf_index: usize,
    tree_size: usize,
    proof: &[[u8; 32]],
    root: &[u8; 32],
) -> bool {
    if leaf_index >= tree_size {
        return false;
    }
    match verify_path(leaf_index, tree_size, leaf_hash(leaf_data), proof) {
        Some(reconstructed) => &reconstructed == root,
        None => false,
    }
}

// ---------------------------------------------------------------------
// RFC 6962 §2.1.2 — Merkle consistency proofs
// ---------------------------------------------------------------------
//
// PROOF(m, D[n]) = SUBPROOF(m, D[n], true)
//
// SUBPROOF(m, D[m], true)  = {}
// SUBPROOF(m, D[m], false) = {MTH(D[m])}
//
// For m < n, let k be the largest power of two strictly less than n:
// SUBPROOF(m, D[n], b) = SUBPROOF(m, D[0:k], b) : MTH(D[k:n])   if m <= k
// SUBPROOF(m, D[n], b) = SUBPROOF(m-k, D[k:n], false) : MTH(D[0:k])  if m > k
//
// Also verbatim from RFC 6962 text — `subproof` below is that formula with
// no approximation, sharing `split_point`/`node_hash`/`mth` with the rest
// of this module for the same reason `path` above does.

/// RFC 6962 `PROOF(first, D[0:second])` — the consistency proof that the
/// tree at `second` leaves is a strict append-only extension of the tree at
/// `first` leaves. `leaves` is the *larger* (`second`-sized) tree's full
/// leaf-input list; `first` must be in `1..=leaves.len()`. Per RFC 6962,
/// `first == 0` is trivially consistent with anything and always has an
/// empty proof — callers should special-case it rather than calling this
/// (this function still accepts it, returning `Ok(vec![])`, for callers
/// that don't want to special-case it themselves).
pub fn consistency_proof<T: AsRef<[u8]>>(
    first: usize,
    leaves: &[T],
) -> Result<Vec<[u8; 32]>, String> {
    let n = leaves.len();
    if first == 0 {
        return Ok(Vec::new());
    }
    if first > n {
        return Err(format!("first {first} exceeds tree size {n}"));
    }
    if first == n {
        return Ok(Vec::new());
    }
    Ok(subproof(first, leaves, true))
}

fn subproof<T: AsRef<[u8]>>(m: usize, d: &[T], b: bool) -> Vec<[u8; 32]> {
    let n = d.len();
    if m == n {
        if b {
            Vec::new()
        } else {
            vec![mth(d)]
        }
    } else {
        let k = split_point(n);
        if m <= k {
            let mut proof = subproof(m, &d[..k], b);
            proof.push(mth(&d[k..]));
            proof
        } else {
            let mut proof = subproof(m - k, &d[k..], false);
            proof.push(mth(&d[..k]));
            proof
        }
    }
}

/// [`consistency_proof`] over hex-encoded `entry_hash` values — what
/// `server`'s `GET /ledger/proof/consistency` actually calls.
pub fn consistency_proof_of_hex_hashes(
    first: usize,
    hashes: &[String],
) -> Result<Vec<[u8; 32]>, String> {
    let leaves = decode_hex_leaves(hashes)?;
    consistency_proof(first, &leaves)
}

/// Direct inverse of [`subproof`]: reconstructs `(MTH(D[0:m]), MTH(D[0:n]))`
/// from a consistency proof, given the already-trusted `old_root` to seed
/// the base case that RFC 6962 leaves implicit (the subtree that exactly
/// equals the first tree needs no proof element of its own — the verifier
/// already knows its hash *is* `old_root`, that's the whole point of the
/// proof).
fn verify_subproof(
    m: usize,
    n: usize,
    proof: &[[u8; 32]],
    b: bool,
    old_root: &[u8; 32],
) -> Option<([u8; 32], [u8; 32])> {
    if m == n {
        if b {
            if !proof.is_empty() {
                return None;
            }
            Some((*old_root, *old_root))
        } else {
            match proof {
                [only] => Some((*only, *only)),
                _ => None,
            }
        }
    } else if m < n {
        let k = split_point(n);
        let (last, rest) = proof.split_last()?;
        if m <= k {
            let (old_l, new_l) = verify_subproof(m, k, rest, b, old_root)?;
            Some((old_l, node_hash(&new_l, last)))
        } else {
            let (old_r, new_r) = verify_subproof(m - k, n - k, rest, false, old_root)?;
            Some((node_hash(last, &old_r), node_hash(last, &new_r)))
        }
    } else {
        None
    }
}

/// RFC 6962 consistency-proof verification: does `proof` actually prove the
/// tree at `second` leaves (root `new_root`) is a strict append-only
/// extension of the tree at `first` leaves (root `old_root`)? Needs nothing
/// but the two claimed sizes, the two claimed roots, and the proof.
pub fn verify_consistency_proof(
    first: usize,
    second: usize,
    proof: &[[u8; 32]],
    old_root: &[u8; 32],
    new_root: &[u8; 32],
) -> bool {
    if first > second {
        return false;
    }
    if first == 0 {
        // RFC 6962: trivially consistent; a log MAY return an empty proof,
        // so don't require one, but a non-empty proof is not itself an
        // error to tolerate silently — there's simply nothing to check
        // against `old_root` (the empty tree has exactly one root value,
        // `empty_root()`, which this function doesn't have an opinion on).
        return true;
    }
    if first == second {
        return proof.is_empty() && old_root == new_root;
    }
    match verify_subproof(first, second, proof, true, old_root) {
        Some((old_h, new_h)) => &old_h == old_root && &new_h == new_root,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hd(s: &str) -> Vec<u8> {
        hex::decode(s).expect("test vector hex should always decode")
    }

    /// The 8 fixed leaf inputs `transparency-dev/merkle` (the maintained
    /// successor to Google's original Certificate Transparency Go/Trillian
    /// Merkle implementation) uses for its own RFC 6962 reference tests —
    /// `testonly.LeafInputs()` in that project's
    /// `testonly/constants.go`. Reproduced here verbatim (as raw bytes, not
    /// entry_hash-shaped data) purely to cross-check this module's `mth`
    /// against a known-good, independently maintained implementation —
    /// exactly the "RFC 6962's own published test vectors or an equivalent
    /// known-good reference" issue #210 asks for.
    fn leaf_inputs() -> Vec<Vec<u8>> {
        vec![
            hd(""),
            hd("00"),
            hd("10"),
            hd("2021"),
            hd("3031"),
            hd("40414243"),
            hd("5051525354555657"),
            hd("606162636465666768696a6b6c6d6e6f"),
        ]
    }

    /// `testonly.RootHashes()` from the same file — the MTH of the first
    /// `n` of `leaf_inputs()`, indexed by tree size; index 0 is the empty
    /// tree's root.
    fn expected_roots() -> Vec<Vec<u8>> {
        [
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
            "fac54203e7cc696cf0dfcb42c92a1d9dbaf70ad9e621f4bd8d98662f00e3c125",
            "aeb6bcfe274b70a14fb067a5e5578264db0fa9b51af5e0ba159158f329e06e77",
            "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
            "4e3bbb1f7b478dcfe71fb631631519a3bca12c9aefca1612bfce4c13a86264d4",
            "76e67dadbcdf1e10e1b74ddc608abd2f98dfb16fbce75277b5232a127f2087ef",
            "ddb89be403809e325750d3d263cd78929c2942b7942a34b77e122c9594a74c8c",
            "5dc9da79a70659a9ad559cb701ded9a2ab9d823aad2f4960cfe370eff4604328",
        ]
        .into_iter()
        .map(hd)
        .collect()
    }

    #[test]
    fn empty_root_matches_sha256_of_empty_string() {
        assert_eq!(empty_root().to_vec(), expected_roots()[0]);
        assert_eq!(mth::<Vec<u8>>(&[]).to_vec(), expected_roots()[0]);
    }

    /// The core correctness test: MTH for every tree size from the
    /// reference vectors' n=1 (single leaf) through n=8, i.e. covering and
    /// exceeding issue #210's "n=1 through at least n=7" requirement.
    #[test]
    fn mth_matches_reference_vectors_for_every_tree_size() {
        let leaves = leaf_inputs();
        let roots = expected_roots();
        for size in 1..=leaves.len() {
            let got = mth(&leaves[..size]);
            assert_eq!(
                got.to_vec(),
                roots[size],
                "MTH mismatch at tree size {size}"
            );
        }
    }

    /// Appending one more leaf (tree_size N -> N+1) must produce exactly
    /// the root the reference vectors give for N+1 — the incremental-append
    /// property issue #210 explicitly calls out, not just "some tree of the
    /// right size happens to work."
    #[test]
    fn incremental_append_matches_reference_vectors() {
        let leaves = leaf_inputs();
        let roots = expected_roots();
        for size in 1..leaves.len() {
            let smaller = mth(&leaves[..size]);
            let larger = mth(&leaves[..=size]);
            assert_eq!(smaller.to_vec(), roots[size]);
            assert_eq!(larger.to_vec(), roots[size + 1]);
            assert_ne!(smaller, larger, "appending a leaf must change the root");
        }
    }

    #[test]
    fn mth_of_hex_hashes_matches_mth_of_raw_bytes() {
        let leaves = leaf_inputs();
        let hex_hashes: Vec<String> = leaves.iter().map(hex::encode).collect();
        for size in 1..=leaves.len() {
            let via_bytes = mth(&leaves[..size]);
            let via_hex =
                mth_of_hex_hashes(&hex_hashes[..size]).expect("valid hex should never fail");
            assert_eq!(via_bytes, via_hex);
        }
    }

    #[test]
    fn mth_of_hex_hashes_rejects_invalid_hex() {
        let bad = vec!["not-hex".to_string()];
        assert!(mth_of_hex_hashes(&bad).is_err());
    }

    /// Tampering with any single leaf changes the root — the property
    /// `verify`'s Merkle recomputation in `postgres.rs` actually relies on.
    #[test]
    fn tampering_with_any_leaf_changes_the_root() {
        let leaves = leaf_inputs();
        let original = mth(&leaves);

        for i in 0..leaves.len() {
            let mut tampered = leaves.clone();
            tampered[i] = hd("ff");
            let tampered_root = mth(&tampered);
            assert_ne!(
                original, tampered_root,
                "tampering with leaf {i} should change the root"
            );
        }
    }

    #[test]
    fn leaf_hash_and_node_hash_are_domain_separated() {
        // A leaf hash and a node hash over the same-looking bytes must never
        // collide — RFC 6962's whole point in prefixing them differently.
        let data = b"same bytes either way";
        let as_leaf = leaf_hash(data);
        let as_node = node_hash(
            &Sha256::digest(&data[..16]).into(),
            &Sha256::digest(&data[16..]).into(),
        );
        assert_ne!(as_leaf, as_node);
    }

    // -------------------------------------------------------------
    // Inclusion / consistency proofs
    // -------------------------------------------------------------
    //
    // Correctness strategy ("don't just check the
    // proof looks reasonable"):
    //
    //  1. Every generated proof is round-tripped through `verify_path`/
    //     `verify_subproof` (via the public `verify_inclusion_proof`/
    //     `verify_consistency_proof`) and checked against `expected_roots`
    //     — the same RFC 6962 reference root hashes above, independently
    //     sourced from `transparency-dev/merkle`'s own test constants, not
    //     merely `mth`'s own output. A proof-generation bug that happened
    //     to agree with `mth` but not with the real tree structure would
    //     still be caught here, since the roots it must reconstruct come
    //     from an external source.
    //  2. A from-scratch, deliberately naive second implementation
    //     (`naive_inclusion_proof` below) rebuilds the whole tree
    //     layer-by-layer and walks it directly, rather than sharing any
    //     code with `path`'s split-point recursion. Exhaustive comparison
    //     against the optimized implementation across every valid
    //     (tree_size, index) pair up to 12 leaves catches a bug in the RFC
    //     recursion that happens to still verify against itself. (The
    //     consistency-proof side of this is covered by (1) and (3) instead
    //     — `verify_subproof` is the direct structural inverse of
    //     `subproof`, so round-tripping through externally-sourced roots
    //     plus exhaustive tampering checks is the stronger signal there.)
    //  3. Negative tests: tampering with any single proof node must fail
    //     verification; requesting a proof for an out-of-range
    //     index/tree_size must return an error, never a fabricated proof.

    /// Deliberately naive: builds every level of the tree bottom-up as
    /// plain `Vec<[u8;32]>`s (not `mth`'s recursive split), following RFC
    /// 6962's *implicit* "left-balanced" node structure — the same
    /// construction real implementations like Certificate Transparency's
    /// use internally. `layers[0]` is leaf hashes, `layers[i+1]` is built
    /// by pairing up `layers[i]`, carrying an unpaired last node straight
    /// up unchanged (RFC 6962's tree is not padded to a power of two).
    fn naive_layers(leaves: &[Vec<u8>]) -> Vec<Vec<[u8; 32]>> {
        let mut layers = vec![leaves.iter().map(|l| leaf_hash(l)).collect::<Vec<_>>()];
        while layers.last().unwrap().len() > 1 {
            let prev = layers.last().unwrap();
            let mut next = Vec::with_capacity(prev.len().div_ceil(2));
            let mut i = 0;
            while i + 1 < prev.len() {
                next.push(node_hash(&prev[i], &prev[i + 1]));
                i += 2;
            }
            if i < prev.len() {
                next.push(prev[i]);
            }
            layers.push(next);
        }
        layers
    }

    /// Naive audit path: at each level, find the sibling of the current
    /// node and record it if a sibling actually exists at that position
    /// (an unpaired carried-up node has no sibling to prove against at that
    /// level — RFC 6962's `path` simply never descends into it, since
    /// `split_point` always sends `m` toward the side that still has real
    /// structure). This mirrors `path`'s split-point behavior emergently,
    /// from the tree's shape, rather than from the same formula.
    fn naive_inclusion_proof(leaf_index: usize, leaves: &[Vec<u8>]) -> Vec<[u8; 32]> {
        let layers = naive_layers(leaves);
        let mut proof = Vec::new();
        let mut n = leaves.len();
        let mut idx = leaf_index;
        for layer in layers.iter().take(layers.len().saturating_sub(1)) {
            let sibling = idx ^ 1;
            if sibling < n {
                proof.push(layer[sibling]);
            }
            idx /= 2;
            n = n.div_ceil(2);
        }
        proof
    }

    #[test]
    fn naive_inclusion_proof_matches_optimized_implementation_exhaustively() {
        for size in 1..=12usize {
            let leaves: Vec<Vec<u8>> = (0..size).map(|i| vec![i as u8, 0xAB]).collect();
            for index in 0..size {
                let got = inclusion_proof(index, &leaves).unwrap();
                let want = naive_inclusion_proof(index, &leaves);
                assert_eq!(
                    got, want,
                    "inclusion proof mismatch at size={size} index={index}"
                );
            }
        }
    }

    #[test]
    fn inclusion_proofs_verify_against_externally_sourced_reference_roots() {
        let leaves = leaf_inputs();
        let roots = expected_roots();
        for size in 1..=leaves.len() {
            let root: [u8; 32] = roots[size].clone().try_into().unwrap();
            for index in 0..size {
                let proof = inclusion_proof(index, &leaves[..size]).unwrap();
                assert!(
                    verify_inclusion_proof(&leaves[index], index, size, &proof, &root),
                    "inclusion proof for size={size} index={index} failed to verify against the reference root"
                );
            }
        }
    }

    #[test]
    fn inclusion_proof_rejects_out_of_range_index() {
        let leaves = leaf_inputs();
        assert!(inclusion_proof(7, &leaves[..7]).is_err());
        assert!(inclusion_proof(100, &leaves).is_err());
    }

    #[test]
    fn inclusion_proof_verification_rejects_tampering() {
        let leaves = leaf_inputs();
        let roots = expected_roots();
        let size = 7;
        let root: [u8; 32] = roots[size].clone().try_into().unwrap();
        let index = 3;
        let proof = inclusion_proof(index, &leaves[..size]).unwrap();

        // Wrong leaf data.
        assert!(!verify_inclusion_proof(
            b"wrong data",
            index,
            size,
            &proof,
            &root
        ));
        // Wrong index.
        assert!(!verify_inclusion_proof(
            &leaves[index],
            index + 1,
            size,
            &proof,
            &root
        ));
        // Wrong tree size: this stale (size-7) proof's last element is
        // `MTH(D[4:7])`, not the `MTH(D[4:8])` a genuine size-8 proof for
        // this leaf would carry, so it must not validate against the real
        // size-8 root either — comparing against `root` (still size 7's)
        // wouldn't actually exercise this, since a size-8 recursion happens
        // to share this leaf's entire left-subtree structure with size 7.
        let root8: [u8; 32] = roots[size + 1].clone().try_into().unwrap();
        assert!(!verify_inclusion_proof(
            &leaves[index],
            index,
            size + 1,
            &proof,
            &root8
        ));
        // Tampered proof node.
        for i in 0..proof.len() {
            let mut tampered = proof.clone();
            tampered[i] = [0xFFu8; 32];
            assert!(
                !verify_inclusion_proof(&leaves[index], index, size, &tampered, &root),
                "tampering with proof node {i} should fail verification"
            );
        }
        // Wrong root.
        assert!(!verify_inclusion_proof(
            &leaves[index],
            index,
            size,
            &proof,
            &[0u8; 32]
        ));
    }

    #[test]
    fn inclusion_proof_of_hex_hashes_matches_raw_bytes_version() {
        let leaves = leaf_inputs();
        let hex_hashes: Vec<String> = leaves.iter().map(hex::encode).collect();
        for size in 1..=leaves.len() {
            for index in 0..size {
                let via_bytes = inclusion_proof(index, &leaves[..size]).unwrap();
                let via_hex = inclusion_proof_of_hex_hashes(index, &hex_hashes[..size]).unwrap();
                assert_eq!(via_bytes, via_hex);
            }
        }
    }

    #[test]
    fn consistency_proofs_verify_against_externally_sourced_reference_roots() {
        let leaves = leaf_inputs();
        let roots = expected_roots();
        for first in 1..=leaves.len() {
            for second in first..=leaves.len() {
                let old_root: [u8; 32] = roots[first].clone().try_into().unwrap();
                let new_root: [u8; 32] = roots[second].clone().try_into().unwrap();
                let proof = consistency_proof(first, &leaves[..second]).unwrap();
                assert!(
                    verify_consistency_proof(first, second, &proof, &old_root, &new_root),
                    "consistency proof from {first} to {second} failed to verify against reference roots"
                );
            }
        }
    }

    #[test]
    fn consistency_proof_trivial_cases() {
        let leaves = leaf_inputs();
        // first == 0: trivially consistent, empty proof.
        let proof = consistency_proof(0, &leaves).unwrap();
        assert!(proof.is_empty());
        let root: [u8; 32] = expected_roots()[leaves.len()].clone().try_into().unwrap();
        assert!(verify_consistency_proof(
            0,
            leaves.len(),
            &proof,
            &[0u8; 32], // old root is meaningless when first == 0
            &root
        ));

        // first == second: also trivially empty, and the two roots must
        // actually match.
        let proof = consistency_proof(5, &leaves[..5]).unwrap();
        assert!(proof.is_empty());
        let root5: [u8; 32] = expected_roots()[5].clone().try_into().unwrap();
        assert!(verify_consistency_proof(5, 5, &proof, &root5, &root5));
        assert!(!verify_consistency_proof(5, 5, &proof, &root5, &root));
    }

    #[test]
    fn consistency_proof_rejects_first_greater_than_tree_size() {
        let leaves = leaf_inputs();
        assert!(consistency_proof(6, &leaves[..5]).is_err());
    }

    #[test]
    fn consistency_proof_verification_rejects_tampering() {
        let leaves = leaf_inputs();
        let roots = expected_roots();
        let (first, second) = (3, 8);
        let old_root: [u8; 32] = roots[first].clone().try_into().unwrap();
        let new_root: [u8; 32] = roots[second].clone().try_into().unwrap();
        let proof = consistency_proof(first, &leaves[..second]).unwrap();
        assert!(!proof.is_empty());

        for i in 0..proof.len() {
            let mut tampered = proof.clone();
            tampered[i] = [0xFFu8; 32];
            assert!(
                !verify_consistency_proof(first, second, &tampered, &old_root, &new_root),
                "tampering with consistency proof node {i} should fail verification"
            );
        }
        // Wrong old root, wrong new root, swapped sizes.
        assert!(!verify_consistency_proof(
            first, second, &proof, &[0u8; 32], &new_root
        ));
        assert!(!verify_consistency_proof(
            first, second, &proof, &old_root, &[0u8; 32]
        ));
        assert!(!verify_consistency_proof(
            second, first, &proof, &new_root, &old_root
        ));
    }

    #[test]
    fn consistency_proof_of_hex_hashes_matches_raw_bytes_version() {
        let leaves = leaf_inputs();
        let hex_hashes: Vec<String> = leaves.iter().map(hex::encode).collect();
        for first in 1..=leaves.len() {
            for second in first..=leaves.len() {
                let via_bytes = consistency_proof(first, &leaves[..second]).unwrap();
                let via_hex =
                    consistency_proof_of_hex_hashes(first, &hex_hashes[..second]).unwrap();
                assert_eq!(via_bytes, via_hex);
            }
        }
    }
}
