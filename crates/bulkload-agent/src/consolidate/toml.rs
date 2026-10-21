//! TOML 1.0 for the structured-config union (Codex `config.toml`).
//!
//! A parser into [`Value`] remembers where each table and value is
//! defined, and an editor writes a union into the live text as edits, so
//! its comments, order and spelling stay.
//!
//! The editor only adds and replaces, as the union does:
//! - a new key goes on a new line at the end of its table's section;
//! - a new table goes in a header block at the end of the file;
//! - new array elements go in before the closing bracket;
//! - a value the source's side won replaces that value's span only.
//!
//! What it cannot write in place, such as a header-defined table whose kind
//! the union changes, is [`Unsupported`]: the file is held, nothing written.
//! Every edit's result is parsed again and must be the union.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::value::Value;
use super::Unsupported;

/// The deepest nesting parsed; deeper is [`Unsupported`].
const DEPTH_LIMIT: usize = 128;

/// A parsed live document.
pub struct Doc<'a> {
    text: &'a str,
    pub value: Value,
    /// How each table or value not inside an array of tables is defined,
    /// by its key path.
    defs: BTreeMap<Vec<String>, Def>,
    /// Where a new `key = value` line of each section goes.
    sections: Vec<usize>,
    newline: &'static str,
}

#[derive(Debug, Clone)]
enum Def {
    /// A table with its own section: the root (0) or a `[header]`.
    Table(usize),
    /// A table only deeper headers name.
    Implicit,
    /// A table dotted keys of a section create; `prefix` is its key path
    /// from the section's table.
    Dotted { section: usize, prefix: Vec<String> },
    /// A `key = value` value, by its span.
    Value(Span),
    /// `[[header]]` entries.
    ArrayOfTables,
}

#[derive(Debug, Clone)]
struct Span {
    start: usize,
    end: usize,
    array: Option<ArraySpan>,
}

#[derive(Debug, Clone)]
struct ArraySpan {
    /// Just after `[`.
    open: usize,
    /// Just after the last element.
    last_end: Option<usize>,
    multiline: bool,
    /// The last element's indent, when it starts a line.
    indent: String,
}

/// Parse `text` as a TOML document.
///
/// # Errors
/// [`Unsupported`] naming why the document is not carried.
pub fn parse(text: &str) -> Result<Doc<'_>, Unsupported> {
    if text.starts_with('\u{feff}') {
        return Err(Unsupported("toml: byte-order mark"));
    }
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        at: 0,
    };
    let mut doc = Doc {
        text,
        value: Value::Object(Vec::new()),
        defs: BTreeMap::new(),
        sections: vec![0],
        newline: if text.contains("\r\n") { "\r\n" } else { "\n" },
    };
    doc.defs.insert(Vec::new(), Def::Table(0));
    // The current section: its table's key path, whether that path passes
    // through an array of tables, and its index.
    let mut table: Vec<String> = Vec::new();
    let mut in_array = false;
    let mut section = 0_usize;
    let mut root_seen_value = false;
    let mut first_header: Option<usize> = None;
    loop {
        parser.blank();
        let line_start = parser.at;
        match parser.peek() {
            None => break,
            Some(b'\n') => parser.at += 1,
            Some(b'\r') => parser.newline()?,
            Some(b'#') => {
                parser.comment()?;
                parser.end_of_line()?;
            }
            Some(b'[') => {
                first_header.get_or_insert(line_start);
                let aot = parser
                    .text
                    .get(parser.at..)
                    .is_some_and(|rest| rest.starts_with("[["));
                parser.at += if aot { 2 } else { 1 };
                parser.blank();
                let path = parser.key()?;
                parser.blank();
                let close = if aot { "]]" } else { "]" };
                if !parser
                    .text
                    .get(parser.at..)
                    .is_some_and(|rest| rest.starts_with(close))
                {
                    return Err(Unsupported("toml: malformed header"));
                }
                parser.at += close.len();
                parser.blank();
                if parser.peek() == Some(b'#') {
                    parser.comment()?;
                }
                parser.end_of_line()?;
                doc.sections.push(parser.at);
                section = doc.sections.len() - 1;
                in_array = doc.open_header(&path, aot, section)?;
                table = path;
            }
            Some(_) => {
                let key = parser.key()?;
                parser.blank();
                if parser.peek() != Some(b'=') {
                    return Err(Unsupported("toml: expected `=`"));
                }
                parser.at += 1;
                parser.blank();
                let start = parser.at;
                let (value, array) = parser.value(0)?;
                let span = Span {
                    start,
                    end: parser.at,
                    array,
                };
                parser.blank();
                if parser.peek() == Some(b'#') {
                    parser.comment()?;
                }
                parser.end_of_line()?;
                if let Some(insert) = doc.sections.get_mut(section) {
                    *insert = parser.at;
                }
                if section == 0 {
                    root_seen_value = true;
                }
                doc.assign(&table, in_array, section, &key, value, span)?;
            }
        }
    }
    // A root with no `key = value` takes new keys before the first header
    // (and the comment lines right above it), else at the end.
    if !root_seen_value {
        let insert = first_header.map_or(text.len(), |at| comment_block_start(text, at));
        if let Some(root) = doc.sections.get_mut(0) {
            *root = insert;
        }
    }
    Ok(doc)
}

