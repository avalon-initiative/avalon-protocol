#!/usr/bin/env python3
"""Writes conformance/vectors/structured-signing-bytes.json and conformance/vectors/envelope.json.

An encoder for the structured signing-bytes primitive (header, algorithm tags, extensions region)
that shares no code with the Rust one, so the committed expectations are an independent check of it.
The build inputs of structured-signing-bytes.json are kept from the existing file; the bytes and
every reject and envelope vector are produced here. Run with --check to fail when a file is stale.
"""
import hashlib
import json
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import signing_envelope as env  # noqa: E402

VECTORS = Path(__file__).resolve().parent.parent / "conformance" / "vectors"
CONF = b"avalon.conformance.vector"
INTEREST = b"avalon.interest_claim"
CAP = env.MAX_EXTENSION_BYTES_V1


def s(text):
    raw = text.encode("utf-8")
    return struct.pack(">I", len(raw)) + raw


def field_bytes(f):
    """The raw payload bytes a field object stands for (before any length prefix or tag)."""
    t = f["type"]
    if t == "str":
        return (f["utf8"] if "utf8" in f else f["repeatUtf8"] * f["count"]).encode("utf-8")
    if t == "bytes":
        return bytes.fromhex(f["hex"]) if "hex" in f else bytes.fromhex(f["repeatByteHex"]) * f["count"]
    if t in ("key", "hash", "fixed", "signature"):
        return bytes.fromhex(f["hex"])
    raise ValueError(t)


def encode_field(f):
    t = f["type"]
    if t in ("str", "bytes"):
        raw = field_bytes(f)
        return struct.pack(">I", len(raw)) + raw
    if t == "key":
        return env.key(field_bytes(f))
    if t == "signature":
        return env.signature(field_bytes(f))
    if t in ("hash", "fixed"):
        return field_bytes(f)
    if t == "hash_algo":
        return env.hash_algo(int(f["value"]))
    if t == "uuid":
        import uuid

        return uuid.UUID(f["value"]).bytes
    pack = {"u8": ">B", "u16": ">H", "u32": ">I", "u64": ">Q", "i64": ">q"}[t]
    return struct.pack(pack, int(f["value"]))


def ext_list(exts):
    return [(e["type"], e["critical"], bytes.fromhex(e["valueHex"])) for e in exts]


def build(inp):
    msg = env.header(inp["tag"].encode(), inp["layoutVersion"], inp.get("rulesVersion", 1))
    for f in inp["fields"]:
        msg += encode_field(f)
    return msg + env.extension_region(ext_list(inp.get("extensions", [])))


def expected_of(msg):
    if len(msg) > 4096:
        return {"signingBytesLength": len(msg), "signingBytesSha256Hex": hashlib.sha256(msg).hexdigest()}
    return {"signingBytesHex": msg.hex()}


def dump(doc):
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"


def msg(body=b"", layout=1, rules=1, region=None, tag=CONF):
    """A raw message: header, body bytes, then an extensions region (empty unless given)."""
    return env.header(tag, layout, rules) + body + (env.extension_region() if region is None else region)


def raw_region(entries):
    """An extensions region written verbatim, so malformed ones can be expressed.

    entries: (ext_type, flags, value) in the order given, with no validation."""
    out = struct.pack(">H", len(entries))
    for ext_type, flags, value in entries:
        out += struct.pack(">HBI", ext_type, flags, len(value)) + value
    return out


# ------------------------------------------------------- structured-signing-bytes.json -------


