//! S-expression parser and serializer for the EPC protocol.
//!
//! Implements parsing and serialization compatible with Python's `sexpdata` library
//! and Emacs Lisp's `read`/`print` functions.

use std::fmt;

use thiserror::Error;

/// Errors that can occur during S-expression parsing.
#[derive(Debug, Error)]
pub enum SexpError {
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("unexpected character: {0:?}")]
    UnexpectedChar(char),
    #[error("unterminated string")]
    UnterminatedString,
    #[error("invalid escape sequence: \\{0}")]
    InvalidEscape(char),
    #[error("unmatched closing parenthesis")]
    UnmatchedClose,
    #[error("invalid number: {0}")]
    InvalidNumber(String),
}

/// An S-expression value, compatible with Emacs Lisp and Python sexpdata.
#[derive(Debug, Clone, PartialEq)]
pub enum SexpValue {
    /// `nil` — both false and empty list in Emacs Lisp
    Nil,
    /// `t` — boolean true
    Bool(bool),
    /// Integer (i64)
    Integer(i64),
    /// Floating-point number
    Float(f64),
    /// Double-quoted string
    String(String),
    /// Unquoted symbol (e.g., `method-name`, `textDocument/completion`)
    Symbol(String),
    /// Keyword symbol starting with `:` (e.g., `:line`, `:character`)
    Keyword(String),
    /// Cons cell `(a . b)`
    Cons(Box<SexpValue>, Box<SexpValue>),
    /// Proper list `(a b c)`
    List(Vec<SexpValue>),
    /// Quoted value `'x`
    Quoted(Box<SexpValue>),
}

impl SexpValue {
    /// Returns true if this value is Nil.
    pub fn is_nil(&self) -> bool {
        matches!(self, SexpValue::Nil)
    }

    /// Try to extract as a string reference.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            SexpValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to extract as a symbol name.
    pub fn as_symbol(&self) -> Option<&str> {
        match self {
            SexpValue::Symbol(s) => Some(s),
            _ => None,
        }
    }

    /// Try to extract as an integer.
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            SexpValue::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// Try to extract as a float.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            SexpValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Try to extract as a list reference.
    pub fn as_list(&self) -> Option<&[SexpValue]> {
        match self {
            SexpValue::List(items) => Some(items),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// A simple recursive-descent S-expression parser.
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

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<SexpValue, SexpError> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(SexpError::UnexpectedEof),
            Some(b'"') => self.parse_string(),
            Some(b'(') => self.parse_list(),
            Some(b')') => Err(SexpError::UnmatchedClose),
            Some(b'\'') => self.parse_quoted(),
            Some(_) => self.parse_atom(),
        }
    }

    fn parse_string(&mut self) -> Result<SexpValue, SexpError> {
        assert_eq!(self.peek(), Some(b'"'));
        self.advance(); // skip opening "

        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err(SexpError::UnterminatedString),
                Some(b'"') => {
                    self.advance();
                    return Ok(SexpValue::String(s));
                }
                Some(b'\\') => {
                    self.advance();
                    match self.peek() {
                        None => return Err(SexpError::UnterminatedString),
                        Some(b'n') => {
                            s.push('\n');
                            self.advance();
                        }
                        Some(b't') => {
                            s.push('\t');
                            self.advance();
                        }
                        Some(b'r') => {
                            s.push('\r');
                            self.advance();
                        }
                        Some(b'\\') => {
                            s.push('\\');
                            self.advance();
                        }
                        Some(b'"') => {
                            s.push('"');
                            self.advance();
                        }
                        Some(b'a') => {
                            s.push('\x07'); // bell
                            self.advance();
                        }
                        Some(b'b') => {
                            s.push('\x08'); // backspace
                            self.advance();
                        }
                        Some(b'f') => {
                            s.push('\x0C'); // form feed
                            self.advance();
                        }
                        Some(b'0') => {
                            s.push('\0');
                            self.advance();
                        }
                        Some(c) => {
                            // Unknown escape: keep literally (Emacs behavior)
                            s.push(c as char);
                            self.advance();
                        }
                    }
                }
                Some(_) => {
                    // Read a full UTF-8 character
                    let remaining = &self.input[self.pos..];
                    let ch = std::str::from_utf8(remaining)
                        .ok()
                        .and_then(|s| s.chars().next())
                        .unwrap_or('?' as char);
                    s.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn parse_list(&mut self) -> Result<SexpValue, SexpError> {
        assert_eq!(self.peek(), Some(b'('));
        self.advance(); // skip (

        let mut items = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return Err(SexpError::UnexpectedEof),
                Some(b')') => {
                    self.advance();
                    return Ok(SexpValue::List(items));
                }
                Some(b'.') => {
                    // Check if this is a cons dot (surrounded by whitespace)
                    // or part of an atom like "3.14" or ".method"
                    let next = self.input.get(self.pos + 1).copied();
                    if next == Some(b' ') || next == Some(b'\t') || next == Some(b'\n') || next == Some(b')') {
                        // Cons dot — only valid with exactly one item before it
                        if items.len() != 1 {
                            // Multiple items before dot: handle as improper list
                            // For now, treat as cons of last item
                        }
                        self.advance(); // skip .
                        self.skip_whitespace();
                        let cdr = self.parse_value()?;
                        self.skip_whitespace();
                        match self.peek() {
                            Some(b')') => {
                                self.advance();
                                if items.len() == 1 {
                                    return Ok(SexpValue::Cons(
                                        Box::new(items.into_iter().next().unwrap()),
                                        Box::new(cdr),
                                    ));
                                } else {
                                    // Improper list with multiple items: not standard but handle gracefully
                                    // Build nested cons cells
                                    let car = items.remove(0);
                                    let mut result = SexpValue::Cons(
                                        Box::new(items.pop().unwrap_or(SexpValue::Nil)),
                                        Box::new(cdr),
                                    );
                                    for item in items.into_iter().rev() {
                                        result = SexpValue::Cons(Box::new(item), Box::new(result));
                                    }
                                    return Ok(SexpValue::Cons(Box::new(car), Box::new(result)));
                                }
                            }
                            _ => return Err(SexpError::UnexpectedChar('.')),
                        }
                    } else {
                        // Part of an atom (e.g., number like ".5" or symbol like ".method")
                        items.push(self.parse_atom()?);
                    }
                }
                _ => {
                    items.push(self.parse_value()?);
                }
            }
        }
    }