/// The start of the run of comment lines that ends right above `at` (a
/// line start), or `at`.
fn comment_block_start(text: &str, at: usize) -> usize {
    let mut start = at;
    let head = text.get(..at).unwrap_or_default();
    let mut lines: Vec<&str> = head.split_inclusive('\n').collect();
    while let Some(line) = lines.pop() {
        if line.trim_start().starts_with('#') {
            start -= line.len();
        } else {
            break;
        }
    }
    start
}

impl Doc<'_> {
    /// Open `[path]` (or `[[path]]`): returns whether its table is inside
    /// an array of tables.
    fn open_header(
        &mut self,
        path: &[String],
        aot: bool,
        section: usize,
    ) -> Result<bool, Unsupported> {
        let (last, parents) = path.split_last().ok_or(Unsupported("toml: empty header"))?;
        let mut in_array = false;
        let mut prefix: Vec<String> = Vec::new();
        let mut node = &mut self.value;
        for key in parents {
            prefix.push(key.clone());
            if !in_array {
                match self.defs.get(&prefix) {
                    None => {
                        self.defs.insert(prefix.clone(), Def::Implicit);
                    }
                    Some(Def::Value(_)) => return Err(Unsupported("toml: header extends a value")),
                    Some(Def::ArrayOfTables) => in_array = true,
                    Some(_) => {}
                }
            }
            if let Value::Object(entries) = node {
                if !entries.iter().any(|(held, _)| held == key) {
                    entries.push((key.clone(), Value::Object(Vec::new())));
                }
            }
            node = child_table(node, key)?;
        }
        prefix.push(last.clone());
        let Value::Object(entries) = node else {
            return Err(Unsupported("toml: header under a value"));
        };
        let held = entries.iter().position(|(key, _)| key == last);
        if aot {
            match held.and_then(|index| entries.get_mut(index)) {
                Some((_, Value::Array(elements))) => {
                    if !in_array && !matches!(self.defs.get(&prefix), Some(Def::ArrayOfTables)) {
                        return Err(Unsupported("toml: `[[header]]` over a value"));
                    }
                    elements.push(Value::Object(Vec::new()));
                }
                Some(_) => return Err(Unsupported("toml: `[[header]]` over a table")),
                None => {
                    entries.push((last.clone(), Value::Array(vec![Value::Object(Vec::new())])));
                    if !in_array {
                        self.defs.insert(prefix, Def::ArrayOfTables);
                    }
                }
            }
            return Ok(true);
        }
        match held.and_then(|index| entries.get(index)) {
            Some((_, Value::Object(_))) => {
                if !in_array {
                    match self.defs.get(&prefix) {
                        Some(Def::Implicit) => {
                            self.defs.insert(prefix, Def::Table(section));
                        }
                        _ => return Err(Unsupported("toml: table defined twice")),
                    }
                }
            }
            Some(_) => return Err(Unsupported("toml: header over a value")),
            None => {
                entries.push((last.clone(), Value::Object(Vec::new())));
                if !in_array {
                    self.defs.insert(prefix, Def::Table(section));
                }
            }
        }
        Ok(in_array)
    }

    /// `key = value` in the section's table `table`.
    fn assign(
        &mut self,
        table: &[String],
        in_array: bool,
        section: usize,
        key: &[String],
        value: Value,
        span: Span,
    ) -> Result<(), Unsupported> {
        let mut node = &mut self.value;
        for step in table {
            node = child_table(node, step)?;
        }
        let (last, parents) = key.split_last().ok_or(Unsupported("toml: empty key"))?;
        let mut path: Vec<String> = table.to_vec();
        let mut dotted: Vec<String> = Vec::new();
        for step in parents {
            path.push(step.clone());
            dotted.push(step.clone());
            let Value::Object(entries) = node else {
                return Err(Unsupported("toml: dotted key under a value"));
            };
            if !entries.iter().any(|(held, _)| held == step) {
                entries.push((step.clone(), Value::Object(Vec::new())));
                if !in_array {
                    self.defs.insert(
                        path.clone(),
                        Def::Dotted {
                            section,
                            prefix: dotted.clone(),
                        },
                    );
                }
            } else if !in_array && !matches!(self.defs.get(&path), Some(Def::Dotted { .. })) {
                return Err(Unsupported("toml: dotted key extends a table"));
            }
            node = child_table(node, step)?;
        }
        let Value::Object(entries) = node else {
            return Err(Unsupported("toml: key under a value"));
        };
        if entries.iter().any(|(held, _)| held == last) {
            return Err(Unsupported("toml: duplicate key"));
        }
        entries.push((last.clone(), value));
        if !in_array {
            path.push(last.clone());
            self.defs.insert(path, Def::Value(span));
        }
        Ok(())
    }
}

