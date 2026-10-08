#!/usr/bin/env python3
"""Rewrites the signing bytes and signatures of the signed tree head, witness cosignature and
witness announce conformance vectors.

An encoder for the avalon.settlement.sth, avalon.witness.cosign and avalon.witness.announce
layouts and a pure-Python Ed25519 (RFC 8032), sharing no code with the Rust implementation, so
the committed expectations are an independent check of it. Inputs and expected outcomes are kept
from the existing files; only bytes and signatures are recomputed. Run with --check to fail when a
file is stale.

Files written: signed-tree-head.json, self-certifying-tree-head.json,
witness-cosigned-tree-head.json, witness-announce.json, domain-tags.json (the three new tags).
"""
import calendar
import hashlib
import json
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import signing_envelope as env  # noqa: E402

VECTORS = Path(__file__).resolve().parent.parent / "conformance" / "vectors"

TAG_STH = b"avalon.settlement.sth"
TAG_COSIGN = b"avalon.witness.cosign"
TAG_ANNOUNCE = b"avalon.witness.announce"

# ---------------------------------------------------------------- Ed25519 (RFC 8032) ---------

P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
I = pow(2, (P - 1) // 4, P)


def _recover_x(y, sign):
    x2 = (y * y - 1) * pow(D * y * y + 1, P - 2, P) % P
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P != 0:
        x = x * I % P
    if (x * x - x2) % P != 0:
        return None
    if x == 0 and sign:
        return 0  # lenient: the vectors use x = 0 with the sign bit set
    if x & 1 != sign:
        x = P - x
    return x


def _add(p, q):
    a = (p[1] - p[0]) * (q[1] - q[0]) % P
    b = (p[1] + p[0]) * (q[1] + q[0]) % P
    c = 2 * p[3] * q[3] * D % P
    d = 2 * p[2] * q[2] % P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _mul(s, p):
    q = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            q = _add(q, p)
        p = _add(p, p)
        s >>= 1
    return q


def _neg(p):
    return ((-p[0]) % P, p[1], p[2], (-p[3]) % P)


def _eq(p, q):
    return (p[0] * q[2] - q[0] * p[2]) % P == 0 and (p[1] * q[2] - q[1] * p[2]) % P == 0


GY = 4 * pow(5, P - 2, P) % P
GX = _recover_x(GY, 0)
B = (GX, GY, 1, GX * GY % P)
IDENTITY = (0, 1, 1, 0)


def compress(p):
    zi = pow(p[2], P - 2, P)
    x, y = p[0] * zi % P, p[1] * zi % P
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")


def decompress(s):
    y = int.from_bytes(s, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    y %= P  # lenient about non-canonical y: the vectors need R and A as raw bytes only
    x = _recover_x(y, sign)
    return None if x is None else (x, y, 1, x * y % P)


def _h(m):
    return int.from_bytes(hashlib.sha512(m).digest(), "little")


def secret_expand(seed):
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, h[32:]


def public_key(seed):
    return compress(_mul(secret_expand(seed)[0], B))


def sign(seed, msg):
    a, prefix = secret_expand(seed)
    pk = compress(_mul(a, B))
    r = _h(prefix + msg) % L
    rs = compress(_mul(r, B))
    k = _h(rs + pk + msg) % L
    return rs + int.to_bytes((r + k * a) % L, 32, "little")


def order(point):
    q = point
    for n in range(1, 9):
        if _eq(q, IDENTITY):
            return n
        q = _add(q, point)
    raise ValueError("not a small-order point")


# ------------------------------------------------------------------ layouts ------------------


def s(text):
    raw = text.encode("utf-8")
    return struct.pack(">I", len(raw)) + raw


def sth_bytes(tree_size, root_hex, network_id, key_id, created, layout=1, rules=1, algo=1, ext=()):
    return (
        env.header(TAG_STH, layout, rules)
        + struct.pack(">q", tree_size)
        + s(network_id)
        + s(key_id)
        + struct.pack(">q", created)
        + env.hash_algo(algo)
        + bytes.fromhex(root_hex)
        + env.extension_region(ext)
    )


def cosign_bytes(sth, author_sig, witness_key_id, observed, witness_public_key, layout=1, rules=1, algo=1,
                 sig_alg=1, key_alg=1, ext=()):
    return (
        env.header(TAG_COSIGN, layout, rules)
        + struct.pack(">q", sth["treeSize"])
        + s(sth["networkId"])
        + struct.pack(">q", sth["createdAtUnixSeconds"])
        + s(sth["signingKeyId"])
        + env.signature(author_sig, sig_alg)
        + s(witness_key_id)
        + env.key(witness_public_key, key_alg)
        + struct.pack(">q", observed)
        + env.hash_algo(algo)
        + bytes.fromhex(sth["rootHashHex"])
        + env.extension_region(ext)
    )


def announce_bytes(base_url, key_hex, announced, layout=1, rules=1, key_alg=1, ext=()):
    return (
        env.header(TAG_ANNOUNCE, layout, rules)
        + s(base_url)
        + env.key(bytes.fromhex(key_hex), key_alg)
        + struct.pack(">q", announced)
        + env.extension_region(ext)
    )


NEEDS_NEWER = [
    # (name suffix, kwargs for the builder, expected what, required)
    ("a layout version above the supported range", dict(layout=2), "layout", 2),
    ("a rules version above the supported range", dict(rules=2), "rules", 2),
    ("an unknown critical extension", dict(ext=[(0x1234, True, b"")]), "critical_extension", 0x1234),
]


def read_rejects(tag, builder, read, extra):
    """Messages this node cannot read, each with the typed result it must give.

    extra: more (suffix, kwargs, what, required) rows specific to the layout."""
    out = []
    for suffix, kw, what, required in NEEDS_NEWER + extra:
        msg = builder(**kw)
        out.append(
            {
                "name": suffix[0].upper() + suffix[1:],
                "input": {"tag": tag.decode(), "messageHex": msg.hex(), "read": read},
                "expected": {"error": "needs_newer_version", "what": what, "required": required},
            }
        )
    return out


def unix(rfc3339):
    # Whole-second UTC instants only ("2026-09-25T11:50:00Z").
    return calendar.timegm(__import__("time").strptime(rfc3339, "%Y-%m-%dT%H:%M:%SZ"))


def head_bytes(h):
    return sth_bytes(
        int(h["treeSize"]), h["rootHashHex"], h["networkId"], h["signingKeyId"], h["createdAtUnixSeconds"]
    )


def load(name):
    return json.loads((VECTORS / name).read_text())


def dump(doc):
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"


# ------------------------------------------------------------- signed-tree-head.json ---------

STH_KEY_IDS = ["", "k:1", "a,b", "nul\u0000key", "clé-世界"]


def gen_signed_tree_head():
    doc = load("signed-tree-head.json")
    seed = bytes.fromhex(doc["signingKeySeedHex"])
    doc["description"] = (
        "Signed Tree Head signing message (#39/#210/#531, decoupled per #774, structured layout #1226). "
        "A node signs the bytes of the structured signing-bytes layout with tag avalon.settlement.sth "
        "(ASCII, no length prefix), layout version 1 as u16 BE, rules version 1 as u32 BE, tree_size as "
        "i64 BE, network_id and signing_key_id as u32-BE length plus UTF-8, created_at as i64 BE unix "
        "seconds, the hash algorithm byte 01 (the hash of the Merkle tree the root belongs to: SHA-256 "
        "leaf and interior hashing and empty root), the root hash as 32 raw bytes, and the extensions "
        "region (00 00 when empty). rootHashHex must be exactly 64 lowercase hex characters: any other root hash cannot be "
        "signed or verified (rejectedVectors). A client verifies a network's claimed identity by "
        "checking this signature against a pinned trust anchor, and a managed-hosting integrator signs a "
        "prepared tree head locally with a key the host never holds, so a client-side reimplementation "
        "must match the node's bytes exactly. Generated by scripts/gen-sth-witness-vectors.py."
    )
    keep = doc["vectors"][:2]
    base = keep[0]["input"]
    vectors = []
    for v in keep:
        inp = dict(v["input"], signingKeyId="settlement-operator-1")
        vectors.append({"name": v["name"], "input": inp})
    extra = [
        ("negative tree size and a pre-1970 timestamp", dict(treeSize=-1, createdAtUnixSeconds=-1, createdAtRfc3339="1969-12-31T23:59:59Z")),
        ("tree size i64 max", dict(treeSize=9223372036854775807)),
        ("tree size i64 min", dict(treeSize=-9223372036854775808)),
    ]
    for name, patch in extra:
        inp = dict(base, signingKeyId="settlement-operator-1")
        inp.update(patch)
        vectors.append({"name": name, "input": inp})
    for key_id in STH_KEY_IDS:
        inp = dict(base, signingKeyId=key_id)
        vectors.append({"name": f"signing key id {key_id!r} is covered by the signature", "input": inp})
    for item in vectors:
        inp = item["input"]
        msg = head_bytes(inp)
        item["expected"] = {"signingBytesHex": msg.hex(), "signatureHex": sign(seed, msg).hex()}
        # JSON numbers above 2^53 are not portable, so those tree sizes are decimal strings.
        if abs(inp["treeSize"]) >= 2**53:
            inp["treeSize"] = str(inp["treeSize"])
    doc["vectors"] = vectors
    good = base["rootHashHex"]
    bad = {"error": "invalid_root_hash"}
    doc["rejectedVectors"] = [
        {"name": "root hash in uppercase hex", "input": dict(base, rootHashHex=good.upper(), signingKeyId="settlement-operator-1"), "expected": bad},
        {"name": "root hash one byte short", "input": dict(base, rootHashHex=good[:-2], signingKeyId="settlement-operator-1"), "expected": bad},
        {"name": "root hash one byte long", "input": dict(base, rootHashHex=good + "ab", signingKeyId="settlement-operator-1"), "expected": bad},
        {"name": "root hash is not hex", "input": dict(base, rootHashHex="zz" * 32, signingKeyId="settlement-operator-1"), "expected": bad},
        {"name": "root hash is empty", "input": dict(base, rootHashHex="", signingKeyId="settlement-operator-1"), "expected": bad},
    ]

    def head_msg(**kw):
        return sth_bytes(
            int(base["treeSize"]), good, base["networkId"], "settlement-operator-1", base["createdAtUnixSeconds"], **kw
        )

    doc["readRejectVectors"] = read_rejects(
        TAG_STH,
        head_msg,
        ["i64", "str", "str", "i64", "hash_algo", "hash"],
        [
            ("an unknown hash algorithm 9", dict(algo=9), "hash_algo", 9),
            ("hash algorithm 0 is not a hash", dict(algo=0), "hash_algo", 0),
        ],
    )
    doc["description"] += (
        " readRejectVectors are messages this node cannot read: input.messageHex read as input.read "
        "(field types in order) under input.tag must fail with exactly expected (error needs_newer_version, "
        "what and required), and nothing is verified. rejectedVectors give expected.error invalid_root_hash."
    )
    return dump(doc)


# ------------------------------------------------------ self-certifying-tree-head.json -------


def gen_self_certifying():
    doc = load("self-certifying-tree-head.json")
    seed1 = bytes.fromhex(doc["signingKeySeedHex"])
    seed2 = bytes.fromhex(doc["otherKeySeedHex"])
    a1 = secret_expand(seed1)[0]
    pk1 = public_key(seed1)
    assert pk1.hex() == doc["signingPublicKeyHex"]
    vectors = doc["vectors"][:79]
    base = vectors[0]["input"]["head"]
    base_msg = head_bytes(base)
    s0 = sign(seed1, base_msg)
    r0, sc0 = s0[:32], int.from_bytes(s0[32:], "little")

    def honest(h, seed=seed1):
        return sign(seed, head_bytes(h)).hex()

    def k_of(r, a_bytes, msg):
        return _h(r + a_bytes + msg) % L

    def forge(a_bytes, msg):
        point = decompress(a_bytes)
        n = order(point)
        for sc in range(0, 1000):
            rb = compress(_mul(sc, B))
            if k_of(rb, a_bytes, msg) % n == 0:
                return rb + int.to_bytes(sc, 32, "little")
        raise ValueError("no forgery found")

    extremes = {"i64 max": 9223372036854775807, "i64 min": -9223372036854775808}

    def signature_for(name, head, key_hex):
        own = dict(head, treeSize=int(head["treeSize"]))
        if "with a signature that verifies under it" in name:
            return forge(bytes.fromhex(key_hex), base_msg).hex()
        if name in ("key does not hash to the id", "right key hash but head signed by a different key"):
            return honest(dict(base, treeSize=int(base["treeSize"])), seed2)
        if name.startswith("tree size") and "adjacent" in name:
            which = "i64 max" if "max" in name else "i64 min"
            return honest(dict(own, treeSize=extremes[which]))
        if name.startswith(("tree size", "network id with non-ASCII")):
            return honest(own)
        if name.startswith(("created_at before 1970", "created_at exactly")):
            return honest(own)
        if name in ("valid head, matching key and id", "valid head on another network and a larger tree",
                    "key does not hash to the id, head signed by the id's key"):
            return honest(own)
        if name in ("signature is not valid hex", "signature has the wrong length"):
            return head["signatureHex"]
        if name == "signature S is the valid S plus L":
            return (r0 + int.to_bytes(sc0 + L, 32, "little")).hex()
        if name == "signature S equals L":
            return (r0 + int.to_bytes(L, 32, "little")).hex()
        if name == "signature S has bit 255 set":
            return (r0 + int.to_bytes(sc0 | (1 << 255), 32, "little")).hex()
        if name == "signature S is 2^256 - 1":
            return (r0 + b"\xff" * 32).hex()
        if name.startswith(("R is the identity encoded non-canonically", "R is an order-4 point encoded", "R is the identity point")):
            r = bytes.fromhex(head["signatureHex"])[:32]
            return (r + int.to_bytes(k_of(r, pk1, base_msg) * a1 % L, 32, "little")).hex()
        if name.startswith("R carries an order-"):
            n = int(name.split("order-")[1].split()[0])
            r = bytes.fromhex(head["signatureHex"])[:32]
            rr = _h_label(f"cofactor-tainted-order-{n}") % L
            assert order(_add(decompress(r), _neg(_mul(rr, B)))) == n, name
            return (r + int.to_bytes((rr + k_of(r, pk1, base_msg) * a1) % L, 32, "little")).hex()
        text = s0.hex()
        if name == "signature hex in uppercase":
            return text.upper()
        if name == "signature hex in mixed case":
            return "".join(c.upper() if i % 3 == 0 else c for i, c in enumerate(text))
        if name == "signature hex with an odd number of digits":
            return text[:-1]
        if name == "signature hex with leading space":
            return " " + text[:-1]
        if name == "signature hex with trailing newline":
            return text + "\n"
        if name == "signature hex with a 0x prefix":
            return "0x" + text[2:]
        if name.startswith("signature hex with a plus sign") or name.startswith("signature hex with a minus sign"):
            i = text.index("0")
            return text[:i] + ("+" if "plus" in name else "-") + text[i + 1 :]
        # Everything else uses the honest signature of the base head: tampered fields, key and id
        # cases, created_at forms of the signed instant, and a root hash that is not 32-byte hex.
        return text

    out = []
    for v in vectors[:79]:
        name = v["name"]
        head = v["input"].get("head")
        if head:
            if name.startswith("root hash with non-ASCII"):
                name = "root hash with non-ASCII characters cannot be signed or verified"
            sig = signature_for(name, head, v["input"].get("signingPublicKeyHex"))
            v = {
                "name": name,
                "input": dict(v["input"], head=dict(head, signatureHex=sig)),
                "expected": v["expected"],
            }
            if name.startswith("root hash with non-ASCII"):
                v["expected"] = {"check": "self_certifying", "verified": False, "failure": "bad_signature"}
        out.append(v)

    base_in = vectors[0]["input"]

    def extra(name, patch, sig_head=None, sig_seed=seed1, raw_sig=None, expected=None):
        h = dict(base_in["head"], **patch)
        if raw_sig is None:
            ref = dict(h if sig_head is None else sig_head)
            ref["treeSize"] = int(ref["treeSize"])
            h["signatureHex"] = sign(sig_seed, head_bytes(ref)).hex()
        else:
            h["signatureHex"] = raw_sig
        out.append(
            {
                "name": name,
                "input": dict(base_in, head=h),
                "expected": expected or {"check": "self_certifying", "verified": True, "failure": None},
            }
        )

    bad = {"check": "self_certifying", "verified": False, "failure": "bad_signature"}
    extra("tampered signing_key_id", {"signingKeyId": "node-key-2"}, sig_head=base, raw_sig=s0.hex(), expected=bad)
    extra("signing key id is empty", {"signingKeyId": ""})
    extra("signing key id with a colon, a comma and a NUL", {"signingKeyId": "a:b,c\u0000d"})
    extra("signing key id with non-ASCII characters is signed as UTF-8", {"signingKeyId": "clé-世界"})
    extra("root hash in uppercase hex is rejected", {"rootHashHex": base["rootHashHex"].upper()}, raw_sig=s0.hex(), expected=bad)
    extra("root hash one byte short is rejected", {"rootHashHex": base["rootHashHex"][:-2]}, raw_sig=s0.hex(), expected=bad)
    extra("root hash one byte long is rejected", {"rootHashHex": base["rootHashHex"] + "ab"}, raw_sig=s0.hex(), expected=bad)

    doc["vectors"] = out
    doc["generation"] = _generation_text()
    return dump(doc)


def _h_label(label):
    return int.from_bytes(hashlib.sha512(label.encode()).digest(), "little")


def _generation_text():
    return (
        "Deterministic. signingKeySeedHex and otherKeySeedHex are the Ed25519 seeds of the two keys "
        "(signingPublicKeyHex / otherPublicKeyHex, ids selfCertifyingId / otherSelfCertifyingId). Each "
        "head's signatureHex is Ed25519 over the Signed Tree Head signing bytes (see signed-tree-head.json: "
        "tag avalon.settlement.sth, layout version 1, rules version 1, i64 tree size, network id and signing key "
        "id as u32-BE length plus UTF-8, i64 unix seconds, hash algorithm byte 01, 32 raw root bytes, empty "
        "extensions); 'signed by a different key' vectors use the other "
        "seed, and tampered vectors reuse the original signature with one field changed. The non-curve key is "
        "0x01 0x7f followed by zeros, which fails point decompression. Never real credentials. Everything is "
        "reproducible from the seeds and the rules here by scripts/gen-sth-witness-vectors.py (a pure-Python "
        "Ed25519 implementation, independent of avalon_protocol; every outcome is asserted against "
        "avalon_protocol). Signatures with a modified S keep the honest R and use S+L, S=L, S+2^255 and "
        "2^256-1. 'R is the identity encoded non-canonically' uses R bytes of y=p+1 (0xee, then 30 bytes of "
        "0xff, then 0x7f) and s = k*a mod L where a is the clamped secret scalar and k = SHA-512(R||A||M) "
        "mod L, so it satisfies the cofactored equation and fails the cofactorless byte comparison; the "
        "order-4 variant does the same with y=p. 'R is the identity point' uses the canonical identity as R "
        "(r=0) and s = k*a mod L, which verifies cofactorless. The three 'R carries a ... component' cases "
        "use r = SHA-512('cofactor-tainted-order-N') mod L (little-endian), R = rB + T with T of order 2 "
        "(y=p-1), 4 (y=0) or 8 (L times a curve point found by trying y=2,3,...), and s = r + k*a mod L: "
        "valid under cofactored verification, invalid under cofactorless. Signature-hex cases edit the "
        "honest signature hex: upper case and mixed case (accepted), odd length, a leading space, a "
        "trailing newline, a 0x prefix, and the first '0' digit replaced by '+' or '-'. Key cases use the "
        "identity (0x01 then zeros), the order-2 point (0xec, 30 bytes of 0xff, 0x7f), all eight canonical "
        "small-order points (the identity, the order-2 point, the two order-4 points and four order-8 "
        "points; the order-4 and order-8 points are the multiples of an order-8 point found as L times a "
        "curve point), the identity and the order-2 point with the sign bit (0x80 in the last byte) set, the "
        "identity as y=p+1, and the y=3 point (valid, not small order) as y+p; each key's id is 'node:' plus "
        "the SHA-256 of its bytes, and the signatures for the identity, order-2, order-4 and order-8 keys are "
        "forgeries that verify cofactorless under those keys (identity: R=identity, s=0; the others: R=[s]B "
        "for the smallest s where it verifies). Id and key cases append a newline or a NUL to the valid id or "
        "key hex, prepend a newline to the key, or replace the first key character with '+'. treeSize is a "
        "JSON number when its magnitude is below 2^53 and a decimal string otherwise (the i64 extremes and "
        "2^53+1), each signed over that exact i64; the 'adjacent value' cases reuse the signature with the "
        "tree size one closer to zero. A root hash must be exactly 64 lowercase hex characters (it is signed "
        "as 32 raw bytes), so a root hash with non-ASCII characters, in uppercase, or of another length "
        "never verifies, and each such case reuses an honest signature to show the rejection comes from the "
        "root hash. The signing key id is covered: the tampered key id reuses the honest signature, and "
        "the key id cases (empty, with ':', ',' and NUL, non-ASCII) sign that exact id. created_at cases "
        "sign the floor of the instant to whole seconds: before 1970 (-1), the epoch (0), a fractional "
        "second, a nine-digit fraction, a pre-1970 fractional second (floors to -1), a +02:00 offset, and one "
        "second later than signed (rejected). createdAtUnixSeconds always equals the floor of createdAtRfc3339."
    )


# -------------------------------------------------- witness-cosigned-tree-head.json ---------


def gen_cosigned():
    doc = load("witness-cosigned-tree-head.json")
    author = bytes.fromhex(doc["authorSigningKeySeedHex"])
    seeds = {k: bytes.fromhex(v) for k, v in doc["witnessSigningKeySeedsHex"].items()}
    network = doc["networkId"]
    key_id = "settlement-operator-1"
    doc["description"] = (
        "Witness-cosigned tree head acceptance (#932, wiring the #934 design-spike primitives; "
        "avalon_protocol::witness::WitnessCosignature/witness_signing_message and "
        "avalon_protocol::cosigned_sth::verify_cosigned_tree_head). A cosigned head is accepted only when "
        "the author's own SignedTreeHead verifies AND at least witness::majority_threshold(knownList.length) "
        "of its cosignatures are individually signature-valid against a key in the verifier's own known list "
        "and fresh (observedAt within [freshnessCutoff, now]). A cosignature signs, with tag "
        "avalon.witness.cosign, layout version 1 (u16 BE) and rules version 1 (u32 BE): tree_size as i64 BE, "
        "network_id as u32-BE length plus UTF-8, the author's created_at as i64 BE unix seconds, the "
        "author's signing_key_id as u32-BE length plus UTF-8, the author's signature (algorithm byte 01 then "
        "64 raw bytes), the witness key id as u32-BE length plus UTF-8, the witness's verifying key "
        "(algorithm byte 01 then 32 raw bytes, derived from the witness seed), observed_at as i64 BE unix "
        "seconds, the hash algorithm byte 01, the root hash as 32 raw bytes and the empty extensions region. A cosignature only counts for "
        "the head whose tree size, root hash, network id, created_at, signing key id and signature all match "
        "what it signed (the sth's signingKeyId is settlement-operator-1 throughout). Each vector's "
        "sth/cosignatures carry precomputed signatures (same fixed seeds throughout the file) so a "
        "reimplementation is checked against real signature bytes, not just its own round trip. "
        "Generated by scripts/gen-sth-witness-vectors.py."
    )

    def author_sth(h):
        h["signingKeyId"] = key_id
        h["signatureHex"] = sign(author, head_bytes(dict(h, networkId=network))).hex()
        return h

    def fix_head(head):
        sth = author_sth(head["sth"])
        sig = bytes.fromhex(sth["signatureHex"])
        full = dict(sth, networkId=network)
        for c in head.get("cosignatures", []):
            m = cosign_bytes(full, sig, c["witnessKeyId"], c["observedAtUnixSeconds"], public_key(seeds[c["witnessKeyId"]]))
            c["signingBytesHex"] = m.hex()
            c["signatureHex"] = sign(seeds[c["witnessKeyId"]], m).hex()

    doc["vectors"] = doc["vectors"][:7]
    for v in doc["vectors"]:
        inp = v["input"]
        if "sth" in inp:
            fix_head(inp)
        else:
            fix_head(inp["headA"])
            fix_head(inp["headB"])
    first = doc["vectors"][0]["input"]
    template_sth = dict(first["sth"])
    sig = bytes.fromhex(template_sth["signatureHex"])
    full = dict(template_sth, networkId=network)

    def variant(name, cosigs):
        inp = {
            "sth": dict(template_sth),
            "cosignatures": cosigs,
            "knownList": ["witness-1", "witness-2", "witness-3"],
            "freshnessCutoffUnixSeconds": first["freshnessCutoffUnixSeconds"],
            "nowUnixSeconds": first["nowUnixSeconds"],
        }
        doc["vectors"].append({"name": name, "input": inp, "expected": {"accepted": False}})

    good = dict(first["cosignatures"][0])
    good2 = dict(first["cosignatures"][1])
    # witness-2 signs the same tuple but over another author key id, or another author signature.
    other_id = dict(full, signingKeyId="rotated-operator-key")
    m = cosign_bytes(other_id, sig, "witness-2", 1790000600, public_key(seeds["witness-2"]))
    variant(
        "cosignature over another author signing key id does not count",
        [good, {"witnessKeyId": "witness-2", "observedAtUnixSeconds": 1790000600, "signatureHex": sign(seeds["witness-2"], m).hex()}],
    )
    other_sig = sign(author, head_bytes(dict(full, signingKeyId="rotated-operator-key")))
    m = cosign_bytes(full, other_sig, "witness-2", 1790000600, public_key(seeds["witness-2"]))
    variant(
        "cosignature over another author signature on the same tuple does not count",
        [good, {"witnessKeyId": "witness-2", "observedAtUnixSeconds": 1790000600, "signatureHex": sign(seeds["witness-2"], m).hex()}],
    )

    def unreadable(witness, name_kw, label):
        """A cosignature signed under an envelope this node cannot read."""
        envelope = {"layoutVersion": 1, "rulesVersion": 1, "hashAlgo": 1, "extensions": "0000"}
        kw = {}
        if "rules" in name_kw:
            envelope["rulesVersion"] = name_kw["rules"]
            kw["rules"] = name_kw["rules"]
        if "algo" in name_kw:
            envelope["hashAlgo"] = name_kw["algo"]
            kw["algo"] = name_kw["algo"]
        if "ext" in name_kw:
            envelope["extensions"] = env.extension_region(name_kw["ext"]).hex()
            kw["ext"] = name_kw["ext"]
        m = cosign_bytes(full, sig, witness, 1790000600, public_key(seeds[witness]), **kw)
        return {
            "witnessKeyId": witness,
            "observedAtUnixSeconds": 1790000600,
            "envelope": envelope,
            "signingBytesHex": m.hex(),
            "signatureHex": sign(seeds[witness], m).hex(),
        }

    def accepting(name, cosigs, accepted):
        inp = {
            "sth": dict(template_sth),
            "cosignatures": cosigs,
            "knownList": ["witness-1", "witness-2", "witness-3"],
            "freshnessCutoffUnixSeconds": first["freshnessCutoffUnixSeconds"],
            "nowUnixSeconds": first["nowUnixSeconds"],
        }
        doc["vectors"].append({"name": name, "input": inp, "expected": {"accepted": accepted}})

    for label, kw in [
        ("a rules version above the supported range", {"rules": 2}),
        ("an unknown hash algorithm (a different one from the head's)", {"algo": 9}),
        ("an unknown critical extension", {"ext": [(0x1234, True, b"")]}),
    ]:
        bad = unreadable("witness-3", kw, label)
        accepting(
            f"a cosignature with {label} is not counted and does not void a head the others carry",
            [good, good2, bad],
            True,
        )
        accepting(
            f"a cosignature with {label} is not counted toward the majority",
            [good, bad],
            False,
        )
    doc["description"] += (
        " A cosignature object may carry envelope {layoutVersion, rulesVersion, hashAlgo, extensions "
        "(hex)}, the values it was signed under: one this node cannot read (a version above its range, a "
        "hash algorithm other than the head's or unknown, a critical extension it does not understand) is "
        "dropped and never counted, and does not void the head. Each cosignature also carries "
        "signingBytesHex, the exact bytes it signs. readRejectVectors are cosignature messages this node "
        "cannot read: input.messageHex read as input.read under input.tag must fail with exactly expected."
    )
    doc["readRejectVectors"] = read_rejects(
        TAG_COSIGN,
        lambda **kw: cosign_bytes(full, sig, "witness-1", 1790000600, public_key(seeds["witness-1"]), **kw),
        ["i64", "str", "i64", "str", "signature", "str", "key", "i64", "hash_algo", "hash"],
        [
            ("an unknown hash algorithm 9", dict(algo=9), "hash_algo", 9),
            ("an unknown author signature algorithm 2", dict(sig_alg=2), "sig_algo", 2),
            ("an unknown witness key algorithm 2", dict(key_alg=2), "sig_algo", 2),
        ],
    )
    return dump(doc)


# ---------------------------------------------------------- witness-announce.json ------------


def gen_announce():
    doc = load("witness-announce.json")
    seeds = {k: bytes.fromhex(v) for k, v in doc["witnessSigningKeySeedsHex"].items()}
    keys = doc["witnessVerifyingKeysHex"]
    k1, k2 = keys["witness-1"], keys["witness-2"]
    s1 = seeds["witness-1"]
    url = "http://192.168.7.174:8080"
    doc["description"] = (
        "Witness announce proof verification (avalon_protocol::witness::verify_witness_announce). A witness "
        "advertises a base url together with a proof: an Ed25519 signature by the advertised key over the "
        "structured layout with tag avalon.witness.announce (ASCII, no length prefix), layout version 1 as "
        "u16 BE, rules version 1 as u32 BE, the base url as u32-BE length plus UTF-8 bytes (the audience "
        "stays the URL text; a node has no protocol-level id to bind yet), the witness key (algorithm byte "
        "01 then 32 raw bytes), announced_at as unix seconds big-endian i64 and the empty extensions region. A proof is accepted only when the key id is exactly 64 lowercase hex "
        "characters (a 32-byte Ed25519 key), the proof is a 64-byte hex signature that verifies under that "
        "key, and announced_at is within one hour (inclusive, either direction) of the verifier's clock. "
        "messageHex is the exact signed message for the input's base url, key id and announcedAt, present "
        "only where the key id is a valid 64-character lowercase hex key; signatures come from the same fixed "
        "seeds as witness-cosigned-tree-head.json (witness-1 = 22..22, witness-2 = 33..33). Generated by "
        "scripts/gen-sth-witness-vectors.py."
    )

    def proof(seed, u, key_hex, at):
        return sign(seed, announce_bytes(u, key_hex, unix(at))).hex()

    out = []
    for v in doc["vectors"][:13]:
        i = dict(v["input"])
        name = v["name"]
        at = i["announcedAt"]
        key_hex = i["witnessKeyId"]
        valid_key = len(key_hex) == 64 and all(c in "0123456789abcdef" for c in key_hex)
        honest_at = "2026-09-25T11:59:00Z"
        if name.startswith("accepted") or name.startswith("rejected: announced one hour"):
            i["proofHex"] = proof(s1, url, k1, at)
        elif name == "rejected: proof was made for a different base url":
            i["proofHex"] = proof(s1, "http://192.168.7.175:8080", k1, at)
        elif name == "rejected: proof made by one key while advertising another key id":
            i["proofHex"] = proof(s1, url, k1, at)
        elif name == "rejected: signature bytes altered":
            p = proof(s1, url, k1, at)
            i["proofHex"] = "00" + p[2:]
        elif name == "rejected: proof is 63 bytes":
            i["proofHex"] = proof(s1, url, k1, honest_at)[:-2]
        elif name == "rejected: proof is not hex":
            pass
        else:  # key id not hex, 31 bytes, announced_at differs: proof of the honest 11:59:00 announce
            i["proofHex"] = proof(s1, url, k1, honest_at)
        if valid_key:
            i["messageHex"] = announce_bytes(i["baseUrl"], key_hex, unix(at)).hex()
        else:
            i.pop("messageHex", None)
        out.append({"name": name, "input": i, "expected": v["expected"]})

    def add(name, patch, accepted, seed=s1, signed_url=None, signed_key=k1):
        i = {"announcedAt": "2026-09-25T11:59:00Z", "baseUrl": url, "now": "2026-09-25T12:00:00Z", "witnessKeyId": k1}
        i.update(patch)
        u = signed_url if signed_url is not None else i["baseUrl"]
        i["proofHex"] = proof(seed, u, signed_key, i["announcedAt"])
        if len(i["witnessKeyId"]) == 64 and i["witnessKeyId"] == i["witnessKeyId"].lower():
            i["messageHex"] = announce_bytes(i["baseUrl"], i["witnessKeyId"], unix(i["announcedAt"])).hex()
        out.append({"name": name, "input": i, "expected": {"accepted": accepted}})

    add("accepted: base url with non-ASCII characters is length-prefixed in UTF-8 bytes", {"baseUrl": "http://exämple.世界:8080/a:b,c"}, True)
    add("accepted: empty base url", {"baseUrl": ""}, True)
    add("rejected: key id in uppercase hex", {"witnessKeyId": k1.upper()}, False)
    doc["vectors"] = out
    doc["readRejectVectors"] = read_rejects(
        TAG_ANNOUNCE,
        lambda **kw: announce_bytes(url, k1, unix("2026-09-25T11:59:00Z"), **kw),
        ["str", "key", "i64"],
        [("an unknown witness key algorithm 2", dict(key_alg=2), "sig_algo", 2)],
    )
    doc["description"] += (
        " readRejectVectors are announce messages this node cannot read: input.messageHex read as "
        "input.read under input.tag must fail with exactly expected (needs_newer_version, what, required)."
    )
    return dump(doc)


# ------------------------------------------------------------------ domain-tags.json ---------


def gen_domain_tags():
    doc = load("domain-tags.json")
    have = {t["kind"] for t in doc["tags"]}
    new = [
        ("identity_chain_signature", "avalon.identity.chain_signature"),
        ("settlement_sth", "avalon.settlement.sth"),
        ("witness_cosign", "avalon.witness.cosign"),
        ("witness_announce", "avalon.witness.announce"),
    ]
    tags = doc["tags"]
    conf = [t for t in tags if t["kind"] == "conformance"]
    rest = [t for t in tags if t["kind"] != "conformance"]
    for kind, tag in new:
        if kind not in have:
            rest.append({"kind": kind, "tag": tag})
    doc["tags"] = rest + conf
    return dump(doc)


FILES = {
    "signed-tree-head.json": gen_signed_tree_head,
    "self-certifying-tree-head.json": gen_self_certifying,
    "witness-cosigned-tree-head.json": gen_cosigned,
    "witness-announce.json": gen_announce,
    "domain-tags.json": gen_domain_tags,
}

if __name__ == "__main__":
    stale = []
    for name, fn in FILES.items():
        text = fn()
        path = VECTORS / name
        if len(sys.argv) > 1 and sys.argv[1] == "--check":
            if path.read_text() != text:
                stale.append(name)
        else:
            path.write_text(text)
    if stale:
        sys.exit("stale: " + ", ".join(stale))
