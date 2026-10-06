#!/usr/bin/env python3
"""Writes the identity key-event signing vectors: identity-created-signing.json,
device-grant-approval.json and signing-key-revoked.json under conformance/vectors.

A structured-bytes encoder that shares no code with the Rust one, so the committed
expectations are an independent check of it. Requires the `cryptography` package.
"""
import hashlib
import json
import struct
import sys
import uuid
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import signing_envelope as env  # noqa: E402

OUT = Path(__file__).resolve().parent.parent / "conformance" / "vectors"
CHECK = "--check" in sys.argv[1:]
STALE = []
SIGNER_SEED = bytes.fromhex("44" * 32)
DEVICE_SEED = bytes.fromhex("55" * 32)
SECOND_SEED = bytes.fromhex("66" * 32)
ID_TAG_HEX = b"avalon-identity-id-v1"


def pubkey(seed):
    return Ed25519PrivateKey.from_private_bytes(seed).public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )


def sign(seed, message):
    return Ed25519PrivateKey.from_private_bytes(seed).sign(message)


def identity_id_of(public_key):
    return hashlib.sha256(ID_TAG_HEX + public_key).digest()


def st(text):
    raw = text.encode("utf-8")
    return struct.pack(">I", len(raw)) + raw


def uid(text):
    return uuid.UUID(text).bytes


def position(seq, prev_hash_hex):
    out = struct.pack(">Q", int(seq)) + env.hash_algo()
    if prev_hash_hex is None:
        return out + b"\x00"
    raw = bytes.fromhex(prev_hash_hex)
    assert len(raw) == 32
    return out + b"\x01" + raw


def header(tag):
    return env.header(tag.encode("ascii"), 1, env.RULES_VERSION)


def tail():
    return env.extension_region()


def created_bytes(i):
    return (
        header("avalon.identity.created")
        + st(i["networkId"])
        + st(i["shardId"])
        + uid(i["ticketId"])
        + identity_id_of(pubkey(SIGNER_SEED))
        + env.key(pubkey(SIGNER_SEED))
        + st(i["displayName"])
        + tail()
    )


def grant_bytes(i):
    return (
        header("avalon.device_grant.approved")
        + uid(i["grantId"])
        + bytes.fromhex(i["identityId"])
        + uid(i["approverSigningKeyId"])
        + env.key(bytes.fromhex(i["requestedPublicKeyHex"]))
        + position(i["seq"], i["prevHashHex"])
        + tail()
    )


def revoked_bytes(i):
    return (
        header("avalon.identity.signing_key_revoked")
        + bytes.fromhex(i["identityId"])
        + uid(i["signingKeyId"])
        + uid(i["revokedBySigningKeyId"])
        + position(i["seq"], i["prevHashHex"])
        + tail()
    )


def legacy_created(i):
    pk = pubkey(SIGNER_SEED).hex()
    return (
        f"avalon:identity.created:v2:{len(i['networkId'].encode())}:{i['networkId']}:"
        f"{len(i['shardId'].encode())}:{i['shardId']}:{i['ticketId']}:"
        f"{identity_id_of(pubkey(SIGNER_SEED)).hex()}:{pk}:{i['displayName']}"
    )


def legacy_grant(i):
    return f"avalon:device_grant.approved:v2:{i['grantId']}:{i['identityId']}:{i['requestedPublicKeyHex']}"


def legacy_revoked(i):
    return (
        f"avalon:identity.signing_key_revoked:v2:{i['identityId']}:"
        f"{i['signingKeyId']}:{i['revokedBySigningKeyId']}"
    )


def vector(name, i, build):
    msg = build(i)
    return {
        "name": name,
        "input": i,
        "expected": {
            "signingBytesHex": msg.hex(),
            "signatureHex": sign(SIGNER_SEED, msg).hex(),
        },
    }


def replay(name, base, changed, build):
    """A signature over `base` offered for the bytes of `changed`: must not verify."""
    assert build(base) != build(changed)
    return {
        "name": name,
        "input": changed,
        "signatureHex": sign(SIGNER_SEED, build(base)).hex(),
        "expected": {"valid": False},
    }


