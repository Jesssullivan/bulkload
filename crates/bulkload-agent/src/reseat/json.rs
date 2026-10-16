//! A streaming scanner for top-level string fields of JSON Lines (R0,
//! TIN-6587).
//!
//! A transcript line can be megabytes (a tool result inside it), and its
//! `cwd` field comes after such values, so the scanner never holds a line:
//! it follows the bytes with a nesting depth and a string state, and keeps
//! only the top-level key it is in and, for a wanted key, its string value
//! (bounded). A raw newline always ends a line, since JSON escapes every
//! newline inside a string, so one malformed line cannot confuse the next.
//! The agent has no JSON parser on its dependency allowlist (R34), and it
//! needs none for this.

/// The longest top-level key the scanner compares.
const KEY_LIMIT: usize = 64;
/// The longest value it keeps; a longer one is not a path or a slug.
const VALUE_LIMIT: usize = 16 * 1024;

/// What the scanner is inside.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Between tokens.
    Outside,
    /// In a string; `escape` after a backslash.
    String { escape: bool },
}

/// Finds the first value of each wanted top-level string field, across
/// every line fed to it.
pub struct FieldScanner {
    wanted: &'static [&'static str],
    found: Vec<Option<String>>,
    depth: u32,
    mode: Mode,
    /// At depth 1, the next string is a key.
    expect_key: bool,
    /// The bytes of the string being read: a key at depth 1, or a wanted
    /// key's value.
    buffer: Vec<u8>,
    /// Whether `buffer` is collecting (a key, or a wanted value), and which.
    collecting: Collect,
    /// The last top-level key read on this line.
    key: Vec<u8>,
    /// Skip to the next newline (a scan that starts mid-line).
    skip_line: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Collect {
    Nothing,
    Key,
    Value(usize),
    /// A value past [`VALUE_LIMIT`]: dropped.
    Overflow,
}

impl FieldScanner {
    /// A scanner for `wanted` fields, at the start of a line.
    #[must_use]
    pub fn new(wanted: &'static [&'static str]) -> Self {
        Self {
            wanted,
            found: vec![None; wanted.len()],
            depth: 0,
            mode: Mode::Outside,
            expect_key: false,
            buffer: Vec::new(),
            collecting: Collect::Nothing,
            key: Vec::new(),
            skip_line: false,
        }
    }

    /// A scanner that ignores bytes up to the first newline: for a feed
    /// that starts after a byte that was not a newline.
    #[must_use]
    pub fn mid_line(wanted: &'static [&'static str]) -> Self {
        let mut scanner = Self::new(wanted);
        scanner.skip_line = true;
        scanner
    }

    /// The first value of wanted field `index`, if one was seen.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&str> {
        self.found.get(index).and_then(Option::as_deref)
    }

    /// Whether every wanted field has a value.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.found.iter().all(Option::is_some)
    }

    /// Follow `bytes`.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.step(byte);
        }
    }

    fn reset_line(&mut self) {
        self.depth = 0;
        self.mode = Mode::Outside;
        self.expect_key = false;
        self.buffer.clear();
        self.collecting = Collect::Nothing;
        self.key.clear();
        self.skip_line = false;
    }

    fn step(&mut self, byte: u8) {
        if byte == b'\n' {
            self.reset_line();
            return;
        }
        if self.skip_line {
            return;
        }
        match self.mode {
            Mode::String { escape } => self.in_string(byte, escape),
            Mode::Outside => self.outside(byte),
        }
    }

    fn in_string(&mut self, byte: u8, escape: bool) {
        if !escape && byte == b'"' {
            self.mode = Mode::Outside;
            self.end_string();
            return;
        }
        self.mode = Mode::String {
            escape: !escape && byte == b'\\',
        };
        match self.collecting {
            Collect::Key if self.buffer.len() < KEY_LIMIT => self.buffer.push(byte),
            Collect::Key => self.collecting = Collect::Nothing,
            Collect::Value(_) if self.buffer.len() < VALUE_LIMIT => self.buffer.push(byte),
            Collect::Value(_) => {
                self.collecting = Collect::Overflow;
                self.buffer.clear();
            }
            Collect::Nothing | Collect::Overflow => {}
        }
    }

    fn end_string(&mut self) {
        match self.collecting {
            Collect::Key => {
                self.key = core::mem::take(&mut self.buffer);
                self.expect_key = false;
            }
            Collect::Value(index) => {
                if let Some(slot) = self.found.get_mut(index) {
                    if slot.is_none() {
                        *slot = unescape(&self.buffer);
                    }
                }
                self.buffer.clear();
            }
            Collect::Nothing | Collect::Overflow => {}
        }
        self.collecting = Collect::Nothing;
    }

    fn outside(&mut self, byte: u8) {
        match byte {
            b'"' => {
                self.mode = Mode::String { escape: false };
                self.buffer.clear();
                self.collecting = if self.depth != 1 {
                    Collect::Nothing
                } else if self.expect_key {
                    Collect::Key
                } else {
                    self.wanted
                        .iter()
                        .position(|wanted| wanted.as_bytes() == self.key.as_slice())
                        .filter(|index| self.found.get(*index).is_some_and(Option::is_none))
                        .map_or(Collect::Nothing, Collect::Value)
                };
            }
            b'{' | b'[' => {
                self.depth = self.depth.saturating_add(1);
                if self.depth == 1 {
                    self.expect_key = byte == b'{';
                }
            }
            b'}' | b']' => self.depth = self.depth.saturating_sub(1),
            b',' if self.depth == 1 => {
                self.expect_key = true;
                self.key.clear();
            }
            _ => {}
        }
    }
}