/// The table `key` names under `node`: an object, or the last table of an
/// array of tables.
fn child_table<'v>(node: &'v mut Value, key: &str) -> Result<&'v mut Value, Unsupported> {
    let Value::Object(entries) = node else {
        return Err(Unsupported("toml: key under a value"));
    };
    let child = entries
        .iter_mut()
        .find(|(held, _)| held == key)
        .map(|(_, value)| value)
        .ok_or(Unsupported("toml: missing table"))?;
    match child {
        Value::Array(elements) => match elements.last_mut() {
            Some(last @ Value::Object(_)) => Ok(last),
            _ => Err(Unsupported("toml: key under an array")),
        },
        Value::Object(_) => Ok(child),
        _ => Err(Unsupported("toml: key under a value")),
    }
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn rest(&self) -> &str {
        self.text.get(self.at..).unwrap_or_default()
    }

    /// Spaces and tabs.
    fn blank(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.at += 1;
        }
    }

    fn newline(&mut self) -> Result<(), Unsupported> {
        match self.peek() {
            Some(b'\n') => {
                self.at += 1;
                Ok(())
            }
            Some(b'\r') if self.bytes.get(self.at + 1) == Some(&b'\n') => {
                self.at += 2;
                Ok(())
            }
            _ => Err(Unsupported("toml: expected a newline")),
        }
    }

    fn end_of_line(&mut self) -> Result<(), Unsupported> {
        if self.peek().is_none() {
            Ok(())
        } else {
            self.newline()
        }
    }

    fn comment(&mut self) -> Result<(), Unsupported> {
        while let Some(byte) = self.peek() {
            match byte {
                b'\n' => break,
                b'\r' if self.bytes.get(self.at + 1) == Some(&b'\n') => break,
                b'\t' => self.at += 1,
                byte if byte < 0x20 || byte == 0x7f => {
                    return Err(Unsupported("toml: control character in a comment"));
                }
                _ => self.at += 1,
            }
        }
        Ok(())
    }

    /// Whitespace, newlines and comments inside an array.
    fn array_space(&mut self) -> Result<bool, Unsupported> {
        let mut newline = false;
        loop {
            self.blank();
            match self.peek() {
                Some(b'\n' | b'\r') => {
                    self.newline()?;
                    newline = true;
                }
                Some(b'#') => self.comment()?,
                _ => return Ok(newline),
            }
        }
    }

    /// A key, dotted or not.
    fn key(&mut self) -> Result<Vec<String>, Unsupported> {
        let mut path = vec![self.simple_key()?];
        loop {
            let save = self.at;
            self.blank();
            if self.peek() == Some(b'.') {
                self.at += 1;
                self.blank();
                path.push(self.simple_key()?);
            } else {
                self.at = save;
                return Ok(path);
            }
        }
    }

    fn simple_key(&mut self) -> Result<String, Unsupported> {
        match self.peek() {
            Some(b'"') => {
                if self.rest().starts_with("\"\"\"") {
                    return Err(Unsupported("toml: multi-line key"));
                }
                self.basic_string()
            }
            Some(b'\'') => {
                if self.rest().starts_with("'''") {
                    return Err(Unsupported("toml: multi-line key"));
                }
                self.literal_string()
            }
            _ => {
                let start = self.at;
                while matches!(self.peek(), Some(byte) if bare(byte)) {
                    self.at += 1;
                }
                if self.at == start {
                    return Err(Unsupported("toml: malformed key"));
                }
                Ok(self.text.get(start..self.at).unwrap_or_default().to_owned())
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<(Value, Option<ArraySpan>), Unsupported> {
        if depth > DEPTH_LIMIT {
            return Err(Unsupported("toml: nested too deep"));
        }
        match self.peek() {
            Some(b'"') => {
                if self.rest().starts_with("\"\"\"") {
                    self.multiline_basic().map(|text| (Value::Str(text), None))
                } else {
                    self.basic_string().map(|text| (Value::Str(text), None))
                }
            }
            Some(b'\'') => {
                if self.rest().starts_with("'''") {
                    self.multiline_literal()
                        .map(|text| (Value::Str(text), None))
                } else {
                    self.literal_string().map(|text| (Value::Str(text), None))
                }
            }
            Some(b'[') => self.array(depth).map(|(value, span)| (value, Some(span))),
            Some(b'{') => self.inline_table(depth).map(|value| (value, None)),
            None | Some(b'\n' | b'\r' | b'#' | b',' | b']' | b'}') => {
                Err(Unsupported("toml: missing value"))
            }
            Some(_) => self.scalar().map(|value| (value, None)),
        }
    }

    fn array(&mut self, depth: usize) -> Result<(Value, ArraySpan), Unsupported> {
        self.at += 1;
        let open = self.at;
        let mut elements = Vec::new();
        let mut span = ArraySpan {
            open,
            last_end: None,
            multiline: false,
            indent: String::new(),
        };
        loop {
            span.multiline |= self.array_space()?;
            if self.peek() == Some(b']') {
                self.at += 1;
                return Ok((Value::Array(elements), span));
            }
            let line_start = self
                .text
                .get(..self.at)
                .and_then(|head| head.rfind('\n'))
                .map_or(0, |at| at + 1);
            let lead = self.text.get(line_start..self.at).unwrap_or_default();
            span.indent = if lead.bytes().all(|byte| byte == b' ' || byte == b'\t') {
                lead.to_owned()
            } else {
                String::new()
            };
            let (value, _) = self.value(depth + 1)?;
            elements.push(value);
            span.last_end = Some(self.at);
            span.multiline |= self.array_space()?;
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok((Value::Array(elements), span));
                }
                _ => return Err(Unsupported("toml: malformed array")),
            }
        }
    }

    fn inline_table(&mut self, depth: usize) -> Result<Value, Unsupported> {
        self.at += 1;
        let mut root = Value::Object(Vec::new());
        // Dotted keys inside it create tables only it may extend.
        let mut created: Vec<Vec<String>> = Vec::new();
        self.blank();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(root);
        }
        loop {
            self.blank();
            let key = self.key()?;
            self.blank();
            if self.peek() != Some(b'=') {
                return Err(Unsupported("toml: expected `=`"));
            }
            self.at += 1;
            self.blank();
            let (value, _) = self.value(depth + 1)?;
            insert_inline(&mut root, &key, value, &mut created)?;
            self.blank();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(root);
                }
                _ => return Err(Unsupported("toml: malformed inline table")),
            }
        }
    }

    fn scalar(&mut self) -> Result<Value, Unsupported> {
        let rest = self.rest();
        for (word, value) in [("true", Value::Bool(true)), ("false", Value::Bool(false))] {
            if rest.starts_with(word)
                && !rest
                    .as_bytes()
                    .get(word.len())
                    .is_some_and(|byte| bare(*byte))
            {
                self.at += word.len();
                return Ok(value);
            }
        }
        if let Some(length) = datetime_length(rest) {
            let token = rest.get(..length).unwrap_or_default().to_owned();
            self.at += length;
            return Ok(Value::Datetime(token));
        }
        let length = rest
            .bytes()
            .take_while(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'-' | b'.')
            })
            .count();
        let token = rest.get(..length).unwrap_or_default();
        let unsigned = token.trim_start_matches(['+', '-']);
        let value = if token.is_empty() {
            return Err(Unsupported("toml: malformed value"));
        } else if unsigned.starts_with("0x")
            || unsigned.starts_with("0o")
            || unsigned.starts_with("0b")
        {
            Value::Int(token.to_owned())
        } else if matches!(unsigned, "inf" | "nan") || token.contains(['.', 'e', 'E']) {
            Value::Float(token.to_owned())
        } else {
            Value::Int(token.to_owned())
        };
        let well_formed = match &value {
            Value::Int(token) => integer_token(token),
            Value::Float(token) => float_token(token),
            _ => false,
        };
        if !well_formed {
            return Err(Unsupported("toml: malformed value"));
        }
        self.at += length;
        Ok(value)
    }

    fn basic_string(&mut self) -> Result<String, Unsupported> {
        self.at += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None | Some(b'\n' | b'\r') => return Err(Unsupported("toml: unterminated string")),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => self.escape(&mut out)?,
                Some(byte) if (byte < 0x20 && byte != b'\t') || byte == 0x7f => {
                    return Err(Unsupported("toml: control character in a string"));
                }
                Some(_) => self.push_char(&mut out)?,
            }
        }
    }

    fn multiline_basic(&mut self) -> Result<String, Unsupported> {
        self.at += 3;
        self.trim_first_newline();
        let mut out = String::new();
        loop {
            if self.rest().starts_with("\"\"\"") {
                let quotes = self.rest().bytes().take_while(|byte| *byte == b'"').count();
                if quotes > 5 {
                    return Err(Unsupported("toml: malformed string"));
                }
                for _ in 3..quotes {
                    out.push('"');
                }
                self.at += quotes;
                return Ok(out);
            }
            match self.peek() {
                None => return Err(Unsupported("toml: unterminated string")),
                Some(b'\\') => {
                    // A line-ending backslash trims the newline and the
                    // whitespace after it.
                    let after = self.rest().get(1..).unwrap_or_default();
                    let blank = after.trim_start_matches([' ', '\t']);
                    if blank.starts_with('\n') || blank.starts_with("\r\n") {
                        self.at += 1 + (after.len() - blank.len());
                        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                            if self.peek() == Some(b'\r') {
                                self.newline()?;
                            } else {
                                self.at += 1;
                            }
                        }
                    } else {
                        self.escape(&mut out)?;
                    }
                }
                Some(b'\r') => {
                    self.newline()?;
                    out.push_str("\r\n");
                }
                Some(b'\n') => {
                    self.at += 1;
                    out.push('\n');
                }
                Some(byte) if (byte < 0x20 && byte != b'\t') || byte == 0x7f => {
                    return Err(Unsupported("toml: control character in a string"));
                }
                Some(_) => self.push_char(&mut out)?,
            }
        }
    }

    fn literal_string(&mut self) -> Result<String, Unsupported> {
        self.at += 1;
        let start = self.at;
        loop {
            match self.peek() {
                None | Some(b'\n' | b'\r') => return Err(Unsupported("toml: unterminated string")),
                Some(b'\'') => {
                    let text = self.text.get(start..self.at).unwrap_or_default().to_owned();
                    self.at += 1;
                    return Ok(text);
                }
                Some(byte) if (byte < 0x20 && byte != b'\t') || byte == 0x7f => {
                    return Err(Unsupported("toml: control character in a string"));
                }
                Some(_) => self.at += 1,
            }
        }
    }

    fn multiline_literal(&mut self) -> Result<String, Unsupported> {
        self.at += 3;
        self.trim_first_newline();
        let start = self.at;
        loop {
            if self.rest().starts_with("'''") {
                let quotes = self
                    .rest()
                    .bytes()
                    .take_while(|byte| *byte == b'\'')
                    .count();
                if quotes > 5 {
                    return Err(Unsupported("toml: malformed string"));
                }
                let end = self.at + quotes - 3;
                let text = self.text.get(start..end).unwrap_or_default().to_owned();
                self.at += quotes;
                return Ok(text);
            }
            match self.peek() {
                None => return Err(Unsupported("toml: unterminated string")),
                Some(b'\r') => self.newline()?,
                Some(byte) if (byte < 0x20 && byte != b'\t' && byte != b'\n') || byte == 0x7f => {
                    return Err(Unsupported("toml: control character in a string"));
                }
                Some(_) => self.at += 1,
            }
        }
    }

    fn trim_first_newline(&mut self) {
        if self.rest().starts_with("\r\n") {
            self.at += 2;
        } else if self.peek() == Some(b'\n') {
            self.at += 1;
        }
    }

    fn push_char(&mut self, out: &mut String) -> Result<(), Unsupported> {
        let c = self
            .rest()
            .chars()
            .next()
            .ok_or(Unsupported("toml: unterminated string"))?;
        out.push(c);
        self.at += c.len_utf8();
        Ok(())
    }

    fn escape(&mut self, out: &mut String) -> Result<(), Unsupported> {
        self.at += 1;
        let escape = self.peek().ok_or(Unsupported("toml: malformed escape"))?;
        self.at += 1;
        match escape {
            b'b' => out.push('\u{8}'),
            b't' => out.push('\t'),
            b'n' => out.push('\n'),
            b'f' => out.push('\u{c}'),
            b'r' => out.push('\r'),
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'u' | b'U' => {
                let width = if escape == b'u' { 4 } else { 8 };
                let digits = self
                    .text
                    .get(self.at..self.at + width)
                    .ok_or(Unsupported("toml: malformed escape"))?;
                if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(Unsupported("toml: malformed escape"));
                }
                let code = u32::from_str_radix(digits, 16)
                    .map_err(|_| Unsupported("toml: malformed escape"))?;
                out.push(char::from_u32(code).ok_or(Unsupported("toml: malformed escape"))?);
                self.at += width;
            }
            _ => return Err(Unsupported("toml: malformed escape")),
        }
        Ok(())
    }
}

