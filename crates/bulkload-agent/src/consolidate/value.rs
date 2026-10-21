//! The structured value both config formats parse into.
//!
//! Also its comparison, the structural union, and the `--map` translation
//! (operator ruling 2026-10-20: the agent homes are consolidated, never
//! landed beside).

use std::cmp::Ordering;

/// A JSON or TOML value. Objects keep their keys in document order;
/// numbers and date-times keep the token they were written as, so an
/// unchanged value is written back as it was.
#[derive(Debug, Clone)]
pub enum Value {
    Object(Vec<(String, Self)>),
    Array(Vec<Self>),
    Str(String),
    /// An integer token (JSON or TOML).
    Int(String),
    /// A float token (JSON or TOML).
    Float(String),
    Bool(bool),
    /// JSON only.
    Null,
    /// A TOML date-time, date or time token.
    Datetime(String),
}

impl Value {
    /// The entry `key` of an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// Whether two values are the same value, whatever their spelling: an
    /// object's keys in any order, a number's token in any radix or with
    /// `_` separators, a date-time's `T` written as `t` or a space.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Object(left), Self::Object(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .all(|(key, value)| other.get(key).is_some_and(|theirs| value.same(theirs)))
            }
            (Self::Array(left), Self::Array(right)) => {
                left.len() == right.len() && left.iter().zip(right).all(|(a, b)| a.same(b))
            }
            (Self::Str(left), Self::Str(right)) => left == right,
            (Self::Int(left), Self::Int(right)) => match (integer(left), integer(right)) {
                (Some(left), Some(right)) => left == right,
                _ => left == right,
            },
            (Self::Int(left) | Self::Float(left), Self::Int(right) | Self::Float(right)) => {
                match (float(left), float(right)) {
                    (Some(left), Some(right)) => {
                        left.total_cmp(&right).is_eq() || (left.is_nan() && right.is_nan())
                    }
                    _ => left == right,
                }
            }
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Null, Self::Null) => true,
            (Self::Datetime(left), Self::Datetime(right)) => {
                datetime_key(left) == datetime_key(right)
            }
            _ => false,
        }
    }

    /// The kind's name, for a report.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Object(_) => "object",
            Self::Array(_) => "array",
            Self::Str(_) => "string",
            Self::Int(_) => "integer",
            Self::Float(_) => "float",
            Self::Bool(_) => "boolean",
            Self::Null => "null",
            Self::Datetime(_) => "datetime",
        }
    }
}

/// An integer token's value: decimal with an optional sign, or `0x`, `0o`,
/// `0b`; `_` separators ignored.
fn integer(token: &str) -> Option<i128> {
    let digits: String = token.chars().filter(|c| *c != '_').collect();
    let (negative, body) = match digits.as_bytes().first() {
        Some(b'-') => (true, digits.get(1..)?),
        Some(b'+') => (false, digits.get(1..)?),
        _ => (false, digits.as_str()),
    };
    let value = if let Some(hex) = body.strip_prefix("0x") {
        i128::from_str_radix(hex, 16).ok()?
    } else if let Some(octal) = body.strip_prefix("0o") {
        i128::from_str_radix(octal, 8).ok()?
    } else if let Some(binary) = body.strip_prefix("0b") {
        i128::from_str_radix(binary, 2).ok()?
    } else {
        body.parse::<i128>().ok()?
    };
    Some(if negative { -value } else { value })
}

/// A number token's value as a float (`inf`, `nan` and `_` included).
fn float(token: &str) -> Option<f64> {
    let digits: String = token.chars().filter(|c| *c != '_').collect();
    match digits.trim_start_matches(['+', '-']) {
        "inf" => Some(if digits.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }),
        "nan" => Some(f64::NAN),
        _ => integer(&digits)
            .map(|value| {
                #[allow(clippy::cast_precision_loss, reason = "compared as floats")]
                let value = value as f64;
                value
            })
            .or_else(|| digits.parse::<f64>().ok()),
    }
}

/// A date-time token with its separator and zone letter spelled one way.
fn datetime_key(token: &str) -> String {
    token
        .char_indices()
        .map(|(at, c)| match c {
            't' | ' ' if at == 10 => 'T',
            'z' => 'Z',
            other => other,
        })
        .collect()
}

/// One step of a key path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Key(String),
    Index(usize),
}

/// A key path as an RFC 6901 JSON pointer (`/permissions/allow/0`).
#[must_use]
pub fn pointer(path: &[Step]) -> String {
    let mut out = String::new();
    for step in path {
        out.push('/');
        match step {
            Step::Key(key) => out.push_str(&key.replace('~', "~0").replace('/', "~1")),
            Step::Index(index) => out.push_str(&index.to_string()),
        }
    }
    out
}

/// Which side's value is in the merged file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The destination's live file.
    Live,
    /// The source's file, as the pull offered it.
    Source,
}

impl Side {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Source => "source",
        }
    }
}

/// A key both sides hold with different values: `kept` is in the merged
/// file, `other` is recorded in the ledger (nothing lost, no second file).
#[derive(Debug, Clone)]
pub struct Conflict {
    pub path: Vec<Step>,
    pub kept_side: Side,
    pub kept: Value,
    pub other: Value,
}

/// The structural union of a live value and a source value.
#[derive(Debug)]
pub struct Union {
    pub value: Value,
    pub conflicts: Vec<Conflict>,
    /// Keys the source added.
    pub keys_added: usize,
    /// Array elements the source added.
    pub elements_added: usize,
    /// Values the source's won (the later-modified side, `Source`).
    pub replaced: usize,
}

impl Union {
    /// Whether the union is the live value: nothing to write.
    #[must_use]
    pub const fn covered(&self) -> bool {
        self.keys_added == 0 && self.elements_added == 0 && self.replaced == 0
    }
}

