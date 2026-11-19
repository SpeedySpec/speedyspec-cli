//! Search and replace that keeps PHP-serialized values valid.
//!
//! Apps store arrays and objects in the database as PHP's `serialize()` writes them, where every string carries its
//! length in bytes (`s:19:"https://old.example";`). Replacing a URL in place would leave the length wrong and the
//! whole value unreadable, so a value that is serialized is rewritten structurally instead: each string inside it is
//! replaced and its length recomputed, and a string whose contents are themselves serialized is rewritten the same
//! way. Anything that is not exactly one serialized value is replaced as plain bytes.

use std::borrow::Cow;

/// How deep arrays, objects and serialized strings may nest before a value is treated as plain text.
const MAX_DEPTH: usize = 64;

/// `value` with every `from` replaced by `to`, serialized values kept valid. An unchanged value is borrowed.
pub fn replace<'a>(value: &'a [u8], from: &[u8], to: &[u8]) -> Cow<'a, [u8]> {
    if from.is_empty() || find(value, from, 0).is_none() {
        return Cow::Borrowed(value);
    }
    Cow::Owned(rewrite(value, from, to, 0))
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn plain(value: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut at = 0;
    while let Some(found) = find(value, from, at) {
        out.extend_from_slice(&value[at..found]);
        out.extend_from_slice(to);
        at = found + from.len();
    }
    out.extend_from_slice(&value[at..]);
    out
}

fn rewrite(value: &[u8], from: &[u8], to: &[u8], depth: usize) -> Vec<u8> {
    if depth < MAX_DEPTH {
        let mut parser = Parser {
            input: value,
            pos: 0,
            from,
            to,
            depth,
        };
        let mut out = Vec::with_capacity(value.len());
        if parser.value(&mut out).is_some() && parser.pos == value.len() {
            return out;
        }
    }
    plain(value, from, to)
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    from: &'a [u8],
    to: &'a [u8],
    depth: usize,
}