    fn parse_quoted(&mut self) -> Result<SexpValue, SexpError> {
        assert_eq!(self.peek(), Some(b'\''));
        self.advance(); // skip '
        let value = self.parse_value()?;
        Ok(SexpValue::Quoted(Box::new(value)))
    }

    fn parse_atom(&mut self) -> Result<SexpValue, SexpError> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'(' || b == b')' || b == b'"' {
                break;
            }
            self.advance();
        }

        if self.pos == start {
            return Err(self.peek().map_or(SexpError::UnexpectedEof, |c| {
                SexpError::UnexpectedChar(c as char)
            }));
        }

        let token = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| SexpError::UnexpectedChar('?'))?;

        // nil
        if token == "nil" {
            return Ok(SexpValue::Nil);
        }

        // t (boolean true)
        if token == "t" {
            return Ok(SexpValue::Bool(true));
        }

        // Keyword (starts with :)
        if token.starts_with(':') {
            return Ok(SexpValue::Keyword(token.to_string()));
        }

        // Try integer
        if let Ok(n) = token.parse::<i64>() {
            return Ok(SexpValue::Integer(n));
        }

        // Try float
        if let Ok(f) = token.parse::<f64>() {
            // Only treat as float if it contains a dot or 'e'/'E'
            if token.contains('.') || token.contains('e') || token.contains('E') {
                return Ok(SexpValue::Float(f));
            }
        }

        // Otherwise it's a symbol
        Ok(SexpValue::Symbol(token.to_string()))
    }
}

/// Parse an S-expression string into a `SexpValue`.
pub fn parse(input: &str) -> Result<SexpValue, SexpError> {
    let mut parser = Parser::new(input);
    let value = parser.parse_value()?;
    Ok(value)
}

/// Parse an S-expression, returning the value and the number of bytes consumed.
pub fn parse_with_len(input: &str) -> Result<(SexpValue, usize), SexpError> {
    let mut parser = Parser::new(input);
    let value = parser.parse_value()?;
    Ok((value, parser.pos))
}

// ---------------------------------------------------------------------------
// Serializer
// ---------------------------------------------------------------------------

/// Serialize a `SexpValue` to an S-expression string.
pub fn serialize(value: &SexpValue) -> String {
    let mut buf = String::new();
    serialize_into(value, &mut buf);
    buf
}

fn serialize_into(value: &SexpValue, buf: &mut String) {
    match value {
        SexpValue::Nil => buf.push_str("nil"),
        SexpValue::Bool(true) => buf.push('t'),
        SexpValue::Bool(false) => buf.push_str("nil"),
        SexpValue::Integer(n) => {
            buf.push_str(&n.to_string());
        }
        SexpValue::Float(f) => {
            // Emacs/sexpdata format: always include decimal point
            let s = format!("{}", f);
            if !s.contains('.') && !s.contains('e') && !s.contains('E') && !s.contains("inf") && !s.contains("nan") {
                buf.push_str(&s);
                buf.push_str(".0");
            } else {
                buf.push_str(&s);
            }
        }
        SexpValue::String(s) => {
            buf.push('"');
            for ch in s.chars() {
                match ch {
                    '"' => buf.push_str("\\\""),
                    '\\' => buf.push_str("\\\\"),
                    '\n' => buf.push_str("\\n"),
                    '\t' => buf.push_str("\\t"),
                    '\r' => buf.push_str("\\r"),
                    '\x07' => buf.push_str("\\a"),
                    '\x08' => buf.push_str("\\b"),
                    '\x0C' => buf.push_str("\\f"),
                    '\0' => buf.push_str("\\0"),
                    c => buf.push(c),
                }
            }
            buf.push('"');
        }
        SexpValue::Symbol(s) => {
            buf.push_str(s);
        }
        SexpValue::Keyword(s) => {
            buf.push_str(s);
        }
        SexpValue::Cons(car, cdr) => {
            buf.push('(');
            serialize_into(car, buf);
            buf.push_str(" . ");
            serialize_into(cdr, buf);
            buf.push(')');
        }
        SexpValue::List(items) => {
            buf.push('(');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    buf.push(' ');
                }
                serialize_into(item, buf);
            }
            buf.push(')');
        }
        SexpValue::Quoted(inner) => {
            buf.push('\'');
            serialize_into(inner, buf);
        }
    }
}

impl fmt::Display for SexpValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", serialize(self))
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers (for building S-expressions programmatically)
// ---------------------------------------------------------------------------

