//! Native bindings for `system.text.*` — stdlib.md § system.text.Regex,
//! system.text.Encoding. Regex matching itself lives in `crate::mini_regex`
//! (shared with `system.io.File.glob`); this module wires a match result
//! into a `system.text.RegexMatch` `Value::Object` and implements base64
//! (no external crate — same "no dependency for this" stance already taken
//! for `mini_regex`/`system.SecureRandom`'s `/dev/urandom` CSPRNG).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::mini_regex;
use crate::value::{Object, Value};

/// Builds a `system.text.RegexMatch` object (stdlib.md § Result types:
/// `fullMatch: string`, `groups: string[]`) from a `mini_regex::Match` — `m`'s
/// offsets are char indices into `chars`, the same char vector the match was
/// found against. `groups[0]` duplicates `fullMatch` (stdlib.md documents
/// the exact layout as implementation-defined); a group that didn't
/// participate in the match (e.g. the untaken side of an alternation)
/// contributes an empty string rather than `null` — `groups` is declared
/// `string[]`, not `string|null[]`.
pub fn build_regex_match(m: &mini_regex::Match, chars: &[char]) -> Value {
    let slice = |s: usize, e: usize| -> String { chars[s..e].iter().collect() };
    let full_match = slice(m.start, m.end);
    let mut groups = vec![Value::Str(Arc::new(full_match.clone()))];
    groups.extend(m.groups.iter().map(|g| Value::Str(Arc::new(g.map(|(s, e)| slice(s, e)).unwrap_or_default()))));
    let mut fields = HashMap::new();
    fields.insert("fullMatch".to_string(), Value::Str(Arc::new(full_match)));
    fields.insert("groups".to_string(), Value::Array(Arc::new(Mutex::new(groups))));
    Value::Object(Arc::new(Mutex::new(Object { class_name: "system.text.RegexMatch".to_string(), fields })))
}

const BASE64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard (RFC 4648) base64 with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(BASE64_ALPHABET[(b0 >> 2) as usize] as char);
        out.push(BASE64_ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 { BASE64_ALPHABET[(b2 & 0x3f) as usize] as char } else { '=' });
    }
    out
}

/// Rejects anything that isn't valid base64 (bad character, or a length that
/// can't be padding-consistent) — stdlib.md declares `base64Decode` `throws
/// FormatException`, unlike `encodeUtf8`/`decodeUtf8`/`base64Encode` which
/// can't fail on their well-typed inputs.
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let trimmed = s.trim_end_matches('=');
    if trimmed.len() % 4 == 1 {
        return Err(format!("invalid base64 length: '{s}'"));
    }
    let mut bits: u32 = 0;
    let mut bit_count = 0u32;
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    for c in trimmed.chars() {
        let val = BASE64_ALPHABET
            .iter()
            .position(|&b| b as char == c)
            .ok_or_else(|| format!("invalid base64 character '{c}'"))? as u32;
        bits = (bits << 6) | val;
        bit_count += 6;
        if bit_count >= 8 {
            bit_count -= 8;
            out.push((bits >> bit_count) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{base64_decode, base64_encode};

    #[test]
    fn roundtrip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "hello, world!"] {
            let encoded = base64_encode(s.as_bytes());
            assert_eq!(base64_decode(&encoded).unwrap(), s.as_bytes());
        }
    }

    #[test]
    fn known_vectors() {
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYg==").unwrap(), b"foob");
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(base64_decode("a").is_err());
        assert!(base64_decode("!!!!").is_err());
    }
}
