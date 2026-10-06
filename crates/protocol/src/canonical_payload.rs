//! Canonical encoding of free-form payloads: RFC 8785 (JCS) restricted so
//! three independent SDKs can reproduce it byte for byte.
//!
//! Restrictions on top of RFC 8785:
//! - A number is valid only if its decimal text survives a round trip through
//!   an IEEE double unchanged: integers within +/-2^53, other numbers with at
//!   most 15 significant digits and written exactly as ECMAScript prints them
//!   (so `1.0`, `1e2`, `-0` and `1E-7` are invalid). Anything else is a string.
//! - Duplicate object keys are rejected.
//! - U+0000 is rejected in every string and object key, written as `\u0000` or
//!   (as any raw control character is) unescaped. PostgreSQL `JSONB` cannot
//!   store it, and validity must not depend on the database.
//!
//! Permitted string characters: every Unicode scalar value except U+0000. U+0001
//! to U+001F are valid only as escapes; lone surrogates (`\ud800`) are
//! `malformed`. Noncharacters (U+FFFE, U+FFFF) and U+007F..U+009F are valid and
//! round trip through `JSONB`.
//!
//! Object keys sort by UTF-16 code units, strings escape only `"`, `\`, and
//! control characters below U+0020 (`\b \t \n \f \r`, else lowercase `\u00xx`).
//! No I/O; every node derives the same bytes from the same value.

use std::cmp::Ordering;
use std::fmt;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// Largest integer magnitude an IEEE double represents exactly.
pub const MAX_SAFE_INTEGER: u64 = 1 << 53;
/// Most significant digits a non-integer number may carry.
pub const MAX_SIGNIFICANT_DIGITS: usize = 15;
/// Deepest object/array nesting a payload may have.
pub const MAX_DEPTH: usize = 128;

/// Why a payload has no canonical encoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalPayloadError {
    #[error("number `{number}` is not exactly representable as an IEEE double; use a string")]
    InvalidNumber { number: String },
    #[error("duplicate object key {key:?}")]
    DuplicateKey { key: String },
    #[error("string or object key contains U+0000")]
    NulCharacter,
    #[error("malformed JSON: {0}")]
    Malformed(String),
    #[error("payload nests deeper than {MAX_DEPTH} levels")]
    TooDeep,
}

impl CanonicalPayloadError {
    /// Stable machine-readable code, shared with `conformance/vectors/canonical-payload.json`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidNumber { .. } => "invalid_number",
            Self::DuplicateKey { .. } => "duplicate_key",
            Self::NulCharacter => "nul_character",
            Self::Malformed(_) => "malformed",
            Self::TooDeep => "too_deep",
        }
    }
}

fn check_string(s: &str) -> Result<(), CanonicalPayloadError> {
    if s.contains('\0') {
        return Err(CanonicalPayloadError::NulCharacter);
    }
    Ok(())
}

/// Canonical encoding of `value`, or the first restriction it violates. Judges numbers by
/// value (`1.0` hashes as `1`); only [`parse_strict`] checks the number text.
pub fn canonicalize(value: &Value) -> Result<String, CanonicalPayloadError> {
    let mut out = String::new();
    write_value(value, &mut out, 0)?;
    Ok(out)
}

/// Checks that `value` has a canonical encoding.
pub fn validate(value: &Value) -> Result<(), CanonicalPayloadError> {
    canonicalize(value).map(|_| ())
}

/// Parses JSON `text` under the full restrictions, including the exact
/// decimal text of every number and duplicate keys, which a `Value` no longer
/// carries.
pub fn parse_strict(text: &str) -> Result<Value, CanonicalPayloadError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
    };
    parser.skip_ws();
    let value = parser.value(0)?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(parser.malformed("trailing characters"));
    }
    Ok(value)
}

/// Canonical encoding of the JSON document `text`.
pub fn canonicalize_str(text: &str) -> Result<String, CanonicalPayloadError> {
    canonicalize(&parse_strict(text)?)
}

/// `serde` deserializer for payload fields: rejects duplicate keys and
/// invalid numbers while the document is read.
pub fn deserialize_strict<'de, D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
    d.deserialize_any(StrictVisitor)
}

