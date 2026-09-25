//! Canonical JSON — the one byte encoding of a JSON value that signing and
//! digests can rely on. Byte-compatible with `crypto/canonical.ts` in
//! `hraness/relay`: object keys sort by UTF-16 code unit order (JavaScript
//! `sort()` semantics), strings escape exactly like `JSON.stringify`,
//! numbers are integers only, and there is no whitespace.
//!
//! Wire values are integers or strings in practice; floating-point numbers
//! are rejected rather than re-encoded, because `serde_json` and
//! `JSON.stringify` disagree on their rendering.

use serde_json::Value;

use crate::{Error, Result};

fn invalid(what: &'static str) -> Error {
    Error::from(xcb_core::Error::Invalid(what))
}

/// Escape a string exactly as `JSON.stringify` does: `"` and `\` escaped,
/// the five named control escapes, `\u00xx` lowercase for other controls,
/// and every other code point emitted raw.
fn escape_string(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{0009}' => out.push_str("\\t"),
            '\u{000a}' => out.push_str("\\n"),
            '\u{000c}' => out.push_str("\\f"),
            '\u{000d}' => out.push_str("\\r"),
            ch if ch < '\u{0020}' => {
                out.push_str("\\u00");
                out.push(char::from_digit((ch as u32) >> 4, 16).unwrap_or('0'));
                out.push(char::from_digit((ch as u32) & 0xf, 16).unwrap_or('0'));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

fn write_canonical(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                out.push_str(&int.to_string());
            } else if let Some(int) = number.as_u64() {
                out.push_str(&int.to_string());
            } else {
                return Err(invalid("canonical JSON carries integers only"));
            }
        }
        Value::String(text) => escape_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // JavaScript sorts keys by UTF-16 code units; UTF-8 byte order
            // disagrees above the BMP, so compare encoded key order.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                escape_string(key, out);
                out.push(':');
                write_canonical(&map[*key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// The canonical UTF-8 rendering of `value`.
pub fn canonicalize(value: &Value) -> Result<String> {
    let mut out = String::with_capacity(256);
    write_canonical(value, &mut out)?;
    Ok(out)
}

/// `sha256:<64 lowercase hex>` over the canonical encoding.
pub fn canonical_digest(value: &Value) -> Result<String> {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(canonicalize(value)?.as_bytes());
    Ok(format!("sha256:{}", hex::encode(digest)))
}

/// `sha256:<64 lowercase hex>` over raw bytes — the request/result digest
/// contract. Committing to the *plaintext* keeps the digest stable across
/// an idempotent replay, where a resealed envelope would draw a fresh IV
/// and hash differently every time.
pub fn bytes_digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_and_strips_whitespace() {
        let value = json!({ "b": 2, "a": { "d": [3, "x"], "c": true }, "z": null });
        assert_eq!(
            canonicalize(&value).unwrap(),
            "{\"a\":{\"c\":true,\"d\":[3,\"x\"]},\"b\":2,\"z\":null}"
        );
    }

    #[test]
    fn matches_the_golden_vector() {
        let value =
            json!({ "challengeId": "vec", "contract": "relay.dev.v1:device-bind", "nonce": "AA" });
        assert_eq!(
            canonicalize(&value).unwrap(),
            "{\"challengeId\":\"vec\",\"contract\":\"relay.dev.v1:device-bind\",\"nonce\":\"AA\"}"
        );
    }

    #[test]
    fn escapes_like_json_stringify() {
        assert_eq!(
            canonicalize(&json!("a\"b\\c\u{0001}")).unwrap(),
            "\"a\\\"b\\\\c\\u0001\""
        );
        assert_eq!(
            canonicalize(&json!("\u{0008}\u{000b}")).unwrap(),
            "\"\\b\\u000b\""
        );
        // Non-ASCII is emitted raw, never \uXXXX-escaped.
        assert_eq!(canonicalize(&json!("café")).unwrap(), "\"café\"");
    }

    #[test]
    fn sorts_keys_by_utf16_code_units() {
        // A supplementary-plane key (lead surrogate D800) sorts before a
        // BMP key at E000 under UTF-16 order, after it under code points.
        let value = json!({ "\u{e000}": 1, "\u{10000}": 2 });
        assert_eq!(
            canonicalize(&value).unwrap(),
            "{\"\u{10000}\":2,\"\u{e000}\":1}"
        );
    }

    #[test]
    fn rejects_floats() {
        assert!(canonicalize(&json!(2.5)).is_err());
        // Even an integral f64 would render differently under JSON.stringify.
        assert!(canonicalize(&json!(2.0)).is_err());
        assert_eq!(canonicalize(&json!(-3)).unwrap(), "-3");
        assert_eq!(
            canonicalize(&json!(u64::MAX)).unwrap(),
            "18446744073709551615"
        );
    }
}