/// A key byte that needs no quotes.
const fn bare(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// The length of the date-time, date or time token `text` starts with.
fn datetime_length(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let digits = |from: usize, count: usize| {
        (from..from + count).all(|at| bytes.get(at).is_some_and(u8::is_ascii_digit))
    };
    let time = |from: usize| -> Option<usize> {
        if !(digits(from, 2) && bytes.get(from + 2) == Some(&b':') && digits(from + 3, 2)) {
            return None;
        }
        let mut at = from + 5;
        if bytes.get(at) == Some(&b':') && digits(at + 1, 2) {
            at += 3;
            if bytes.get(at) == Some(&b'.') && digits(at + 1, 1) {
                at += 1;
                while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                    at += 1;
                }
            }
        }
        Some(at)
    };
    if digits(0, 4)
        && bytes.get(4) == Some(&b'-')
        && digits(5, 2)
        && bytes.get(7) == Some(&b'-')
        && digits(8, 2)
    {
        let mut at = 10;
        if matches!(bytes.get(at), Some(b'T' | b't' | b' ')) && digits(at + 1, 2) {
            if let Some(end) = time(at + 1) {
                at = end;
                match bytes.get(at) {
                    Some(b'Z' | b'z') => at += 1,
                    Some(b'+' | b'-')
                        if digits(at + 1, 2)
                            && bytes.get(at + 3) == Some(&b':')
                            && digits(at + 4, 2) =>
                    {
                        at += 6;
                    }
                    _ => {}
                }
            }
        }
        return Some(at);
    }
    time(0)
}

