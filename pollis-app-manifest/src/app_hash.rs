//! The App hash (#2005): the one function the runtime, the `pollis app` CLI
//! and the Pollis apps use to name exactly one App.
//!
//! ```text
//! app_hash      = SHA-256("pollis-app-v1"          || len(component) || component || JCS(manifest))
//! manifest_hash = SHA-256("pollis-app-manifest-v1" || JCS(manifest))
//! ```
//!
//! `len` is the component's length as an unsigned 64-bit big-endian integer and
//! JCS is the JSON Canonicalization Scheme (RFC 8785), so reformatting a
//! manifest never changes either hash. An approval pins both: `app_hash` binds
//! the code and its manifest together, `manifest_hash` lets an admin's app (which
//! never holds the component) bind exactly the manifest it showed.
//!
//! The test vectors below are also checked by the TypeScript CLI (#2006).

use sha2::{Digest, Sha256};

use crate::ManifestError;

/// Domain separation prefix of the App hash.
pub const APP_HASH_DOMAIN: &[u8] = b"pollis-app-v1";

/// Domain separation prefix of the manifest hash.
pub const MANIFEST_HASH_DOMAIN: &[u8] = b"pollis-app-manifest-v1";

/// A SHA-256 digest.
pub type Hash = [u8; 32];

/// RFC 8785 canonical form of a JSON value. A manifest holds only strings,
/// integers, booleans, arrays and objects; a non-integer number is refused
/// rather than risk an ECMAScript number formatting mismatch.
pub fn jcs(value: &serde_json::Value) -> Result<String, ManifestError> {
    let mut out = String::new();
    write_jcs(value, &mut out)?;
    Ok(out)
}

fn write_jcs(value: &serde_json::Value, out: &mut String) -> Result<(), ManifestError> {
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
                return Err(ManifestError::Parse(format!(
                    "non-integer number {n}; a manifest holds whole numbers only"
                )));
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

/// The canonical (JCS) text of a manifest given as JSON bytes.
pub fn canonical_manifest(manifest_json: &[u8]) -> Result<String, ManifestError> {
    let value: serde_json::Value =
        serde_json::from_slice(manifest_json).map_err(|e| ManifestError::Parse(e.to_string()))?;
    jcs(&value)
}

/// The App hash of a component and its manifest (the manifest as JSON bytes).
pub fn app_hash(component: &[u8], manifest_json: &[u8]) -> Result<Hash, ManifestError> {
    let canonical = canonical_manifest(manifest_json)?;
    let mut h = Sha256::new();
    h.update(APP_HASH_DOMAIN);
    h.update((component.len() as u64).to_be_bytes());
    h.update(component);
    h.update(canonical.as_bytes());
    Ok(h.finalize().into())
}

/// The manifest hash an approval pins beside the App hash.
pub fn manifest_hash(manifest_json: &[u8]) -> Result<Hash, ManifestError> {
    let canonical = canonical_manifest(manifest_json)?;
    let mut h = Sha256::new();
    h.update(MANIFEST_HASH_DOMAIN);
    h.update(canonical.as_bytes());
    Ok(h.finalize().into())
}

/// Lowercase hex, the form hashes are shown and signed in.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse a 64-character lowercase hex hash.
pub fn parse_hex_hash(text: &str) -> Option<Hash> {
    if text.len() != 64 || text.bytes().any(|b| !matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).ok()?;
        out[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(out)
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

    /// The pinned vector: the same one `pollis app build` (#2006) and the
    /// TypeScript CLI's own test check against.
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

    #[test]
    fn manifest_hash_vector() {
        let a = manifest_hash(br#"{ "version": "1.0.0", "name": "hello" }"#).unwrap();
        let b = manifest_hash(br#"{"name":"hello","version":"1.0.0"}"#).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            hex(&a),
            "5f671cbdbec345ca07fa59e5bd72f901b1483bad99eef9913990ea2c964c18cd"
        );
    }

    #[test]
    fn the_length_prefix_separates_component_from_manifest() {
        // Moving bytes between the component and the manifest changes the hash.
        let a = app_hash(b"ab", br#"{"n":1}"#).unwrap();
        let b = app_hash(b"a", br#"{"n":1}"#).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn hex_round_trips_and_refuses_uppercase() {
        let h = app_hash(b"x", b"{}").unwrap();
        assert_eq!(parse_hex_hash(&hex(&h)), Some(h));
        assert_eq!(parse_hex_hash(&hex(&h).to_uppercase()), None);
        assert_eq!(parse_hex_hash("abc"), None);
    }
}