fn write_value(value: &Value, out: &mut String, depth: usize) -> Result<(), CanonicalPayloadError> {
    if depth > MAX_DEPTH {
        return Err(CanonicalPayloadError::TooDeep);
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&number_text(n)?),
        Value::String(s) => write_string(s, out)?,
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out, depth + 1)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| cmp_utf16(a.0, b.0));
            out.push('{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out)?;
                out.push(':');
                write_value(item, out, depth + 1)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn cmp_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_string(s: &str, out: &mut String) -> Result<(), CanonicalPayloadError> {
    check_string(s)?;
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(())
}

fn number_text(n: &Number) -> Result<String, CanonicalPayloadError> {
    if let Some(u) = n.as_u64() {
        return safe_integer(u as i128);
    }
    if let Some(i) = n.as_i64() {
        return safe_integer(i as i128);
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    float_text(f).ok_or_else(|| CanonicalPayloadError::InvalidNumber {
        number: n.to_string(),
    })
}

fn safe_integer(i: i128) -> Result<String, CanonicalPayloadError> {
    if i.unsigned_abs() <= MAX_SAFE_INTEGER as u128 {
        Ok(i.to_string())
    } else {
        Err(CanonicalPayloadError::InvalidNumber {
            number: i.to_string(),
        })
    }
}

/// ECMAScript `Number::toString` of `f`, or `None` when `f` is not a valid
/// payload number (non-finite, integral beyond 2^53, or over 15 significant
/// digits when not integral).
fn float_text(f: f64) -> Option<String> {
    if !f.is_finite() {
        return None;
    }
    if f == 0.0 {
        return Some("0".to_string());
    }
    let integral = f.fract() == 0.0;
    if integral && f.abs() > MAX_SAFE_INTEGER as f64 {
        return None;
    }
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e')?;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if !integral && digits.len() > MAX_SIGNIFICANT_DIGITS {
        return None;
    }
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().ok()? + 1;
    let mut out = String::new();
    if f < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.push_str(&"0".repeat((n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat((-n) as usize));
        out.push_str(&digits);
    } else {
        let e = n - 1;
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&e.abs().to_string());
    }
    Some(out)
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        safe_integer(v as i128).map_err(E::custom)?;
        Ok(Value::from(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        safe_integer(v as i128).map_err(E::custom)?;
        Ok(Value::from(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        if float_text(v).is_none() {
            return Err(E::custom(CanonicalPayloadError::InvalidNumber {
                number: v.to_string(),
            }));
        }
        Ok(Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        check_string(v).map_err(E::custom)?;
        Ok(Value::String(v.to_string()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        check_string(&v).map_err(E::custom)?;
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(StrictSeed)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut map = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            check_string(&key).map_err(de::Error::custom)?;
            let value = access.next_value_seed(StrictSeed)?;
            if map.insert(key.clone(), value).is_some() {
                return Err(de::Error::custom(CanonicalPayloadError::DuplicateKey {
                    key,
                }));
            }
        }
        Ok(Value::Object(map))
    }
}

struct StrictSeed;

impl<'de> de::DeserializeSeed<'de> for StrictSeed {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn malformed(&self, what: &str) -> CanonicalPayloadError {
        CanonicalPayloadError::Malformed(format!("{what} at byte {}", self.pos))
    }

    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, literal: &str) -> bool {
        if self.bytes[self.pos..].starts_with(literal.as_bytes()) {
            self.pos += literal.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, CanonicalPayloadError> {
        if depth > MAX_DEPTH {
            return Err(CanonicalPayloadError::TooDeep);
        }
        match self.bytes.get(self.pos) {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') if self.eat("true") => Ok(Value::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Value::Bool(false)),
            Some(b'n') if self.eat("null") => Ok(Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.malformed("unexpected token")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, CanonicalPayloadError> {
        self.pos += 1;
        let mut map = Map::new();
        self.skip_ws();
        if self.eat("}") {
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws();
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.malformed("expected object key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(":") {
                return Err(self.malformed("expected `:`"));
            }
            self.skip_ws();
            let value = self.value(depth + 1)?;
            if map.insert(key.clone(), value).is_some() {
                return Err(CanonicalPayloadError::DuplicateKey { key });
            }
            self.skip_ws();
            if self.eat(",") {
                continue;
            }
            if self.eat("}") {
                return Ok(Value::Object(map));
            }
            return Err(self.malformed("expected `,` or `}`"));
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, CanonicalPayloadError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat("]") {
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            if self.eat(",") {
                continue;
            }
            if self.eat("]") {
                return Ok(Value::Array(items));
            }
            return Err(self.malformed("expected `,` or `]`"));
        }
    }

    fn hex4(&mut self) -> Result<u32, CanonicalPayloadError> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| self.malformed("bad \\u escape"))?;
        self.pos += 4;
        Ok(u32::from_str_radix(digits, 16).expect("four hex digits"))
    }

    fn string(&mut self) -> Result<String, CanonicalPayloadError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let rest = &self.text[self.pos..];
            let c = rest
                .chars()
                .next()
                .ok_or_else(|| self.malformed("unterminated string"))?;
            self.pos += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let esc = self.bytes.get(self.pos).copied();
                    self.pos += 1;
                    match esc {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'u') => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) {
                                if !self.eat("\\u") {
                                    return Err(self.malformed("lone surrogate"));
                                }
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.malformed("lone surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            if code == 0 {
                                return Err(CanonicalPayloadError::NulCharacter);
                            }
                            out.push(
                                char::from_u32(code)
                                    .ok_or_else(|| self.malformed("lone surrogate"))?,
                            );
                        }
                        _ => return Err(self.malformed("bad escape")),
                    }
                }
                c if (c as u32) < 0x20 => return Err(self.malformed("raw control character")),
                c => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Result<Value, CanonicalPayloadError> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let from = p.pos;
            while matches!(p.bytes.get(p.pos), Some(b'0'..=b'9')) {
                p.pos += 1;
            }
            p.pos - from
        };
        self.eat("-");
        if self.bytes.get(self.pos) == Some(&b'0') {
            self.pos += 1;
        } else if digits(self) == 0 {
            return Err(self.malformed("bad number"));
        }
        let mut integer_form = true;
        if self.eat(".") {
            integer_form = false;
            if digits(self) == 0 {
                return Err(self.malformed("bad number"));
            }
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            integer_form = false;
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if digits(self) == 0 {
                return Err(self.malformed("bad number"));
            }
        }
        let token = &self.text[start..self.pos];
        let invalid = || CanonicalPayloadError::InvalidNumber {
            number: token.to_string(),
        };
        if integer_form {
            if token == "-0" {
                return Err(invalid());
            }
            let n: i128 = token.parse().map_err(|_| invalid())?;
            safe_integer(n).map_err(|_| invalid())?;
            return Ok(Value::from(n as i64));
        }
        let f: f64 = token.parse().map_err(|_| invalid())?;
        if float_text(f).as_deref() != Some(token) {
            return Err(invalid());
        }
        Ok(Number::from_f64(f).map_or(Value::Null, Value::Number))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canon(text: &str) -> String {
        canonicalize_str(text).unwrap()
    }

    #[test]
    fn keys_sort_by_utf16_code_units_not_code_points() {
        // U+10000 is the surrogate pair D800 DC00, which sorts before U+E000.
        let got = canon("{\"\u{e000}\":1,\"\u{10000}\":2}");
        assert_eq!(got, "{\"\u{10000}\":2,\"\u{e000}\":1}");
    }

    #[test]
    fn strings_escape_only_what_rfc_8785_requires() {
        let got = canon(r#"["a\u0008\u001f\u007f \/\"\\é😀"]"#);
        assert_eq!(got, "[\"a\\b\\u001f\u{7f}\u{2028}/\\\"\\\\é😀\"]");
    }

    #[test]
    fn numbers_at_and_beyond_bounds() {
        assert_eq!(
            canon("[9007199254740992,-9007199254740992]"),
            "[9007199254740992,-9007199254740992]"
        );
        for bad in [
            "9007199254740993",
            "-9007199254740993",
            "1e21",
            "1.0",
            "-0",
            "1e2",
            "1E-7",
            "0.1234567890123456",
            "01",
        ] {
            assert!(canonicalize_str(bad).is_err(), "{bad}");
        }
        assert_eq!(
            canon("[0.5,-0.000001,1e-7,12345678901234.5]"),
            "[0.5,-0.000001,1e-7,12345678901234.5]"
        );
    }

    #[test]
    fn duplicate_keys_are_rejected_at_any_depth() {
        assert!(matches!(
            canonicalize_str(r#"{"a":{"b":1,"b":2}}"#),
            Err(CanonicalPayloadError::DuplicateKey { .. })
        ));
        assert!(serde_json::from_str::<Wrapper>(r#"{"p":{"a":1,"a":1}}"#).is_err());
    }

    #[derive(serde::Deserialize)]
    struct Wrapper {
        #[serde(deserialize_with = "deserialize_strict")]
        #[allow(dead_code)]
        p: Value,
    }

    #[test]
    fn value_path_rejects_unrepresentable_numbers() {
        assert!(validate(&json!({"n": 18446744073709551615u64})).is_err());
        assert!(validate(&json!({"n": 0.1234567890123456})).is_err());
        assert!(validate(&json!({"n": 1e300})).is_err());
        assert!(serde_json::from_str::<Wrapper>(r#"{"p":[1e300]}"#).is_err());
        assert_eq!(
            canonicalize(&json!({"b": [], "a": {}})).unwrap(),
            r#"{"a":{},"b":[]}"#
        );
    }

    #[test]
    fn value_path_parses_doubles_correctly_rounded() {
        let w: Wrapper =
            serde_json::from_str(r#"{"p":[1e-24,5e-304,9.32314010864547e-18]}"#).unwrap();
        assert_eq!(
            canonicalize(&w.p).unwrap(),
            "[1e-24,5e-304,9.32314010864547e-18]"
        );
    }

    #[test]
    fn nul_is_rejected_in_values_and_keys_on_every_path() {
        for text in [r#"["a\u0000b"]"#, r#"{"a\u0000":1}"#, r#"{"a":{"\u0000":1}}"#] {
            assert_eq!(
                canonicalize_str(text),
                Err(CanonicalPayloadError::NulCharacter),
                "{text}"
            );
            assert!(serde_json::from_str::<Wrapper>(&format!(r#"{{"p":{text}}}"#)).is_err());
        }
        assert_eq!(
            validate(&json!({"k": ["x\0"]})),
            Err(CanonicalPayloadError::NulCharacter)
        );
        assert_eq!(
            validate(&json!({"k\0": 1})),
            Err(CanonicalPayloadError::NulCharacter)
        );
        assert!(matches!(
            canonicalize_str("[\"a\0b\"]"),
            Err(CanonicalPayloadError::Malformed(_))
        ));
    }

    #[test]
    fn nesting_is_bounded() {
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert_eq!(canonicalize_str(&deep), Err(CanonicalPayloadError::TooDeep));
    }

    #[test]
    fn protocol_events_reject_duplicate_payload_keys_on_read() {
        let id = crate::ids::GlobalId::new("identity", "a", "self", "k");
        let event = crate::events::ProtocolEvent {
            id: uuid::Uuid::nil(),
            kind: "k".into(),
            issuer: id.clone(),
            subject: id,
            payload: json!("PAYLOAD"),
            timestamp: time::OffsetDateTime::UNIX_EPOCH,
            version: 1,
            identity_chain: None,
        };
        let template = serde_json::to_string(&event).unwrap();
        let with = |payload: &str| template.replace("\"PAYLOAD\"", payload);
        let parse = |t: String| serde_json::from_str::<crate::events::ProtocolEvent>(&t);
        assert!(parse(with(r#"{"a":1}"#)).is_ok());
        assert!(parse(with(r#"{"a":1,"a":2}"#)).is_err());
        assert!(parse(with(r#"{"a":"\u0000"}"#)).is_err());
    }
}
