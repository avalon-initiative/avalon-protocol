#!/usr/bin/env python3
"""Writes conformance/vectors/ledger-entry-hash.json.

An encoder for the ledger entry hash layout (avalon.ledger.entry) that shares no code with the
Rust one, so the committed expectations are an independent check of it.
"""
import hashlib
import json
import struct
import sys
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import signing_envelope as env  # noqa: E402

TAG = b"avalon.ledger.entry"


def s(text):
    raw = text.encode("utf-8")
    return struct.pack(">I", len(raw)) + raw


def canonical(doc):
    # Sufficient for the BMP keys and integer/string/bool/null values the vectors use.
    return json.dumps(doc, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def message(i, payload_hash):
    return (
        env.header(TAG, i["layoutVersion"], i["rulesVersion"])
        + s(i["networkId"])
        + s(i["shardId"])
        + struct.pack(">Q", int(i["seq"]))
        + uuid.UUID(i["eventId"]).bytes
        + s(i["kind"])
        + s(i["issuer"])
        + s(i["subject"])
        + struct.pack(">I", i["eventVersion"])
        + struct.pack(">q", int(i["timestampUnixMicros"]))
        + env.hash_algo(i["hashAlgo"])
        + bytes.fromhex(i["prevHashHex"])
        + bytes.fromhex(payload_hash)
        + env.extension_region(
            [(e["type"], e["critical"], bytes.fromhex(e["valueHex"])) for e in i["extensions"]]
        )
    )


ZERO = "00" * 32
BASE = dict(
    networkId="avalon-test-net",
    shardId="core",
    seq="1",
    prevHashHex=ZERO,
    eventId="0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
    kind="identity.created",
    issuer="identity:alice:self:created",
    subject="identity:alice:self:created",
    eventTimestampRfc3339="2026-01-02T03:04:05.123456Z",
    timestampUnixMicros="1767323045123456",
    layoutVersion=1,
    rulesVersion=1,
    hashAlgo=1,
    eventVersion=1,
    extensions=[],
)


def full(name, payload_json, **over):
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
            "entryHashHex": hashlib.sha256(msg).hexdigest(),
        },
    }


def skeleton(name, payload_hash, **over):
    i = dict(BASE, **over)
    msg = message(i, payload_hash)
    return {
        "name": name,
        "input": dict(i, payloadHashHex=payload_hash),
        "expected": {
            "signingBytesHex": msg.hex(),
            "entryHashHex": hashlib.sha256(msg).hexdigest(),
        },
    }


def reject(name, error, **over):
    i = dict(BASE, payloadHashHex=ZERO)
    i.update(over)
    return {"name": name, "input": i, "expected": {"error": error}}


vectors = [
    full("basic", '{"display_name":"Alice"}'),
    full("payload keys are sorted before hashing", '{"b":1,"a":{"z":true,"y":null},"c":[2,"x"]}'),
    full("empty payload object", "{}"),
    full("null payload", "null"),
    full("first entry, seq 0 and zero prev hash", '{"n":0}', seq="0"),
    full("seq at u64 max", '{"n":1}', seq="18446744073709551615"),
    full("event version 0", '{"n":1}', eventVersion=0),
    full("event version 65535", '{"n":1}', eventVersion=65535),
    full("event version above the old u16 slot", '{"n":1}', eventVersion=65536),
    full("event version u32 max", '{"n":1}', eventVersion=4294967295),
    full(
        "one non-critical extension",
        '{"n":1}',
        extensions=[{"type": 7, "critical": False, "valueHex": "cafe"}],
    ),
    full(
        "unknown non-critical extensions are hashed as received",
        '{"n":1}',
        extensions=[
            {"type": 1, "critical": False, "valueHex": ""},
            {"type": 4660, "critical": False, "valueHex": "00ff00"},
            {"type": 65535, "critical": False, "valueHex": "ab" * 40},
        ],
    ),
    full(
        "extension region exactly at the cap",
        '{"n":1}',
        extensions=[{"type": 9, "critical": False, "valueHex": "00" * (env.MAX_EXTENSION_BYTES_V1 - 7)}],
    ),
    full("non-zero prev hash", '{"n":1}', prevHashHex="ab" * 32),
    full("other shard", '{"n":1}', shardId="game:acme/eu-1"),
    full("self-certifying shard", '{"n":1}', shardId="node:" + "1f" * 32),
    full("empty kind, issuer and subject", '{"n":1}', kind="", issuer="", subject=""),
    full("boundary: kind ab, issuer c", '{"n":1}', kind="ab", issuer="c"),
    full("boundary: kind a, issuer bc", '{"n":1}', kind="a", issuer="bc"),
    full("boundary: network ab, shard c", '{"n":1}', networkId="ab", shardId="c"),
    full("boundary: network a, shard bc", '{"n":1}', networkId="a", shardId="bc"),
    full(
        "separators and NUL inside strings",
        '{"n":1}',
        kind="a:b,c",
        issuer="i\u0000j",
        subject=":::",
    ),
    full(
        "multi-byte UTF-8 in strings and payload",
        '{"name":"héllo 世界 ☃"}',
        kind="ké",
        issuer="世界",
        subject="☃",
    ),
    full(
        "sub-second time keeps microseconds",
        '{"n":1}',
        eventTimestampRfc3339="2026-01-02T03:04:05.000001Z",
        timestampUnixMicros="1767323045000001",
    ),
    full(
        "nanoseconds truncate to microseconds",
        '{"n":1}',
        eventTimestampRfc3339="2026-01-02T03:04:05.123456789Z",
        timestampUnixMicros="1767323045123456",
    ),
    full(
        "whole second",
        '{"n":1}',
        eventTimestampRfc3339="2026-01-02T03:04:05Z",
        timestampUnixMicros="1767323045000000",
    ),
    full(
        "time before the epoch",
        '{"n":1}',
        eventTimestampRfc3339="1969-12-31T23:59:59.999999Z",
        timestampUnixMicros="-1",
    ),
    full(
        "pre-2000 time with a sub-microsecond part floors",
        '{"n":1}',
        eventTimestampRfc3339="1969-12-31T23:59:59.9999995Z",
        timestampUnixMicros="-1",
    ),
    full(
        "offset time is the same instant",
        '{"n":1}',
        eventTimestampRfc3339="2026-01-02T05:04:05.123456+02:00",
        timestampUnixMicros="1767323045123456",
    ),
    skeleton("skeleton row, payload pruned", hashlib.sha256(canonical({"display_name": "Alice"}).encode()).hexdigest()),
    skeleton("skeleton row, arbitrary payload hash", "cd" * 32, seq="7", prevHashHex="ef" * 32),
]

