"""Header, algorithm tags and extensions region of the structured signing-bytes layouts.

Written from the layout rules in conformance/vectors/SCHEMA.md, sharing no code with the Rust
implementation, so every generator that imports it is an independent check of it.
"""
import struct

RULES_VERSION = 1
HASH_SHA256 = 1
SIG_ED25519 = 1
# Cap for rules version 1 (extension entries, each counting its 7 header bytes).
MAX_EXTENSION_BYTES_V1 = 4096


def header(tag, layout_version=1, rules_version=RULES_VERSION):
    return tag + struct.pack(">H", layout_version) + struct.pack(">I", rules_version)


def hash_algo(algo=HASH_SHA256):
    return bytes([algo])


def key(raw32, algo=SIG_ED25519):
    assert len(raw32) == 32
    return bytes([algo]) + raw32


def signature(raw64, algo=SIG_ED25519):
    assert len(raw64) == 64
    return bytes([algo]) + raw64


def extension_region(entries=()):
    """entries: (ext_type, critical, value_bytes) already in strictly ascending ext_type order."""
    out = struct.pack(">H", len(entries))
    last = -1
    for ext_type, critical, value in entries:
        assert ext_type > last, "extensions must be strictly ascending"
        last = ext_type
        out += struct.pack(">HBI", ext_type, 1 if critical else 0, len(value)) + value
    return out
