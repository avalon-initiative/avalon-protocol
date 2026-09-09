//! RFC 6962 (Certificate Transparency) Merkle Tree Hash — issue #210,
//! implementing the design decided in #40/#39 (see
//! `docs/architecture/settlement.md`'s "What is decided (continued)"
//! section). This is a layered addition on top of `postgres.rs`'s existing
//! sequential hash chain (`prev_hash`/`entry_hash`), not a replacement for
//! it: the chain stays the cheap, no-network, O(1)-per-link integrity check
//! `avalon inspect-ledger` already walks end-to-end; this module computes a
//! single append-only Merkle tree over the *whole ledger's* `entry_hash`
//! values, ordered by `seq`, which is what makes succinct inclusion/
//! consistency proofs possible for a remote mirror (built in issue #211) —
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
//! storage, no embedded engine, per ADR #186 and this decision's own "no
//! new storage required to start" note. An incremental frontier/proof-cache
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

fn leaf_hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([LEAF_HASH_PREFIX]);
    hasher.update(data);
    hasher.finalize().into()
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([NODE_HASH_PREFIX]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// The largest power of two strictly less than `n` (`n` must be >= 2) —
/// RFC 6962's left/right split point for an interior node covering `n`
/// leaves (§2.1: "k is the largest power of two smaller than n").
fn split_point(n: usize) -> usize {
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
}
