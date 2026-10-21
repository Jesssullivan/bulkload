//! JSON (RFC 8259) for the structured-config union: a strict parser into
//! [`Value`] and a writer in the live file's style.
//!
//! The agent has no JSON parser on its dependency allowlist (R34), so this
//! is its own. It refuses what it cannot carry losslessly: a duplicate key
//! (a reader keeps one of them), a byte-order mark, comments, trailing
//! commas, a lone surrogate, nesting past [`DEPTH_LIMIT`]. The writer
//! writes what `JSON.stringify(value, null, indent)` writes (Claude Code's
//! own files), with the live file's indent, newline and spacing, so an
//! unchanged document is written back byte for byte.

use std::fmt::Write as _;

use super::value::Value;
use super::Unsupported;

/// The deepest nesting parsed; deeper is [`Unsupported`].
const DEPTH_LIMIT: usize = 256;

/// Parse `text` as one JSON document.
///
/// # Errors
/// [`Unsupported`] naming why the document is not carried.
pub fn parse(text: &str) -> Result<Value, Unsupported> {
    if text.starts_with('\u{feff}') {
        return Err(Unsupported("json: byte-order mark"));
    }
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        at: 0,
    };
    parser.space();
    let value = parser.value(0)?;
    parser.space();
    if parser.at != parser.bytes.len() {
        return Err(Unsupported("json: bytes after the document"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Result<(), Unsupported> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(Unsupported("json: malformed"))
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, Unsupported> {
        if self
            .text
            .get(self.at..)
            .is_some_and(|rest| rest.starts_with(word))
        {
            self.at += word.len();
            Ok(value)
        } else {
            Err(Unsupported("json: malformed"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Unsupported> {
        if depth > DEPTH_LIMIT {
            return Err(Unsupported("json: nested too deep"));
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(Unsupported("json: malformed")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, Unsupported> {
        self.eat(b'{')?;
        let mut entries: Vec<(String, Value)> = Vec::new();
        self.space();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Value::Object(entries));
        }
        loop {
            self.space();
            let key = self.string()?;
            if entries.iter().any(|(held, _)| *held == key) {
                return Err(Unsupported("json: duplicate key"));
            }
            self.space();
            self.eat(b':')?;
            self.space();
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(entries));
                }
                _ => return Err(Unsupported("json: malformed")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, Unsupported> {
        self.eat(b'[')?;
        let mut elements = Vec::new();
        self.space();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Value::Array(elements));
        }
        loop {
            self.space();
            elements.push(self.value(depth + 1)?);
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Array(elements));
                }
                _ => return Err(Unsupported("json: malformed")),
            }
        }
    }

    fn number(&mut self) -> Result<Value, Unsupported> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(Unsupported("json: malformed number")),
        }
        let mut float = false;
        if self.peek() == Some(b'.') {
            float = true;
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Unsupported("json: malformed number"));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            float = true;
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Unsupported("json: malformed number"));
            }
            self.digits();
        }
        let token = self
            .text
            .get(start..self.at)
            .ok_or(Unsupported("json: malformed number"))?
            .to_owned();
        Ok(if float {
            Value::Float(token)
        } else {
            Value::Int(token)
        })
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }

    fn hex4(&mut self) -> Result<u32, Unsupported> {
        let digits = self
            .text
            .get(self.at..self.at + 4)
            .ok_or(Unsupported("json: malformed escape"))?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Unsupported("json: malformed escape"));
        }
        self.at += 4;
        u32::from_str_radix(digits, 16).map_err(|_| Unsupported("json: malformed escape"))
    }

    fn string(&mut self) -> Result<String, Unsupported> {
        self.eat(b'"')?;
        let mut out = String::new();
        let mut run = self.at;
        loop {
            match self.peek() {
                None => return Err(Unsupported("json: unterminated string")),
                Some(b'"') => {
                    out.push_str(self.text.get(run..self.at).unwrap_or_default());
                    self.at += 1;
                    return Ok(out);
                }
                Some(byte) if byte < 0x20 => {
                    return Err(Unsupported("json: control character in a string"));
                }
                Some(b'\\') => {
                    out.push_str(self.text.get(run..self.at).unwrap_or_default());
                    self.at += 1;
                    let escape = self.peek().ok_or(Unsupported("json: malformed escape"))?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let high = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&high) {
                                if self.text.get(self.at..self.at + 2) != Some("\\u") {
                                    return Err(Unsupported("json: lone surrogate"));
                                }
                                self.at += 2;
                                let low = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&low) {
                                    return Err(Unsupported("json: lone surrogate"));
                                }
                                0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                            } else {
                                high
                            };
                            out.push(
                                char::from_u32(code).ok_or(Unsupported("json: lone surrogate"))?,
                            );
                        }
                        _ => return Err(Unsupported("json: malformed escape")),
                    }
                    run = self.at;
                }
                Some(_) => self.at += 1,
            }
        }
    }
}