fn integer_token(token: &str) -> bool {
    let unsigned = token.trim_start_matches(['+', '-']);
    if unsigned.len() + 1 < token.len() {
        return false;
    }
    let (digits, radix) = if let Some(hex) = unsigned.strip_prefix("0x") {
        (hex, 16)
    } else if let Some(octal) = unsigned.strip_prefix("0o") {
        (octal, 8)
    } else if let Some(binary) = unsigned.strip_prefix("0b") {
        (binary, 2)
    } else {
        if unsigned.len() > 1 && unsigned.starts_with('0') {
            return false;
        }
        (unsigned, 10)
    };
    if radix != 10 && unsigned.len() != token.len() {
        return false;
    }
    underscores_ok(digits) && i128::from_str_radix(&digits.replace('_', ""), radix).is_ok()
}

fn float_token(token: &str) -> bool {
    let unsigned = token.trim_start_matches(['+', '-']);
    if unsigned.len() + 1 < token.len() {
        return false;
    }
    if matches!(unsigned, "inf" | "nan") {
        return true;
    }
    let (mantissa, exponent) = unsigned.find(['e', 'E']).map_or((unsigned, None), |at| {
        (
            unsigned.get(..at).unwrap_or_default(),
            unsigned.get(at + 1..),
        )
    });
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (mantissa, None),
    };
    let decimal = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'_')
            && underscores_ok(part)
    };
    if !decimal(whole) || (whole.len() > 1 && whole.starts_with('0')) {
        return false;
    }
    if fraction.is_some_and(|fraction| !decimal(fraction)) {
        return false;
    }
    if let Some(exponent) = exponent {
        if !decimal(exponent.trim_start_matches(['+', '-']))
            || exponent.len() > exponent.trim_start_matches(['+', '-']).len() + 1
        {
            return false;
        }
    }
    unsigned.replace('_', "").parse::<f64>().is_ok()
}

