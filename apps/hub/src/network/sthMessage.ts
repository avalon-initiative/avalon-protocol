// Byte-for-byte reconstruction of the message an STH's signature covers —
// must match `crates/chain/src/sth.rs::signing_message` exactly, or every
// signature verification in this module fails even for a genuine STH.
// See that function's own doc comment for the field-by-field rationale
// (fixed domain tag, big-endian fixed-width integers, a length-prefixed
// root hash, network_id unambiguous as the last variable-length field
// before the fixed-width timestamp suffix).
import { concatBytes } from '@noble/hashes/utils'
import type { SignedTreeHeadResponse } from '@avalon-initiative/protocol-sdk'

const DOMAIN_TAG = new TextEncoder().encode('avalon-settlement-sth-v1')

/** Big-endian two's-complement 8-byte encoding of a (possibly negative) i64. */
function i64BigEndian(value: bigint): Uint8Array {
  const bytes = new Uint8Array(8)
  // Two's complement via BigInt's own wraparound at 2^64, matching Rust's
  // `i64::to_be_bytes` bit pattern for any value in i64's range.
  let unsigned = value < 0n ? value + (1n << 64n) : value
  for (let i = 7; i >= 0; i -= 1) {
    bytes[i] = Number(unsigned & 0xffn)
    unsigned >>= 8n
  }
  return bytes
}

/** Big-endian 4-byte encoding of a u32 length. */
function u32BigEndian(value: number): Uint8Array {
  const bytes = new Uint8Array(4)
  new DataView(bytes.buffer).setUint32(0, value, false)
  return bytes
}

/**
 * `OffsetDateTime::unix_timestamp()` on the Rust side is whole seconds,
 * truncating any sub-second component — an RFC 3339 string with fractional
 * seconds must be floored the same way here, not rounded, so a signature
 * produced from a timestamp with a fractional part still verifies.
 */
export function unixSecondsFromRfc3339(createdAt: string): bigint {
  const ms = Date.parse(createdAt)
  if (Number.isNaN(ms)) {
    throw new Error(`Not a valid RFC 3339 timestamp: ${createdAt}`)
  }
  return BigInt(Math.floor(ms / 1000))
}

/**
 * Reconstructs the exact bytes `crates/chain/src/sth.rs::sign_tree_head`
 * signs and `verify_tree_head` re-derives: the domain tag, `tree_size` as
 * big-endian i64, `root_hash`'s length-prefixed UTF-8 bytes, `network_id`'s
 * raw UTF-8 bytes, and the timestamp as big-endian i64 unix seconds.
 */
export function signingMessage(sth: SignedTreeHeadResponse): Uint8Array {
  const rootHashBytes = new TextEncoder().encode(sth.root_hash)
  const networkIdBytes = new TextEncoder().encode(sth.network_id)
  return concatBytes(
    DOMAIN_TAG,
    i64BigEndian(BigInt(sth.tree_size)),
    u32BigEndian(rootHashBytes.length),
    rootHashBytes,
    networkIdBytes,
    i64BigEndian(unixSecondsFromRfc3339(sth.created_at)),
  )
}
