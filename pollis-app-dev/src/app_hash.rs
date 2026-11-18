//! The App hash `pollis app build` prints (#2006), as #2005 specifies it:
//!
//! `app_hash = SHA-256("pollis-app-v1" || len(component) || component || JCS(manifest))`
//!
//! with `len` as an unsigned 64-bit big-endian integer and JCS the JSON
//! Canonicalization Scheme (RFC 8785). #2005 moves this into the shared crate
//! the runtime and the apps use, with the same test vector (`tests` below);
//! until then the CLI gets it from here.

use sha2::{Digest, Sha256};

/// Domain separation prefix.
pub const DOMAIN: &[u8] = b"pollis-app-v1";

/// RFC 8785 canonical form of a JSON value. A manifest holds only strings,
/// integers, booleans, arrays and objects; a non-integer number is refused
/// rather than risk an ECMAScript number formatting mismatch.
pub fn jcs(value: &serde_json::Value) -> Result<String, String> {
    let mut out = String::new();
    write_jcs(value, &mut out)?;
    Ok(out)
}

fn write_jcs(value: &serde_json::Value, out: &mut String) -> Result<(), String> {
    use serde_json::Value;
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                return Err(format!("non-integer number {n} in a manifest"));
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_jcs(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // RFC 8785 sorts member names by their UTF-16 code units.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_jcs(&map[key.as_str()], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// ECMAScript `JSON.stringify` string escaping, as RFC 8785 requires.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The App hash of a component and its manifest (the manifest as JSON text).
pub fn app_hash(component: &[u8], manifest_json: &[u8]) -> Result<[u8; 32], String> {
    let manifest: serde_json::Value =
        serde_json::from_slice(manifest_json).map_err(|e| format!("manifest: {e}"))?;
    let canonical = jcs(&manifest)?;
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update((component.len() as u64).to_be_bytes());
    h.update(component);
    h.update(canonical.as_bytes());
    Ok(h.finalize().into())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jcs_sorts_keys_and_drops_whitespace() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{ "b": [1, 2], "a": { "y": "\u0001\n", "x": true } }"#)
                .unwrap();
        assert_eq!(
            jcs(&v).unwrap(),
            r#"{"a":{"x":true,"y":"\u0001\n"},"b":[1,2]}"#
        );
    }

    #[test]
    fn jcs_refuses_floats() {
        let v: serde_json::Value = serde_json::from_str(r#"{"a": 1.5}"#).unwrap();
        assert!(jcs(&v).is_err());
    }

    /// The test vector #2005's shared function must reproduce, and the one
    /// the TypeScript CLI's own test checks against.
    #[test]
    fn app_hash_vector() {
        let component = b"\0asm\x0d\0\x01\0";
        let manifest = br#"{ "version": "1.0.0", "name": "hello" }"#;
        let hash = app_hash(component, manifest).unwrap();
        // Reformatting the manifest does not change the hash.
        let same = app_hash(component, br#"{"name":"hello","version":"1.0.0"}"#).unwrap();
        assert_eq!(hash, same);
        assert_eq!(
            hex(&hash),
            "9e07685078ac15851215cedf4ed4c955234cc1386ec457030dc8491bc0d07aff"
        );
    }
}