/// `_` only between digits.
fn underscores_ok(digits: &str) -> bool {
    !digits.is_empty()
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
}

/// `key = value` inside an inline table.
fn insert_inline(
    root: &mut Value,
    key: &[String],
    value: Value,
    created: &mut Vec<Vec<String>>,
) -> Result<(), Unsupported> {
    let (last, parents) = key.split_last().ok_or(Unsupported("toml: empty key"))?;
    let mut node = root;
    let mut path: Vec<String> = Vec::new();
    for step in parents {
        path.push(step.clone());
        let Value::Object(entries) = node else {
            return Err(Unsupported("toml: dotted key under a value"));
        };
        let index = if let Some(index) = entries.iter().position(|(held, _)| held == step) {
            if !created.contains(&path) {
                return Err(Unsupported("toml: dotted key extends a value"));
            }
            index
        } else {
            entries.push((step.clone(), Value::Object(Vec::new())));
            created.push(path.clone());
            entries.len() - 1
        };
        node = &mut entries
            .get_mut(index)
            .ok_or(Unsupported("toml: malformed inline table"))?
            .1;
    }
    let Value::Object(entries) = node else {
        return Err(Unsupported("toml: key under a value"));
    };
    if entries.iter().any(|(held, _)| held == last) {
        return Err(Unsupported("toml: duplicate key"));
    }
    entries.push((last.clone(), value));
    Ok(())
}

/// A key as TOML writes it: bare when it can be.
fn key_text(out: &mut String, key: &str) {
    if !key.is_empty() && key.bytes().all(bare) {
        out.push_str(key);
    } else {
        string(out, key);
    }
}

fn dotted(out: &mut String, path: &[String]) {
    for (index, key) in path.iter().enumerate() {
        if index > 0 {
            out.push('.');
        }
        key_text(out, key);
    }
}

/// A basic string.
fn string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if u32::from(c) < 0x20 || u32::from(c) == 0x7f => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `value` written inline (one line).
///
/// # Errors
/// [`Unsupported`] for a JSON `null`, which TOML has no spelling of.
pub fn inline(out: &mut String, value: &Value) -> Result<(), Unsupported> {
    match value {
        Value::Str(text) => string(out, text),
        Value::Int(token) | Value::Float(token) | Value::Datetime(token) => out.push_str(token),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Null => return Err(Unsupported("toml: null")),
        Value::Array(elements) => {
            out.push('[');
            for (index, element) in elements.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                inline(out, element)?;
            }
            out.push(']');
        }
        Value::Object(entries) if entries.is_empty() => out.push_str("{}"),
        Value::Object(entries) => {
            out.push_str("{ ");
            for (index, (key, element)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                key_text(out, key);
                out.push_str(" = ");
                inline(out, element)?;
            }
            out.push_str(" }");
        }
    }
    Ok(())
}