/// The string value at the object key path `path` (`["payload", "cwd"]`)
/// in one JSON line, or `None` when there is none (or it is not a
/// string). Keys are compared as written, unescaped values returned.
#[must_use]
pub fn nested_string(line: &[u8], path: &[&str]) -> Option<String> {
    // One entry per open container: (is an object, its current key, a key
    // is expected next).
    let mut stack: Vec<(bool, Vec<u8>, bool)> = Vec::new();
    let mut at = 0;
    while let Some(&byte) = line.get(at) {
        match byte {
            b'{' => stack.push((true, Vec::new(), true)),
            b'[' => stack.push((false, Vec::new(), false)),
            b'}' | b']' => {
                stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut().filter(|top| top.0) {
                    top.2 = true;
                }
            }
            b':' => {
                if let Some(top) = stack.last_mut().filter(|top| top.0) {
                    top.2 = false;
                }
            }
            b'"' => {
                let start = at + 1;
                let mut end = start;
                let mut escape = false;
                while let Some(&inner) = line.get(end) {
                    if escape {
                        escape = false;
                    } else if inner == b'\\' {
                        escape = true;
                    } else if inner == b'"' {
                        break;
                    }
                    end += 1;
                }
                let raw = line.get(start..end)?;
                let at_key = stack.last().is_some_and(|top| top.0 && top.2);
                if at_key {
                    if let Some(top) = stack.last_mut() {
                        top.1 = raw.to_vec();
                    }
                } else if stack.len() == path.len()
                    && stack
                        .iter()
                        .zip(path)
                        .all(|((object, key, _), want)| *object && key == want.as_bytes())
                {
                    return unescape(raw);
                }
                at = end;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// The raw bytes of the value at the object key path `path` in one JSON
/// line (a string with its quotes, an object or array with its brackets,
/// or a scalar), or `None` when there is none.
#[must_use]
pub fn nested_raw<'a>(line: &'a [u8], path: &[&str]) -> Option<&'a [u8]> {
    // One entry per open container: (is an object, its current key, a key
    // is expected next).
    let mut stack: Vec<(bool, Vec<u8>, bool)> = Vec::new();
    let mut at = 0;
    let matches = |stack: &[(bool, Vec<u8>, bool)]| {
        stack.len() == path.len()
            && stack
                .iter()
                .zip(path)
                .all(|((object, key, _), want)| *object && key == want.as_bytes())
    };
    while let Some(&byte) = line.get(at) {
        let at_value = stack.last().is_some_and(|top| top.0 && !top.2) && matches(&stack);
        if at_value && !byte.is_ascii_whitespace() && byte != b':' {
            let end = value_end(line, at)?;
            return line.get(at..end);
        }
        match byte {
            b'{' => stack.push((true, Vec::new(), true)),
            b'[' => stack.push((false, Vec::new(), false)),
            b'}' | b']' => {
                stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut().filter(|top| top.0) {
                    top.2 = true;
                }
            }
            b':' => {
                if let Some(top) = stack.last_mut().filter(|top| top.0) {
                    top.2 = false;
                }
            }
            b'"' => {
                let end = string_end(line, at)?;
                let raw = line.get(at + 1..end - 1)?;
                if let Some(top) = stack.last_mut().filter(|top| top.0 && top.2) {
                    top.1 = raw.to_vec();
                }
                at = end;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// The end (exclusive) of the string starting with the quote at `at`.
fn string_end(line: &[u8], at: usize) -> Option<usize> {
    let mut end = at + 1;
    let mut escape = false;
    loop {
        let inner = *line.get(end)?;
        if escape {
            escape = false;
        } else if inner == b'\\' {
            escape = true;
        } else if inner == b'"' {
            return Some(end + 1);
        }
        end += 1;
    }
}

/// The end (exclusive) of the value starting at `at`.
fn value_end(line: &[u8], at: usize) -> Option<usize> {
    match *line.get(at)? {
        b'"' => string_end(line, at),
        b'{' | b'[' => {
            let mut depth = 0_usize;
            let mut end = at;
            loop {
                match *line.get(end)? {
                    b'"' => {
                        end = string_end(line, end)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(end + 1);
                        }
                    }
                    _ => {}
                }
                end += 1;
            }
        }
        _ => {
            let mut end = at;
            while let Some(&byte) = line.get(end) {
                if matches!(byte, b',' | b'}' | b']') || byte.is_ascii_whitespace() {
                    break;
                }
                end += 1;
            }
            Some(end)
        }
    }
}

/// The top-level elements of a raw JSON array, raw.
#[must_use]
pub fn elements(raw: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if raw.first() != Some(&b'[') {
        return out;
    }
    let mut at = 1;
    while let Some(&byte) = raw.get(at) {
        if byte == b']' {
            break;
        }
        if byte == b',' || byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let Some(end) = value_end(raw, at) else {
            break;
        };
        if let Some(element) = raw.get(at..end) {
            out.push(element);
        }
        at = end;
    }
    out
}

/// A raw JSON string value decoded, or `None` when it is not a string.
#[must_use]
pub fn raw_string(raw: &[u8]) -> Option<String> {
    let inner = raw.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
    unescape(inner)
}

/// A JSON string's raw bytes (between the quotes) decoded, or `None` for a
/// malformed escape or invalid UTF-8.
fn unescape(raw: &[u8]) -> Option<String> {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut at = 0;
    while let Some(&byte) = raw.get(at) {
        at += 1;
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let escaped = *raw.get(at)?;
        at += 1;
        let simple = match escaped {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'/' => Some(b'/'),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'u' => None,
            _ => return None,
        };
        if let Some(simple) = simple {
            out.push(simple);
            continue;
        }
        let high = hex4(raw.get(at..at + 4)?)?;
        at += 4;
        let code = if (0xd800..0xdc00).contains(&high) {
            if raw.get(at..at + 2)? != b"\\u" {
                return None;
            }
            let low = hex4(raw.get(at + 2..at + 6)?)?;
            at += 6;
            if !(0xdc00..0xe000).contains(&low) {
                return None;
            }
            0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
        } else {
            high
        };
        let character = char::from_u32(code)?;
        let mut encoded = [0_u8; 4];
        out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    }
    String::from_utf8(out).ok()
}

fn hex4(digits: &[u8]) -> Option<u32> {
    let text = core::str::from_utf8(digits).ok()?;
    u32::from_str_radix(text, 16).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::FieldScanner;

    const WANTED: &[&str] = &["cwd", "slug"];

    #[test]
    fn finds_top_level_fields_after_nested_values() {
        let mut scanner = FieldScanner::new(WANTED);
        scanner.feed(br#"{"type":"queue-operation","content":"x \"cwd\":\"/no\""}"#);
        scanner.feed(b"\n");
        scanner.feed(br#"{"attachment":{"cwd":"/nested","list":["cwd","/also-no"]},"cwd":"/Users/jess/git/lab","x":1}"#);
        scanner.feed(b"\n");
        assert_eq!(scanner.get(0), Some("/Users/jess/git/lab"));
        assert_eq!(scanner.get(1), None);
        assert!(!scanner.complete());
        scanner.feed(b"{\"cwd\":\"/later\",\"slug\":\"quiet-\\u00e9-seal\\ud83d\\ude00\"}");
        assert_eq!(scanner.get(0), Some("/Users/jess/git/lab"));
        assert_eq!(scanner.get(1), Some("quiet-\u{e9}-seal\u{1f600}"));
        assert!(scanner.complete());
    }

    #[test]
    fn nested_string_finds_a_key_path() {
        let line = br#"{"type":"session_meta","payload":{"base":{"text":"x \"cwd\":\"/no\""},"list":[{"cwd":"/no"}],"cwd":"/Users/jess/git/lab","id":"t"}}"#;
        assert_eq!(
            super::nested_string(line, &["payload", "cwd"]).as_deref(),
            Some("/Users/jess/git/lab")
        );
        assert_eq!(super::nested_string(line, &["cwd"]), None);
        assert_eq!(
            super::nested_string(b"{\"payload\":{\"cwd\":1}}", &["payload", "cwd"]),
            None
        );
    }

    #[test]
    fn split_feeds_and_malformed_lines() {
        let line = br#"{"a":"}{[","cwd":"/p\\q\"r"}"#;
        let mut scanner = FieldScanner::new(WANTED);
        scanner.feed(br#"{"cwd":{"#);
        scanner.feed(b"\n");
        for byte in line {
            scanner.feed(&[*byte]);
        }
        assert_eq!(scanner.get(0), Some(r#"/p\q"r"#));
        let mut scanner = FieldScanner::mid_line(WANTED);
        scanner.feed(br#"tail of a line","cwd":"/wrong"}"#);
        scanner.feed(b"\n{\"cwd\":\"/right\"}\n");
        assert_eq!(scanner.get(0), Some("/right"));
    }

    #[test]
    fn nested_raw_finds_values_of_any_kind() {
        let line = br#"{"type":"session_meta","payload":{"base":{"text":"x \"source\":1"},"source":{"subagent":{"thread_spawn":{"parent_thread_id":"p","depth":2}}},"cwd":"/a","n": 42 ,"content":[{"type":"input_text","text":"hi"},{"text":"b\"c"}]}}"#;
        assert_eq!(
            super::nested_raw(line, &["payload", "source"]),
            Some(&br#"{"subagent":{"thread_spawn":{"parent_thread_id":"p","depth":2}}}"#[..])
        );
        assert_eq!(
            super::nested_raw(line, &["payload", "cwd"]),
            Some(&br#""/a""#[..])
        );
        assert_eq!(super::nested_raw(line, &["payload", "n"]), Some(&b"42"[..]));
        assert_eq!(super::nested_raw(line, &["payload", "missing"]), None);
        let content = super::nested_raw(line, &["payload", "content"]).unwrap();
        let elements = super::elements(content);
        assert_eq!(elements.len(), 2);
        assert_eq!(
            super::nested_string(elements.get(1).unwrap(), &["text"]).as_deref(),
            Some("b\"c")
        );
        assert_eq!(super::raw_string(br#""x\ny""#).as_deref(), Some("x\ny"));
        assert_eq!(super::raw_string(b"42"), None);
    }
}
