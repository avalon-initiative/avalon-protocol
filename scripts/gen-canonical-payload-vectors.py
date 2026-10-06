#!/usr/bin/env python3
"""Writes conformance/vectors/canonical-payload.json.

An implementation of the restricted RFC 8785 encoding that shares no code with
the Rust one, so the committed expectations are an independent check of it.
"""
import json
import sys
from decimal import Decimal
from pathlib import Path

MAX_SAFE = 2**53


class Rejected(Exception):
    def __init__(self, code):
        self.code = code


class Num:
    def __init__(self, text, integer_form):
        self.text, self.integer_form = text, integer_form


def es6_float(f):
    """ECMAScript Number::toString of a finite non-zero float, plus its digit count."""
    sign, digits, exp = Decimal(repr(abs(f))).as_tuple()
    digits = list(digits)
    while len(digits) > 1 and digits[-1] == 0:
        digits.pop()
        exp += 1
    k, n = len(digits), len(digits) + exp
    ds = "".join(map(str, digits))
    if k <= n <= 21:
        body = ds + "0" * (n - k)
    elif 0 < n <= 21:
        body = ds[:n] + "." + ds[n:]
    elif -6 < n <= 0:
        body = "0." + "0" * -n + ds
    else:
        e = n - 1
        body = ds[0] + ("." + ds[1:] if k > 1 else "") + "e" + ("+" if e >= 0 else "-") + str(abs(e))
    return ("-" if f < 0 else "") + body, k


def number_text(num):
    if num.integer_form:
        if num.text == "-0":
            raise Rejected("invalid_number")
        if abs(int(num.text)) > MAX_SAFE:
            raise Rejected("invalid_number")
        return num.text
    f = float(num.text)
    if f in (float("inf"), float("-inf")):
        raise Rejected("invalid_number")
    if f == 0.0:
        raise Rejected("invalid_number")  # any non-integer-form zero changes text
    text, k = es6_float(f)
    if f.is_integer() and abs(f) > MAX_SAFE:
        raise Rejected("invalid_number")
    if not f.is_integer() and k > 15:
        raise Rejected("invalid_number")
    if text != num.text:
        raise Rejected("invalid_number")
    return text


def pairs(items):
    obj = {}
    for k, v in items:
        if k in obj:
            raise Rejected("duplicate_key")
        obj[k] = v
    return obj


def utf16_key(s):
    b = s.encode("utf-16-be")
    return [int.from_bytes(b[i:i + 2], "big") for i in range(0, len(b), 2)]


def esc(s):
    out = ['"']
    for ch in s:
        o = ord(ch)
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif ch in "\b\t\n\f\r":
            out.append({"\b": "\\b", "\t": "\\t", "\n": "\\n", "\f": "\\f", "\r": "\\r"}[ch])
        elif o < 0x20:
            out.append("\\u%04x" % o)
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def enc(v):
    if v is None:
        return "null"
    if v is True:
        return "true"
    if v is False:
        return "false"
    if isinstance(v, Num):
        return number_text(v)
    if isinstance(v, str):
        return esc(v)
    if isinstance(v, list):
        return "[" + ",".join(enc(x) for x in v) + "]"
    keys = sorted(v, key=utf16_key)
    return "{" + ",".join(esc(k) + ":" + enc(v[k]) for k in keys) + "}"


def has_surrogate(v):
    if isinstance(v, str):
        return any(0xD800 <= ord(c) <= 0xDFFF for c in v)
    if isinstance(v, list):
        return any(has_surrogate(x) for x in v)
    if isinstance(v, dict):
        return any(has_surrogate(k) or has_surrogate(x) for k, x in v.items())
    return False


def has_nul(v):
    if isinstance(v, str):
        return "\x00" in v
    if isinstance(v, list):
        return any(has_nul(x) for x in v)
    if isinstance(v, dict):
        return any(has_nul(k) or has_nul(x) for k, x in v.items())
    return False


def canonicalize(text):
    def bad_constant(_):
        raise Rejected("malformed")

    try:
        v = json.loads(
            text,
            object_pairs_hook=pairs,
            parse_int=lambda t: Num(t, True),
            parse_float=lambda t: Num(t, False),
            parse_constant=bad_constant,
        )
    except Rejected:
        raise
    except ValueError:
        raise Rejected("malformed")
    if has_surrogate(v):
        raise Rejected("malformed")
    if has_nul(v):
        raise Rejected("nul_character")
    return enc(v)


