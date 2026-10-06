#!/usr/bin/env python3
"""Writes the hashVectors of conformance/vectors/identity-chain.json.

An encoder for the identity chain event hash layout (avalon.identity.chain_event) that shares no
code with the Rust one, so the committed expectations are an independent check of it. The
resolutionCases are kept from the existing file.
"""
import hashlib
import json
import struct
import sys
import uuid
from pathlib import Path

TAG = b"avalon.identity.chain_event"
OUT = Path(__file__).resolve().parent.parent / "conformance" / "vectors" / "identity-chain.json"


def s(text):
    raw = text.encode("utf-8")
    return struct.pack(">I", len(raw)) + raw


def canonical(doc):
    # Sufficient for the BMP keys and integer/string/bool/null values the vectors use.
    return json.dumps(doc, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def message(i, payload_hash):
    prev = i["prevHashHex"]
    return (
        TAG
        + struct.pack(">H", 1)
        + bytes.fromhex(i["identityIdHex"])
        + struct.pack(">Q", int(i["seq"]))
        + (b"\x00" if prev is None else b"\x01" + bytes.fromhex(prev))
        + uuid.UUID(i["eventId"]).bytes
        + s(i["kind"])
        + s(i["issuer"])
        + s(i["subject"])
        + struct.pack(">I", i["eventVersion"])
        + struct.pack(">q", int(i["timestampUnixMicros"]))
        + bytes.fromhex(payload_hash)
    )


ID = "97129fe5aedcb1e8eccb2072bcf83e58478a22330dd589afd9d4545764f0cd56"
SELF = f"identity:{ID}:self:profile_updated"
BASE = dict(
    identityIdHex=ID,
    seq="1",
    prevHashHex=None,
    eventId="0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
    kind="profile.updated",
    issuer=SELF,
    subject=SELF,
    eventVersion=1,
    timestampUnixMicros="1700000000000000",
)


def vec(name, payload_json, **over):
    i = dict(BASE, **over)
    canon = canonical(json.loads(payload_json))
    ph = hashlib.sha256(canon.encode("utf-8")).hexdigest()
    msg = message(i, ph)
    return {
        "name": name,
        "input": dict(i, payloadJsonUtf8=payload_json),
        "expected": {
            "payloadCanonicalUtf8": canon,
            "payloadHashHex": ph,
            "signingBytesHex": msg.hex(),
            "eventHashHex": hashlib.sha256(msg).hexdigest(),
        },
    }


vectors = [vec("genesis profile update", '{"bio":"first"}')]
first = vectors[0]["expected"]["eventHashHex"]
vectors.append(
    vec(
        "second event points at the first",
        '{"bio":"second"}',
        seq="2",
        prevHashHex=first,
        eventId="0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c",
        timestampUnixMicros="1700000060000000",
    )
)
vectors += [
    vec(
        "signing key added",
        '{"public_key":"AAAA"}',
        kind="identity.signing_key_added",
        issuer=f"identity:{ID}:self:signing_key_added",
        subject=f"identity:{ID}:self:signing_key_added",
        seq="3",
        prevHashHex="af" * 32,
    ),
    vec("payload keys are sorted before hashing", '{"b":1,"a":{"z":true,"y":null},"c":[2,"x"]}'),
    vec("empty payload object", "{}"),
    vec("null payload", "null"),
    vec("seq at u64 max", '{"n":1}', seq="18446744073709551615", prevHashHex="ab" * 32),
    vec("seq 0", '{"n":1}', seq="0"),
    vec("event version 0", '{"n":1}', eventVersion=0),
    vec("event version u32 max", '{"n":1}', eventVersion=4294967295),
    vec("event version 2", '{"n":1}', eventVersion=2),
    vec("non-zero prev hash", '{"n":1}', seq="2", prevHashHex="ab" * 32),
    vec("all-zero prev hash is not genesis", '{"n":1}', seq="2", prevHashHex="00" * 32),
    vec("other identity", '{"n":1}', identityIdHex="11" * 32),
    vec("nil event id", '{"n":1}', eventId="00000000-0000-0000-0000-000000000000"),
    vec("empty kind, issuer and subject", '{"n":1}', kind="", issuer="", subject=""),
    vec("boundary: kind ab, issuer c", '{"n":1}', kind="ab", issuer="c", subject="d"),
    vec("boundary: kind a, issuer bc", '{"n":1}', kind="a", issuer="bc", subject="d"),
    vec("boundary: issuer ab, subject c", '{"n":1}', kind="k", issuer="ab", subject="c"),
    vec("boundary: issuer a, subject bc", '{"n":1}', kind="k", issuer="a", subject="bc"),
    vec(
        "separators and NUL inside strings",
        '{"n":1}',
        kind="a:b,c",
        issuer="i\u0000j",
        subject=":::",
    ),
    vec(
        "multi-byte UTF-8 in strings and payload",
        '{"name":"héllo 世界 ☃"}',
        kind="ké",
        issuer="世界",
        subject="☃",
    ),
    vec("microsecond time", '{"n":1}', timestampUnixMicros="1700000000000001"),
    vec("time before the epoch", '{"n":1}', timestampUnixMicros="-1"),
    vec("time at the epoch", '{"n":1}', timestampUnixMicros="0"),
    vec("time at i64 min", '{"n":1}', timestampUnixMicros="-9223372036854775808"),
    vec("time at i64 max", '{"n":1}', timestampUnixMicros="9223372036854775807"),
]

old = json.loads(OUT.read_text())
doc = {
    "$schema": "./SCHEMA.md#identity-chain",
    "description": (
        "Per-identity event chains (avalon_protocol::identity_chain). hashVectors pin the chain event "
        "hash (domain tag avalon.identity.chain_event): eventHashHex is the SHA-256 of signingBytesHex, "
        "which is the tag (ASCII, no length), layout version 1 as u16 BE, identityIdHex (32 raw bytes), "
        "seq as u64 BE, prevHashHex (u8 0 for genesis, or u8 1 then 32 raw bytes), eventId (16 raw bytes), "
        "kind, issuer and subject (u32 BE length and UTF-8), eventVersion as u32 BE, "
        "timestampUnixMicros as i64 BE (the instant floored to microseconds) and the payload hash "
        "(32 raw bytes, the SHA-256 of the canonical payload from canonical-payload.json). The chain "
        "position is never part of the payload. seq and timestampUnixMicros are decimal strings. "
        "resolutionCases pin the deterministic conflict rule (apply_chain): a runner must reach the "
        "same accepted chain and fork position for every ordering of a case's events. "
        "hashVectors are generated by scripts/gen-identity-chain-vectors.py, an encoder independent "
        "of the Rust one."
    ),
    "supportedIn": ["rust"],
    "notSupported": old["notSupported"],
    "hashVectors": vectors,
    "resolutionCases": old["resolutionCases"],
}
text = json.dumps(doc, indent=2, ensure_ascii=False) + "\n"
if len(sys.argv) > 1 and sys.argv[1] == "--check":
    sys.exit(0 if OUT.read_text() == text else "identity-chain.json is stale")
OUT.write_text(text)