def legacy(name, i, legacy_build):
    text = legacy_build(i)
    return {
        "name": name,
        "input": i,
        "legacySigningBytesUtf8": text,
        "signatureHex": sign(SIGNER_SEED, text.encode()).hex(),
        "expected": {"valid": False},
    }


def header_fields(description, generation, extra=None):
    doc = {
        "description": description,
        "generation": generation,
        "identityId": identity_id_of(pubkey(SIGNER_SEED)).hex(),
        "signingKeySeedHex": SIGNER_SEED.hex(),
        "signingPublicKeyHex": pubkey(SIGNER_SEED).hex(),
    }
    doc.update(extra or {})
    return doc


def finish(name, schema, doc, reason):
    out = {"$schema": f"./SCHEMA.md#{schema}"}
    out.update(doc)
    out["supportedIn"] = ["rust"]
    out["notSupported"] = {"csharp": reason, "typescript": reason}
    text = json.dumps(out, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    if CHECK:
        if (OUT / name).read_text() != text:
            STALE.append(name)
    else:
        (OUT / name).write_text(text)


STRUCTURED = (
    "The layout is the structured signing-bytes encoding (avalon_protocol::signing_bytes): the "
    "ASCII domain tag with no length, a u16 big-endian layout version (1) and a u32 big-endian rules "
    "version (1), then the fields in the order listed, then the extensions region (count u16 = 0: "
    "00 00). str is a u32 big-endian byte length and the UTF-8 bytes; uuid is its 16 raw bytes; "
    "identity ids are 32 raw bytes; a public key is the algorithm byte 01 (Ed25519) then its 32 raw "
    "bytes; seq is a u64 big-endian, followed by the hash algorithm byte 01 (SHA-256) and prev_hash, "
    "one byte (0 = no previous event, 1 = a 32-byte event hash follows). The signature is Ed25519 by "
    "the signer key over the bytes, verified strictly."
)
GEN = (
    "Generated by scripts/gen-identity-key-event-vectors.py, an encoder that shares no code with "
    "the Rust one (cryptography Ed25519, hashlib). signingKeySeedHex is the Ed25519 seed of the "
    "signer; Ed25519 signatures are deterministic."
)
REASON = (
    "Protocol-only until the SDK slice for #1214 (avalon-sdks #99, #100, #101) lands the "
    "structured identity key-event layouts."
)

IDENTITY = identity_id_of(pubkey(SIGNER_SEED)).hex()
DEVICE_KEY = pubkey(DEVICE_SEED).hex()
SECOND_KEY = pubkey(SECOND_SEED).hex()
KEY_A = "3f2b8c1a-9d4e-4f6a-8b7c-0a1b2c3d4e5f"
KEY_B = "a1b2c3d4-e5f6-4789-8abc-def012345678"
HEAD = hashlib.sha256(b"identity chain head").hexdigest()
OTHER_HEAD = hashlib.sha256(b"another head").hexdigest()


def write_created():
    t = "0b7a2c1e-5d4f-4a3b-9c8d-1e2f3a4b5c6d"

    def inp(net, shard, ticket, name):
        return {"networkId": net, "shardId": shard, "ticketId": ticket, "displayName": name}

    base = inp("avalon-dev-local", "core", t, "Alice")
    vectors = [
        vector('display name "Alice" on avalon-dev-local/core', base, created_bytes),
        vector('display name "a:b:c" on avalon-dev-local/core', inp("avalon-dev-local", "core", t, "a:b:c"), created_bytes),
        vector(
            "multi-byte display name with a trailing colon",
            inp("avalon-int-1", "game:wow-demo/3", "6f1d0c9a-2b3e-4c5d-8e7f-0a1b2c3d4e5f", "Zoë 日本語 🎮 :end"),
            created_bytes,
        ),
        vector(
            "multi-byte UTF-8 network and shard ids use byte lengths (11 and 13)",
            inp("avalon-é-1", "game:wow-é/3", "3c1f5a7e-9b2d-4e6f-8a0b-1c2d3e4f5a6b", "Alice"),
            created_bytes,
        ),
        vector("empty display name", inp("avalon-dev-local", "core", t, ""), created_bytes),
        vector("NUL in the display name", inp("avalon-dev-local", "core", t, "a\u0000b"), created_bytes),
    ]
    t2 = "11111111-1111-4111-8111-111111111111"
    replays = [
        replay("signature for ticket A is not valid for ticket B", base, dict(base, ticketId=t2), created_bytes),
        replay("signature for network A is not valid for network B", base, dict(base, networkId="avalon-int-1"), created_bytes),
        replay("signature for shard A is not valid for shard B", base, dict(base, shardId="game:other/1"), created_bytes),
        replay("a ':' moved across the network and shard boundary", inp("avalon-dev-local", "core", t, "Alice"), inp("avalon-dev-local:core", "", t, "Alice"), created_bytes),
        replay("bytes moved from the shard into the display name", inp("net", "ab", t, "c"), inp("net", "a", t, "bc"), created_bytes),
        replay("bytes moved from the network into the shard", inp("ab", "c", t, "n"), inp("a", "bc", t, "n"), created_bytes),
    ]
    legacy_vectors = [legacy("the old text layout does not verify", base, legacy_created)]
    doc = header_fields(
        "identity.created signing bytes (avalon_protocol::identity_id::identity_created_signing_bytes): "
        "tag avalon.identity.created, layout version 1, fields network_id (str), shard_id (str), ticket_id "
        "(uuid), identity_id (32 raw bytes), inception public key (algorithm byte + 32 raw bytes), display_name (str). "
        "The ticket is the server-issued register/start ticket and is also the id of the inception "
        "signing key; network_id and shard_id bind the ledger stream, so a copied signature does not "
        "verify for another ticket, network or shard (replayVectors). The event is unchained, so it "
        "signs no chain position. " + STRUCTURED +
        " legacyLayoutVectors carry a signature over the retired colon-delimited text layout; it must not verify.",
        GEN,
        {"replayVectors": replays, "legacyLayoutVectors": legacy_vectors, "vectors": vectors},
    )
    finish("identity-created-signing.json", "identity-created-signing", doc, REASON)


def write_grant():
    base = {
        "grantId": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
        "identityId": IDENTITY,
        "approverSigningKeyId": KEY_A,
        "requestedPublicKeyHex": DEVICE_KEY,
        "seq": "1",
        "prevHashHex": None,
    }
    mid = dict(base, seq="4", prevHashHex=HEAD)
    vectors = [
        vector("first chained event of a new chain (seq 1, no prev_hash)", base, grant_bytes),
        vector("extending a chain head", mid, grant_bytes),
        vector("largest seq and an all-ones prev_hash", dict(base, seq="18446744073709551615", prevHashHex="ff" * 32), grant_bytes),
        vector("an all-zero prev_hash is not the same as no prev_hash", dict(base, prevHashHex="00" * 32), grant_bytes),
    ]
    replays = [
        replay("signed under another approver key id", mid, dict(mid, approverSigningKeyId=KEY_B), grant_bytes),
        replay("signed at another seq", mid, dict(mid, seq="5"), grant_bytes),
        replay("signed on another chain head", mid, dict(mid, prevHashHex=OTHER_HEAD), grant_bytes),
        replay("prev_hash dropped", mid, dict(mid, prevHashHex=None), grant_bytes),
        replay("another grant id", mid, dict(mid, grantId="7c9e6679-7425-40de-944b-e07fc1f90ae8"), grant_bytes),
        replay("another requested device key", mid, dict(mid, requestedPublicKeyHex=SECOND_KEY), grant_bytes),
        replay("another identity", mid, dict(mid, identityId=identity_id_of(pubkey(SECOND_SEED)).hex()), grant_bytes),
    ]
    legacy_vectors = [legacy("the old text layout does not verify", base, legacy_grant)]
    doc = header_fields(
        "Device grant approval signing bytes (avalon_protocol::identity_id::device_grant_approval_signing_bytes): "
        "tag avalon.device_grant.approved, layout version 1, fields grant_id (uuid), identity_id (32 raw bytes), "
        "approver_signing_key_id (uuid), requested device public key (algorithm byte + 32 raw bytes), seq (u64), hash algorithm byte, prev_hash "
        "(flag byte, then 32 bytes when present). The approver signs the chain position the approval will "
        "occupy: seq is the identity chain head seq plus one and prev_hash the head event hash, absent for "
        "the first chained event. The grant id is also the id of the key the grant creates. " + STRUCTURED +
        " replayVectors offer a signature made for one input with another input's bytes; legacyLayoutVectors "
        "carry a signature over the retired text layout. Neither may verify.",
        GEN,
        {
            "requestedKeySeedHex": DEVICE_SEED.hex(),
            "requestedPublicKeyHex": DEVICE_KEY,
            "replayVectors": replays,
            "legacyLayoutVectors": legacy_vectors,
            "vectors": vectors,
        },
    )
    finish("device-grant-approval.json", "device-grant-approval", doc, REASON)


def write_revoked():
    base = {
        "identityId": IDENTITY,
        "signingKeyId": KEY_A,
        "revokedBySigningKeyId": KEY_B,
        "seq": "2",
        "prevHashHex": HEAD,
    }
    vectors = [
        vector("first key revoked by second", base, revoked_bytes),
        vector("ids swapped give different bytes", dict(base, signingKeyId=KEY_B, revokedBySigningKeyId=KEY_A), revoked_bytes),
        vector("self-revocation", dict(base, revokedBySigningKeyId=KEY_A), revoked_bytes),
        vector("largest seq and an all-ones prev_hash", dict(base, seq="18446744073709551615", prevHashHex="ff" * 32), revoked_bytes),
        vector("an all-zero prev_hash is not the same as no prev_hash", dict(base, seq="1", prevHashHex="00" * 32), revoked_bytes),
        vector("no prev_hash", dict(base, seq="1", prevHashHex=None), revoked_bytes),
    ]
    replays = [
        replay("signed for another revoked key id", base, dict(base, signingKeyId="3f2b8c1a-9d4e-4f6a-8b7c-0a1b2c3d4e60"), revoked_bytes),
        replay("signed under another revoker key id", base, dict(base, revokedBySigningKeyId="a1b2c3d4-e5f6-4789-8abc-def012345679"), revoked_bytes),
        replay("signed at another seq", base, dict(base, seq="3"), revoked_bytes),
        replay("signed on another chain head", base, dict(base, prevHashHex=OTHER_HEAD), revoked_bytes),
        replay("prev_hash dropped", base, dict(base, prevHashHex=None), revoked_bytes),
        replay("another identity", base, dict(base, identityId=identity_id_of(pubkey(SECOND_SEED)).hex()), revoked_bytes),
    ]
    legacy_vectors = [legacy("the old text layout does not verify", base, legacy_revoked)]
    doc = header_fields(
        "Signing-key revocation signing bytes (avalon_protocol::identity_id::signing_key_revoked_signing_bytes): "
        "tag avalon.identity.signing_key_revoked, layout version 1, fields identity_id (32 raw bytes), "
        "signing_key_id (uuid, the revoked key), revoked_by_signing_key_id (uuid), seq (u64), hash algorithm byte, prev_hash "
        "(flag byte, then 32 bytes when present). seq is the identity chain head seq plus one and prev_hash "
        "the head event hash, absent for the first chained event. " + STRUCTURED +
        " replayVectors offer a signature made for one input with another input's bytes; legacyLayoutVectors "
        "carry a signature over the retired text layout. Neither may verify.",
        GEN,
        {"replayVectors": replays, "legacyLayoutVectors": legacy_vectors, "vectors": vectors},
    )
    finish("signing-key-revoked.json", "signing-key-revoked", doc, REASON)


if __name__ == "__main__":
    write_created()
    write_grant()
    write_revoked()
    if STALE:
        sys.exit("stale: " + ", ".join(STALE))