/// Written as `[[header]]` blocks: a non-empty array of tables only.
fn table_array(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Array(elements)
            if !elements.is_empty()
                && elements
                    .iter()
                    .all(|element| matches!(element, Value::Object(_))) =>
        {
            Some(elements)
        }
        _ => None,
    }
}

/// One edit of the live text.
struct Edit {
    at: usize,
    remove: usize,
    text: String,
    /// A whole line, which needs a newline before it at a last line that
    /// has none.
    line: bool,
}

struct Editor<'d, 'a> {
    doc: &'d Doc<'a>,
    edits: Vec<Edit>,
    /// Header blocks appended at the end of the file.
    blocks: Vec<String>,
    /// New `key = value` lines of implicit tables, by table.
    implicit: BTreeMap<Vec<String>, Vec<String>>,
}

impl Doc<'_> {
    /// The live text with `union` (the union of this document's value and
    /// the source's) written into it.
    ///
    /// # Errors
    /// [`Unsupported`] when the union cannot be written as edits, or the
    /// result does not parse back as `union`.
    pub fn apply(&self, union: &Value) -> Result<String, Unsupported> {
        let mut editor = Editor {
            doc: self,
            edits: Vec::new(),
            blocks: Vec::new(),
            implicit: BTreeMap::new(),
        };
        let mut path = Vec::new();
        editor.table(&mut path, &self.value, union)?;
        let text = editor.finish()?;
        let parsed = parse(&text)?;
        if !parsed.value.same(union) {
            return Err(Unsupported(
                "toml: the edit does not read back as the union",
            ));
        }
        Ok(text)
    }
}

impl Editor<'_, '_> {
    fn def(&self, path: &[String]) -> Option<&Def> {
        self.doc.defs.get(path)
    }

    fn table(
        &mut self,
        path: &mut Vec<String>,
        live: &Value,
        merged: &Value,
    ) -> Result<(), Unsupported> {
        let Value::Object(entries) = merged else {
            return Err(Unsupported("toml: the union of a table is no table"));
        };
        for (key, value) in entries {
            match live.get(key) {
                None => self.add(path, key, value)?,
                Some(old) if old.same(value) => {}
                Some(old) => {
                    path.push(key.clone());
                    let changed = self.change(path, old, value);
                    path.pop();
                    changed?;
                }
            }
        }
        Ok(())
    }

    fn change(
        &mut self,
        path: &mut Vec<String>,
        old: &Value,
        new: &Value,
    ) -> Result<(), Unsupported> {
        let def = self.def(path).cloned();
        match (old, new, def) {
            (
                Value::Object(_),
                Value::Object(_),
                Some(Def::Table(_) | Def::Implicit | Def::Dotted { .. }),
            ) => self.table(path, old, new),
            (Value::Array(held), Value::Array(all), Some(Def::Value(span)))
                if prefix_of(held, all) && span.array.is_some() =>
            {
                let added = all.get(held.len()..).unwrap_or_default();
                self.append_elements(&span, added)
            }
            (Value::Array(held), Value::Array(all), Some(Def::ArrayOfTables))
                if prefix_of(held, all) =>
            {
                for element in all.get(held.len()..).unwrap_or_default() {
                    let Value::Object(_) = element else {
                        return Err(Unsupported("toml: a value joins an array of tables"));
                    };
                    let mut block = String::new();
                    self.emit_table(&mut block, path, element, true)?;
                    self.blocks.push(block);
                }
                Ok(())
            }
            (_, _, Some(Def::Value(span))) => {
                let mut text = String::new();
                inline(&mut text, new)?;
                self.edits.push(Edit {
                    at: span.start,
                    remove: span.end - span.start,
                    text,
                    line: false,
                });
                Ok(())
            }
            _ => Err(Unsupported("toml: a table defined by headers changes kind")),
        }
    }

    fn append_elements(&mut self, span: &Span, added: &[Value]) -> Result<(), Unsupported> {
        let array = span.array.as_ref().ok_or(Unsupported("toml: no array"))?;
        let mut text = String::new();
        match array.last_end {
            None => {
                for (index, element) in added.iter().enumerate() {
                    if index > 0 {
                        text.push_str(", ");
                    }
                    inline(&mut text, element)?;
                }
                self.edits.push(Edit {
                    at: array.open,
                    remove: 0,
                    text,
                    line: false,
                });
            }
            Some(last) => {
                for element in added {
                    text.push(',');
                    if array.multiline {
                        text.push_str(self.doc.newline);
                        text.push_str(&array.indent);
                    } else {
                        text.push(' ');
                    }
                    inline(&mut text, element)?;
                }
                self.edits.push(Edit {
                    at: last,
                    remove: 0,
                    text,
                    line: false,
                });
            }
        }
        Ok(())
    }

