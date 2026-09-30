//! Minimal strict JSON reader and the canonical JSON writer primitives.
//!
//! The reader keeps object members in document order and numbers as their raw
//! text, so the validator can decode a line into typed records, re-serialize
//! them canonically and compare bytes.

use std::fmt::Write as _;

/// Largest magnitude allowed for any integer in a `.vtj` file: 2^53 − 1.
pub const MAX_SAFE_INT: i64 = (1 << 53) - 1;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Raw number text exactly as written.
    Number(String),
    String(String),
    Array(Vec<Value>),
    /// Members in document order. Duplicate keys are rejected by the parser.
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
}

const MAX_DEPTH: usize = 64;

/// Parses exactly one JSON value spanning all of `input`.
pub fn parse(input: &str) -> Result<Value, String> {
    let mut p = Parser { s: input.as_bytes(), i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("unexpected trailing data at byte {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.i))
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &[u8]) -> bool {
        if self.s[self.i..].starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            None => self.err("unexpected end of input"),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b't') if self.eat(b"true") => Ok(Value::Bool(true)),
            Some(b'f') if self.eat(b"false") => Ok(Value::Bool(false)),
            Some(b'n') if self.eat(b"null") => Ok(Value::Null),
            Some(_) => self.err("unexpected character"),
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err("nesting too deep");
        }
        Ok(())
    }

    fn object(&mut self) -> Result<Value, String> {
        self.enter()?;
        self.i += 1;
        let mut members: Vec<(String, Value)> = Vec::new();
        self.ws();
        if self.eat(b"}") {
            self.depth -= 1;
            return Ok(Value::Object(members));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return self.err("expected object key");
            }
            let key = self.string()?;
            if members.iter().any(|(k, _)| *k == key) {
                return self.err(&format!("duplicate key \"{key}\""));
            }
            self.ws();
            if !self.eat(b":") {
                return self.err("expected ':'");
            }
            self.ws();
            let v = self.value()?;
            members.push((key, v));
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"}") {
                break;
            }
            return self.err("expected ',' or '}'");
        }
        self.depth -= 1;
        Ok(Value::Object(members))
    }

    fn array(&mut self) -> Result<Value, String> {
        self.enter()?;
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.eat(b"]") {
            self.depth -= 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value()?);
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"]") {
                break;
            }
            return self.err("expected ',' or ']'");
        }
        self.depth -= 1;
        Ok(Value::Array(items))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let Some(h) = self.s.get(self.i..self.i + 4) else {
            return self.err("truncated \\u escape");
        };
        let h = std::str::from_utf8(h).map_err(|_| format!("invalid \\u escape at byte {}", self.i))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| format!("invalid \\u escape at byte {}", self.i))?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return self.err("invalid \\u escape");
        }
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(c) = self.peek() {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).map_err(|_| "invalid UTF-8".to_string())?);
            match self.peek() {
                None => return self.err("unterminated string"),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let Some(e) = self.peek() else { return self.err("unterminated escape") };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xd800..0xdc00).contains(&hi) {
                                if !self.eat(b"\\u") {
                                    return self.err("unpaired surrogate");
                                }
                                let lo = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&lo) {
                                    return self.err("unpaired surrogate");
                                }
                                0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                            } else if (0xdc00..0xe000).contains(&hi) {
                                return self.err("unpaired surrogate");
                            } else {
                                hi
                            };
                            out.push(char::from_u32(cp).expect("valid scalar value"));
                        }
                        _ => return self.err("invalid escape"),
                    }
                }
                Some(_) => return self.err("unescaped control character in string"),
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        self.eat(b"-");
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return self.err("invalid number"),
        }
        if self.eat(b".") && self.digits() == 0 {
            return self.err("invalid number fraction");
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if !self.eat(b"+") {
                self.eat(b"-");
            }
            if self.digits() == 0 {
                return self.err("invalid number exponent");
            }
        }
        Ok(Value::Number(String::from_utf8(self.s[start..self.i].to_vec()).expect("ascii")))
    }
}

/// Appends `s` as a canonical JSON string literal: only `"`, `\`, and control
/// characters are escaped; `/` and non-ASCII characters are written as is.
pub fn write_str(out: &mut String, s: &str) {
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
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Canonical text of a real number (used only for mastering metadata and
/// projection angles): the shortest decimal that round-trips, never an
/// exponent, `-0` written as `0`.
pub fn format_real(v: f64) -> String {
    debug_assert!(v.is_finite());
    if v == 0.0 {
        return "0".to_string();
    }
    format!("{v}")
}

/// Incremental writer for one canonical JSON object.
pub struct Obj<'a> {
    out: &'a mut String,
    first: bool,
}

impl<'a> Obj<'a> {
    pub fn new(out: &'a mut String) -> Self {
        out.push('{');
        Obj { out, first: true }
    }

    /// Writes the key and returns the buffer for the caller to append the value.
    pub fn key(&mut self, k: &str) -> &mut String {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
        write_str(self.out, k);
        self.out.push(':');
        self.out
    }

    pub fn str(&mut self, k: &str, v: &str) {
        write_str(self.key(k), v);
    }

    pub fn int(&mut self, k: &str, v: i64) {
        let _ = write!(self.key(k), "{v}");
    }

    pub fn uint(&mut self, k: &str, v: u64) {
        let _ = write!(self.key(k), "{v}");
    }

    pub fn real(&mut self, k: &str, v: f64) {
        let s = format_real(v);
        self.key(k).push_str(&s);
    }

    pub fn bool(&mut self, k: &str, v: bool) {
        self.key(k).push_str(if v { "true" } else { "false" });
    }

    pub fn end(self) {
        self.out.push('}');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested() {
        let v = parse(r#" {"a":[1,-2.5e3,"xé😀"],"b":{"c":null,"d":true}} "#).unwrap();
        let Value::Object(m) = v else { panic!() };
        assert_eq!(m[0].0, "a");
        let Value::Array(a) = &m[0].1 else { panic!() };
        assert_eq!(a[1], Value::Number("-2.5e3".into()));
        assert_eq!(a[2], Value::String("xé😀".into()));
    }

    #[test]
    fn rejects_bad_json() {
        for bad in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":01}"#,
            r#"{"a":1.}"#,
            r#"{"a":+1}"#,
            r#"{"a":"\ud800"}"#,
            r#"{"a":"\x"}"#,
            "{\"a\":\"\u{1}\"}",
            r#"{"a":1,}"#,
            r#"[1 2]"#,
            r#"{"a":1} x"#,
            r#"{"a":NaN}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn canonical_escapes() {
        let mut s = String::new();
        write_str(&mut s, "a\"\\/\u{8}\u{c}\n\r\t\u{1}\u{1f}é\u{7f}");
        assert_eq!(s, "\"a\\\"\\\\/\\b\\f\\n\\r\\t\\u0001\\u001fé\u{7f}\"");
    }

    #[test]
    fn real_format() {
        assert_eq!(format_real(1000.0), "1000");
        assert_eq!(format_real(0.708), "0.708");
        assert_eq!(format_real(-0.0), "0");
        assert_eq!(format_real(0.0001), "0.0001");
        assert_eq!(format_real(1e-7), "0.0000001");
    }
}