/// The union of `live` and `source` (already translated).
///
/// - Objects are merged key by key: the live file's key order is kept and
///   the source's new keys are appended.
/// - Arrays are unioned by value: the live elements first and in order,
///   then each source element no element equals.
/// - Equal scalars are kept.
/// - A key both hold with different values (or kinds) takes the value of
///   `later`, the side modified last, and the other value is a
///   [`Conflict`].
#[must_use]
pub fn union(live: &Value, source: &Value, later: Side) -> Union {
    let mut out = Union {
        value: Value::Null,
        conflicts: Vec::new(),
        keys_added: 0,
        elements_added: 0,
        replaced: 0,
    };
    let mut path = Vec::new();
    out.value = merge(&mut path, live, source, later, &mut out);
    out
}

fn merge(
    path: &mut Vec<Step>,
    live: &Value,
    source: &Value,
    later: Side,
    out: &mut Union,
) -> Value {
    match (live, source) {
        (Value::Object(ours), Value::Object(theirs)) => {
            let mut entries = Vec::with_capacity(ours.len());
            for (key, value) in ours {
                let merged = source.get(key).map_or_else(
                    || value.clone(),
                    |other| {
                        path.push(Step::Key(key.clone()));
                        let merged = merge(path, value, other, later, out);
                        path.pop();
                        merged
                    },
                );
                entries.push((key.clone(), merged));
            }
            for (key, value) in theirs {
                if live.get(key).is_none() {
                    out.keys_added += 1;
                    entries.push((key.clone(), value.clone()));
                }
            }
            Value::Object(entries)
        }
        (Value::Array(ours), Value::Array(theirs)) => {
            let mut elements = ours.clone();
            for value in theirs {
                if !elements.iter().any(|held| held.same(value)) {
                    out.elements_added += 1;
                    elements.push(value.clone());
                }
            }
            Value::Array(elements)
        }
        _ if live.same(source) => live.clone(),
        _ => {
            let (kept, other) = match later {
                Side::Live => (live, source),
                Side::Source => {
                    out.replaced += 1;
                    (source, live)
                }
            };
            out.conflicts.push(Conflict {
                path: path.clone(),
                kept_side: later,
                kept: kept.clone(),
                other: other.clone(),
            });
            kept.clone()
        }
    }
}

/// The `--map` set as path prefixes, the longest first: a source value
/// (or key) naming a source path is compared and written as the
/// destination's path.
#[derive(Debug, Clone, Default)]
pub struct Maps {
    pairs: Vec<(String, String)>,
}

impl Maps {
    /// The pairs `(source prefix, destination prefix)`; empty or equal
    /// pairs are dropped, and a prefix's trailing `/` is ignored.
    #[must_use]
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut pairs: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(from, to)| (trim_slash(&from).to_owned(), trim_slash(&to).to_owned()))
            .filter(|(from, to)| !from.is_empty() && from != to)
            .collect();
        pairs.sort_by(|a, b| match b.0.len().cmp(&a.0.len()) {
            Ordering::Equal => a.0.cmp(&b.0),
            other => other,
        });
        pairs.dedup_by(|a, b| a.0 == b.0);
        Self { pairs }
    }

    /// Whether there is nothing to translate.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// The pairs, longest source prefix first.
    #[must_use]
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// `text` with every source prefix that stands as a whole path (at the
    /// start of the text or after a character no path name holds, and
    /// followed by `/`, the end, or such a character) written as the
    /// destination's.
    #[must_use]
    pub fn string(&self, text: &str) -> String {
        if self.pairs.is_empty() {
            return text.to_owned();
        }
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        let mut copied = 0;
        while at < bytes.len() {
            let boundary = at == 0 || bytes.get(at - 1).is_some_and(|byte| !path_byte(*byte));
            let found = if boundary {
                self.pairs.iter().find(|(from, _)| {
                    text.get(at..)
                        .is_some_and(|rest| rest.starts_with(from.as_str()))
                        && bytes
                            .get(at + from.len())
                            .is_none_or(|next| *next == b'/' || !path_byte(*next))
                })
            } else {
                None
            };
            if let Some((from, to)) = found {
                out.push_str(text.get(copied..at).unwrap_or_default());
                out.push_str(to);
                at += from.len();
                copied = at;
            } else {
                at += 1;
            }
        }
        out.push_str(text.get(copied..).unwrap_or_default());
        out
    }

    /// `value` with every string and object key translated; `None` when
    /// two keys of one object translate to the same key (the file is held,
    /// never merged with one of them dropped).
    #[must_use]
    pub fn value(&self, value: &Value) -> Option<Value> {
        if self.pairs.is_empty() {
            return Some(value.clone());
        }
        Some(match value {
            Value::Object(entries) => {
                let mut out: Vec<(String, Value)> = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    let key = self.string(key);
                    if out.iter().any(|(held, _)| *held == key) {
                        return None;
                    }
                    out.push((key, self.value(value)?));
                }
                Value::Object(out)
            }
            Value::Array(elements) => Value::Array(
                elements
                    .iter()
                    .map(|value| self.value(value))
                    .collect::<Option<Vec<_>>>()?,
            ),
            Value::Str(text) => Value::Str(self.string(text)),
            other => other.clone(),
        })
    }
}

fn trim_slash(text: &str) -> &str {
    if text.len() > 1 {
        text.trim_end_matches('/')
    } else {
        text
    }
}

/// A byte a path name holds next to a prefix, so the prefix is not a whole
/// path there (`/Users/jess` in `/Users/jessie`).
const fn path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b'~') || byte >= 0x80
}