impl From<i64> for SexpValue {
    fn from(n: i64) -> Self {
        SexpValue::Integer(n)
    }
}

impl From<f64> for SexpValue {
    fn from(f: f64) -> Self {
        SexpValue::Float(f)
    }
}

impl From<&str> for SexpValue {
    fn from(s: &str) -> Self {
        SexpValue::String(s.to_string())
    }
}

impl From<String> for SexpValue {
    fn from(s: String) -> Self {
        SexpValue::String(s)
    }
}

impl From<bool> for SexpValue {
    fn from(b: bool) -> Self {
        if b {
            SexpValue::Bool(true)
        } else {
            SexpValue::Nil
        }
    }
}

impl From<Vec<SexpValue>> for SexpValue {
    fn from(items: Vec<SexpValue>) -> Self {
        SexpValue::List(items)
    }
}

impl SexpValue {
    /// Create a symbol value.
    pub fn symbol(name: impl Into<String>) -> Self {
        SexpValue::Symbol(name.into())
    }

    /// Create a keyword value (with leading colon).
    pub fn keyword(name: impl Into<String>) -> Self {
        let name = name.into();
        if name.starts_with(':') {
            SexpValue::Keyword(name)
        } else {
            SexpValue::Keyword(format!(":{}", name))
        }
    }

    /// Create a quoted value.
    pub fn quoted(inner: SexpValue) -> Self {
        SexpValue::Quoted(Box::new(inner))
    }
}

// ---------------------------------------------------------------------------
// JSON ↔ Sexp conversion helpers
// ---------------------------------------------------------------------------

impl SexpValue {
    /// Convert a serde_json::Value to a SexpValue.
    ///
    /// This follows the same conventions as Python's epc_arg_transformer in reverse:
    /// - JSON object → keyword plist `(:key1 val1 :key2 val2)`
    /// - JSON array → list `(val1 val2 val3)`
    /// - JSON string → string
    /// - JSON number → integer or float
    /// - JSON bool → t or nil
    /// - JSON null → nil
    pub fn from_json(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => SexpValue::Nil,
            serde_json::Value::Bool(b) => {
                if *b {
                    SexpValue::Bool(true)
                } else {
                    SexpValue::Nil
                }
            }
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    SexpValue::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    SexpValue::Float(f)
                } else {
                    SexpValue::Nil
                }
            }
            serde_json::Value::String(s) => SexpValue::String(s.clone()),
            serde_json::Value::Array(arr) => {
                SexpValue::List(arr.iter().map(SexpValue::from_json).collect())
            }
            serde_json::Value::Object(map) => {
                let mut items = Vec::with_capacity(map.len() * 2);
                for (key, val) in map {
                    items.push(SexpValue::Keyword(format!(":{}", key)));
                    items.push(SexpValue::from_json(val));
                }
                SexpValue::List(items)
            }
        }
    }

    /// Convert a SexpValue to a serde_json::Value.
    ///
    /// This follows the same conventions as Python's epc_arg_transformer:
    /// - Keyword plist `(:key1 val1 :key2 val2)` → JSON object
    /// - Regular list `(val1 val2 val3)` → JSON array
    /// - String → JSON string
    /// - Integer → JSON number
    /// - Float → JSON number
    /// - Bool(true) / Symbol("t") → JSON true
    /// - Nil → JSON null
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            SexpValue::Nil => serde_json::Value::Null,
            SexpValue::Bool(true) => serde_json::Value::Bool(true),
            SexpValue::Bool(false) => serde_json::Value::Null,
            SexpValue::Integer(n) => serde_json::json!(*n),
            SexpValue::Float(f) => serde_json::json!(*f),
            SexpValue::String(s) => serde_json::Value::String(s.clone()),
            SexpValue::Symbol(s) if s == "t" => serde_json::Value::Bool(true),
            SexpValue::Symbol(s) if s == "nil" => serde_json::Value::Null,
            SexpValue::Symbol(s) => serde_json::Value::String(s.clone()),
            SexpValue::Keyword(s) => serde_json::Value::String(s.clone()),
            SexpValue::Quoted(inner) => inner.to_json(),
            SexpValue::Cons(car, cdr) => {
                serde_json::json!([car.to_json(), cdr.to_json()])
            }
            SexpValue::List(items) => {
                // Check if this is a keyword plist: even length, every other element is a keyword
                if is_keyword_plist(items) {
                    let mut map = serde_json::Map::new();
                    for pair in items.chunks(2) {
                        if let SexpValue::Keyword(k) = &pair[0] {
                            let key = k.strip_prefix(':').unwrap_or(k);
                            map.insert(key.to_string(), pair[1].to_json());
                        }
                    }
                    serde_json::Value::Object(map)
                } else {
                    serde_json::Value::Array(items.iter().map(|v| v.to_json()).collect())
                }
            }
        }
    }
}