def gen_structured():
    old = json.loads((VECTORS / "structured-signing-bytes.json").read_text())
    vectors = []
    for v in old["vectors"]:
        if v["name"] in ("version 0", "layout version 1, no fields"):
            continue  # the old u16 slot is gone; "no fields" already covers layout version 1
        inp = dict(v["input"])
        if "version" in inp:  # first run over the old file: the layout version is the old u16 slot
            name = {"version 0": "layout version 1, no fields", "version 65535": "layout version 2, one string"}.get(
                v["name"], v["name"]
            )
            inp["layoutVersion"] = {"version 0": 1, "version 65535": 2}.get(v["name"], inp["version"])
            del inp["version"]
        else:
            name = v["name"]
        # Keep the key order stable: tag, layoutVersion, fields.
        inp = {"tag": inp["tag"], "layoutVersion": inp["layoutVersion"], "fields": inp["fields"]}
        vectors.append({"name": name, "input": inp, "expected": expected_of(build(inp))})

    def b(body):
        return msg(body).hex()

    reject = [
        ("tag of another kind", CONF, msg(s("ab"), tag=INTEREST), ["str"], {"error": "tag_mismatch"}),
        ("tag is a strict prefix of the message tag", CONF, CONF[:-1] + env.header(b"", 1, 1) + env.extension_region(), [], {"error": "tag_mismatch"}),
        ("empty message", CONF, b"", [], {"error": "tag_mismatch"}),
        ("message ends inside the layout version", CONF, CONF + b"\x00", [], {"error": "truncated"}),
        ("message ends inside the rules version", CONF, CONF + b"\x00\x01\x00\x00", [], {"error": "truncated"}),
        ("message ends inside the length prefix", CONF, env.header(CONF) + b"\x00\x00", ["str"], {"error": "truncated"}),
        ("declared length runs past the end", CONF, env.header(CONF) + b"\x00\x00\x00\x02a", ["str"], {"error": "truncated"}),
        ("declared length is 4 GiB - 1", CONF, env.header(CONF) + b"\xff\xff\xff\xffx", ["bytes"], {"error": "truncated"}),
        ("fixed field cut short", CONF, env.header(CONF) + b"\x00\x00\x00", ["u32"], {"error": "truncated"}),
        ("extensions count missing", CONF, env.header(CONF), [], {"error": "truncated"}),
        ("extension entry cut short", CONF, env.header(CONF) + b"\x00\x01\x00\x05\x00", [], {"error": "truncated"}),
        ("extension length runs past the end", CONF, env.header(CONF) + b"\x00\x01\x00\x05\x00\x00\x00\x00\x09ab", [], {"error": "truncated"}),
        ("bytes remain after the extensions region", CONF, msg(s("ab")) + b"\x00", ["str"], {"error": "trailing_bytes"}),
        ("a field left unread before the extensions region", CONF, msg(s("ab")), [], {"error": "trailing_bytes"}),
        ("string is not UTF-8", CONF, msg(b"\x00\x00\x00\x02\xc3\x28"), ["str"], {"error": "invalid_utf8"}),
        ("string is a lone surrogate encoding", CONF, msg(b"\x00\x00\x00\x03\xed\xa0\x80"), ["str"], {"error": "invalid_utf8"}),
    ]
    reject_vectors = [
        {"name": n, "input": {"tag": t.decode(), "messageHex": m.hex(), "read": r}, "expected": e}
        for n, t, m, r, e in reject
    ]
    doc = {
        "$schema": "./SCHEMA.md#structured-signing-bytes",
        "description": (
            "Structured signing bytes (avalon_protocol::signing_bytes): domain tag (ASCII, no length), "
            "layoutVersion as u16 big-endian, rulesVersion as u32 big-endian (1 unless input.rulesVersion "
            "says otherwise), the fields in a fixed order, then the extensions region (count u16; see "
            "envelope.json). str and bytes are a u32 big-endian length and the bytes; key is the algorithm "
            "tag 1 (Ed25519) then 32 raw bytes, signature is the algorithm tag 1 then 64 raw bytes; hash, "
            "fixed and uuid are raw bytes; hash_algo is one byte; u8, u16, u32, u64 and i64 are fixed-width "
            "big-endian. Every build vector uses the reserved conformance tag, whose supported layout "
            "versions are 1 and 2. A field object is {type: str, utf8} or {type: str, repeatUtf8, count} "
            "(count repetitions of a UTF-8 string), {type: bytes, hex} or {type: bytes, repeatByteHex, "
            "count}, {type: key|hash|fixed|signature, hex} (a key is 32 bytes, a signature 64, a hash 32, "
            "fixed 4), {type: uuid, value} or {type: u8|u16|u32|u64|i64|hash_algo, value} with value a "
            "decimal string. A build vector's expected is signingBytesHex, or signingBytesLength plus "
            "signingBytesSha256Hex when the message is large. Every build vector must also read back, field "
            "by field in order, to its input fields and then have no bytes left. rejectVectors list messages "
            "that must fail with exactly expected.error when read as input.read (the field types in order) "
            "under input.tag: tag_mismatch (message does not start with the tag), truncated (the message "
            "ends inside a header value, a length prefix, a field, the extensions region, or a declared "
            "length exceeds the remaining bytes, checked before any allocation), invalid_utf8 (str bytes are "
            "not valid UTF-8, no lossy decoding) and trailing_bytes (bytes remain after the extensions "
            "region). Errors are checked in that order of discovery while reading left to right. Header and "
            "extension semantics (versions above range, algorithms, critical and unsorted extensions) are "
            "in envelope.json. Generated by scripts/gen-envelope-vectors.py, an encoder independent of the "
            "Rust one."
        ),
        "supportedIn": ["rust", "csharp", "typescript"],
        "vectors": vectors,
        "rejectVectors": reject_vectors,
    }
    return dump(doc)


