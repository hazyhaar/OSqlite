/// Minimal recursive descent JSON parser for bare-metal use.
///
/// Produces a `JsonValue` tree from a JSON string. No external dependencies.
/// Handles: null, booleans, numbers (f64), strings (with full escape handling),
/// arrays, and objects.

use alloc::string::String;
use alloc::vec::Vec;

/// A JSON value.
#[derive(Debug, Clone)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// Get a field from an object by key.
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(fields) => {
                fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            }
            _ => None,
        }
    }

    /// Get as string reference.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Get as array slice.
    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Get as f64.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            JsonValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Get as i64 (truncates).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            JsonValue::Number(n) => Some(*n as i64),
            _ => None,
        }
    }

    /// Get as bool.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// Parse a JSON string into a `JsonValue`.
pub fn parse(input: &str) -> Result<JsonValue, JsonError> {
    let mut parser = Parser::new(input);
    let val = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos < parser.input.len() {
        return Err(JsonError::TrailingData);
    }
    Ok(val)
}

/// JSON parse error.
#[derive(Debug)]
pub enum JsonError {
    UnexpectedEof,
    UnexpectedChar(char),
    InvalidEscape,
    InvalidNumber,
    TrailingData,
}

impl core::fmt::Display for JsonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            JsonError::UnexpectedEof => write!(f, "unexpected end of JSON"),
            JsonError::UnexpectedChar(c) => write!(f, "unexpected character: '{}'", c),
            JsonError::InvalidEscape => write!(f, "invalid escape sequence"),
            JsonError::InvalidNumber => write!(f, "invalid number"),
            JsonError::TrailingData => write!(f, "trailing data after JSON"),
        }
    }
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn next_byte(&mut self) -> Option<u8> {
        let b = self.input.get(self.pos).copied()?;
        self.pos += 1;
        Some(b)
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), JsonError> {
        match self.next_byte() {
            Some(got) if got == b => Ok(()),
            Some(got) => Err(JsonError::UnexpectedChar(got as char)),
            None => Err(JsonError::UnexpectedEof),
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, JsonError> {
        self.skip_ws();
        match self.peek() {
            Some(b'"') => self.parse_string().map(JsonValue::Str),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b't') => self.parse_true(),
            Some(b'f') => self.parse_false(),
            Some(b'n') => self.parse_null(),
            Some(b) if b == b'-' || b.is_ascii_digit() => self.parse_number(),
            Some(b) => Err(JsonError::UnexpectedChar(b as char)),
            None => Err(JsonError::UnexpectedEof),
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.expect(b'"')?;
        let mut s = String::new();

        loop {
            match self.next_byte() {
                Some(b'"') => return Ok(s),
                Some(b'\\') => {
                    match self.next_byte() {
                        Some(b'"') => s.push('"'),
                        Some(b'\\') => s.push('\\'),
                        Some(b'/') => s.push('/'),
                        Some(b'n') => s.push('\n'),
                        Some(b'r') => s.push('\r'),
                        Some(b't') => s.push('\t'),
                        Some(b'b') => s.push('\u{08}'),
                        Some(b'f') => s.push('\u{0C}'),
                        Some(b'u') => {
                            let code = self.parse_hex4()?;
                            // Handle UTF-16 surrogate pairs
                            if (0xD800..=0xDBFF).contains(&code) {
                                // High surrogate — expect \uXXXX low surrogate
                                if self.next_byte() != Some(b'\\') || self.next_byte() != Some(b'u') {
                                    return Err(JsonError::InvalidEscape);
                                }
                                let low = self.parse_hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err(JsonError::InvalidEscape);
                                }
                                let cp = 0x10000 + ((code as u32 - 0xD800) << 10) + (low as u32 - 0xDC00);
                                if let Some(ch) = char::from_u32(cp) {
                                    s.push(ch);
                                }
                            } else if let Some(ch) = char::from_u32(code as u32) {
                                s.push(ch);
                            }
                        }
                        _ => return Err(JsonError::InvalidEscape),
                    }
                }
                Some(b) => {
                    // UTF-8 passthrough
                    s.push(b as char);
                }
                None => return Err(JsonError::UnexpectedEof),
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u16, JsonError> {
        let mut val = 0u16;
        for _ in 0..4 {
            let b = self.next_byte().ok_or(JsonError::UnexpectedEof)?;
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(JsonError::InvalidEscape),
            };
            val = (val << 4) | digit as u16;
        }
        Ok(val)
    }

    fn parse_number(&mut self) -> Result<JsonValue, JsonError> {
        let start = self.pos;

        // Optional minus
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }

        // Integer part
        match self.peek() {
            Some(b'0') => { self.pos += 1; }
            Some(b) if b.is_ascii_digit() => {
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() { self.pos += 1; } else { break; }
                }
            }
            _ => return Err(JsonError::InvalidNumber),
        }

        // Fractional part
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !self.peek().map_or(false, |b| b.is_ascii_digit()) {
                return Err(JsonError::InvalidNumber);
            }
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() { self.pos += 1; } else { break; }
            }
        }

        // Exponent
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            if !self.peek().map_or(false, |b| b.is_ascii_digit()) {
                return Err(JsonError::InvalidNumber);
            }
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() { self.pos += 1; } else { break; }
            }
        }

        let num_str = core::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| JsonError::InvalidNumber)?;

        // Parse using our own implementation since we don't have std
        let val = parse_f64(num_str).ok_or(JsonError::InvalidNumber)?;
        Ok(JsonValue::Number(val))
    }

    fn parse_object(&mut self) -> Result<JsonValue, JsonError> {
        self.expect(b'{')?;
        self.skip_ws();

        let mut fields = Vec::new();

        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(fields));
        }

        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let val = self.parse_value()?;
            fields.push((key, val));

            self.skip_ws();
            match self.peek() {
                Some(b',') => { self.pos += 1; }
                Some(b'}') => { self.pos += 1; return Ok(JsonValue::Object(fields)); }
                Some(b) => return Err(JsonError::UnexpectedChar(b as char)),
                None => return Err(JsonError::UnexpectedEof),
            }
        }
    }

    fn parse_array(&mut self) -> Result<JsonValue, JsonError> {
        self.expect(b'[')?;
        self.skip_ws();

        let mut items = Vec::new();

        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }

        loop {
            let val = self.parse_value()?;
            items.push(val);

            self.skip_ws();
            match self.peek() {
                Some(b',') => { self.pos += 1; }
                Some(b']') => { self.pos += 1; return Ok(JsonValue::Array(items)); }
                Some(b) => return Err(JsonError::UnexpectedChar(b as char)),
                None => return Err(JsonError::UnexpectedEof),
            }
        }
    }

    fn parse_true(&mut self) -> Result<JsonValue, JsonError> {
        self.expect_literal(b"true")?;
        Ok(JsonValue::Bool(true))
    }

    fn parse_false(&mut self) -> Result<JsonValue, JsonError> {
        self.expect_literal(b"false")?;
        Ok(JsonValue::Bool(false))
    }

    fn parse_null(&mut self) -> Result<JsonValue, JsonError> {
        self.expect_literal(b"null")?;
        Ok(JsonValue::Null)
    }

    fn expect_literal(&mut self, lit: &[u8]) -> Result<(), JsonError> {
        for &expected in lit {
            match self.next_byte() {
                Some(got) if got == expected => {}
                Some(got) => return Err(JsonError::UnexpectedChar(got as char)),
                None => return Err(JsonError::UnexpectedEof),
            }
        }
        Ok(())
    }
}