/// Check if a list is a keyword plist (even length, alternating keywords and values).
fn is_keyword_plist(items: &[SexpValue]) -> bool {
    if items.is_empty() || items.len() % 2 != 0 {
        return false;
    }
    items.iter().step_by(2).all(|v| matches!(v, SexpValue::Keyword(_)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Phase 0a: S-expression parsing tests =====

    #[test]
    fn parse_integer() {
        assert_eq!(parse("42").unwrap(), SexpValue::Integer(42));
    }

    #[test]
    fn parse_negative_integer() {
        assert_eq!(parse("-7").unwrap(), SexpValue::Integer(-7));
    }

    #[test]
    fn parse_zero() {
        assert_eq!(parse("0").unwrap(), SexpValue::Integer(0));
    }

    #[test]
    fn parse_large_integer() {
        assert_eq!(
            parse("9223372036854775807").unwrap(),
            SexpValue::Integer(i64::MAX)
        );
    }

    #[test]
    fn parse_float() {
        assert_eq!(parse("3.14").unwrap(), SexpValue::Float(3.14));
    }

    #[test]
    fn parse_negative_float() {
        assert_eq!(parse("-2.5").unwrap(), SexpValue::Float(-2.5));
    }

    #[test]
    fn parse_float_scientific() {
        assert_eq!(parse("1.5e10").unwrap(), SexpValue::Float(1.5e10));
    }

    #[test]
    fn parse_float_zero() {
        assert_eq!(parse("0.0").unwrap(), SexpValue::Float(0.0));
    }

    #[test]
    fn parse_string_simple() {
        assert_eq!(
            parse(r#""hello world""#).unwrap(),
            SexpValue::String("hello world".to_string())
        );
    }

    #[test]
    fn parse_string_empty() {
        assert_eq!(
            parse(r#""""#).unwrap(),
            SexpValue::String(String::new())
        );
    }

    #[test]
    fn parse_string_with_escapes() {
        assert_eq!(
            parse(r#""line\nbreak""#).unwrap(),
            SexpValue::String("line\nbreak".to_string())
        );
    }

    #[test]
    fn parse_string_with_tab() {
        assert_eq!(
            parse(r#""col\tcol""#).unwrap(),
            SexpValue::String("col\tcol".to_string())
        );
    }

    #[test]
    fn parse_string_with_escaped_quotes() {
        assert_eq!(
            parse(r#""he said \"hi\"""#).unwrap(),
            SexpValue::String(r#"he said "hi""#.to_string())
        );
    }

    #[test]
    fn parse_string_with_backslash() {
        assert_eq!(
            parse(r#""path\\to\\file""#).unwrap(),
            SexpValue::String("path\\to\\file".to_string())
        );
    }

    #[test]
    fn parse_string_with_unicode() {
        assert_eq!(
            parse("\"测试\"").unwrap(),
            SexpValue::String("测试".to_string())
        );
    }

    #[test]
    fn parse_string_with_unicode_mixed() {
        assert_eq!(
            parse("\"hello 世界 🌍\"").unwrap(),
            SexpValue::String("hello 世界 🌍".to_string())
        );
    }

    #[test]
    fn parse_string_with_null() {
        assert_eq!(
            parse(r#""null\0byte""#).unwrap(),
            SexpValue::String("null\0byte".to_string())
        );
    }

    #[test]
    fn parse_symbol() {
        assert_eq!(
            parse("method-name").unwrap(),
            SexpValue::Symbol("method-name".to_string())
        );
    }

    #[test]
    fn parse_symbol_with_slash() {
        assert_eq!(
            parse("textDocument/completion").unwrap(),
            SexpValue::Symbol("textDocument/completion".to_string())
        );
    }

    #[test]
    fn parse_symbol_with_underscore() {
        assert_eq!(
            parse("open_file").unwrap(),
            SexpValue::Symbol("open_file".to_string())
        );
    }

    #[test]
    fn parse_keyword() {
        assert_eq!(
            parse(":key-name").unwrap(),
            SexpValue::Keyword(":key-name".to_string())
        );
    }

    #[test]
    fn parse_keyword_line() {
        assert_eq!(
            parse(":line").unwrap(),
            SexpValue::Keyword(":line".to_string())
        );
    }

    #[test]
    fn parse_nil() {
        assert_eq!(parse("nil").unwrap(), SexpValue::Nil);
    }

    #[test]
    fn parse_t() {
        assert_eq!(parse("t").unwrap(), SexpValue::Bool(true));
    }

    #[test]
    fn parse_quoted_symbol() {
        assert_eq!(
            parse("'some-symbol").unwrap(),
            SexpValue::Quoted(Box::new(SexpValue::Symbol("some-symbol".to_string())))
        );
    }

    #[test]
    fn parse_quoted_list() {
        assert_eq!(
            parse("'(1 2 3)").unwrap(),
            SexpValue::Quoted(Box::new(SexpValue::List(vec![
                SexpValue::Integer(1),
                SexpValue::Integer(2),
                SexpValue::Integer(3),
            ])))
        );
    }

    #[test]
    fn parse_empty_list() {
        assert_eq!(parse("()").unwrap(), SexpValue::List(vec![]));
    }

    #[test]
    fn parse_flat_list() {
        assert_eq!(
            parse("(1 2 3)").unwrap(),
            SexpValue::List(vec![
                SexpValue::Integer(1),
                SexpValue::Integer(2),
                SexpValue::Integer(3),
            ])
        );
    }

    #[test]
    fn parse_nested_list() {
        assert_eq!(
            parse("(a (b c) d)").unwrap(),
            SexpValue::List(vec![
                SexpValue::Symbol("a".to_string()),
                SexpValue::List(vec![
                    SexpValue::Symbol("b".to_string()),
                    SexpValue::Symbol("c".to_string()),
                ]),
                SexpValue::Symbol("d".to_string()),
            ])
        );
    }

    #[test]
    fn parse_mixed_list() {
        assert_eq!(
            parse("(1 \"two\" three)").unwrap(),
            SexpValue::List(vec![
                SexpValue::Integer(1),
                SexpValue::String("two".to_string()),
                SexpValue::Symbol("three".to_string()),
            ])
        );
    }

    #[test]
    fn parse_keyword_plist() {
        assert_eq!(
            parse("(:a 1 :b 2)").unwrap(),
            SexpValue::List(vec![
                SexpValue::Keyword(":a".to_string()),
                SexpValue::Integer(1),
                SexpValue::Keyword(":b".to_string()),
                SexpValue::Integer(2),
            ])
        );
    }

    #[test]
    fn parse_deeply_nested() {
        let input = "(a (b (c (d (e f)))))";
        let result = parse(input).unwrap();
        // Verify it parses without error and has correct structure
        if let SexpValue::List(outer) = &result {
            assert_eq!(outer.len(), 2);
            assert_eq!(outer[0], SexpValue::Symbol("a".to_string()));
            if let SexpValue::List(level2) = &outer[1] {
                assert_eq!(level2.len(), 2);
                assert_eq!(level2[0], SexpValue::Symbol("b".to_string()));
            } else {
                panic!("expected nested list");
            }
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn parse_large_string() {
        let large = "x".repeat(100_000);
        let input = format!("\"{}\"", large);
        let result = parse(&input).unwrap();
        assert_eq!(result, SexpValue::String(large));
    }

    #[test]
    fn parse_cons_cell() {
        assert_eq!(
            parse("(a . b)").unwrap(),
            SexpValue::Cons(
                Box::new(SexpValue::Symbol("a".to_string())),
                Box::new(SexpValue::Symbol("b".to_string())),
            )
        );
    }

    #[test]
    fn parse_cons_with_string() {
        assert_eq!(
            parse("(\"key\" . \"value\")").unwrap(),
            SexpValue::Cons(
                Box::new(SexpValue::String("key".to_string())),
                Box::new(SexpValue::String("value".to_string())),
            )
        );
    }

    #[test]
    fn parse_cons_with_numbers() {
        assert_eq!(
            parse("(1 . 2)").unwrap(),
            SexpValue::Cons(
                Box::new(SexpValue::Integer(1)),
                Box::new(SexpValue::Integer(2)),
            )
        );
    }

    #[test]
    fn parse_list_with_extra_whitespace() {
        assert_eq!(
            parse("(  1   2   3  )").unwrap(),
            SexpValue::List(vec![
                SexpValue::Integer(1),
                SexpValue::Integer(2),
                SexpValue::Integer(3),
            ])
        );
    }

    #[test]
    fn parse_list_with_newlines() {
        assert_eq!(
            parse("(1\n2\n3)").unwrap(),
            SexpValue::List(vec![
                SexpValue::Integer(1),
                SexpValue::Integer(2),
                SexpValue::Integer(3),
            ])
        );
    }

    #[test]
    fn parse_epc_call_message() {
        // Real EPC message format
        let input = r#"(call 42 open_file ("/tmp/test.py"))"#;
        let result = parse(input).unwrap();
        assert_eq!(
            result,
            SexpValue::List(vec![
                SexpValue::Symbol("call".to_string()),
                SexpValue::Integer(42),
                SexpValue::Symbol("open_file".to_string()),
                SexpValue::List(vec![SexpValue::String("/tmp/test.py".to_string())]),
            ])
        );
    }

    #[test]
    fn parse_epc_return_message() {
        let input = r#"(return 42 "ok")"#;
        let result = parse(input).unwrap();
        assert_eq!(
            result,
            SexpValue::List(vec![
                SexpValue::Symbol("return".to_string()),
                SexpValue::Integer(42),
                SexpValue::String("ok".to_string()),
            ])
        );
    }

    #[test]
    fn parse_eval_in_emacs_sexp() {
        // What Python sends via eval_in_emacs
        let input = r#"(lsp-bridge-completion--record-items "file.py" "localhost" ((:label "print" :kind "Function")))"#;
        let result = parse(input).unwrap();
        if let SexpValue::List(items) = &result {
            assert_eq!(items[0], SexpValue::Symbol("lsp-bridge-completion--record-items".to_string()));
            assert_eq!(items[1], SexpValue::String("file.py".to_string()));
            assert_eq!(items[2], SexpValue::String("localhost".to_string()));
            if let SexpValue::List(candidates) = &items[3] {
                assert_eq!(candidates.len(), 1);
                if let SexpValue::List(plist) = &candidates[0] {
                    assert_eq!(plist[0], SexpValue::Keyword(":label".to_string()));
                    assert_eq!(plist[1], SexpValue::String("print".to_string()));
                    assert_eq!(plist[2], SexpValue::Keyword(":kind".to_string()));
                    assert_eq!(plist[3], SexpValue::String("Function".to_string()));
                } else {
                    panic!("expected plist");
                }
            } else {
                panic!("expected candidates list");
            }
        } else {
            panic!("expected list");
        }
    }

    // ===== Serialization tests =====

    #[test]
    fn serialize_integer() {
        assert_eq!(serialize(&SexpValue::Integer(42)), "42");
    }

    #[test]
    fn serialize_negative_integer() {
        assert_eq!(serialize(&SexpValue::Integer(-7)), "-7");
    }

    #[test]
    fn serialize_float() {
        assert_eq!(serialize(&SexpValue::Float(3.14)), "3.14");
    }

    #[test]
    fn serialize_float_whole() {
        // Ensure whole numbers get .0 appended
        assert_eq!(serialize(&SexpValue::Float(5.0)), "5.0");
    }

    #[test]
    fn serialize_string() {
        assert_eq!(
            serialize(&SexpValue::String("hello".to_string())),
            "\"hello\""
        );
    }

    #[test]
    fn serialize_string_with_escapes() {
        assert_eq!(
            serialize(&SexpValue::String("line\nbreak".to_string())),
            "\"line\\nbreak\""
        );
    }

    #[test]
    fn serialize_string_with_quotes() {
        assert_eq!(
            serialize(&SexpValue::String("say \"hi\"".to_string())),
            "\"say \\\"hi\\\"\""
        );
    }

    #[test]
    fn serialize_string_with_unicode() {
        assert_eq!(
            serialize(&SexpValue::String("测试".to_string())),
            "\"测试\""
        );
    }

    #[test]
    fn serialize_symbol() {
        assert_eq!(
            serialize(&SexpValue::Symbol("method-name".to_string())),
            "method-name"
        );
    }

    #[test]
    fn serialize_keyword() {
        assert_eq!(
            serialize(&SexpValue::Keyword(":line".to_string())),
            ":line"
        );
    }

    #[test]
    fn serialize_nil() {
        assert_eq!(serialize(&SexpValue::Nil), "nil");
    }

    #[test]
    fn serialize_bool_true() {
        assert_eq!(serialize(&SexpValue::Bool(true)), "t");
    }

    #[test]
    fn serialize_bool_false() {
        assert_eq!(serialize(&SexpValue::Bool(false)), "nil");
    }

    #[test]
    fn serialize_empty_list() {
        assert_eq!(serialize(&SexpValue::List(vec![])), "()");
    }

    #[test]
    fn serialize_flat_list() {
        let list = SexpValue::List(vec![
            SexpValue::Integer(1),
            SexpValue::Integer(2),
            SexpValue::Integer(3),
        ]);
        assert_eq!(serialize(&list), "(1 2 3)");
    }

    #[test]
    fn serialize_nested_list() {
        let list = SexpValue::List(vec![
            SexpValue::Symbol("a".to_string()),
            SexpValue::List(vec![
                SexpValue::Symbol("b".to_string()),
                SexpValue::Symbol("c".to_string()),
            ]),
        ]);
        assert_eq!(serialize(&list), "(a (b c))");
    }

    #[test]
    fn serialize_cons() {
        let cons = SexpValue::Cons(
            Box::new(SexpValue::Symbol("a".to_string())),
            Box::new(SexpValue::Symbol("b".to_string())),
        );
        assert_eq!(serialize(&cons), "(a . b)");
    }

    #[test]
    fn serialize_quoted() {
        let quoted = SexpValue::Quoted(Box::new(SexpValue::Symbol("sym".to_string())));
        assert_eq!(serialize(&quoted), "'sym");
    }

    #[test]
    fn serialize_keyword_plist() {
        let plist = SexpValue::List(vec![
            SexpValue::Keyword(":line".to_string()),
            SexpValue::Integer(10),
            SexpValue::Keyword(":character".to_string()),
            SexpValue::Integer(5),
        ]);
        assert_eq!(serialize(&plist), "(:line 10 :character 5)");
    }

    // ===== Roundtrip tests =====

    fn roundtrip(input: &str) {
        let parsed = parse(input).unwrap();
        let serialized = serialize(&parsed);
        let reparsed = parse(&serialized).unwrap();
        assert_eq!(parsed, reparsed, "roundtrip failed for: {}", input);
    }

    #[test]
    fn roundtrip_integer() {
        roundtrip("42");
    }

    #[test]
    fn roundtrip_negative_integer() {
        roundtrip("-99");
    }

    #[test]
    fn roundtrip_float() {
        roundtrip("3.14");
    }

    #[test]
    fn roundtrip_string() {
        roundtrip("\"hello world\"");
    }

    #[test]
    fn roundtrip_string_with_escapes() {
        roundtrip("\"line\\nbreak\\ttab\"");
    }

    #[test]
    fn roundtrip_symbol() {
        roundtrip("method-name");
    }

    #[test]
    fn roundtrip_keyword() {
        roundtrip(":key");
    }

    #[test]
    fn roundtrip_nil() {
        roundtrip("nil");
    }

    #[test]
    fn roundtrip_t() {
        // t → Bool(true) → "t"
        let parsed = parse("t").unwrap();
        let serialized = serialize(&parsed);
        assert_eq!(serialized, "t");
    }

    #[test]
    fn roundtrip_list() {
        roundtrip("(1 2 3)");
    }

    #[test]
    fn roundtrip_nested() {
        roundtrip("(a (b c) d)");
    }

    #[test]
    fn roundtrip_plist() {
        roundtrip("(:a 1 :b 2)");
    }

    #[test]
    fn roundtrip_cons() {
        roundtrip("(a . b)");
    }

    #[test]
    fn roundtrip_quoted() {
        roundtrip("'sym");
    }

    #[test]
    fn roundtrip_complex_epc_message() {
        roundtrip(r#"(call 42 try_completion ("/tmp/test.py" 10 5 "." "os" 1))"#);
    }

    #[test]
    fn roundtrip_unicode() {
        roundtrip("\"hello 世界 🌍\"");
    }

    // ===== JSON conversion tests =====

    #[test]
    fn json_to_sexp_null() {
        assert_eq!(
            SexpValue::from_json(&serde_json::Value::Null),
            SexpValue::Nil
        );
    }

    #[test]
    fn json_to_sexp_bool() {
        assert_eq!(
            SexpValue::from_json(&serde_json::json!(true)),
            SexpValue::Bool(true)
        );
        assert_eq!(
            SexpValue::from_json(&serde_json::json!(false)),
            SexpValue::Nil
        );
    }

    #[test]
    fn json_to_sexp_number() {
        assert_eq!(
            SexpValue::from_json(&serde_json::json!(42)),
            SexpValue::Integer(42)
        );
        assert_eq!(
            SexpValue::from_json(&serde_json::json!(3.14)),
            SexpValue::Float(3.14)
        );
    }

    #[test]
    fn json_to_sexp_string() {
        assert_eq!(
            SexpValue::from_json(&serde_json::json!("hello")),
            SexpValue::String("hello".to_string())
        );
    }

    #[test]
    fn json_to_sexp_array() {
        assert_eq!(
            SexpValue::from_json(&serde_json::json!([1, 2, 3])),
            SexpValue::List(vec![
                SexpValue::Integer(1),
                SexpValue::Integer(2),
                SexpValue::Integer(3),
            ])
        );
    }

    #[test]
    fn json_to_sexp_object() {
        let result = SexpValue::from_json(&serde_json::json!({"line": 10, "character": 5}));
        // Object → keyword plist, order may vary
        if let SexpValue::List(items) = &result {
            assert_eq!(items.len(), 4);
            // Check that it contains :line 10 and :character 5
            let serialized = serialize(&result);
            assert!(serialized.contains(":line 10"));
            assert!(serialized.contains(":character 5"));
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn sexp_to_json_plist() {
        let plist = SexpValue::List(vec![
            SexpValue::Keyword(":a".to_string()),
            SexpValue::Integer(1),
            SexpValue::Keyword(":b".to_string()),
            SexpValue::Integer(2),
        ]);
        let json = plist.to_json();
        assert_eq!(json, serde_json::json!({"a": 1, "b": 2}));
    }

    #[test]
    fn sexp_to_json_plain_list() {
        let list = SexpValue::List(vec![
            SexpValue::Integer(1),
            SexpValue::Integer(2),
            SexpValue::Integer(3),
        ]);
        let json = list.to_json();
        assert_eq!(json, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn sexp_to_json_nested_plist() {
        let plist = SexpValue::List(vec![
            SexpValue::Keyword(":a".to_string()),
            SexpValue::Integer(1),
            SexpValue::Keyword(":b".to_string()),
            SexpValue::List(vec![
                SexpValue::Keyword(":c".to_string()),
                SexpValue::Integer(2),
            ]),
        ]);
        let json = plist.to_json();
        assert_eq!(json, serde_json::json!({"a": 1, "b": {"c": 2}}));
    }

    #[test]
    fn sexp_to_json_empty_list() {
        // Empty list → JSON array (not object, since we can't detect plist)
        let list = SexpValue::List(vec![]);
        let json = list.to_json();
        assert_eq!(json, serde_json::json!([]));
    }

    #[test]
    fn sexp_to_json_mixed_list() {
        let list = SexpValue::List(vec![
            SexpValue::Integer(1),
            SexpValue::Integer(2),
            SexpValue::List(vec![SexpValue::Integer(3), SexpValue::Integer(4)]),
        ]);
        let json = list.to_json();
        assert_eq!(json, serde_json::json!([1, 2, [3, 4]]));
    }

    // ===== Error handling tests =====

    #[test]
    fn parse_unterminated_string() {
        assert!(parse("\"hello").is_err());
    }

    #[test]
    fn parse_unmatched_close() {
        assert!(parse(")").is_err());
    }

    #[test]
    fn parse_empty_input() {
        assert!(parse("").is_err());
    }

    #[test]
    fn parse_whitespace_only() {
        assert!(parse("   ").is_err());
    }

    #[test]
    fn parse_unclosed_list() {
        assert!(parse("(1 2 3").is_err());
    }

    // ===== Python sexpdata parity tests =====
    // These test that our serialize() output matches Python's sexpdata.dumps()
    // Ground truth generated by running: uv run python3 -c "import sexpdata; sexpdata.dumps(...)"

    /// Helper: verify our serialize matches Python's output for a given SexpValue
    fn assert_python_parity(value: &SexpValue, expected_python: &str) {
        let our_output = serialize(value);
        assert_eq!(
            our_output, expected_python,
            "Rust serialize != Python sexpdata.dumps()\n  Rust:   {}\n  Python: {}",
            our_output, expected_python
        );
    }

    /// Helper: verify roundtrip through parse matches Python output
    fn assert_python_parse_parity(python_sexp: &str) {
        let parsed = parse(python_sexp)
            .unwrap_or_else(|e| panic!("failed to parse Python sexp {:?}: {}", python_sexp, e));
        let reserialized = serialize(&parsed);
        let reparsed = parse(&reserialized).unwrap();
        assert_eq!(
            parsed, reparsed,
            "roundtrip failed for Python sexp: {}",
            python_sexp
        );
    }

    #[test]
    fn python_parity_integer() {
        assert_python_parity(&SexpValue::Integer(42), "42");
    }

    #[test]
    fn python_parity_neg_integer() {
        assert_python_parity(&SexpValue::Integer(-7), "-7");
    }

    #[test]
    fn python_parity_float() {
        assert_python_parity(&SexpValue::Float(3.14), "3.14");
    }

    #[test]
    fn python_parity_float_zero() {
        assert_python_parity(&SexpValue::Float(0.0), "0.0");
    }

    #[test]
    fn python_parity_float_whole() {
        assert_python_parity(&SexpValue::Float(5.0), "5.0");
    }

    #[test]
    fn python_parity_string() {
        assert_python_parity(
            &SexpValue::String("hello world".to_string()),
            "\"hello world\"",
        );
    }

    #[test]
    fn python_parity_unicode() {
        assert_python_parity(&SexpValue::String("测试".to_string()), "\"测试\"");
    }

    #[test]
    fn python_parity_string_with_quotes() {
        assert_python_parity(
            &SexpValue::String("he said \"hi\"".to_string()),
            "\"he said \\\"hi\\\"\"",
        );
    }

    #[test]
    fn python_parity_string_with_newline() {
        assert_python_parity(
            &SexpValue::String("line\nbreak".to_string()),
            "\"line\\nbreak\"",
        );
    }

    #[test]
    fn python_parity_string_with_tab() {
        assert_python_parity(
            &SexpValue::String("col\tcol".to_string()),
            "\"col\\tcol\"",
        );
    }

    #[test]
    fn python_parity_string_backslash() {
        assert_python_parity(
            &SexpValue::String("C:\\Users\\test".to_string()),
            "\"C:\\\\Users\\\\test\"",
        );
    }

    #[test]
    fn python_parity_empty_string() {
        assert_python_parity(&SexpValue::String(String::new()), "\"\"");
    }

    #[test]
    fn python_parity_symbol() {
        assert_python_parity(
            &SexpValue::Symbol("method-name".to_string()),
            "method-name",
        );
    }

    #[test]
    fn python_parity_symbol_slash() {
        assert_python_parity(
            &SexpValue::Symbol("textDocument/completion".to_string()),
            "textDocument/completion",
        );
    }

    #[test]
    fn python_parity_nil() {
        // Python: sexpdata.dumps([]) == "()"
        assert_python_parity(&SexpValue::List(vec![]), "()");
    }

    #[test]
    fn python_parity_true() {
        // Python: sexpdata.dumps(True) == "t"
        assert_python_parity(&SexpValue::Bool(true), "t");
    }

    #[test]
    fn python_parity_list_ints() {
        assert_python_parity(
            &SexpValue::List(vec![
                SexpValue::Integer(1),
                SexpValue::Integer(2),
                SexpValue::Integer(3),
            ]),
            "(1 2 3)",
        );
    }

    #[test]
    fn python_parity_nested() {
        assert_python_parity(
            &SexpValue::List(vec![
                SexpValue::Symbol("a".to_string()),
                SexpValue::List(vec![
                    SexpValue::Symbol("b".to_string()),
                    SexpValue::Symbol("c".to_string()),
                ]),
                SexpValue::Symbol("d".to_string()),
            ]),
            "(a (b c) d)",
        );
    }

    #[test]
    fn python_parity_keyword_plist() {
        assert_python_parity(
            &SexpValue::List(vec![
                SexpValue::Keyword(":a".to_string()),
                SexpValue::Integer(1),
                SexpValue::Keyword(":b".to_string()),
                SexpValue::Integer(2),
            ]),
            "(:a 1 :b 2)",
        );
    }

    #[test]
    fn python_parity_quoted_sym() {
        assert_python_parity(
            &SexpValue::Quoted(Box::new(SexpValue::Symbol("sym".to_string()))),
            "'sym",
        );
    }

    #[test]
    fn python_parity_quoted_list() {
        assert_python_parity(
            &SexpValue::Quoted(Box::new(SexpValue::List(vec![
                SexpValue::Integer(1),
                SexpValue::Integer(2),
                SexpValue::Integer(3),
            ]))),
            "'(1 2 3)",
        );
    }

    // ===== Parse Python sexpdata output =====
    // Verify we can parse every sexp string that Python sexpdata.dumps() produces

    #[test]
    fn parse_python_eval_in_emacs_message() {
        assert_python_parse_parity("(message '\"[LSP-Bridge] hello\")");
    }

    #[test]
    fn parse_python_eval_in_emacs_quoted_symbol() {
        assert_python_parse_parity("(func 'python-mode)");
    }

    #[test]
    fn parse_python_eval_in_emacs_jump() {
        assert_python_parse_parity(
            "(lsp-bridge-define--jump '\"/tmp/test.py\" '\"localhost\" '10 '5)",
        );
    }

    #[test]
    fn parse_python_eval_in_emacs_completion_record() {
        assert_python_parse_parity(
            "(lsp-bridge-completion--record-items '\"file.py\" '\"localhost\" '((:label \"print\" :kind \"Function\")) '(:line 10 :character 5) '\"pyright\" '(\".\") '(\"pyright\"))",
        );
    }

    #[test]
    fn parse_python_epc_call() {
        assert_python_parse_parity("(call 42 open_file (\"/tmp/test.py\"))");
    }

    #[test]
    fn parse_python_epc_return() {
        assert_python_parse_parity("(return 42 \"ok\")");
    }

    #[test]
    fn parse_python_epc_return_error() {
        assert_python_parse_parity("(return-error 42 \"method not found\")");
    }

    #[test]
    fn parse_python_epc_error() {
        assert_python_parse_parity("(epc-error 42 \"protocol error\")");
    }

    #[test]
    fn parse_python_epc_methods() {
        assert_python_parse_parity("(methods 1)");
    }
}