CASES = [
    # key order
    ("keys sort ascending", '{"b":1,"a":2,"c":3}'),
    ("keys sort by code unit, uppercase before lowercase", '{"a":1,"B":2,"A":3,"b":4}'),
    ("empty key sorts first", '{"a":1,"":2}'),
    ("non-BMP key sorts before U+E000 (UTF-16 order, not code point order)", '{"":1,"\U00010000":2}'),
    ("non-BMP key sorts before U+FFFF", '{"￿":1,"\U0001f600":2}'),
    ("non-BMP keys order by surrogates", '{"\U0001f601":1,"\U0001f600":2,"\U00010000":3}'),
    ("BMP above surrogates sorts after non-BMP", '{"퟿":1,"":2,"\U00010000":3,"～":4}'),
    ("keys given as escapes are decoded before sorting", '{"\\ud83d\\ude00":1,"\\uffff":2,"a":3}'),
    ("key prefix sorts first", '{"ab":1,"a":2,"abc":3}'),
    ("nested objects sort independently", '{"z":{"y":1,"x":2},"a":{"d":[{"b":1,"a":2}],"c":3}}'),
    ("array order is preserved", '[3,1,2,{"b":1,"a":2}]'),
    # strings
    ("short escapes", '["\\b\\t\\n\\f\\r\\"\\\\"]'),
    ("other control characters use lowercase hex", '["\\u0001\\u001f\\u001F"]'),
    ("slash escape is decoded and not re-escaped", '["a\\/b"]'),
    ("DEL and U+2028 are literal", '["\\u007f\\u2028\\u2029"]'),
    ("unicode escapes are decoded to the character", '["\\u0041\\u00e9\\u20ac"]'),
    ("surrogate pair escape becomes one non-BMP character", '["\\ud83d\\ude00"]'),
    ("literal non-BMP character is kept", '["\U0001f600"]'),
    ("composed and decomposed forms stay distinct", '["é","é"]'),
    ("empty string", '[""]'),
    ("key with escapes is re-escaped canonically", '{"a\\u0001\\"b":1}'),
    ("U+0001 and U+001F are valid", '["\\u0001\\u001f"]'),
    ("noncharacters and C1 controls are valid", '["\\ufffe\\uffff\\u0080\\u009f"]'),
    # U+0000
    ("escaped NUL in a string value", '["\\u0000"]'),
    ("escaped NUL inside a longer string value", '["a\\u0000b"]'),
    ("escaped NUL in an object value", '{"a":"\\u0000"}'),
    ("escaped NUL in an object key", '{"\\u0000":1}'),
    ("escaped NUL inside a longer object key", '{"a\\u0000b":1}'),
    ("escaped NUL in a nested key", '{"a":[{"b":{"\\u0000":null}}]}'),
    ("escaped NUL in a nested value", '{"a":[{"b":["x\\u0000"]}]}'),
    ("escaped NUL at top level", '"\\u0000"'),
    ("raw NUL in a string value", '["a\x00b"]'),
    ("raw NUL in an object key", '{"a\x00":1}'),
    ("raw NUL between tokens", '[1,\x002]'),
    ("escaped backslash followed by u0000 text is not a NUL", '["\\\\u0000"]'),
    # numbers
    ("zero", "[0]"),
    ("small integers", "[1,-1,10,100]"),
    ("integer at the upper bound", "[9007199254740992]"),
    ("integer at the lower bound", "[-9007199254740992]"),
    ("integer just below the bounds", "[9007199254740991,-9007199254740991]"),
    ("integer just above the upper bound", "[9007199254740993]"),
    ("integer just above the upper bound, exactly representable", "[9007199254740994]"),
    ("integer just below the lower bound", "[-9007199254740993]"),
    ("integer far beyond 64 bits", "[123456789012345678901234567890]"),
    ("decimal fraction", "[0.5,-0.5,0.1,4.35]"),
    ("fraction with 15 significant digits", "[0.123456789012345]"),
    ("fraction with 16 significant digits", "[0.1234567890123456]"),
    ("fraction with 15 digits and a large integer part", "[12345678901234.5]"),
    ("fraction with 16 digits and a large integer part", "[123456789012345.6]"),
    ("smallest decimal before exponent form", "[0.000001]"),
    ("exponent form below 1e-6", "[1e-7,1.5e-7,-2.5e-10]"),
    ("denormal minimum", "[5e-324]"),
    ("trailing zero in fraction changes the text", "[1.0]"),
    ("trailing zero after digits changes the text", "[0.10]"),
    ("exponent on an integer changes the text", "[1e2]"),
    ("plus sign in exponent changes the text", "[1e+2]"),
    ("uppercase exponent changes the text", "[1E-7]"),
    ("negative zero", "[-0]"),
    ("negative zero with fraction", "[-0.0]"),
    ("positive zero with fraction", "[0.0]"),
    ("integer-valued exponent form beyond 2^53", "[1e21]"),
    ("integer-valued 1e16", "[1e16]"),
    ("largest double", "[1.7976931348623157e308]"),
    ("overflowing exponent", "[1e400]"),
    ("leading zero", "[01]"),
    ("plus sign", "[+1]"),
    ("bare fraction", "[.5]"),
    ("trailing dot", "[1.]"),
    ("NaN literal", "[NaN]"),
    ("Infinity literal", "[Infinity]"),
    # duplicate keys
    ("duplicate top-level key", '{"a":1,"a":2}'),
    ("duplicate with identical value", '{"a":1,"a":1}'),
    ("duplicate in nested object", '{"x":{"b":1,"b":2}}'),
    ("duplicate inside array element", '[{"k":1,"k":2}]'),
    ("duplicate after key decoding", '{"a":1,"\\u0061":2}'),
    ("duplicate non-BMP key", '{"\U0001f600":1,"\\ud83d\\ude00":2}'),
    ("keys differing in case are distinct", '{"a":1,"A":2}'),
    ("same key in different objects is fine", '{"a":{"k":1},"b":{"k":2}}'),
    # nested and empty
    ("empty object", "{}"),
    ("empty array", "[]"),
    ("empty containers nested", '{"b":[],"a":{},"c":[[],{}]}'),
    ("null true false", '{"n":null,"t":true,"f":false}'),
    ("top-level string", '"x"'),
    ("top-level number", "42"),
    ("top-level null", "null"),
    ("insignificant whitespace is dropped", ' \n{ "b" : [ 1 , 2 ] ,\t"a" : { } }\r\n'),
    ("deep nesting", '{"a":{"b":{"c":{"d":[[[[1]]]]}}}}'),
    # malformed
    ("empty input", ""),
    ("trailing comma in object", '{"a":1,}'),
    ("trailing comma in array", "[1,]"),
    ("unquoted key", "{a:1}"),
    ("single quoted string", "['a']"),
    ("trailing characters", "{} x"),
    ("two documents", "{}{}"),
    ("lone high surrogate escape", '["\\ud800"]'),
    ("lone low surrogate escape", '["\\udc00"]'),
    ("raw newline in string", '["a\nb"]'),
    ("unknown escape", '["\\x41"]'),
    ("unterminated string", '["a'),
    ("comment", "[1]//x"),
]