/// Simple f64 parser for no_std (handles integer, decimal, negative, exponent).
fn parse_f64(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut pos = 0;
    let negative = if bytes[pos] == b'-' {
        pos += 1;
        true
    } else {
        false
    };

    // Integer part
    let mut int_part: f64 = 0.0;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        int_part = int_part * 10.0 + (bytes[pos] - b'0') as f64;
        pos += 1;
    }

    // Fractional part
    let mut frac_part: f64 = 0.0;
    if pos < bytes.len() && bytes[pos] == b'.' {
        pos += 1;
        let mut scale = 0.1;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            frac_part += (bytes[pos] - b'0') as f64 * scale;
            scale *= 0.1;
            pos += 1;
        }
    }

    let mut val = int_part + frac_part;
    if negative {
        val = -val;
    }

    // Exponent
    if pos < bytes.len() && (bytes[pos] == b'e' || bytes[pos] == b'E') {
        pos += 1;
        let exp_neg = if pos < bytes.len() && bytes[pos] == b'-' {
            pos += 1;
            true
        } else {
            if pos < bytes.len() && bytes[pos] == b'+' {
                pos += 1;
            }
            false
        };

        let mut exp: i32 = 0;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            exp = exp * 10 + (bytes[pos] - b'0') as i32;
            pos += 1;
        }

        if exp_neg {
            for _ in 0..exp {
                val /= 10.0;
            }
        } else {
            for _ in 0..exp {
                val *= 10.0;
            }
        }
    }

    Some(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_null() {
        let v = parse("null").unwrap();
        assert!(matches!(v, JsonValue::Null));
    }

    #[test]
    fn test_parse_bool() {
        assert_eq!(parse("true").unwrap().as_bool(), Some(true));
        assert_eq!(parse("false").unwrap().as_bool(), Some(false));
    }

    #[test]
    fn test_parse_number() {
        assert_eq!(parse("42").unwrap().as_number(), Some(42.0));
        assert_eq!(parse("-3.14").unwrap().as_number(), Some(-3.14));
        assert_eq!(parse("1e2").unwrap().as_number(), Some(100.0));
    }

    #[test]
    fn test_parse_string() {
        assert_eq!(parse(r#""hello""#).unwrap().as_str(), Some("hello"));
        assert_eq!(parse(r#""he\"llo""#).unwrap().as_str(), Some("he\"llo"));
        assert_eq!(parse(r#""a\\b""#).unwrap().as_str(), Some("a\\b"));
        assert_eq!(parse(r#""\n\t""#).unwrap().as_str(), Some("\n\t"));
    }

    #[test]
    fn test_parse_unicode_escape() {
        assert_eq!(parse(r#""\u0041""#).unwrap().as_str(), Some("A"));
    }

    #[test]
    fn test_parse_array() {
        let v = parse("[1, 2, 3]").unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_number(), Some(1.0));
    }

    #[test]
    fn test_parse_object() {
        let v = parse(r#"{"key": "value", "num": 42}"#).unwrap();
        assert_eq!(v.get("key").unwrap().as_str(), Some("value"));
        assert_eq!(v.get("num").unwrap().as_number(), Some(42.0));
    }

    #[test]
    fn test_parse_nested() {
        let v = parse(r#"{"a": [1, {"b": true}]}"#).unwrap();
        let a = v.get("a").unwrap().as_array().unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[1].get("b").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn test_parse_empty_object() {
        let v = parse("{}").unwrap();
        assert!(v.get("anything").is_none());
    }

    #[test]
    fn test_parse_empty_array() {
        let v = parse("[]").unwrap();
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_sse_content_delta() {
        // Simulate parsing an actual SSE data payload
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello, world!"}}"#;
        let v = parse(data).unwrap();
        assert_eq!(v.get("type").unwrap().as_str(), Some("content_block_delta"));
        let delta = v.get("delta").unwrap();
        assert_eq!(delta.get("text").unwrap().as_str(), Some("Hello, world!"));
    }

    #[test]
    fn test_api_error_response() {
        let data = r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limited"}}"#;
        let v = parse(data).unwrap();
        assert_eq!(v.get("type").unwrap().as_str(), Some("error"));
        let err = v.get("error").unwrap();
        assert_eq!(err.get("type").unwrap().as_str(), Some("rate_limit_error"));
    }
}