/// How the live file is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Style {
    /// One level's indent; `None` for a document on one line.
    indent: Option<String>,
    /// A space after `:`.
    colon_space: bool,
    /// A space after `,` (one-line documents).
    comma_space: bool,
    newline: &'static str,
    /// A newline after the document.
    trailing_newline: bool,
}

impl Style {
    /// The style of `text`, a document [`parse`] accepted.
    #[must_use]
    pub fn of(text: &str) -> Self {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing_newline = text.ends_with('\n');
        let body = text.trim_end();
        let indent = body.find('\n').and_then(|at| {
            let rest = body.get(at + 1..)?;
            let run: String = rest
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            (!run.is_empty()).then_some(run)
        });
        let (colon_space, comma_space) = punctuation_spacing(body);
        // With no `:` to read it from, as `JSON.stringify` would write it.
        let colon_space = colon_space.unwrap_or_else(|| indent.is_some());
        Self {
            indent,
            colon_space,
            // A one-line document with no `,` yet: spaced as its `:` is.
            comma_space: comma_space.unwrap_or(colon_space),
            newline,
            trailing_newline,
        }
    }

    /// `JSON.stringify(value, null, 2)` followed by a newline: for a file
    /// the destination does not hold.
    #[must_use]
    pub fn pretty() -> Self {
        Self {
            indent: Some("  ".to_owned()),
            colon_space: true,
            comma_space: false,
            newline: "\n",
            trailing_newline: true,
        }
    }
}

/// Whether the first `:` and `,` outside a string are followed by a space.
fn punctuation_spacing(body: &str) -> (Option<bool>, Option<bool>) {
    let bytes = body.as_bytes();
    let (mut colon, mut comma) = (None, None);
    let mut in_string = false;
    let mut escape = false;
    for (at, byte) in bytes.iter().enumerate() {
        if in_string {
            match (escape, *byte) {
                (true, _) => escape = false,
                (false, b'\\') => escape = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        let next_space = bytes.get(at + 1) == Some(&b' ');
        match *byte {
            b'"' => in_string = true,
            b':' if colon.is_none() => colon = Some(next_space),
            b',' if comma.is_none() => comma = Some(next_space),
            _ => {}
        }
        if colon.is_some() && comma.is_some() {
            break;
        }
    }
    (colon, comma)
}

/// `value` written in `style`.
#[must_use]
pub fn write(value: &Value, style: &Style) -> String {
    let mut out = String::new();
    write_value(&mut out, value, style, 0);
    if style.trailing_newline {
        out.push_str(style.newline);
    }
    out
}

fn line(out: &mut String, style: &Style, depth: usize) {
    if let Some(indent) = &style.indent {
        out.push_str(style.newline);
        for _ in 0..depth {
            out.push_str(indent);
        }
    }
}

fn write_value(out: &mut String, value: &Value, style: &Style, depth: usize) {
    match value {
        Value::Object(entries) if entries.is_empty() => out.push_str("{}"),
        Value::Array(elements) if elements.is_empty() => out.push_str("[]"),
        Value::Object(entries) => {
            out.push('{');
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    separator(out, style);
                }
                line(out, style, depth + 1);
                string(out, key);
                out.push(':');
                if style.colon_space {
                    out.push(' ');
                }
                write_value(out, value, style, depth + 1);
            }
            line(out, style, depth);
            out.push('}');
        }
        Value::Array(elements) => {
            out.push('[');
            for (index, value) in elements.iter().enumerate() {
                if index > 0 {
                    separator(out, style);
                }
                line(out, style, depth + 1);
                write_value(out, value, style, depth + 1);
            }
            line(out, style, depth);
            out.push(']');
        }
        Value::Str(text) => string(out, text),
        Value::Int(token) | Value::Float(token) | Value::Datetime(token) => out.push_str(token),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Null => out.push_str("null"),
    }
}

fn separator(out: &mut String, style: &Style) {
    out.push(',');
    if style.indent.is_none() && style.comma_space {
        out.push(' ');
    }
}

/// A JSON string as `JSON.stringify` writes it.
pub fn string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `value` as one line of compact JSON: how the ledger records a value.
#[must_use]
pub fn compact(value: &Value) -> String {
    let style = Style {
        indent: None,
        colon_space: false,
        comma_space: false,
        newline: "\n",
        trailing_newline: false,
    };
    write(value, &style)
}