# ------------------------------------------------------------------------- envelope.json -----


def ext(t, critical=False, value=b""):
    return {"type": t, "critical": critical, "valueHex": value.hex()}


def gen_envelope():
    build_in = []

    def bv(name, **kw):
        inp = {"tag": CONF.decode(), "layoutVersion": 1, "fields": [], **kw}
        build_in.append({"name": name, "input": inp, "expected": expected_of(build(inp))})

    bv("header with no fields and no extensions")
    bv("layout version 2, the newest conformance layout", layoutVersion=2)
    bv("rules version 1 stated explicitly", rulesVersion=1)
    bv(
        "hash algorithm byte before a hash",
        fields=[{"type": "hash_algo", "value": "1"}, {"type": "hash", "hex": "ab" * 32}],
    )
    bv(
        "a key and a signature carry the Ed25519 algorithm tag",
        fields=[{"type": "key", "hex": "11" * 32}, {"type": "signature", "hex": "22" * 64}],
    )
    bv("one non-critical extension", extensions=[ext(7, False, b"\xca\xfe")])
    bv(
        "extensions in ascending order with an empty value",
        extensions=[ext(1), ext(2, False, b"\x00"), ext(65535, False, b"\xff" * 3)],
    )
    bv(
        "extension region at the cap for rules version 1",
        extensions=[ext(9, False, b"\x00" * (CAP - 7))],
    )
    bv(
        "two extensions filling the cap",
        extensions=[ext(1, False, b"\x01" * 2000), ext(2, False, b"\x02" * (CAP - 2000 - 14))],
    )

    ok = lambda body, **kw: msg(body, **kw)  # noqa: E731

    reads = []

    def rd(name, message, read, expected):
        reads.append({"name": name, "input": {"tag": CONF.decode(), "messageHex": message.hex(), "read": read}, "expected": expected})

    def region_of(entries):
        return env.extension_region(entries)

    # Unknown non-critical extensions are accepted, kept byte for byte and hash the same message.
    m = ok(s("x"), region=region_of([(5, False, b"abc"), (9, False, b""), (0x7001, False, b"\x01\x02")]))
    rd(
        "unknown non-critical extensions are preserved byte for byte",
        m,
        ["str"],
        {
            "layoutVersion": 1,
            "rulesVersion": 1,
            "extensions": [ext(5, False, b"abc"), ext(9), ext(0x7001, False, b"\x01\x02")],
            "rebuiltHex": m.hex(),
        },
    )
    rd(
        "an older layout version of a known tag still verifies",
        ok(s("a"), layout=1),
        ["str"],
        {"layoutVersion": 1, "rulesVersion": 1, "extensions": [], "rebuiltHex": ok(s("a"), layout=1).hex()},
    )
    rd(
        "the newest layout version reads the same way",
        ok(s("a"), layout=2),
        ["str"],
        {"layoutVersion": 2, "rulesVersion": 1, "extensions": [], "rebuiltHex": ok(s("a"), layout=2).hex()},
    )
    m = ok(b"" , region=region_of([(1, False, b"\x00" * (CAP - 7))]))
    rd(
        "extension entries exactly at the cap are accepted",
        m,
        [],
        {"layoutVersion": 1, "rulesVersion": 1, "extensions": [ext(1, False, b"\x00" * (CAP - 7))], "rebuiltHex": m.hex()},
    )
    m = ok(env.hash_algo() + b"\x07" * 32)
    rd(
        "a known hash algorithm and hash read back",
        m,
        ["hash_algo", "hash"],
        {"layoutVersion": 1, "rulesVersion": 1, "extensions": [], "rebuiltHex": m.hex()},
    )
    m = ok(env.key(b"\x03" * 32) + env.signature(b"\x04" * 64))
    rd(
        "a key and a signature with algorithm tag 1 read back",
        m,
        ["key", "signature"],
        {"layoutVersion": 1, "rulesVersion": 1, "extensions": [], "rebuiltHex": m.hex()},
    )

    rj = []

    def rej(name, message, read, **expected):
        rj.append({"name": name, "input": {"tag": CONF.decode(), "messageHex": message.hex(), "read": read}, "expected": expected})

    needs = lambda what, required: {"error": "needs_newer_version", "what": what, "required": required}  # noqa: E731
    rej("layout version above the supported range", ok(s("fields of an unknown layout"), layout=3), ["str"], **needs("layout", 3))
    rej("layout version 65535", ok(b"", layout=65535), [], **needs("layout", 65535))
    rej("a newer layout is reported before its fields are read", msg(b"\xde\xad", layout=3), ["str"], **needs("layout", 3))
    rej("rules version above the supported range", ok(b"", rules=2), [], **needs("rules", 2))
    rej("rules version u32 max", ok(b"", rules=4294967295), [], **needs("rules", 4294967295))
    rej("layout version 0 is below every range", ok(b"", layout=0), [], error="unsupported_version", what="layout", value=0)
    rej("rules version 0 is below the range", ok(b"", rules=0), [], error="unsupported_version", what="rules", value=0)
    for algo in (0, 2, 255):
        rej(f"unknown hash algorithm {algo}", ok(bytes([algo]) + b"\x00" * 32), ["hash_algo", "hash"], **needs("hash_algo", algo))
    for algo in (0, 2, 255):
        rej(f"unknown key algorithm {algo}", ok(bytes([algo]) + b"\x00" * 32), ["key"], **needs("sig_algo", algo))
    rej("unknown signature algorithm 2", ok(b"\x02" + b"\x00" * 64), ["signature"], **needs("sig_algo", 2))
    rej(
        "unknown critical extension",
        ok(s("x"), region=region_of([(0x1234, True, b"")])),
        ["str"],
        **needs("critical_extension", 0x1234),
    )
    rej(
        "an unknown critical extension among non-critical ones",
        ok(b"", region=region_of([(1, False, b"a"), (2, True, b"b"), (3, False, b"c")])),
        [],
        **needs("critical_extension", 2),
    )
    rej(
        "extensions out of order",
        ok(b"", region=raw_region([(9, 0, b""), (5, 0, b"")])),
        [],
        error="extensions_unsorted",
    )
    rej(
        "duplicate extension type",
        ok(b"", region=raw_region([(5, 0, b""), (5, 0, b"x")])),
        [],
        error="extension_duplicate",
    )
    for flags in (0x02, 0x80, 0xFF, 0x03):
        rej(
            f"reserved extension flag bits set ({flags:#04x})",
            ok(b"", region=raw_region([(5, flags, b"")])),
            [],
            error="extension_reserved_flags",
        )
    rej(
        "extension entries one byte over the cap",
        ok(b"", region=raw_region([(1, 0, b"\x00" * (CAP - 6))])),
        [],
        error="extensions_too_large",
    )
    rej(
        "two extensions that together exceed the cap",
        ok(b"", region=raw_region([(1, 0, b"\x01" * 2048), (2, 0, b"\x02" * 2048)])),
        [],
        error="extensions_too_large",
    )
    rej(
        "an oversized extension is refused before the unsorted check",
        ok(b"", region=raw_region([(9, 0, b""), (5, 0, b"\x00" * CAP)])),
        [],
        error="extensions_too_large",
    )
    rej(
        "count says more entries than the message holds",
        ok(b"", region=b"\x00\x02" + struct.pack(">HBI", 1, 0, 0)),
        [],
        error="truncated",
    )

    caps = [
        {
            "name": "rules version 1 allows 4096 bytes of extension entries",
            "input": {"rulesVersion": 1},
            "expected": {"maxExtensionBytes": CAP, "entryHeaderBytes": 7},
        }
    ]

    doc = {
        "$schema": "./SCHEMA.md#envelope",
        "description": (
            "The envelope every structured layout shares (avalon_protocol::signing_bytes): header = tag "
            "(ASCII, no length), layoutVersion u16 BE, rulesVersion u32 BE; then the fields; then the "
            "extensions region: count u16 BE, and per entry ext_type u16 BE, flags u8 (bit 0 = critical, "
            "every other bit must be zero), length u32 BE and the value. Entries are strictly ascending by "
            "ext_type. The entries (7 header bytes plus the value each) total at most the cap for the "
            "message's own rulesVersion (4096 for rules version 1); caps are only ever raised for entries "
            "authored under a newer rules version, so a lower cap never invalidates an older entry. A "
            "layout that contains a hash writes a hash_algo byte (1 = SHA-256) immediately before the first "
            "hash; every key and signature is written with an algorithm byte (1 = Ed25519) in front. "
            "Supported ranges for the reserved conformance tag: layout versions 1..2, rules versions 1..1, "
            "hash algorithm 1, key and signature algorithm 1, no understood critical extension. "
            "buildVectors give input (same field objects as structured-signing-bytes.json plus extensions "
            "[{type, critical, valueHex}]) and expected.signingBytesHex. readVectors must read as "
            "input.read under input.tag with exactly the expected layoutVersion, rulesVersion and "
            "extensions (every unknown non-critical extension kept) and rebuild, from those values and the "
            "fields read, to expected.rebuiltHex. rejectVectors must fail with expected.error: "
            "needs_newer_version (with what = layout | rules | hash_algo | sig_algo | critical_extension "
            "and required = the version, algorithm id or extension type; nothing is verified), "
            "unsupported_version (what and value; a version below the range), extensions_unsorted, "
            "extension_duplicate, extension_reserved_flags, extensions_too_large or truncated. A layout or "
            "rules version above the range is reported before any field is read; the extension checks run "
            "in this order: for each entry as read, its reserved flag bits and then the running size; after "
            "the last entry, ascending order and duplicates; then critical-extension understanding. Read types: str, bytes, u8, u16, u32, u64, "
            "i64, uuid, hash_algo, hash, key, signature. capVectors state the cap per rules version. "
            "Generated by scripts/gen-envelope-vectors.py, an encoder independent of the Rust one."
        ),
        "supportedIn": ["rust"],
        "notSupported": {
            "csharp": "the SDK primitive follows once this layout is merged",
            "typescript": "the SDK primitive follows once this layout is merged",
        },
        "buildVectors": build_in,
        "readVectors": reads,
        "rejectVectors": rj,
        "capVectors": caps,
    }
    return dump(doc)


FILES = {
    "structured-signing-bytes.json": gen_structured,
    "envelope.json": gen_envelope,
}

if __name__ == "__main__":
    stale = []
    for name, fn in FILES.items():
        text = fn()
        path = VECTORS / name
        if len(sys.argv) > 1 and sys.argv[1] == "--check":
            if not path.exists() or path.read_text() != text:
                stale.append(name)
        else:
            path.write_text(text)
    if stale:
        sys.exit("stale: " + ", ".join(stale))