def main():
    vectors = []
    for name, text in CASES:
        try:
            expected = {"canonicalUtf8": canonicalize(text)}
        except Rejected as r:
            expected = {"error": r.code}
        vectors.append({"name": name, "input": {"jsonUtf8": text}, "expected": expected})
    doc = {
        "description": "Canonical encoding of free-form payloads: RFC 8785 with three restrictions. A number is valid only when its decimal text survives a round trip through an IEEE double unchanged (integers within +/-2^53 written without fraction or exponent, other numbers with at most 15 significant digits written exactly as ECMAScript prints them); anything else must be a string. Duplicate object keys are rejected. U+0000 in any string or object key, written as the escape \\u0000, is rejected as nul_character (PostgreSQL JSONB cannot store it); a raw NUL byte is malformed like any raw control character, and lone surrogate escapes are malformed. Every other Unicode scalar value is a valid string character. Object keys sort by UTF-16 code units. input.jsonUtf8 is the document text; expected is either canonicalUtf8 (the exact canonical text) or error, one of invalid_number, duplicate_key, nul_character, malformed. The first problem in document order decides the error. Generated by scripts/gen-canonical-payload-vectors.py, an implementation independent of the Rust one.",
        "supportedIn": ["rust", "csharp", "typescript"],
        "vectors": vectors,
    }
    out = Path(__file__).resolve().parent.parent / "conformance/vectors/canonical-payload.json"
    out.write_text(json.dumps(doc, indent=2, ensure_ascii=True) + "\n")
    print(f"wrote {len(vectors)} vectors to {out}", file=sys.stderr)


main()