    fn add(&mut self, path: &mut Vec<String>, key: &str, value: &Value) -> Result<(), Unsupported> {
        let def = self.def(path).cloned();
        path.push(key.to_owned());
        let added = self.add_at(path, def, key, value);
        path.pop();
        added
    }

    fn add_at(
        &mut self,
        path: &[String],
        parent: Option<Def>,
        key: &str,
        value: &Value,
    ) -> Result<(), Unsupported> {
        let parents = path.get(..path.len().saturating_sub(1)).unwrap_or_default();
        match parent {
            Some(Def::Dotted { section, prefix }) => {
                let mut line = String::new();
                let mut full = prefix;
                full.push(key.to_owned());
                dotted(&mut line, &full);
                line.push_str(" = ");
                inline(&mut line, value)?;
                line.push_str(self.doc.newline);
                self.insert_line(section, line)
            }
            Some(parent @ (Def::Table(_) | Def::Implicit)) => {
                if let Value::Object(_) = value {
                    let mut block = String::new();
                    self.emit_table(&mut block, path, value, false)?;
                    self.blocks.push(block);
                    return Ok(());
                }
                if let Some(elements) = table_array(value) {
                    for element in elements {
                        let mut block = String::new();
                        self.emit_table(&mut block, path, element, true)?;
                        self.blocks.push(block);
                    }
                    return Ok(());
                }
                let mut line = String::new();
                key_text(&mut line, key);
                line.push_str(" = ");
                inline(&mut line, value)?;
                line.push_str(self.doc.newline);
                if let Def::Table(section) = parent {
                    self.insert_line(section, line)
                } else {
                    self.implicit
                        .entry(parents.to_vec())
                        .or_default()
                        .push(line);
                    Ok(())
                }
            }
            _ => Err(Unsupported("toml: a key joins a value")),
        }
    }

    fn insert_line(&mut self, section: usize, text: String) -> Result<(), Unsupported> {
        let at = *self
            .doc
            .sections
            .get(section)
            .ok_or(Unsupported("toml: no section"))?;
        self.edits.push(Edit {
            at,
            remove: 0,
            text,
            line: true,
        });
        Ok(())
    }

    /// `[path]` (or `[[path]]`) and its body: plain keys first, then its
    /// tables and arrays of tables as headers of their own.
    fn emit_table(
        &self,
        out: &mut String,
        path: &[String],
        value: &Value,
        aot: bool,
    ) -> Result<(), Unsupported> {
        let Value::Object(entries) = value else {
            return Err(Unsupported("toml: a header for a value"));
        };
        let newline = self.doc.newline;
        out.push_str(if aot { "[[" } else { "[" });
        dotted(out, path);
        out.push_str(if aot { "]]" } else { "]" });
        out.push_str(newline);
        for (key, element) in entries {
            if matches!(element, Value::Object(_)) || table_array(element).is_some() {
                continue;
            }
            key_text(out, key);
            out.push_str(" = ");
            inline(out, element)?;
            out.push_str(newline);
        }
        for (key, element) in entries {
            let mut child = path.to_vec();
            child.push(key.clone());
            if let Value::Object(_) = element {
                out.push_str(newline);
                self.emit_table(out, &child, element, false)?;
            } else if let Some(elements) = table_array(element) {
                for element in elements {
                    out.push_str(newline);
                    self.emit_table(out, &child, element, true)?;
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<String, Unsupported> {
        for (path, lines) in std::mem::take(&mut self.implicit) {
            let mut block = String::from("[");
            dotted(&mut block, &path);
            block.push(']');
            block.push_str(self.doc.newline);
            for line in lines {
                block.push_str(&line);
            }
            self.blocks.push(block);
        }
        let text = self.doc.text;
        let newline = self.doc.newline;
        let mut edits = self.edits;
        edits.sort_by_key(|edit| edit.at);
        let mut out = String::with_capacity(text.len() + 256);
        let mut cursor = 0;
        for edit in edits {
            if edit.at < cursor {
                return Err(Unsupported("toml: overlapping edits"));
            }
            out.push_str(
                text.get(cursor..edit.at)
                    .ok_or(Unsupported("toml: edit off a boundary"))?,
            );
            if edit.line
                && edit.at == text.len()
                && !text.is_empty()
                && !text.ends_with('\n')
                && !out.ends_with('\n')
            {
                out.push_str(newline);
            }
            out.push_str(&edit.text);
            cursor = edit.at + edit.remove;
        }
        out.push_str(
            text.get(cursor..)
                .ok_or(Unsupported("toml: edit off a boundary"))?,
        );
        for block in self.blocks {
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push_str(newline);
                }
                out.push_str(newline);
            }
            out.push_str(&block);
        }
        Ok(out)
    }
}

/// Whether `held` is the start of `all`.
fn prefix_of(held: &[Value], all: &[Value]) -> bool {
    held.len() <= all.len() && held.iter().zip(all).all(|(a, b)| a.same(b))
}