impl Parser<'_> {
    fn expect(&mut self, text: &[u8]) -> Option<()> {
        if self.input.get(self.pos..self.pos + text.len())? == text {
            self.pos += text.len();
            Some(())
        } else {
            None
        }
    }

    /// A run of ASCII digits, as a number.
    fn number(&mut self) -> Option<usize> {
        let start = self.pos;
        while self.input.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        if self.pos == start || self.pos - start > 18 {
            return None;
        }
        std::str::from_utf8(&self.input[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }

    /// Copies from `start` to the current position.
    fn copy(&self, start: usize, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.input[start..self.pos]);
    }

    /// Bytes up to and including `;`, each one allowed by `ok`.
    fn scalar(&mut self, ok: impl Fn(u8) -> bool) -> Option<()> {
        let start = self.pos;
        while let Some(&b) = self.input.get(self.pos) {
            if b == b';' {
                if self.pos == start {
                    return None;
                }
                self.pos += 1;
                return Some(());
            }
            if !ok(b) {
                return None;
            }
            self.pos += 1;
        }
        None
    }

    /// `len` raw bytes.
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let bytes = self.input.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some(bytes)
    }

    fn nested(&mut self, f: impl FnOnce(&mut Self) -> Option<()>) -> Option<()> {
        self.depth += 1;
        if self.depth >= MAX_DEPTH {
            return None;
        }
        let result = f(self);
        self.depth -= 1;
        result
    }

    fn value(&mut self, out: &mut Vec<u8>) -> Option<()> {
        let start = self.pos;
        match *self.input.get(self.pos)? {
            b'N' => {
                self.expect(b"N;")?;
                self.copy(start, out);
            }
            b'b' => {
                self.expect(b"b:")?;
                self.scalar(|b| b == b'0' || b == b'1')?;
                self.copy(start, out);
            }
            b'i' => {
                self.expect(b"i:")?;
                self.scalar(|b| b.is_ascii_digit() || b == b'-' || b == b'+')?;
                self.copy(start, out);
            }
            b'd' => {
                self.expect(b"d:")?;
                self.scalar(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.'))?;
                self.copy(start, out);
            }
            b'r' | b'R' => {
                self.pos += 1;
                self.expect(b":")?;
                self.scalar(|b| b.is_ascii_digit())?;
                self.copy(start, out);
            }
            b's' => {
                self.expect(b"s:")?;
                let len = self.number()?;
                self.expect(b":\"")?;
                let content = self.take(len)?.to_vec();
                self.expect(b"\";")?;
                let replaced = if find(&content, self.from, 0).is_some() {
                    rewrite(&content, self.from, self.to, self.depth + 1)
                } else {
                    content
                };
                out.extend_from_slice(format!("s:{}:\"", replaced.len()).as_bytes());
                out.extend_from_slice(&replaced);
                out.extend_from_slice(b"\";");
            }
            b'S' => {
                // An escaped string: its length counts decoded bytes. Kept as it is.
                self.expect(b"S:")?;
                let len = self.number()?;
                self.expect(b":\"")?;
                for _ in 0..len {
                    if *self.input.get(self.pos)? == b'\\' {
                        let hex = self.take(3)?;
                        if !hex[1].is_ascii_hexdigit() || !hex[2].is_ascii_hexdigit() {
                            return None;
                        }
                    } else {
                        self.pos += 1;
                    }
                }
                self.expect(b"\";")?;
                self.copy(start, out);
            }
            b'E' => {
                self.expect(b"E:")?;
                let len = self.number()?;
                self.expect(b":\"")?;
                self.take(len)?;
                self.expect(b"\";")?;
                self.copy(start, out);
            }
            b'a' => {
                self.expect(b"a:")?;
                let count = self.number()?;
                self.expect(b":{")?;
                out.extend_from_slice(&self.input[start..self.pos]);
                self.nested(|p| p.members(count, out))?;
                self.expect(b"}")?;
                out.push(b'}');
            }
            b'O' => {
                self.expect(b"O:")?;
                let len = self.number()?;
                self.expect(b":\"")?;
                self.take(len)?;
                self.expect(b"\":")?;
                let count = self.number()?;
                self.expect(b":{")?;
                out.extend_from_slice(&self.input[start..self.pos]);
                self.nested(|p| p.members(count, out))?;
                self.expect(b"}")?;
                out.push(b'}');
            }
            b'C' => {
                // A class with its own serialize(): the payload is opaque, so it is replaced as plain bytes.
                self.expect(b"C:")?;
                let len = self.number()?;
                self.expect(b":\"")?;
                self.take(len)?;
                self.expect(b"\":")?;
                let head_end = self.pos;
                let payload_len = self.number()?;
                self.expect(b":{")?;
                let payload = self.take(payload_len)?.to_vec();
                self.expect(b"}")?;
                let replaced = plain(&payload, self.from, self.to);
                out.extend_from_slice(&self.input[start..head_end]);
                out.extend_from_slice(format!("{}:{{", replaced.len()).as_bytes());
                out.extend_from_slice(&replaced);
                out.push(b'}');
            }
            _ => return None,
        }
        Some(())
    }

    /// `count` key and value pairs.
    fn members(&mut self, count: usize, out: &mut Vec<u8>) -> Option<()> {
        for _ in 0..count {
            self.value(out)?;
            self.value(out)?;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &[u8] = b"https://old.example";
    const NEW: &[u8] = b"https://new.example.org";

    fn r(value: &str) -> String {
        String::from_utf8(replace(value.as_bytes(), OLD, NEW).into_owned()).unwrap()
    }

    #[test]
    fn plain_text() {
        assert_eq!(
            r("see https://old.example/a"),
            "see https://new.example.org/a"
        );
    }

    #[test]
    fn a_longer_url_in_a_serialized_string() {
        assert_eq!(
            r(r#"s:23:"https://old.example/one";"#),
            r#"s:27:"https://new.example.org/one";"#
        );
    }

    #[test]
    fn a_shorter_url() {
        let out = replace(br#"s:23:"https://old.example/one";"#, OLD, b"https://n.io");
        assert_eq!(&*out, br#"s:16:"https://n.io/one";"#);
    }

    #[test]
    fn two_urls_in_one_string() {
        assert_eq!(
            r(r#"s:43:"https://old.example and https://old.example";"#),
            r#"s:51:"https://new.example.org and https://new.example.org";"#
        );
    }

    #[test]
    fn nested_serialization() {
        // A serialized string whose contents are themselves serialized.
        let inner = r#"s:19:"https://old.example";"#;
        let outer = format!("s:{}:\"{inner}\";", inner.len());
        let expected_inner = r#"s:23:"https://new.example.org";"#;
        let expected = format!("s:{}:\"{expected_inner}\";", expected_inner.len());
        assert_eq!(r(&outer), expected);
    }

    #[test]
    fn arrays_objects_and_references() {
        let value = r#"a:3:{s:4:"home";s:19:"https://old.example";i:0;O:8:"stdClass":2:{s:3:"url";s:19:"https://old.example";s:4:"same";r:2;}s:1:"n";N;}"#;
        let expected = r#"a:3:{s:4:"home";s:23:"https://new.example.org";i:0;O:8:"stdClass":2:{s:3:"url";s:23:"https://new.example.org";s:4:"same";r:2;}s:1:"n";N;}"#;
        assert_eq!(r(value), expected);
    }

    #[test]
    fn scalars_and_enums_are_kept() {
        let value = r#"a:5:{i:0;b:1;i:1;i:-42;i:2;d:1.5;i:3;E:11:"Suit:Hearts";i:4;s:19:"https://old.example";}"#;
        let expected = r#"a:5:{i:0;b:1;i:1;i:-42;i:2;d:1.5;i:3;E:11:"Suit:Hearts";i:4;s:23:"https://new.example.org";}"#;
        assert_eq!(r(value), expected);
    }

    #[test]
    fn custom_serialized_payloads_get_their_length_recomputed() {
        let payload = "x=https://old.example";
        let value = format!("C:3:\"Foo\":{}:{{{payload}}}", payload.len());
        let new_payload = "x=https://new.example.org";
        assert_eq!(
            r(&value),
            format!("C:3:\"Foo\":{}:{{{new_payload}}}", new_payload.len())
        );
    }

    #[test]
    fn lengths_are_bytes() {
        let value = "s:22:\"é https://old.example\";";
        assert_eq!(r(value), "s:26:\"é https://new.example.org\";");
    }

    #[test]
    fn what_only_looks_serialized_is_plain_text() {
        // Wrong length, truncated, and trailing bytes: none is a serialized value, so the text is replaced as it is.
        assert_eq!(
            r(r#"s:5:"https://old.example";"#),
            r#"s:5:"https://new.example.org";"#
        );
        assert_eq!(
            r(r#"a:1:{s:3:"url";s:19:"https://old.example";"#),
            r#"a:1:{s:3:"url";s:19:"https://new.example.org";"#
        );
        assert_eq!(
            r(r#"s:19:"https://old.example";extra"#),
            r#"s:19:"https://new.example.org";extra"#
        );
    }

    #[test]
    fn deep_nesting_stops_at_the_cap_without_overflowing() {
        let mut value = String::from(r#"s:19:"https://old.example";"#);
        for _ in 0..1000 {
            value = format!("a:1:{{i:0;{value}}}");
        }
        let out = r(&value);
        assert!(out.contains("https://new.example.org"));
        assert!(!out.contains("https://old.example"));
    }

    #[test]
    fn unchanged_values_are_borrowed() {
        assert!(matches!(
            replace(b"nothing here", OLD, NEW),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            replace(b"s:3:\"abc\";", OLD, NEW),
            Cow::Borrowed(_)
        ));
    }
}