reject_vectors = [
    reject("prev hash too short", "invalid_hash", prevHashHex="00" * 31),
    reject("prev hash upper case", "invalid_hash", prevHashHex="AB" * 32),
    reject("prev hash not hex", "invalid_hash", prevHashHex="zz" * 32),
    reject("payload hash too long", "invalid_hash", payloadHashHex="00" * 33),
    reject("negative seq", "out_of_range", seq="-1"),
    reject("seq above u64", "out_of_range", seq="18446744073709551616"),
    reject("event version above u32", "out_of_range", eventVersion=4294967296),
    reject("negative event version", "out_of_range", eventVersion=-1),
    reject("layout version above the supported range", "needs_newer_version", layoutVersion=2),
    reject("rules version above the supported range", "needs_newer_version", rulesVersion=2),
    reject("unknown hash algorithm", "needs_newer_version", hashAlgo=2),
    reject("hash algorithm 0 is not a hash", "needs_newer_version", hashAlgo=0),
    reject(
        "unknown critical extension",
        "needs_newer_version",
        extensions=[{"type": 3, "critical": True, "valueHex": ""}],
    ),
    reject(
        "extensions out of order",
        "extensions_unsorted",
        extensions=[
            {"type": 9, "critical": False, "valueHex": ""},
            {"type": 5, "critical": False, "valueHex": ""},
        ],
    ),
    reject(
        "duplicate extension type",
        "extension_duplicate",
        extensions=[
            {"type": 5, "critical": False, "valueHex": ""},
            {"type": 5, "critical": False, "valueHex": "00"},
        ],
    ),
    reject(
        "extension region one byte over the cap",
        "extensions_too_large",
        extensions=[{"type": 9, "critical": False, "valueHex": "00" * (env.MAX_EXTENSION_BYTES_V1 - 6)}],
    ),
]

doc = {
    "$schema": "./SCHEMA.md#ledger-entry-hash",
    "description": (
        "The ledger entry hash (avalon_protocol::ledger_entry, domain tag avalon.ledger.entry). "
        "entryHashHex is the digest (hashAlgo 1 = SHA-256) of signingBytesHex, which is the tag "
        "(ASCII, no length), layoutVersion as u16 BE, rulesVersion as u32 BE, then networkId and "
        "shardId (u32 BE length and UTF-8), seq as u64 BE, eventId (16 raw bytes), kind, issuer and "
        "subject (u32 BE length and UTF-8), eventVersion as u32 BE (the payload schema version of "
        "kind, not the layout version), timestampUnixMicros as i64 BE, hashAlgo as u8, prevHashHex "
        "(32 raw bytes), the payload hash (32 raw bytes) and the extensions region (count u16, then "
        "type u16, flags u8 with bit 0 = critical, length u32 and the value, strictly ascending by "
        "type, at most 4096 bytes counting 7 header bytes per entry for rules version 1). The payload hash is the SHA-256 of the canonical payload "
        "(canonical-payload.json) so a row with a pruned payload still verifies: skeleton vectors "
        "give payloadHashHex instead of payloadJsonUtf8. A timestamp is the instant truncated "
        "(toward negative infinity) to unix microseconds. prevHashHex, payloadHashHex are exactly "
        "64 lowercase hex characters, seq is a decimal string in 0..2^64-1 and eventVersion is "
        "0..2^32-1; extensions is a list of {type, critical, valueHex}. Anything else must be "
        "rejected with expected.error: invalid_hash, out_of_range, needs_newer_version (layout or "
        "rules version above the supported range, unknown hashAlgo, unknown critical extension), "
        "extensions_unsorted, extension_duplicate or extensions_too_large. "
        "Generated by scripts/gen-ledger-entry-vectors.py, an encoder independent of the Rust one."
    ),
    "supportedIn": ["rust"],
    "notSupported": {
        "csharp": "the SDK has no ledger entry hash yet",
        "typescript": "the SDK has no ledger entry hash yet",
    },
    "vectors": vectors,
    "rejectVectors": reject_vectors,
}

out = Path(__file__).resolve().parent.parent / "conformance" / "vectors" / "ledger-entry-hash.json"
text = json.dumps(doc, indent=2, ensure_ascii=False) + "\n"
if len(sys.argv) > 1 and sys.argv[1] == "--check":
    sys.exit(0 if out.read_text() == text else "ledger-entry-hash.json is stale")
out.write_text(text)
