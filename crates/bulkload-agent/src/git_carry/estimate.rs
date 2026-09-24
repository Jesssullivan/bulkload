//! Read-only measurement of what git carry v2 would move (R-N60 baseline;
//! R-N74 and R-N97 gate metric; R-N75 refusals).
//!
//! One probe script, [`PROBE_SCRIPT`], reads a repository's offer: every ref
//! tip, every worktree's `HEAD` and per-worktree refs (`refs/worktree/`,
//! `refs/bisect/`, `refs/rewritten/`, found through each administrative
//! directory under `worktrees/`, so a pruned-away checkout still counts), its
//! shallow frontier, its git dir and whether it is a partial clone. The probe
//! first proves the path is the repository's root: a work tree's top level or a
//! bare repository's git dir, with discovery fenced by `GIT_CEILING_DIRECTORIES`
//! and paths carrying control characters refused. The same text runs through
//! `bash -s` on this host for the source and a local destination, and through
//! one `ssh` session for a remote destination, so the remote tips and shallow
//! frontier come from one connection. Every later source command names the
//! probed git dir with `--git-dir`, never the path it was given.
//!
//! The negotiation model is upload-pack's, fed M1's first round (R-N113,
//! R-N116):
//! - haves are *every* destination tip the source holds, offered ancestors
//!   first (topological order), so upload-pack keeps each one as an exclusion
//!   and returns its smallest pack;
//! - wants are the source's probed tips and stash entries, minus every tip the
//!   destination already holds;
//! - for a shallow destination (R-N75 guarantees its frontier equals the
//!   source's), `pack-objects` runs as upload-pack runs it for a shallow client:
//!   `--shallow-file ''`, `--shallow` and one `--shallow <oid>` line per
//!   boundary commit, so the walk is edge-aggressive over every have.
//!
//! The source then walks the wants minus the haves (`rev-list --objects
//! --missing=print`), refusing a non-partial source that cannot reach an object
//! its refs name, and tallying `%(objectsize:disk)` of the rest (informational).
//! It streams `pack-objects --stdout --thin --revs --delta-base-offset` over the
//! same request into a byte counter: `missing_thin_pack_bytes`. The pack never
//! reaches disk. `pack.useSparse` and `pack.useBitmaps` are pinned off so the
//! pack is exactly the walked set; the M1 sender must pin both too, or it can
//! send a different set.
//!
//! **Gate (R-N97, R-N113, R-N116):** sent bytes ≤ 1.1 ×
//! `missing_thin_pack_bytes`, and sent objects ≤ `missing_objects`. M1's
//! first round sends *exactly* the destination's held tips (every destination
//! tip the source holds) as haves, in topological order, ancestors first. The
//! exactness oracle is upload-pack run over exactly that have list in that
//! order, not `git fetch`'s newest-first negotiation; the estimate equals it.
//! Shallow rule: a shallow destination must be shallow at the source's
//! frontier (R-N75), the request carries its `shallow <oid>` lines, and
//! upload-pack packs with `--shallow`, marking the trees of every have.
//!
//! Order matters because upload-pack drops a have only when it arrives after
//! a have that implies it (its child), that is, out of ancestors-first order.
//! Ancestors first, nothing is dropped. Child first, a shallow destination
//! loses the parent's tree from the exclusion (reviewer fixture P1: 162 B
//! ancestors first, 3,889 B child first). Haves beyond the held tips are not
//! free either: offered out of ancestors-first order (as `git fetch` offers
//! them, newest first), an extra intermediate have makes upload-pack drop the
//! have it implies, which can *enlarge* a shallow pack (fixture A3: 232 B with
//! the held tips, 3,919 B once `git fetch` offers the parent too). Offered
//! ancestors first, the same extra have drops nothing (fixture A3X: 232 B). Extra haves can shrink
//! a non-shallow pack. Ancestor probing (`GitHaveQuery`) must therefore not
//! add haves to a shallow destination's first round.
//!
//! The pack also depends on `pack.threads` and `pack.windowMemory`, which
//! [`git`] pins to 2 and 64m for every call; the M1 sender must pin them the
//! same way, as well as `pack.useSparse=false` and `pack.useBitmaps=false`.
//! A bitmapped sender that did not pin bitmaps would pack *fewer* objects than
//! the walk (W6 M1 spike, bulkload#64), so the estimate stays an upper bound
//! for it.
//!
//! Nothing is fetched, written or updated on either side. Every Git call runs
//! with `GIT_NO_LAZY_FETCH=1`, `--no-optional-locks`, `maintenance.auto=false`,
//! `gc.auto=0` and `core.hooksPath=/dev/null`. The probe accepts only a Git that
//! honours `GIT_NO_LAZY_FETCH` (2.45.0 and later, 2.44.1+, and the backports
//! 2.43.4+, 2.42.2+, 2.41.1+, 2.40.2+ and 2.39.4+); any other or unparseable
//! version is refused.
//!
//! A destination whose refs cannot prove it holds their history is refused
//! (R-N75): a partial clone (by any config file of any worktree, a `.promisor`
//! pack in its object store or any alternate's), or a shallow repository whose
//! frontier differs from the source's.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{git, oid};
use crate::{BulkloadRefusal, Result};

/// Where the destination's offer is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A repository on this host.
    Local(PathBuf),
    /// A repository reached with `ssh -T -oBatchMode=yes HOST`.
    Remote {
        /// SSH destination (an alias or `user@host`).
        host: String,
        /// Absolute path of the repository on that host.
        path: String,
    },
}

impl Destination {
    /// Parse `PATH` or scp-style `HOST:PATH`.
    ///
    /// A value starting with `/` or `.`, or with no `:` before its first `/`,
    /// is local. Remote hosts and paths are held to a conservative character
    /// set, because the remote login shell (fish on sting) re-parses them.
    ///
    /// # Errors
    /// Refuses a remote host or path outside that character set, or a remote
    /// path that is not absolute.
    pub fn parse(value: &OsStr) -> Result<Self> {
        let Some(text) = value.to_str() else {
            return Ok(Self::Local(PathBuf::from(value)));
        };
        let remote = !text.starts_with('/')
            && !text.starts_with('.')
            && text
                .split_once(':')
                .is_some_and(|(host, _)| !host.contains('/'));
        if !remote {
            return Ok(Self::Local(PathBuf::from(value)));
        }
        let (host, path) = text
            .split_once(':')
            .ok_or(BulkloadRefusal::PathNotPortable)?;
        if !remote_host(host) {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        if !path.starts_with('/') {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        if !remote_path(path) {
            return Err(BulkloadRefusal::PathNotPortable);
        }
        Ok(Self::Remote {
            host: host.to_owned(),
            path: path.to_owned(),
        })
    }

    /// Operator-facing spelling, as accepted by [`Destination::parse`].
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Remote { host, path } => format!("{host}:{path}"),
        }
    }
}

fn remote_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
}

fn remote_path(path: &str) -> bool {
    path.len() <= 4096
        && !path.split('/').any(|part| part == "..")
        && path.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+' | b',' | b'=')
        })
}

/// A refusal plus, when a child process explained it, a bounded and redacted
/// excerpt of that child's standard error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The typed refusal; its code is the stable identity.
    pub refusal: BulkloadRefusal,
    /// At most [`DETAIL_LIMIT`] printable characters, one line, with URL
    /// credentials and token-shaped words removed.
    pub detail: Option<String>,
}

impl Refused {
    const fn new(refusal: BulkloadRefusal, detail: Option<String>) -> Self {
        Self { refusal, detail }
    }
}

impl From<BulkloadRefusal> for Refused {
    fn from(refusal: BulkloadRefusal) -> Self {
        Self::new(refusal, None)
    }
}

impl From<std::io::Error> for Refused {
    fn from(error: std::io::Error) -> Self {
        BulkloadRefusal::from(error).into()
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(detail) => write!(f, "{}: {detail}", self.refusal),
            None => write!(f, "{}", self.refusal),
        }
    }
}

/// Longest stderr excerpt a [`Refused`] carries, in characters.
pub const DETAIL_LIMIT: usize = 240;

const REDACTED: &str = "[redacted]";

/// One line of at most [`DETAIL_LIMIT`] characters from a child's stderr.
///
/// Before redaction, every whitespace or control character (ASCII or Unicode:
/// NBSP, EM SPACE, U+2028, NUL, BS, DEL, ...) becomes a space, so it separates
/// a key from its value, and every invisible or format (`Cf`) character is
/// dropped, so it cannot split a word. A token prefix split across two words
/// (`gh p_...`) redacts both.
fn detail(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let cleaned: String = text
        .chars()
        .filter_map(|c| {
            if invisible(c) {
                None
            } else if c.is_whitespace() || c.is_control() {
                Some(' ')
            } else {
                Some(c)
            }
        })
        .collect();
    let words: Vec<&str> = cleaned.split(' ').filter(|word| !word.is_empty()).collect();
    let line = redact_words(&words).join(" ");
    if line.is_empty() {
        return None;
    }
    let mut bounded: String = line.chars().take(DETAIL_LIMIT).collect();
    if line.chars().count() > DETAIL_LIMIT {
        bounded.push_str("...");
    }
    Some(bounded)
}

/// Unicode format (`Cf`) and other invisible characters: soft hyphen, Arabic
/// and Syriac format marks, Mongolian vowel separator, zero-width and bidi
/// marks, word joiners and invisible operators, the byte-order mark,
/// interlinear annotation marks, and the shorthand, musical and tag format
/// blocks.
const fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{890}'..='\u{891}'
            | '\u{8e2}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

/// Words whose value, inline or in the next word, is an authorization scheme
/// followed by the credential itself (`Bearer`, `Bearer:`, `basic=`).
fn scheme_word(value: &str) -> bool {
    matches!(
        value.trim_matches(['"', '\'', ',', ';', ':', '=']),
        "bearer" | "basic" | "digest" | "token" | "negotiate"
    )
}

const TOKEN_PREFIXES: [&str; 8] = [
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "glpat-",
    "xox",
    "AKIA",
];

/// Index of the word where a PEM header (`-----BEGIN`) starts, even when a
/// line break or spaces split it (`-----BEG` / `IN`, `----- BEGIN`).
fn pem_start(words: &[&str]) -> Option<usize> {
    let mut squashed = String::new();
    let mut starts = Vec::with_capacity(words.len());
    for word in words {
        starts.push(squashed.len());
        squashed.push_str(&word.to_ascii_lowercase());
    }
    let at = squashed.find("-----begin")?;
    starts.iter().rposition(|start| *start <= at)
}

/// Redact credential-shaped words, the value after a key such as
/// `password:`, `token =` or `Authorization: Bearer`, everything from a PEM
/// header on, and token prefixes split across two words.
fn redact_words(words: &[&str]) -> Vec<String> {
    let pem = pem_start(words);
    let mut redacted = Vec::with_capacity(words.len());
    let mut pending = 0_u8;
    for (index, word) in words.iter().enumerate() {
        let lower = word.to_ascii_lowercase();
        if pem.is_some_and(|start| index >= start) {
            redacted.push(REDACTED.to_owned());
            continue;
        }
        if pending > 0 {
            pending -= 1;
            redacted.push(REDACTED.to_owned());
            // `key = value`, `key : value` and `Authorization: Bearer value`
            // carry the value one word further on.
            if lower.trim_matches([':', '=']).is_empty() || scheme_word(&lower) {
                pending = 1;
            }
            continue;
        }
        if spaced_short_key(&lower, words.get(index + 1).copied()) {
            // `pass : x`, `pwd = x`, `{"pwd" : "x"}`: the separator word and
            // the value after it are redacted through `pending`.
            redacted.push(REDACTED.to_owned());
            pending = 1;
            continue;
        }
        if let Some(value) = sensitive_key(&lower) {
            redacted.push(REDACTED.to_owned());
            // `Authorization: Bearer x` and `authorization=Basic x`.
            if match value {
                KeyValue::Next => true,
                KeyValue::Inline(inline) => scheme_word(&inline),
            } {
                pending = 1;
            }
            continue;
        }
        redacted.push(redact(word));
    }
    for (index, pair) in words.windows(2).enumerate() {
        let (Some(first), Some(second)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let joined = format!("{first}{second}");
        if TOKEN_PREFIXES
            .iter()
            .any(|prefix| prefix_reaches_boundary(&joined, first, prefix))
        {
            for at in [index, index + 1] {
                if let Some(slot) = redacted.get_mut(at) {
                    REDACTED.clone_into(slot);
                }
            }
        }
    }
    redacted
}

/// Whether a token `prefix` in `joined` (`first` then the next word) runs up
/// to, or across, the boundary between the words: `gh` / `p_x` split by a
/// separator, `ghp_` / `x` with the whole prefix before it, or `xoxb-` / `x`
/// with a few token characters after the prefix. The token body is then the
/// next word.
fn prefix_reaches_boundary(joined: &str, first: &str, prefix: &str) -> bool {
    joined.match_indices(prefix).any(|(at, _)| {
        let end = at + prefix.len();
        at < first.len()
            && first.len() <= end + 4
            && first
                .get(end..)
                .unwrap_or("")
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    })
}

/// A short key (`pass`, `pwd`, `sig`, `signature`, `cookie`, `auth`, `key`,
/// `code`, `session`, `sid`) standing alone, quoted or not, whose separator
/// is the next word (`pass : x`, `pwd = x`, `{"pwd" : "x"}`).
fn spaced_short_key(lower: &str, next: Option<&str>) -> bool {
    let bare = lower.trim_matches(['{', '[', '(', '"', '\'', ',']);
    let separator = next.and_then(|word| word.chars().next());
    SECRET_KEYS.iter().any(|(key, follow)| {
        bare == *key
            && match follow {
                Follow::Any => false,
                Follow::Separator => matches!(separator, Some(':' | '=')),
                Follow::Equals => separator == Some('='),
            }
    })
}

/// Where a secret key's value is.
enum KeyValue {
    /// In the next word (`password: x`, `token x`).
    Next,
    /// In the same word (`password=x`, `"token":"x"`).
    Inline(String),
}

/// What must follow a key for it to name a secret.
#[derive(Clone, Copy)]
enum Follow {
    /// Anything: `password hunter2` redacts `hunter2`.
    Any,
    /// `:` or `=`: short words like `pass` or `signature` are ordinary
    /// English until a separator makes them a key.
    Separator,
    /// `=` only: query and cookie keys (`?code=`, `session=`), which appear
    /// with `:` in ordinary text (`exit code: 128`).
    Equals,
}

/// Secret key words and what must follow each for it to name a secret.
const SECRET_KEYS: [(&str, Follow); 24] = [
    ("password", Follow::Any),
    ("passwd", Follow::Any),
    ("passphrase", Follow::Any),
    ("token", Follow::Any),
    ("secret", Follow::Any),
    ("authorization", Follow::Any),
    ("bearer", Follow::Any),
    ("api_key", Follow::Any),
    ("apikey", Follow::Any),
    ("api-key", Follow::Any),
    ("credential", Follow::Any),
    ("private_key", Follow::Any),
    ("access_key", Follow::Any),
    ("x-amz-signature", Follow::Any),
    ("pass", Follow::Separator),
    ("pwd", Follow::Separator),
    ("sig", Follow::Separator),
    ("signature", Follow::Separator),
    ("cookie", Follow::Separator),
    ("auth", Follow::Separator),
    ("key", Follow::Separator),
    ("code", Follow::Equals),
    ("session", Follow::Equals),
    ("sid", Follow::Equals),
];

/// The value's place when `lower` names a secret at a word boundary.
fn sensitive_key(lower: &str) -> Option<KeyValue> {
    SECRET_KEYS.iter().find_map(|(key, follow)| {
        lower.match_indices(key).find_map(|(at, _)| {
            let before = lower.get(..at).and_then(|head| head.chars().next_back());
            let tail = lower.get(at + key.len()..).unwrap_or("");
            if before.is_some_and(|c| c.is_ascii_alphanumeric())
                || tail.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            {
                return None;
            }
            let rest = tail.trim_start_matches(['"', '\'']);
            let separator = rest.chars().next();
            let accepted = match follow {
                Follow::Any => true,
                Follow::Separator => matches!(separator, Some(':' | '=')),
                Follow::Equals => separator == Some('='),
            };
            if !accepted {
                return None;
            }
            let separated = rest.trim_start_matches([':', '=']);
            let value = separated.trim_matches(['"', '\'', ',', ';', '}', '{']);
            if separated.len() == rest.len() || value.is_empty() {
                return Some(KeyValue::Next);
            }
            Some(KeyValue::Inline(value.to_owned()))
        })
    })
}

/// Drop anything credential-shaped from one word of diagnostic text.
fn redact(word: &str) -> String {
    if TOKEN_PREFIXES.iter().any(|prefix| word.contains(prefix)) {
        return REDACTED.to_owned();
    }
    if let Some((scheme, rest)) = word.split_once("://") {
        if let Some((userinfo, host)) = rest.rsplit_once('@') {
            if url_credential(userinfo) {
                return format!("{scheme}://{REDACTED}@{host}");
            }
        }
        return word.to_owned();
    }
    // Schemeless `user:password@host`: userinfo holding a `:` is a secret;
    // scp-style `git@host:path` is not.
    if let Some((user, host)) = word.rsplit_once('@') {
        let start = user
            .rfind(['\'', '"', '(', '<', '[', '=', ','])
            .map_or(0, |at| at + 1);
        let (prefix, userinfo) = user.split_at(start);
        if userinfo.contains(':') {
            return format!("{prefix}{REDACTED}@{host}");
        }
    }
    word.to_owned()
}

/// Whether the text between `://` and a URL's last `@` is userinfo. A bare
/// token (no `/`) is; so is `user:password`, even when the password holds a
/// `/`. `host/@scope` and `host:443/@scope` are a path after a host and port.
fn url_credential(userinfo: &str) -> bool {
    let Some((authority, _)) = userinfo.split_once('/') else {
        return true;
    };
    authority
        .split_once(':')
        .is_some_and(|(_, after)| !after.bytes().all(|b| b.is_ascii_digit()))
}

/// Object count and stored bytes of one Git object type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TypeTally {
    /// Objects of this type.
    pub count: u64,
    /// Sum of `%(objectsize:disk)` over those objects.
    pub bytes_disk: u64,
}

/// Per-type tallies of one object set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Commits.
    pub commit: TypeTally,
    /// Trees.
    pub tree: TypeTally,
    /// Blobs.
    pub blob: TypeTally,
    /// Annotated tags.
    pub tag: TypeTally,
    /// Reached by the walk but absent from the source's store: `rev-list
    /// --missing=print` lines (a partial clone's unfetched objects).
    pub unavailable: u64,
}

impl Tally {
    /// Objects present in the store, all types.
    #[must_use]
    pub const fn objects(&self) -> u64 {
        self.commit.count + self.tree.count + self.blob.count + self.tag.count
    }

    /// Stored bytes, all types.
    #[must_use]
    pub const fn bytes_disk(&self) -> u64 {
        self.commit.bytes_disk + self.tree.bytes_disk + self.blob.bytes_disk + self.tag.bytes_disk
    }

    fn add(&mut self, line: &str) -> Result<()> {
        let mut fields = line.split(' ');
        let (Some(value), Some(kind)) = (fields.next(), fields.next()) else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if kind == "missing" {
            self.unavailable += 1;
            return Ok(());
        }
        let bytes = fields
            .next()
            .and_then(|size| size.parse::<u64>().ok())
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let slot = match kind {
            "commit" => &mut self.commit,
            "tree" => &mut self.tree,
            "blob" => &mut self.blob,
            "tag" => &mut self.tag,
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        };
        slot.count += 1;
        slot.bytes_disk += bytes;
        Ok(())
    }
}

/// The thin pack `pack-objects` builds for the missing set, counted in flight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThinPack {
    /// Bytes of the whole pack stream, header and trailer included.
    pub bytes: u64,
    /// Object count from the pack header.
    pub objects: u64,
}

/// What git carry v2 would have to move from one source to one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarryEstimate {
    /// Distinct oids among the destination's tips (refs and worktree `HEAD`s).
    pub destination_tip_count: usize,
    /// Lines in the destination's shallow file.
    pub destination_shallow_count: usize,
    /// Destination tips that exist as objects in the source.
    pub haves_used: usize,
    /// Lines in the source's shallow file.
    pub source_shallow_count: usize,
    /// Whether the source is a partial clone (it may then lack objects).
    pub source_partial: bool,
    /// Stash reflog entries walked in addition to the probed tips.
    pub stash_entries: usize,
    /// The whole closure of the source's tips and stash entries.
    pub source: Tally,
    /// That closure minus everything reachable from the haves.
    pub missing: Tally,
    /// The thin pack for `missing`: the W6 M1 gate metric (R-N74).
    pub thin_pack: ThinPack,
}

impl CarryEstimate {
    /// `key=value` lines, one per measurement.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("source_objects={}", self.source.objects()),
            format!("source_history_bytes={}", self.source.bytes_disk()),
            format!("source_commits={}", self.source.commit.count),
            format!("source_trees={}", self.source.tree.count),
            format!("source_blobs={}", self.source.blob.count),
            format!("source_tags={}", self.source.tag.count),
            format!("source_unavailable_objects={}", self.source.unavailable),
            format!("source_partial_clone={}", u8::from(self.source_partial)),
            format!("source_shallow_count={}", self.source_shallow_count),
            format!("stash_entries={}", self.stash_entries),
            format!("destination_tip_count={}", self.destination_tip_count),
            format!(
                "destination_shallow_count={}",
                self.destination_shallow_count
            ),
            format!("haves_used={}", self.haves_used),
            format!(
                "destination_tips_unknown_to_source={}",
                self.destination_tip_count.saturating_sub(self.haves_used)
            ),
            format!("missing_objects={}", self.missing.objects()),
            format!("missing_unavailable_objects={}", self.missing.unavailable),
            format!("missing_thin_pack_bytes={}", self.thin_pack.bytes),
            format!("missing_thin_pack_objects={}", self.thin_pack.objects),
            format!("missing_bytes_disk={}", self.missing.bytes_disk()),
        ];
        for (name, tally) in [
            ("commit", self.missing.commit),
            ("tree", self.missing.tree),
            ("blob", self.missing.blob),
            ("tag", self.missing.tag),
        ] {
            lines.push(format!("missing_{name}s={}", tally.count));
            lines.push(format!("missing_{name}_bytes_disk={}", tally.bytes_disk));
        }
        lines.push("gate_metric=missing_thin_pack_bytes".to_owned());
        lines.push(
            "gate_rule=R-N97,R-N113,R-N116 sent_bytes<=1.1*missing_thin_pack_bytes \
             sent_objects<=missing_objects oracle=upload-pack(haves=every_held_tip,\
             order=ancestors_first; shallow: frontier=source's, shallow_lines, --shallow)"
                .to_owned(),
        );
        lines.push(
            "informational=source_history_bytes,missing_bytes_disk,missing_*_bytes_disk".to_owned(),
        );
        lines
    }
}

/// Measure, read-only, what carrying `source` to `destination` would move.
///
/// # Errors
/// Refuses a path that carries a control character or is not a repository
/// root, a destination whose refs do not prove their history (R-N75), a
/// non-partial source that cannot reach an object its refs name, an
/// unreachable remote, or Git output that is not the shape these commands
/// promise. A refusal raised by a child process carries a bounded excerpt of
/// its stderr.
pub fn estimate(
    source: &Path,
    destination: &Destination,
) -> std::result::Result<CarryEstimate, Refused> {
    if has_control(source.as_os_str()) {
        return Err(BulkloadRefusal::PathNotPortable.into());
    }
    let own = run_probe(&mut local_probe(source))?;
    let offer = match destination {
        Destination::Local(path) => {
            if has_control(path.as_os_str()) {
                return Err(BulkloadRefusal::PathNotPortable.into());
            }
            run_probe(&mut local_probe(path))?
        }
        Destination::Remote { host, path } => {
            if !remote_host(host) || !remote_path(path) || !path.starts_with('/') {
                return Err(BulkloadRefusal::PathNotPortable.into());
            }
            run_probe(&mut ssh_command(host, &remote_command(path)))?
        }
    };
    if offer.partial {
        return Err(Refused::new(
            BulkloadRefusal::GitHavesUnprovable,
            Some("destination is a partial clone".to_owned()),
        ));
    }
    let shallow = !offer.shallow.is_empty();
    if shallow && offer.shallow != own.shallow {
        return Err(Refused::new(
            BulkloadRefusal::GitHavesUnprovable,
            Some("destination is shallow at a frontier other than the source's".to_owned()),
        ));
    }
    let repo = Repository {
        git_dir: own.git_dir,
        ceiling: own.ceiling,
    };
    let held = present(&repo, &offer.tips)?;
    // R-N116: M1 offers every held tip, ancestors first, so upload-pack keeps
    // each one as an exclusion; the request carries them all.
    let haves: Vec<String> = held.iter().map(|(value, _)| value.clone()).collect();
    let stash = stash_entries(&repo)?;
    let mut wants = own.tips;
    wants.extend(stash.iter().cloned());
    let closure = walk(&repo, &revisions(&wants, &[]), false)?;
    if !own.partial && closure.unavailable > 0 {
        return Err(Refused::new(
            BulkloadRefusal::GitInventoryMalformed,
            Some(format!(
                "source is not a partial clone but lacks {} objects its refs reach",
                closure.unavailable
            )),
        ));
    }
    // A fetch never wants what the destination already holds.
    let held_oids: BTreeSet<&String> = held.iter().map(|(value, _)| value).collect();
    let needed: BTreeSet<String> = wants
        .iter()
        .filter(|value| !held_oids.contains(value))
        .cloned()
        .collect();
    let request = revisions(&needed, &haves);
    let missing = walk(&repo, &request, shallow)?;
    let thin_pack = thin_pack(&repo, &request, missing, shallow.then_some(&offer.shallow))?;
    Ok(CarryEstimate {
        destination_tip_count: offer.tips.len(),
        destination_shallow_count: offer.shallow.len(),
        haves_used: held.len(),
        source_shallow_count: own.shallow.len(),
        source_partial: own.partial,
        stash_entries: stash.len(),
        source: closure,
        missing,
        thin_pack,
    })
}

fn has_control(path: &OsStr) -> bool {
    path.as_bytes().iter().any(u8::is_ascii_control)
}

/// The source repository as the probe resolved it.
struct Repository {
    /// `rev-parse --absolute-git-dir` of the probed root.
    git_dir: PathBuf,
    /// The root's parent: discovery never climbs above the root.
    ceiling: PathBuf,
}

/// [`git`] on the probed git dir, plus the estimate's no-write, no-network
/// hardening (F1): `--no-optional-locks`, `gc.auto=0` and
/// `core.hooksPath=/dev/null` come from [`git`]; this adds `--git-dir`,
/// `GIT_CEILING_DIRECTORIES`, `maintenance.auto=false` and
/// `GIT_NO_LAZY_FETCH`.
fn hardened(repository: &Repository) -> Command {
    let mut command = git(&repository.git_dir);
    let mut git_dir = std::ffi::OsString::from("--git-dir=");
    git_dir.push(&repository.git_dir);
    command
        .arg(git_dir)
        .args(["-c", "maintenance.auto=false"])
        .env("GIT_CEILING_DIRECTORIES", &repository.ceiling)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

/// What one repository offers, as [`PROBE_SCRIPT`] reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Probe {
    tips: BTreeSet<String>,
    shallow: BTreeSet<String>,
    partial: bool,
    git_dir: PathBuf,
    ceiling: PathBuf,
}

/// Exit status of [`PROBE_SCRIPT`] when its argument is not a repository root.
const PROBE_NOT_A_REPOSITORY: i32 = 4;
/// Exit status of [`PROBE_SCRIPT`] when Git may not honour
/// `GIT_NO_LAZY_FETCH`, or its version does not parse.
const PROBE_GIT_TOO_OLD: i32 = 5;

/// The offer probe, run by `bash -s -- PATH` locally and over ssh alike.
///
/// POSIX sh plus `local`, so bash 3.2 (macOS) and bash 5 (sting) read it the
/// same way. It runs no program but Git (`git version`, `rev-parse`,
/// `config --get*` and `for-each-ref`) and reads the shallow and alternates
/// files with the shell's `read`. Output: `gitdir` and `ceiling` lines
/// (absolute paths), one `partial 0|1` line, then `shallow` and `tip` lines
/// carrying one oid each, then `end`.
pub const PROBE_SCRIPT: &str = r#"set -eu
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS GIT_CEILING_DIRECTORIES GIT_DISCOVERY_ACROSS_FILESYSTEM
export GIT_NO_LAZY_FETCH=1 GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0 GIT_NO_REPLACE_OBJECTS=1 GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
g() { git --no-optional-locks -c maintenance.auto=false -c gc.auto=0 -c core.hooksPath=/dev/null -c core.fsmonitor=false "$@"; }
version=$(git version) || exit 5
case "$version" in 'git version '*) version=${version#git version } ;; *) exit 5 ;; esac
case "$version" in
  *' (Apple Git-'*')')
    vendor=${version#* (Apple Git-}
    vendor=${vendor%)}
    case "$vendor" in ''|*[!0-9]*) exit 5 ;; esac
    version=${version%% (Apple Git-*} ;;
esac
case "$version" in ''|*[!0-9.]*|.*|*.|*..*) exit 5 ;; esac
major=${version%%.*}
rest=${version#*.}
[ "$rest" != "$version" ] || exit 5
minor=${rest%%.*}
patch=${rest#*.}
[ "$patch" != "$rest" ] || exit 5
case "$patch" in *.*) exit 5 ;; esac
for part in "$major" "$minor" "$patch"; do case "$part" in ''|???????*) exit 5 ;; esac; done
if [ "$major" -lt 2 ]; then exit 5; fi
if [ "$major" -eq 2 ] && [ "$minor" -lt 45 ]; then
  case "$minor" in 44) need=1 ;; 43) need=4 ;; 42) need=2 ;; 41) need=1 ;; 40) need=2 ;; 39) need=4 ;; *) exit 5 ;; esac
  [ "$patch" -ge "$need" ] || exit 5
fi
[ "$#" -eq 1 ] || exit 2
case "$1" in *[[:cntrl:]]*) exit 4 ;; esac
root=$(cd -P -- "$1" 2>/dev/null && pwd -P) || exit 4
GIT_CEILING_DIRECTORIES=${root%/*}
[ -n "$GIT_CEILING_DIRECTORIES" ] || GIT_CEILING_DIRECTORIES=/
export GIT_CEILING_DIRECTORIES
bare=$(g -C "$root" rev-parse --is-bare-repository 2>/dev/null) || exit 4
if [ "$bare" = true ]; then
  top=$(g -C "$root" rev-parse --absolute-git-dir 2>/dev/null) || exit 4
else
  top=$(g -C "$root" rev-parse --show-toplevel 2>/dev/null) || exit 4
fi
[ "$top" = "$root" ] || exit 4
gitdir=$(g -C "$root" rev-parse --absolute-git-dir)
case "$gitdir" in ''|*[[:cntrl:]]*) exit 4 ;; esac
common=$(g -C "$root" rev-parse --path-format=absolute --git-common-dir)
shallow=$(g -C "$root" rev-parse --path-format=absolute --git-path shallow)
objects=$(g -C "$root" rev-parse --path-format=absolute --git-path objects)
partial=0
partial_config() {
  if value=$(g "$@" --get extensions.partialClone); then
    [ -z "$value" ] || partial=1
  else
    [ "$?" -eq 1 ] || exit 1
  fi
  if value=$(g "$@" --type=bool --get-regexp '^remote\..*\.promisor$'); then
    case "$value" in *' true'*) partial=1 ;; esac
  else
    [ "$?" -eq 1 ] || exit 1
  fi
  if value=$(g "$@" --get-regexp '^(remote\..*|core)\.partialclonefilter$'); then
    [ -z "$value" ] || partial=1
  else
    [ "$?" -eq 1 ] || exit 1
  fi
}
partial_config -C "$root" config
for file in "$common/config" "$common/config.worktree" "$common"/worktrees/*/config.worktree; do
  [ -f "$file" ] || continue
  partial_config config --file "$file" --includes
done
promisor_packs() {
  local store=$1 depth=$2 pack alternate
  for pack in "$store"/pack/*.promisor; do [ ! -e "$pack" ] || partial=1; done
  [ -f "$store/info/alternates" ] || return 0
  [ "$depth" -lt 5 ] || exit 1
  while IFS= read -r alternate || [ -n "$alternate" ]; do
    case "$alternate" in
      ''|'#'*) continue ;;
      '"'*) exit 1 ;;
      /*) ;;
      *) alternate=$store/$alternate ;;
    esac
    [ ! -d "$alternate" ] || promisor_packs "$alternate" $((depth + 1))
  done < "$store/info/alternates"
}
promisor_packs "$objects" 0
printf 'gitdir %s\n' "$gitdir"
printf 'ceiling %s\n' "$GIT_CEILING_DIRECTORIES"
printf 'partial %s\n' "$partial"
if [ -e "$shallow" ]; then
  while IFS= read -r line || [ -n "$line" ]; do printf 'shallow %s\n' "$line"; done < "$shallow"
fi
g --git-dir="$common" for-each-ref '--format=tip %(objectname)'
if head=$(g --git-dir="$common" rev-parse -q --verify HEAD); then printf 'tip %s\n' "$head"; fi
for admin in "$common"/worktrees/*; do
  [ -f "$admin/HEAD" ] || continue
  if head=$(g --git-dir="$admin" rev-parse -q --verify HEAD); then printf 'tip %s\n' "$head"; fi
  g --git-dir="$admin" for-each-ref '--format=tip %(objectname)' refs/worktree/ refs/bisect/ refs/rewritten/
done
printf 'end\n'
"#;

fn local_probe(repository: &Path) -> Command {
    let mut command = Command::new("bash");
    command.args(["-s", "--"]).arg(repository);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(key);
    }
    command.env("GIT_NO_LAZY_FETCH", "1");
    command
}

/// `ssh -T -oBatchMode=yes -oConnectTimeout=15 HOST COMMAND`.
fn ssh_command(host: &str, command: &str) -> Command {
    let mut ssh = Command::new("ssh");
    ssh.args([
        "-T",
        "-oBatchMode=yes",
        "-oConnectTimeout=15",
        "-oServerAliveInterval=15",
        "-oServerAliveCountMax=4",
        host,
        command,
    ]);
    ssh
}

/// The remote login shell (fish on sting) parses this line. `env` sets the
/// variable in any shell, and single quotes mean the same thing to POSIX sh
/// and fish for the [`remote_path`] character set.
fn remote_command(path: &str) -> String {
    format!("env GIT_NO_LAZY_FETCH=1 bash -s -- '{path}'")
}

/// Run the probe with [`PROBE_SCRIPT`] on stdin and parse its answer.
fn run_probe(command: &mut Command) -> std::result::Result<Probe, Refused> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| BulkloadRefusal::GitUnavailable)?;
    let mut stdin = child.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
    let (written, result) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(PROBE_SCRIPT.as_bytes()));
        let result = child.wait_with_output();
        (writer.join(), result)
    });
    let result = result?;
    let refusal = match result.status.code() {
        Some(0) => None,
        Some(PROBE_NOT_A_REPOSITORY) => Some(BulkloadRefusal::GitRepositoryNotAtPath),
        Some(PROBE_GIT_TOO_OLD | 255) | None => Some(BulkloadRefusal::GitUnavailable),
        Some(_) => Some(BulkloadRefusal::GitInventoryMalformed),
    };
    if let Some(refusal) = refusal {
        return Err(Refused::new(refusal, detail(&result.stderr)));
    }
    // A child that answered in full read its whole script.
    written.map_err(|_| BulkloadRefusal::Io(None))??;
    Ok(parse_probe(&result.stdout)?)
}

fn parse_probe(stdout: &[u8]) -> Result<Probe> {
    let text = std::str::from_utf8(stdout).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let mut probe = Probe::default();
    let mut partial = None;
    let mut git_dir = None;
    let mut ceiling = None;
    let mut ended = false;
    for line in text.lines() {
        if ended {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        match line.split_once(' ') {
            Some(("tip", value)) if oid(value) => {
                probe.tips.insert(value.to_owned());
            }
            Some(("shallow", value)) if oid(value) => {
                probe.shallow.insert(value.to_owned());
            }
            Some(("partial", flag @ ("0" | "1"))) if partial.is_none() => {
                partial = Some(flag == "1");
            }
            Some(("gitdir", value)) if value.starts_with('/') && git_dir.is_none() => {
                git_dir = Some(PathBuf::from(value));
            }
            Some(("ceiling", value)) if value.starts_with('/') && ceiling.is_none() => {
                ceiling = Some(PathBuf::from(value));
            }
            None if line == "end" => ended = true,
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        }
    }
    if !ended {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    probe.partial = partial.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    probe.git_dir = git_dir.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    probe.ceiling = ceiling.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    Ok(probe)
}

/// Run `command`, feeding `bytes` on stdin from a separate thread so a large
/// answer can never deadlock against an unread request.
fn feed(command: &mut Command, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
    let (writer, result) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(bytes));
        let result = child.wait_with_output();
        (writer.join(), result)
    });
    writer.map_err(|_| BulkloadRefusal::Io(None))??;
    let result = result?;
    if !result.status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(result.stdout)
}

fn run(command: &mut Command) -> Result<Vec<u8>> {
    let result = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !result.status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(result.stdout)
}

/// Destination tips that exist as objects in `source`, with their types, in
/// oid order. With `GIT_NO_LAZY_FETCH` a partial source answers `missing` for
/// a tip only its promisor holds instead of fetching it (F1).
fn present(source: &Repository, tips: &BTreeSet<String>) -> Result<Vec<(String, String)>> {
    if tips.is_empty() {
        return Ok(Vec::new());
    }
    let mut request = String::new();
    for tip in tips {
        request.push_str(tip);
        request.push('\n');
    }
    let answer = feed(
        hardened(source).args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
        request.as_bytes(),
    )?;
    let answer = String::from_utf8(answer).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let mut haves = Vec::new();
    let mut answered = 0_usize;
    for line in answer.lines() {
        answered += 1;
        let (value, kind) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !tips.contains(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if kind != "missing" {
            haves.push((value.to_owned(), kind.to_owned()));
        }
    }
    if answered != tips.len() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(haves)
}

/// Every stash reflog entry, newest first; empty when there is no stash.
fn stash_entries(source: &Repository) -> Result<Vec<String>> {
    let stash =
        run(hardened(source).args(["for-each-ref", "--format=%(objectname)", "refs/stash"]))?;
    if stash.is_empty() {
        return Ok(Vec::new());
    }
    let entries = run(hardened(source).args(["reflog", "show", "--format=%H", "refs/stash"]))?;
    let entries = String::from_utf8(entries).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    entries
        .lines()
        .map(|line| {
            if oid(line) {
                Ok(line.to_owned())
            } else {
                Err(BulkloadRefusal::GitInventoryMalformed)
            }
        })
        .collect()
}

/// `rev-list`/`pack-objects --revs` stdin: the wants, then `--not` and the
/// haves when there are any.
fn revisions(wants: &BTreeSet<String>, haves: &[String]) -> String {
    let mut request = String::new();
    for value in wants {
        request.push_str(value);
        request.push('\n');
    }
    if !haves.is_empty() {
        request.push_str("--not\n");
        for value in haves {
            request.push_str(value);
            request.push('\n');
        }
    }
    request
}

/// `rev-list --objects --missing=print --stdin`, with its `?` lines counted as
/// unavailable and every other oid forwarded to `cat-file --batch-check`,
/// tallied by type. `edge_aggressive` walks as `pack-objects --shallow` does
/// (`--objects-edge-aggressive`); its `-oid` edge lines are not sent objects.
fn walk(source: &Repository, request: &str, edge_aggressive: bool) -> Result<Tally> {
    let mut list = hardened(source);
    list.args([
        "rev-list",
        "--objects",
        "--no-object-names",
        "--missing=print",
    ]);
    if edge_aggressive {
        list.arg("--objects-edge-aggressive");
    }
    let mut list = list
        .arg("--stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let check = hardened(source)
        .args([
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize:disk)",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut check = match check {
        Ok(check) => check,
        Err(error) => {
            drop(list.stdin.take());
            drop(list.stdout.take());
            list.wait()?;
            return Err(error.into());
        }
    };
    let pipes = (
        list.stdin.take(),
        list.stdout.take(),
        check.stdin.take(),
        check.stdout.take(),
    );
    let outcome = std::thread::scope(|scope| {
        let (Some(mut list_in), Some(list_out), Some(check_in), Some(check_out)) = pipes else {
            return Err(BulkloadRefusal::Io(None));
        };
        let writer = scope.spawn(move || list_in.write_all(request.as_bytes()));
        let reader = scope.spawn(move || tally(BufReader::new(check_out)));
        let unavailable = forward(BufReader::new(list_out), BufWriter::new(check_in));
        let written = writer.join().map_err(|_| BulkloadRefusal::Io(None))?;
        let tallied = reader.join().map_err(|_| BulkloadRefusal::Io(None))?;
        let unavailable = unavailable?;
        written?;
        let mut tallied = tallied?;
        tallied.unavailable += unavailable;
        Ok(tallied)
    });
    let list_status = list.wait()?;
    let check_status = check.wait()?;
    let tallied = outcome?;
    if !list_status.success() || !check_status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(tallied)
}

/// Copy `rev-list` oids to `cat-file`, counting `?oid` lines instead.
fn forward(list: impl BufRead, mut check: impl Write) -> Result<u64> {
    let mut unavailable = 0_u64;
    for line in list.lines() {
        let line = line?;
        if let Some(value) = line.strip_prefix('?') {
            if !oid(value) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            unavailable += 1;
        } else if let Some(edge) = line.strip_prefix('-') {
            if !oid(edge) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
        } else {
            if !oid(&line) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            check.write_all(line.as_bytes())?;
            check.write_all(b"\n")?;
        }
    }
    check.flush()?;
    Ok(unavailable)
}

fn tally(reader: impl BufRead) -> Result<Tally> {
    let mut tally = Tally::default();
    for line in reader.lines() {
        tally.add(&line?)?;
    }
    Ok(tally)
}

/// `pack-objects --stdout --thin --revs --delta-base-offset` over `request`,
/// its stdout counted in a reused buffer and discarded (R-N74). Nothing is
/// built when `missing` holds no object: there is then no pack to send.
///
/// `pack.useSparse=false` and `pack.useBitmaps=false`: sparse edge marking and
/// bitmap traversal each select a set other than the walk (sparse added an
/// object the haves reach, for bulkload in the cohort run; bitmaps leave out
/// objects deep in the haves' history). The M1 sender must pin both too. A
/// pack whose header count still differs from the walk is refused.
///
/// For a shallow destination (`shallow` is its frontier, equal to the
/// source's), this runs as upload-pack runs it for a shallow client:
/// `git --shallow-file '' pack-objects --shallow`, fed `--shallow <oid>` for
/// each boundary commit before the request.
fn thin_pack(
    source: &Repository,
    request: &str,
    missing: Tally,
    shallow: Option<&BTreeSet<String>>,
) -> Result<ThinPack> {
    if missing.objects() == 0 {
        return Ok(ThinPack::default());
    }
    let mut pack = hardened(source);
    let mut input = String::new();
    if let Some(frontier) = shallow {
        pack.args(["--shallow-file", ""]);
        for value in frontier {
            input.push_str("--shallow ");
            input.push_str(value);
            input.push('\n');
        }
    }
    input.push_str(request);
    pack.args([
        "-c",
        "pack.useSparse=false",
        "-c",
        "pack.useBitmaps=false",
        "pack-objects",
        "--stdout",
        "--thin",
        "--revs",
        "--delta-base-offset",
        "--missing=allow-any",
        "-q",
    ]);
    if shallow.is_some() {
        pack.arg("--shallow");
    }
    let mut pack = pack
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let pipes = (pack.stdin.take(), pack.stdout.take());
    let counted = std::thread::scope(|scope| {
        let (Some(mut stdin), Some(stdout)) = pipes else {
            return Err(BulkloadRefusal::Io(None));
        };
        let writer = scope.spawn(move || stdin.write_all(input.as_bytes()));
        let counted = count_pack(stdout);
        writer.join().map_err(|_| BulkloadRefusal::Io(None))??;
        counted
    });
    let status = pack.wait()?;
    let counted = counted?;
    if !status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    if counted.objects != missing.objects() {
        return Err(BulkloadRefusal::ContractSelfInconsistent);
    }
    Ok(counted)
}

/// Count a pack stream's bytes and read the object count from its header.
fn count_pack(mut stream: impl Read) -> Result<ThinPack> {
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut header = Vec::with_capacity(12);
    let mut bytes = 0_u64;
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        let chunk = buffer.get(..read).ok_or(BulkloadRefusal::Io(None))?;
        if header.len() < 12 {
            let wanted = (12 - header.len()).min(chunk.len());
            header.extend_from_slice(chunk.get(..wanted).ok_or(BulkloadRefusal::Io(None))?);
        }
        bytes += u64::try_from(read).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    }
    let (Some(b"PACK"), Some(count)) = (header.get(..4), header.get(8..12)) else {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    };
    let count: [u8; 4] = count
        .try_into()
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    Ok(ThinPack {
        bytes,
        objects: u64::from(u32::from_be_bytes(count)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::super::{output, text};
    use super::*;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "bulkload-estimate-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn repo(&self, name: &str) -> PathBuf {
            let repo = self.root.join(name);
            std::fs::create_dir(&repo).unwrap();
            output(git(&repo).args(["init", "--template=", "-b", "main"])).unwrap();
            configure(&repo);
            repo
        }

        /// A `--depth` clone needs a transport, so go through `file://`.
        fn shallow_clone(&self, origin: &Path, name: &str, depth: u32) -> PathBuf {
            let repo = self.root.join(name);
            let url = format!("file://{}", origin.display());
            output(
                git(&self.root)
                    .args(["clone", "--quiet", "--no-local", "--template="])
                    .args([
                        format!("--depth={depth}").as_str(),
                        "--no-single-branch",
                        url.as_str(),
                        repo.to_str().unwrap(),
                    ]),
            )
            .unwrap();
            configure(&repo);
            repo
        }

        /// A `--filter=blob:none` clone; `origin` must allow filters.
        fn partial_clone(&self, origin: &Path, name: &str, checkout: bool) -> PathBuf {
            let repo = self.root.join(name);
            let url = format!("file://{}", origin.display());
            let mut command = git(&self.root);
            command.args(["clone", "--quiet", "--template=", "--filter=blob:none"]);
            if !checkout {
                command.arg("--no-checkout");
            }
            output(command.args([url.as_str(), repo.to_str().unwrap()])).unwrap();
            configure(&repo);
            repo
        }

        fn plain_clone(&self, origin: &Path, name: &str) -> PathBuf {
            let repo = self.root.join(name);
            let url = format!("file://{}", origin.display());
            output(git(&self.root).args([
                "clone",
                "--quiet",
                "--no-local",
                "--template=",
                url.as_str(),
                repo.to_str().unwrap(),
            ]))
            .unwrap();
            configure(&repo);
            repo
        }
    }

    fn allow_filter(origin: &Path) {
        output(git(origin).args(["config", "uploadpack.allowFilter", "true"])).unwrap();
        output(git(origin).args(["config", "uploadpack.allowAnySHA1InWant", "true"])).unwrap();
    }

    fn pack_count(repo: &Path) -> usize {
        let pack = text(git(repo).args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects/pack",
        ]))
        .unwrap();
        std::fs::read_dir(pack)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "pack")
            })
            .count()
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn configure(repo: &Path) {
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@localhost"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ] {
            output(git(repo).args(["config", key, value])).unwrap();
        }
    }

    fn commit(repo: &Path, path: &str, content: &str) -> String {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
        output(git(repo).args(["add", "--all"])).unwrap();
        output(git(repo).args(["commit", "--quiet", "-m", path])).unwrap();
        text(git(repo).args(["rev-parse", "HEAD"])).unwrap()
    }

    /// Point `destination` at exactly `refs` of `source` (oid, name) without
    /// a transport: share the objects by copying the object directory.
    fn mirror(source: &Path, destination: &Path, refs: &[(&str, &str)]) {
        let from = source.join(".git/objects");
        let to = destination.join(".git/objects");
        copy_tree(&from, &to);
        for (value, name) in refs {
            output(git(destination).args(["update-ref", name, value])).unwrap();
        }
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else if !target.exists() {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// The hand computation the estimate must agree with: an independent
    /// `rev-list --objects` over explicit revisions, counted by line.
    fn hand_count(repo: &Path, revisions: &[&str]) -> u64 {
        let listed = text(git(repo).args(["rev-list", "--objects"]).args(revisions)).unwrap();
        u64::try_from(listed.lines().filter(|line| !line.is_empty()).count()).unwrap()
    }

    fn snapshot(repo: &Path) -> (String, Vec<u8>) {
        let refs = text(git(repo).args(["for-each-ref"])).unwrap();
        let objects = text(git(repo).args(["count-objects", "-v"])).unwrap();
        let shallow = std::fs::read(repo.join(".git/shallow")).unwrap_or_default();
        (format!("{refs}\n{objects}"), shallow)
    }

    #[test]
    fn destination_holding_everything_needs_nothing() {
        let fixture = Fixture::new("all");
        let source = fixture.repo("source");
        let base = commit(&source, "a.txt", "a");
        let side = {
            output(git(&source).args(["checkout", "--quiet", "-b", "side"])).unwrap();
            commit(&source, "dir/b.txt", "b")
        };
        output(git(&source).args(["checkout", "--quiet", "main"])).unwrap();
        let destination = fixture.repo("destination");
        mirror(
            &source,
            &destination,
            &[(&base, "refs/heads/main"), (&side, "refs/heads/side")],
        );
        let before = (snapshot(&source), snapshot(&destination));
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        assert_eq!(before, (snapshot(&source), snapshot(&destination)));
        assert_eq!(result.destination_tip_count, 2);
        assert_eq!(result.haves_used, 2);
        assert_eq!(result.missing, Tally::default());
        assert_eq!(result.missing.objects(), 0);
        assert_eq!(result.source.objects(), hand_count(&source, &["--all"]));
        assert!(result.source.bytes_disk() > 0);
    }

    #[test]
    fn destination_missing_one_branch_needs_exactly_that_branch() {
        let fixture = Fixture::new("branch");
        let source = fixture.repo("source");
        let base = commit(&source, "a.txt", "a");
        output(git(&source).args(["checkout", "--quiet", "-b", "feature"])).unwrap();
        // One commit, its root tree, one new subtree and one new blob.
        commit(&source, "dir/new.txt", "only on feature");
        output(git(&source).args(["checkout", "--quiet", "main"])).unwrap();
        let destination = fixture.repo("destination");
        mirror(&source, &destination, &[(&base, "refs/heads/main")]);
        let result = estimate(&source, &Destination::Local(destination)).unwrap();
        assert_eq!(result.haves_used, 1);
        assert_eq!(result.missing.objects(), 4);
        assert_eq!(result.missing.commit.count, 1);
        assert_eq!(result.missing.tree.count, 2);
        assert_eq!(result.missing.blob.count, 1);
        assert_eq!(
            result.missing.objects(),
            hand_count(&source, &["feature", "--not", &base])
        );
        assert!(result.missing.bytes_disk() > 0);
        assert!(result.missing.bytes_disk() < result.source.bytes_disk());
    }

    #[test]
    fn destination_tip_unknown_to_source_is_not_a_have() {
        let fixture = Fixture::new("unknown");
        let source = fixture.repo("source");
        let base = commit(&source, "a.txt", "a");
        let destination = fixture.repo("destination");
        mirror(&source, &destination, &[(&base, "refs/heads/main")]);
        output(git(&destination).args(["checkout", "--quiet", "-f", "main"])).unwrap();
        output(git(&destination).args(["checkout", "--quiet", "-b", "theirs"])).unwrap();
        let unknown = commit(&destination, "theirs.txt", "only at the destination");
        // Two equal tips de-duplicate to one.
        output(git(&destination).args(["update-ref", "refs/tags/same", &base])).unwrap();
        output(git(&source).args(["checkout", "--quiet", "-b", "ours"])).unwrap();
        commit(&source, "ours.txt", "only at the source");
        std::fs::write(source.join("a.txt"), "stashed").unwrap();
        output(git(&source).args(["stash", "push", "--quiet"])).unwrap();
        let result = estimate(&source, &Destination::Local(destination)).unwrap();
        assert_eq!(result.destination_tip_count, 2);
        assert_eq!(result.haves_used, 1);
        assert_eq!(result.stash_entries, 1);
        assert!(text(git(&source).args(["cat-file", "-t", &unknown])).is_err());
        let stash = text(git(&source).args(["rev-parse", "refs/stash"])).unwrap();
        assert_eq!(
            result.missing.objects(),
            hand_count(&source, &["--all", &stash, "--not", &base])
        );
        // ours: commit + tree + blob. The stash adds its worktree and index
        // commits, one new worktree tree and the stashed blob; the index tree
        // is ours's tree again.
        assert_eq!(result.missing.objects(), 7);
        assert_eq!(result.missing.commit.count, 3);
    }

    #[test]
    fn shallow_source_walks_only_what_it_holds() {
        let fixture = Fixture::new("shallow");
        let origin = fixture.repo("origin");
        commit(&origin, "one.txt", "1");
        commit(&origin, "two.txt", "2");
        let third = commit(&origin, "three.txt", "3");
        let source = fixture.shallow_clone(&origin, "source", 1);
        output(git(&source).args(["checkout", "--quiet", "-b", "local"])).unwrap();
        commit(&source, "four.txt", "4");
        let destination = fixture.repo("destination");
        mirror(&source, &destination, &[(&third, "refs/heads/main")]);
        let result = estimate(&source, &Destination::Local(destination)).unwrap();
        assert_eq!(result.source_shallow_count, 1);
        assert_eq!(result.haves_used, 1);
        // The shallow closure is one commit, one tree and three blobs.
        assert_eq!(result.source.objects() - result.missing.objects(), 5);
        assert_eq!(result.source.objects(), hand_count(&source, &["--all"]));
        assert_eq!(
            result.missing.objects(),
            hand_count(&source, &["--all", "--not", &third])
        );
        assert_eq!(result.missing.objects(), 3);
    }

    /// R-N75 (F2): a shallow destination whose frontier differs from the
    /// source's cannot prove it holds its tips' history, so the verb refuses.
    #[test]
    fn shallow_destination_with_a_different_frontier_is_refused() {
        let fixture = Fixture::new("shallow-destination");
        let source = fixture.repo("source");
        commit(&source, "one.txt", "1");
        commit(&source, "two.txt", "2");
        let destination = fixture.shallow_clone(&source, "destination", 1);
        let refused = estimate(&source, &Destination::Local(destination)).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
    }

    /// R-N75: matching shallow frontiers still estimate.
    #[test]
    fn shallow_destination_with_the_source_frontier_is_estimated() {
        let fixture = Fixture::new("shallow-match");
        let origin = fixture.repo("origin");
        commit(&origin, "one.txt", "1");
        commit(&origin, "two.txt", "2");
        let third = commit(&origin, "three.txt", "3");
        let source = fixture.shallow_clone(&origin, "source", 1);
        output(git(&source).args(["checkout", "--quiet", "-b", "local"])).unwrap();
        commit(&source, "four.txt", "4");
        let destination = fixture.shallow_clone(&origin, "destination", 1);
        let result = estimate(&source, &Destination::Local(destination)).unwrap();
        assert_eq!(result.source_shallow_count, 1);
        assert_eq!(result.destination_shallow_count, 1);
        assert_eq!(result.haves_used, 1);
        assert_eq!(
            result.missing.objects(),
            hand_count(&source, &["--all", "--not", &third])
        );
        assert_eq!(result.missing.objects(), 3);
    }

    /// R-N75 (F3): a partial-clone destination lacks objects its refs name.
    #[test]
    fn partial_clone_destination_is_refused() {
        let fixture = Fixture::new("partial-destination");
        let origin = fixture.repo("origin");
        allow_filter(&origin);
        commit(&origin, "one.txt", "1");
        commit(&origin, "two.txt", "2");
        let source = fixture.plain_clone(&origin, "source");
        let destination = fixture.partial_clone(&origin, "destination", false);
        let refused = estimate(&source, &Destination::Local(destination)).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
        // `remote.*.promisor=true` alone also marks a partial clone.
        let promisor = fixture.plain_clone(&origin, "promisor");
        output(git(&promisor).args(["config", "remote.origin.promisor", "true"])).unwrap();
        let refused = estimate(&source, &Destination::Local(promisor)).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
    }

    /// F1: a partial-clone source must not lazily fetch. A destination tip
    /// only upstream holds stays unknown, and the source gains no pack.
    #[test]
    fn partial_clone_source_never_fetches_and_upstream_tip_is_not_a_have() {
        let fixture = Fixture::new("partial-source");
        let origin = fixture.repo("origin");
        allow_filter(&origin);
        commit(&origin, "one.txt", "1");
        let source = fixture.partial_clone(&origin, "source", true);
        let upstream_only = commit(&origin, "two.txt", "upstream only");
        let destination = fixture.plain_clone(&origin, "destination");
        let packs = pack_count(&source);
        let before = snapshot(&source);
        let result = estimate(&source, &Destination::Local(destination)).unwrap();
        assert_eq!(pack_count(&source), packs, "the source fetched a pack");
        assert_eq!(snapshot(&source), before);
        assert_eq!(result.destination_tip_count, 1);
        assert_eq!(result.haves_used, 0);
        let lookup = text(git(&source).env("GIT_NO_LAZY_FETCH", "1").args([
            "cat-file",
            "-t",
            &upstream_only,
        ]));
        assert!(lookup.is_err());
    }

    /// F8: objects the partial source lacks are counted, not dropped.
    #[test]
    fn partial_clone_source_counts_unavailable_objects() {
        let fixture = Fixture::new("partial-unavailable");
        let origin = fixture.repo("origin");
        allow_filter(&origin);
        commit(&origin, "a.txt", "1");
        commit(&origin, "a.txt", "2");
        commit(&origin, "a.txt", "3");
        let source = fixture.partial_clone(&origin, "source", false);
        let destination = fixture.repo("destination");
        let packs = pack_count(&source);
        let result = estimate(&source, &Destination::Local(destination)).unwrap();
        assert_eq!(pack_count(&source), packs);
        // Three blob versions of a.txt, none fetched.
        assert_eq!(result.source.unavailable, 3);
        assert_eq!(result.missing.unavailable, 3);
        assert_eq!(result.source.blob.count, 0);
        assert_eq!(result.source.commit.count, 3);
        assert!(result.source_partial);
        // Three commits and three trees are sendable; the blobs are not.
        assert_eq!(result.thin_pack.objects, 6);
        assert_eq!(result.thin_pack.objects, result.missing.objects());
    }

    /// F4: SOURCE and DEST must each be a repository root, not a path Git
    /// would resolve to an enclosing repository.
    #[test]
    fn paths_that_are_not_a_repository_root_are_refused() {
        let fixture = Fixture::new("not-a-repo");
        let source = fixture.repo("source");
        commit(&source, "dir/a.txt", "a");
        let destination = fixture.repo("destination");
        std::fs::create_dir(destination.join("sub")).unwrap();
        let plain = fixture.root.join("plain");
        std::fs::create_dir(&plain).unwrap();
        let not_at_path = |source: &Path, destination: PathBuf| {
            estimate(source, &Destination::Local(destination))
                .unwrap_err()
                .refusal
        };
        for (source, destination) in [
            (source.join("dir"), destination.clone()),
            (source.clone(), destination.join("sub")),
            (source.clone(), destination.join(".git")),
            (source.clone(), plain.clone()),
            (plain, destination.clone()),
            (source.clone(), fixture.root.join("absent")),
        ] {
            assert_eq!(
                not_at_path(&source, destination),
                BulkloadRefusal::GitRepositoryNotAtPath
            );
        }
        assert!(estimate(&source, &Destination::Local(destination)).is_ok());
    }

    /// F5: the remote probe must find the shallow file of a linked worktree
    /// and of a bare repository whose name does not end in `.git`.
    #[test]
    fn remote_probe_reads_shallow_of_linked_worktree_and_bare_repository() {
        let fixture = Fixture::new("remote-shallow");
        let origin = fixture.repo("origin");
        commit(&origin, "one.txt", "1");
        let tip = commit(&origin, "two.txt", "2");
        let shallow = fixture.shallow_clone(&origin, "shallow", 1);
        let linked = fixture.root.join("linked");
        output(
            git(&shallow)
                .args(["worktree", "add", "--quiet", "--detach"])
                .arg(&linked)
                .arg(&tip),
        )
        .unwrap();
        let bare = fixture.root.join("bare-shallow");
        let url = format!("file://{}", origin.display());
        output(git(&fixture.root).args([
            "clone",
            "--quiet",
            "--bare",
            "--depth=1",
            url.as_str(),
            bare.to_str().unwrap(),
        ]))
        .unwrap();
        // The exact script the remote side runs, run here through `bash -s`.
        for path in [&linked, &bare] {
            let probe = run_probe(&mut local_probe(path)).unwrap();
            assert_eq!(
                probe.shallow,
                BTreeSet::from([tip.clone()]),
                "{}",
                path.display()
            );
            assert!(probe.tips.contains(&tip), "{}", path.display());
            assert!(!probe.partial);
        }
        // A shallow destination at the source's frontier estimates.
        let result = estimate(&shallow, &Destination::Local(bare)).unwrap();
        assert_eq!(result.destination_shallow_count, 1);
        assert_eq!(result.missing.objects(), 0);
    }

    /// F6: per-worktree refs of every worktree are walked, and the result does
    /// not depend on which worktree path names the source.
    #[test]
    fn every_worktree_ref_is_walked_whichever_worktree_is_named() {
        let fixture = Fixture::new("worktrees");
        let source = fixture.repo("source");
        let base = commit(&source, "a.txt", "a");
        output(git(&source).args(["checkout", "--quiet", "-b", "scratch"])).unwrap();
        let main_keep = commit(&source, "main-keep.txt", "main keep");
        output(git(&source).args(["update-ref", "refs/worktree/main-keep", &main_keep])).unwrap();
        output(git(&source).args(["checkout", "--quiet", "main"])).unwrap();
        output(git(&source).args(["branch", "--quiet", "-D", "scratch"])).unwrap();
        let linked = fixture.root.join("linked");
        output(
            git(&source)
                .args(["worktree", "add", "--quiet", "--detach"])
                .arg(&linked)
                .arg(&base),
        )
        .unwrap();
        let bisect = commit(&linked, "bisect.txt", "bisect");
        output(git(&linked).args(["update-ref", "refs/bisect/bad", &bisect])).unwrap();
        output(git(&linked).args(["checkout", "--quiet", "--detach", &base])).unwrap();
        let keep = commit(&linked, "keep.txt", "keep");
        output(git(&linked).args(["update-ref", "refs/worktree/keep", &keep])).unwrap();
        output(git(&linked).args(["checkout", "--quiet", "--detach", &base])).unwrap();
        let head = commit(&linked, "head.txt", "detached head");
        let destination = fixture.repo("destination");
        mirror(&source, &destination, &[(&base, "refs/heads/main")]);
        let expected = hand_count(
            &source,
            &[&main_keep, &bisect, &keep, &head, "--not", &base],
        );
        // Four commits, each with a root tree and one new blob.
        assert_eq!(expected, 12);
        let from_main = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        let from_linked = estimate(&linked, &Destination::Local(destination)).unwrap();
        assert_eq!(from_main.missing.objects(), expected);
        assert_eq!(from_linked.missing.objects(), expected);
        assert_eq!(from_main, from_linked);
    }

    #[test]
    fn destinations_parse_conservatively() {
        assert_eq!(
            Destination::parse(OsStr::new("sting:/srv/fast-local/jess/git/glorious.build"))
                .unwrap(),
            Destination::Remote {
                host: "sting".to_owned(),
                path: "/srv/fast-local/jess/git/glorious.build".to_owned()
            }
        );
        assert_eq!(
            Destination::parse(OsStr::new("/tmp/a:b")).unwrap(),
            Destination::Local(PathBuf::from("/tmp/a:b"))
        );
        assert_eq!(
            Destination::parse(OsStr::new("./x:y")).unwrap(),
            Destination::Local(PathBuf::from("./x:y"))
        );
        for refused in [
            "sting:relative",
            "sting:/a b",
            "sting:/a'b",
            "sting:/a;b",
            "sting:/a/../b",
            "-oProxyCommand=x:/a",
            "st$ing:/a",
        ] {
            assert!(
                Destination::parse(OsStr::new(refused)).is_err(),
                "{refused}"
            );
        }
        assert_eq!(
            remote_command("/srv/fast-local/jess/git/glorious.build"),
            "env GIT_NO_LAZY_FETCH=1 bash -s -- '/srv/fast-local/jess/git/glorious.build'"
        );
    }

    #[test]
    fn estimate_lines_are_key_value() {
        let mut result = CarryEstimate {
            destination_tip_count: 3,
            destination_shallow_count: 0,
            haves_used: 2,
            source_shallow_count: 0,
            source_partial: false,
            stash_entries: 0,
            source: Tally::default(),
            missing: Tally::default(),
            thin_pack: ThinPack {
                bytes: 7,
                objects: 2,
            },
        };
        result.missing.blob = TypeTally {
            count: 2,
            bytes_disk: 10,
        };
        let lines = result.lines();
        assert!(lines.iter().all(|line| line.split_once('=').is_some()));
        assert!(lines.contains(&"missing_objects=2".to_owned()));
        assert!(lines.contains(&"missing_bytes_disk=10".to_owned()));
        assert!(lines.contains(&"destination_tips_unknown_to_source=1".to_owned()));
        assert!(lines.contains(&"missing_thin_pack_bytes=7".to_owned()));
        assert!(lines.contains(&"gate_metric=missing_thin_pack_bytes".to_owned()));
    }

    /// F9: the unknown-tip count saturates instead of underflowing.
    #[test]
    fn unknown_tip_count_saturates() {
        let result = CarryEstimate {
            destination_tip_count: 1,
            destination_shallow_count: 0,
            haves_used: 2,
            source_shallow_count: 0,
            source_partial: false,
            stash_entries: 0,
            source: Tally::default(),
            missing: Tally::default(),
            thin_pack: ThinPack::default(),
        };
        assert!(result
            .lines()
            .contains(&"destination_tips_unknown_to_source=0".to_owned()));
    }

    /// Deterministic, poorly compressible text of `lines` lines.
    fn noise(seed: u64, lines: usize) -> String {
        use std::fmt::Write as _;
        let mut state = seed;
        let mut text = String::new();
        for _ in 0..lines {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            writeln!(text, "{state:016x}").unwrap();
        }
        text
    }

    /// A bare destination holding exactly `revision` of `source`, fetched.
    fn fetched_destination(fixture: &Fixture, source: &Path, revision: &str) -> PathBuf {
        let destination = fixture.root.join("destination.git");
        output(
            git(&fixture.root)
                .args(["init", "--quiet", "--bare", "--template="])
                .arg(&destination),
        )
        .unwrap();
        let url = format!("file://{}", source.display());
        output(git(&destination).args([
            "fetch",
            "--quiet",
            "--no-tags",
            url.as_str(),
            &format!("{revision}:refs/heads/main"),
        ]))
        .unwrap();
        destination
    }

    /// The pack a real `git fetch` receives (`GIT_TRACE_PACKFILE`): the
    /// transfer the W6 M1 gate measures. Returns its bytes and header count.
    fn fetched_pack(fixture: &Fixture, destination: &Path, source: &Path) -> ThinPack {
        let trace = fixture.root.join("received.pack");
        let url = format!("file://{}", source.display());
        output(git(destination).env("GIT_TRACE_PACKFILE", &trace).args([
            "fetch",
            "--quiet",
            "--no-tags",
            url.as_str(),
            "+refs/heads/*:refs/remotes/source/*",
        ]))
        .unwrap();
        count_pack(std::fs::File::open(&trace).unwrap()).unwrap()
    }

    fn assert_tight(estimated: ThinPack, sent: ThinPack) {
        assert_eq!(estimated.objects, sent.objects);
        // Same objects, same deltas: allow 1 % plus a few header bytes for
        // delta-search variation between our pack-objects and upload-pack's.
        assert!(
            estimated.bytes.abs_diff(sent.bytes) <= sent.bytes / 100 + 16,
            "estimated {} vs sent {}",
            estimated.bytes,
            sent.bytes
        );
    }

    /// R-N74 (F7): a blob stored whole in the source but sendable as a delta
    /// against a have. `missing_bytes_disk` is far above what is sent;
    /// `missing_thin_pack_bytes` matches a real fetch.
    #[test]
    fn thin_pack_bytes_match_a_real_fetch_for_a_deltified_blob() {
        let fixture = Fixture::new("thin-delta");
        let source = fixture.repo("source");
        let base = commit(&source, "big.txt", &noise(1, 8000));
        // Growing the file makes the new version the whole (larger) delta base
        // after a repack, so the source stores it undeltified.
        let mut grown = noise(1, 8000);
        grown.push_str(&noise(2, 40));
        commit(&source, "big.txt", &grown);
        output(git(&source).args(["repack", "--quiet", "-a", "-d", "-f"])).unwrap();
        let destination = fetched_destination(&fixture, &source, &base);
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        assert_eq!(result.haves_used, 1);
        assert_eq!(result.missing.objects(), 3);
        assert_eq!(result.thin_pack.objects, 3);
        assert!(
            result.thin_pack.bytes * 10 < result.missing.bytes_disk(),
            "thin {} vs disk {}",
            result.thin_pack.bytes,
            result.missing.bytes_disk()
        );
        assert_tight(
            result.thin_pack,
            fetched_pack(&fixture, &destination, &source),
        );
    }

    /// R-N74 (F7): loose missing objects, packed thin in flight.
    #[test]
    fn thin_pack_bytes_match_a_real_fetch_for_loose_objects() {
        let fixture = Fixture::new("thin-loose");
        let source = fixture.repo("source");
        let base = commit(&source, "big.txt", &noise(3, 4000));
        output(git(&source).args(["repack", "--quiet", "-a", "-d"])).unwrap();
        let mut edited = noise(3, 4000);
        edited.push_str("one more line\n");
        commit(&source, "big.txt", &edited);
        commit(&source, "small.txt", "small");
        let destination = fetched_destination(&fixture, &source, &base);
        let loose = text(git(&source).args(["count-objects"])).unwrap();
        assert!(!loose.starts_with("0 objects"), "{loose}");
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        // Two commits, two root trees, the edited blob and small.txt.
        assert_eq!(result.missing.objects(), 6);
        assert_eq!(result.thin_pack.objects, 6);
        assert_tight(
            result.thin_pack,
            fetched_pack(&fixture, &destination, &source),
        );
    }

    /// R-N74: the thin pack is exactly the walked set. A directory rename
    /// makes Git's default sparse edge marking (`pack.useSparse`) pack the
    /// moved blobs again although the have already holds them.
    #[test]
    fn thin_pack_objects_equal_the_walk_across_a_directory_rename() {
        let fixture = Fixture::new("thin-sparse");
        let source = fixture.repo("source");
        commit(&source, "a/z.txt", "other");
        let base = commit(&source, "a/x.txt", "shared");
        output(git(&source).args(["mv", "a", "b"])).unwrap();
        commit(&source, "b/y.txt", "new");
        let destination = fixture.repo("destination");
        mirror(&source, &destination, &[(&base, "refs/heads/main")]);
        let result = estimate(&source, &Destination::Local(destination)).unwrap();
        // One commit, the root tree, tree b and blob y.
        assert_eq!(result.missing.objects(), 4);
        assert_eq!(result.thin_pack.objects, 4);
    }

    /// F11: ssh gets a connect timeout, and a failed probe carries a bounded,
    /// credential-free excerpt of the child's stderr.
    #[test]
    fn remote_failures_carry_bounded_redacted_stderr() {
        let ssh = ssh_command("sting", "true");
        let args: Vec<_> = ssh.get_args().collect();
        assert_eq!(
            args,
            [
                "-T",
                "-oBatchMode=yes",
                "-oConnectTimeout=15",
                "-oServerAliveInterval=15",
                "-oServerAliveCountMax=4",
                "sting",
                "true"
            ]
        );
        let mut failing = Command::new("sh");
        failing.args([
            "-c",
            "cat >/dev/null; printf 'ssh: connect to host sting port 22: Connection timed out\\nfetch https://jess:hunter2@example.org/r ghp_abc token=xyz\\n' >&2; head -c 4000 /dev/zero | tr '\\0' x >&2; exit 255",
        ]);
        let refused = run_probe(&mut failing).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitUnavailable);
        let detail = refused.detail.unwrap();
        assert!(
            detail.starts_with("ssh: connect to host sting port 22: Connection timed out"),
            "{detail}"
        );
        assert!(
            detail.contains("https://[redacted]@example.org/r"),
            "{detail}"
        );
        for secret in ["hunter2", "ghp_abc", "xyz", "jess:"] {
            assert!(!detail.contains(secret), "{detail}");
        }
        assert!(detail.chars().count() <= DETAIL_LIMIT + 3);
        assert!(!detail.contains('\n'));
        let mut malformed = Command::new("sh");
        malformed.args(["-c", "cat >/dev/null; echo 'fatal: bad object' >&2; exit 1"]);
        let refused = run_probe(&mut malformed).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitInventoryMalformed);
        assert_eq!(refused.detail.as_deref(), Some("fatal: bad object"));
        assert_eq!(
            refused.to_string(),
            "GIT_INVENTORY_MALFORMED: fatal: bad object"
        );
    }

    /// F1: every Git call the probe makes carries the no-write, no-network
    /// hardening, locally and in the remote command.
    #[test]
    fn probe_and_source_git_calls_are_hardened() {
        let g = PROBE_SCRIPT
            .lines()
            .find(|line| line.starts_with("g() {"))
            .unwrap();
        for flag in [
            "--no-optional-locks",
            "-c maintenance.auto=false",
            "-c gc.auto=0",
            "-c core.hooksPath=/dev/null",
        ] {
            assert!(g.contains(flag), "{flag}");
        }
        assert!(PROBE_SCRIPT.contains("export GIT_NO_LAZY_FETCH=1"));
        // Every git call but `git version` goes through `g`.
        for line in PROBE_SCRIPT.lines().filter(|line| line.contains("git ")) {
            assert!(
                line.starts_with("g() {")
                    || line == "version=$(git version) || exit 5"
                    || line
                        == "case \"$version\" in 'git version '*) version=${version#git version } ;; *) exit 5 ;; esac",
                "{line}"
            );
        }
        assert!(remote_command("/srv/r").starts_with("env GIT_NO_LAZY_FETCH=1 "));
        let source = hardened(&Repository {
            git_dir: PathBuf::from("/probed/repo/.git"),
            ceiling: PathBuf::from("/probed"),
        });
        let args: Vec<_> = source.get_args().filter_map(|arg| arg.to_str()).collect();
        // N5: source commands name the probed git dir, never the given path.
        assert!(args.contains(&"--git-dir=/probed/repo/.git"), "{args:?}");
        assert!(source.get_envs().any(|(key, value)| {
            key == "GIT_CEILING_DIRECTORIES" && value == Some(OsStr::new("/probed"))
        }));
        for flag in [
            "--no-optional-locks",
            "maintenance.auto=false",
            "gc.auto=0",
            "core.hooksPath=/dev/null",
        ] {
            assert!(args.contains(&flag), "{flag}");
        }
        assert!(source
            .get_envs()
            .any(|(key, value)| key == "GIT_NO_LAZY_FETCH" && value == Some(OsStr::new("1"))));
    }

    // ---- PR #55 review r2 (R-N74, R-N75, R-N97) ---------------------------

    /// `commit` with a fixed, strictly increasing date, so the order in which a
    /// fetch client offers its haves (newest first) is deterministic.
    fn dated_commit(repo: &Path, path: &str, content: &str, second: u32) -> String {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
        output(git(repo).args(["add", "--all"])).unwrap();
        let date = format!("@{} +0000", 1_700_000_000 + u64::from(second));
        output(
            git(repo)
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .args(["commit", "--quiet", "-m", path]),
        )
        .unwrap();
        text(git(repo).args(["rev-parse", "HEAD"])).unwrap()
    }

    /// A real `git fetch` from `source` into a scratch copy of `destination`
    /// (all refs, as the verb offers them), through `upload_pack`. Returns the
    /// received pack's bytes and header count; `destination` is untouched.
    fn real_fetch(
        fixture: &Fixture,
        destination: &Path,
        source: &Path,
        upload_pack: &str,
    ) -> ThinPack {
        let copy = fixture.root.join(format!(
            "fetch-copy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        output(Command::new("cp").arg("-R").arg(destination).arg(&copy)).unwrap();
        let trace = copy.with_extension("pack");
        let url = format!("file://{}", source.display());
        output(
            git(&copy)
                .env("GIT_TRACE_PACKFILE", &trace)
                .args(["fetch", "--quiet", "--no-tags"])
                .arg(format!("--upload-pack={upload_pack}"))
                .args([url.as_str(), "+refs/*:refs/fetched/*"]),
        )
        .unwrap();
        let pack = std::fs::File::open(&trace)
            .map_or_else(|_| ThinPack::default(), |file| count_pack(file).unwrap());
        std::fs::remove_dir_all(&copy).unwrap();
        pack
    }

    /// The sender M1 must be: upload-pack with sparse edges and bitmaps off.
    const PINNED_UPLOAD_PACK: &str =
        "git -c pack.useSparse=false -c pack.useBitmaps=false upload-pack";

    /// N1: only Git versions that honour `GIT_NO_LAZY_FETCH` pass; anything
    /// unparseable fails closed.
    #[test]
    fn version_gate_accepts_only_versions_that_honour_no_lazy_fetch() {
        let fixture = Fixture::new("version-gate");
        let repo = fixture.repo("repo");
        commit(&repo, "a.txt", "a");
        let real = text(Command::new("sh").args(["-c", "command -v git"])).unwrap();
        let path = std::env::var("PATH").unwrap();
        for (index, (version, accepted)) in [
            ("2.44.0", false),
            ("2.44.1", true),
            ("2.45.0", true),
            ("2.52.0", true),
            ("3.0.0", true),
            ("2.43.3", false),
            ("2.43.4", true),
            ("2.42.1", false),
            ("2.42.2", true),
            ("2.41.0", false),
            ("2.41.1", true),
            ("2.40.1", false),
            ("2.40.2", true),
            ("2.39.3", false),
            ("2.39.4", true),
            ("2.39.5 (Apple Git-154)", true),
            ("2.38.9", false),
            ("2.45.0-rc0", false),
            ("2.45.0.rc1", false),
            ("2.52.0.123.gabcdef", false),
            ("2.52", false),
            ("garbage", false),
            ("", false),
        ]
        .into_iter()
        .enumerate()
        {
            let bin = fixture.root.join(format!("bin-{index}"));
            std::fs::create_dir(&bin).unwrap();
            let wrapper = bin.join("git");
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = version ]; then echo 'git version {version}'; exit 0; fi\nexec '{real}' \"$@\"\n"
                ),
            )
            .unwrap();
            output(Command::new("chmod").arg("+x").arg(&wrapper)).unwrap();
            let probe =
                run_probe(local_probe(&repo).env("PATH", format!("{}:{path}", bin.display())));
            if accepted {
                assert!(probe.is_ok(), "[{version}] refused: {probe:?}");
            } else {
                assert_eq!(
                    probe.unwrap_err().refusal,
                    BulkloadRefusal::GitUnavailable,
                    "[{version}] accepted"
                );
            }
        }
    }

    /// N2: a bitmapped source estimates (no false `CONTRACT_SELF_INCONSISTENT`)
    /// and matches a sender that pins `pack.useBitmaps=false` and
    /// `pack.useSparse=false`. A bitmap sender may send less (R-N97).
    #[test]
    fn bitmapped_source_is_estimated_like_a_pinned_sender() {
        let fixture = Fixture::new("bitmaps");
        let source = fixture.repo("source");
        dated_commit(&source, "big.txt", &noise(4, 2000), 1);
        dated_commit(&source, "other.txt", "o", 2);
        output(git(&source).args(["rm", "--quiet", "big.txt"])).unwrap();
        let removed = dated_commit(&source, "gone.txt", "g", 3);
        let destination = fetched_destination(&fixture, &source, &removed);
        dated_commit(&source, "big.txt", &noise(4, 2000), 4);
        output(git(&source).args(["repack", "--quiet", "-a", "-d", "-b"])).unwrap();
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        assert_eq!(result.thin_pack.objects, result.missing.objects());
        let pinned = real_fetch(&fixture, &destination, &source, PINNED_UPLOAD_PACK);
        assert_eq!(result.thin_pack, pinned);
        let bitmapped = real_fetch(&fixture, &destination, &source, "git upload-pack");
        assert!(bitmapped.objects <= result.thin_pack.objects);
        assert!(bitmapped.bytes <= result.thin_pack.bytes);
    }

    /// N3: for a shallow destination the estimate models upload-pack
    /// (`--shallow`, edge-aggressive over the haves it keeps) and equals a
    /// real fetch into the shallow clone, byte for byte.
    #[test]
    fn shallow_destination_matches_a_real_fetch_into_a_shallow_clone() {
        let fixture = Fixture::new("shallow-fetch");
        let origin = fixture.repo("origin");
        dated_commit(&origin, "keep.txt", &noise(5, 300), 1);
        dated_commit(&origin, "y.txt", "two", 2);
        output(git(&origin).args(["checkout", "--quiet", "-b", "side"])).unwrap();
        dated_commit(&origin, "b.txt", &noise(6, 200), 3);
        output(git(&origin).args(["checkout", "--quiet", "main"])).unwrap();
        let source = fixture.shallow_clone(&origin, "source", 1);
        let destination = fixture.shallow_clone(&origin, "destination", 1);
        // The source re-adds, on main, a blob only the destination's other
        // shallow tip holds, plus a new blob.
        output(git(&source).args(["checkout", "--quiet", "main"])).unwrap();
        dated_commit(&source, "b.txt", &noise(6, 200), 4);
        dated_commit(&source, "z.txt", "three", 5);
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        assert_eq!(result.destination_shallow_count, 2);
        let sent = real_fetch(&fixture, &destination, &source, PINNED_UPLOAD_PACK);
        assert_eq!(result.thin_pack, sent);
        assert_eq!(result.missing.objects(), sent.objects);
        // Two commits, one new root tree and z.txt: b.txt and the tree that
        // re-adds it are both already in the destination's other shallow tip.
        assert_eq!(sent.objects, 4);
    }

    /// N3 / R-N116: a shallow destination holding a commit and its parent.
    /// M1 offers them ancestors first, so upload-pack keeps both and the
    /// estimate equals it; `git fetch` offers newest first, upload-pack drops
    /// the parent, and the fetch sends more.
    #[test]
    fn shallow_destination_with_a_parent_have_matches_a_real_fetch() {
        let fixture = Fixture::new("shallow-parent-have");
        let origin = fixture.repo("origin");
        dated_commit(&origin, "keep.txt", &noise(7, 300), 1);
        dated_commit(&origin, "x.txt", "one", 2);
        dated_commit(&origin, "y.txt", "two", 3);
        let source = fixture.shallow_clone(&origin, "source", 1);
        let destination = fixture.shallow_clone(&origin, "destination", 1);
        output(git(&source).args(["rm", "--quiet", "x.txt"])).unwrap();
        let removed = dated_commit(&source, "w.txt", "w", 4);
        let url = format!("file://{}", source.display());
        output(git(&destination).args([
            "fetch",
            "--quiet",
            url.as_str(),
            &format!("{removed}:refs/heads/tmp"),
        ]))
        .unwrap();
        dated_commit(&source, "x.txt", "one", 5);
        dated_commit(&source, "z.txt", "three", 6);
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        let (wants, haves) = r_n113_request(&source, &destination);
        let oracle = upload_pack_oracle(&source, &wants, &haves, &shallow_of(&destination));
        assert_eq!(result.thin_pack, oracle);
        assert_eq!(result.missing.objects(), oracle.objects);
        let sent = real_fetch(&fixture, &destination, &source, PINNED_UPLOAD_PACK);
        assert!(sent.objects > oracle.objects, "{sent:?} vs {oracle:?}");
    }

    fn partial_destination_fixture(fixture: &Fixture) -> (PathBuf, PathBuf, PathBuf) {
        let origin = fixture.repo("origin");
        allow_filter(&origin);
        for version in 1..=4 {
            commit(&origin, "a.txt", &format!("version {version}\n"));
        }
        let destination = fixture.partial_clone(&origin, "destination", true);
        let linked = fixture.root.join("destination-linked");
        output(
            git(&destination)
                .args(["worktree", "add", "--quiet", "--detach"])
                .arg(&linked),
        )
        .unwrap();
        output(git(&destination).args(["config", "extensions.worktreeConfig", "true"])).unwrap();
        output(git(&linked).args(["config", "--worktree", "remote.origin.promisor", "true"]))
            .unwrap();
        output(git(&destination).args(["config", "--unset", "remote.origin.promisor"])).unwrap();
        let _ = output(git(&destination).args(["config", "--unset", "extensions.partialClone"]));
        let source = fixture.plain_clone(&origin, "source");
        commit(&source, "b.txt", "new\n");
        (source, destination, linked)
    }

    /// N4 (R-N75): a partial destination is refused whichever worktree names
    /// it, and even with its promisor configuration scrubbed.
    #[test]
    fn partial_destination_is_refused_whichever_worktree_names_it() {
        let fixture = Fixture::new("partial-worktree");
        let (source, destination, linked) = partial_destination_fixture(&fixture);
        for path in [&destination, &linked] {
            let refused = estimate(&source, &Destination::Local(path.clone())).unwrap_err();
            assert_eq!(
                refused.refusal,
                BulkloadRefusal::GitHavesUnprovable,
                "{}",
                path.display()
            );
        }
        output(git(&destination).args(["config", "--unset", "extensions.worktreeConfig"])).unwrap();
        for entry in std::fs::read_dir(destination.join(".git/worktrees")).unwrap() {
            let _ = std::fs::remove_file(entry.unwrap().path().join("config.worktree"));
        }
        // Only the `.promisor` packs remain.
        let refused = estimate(&source, &Destination::Local(destination)).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
    }

    /// N4 (R-N75): a destination borrowing objects from a partial clone
    /// through alternates is refused.
    #[test]
    fn destination_with_a_partial_alternate_is_refused() {
        let fixture = Fixture::new("partial-alternate");
        let origin = fixture.repo("origin");
        allow_filter(&origin);
        commit(&origin, "a.txt", "1");
        let tip = commit(&origin, "a.txt", "2");
        let partial = fixture.partial_clone(&origin, "partial", false);
        let destination = fixture.repo("destination");
        std::fs::write(
            destination.join(".git/objects/info/alternates"),
            format!("{}\n", partial.join(".git/objects").display()),
        )
        .unwrap();
        output(git(&destination).args(["update-ref", "refs/heads/main", &tip])).unwrap();
        let source = fixture.plain_clone(&origin, "source");
        let refused = estimate(&source, &Destination::Local(destination)).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
    }

    /// N5: a path with a control character (here a trailing newline, which
    /// `$(...)` would strip) is refused before any Git runs.
    #[test]
    fn paths_with_control_characters_are_refused() {
        let fixture = Fixture::new("control-path");
        let outer = fixture.repo("outer");
        commit(&outer, "e.txt", "enclosing");
        let inner = outer.join("r");
        std::fs::create_dir(&inner).unwrap();
        output(git(&inner).args(["init", "--quiet", "--template=", "-b", "main"])).unwrap();
        configure(&inner);
        commit(&inner, "only.txt", "only");
        let trailing = outer.join("r\n");
        std::fs::create_dir(&trailing).unwrap();
        let destination = fixture.repo("destination");
        commit(&destination, "d.txt", "d");
        let refused = estimate(&trailing, &Destination::Local(destination.clone())).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::PathNotPortable);
        let refused = estimate(&destination, &Destination::Local(trailing)).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::PathNotPortable);
    }

    /// N5: a source that is not a partial clone but cannot reach an object its
    /// refs name is refused, not reported with an `unavailable` count.
    #[test]
    fn a_non_partial_source_missing_an_object_is_refused() {
        let fixture = Fixture::new("corrupt-source");
        let source = fixture.repo("source");
        commit(&source, "a.txt", "a");
        let blob = text(git(&source).args(["rev-parse", "HEAD:a.txt"])).unwrap();
        let loose = source
            .join(".git/objects")
            .join(&blob[..2])
            .join(&blob[2..]);
        std::fs::remove_file(loose).unwrap();
        let destination = fixture.repo("destination");
        let refused = estimate(&source, &Destination::Local(destination)).unwrap_err();
        assert_eq!(refused.refusal, BulkloadRefusal::GitInventoryMalformed);
    }

    /// N6: control characters are dropped before redaction, the value after a
    /// spaced key is redacted, and schemeless `user:pw@host` is redacted.
    #[test]
    fn redaction_covers_spaced_keys_schemeless_userinfo_and_split_tokens() {
        let spaced = detail(
            b"fatal: auth failed; password: hunter2 Authorization: Bearer eyJhbGciOi.secretpart token = s3cr3t",
        )
        .unwrap();
        for secret in ["hunter2", "eyJhbGciOi.secretpart", "s3cr3t"] {
            assert!(!spaced.contains(secret), "{spaced}");
        }
        assert!(spaced.starts_with("fatal: auth failed;"), "{spaced}");
        let schemeless = detail(b"could not read from jess:hunter2@example.org:repo.git").unwrap();
        assert!(!schemeless.contains("hunter2"), "{schemeless}");
        assert!(!schemeless.contains("jess:"), "{schemeless}");
        assert!(schemeless.contains("@example.org:repo.git"), "{schemeless}");
        let kept = detail(b"ssh: git@github.com:org/repo.git denied").unwrap();
        assert!(kept.contains("git@github.com:org/repo.git"), "{kept}");
        let split = detail(b"x gh\x1bp_abcdef0123456789 y").unwrap();
        assert!(!split.contains("abcdef0123456789"), "{split}");
    }

    // ---- PR #55 review r3 (R-N113) -----------------------------------------

    fn pkt_line(out: &mut Vec<u8>, line: &str) {
        out.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
        out.extend_from_slice(line.as_bytes());
    }

    /// R-N113's exactness oracle, the reviewer's `oracle_upload_pack.py` in
    /// Rust: `upload-pack` itself (protocol v2, stateless), given exactly
    /// `haves` in this order, with the verb's pack pins. Returns the pack it
    /// streams on sideband 1.
    fn upload_pack_oracle(
        source: &Path,
        wants: &BTreeSet<String>,
        haves: &[String],
        shallow: &BTreeSet<String>,
    ) -> ThinPack {
        let mut request = Vec::new();
        pkt_line(&mut request, "command=fetch\n");
        request.extend_from_slice(b"0001");
        for line in ["thin-pack\n", "ofs-delta\n", "no-progress\n"] {
            pkt_line(&mut request, line);
        }
        for value in shallow {
            pkt_line(&mut request, &format!("shallow {value}\n"));
        }
        for value in wants {
            pkt_line(&mut request, &format!("want {value}\n"));
        }
        for value in haves {
            pkt_line(&mut request, &format!("have {value}\n"));
        }
        pkt_line(&mut request, "done\n");
        request.extend_from_slice(b"0000");
        let mut child = git(source)
            .args([
                "-c",
                "pack.useSparse=false",
                "-c",
                "pack.useBitmaps=false",
                "upload-pack",
                "--stateless-rpc",
                ".",
            ])
            .env("GIT_PROTOCOL", "version=2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&request).unwrap();
        let answer = child.wait_with_output().unwrap();
        assert!(answer.status.success());
        let mut rest = &answer.stdout[..];
        let mut in_pack = false;
        let mut pack = Vec::new();
        while rest.len() >= 4 {
            let length =
                usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
            if length < 4 {
                rest = &rest[4..];
                if length == 0 && in_pack {
                    break;
                }
                continue;
            }
            let payload = &rest[4..length];
            rest = &rest[length..];
            if in_pack {
                assert_ne!(payload[0], 3, "upload-pack error");
                if payload[0] == 1 {
                    pack.extend_from_slice(&payload[1..]);
                }
            } else if payload == b"packfile\n" {
                in_pack = true;
            }
        }
        if pack.is_empty() {
            ThinPack::default()
        } else {
            count_pack(&pack[..]).unwrap()
        }
    }

    fn tips_of(repo: &Path) -> BTreeSet<String> {
        let mut tips: BTreeSet<String> =
            text(git(repo).args(["for-each-ref", "--format=%(objectname)"]))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect();
        if let Ok(head) = text(git(repo).args(["rev-parse", "--verify", "-q", "HEAD"])) {
            tips.insert(head);
        }
        tips
    }

    /// R-N113/R-N116's request for `destination` against `source`: the wants
    /// (source tips not held) and, as haves, exactly every destination tip the
    /// source holds, ancestors first (topological order over their commits).
    fn r_n113_request(source: &Path, destination: &Path) -> (BTreeSet<String>, Vec<String>) {
        let held: BTreeSet<String> = tips_of(destination)
            .into_iter()
            .filter(|tip| output(git(source).args(["cat-file", "-e", tip])).is_ok())
            .collect();
        let wants = tips_of(source).difference(&held).cloned().collect();
        (wants, ancestors_first(source, &held))
    }

    /// `held` ordered so every commit comes after its held ancestors: the
    /// order in which upload-pack keeps every have (R-N116).
    fn ancestors_first(source: &Path, held: &BTreeSet<String>) -> Vec<String> {
        let mut peeled: Vec<(String, Option<String>)> = held
            .iter()
            .map(|tip| {
                let commit = text(git(source).args([
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("{tip}^{{commit}}"),
                ]))
                .ok();
                (tip.clone(), commit)
            })
            .collect();
        let commits: Vec<&String> = peeled
            .iter()
            .filter_map(|(_, commit)| commit.as_ref())
            .collect();
        let order: Vec<String> = if commits.is_empty() {
            Vec::new()
        } else {
            text(
                git(source)
                    .args(["rev-list", "--topo-order", "--reverse"])
                    .args(&commits),
            )
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
        };
        peeled.sort_by_key(|(_, commit)| {
            commit
                .as_ref()
                .and_then(|commit| order.iter().position(|value| value == commit))
                .map_or(0, |at| at + 1)
        });
        peeled.into_iter().map(|(tip, _)| tip).collect()
    }

    fn shallow_of(repo: &Path) -> BTreeSet<String> {
        std::fs::read_to_string(repo.join(".git/shallow"))
            .or_else(|_| std::fs::read_to_string(repo.join("shallow")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// R3-1 (R-N113), reviewer fixture B: non-shallow destination whose old
    /// held tip `git fetch` never offers (the server is ready first). The
    /// estimate equals upload-pack given every held tip; a real fetch sends
    /// more, which R-N113 accepts because M1 offers every held tip.
    #[test]
    fn every_held_tip_is_a_have_even_when_fetch_would_not_offer_it() {
        let fixture = Fixture::new("r3-fixture-b");
        let origin = fixture.repo("origin");
        dated_commit(&origin, "base.txt", "base", 1);
        output(git(&origin).args(["checkout", "--quiet", "-b", "feature"])).unwrap();
        dated_commit(&origin, "f.txt", &noise(31, 2000), 2);
        output(git(&origin).args(["checkout", "--quiet", "main"])).unwrap();
        for index in 1..=5 {
            dated_commit(
                &origin,
                &format!("m{index}.txt"),
                &format!("m{index}"),
                100 + index,
            );
        }
        let url = format!("file://{}", origin.display());
        let source = fixture.root.join("source.git");
        output(
            git(&fixture.root)
                .args(["clone", "--quiet", "--bare", "--no-local", url.as_str()])
                .arg(&source),
        )
        .unwrap();
        let destination = fixture.plain_clone(&origin, "destination");
        let root = text(git(&destination).args(["rev-list", "--max-parents=0", "HEAD"])).unwrap();
        output(git(&destination).args(["checkout", "--quiet", "-b", "local", &root])).unwrap();
        for index in 1..=20 {
            dated_commit(
                &destination,
                &format!("l{index}.txt"),
                &format!("l{index}"),
                10 + index,
            );
        }
        output(git(&destination).args(["checkout", "--quiet", "main"])).unwrap();
        let work = fixture.plain_clone(&origin, "work");
        output(git(&work).args(["merge", "--quiet", "--no-edit", "origin/feature"])).unwrap();
        let merged = text(git(&work).args(["rev-parse", "HEAD"])).unwrap();
        output(
            git(&source)
                .args(["fetch", "--quiet"])
                .arg(&work)
                .arg(format!("{merged}:refs/heads/merged")),
        )
        .unwrap();
        output(git(&source).args(["update-ref", "-d", "refs/heads/feature"])).unwrap();
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        let (wants, haves) = r_n113_request(&source, &destination);
        let oracle = upload_pack_oracle(&source, &wants, &haves, &BTreeSet::new());
        assert_eq!(result.thin_pack, oracle);
        assert_eq!(result.missing.objects(), oracle.objects);
        // git fetch's negotiation stops before offering origin/feature, so it
        // sends f.txt's history too: more than the R-N113 first round.
        let fetched = real_fetch(&fixture, &destination, &source, PINNED_UPLOAD_PACK);
        assert!(
            fetched.objects > oracle.objects,
            "{fetched:?} vs {oracle:?}"
        );
    }

    /// R-N113, reviewer fixture A3: shallow destination whose held tips are a
    /// commit and its grandparent, neither advertised by the source. The
    /// estimate equals upload-pack given every held tip, in either order. A
    /// real `git fetch` sends more: it also offers the intermediate parent,
    /// upload-pack then drops the grandparent as implied, and the shallow walk
    /// no longer marks g.txt's tree. An extra have can enlarge a shallow pack.
    #[test]
    fn shallow_destination_with_a_grandparent_have_equals_upload_pack() {
        let fixture = Fixture::new("r3-fixture-a3");
        let origin = fixture.repo("origin");
        dated_commit(&origin, "base.txt", "base", 1);
        let boundary = dated_commit(&origin, "keep.txt", &noise(71, 50), 2);
        let grandparent = dated_commit(&origin, "g.txt", &noise(72, 400), 3);
        output(git(&origin).args(["branch", "old", &grandparent])).unwrap();
        output(git(&origin).args(["rm", "--quiet", "g.txt"])).unwrap();
        dated_commit(&origin, "rm.txt", "rm", 4);
        dated_commit(&origin, "y.txt", "y", 5);
        let destination = fixture.plain_clone(&origin, "destination");
        std::fs::write(destination.join(".git/shallow"), format!("{boundary}\n")).unwrap();
        output(git(&destination).args(["branch", "--quiet", "old", "origin/old"])).unwrap();
        let source = fixture.root.join("source.git");
        output(
            git(&fixture.root)
                .args(["init", "--quiet", "--bare", "--template=", "-b", "main"])
                .arg(&source),
        )
        .unwrap();
        let work = fixture.plain_clone(&origin, "work");
        let readded = dated_commit(&work, "g.txt", &noise(72, 400), 6);
        output(
            git(&source)
                .args(["fetch", "--quiet"])
                .arg(&work)
                .arg(format!("{readded}:refs/heads/main")),
        )
        .unwrap();
        std::fs::write(source.join("shallow"), format!("{boundary}\n")).unwrap();
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        let (wants, haves) = r_n113_request(&source, &destination);
        assert_eq!(haves.len(), 2);
        let frontier = shallow_of(&destination);
        let oracle = upload_pack_oracle(&source, &wants, &haves, &frontier);
        let reversed: Vec<String> = haves.iter().rev().cloned().collect();
        assert_eq!(
            upload_pack_oracle(&source, &wants, &reversed, &frontier),
            oracle
        );
        assert_eq!(result.thin_pack, oracle);
        assert_eq!(result.missing.objects(), oracle.objects);
        // The re-added commit and its tree: g.txt is in the grandparent's tree.
        assert_eq!(oracle.objects, 2);
        let fetched = real_fetch(&fixture, &destination, &source, PINNED_UPLOAD_PACK);
        assert!(fetched.bytes > oracle.bytes, "{fetched:?} vs {oracle:?}");
    }

    /// R3-2: redaction probes from the r3 review (bearer/basic/digest inline
    /// values, `pwd`/`pass`/`cookie`/`sig`/`x-amz-signature`, Unicode format
    /// and invisible separators, `/` in a URL password).
    #[test]
    #[allow(clippy::too_many_lines)]
    fn redaction_r3_probes_leak_nothing() {
        let cases: &[(&[u8], &str)] = &[
            (b"Authorization: Basic dXNlcjpodW50ZXIy", "dXNlcjpodW50ZXIy"),
            (
                b"authorization:Bearer eyJhbGciOiJIUzI1NiJ9.payload",
                "eyJhbGciOiJIUzI1NiJ9.payload",
            ),
            (b"Authorization=Basic dXNlcjpodW50ZXIy", "dXNlcjpodW50ZXIy"),
            (
                b"token:Bearer eyJhbGciOiJIUzI1NiJ9.payload",
                "eyJhbGciOiJIUzI1NiJ9.payload",
            ),
            (b"authorization=digest s3cr3tvalue", "s3cr3tvalue"),
            (br#"{"token":"s3cr3tvalue"}"#, "s3cr3tvalue"),
            (br#"{"token": "s3cr3tvalue"}"#, "s3cr3tvalue"),
            (br#"{"access_token" : "s3cr3tvalue"}"#, "s3cr3tvalue"),
            (
                b"GET https://h/x?access_token=s3cr3tvalue&x=1",
                "s3cr3tvalue",
            ),
            (b"GET https://h/x?sig=s3cr3tvalue&se=1", "s3cr3tvalue"),
            (
                b"GET https://h/x?X-Amz-Signature=deadbeefcafe",
                "deadbeefcafe",
            ),
            (b"pwd=hunter2", "hunter2"),
            (b"pass: hunter2", "hunter2"),
            (b"Cookie: session=hunter2", "hunter2"),
            (b"sig=hunter2", "hunter2"),
            (b"x-amz-signature: deadbeefcafe", "deadbeefcafe"),
            (b"PRIVATE-TOKEN: glsecretvalue", "glsecretvalue"),
            (b"password:\nhunter2", "hunter2"),
            (b"line one\r\ntoken =\n  s3cr3tvalue", "s3cr3tvalue"),
            (b"https://jess:hun/ter2@example.org/r", "hun/ter2"),
            (b"https://jess:hunter2@example.org/r", "hunter2"),
            (
                "x gh\u{2028}p_abcdef0123456789 y".as_bytes(),
                "abcdef0123456789",
            ),
            (
                "x gh\u{200b}p_abcdef0123456789 y".as_bytes(),
                "abcdef0123456789",
            ),
            (
                "x gh\u{202e}p_abcdef0123456789 y".as_bytes(),
                "abcdef0123456789",
            ),
            (
                "x ghp\u{0085}_abcdef0123456789 y".as_bytes(),
                "abcdef0123456789",
            ),
            ("pass\u{feff}word: hunter2".as_bytes(), "hunter2"),
            (b"secret\xff=hunter2", "hunter2"),
            // R4-3: whitespace and controls separate key from value.
            ("fatal: password\u{a0}hunter2".as_bytes(), "hunter2"),
            ("token\u{2003}hunter2".as_bytes(), "hunter2"),
            ("password\u{2028}hunter2".as_bytes(), "hunter2"),
            (b"passwd\0hunter2", "hunter2"),
            (b"password\x08hunter2", "hunter2"),
            (b"password\x7fhunter2", "hunter2"),
            (
                b"Authorization: Bearer: eyJhbGciOiJIUzI1NiJ9sekrit",
                "eyJhbGciOiJIUzI1NiJ9sekrit",
            ),
            // R4-4: query keys, split and spaced PEM headers.
            (b"https://h/x?auth=s3cr3tvalue", "s3cr3tvalue"),
            (b"https://h/x?key=s3cr3tvalue", "s3cr3tvalue"),
            (b"https://h/x?code=s3cr3tvalue", "s3cr3tvalue"),
            (b"session=s3cr3tvalue", "s3cr3tvalue"),
            // R5-A: a whole token prefix, then a separator, then the body.
            ("ghp_\u{a0}abcdef0123456789".as_bytes(), "abcdef0123456789"),
            (
                "ghp_\u{2028}abcdef0123456789".as_bytes(),
                "abcdef0123456789",
            ),
            ("ghp_\u{85}abcdef0123456789".as_bytes(), "abcdef0123456789"),
            (b"ghp_\x00abcdef0123456789", "abcdef0123456789"),
            (b"ghp_\x1babcdef0123456789", "abcdef0123456789"),
            (b"remote: ghp_ abcdef0123456789", "abcdef0123456789"),
            (b"ghp_\nabcdef0123456789", "abcdef0123456789"),
            ("AKIA\u{a0}IOSFODNN7EXAMPLE".as_bytes(), "IOSFODNN7EXAMPLE"),
            (
                "github_pat_\u{2003}11ABCDEFG0123456789".as_bytes(),
                "11ABCDEFG0123456789",
            ),
            ("glpat-\u{85}s3cr3tvalue0123".as_bytes(), "s3cr3tvalue0123"),
            (
                "xoxb-\u{a0}123456789012-s3cr3tv".as_bytes(),
                "123456789012-s3cr3tv",
            ),
            // R5-B: a short key whose separator is its own word.
            (b"pass : hunter2", "hunter2"),
            (b"pwd = hunter2", "hunter2"),
            (b"sig = s3cr3tvalue", "s3cr3tvalue"),
            (b"signature : s3cr3tvalue", "s3cr3tvalue"),
            (b"Cookie : abc123s3cr3t", "abc123s3cr3t"),
            (b"{\"pwd\" : \"hunter2\"}", "hunter2"),
            (b"{\"pass\": \"hunter2\"}", "hunter2"),
            ("pass\u{a0}:\u{a0}hunter2".as_bytes(), "hunter2"),
            (b"pass\n:\nhunter2", "hunter2"),
            (b"session = s3cr3tvalue", "s3cr3tvalue"),
            (b"auth: s3cr3tvalue", "s3cr3tvalue"),
            (b"API key: s3cr3tvalue", "s3cr3tvalue"),
        ];
        for (input, secret) in cases {
            let out = detail(input).unwrap_or_default();
            assert!(
                !out.contains(secret),
                "leak: {:?} -> {out:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    /// R4-4: a PEM block redacts from its header on, even with the header
    /// split across lines, written with spaces, or ending in CRLF.
    #[test]
    fn redaction_covers_pem_blocks() {
        // PEM headers are assembled at run time so no key-shaped literal sits
        // in the source for secret scanners to flag.
        let dashes = "-".repeat(5);
        for (input, secret) in [
            (
                format!(
                    "{dashes}BEGIN OPENSSH {} KEY{dashes} b3BlbnNzaC1rZXktdjEAAAAA",
                    "PRIVATE"
                ),
                "b3BlbnNzaC1rZXktdjEAAAAA",
            ),
            (
                format!(
                    "{dashes}BEG\nIN RSA {} KEY{dashes}\nMIIEowIBAAKCAQEAxq9Zbody",
                    "PRIVATE"
                ),
                "MIIEowIBAAKCAQEAxq9Zbody",
            ),
            (
                format!(
                    "{dashes} BEGIN {} KEY {dashes}\nMIIEowIBAAKCAQEAxq9Zbody",
                    "PRIVATE"
                ),
                "MIIEowIBAAKCAQEAxq9Zbody",
            ),
            (
                format!(
                    "{dashes}BEGIN OPENSSH {} KEY{dashes}\r\nb3BlbnNzaC1rZXktdjEAAAAA\r\n",
                    "PRIVATE"
                ),
                "b3BlbnNzaC1rZXktdjEAAAAA",
            ),
        ] {
            let out = detail(input.as_bytes()).unwrap_or_default();
            assert!(!out.contains(secret), "leak: {input:?} -> {out:?}");
        }
    }

    /// R4-4: ordinary diagnostics survive redaction unchanged.
    #[test]
    fn redaction_keeps_ordinary_diagnostics() {
        let kept = detail(b"ssh: connect to host sting port 22: Connection timed out").unwrap();
        assert_eq!(
            kept,
            "ssh: connect to host sting port 22: Connection timed out"
        );
        let kept = detail(b"fatal: bypass passed; design signed https://h/@scope/pkg").unwrap();
        assert_eq!(
            kept,
            "fatal: bypass passed; design signed https://h/@scope/pkg"
        );
        // R4-4: short keys need `=` or `:`; a port is not a password.
        for keep in [
            "hint: pass --force to override",
            "error: no signature found",
            "gpg: Signature made Tue 23 Sep",
            "fetch https://host:443/@scope/pkg",
            "exit status 128",
        ] {
            assert_eq!(detail(keep.as_bytes()).unwrap(), keep);
        }
    }

    // ---- PR #55 review r4 (R-N116) -----------------------------------------

    /// R-N116, reviewer fixture P1: shallow destination holding a parent P
    /// and its child C. Whatever order the tips list in (four seeds give both
    /// hex orders), ancestors-first upload-pack keeps both haves and equals
    /// the estimate; child-first it drops P and sends more.
    #[test]
    fn parent_and_child_held_tips_equal_upload_pack_ancestors_first() {
        for seed in 1..=4_u64 {
            let fixture = Fixture::new(&format!("r4-p1-{seed}"));
            let origin = fixture.repo("origin");
            dated_commit(&origin, "base.txt", &format!("base{seed}"), 1);
            let boundary = dated_commit(&origin, "keep.txt", &noise(80 + seed, 50), 2);
            let parent = dated_commit(&origin, "g.txt", &noise(82, 400), 3);
            output(git(&origin).args(["branch", "old", &parent])).unwrap();
            output(git(&origin).args(["rm", "--quiet", "g.txt"])).unwrap();
            let child = dated_commit(&origin, "rm.txt", "rm", 4);
            let source = fixture.plain_clone(&origin, "source");
            let destination = fixture.plain_clone(&origin, "destination");
            for repo in [&source, &destination] {
                std::fs::write(repo.join(".git/shallow"), format!("{boundary}\n")).unwrap();
            }
            output(git(&destination).args(["branch", "--quiet", "old", "origin/old"])).unwrap();
            dated_commit(&source, "g.txt", &noise(82, 400), 6);
            let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
            let (wants, haves) = r_n113_request(&source, &destination);
            assert_eq!(haves, vec![parent.clone(), child.clone()], "seed {seed}");
            let frontier = shallow_of(&destination);
            let oracle = upload_pack_oracle(&source, &wants, &haves, &frontier);
            assert_eq!(result.thin_pack, oracle, "seed {seed}");
            let child_first = upload_pack_oracle(&source, &wants, &[child, parent], &frontier);
            assert!(child_first.objects > oracle.objects, "seed {seed}");
        }
    }

    /// R-N116, reviewer fixture M: merge, annotated tags and three branches,
    /// non-shallow and shallow (one frontier). The estimate equals
    /// upload-pack over every held tip, ancestors first.
    #[test]
    fn merges_tags_and_branches_equal_upload_pack_ancestors_first() {
        let fixture = Fixture::new("r4-m");
        let origin = fixture.repo("origin");
        let boundary = dated_commit(&origin, "keep.txt", &noise(111, 50), 1);
        output(git(&origin).args(["checkout", "--quiet", "-b", "side"])).unwrap();
        let side = dated_commit(&origin, "s.txt", &noise(112, 300), 2);
        output(git(&origin).args(["tag", "-a", "-m", "t", "vs", &side])).unwrap();
        output(git(&origin).args(["rm", "--quiet", "s.txt"])).unwrap();
        dated_commit(&origin, "rm.txt", "rm", 3);
        output(git(&origin).args(["checkout", "--quiet", "main"])).unwrap();
        dated_commit(&origin, "m.txt", &noise(113, 100), 4);
        output(git(&origin).args(["merge", "--quiet", "--no-edit", "side"])).unwrap();
        output(git(&origin).args(["checkout", "--quiet", "-b", "other"])).unwrap();
        dated_commit(&origin, "t.txt", &noise(114, 200), 6);
        output(git(&origin).args(["checkout", "--quiet", "main"])).unwrap();
        let destination = fixture.plain_clone(&origin, "destination");
        let source = fixture.plain_clone(&origin, "source");
        for repo in [&destination, &source] {
            output(git(repo).args(["fetch", "--quiet", "--tags", "origin"])).unwrap();
            output(git(repo).args(["branch", "--quiet", "side", "origin/side"])).unwrap();
        }
        output(git(&destination).args(["branch", "--quiet", "other", "origin/other"])).unwrap();
        let tip = dated_commit(&source, "s.txt", &noise(112, 300), 7);
        output(git(&source).args(["tag", "-a", "-m", "t2", "v2", &tip])).unwrap();
        output(git(&source).args(["checkout", "--quiet", "-b", "feat", "origin/other"])).unwrap();
        dated_commit(&source, "t2.txt", &format!("{}x", noise(114, 200)), 9);
        output(git(&source).args(["checkout", "--quiet", "main"])).unwrap();
        for shallow in [false, true] {
            if shallow {
                for repo in [&source, &destination] {
                    std::fs::write(repo.join(".git/shallow"), format!("{boundary}\n")).unwrap();
                }
            }
            let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
            let (wants, haves) = r_n113_request(&source, &destination);
            let oracle = upload_pack_oracle(&source, &wants, &haves, &shallow_of(&destination));
            assert_eq!(result.thin_pack, oracle, "shallow={shallow}");
            assert_eq!(
                result.missing.objects(),
                oracle.objects,
                "shallow={shallow}"
            );
        }
    }

    /// R-N116, reviewer fixture F: shallow at two frontiers (two roots), the
    /// held grandparent on the second root.
    #[test]
    fn two_frontier_shallow_destination_equals_upload_pack_ancestors_first() {
        let fixture = Fixture::new("r4-f");
        let origin = fixture.repo("origin");
        dated_commit(&origin, "a0.txt", "a0", 1);
        let first = dated_commit(&origin, "a1.txt", &noise(121, 50), 2);
        output(git(&origin).args(["checkout", "--quiet", "--orphan", "r2"])).unwrap();
        output(git(&origin).args(["rm", "-r", "-f", "--quiet", "."])).unwrap();
        dated_commit(&origin, "b0.txt", "b0", 3);
        let second = dated_commit(&origin, "b1.txt", &noise(122, 50), 4);
        let held = dated_commit(&origin, "h.txt", &noise(123, 300), 5);
        output(git(&origin).args(["branch", "hold", &held])).unwrap();
        output(git(&origin).args(["rm", "--quiet", "h.txt"])).unwrap();
        dated_commit(&origin, "rm.txt", "rm", 6);
        dated_commit(&origin, "z.txt", "z", 7);
        output(git(&origin).args(["checkout", "--quiet", "main"])).unwrap();
        dated_commit(&origin, "a2.txt", "a2", 8);
        let source = fixture.plain_clone(&origin, "source");
        let destination = fixture.plain_clone(&origin, "destination");
        for repo in [&source, &destination] {
            std::fs::write(repo.join(".git/shallow"), format!("{first}\n{second}\n")).unwrap();
            for branch in ["r2", "hold"] {
                output(git(repo).args(["branch", "--quiet", branch, &format!("origin/{branch}")]))
                    .unwrap();
            }
        }
        output(git(&source).args(["checkout", "--quiet", "r2"])).unwrap();
        dated_commit(&source, "h.txt", &noise(123, 300), 9);
        output(git(&source).args(["checkout", "--quiet", "main"])).unwrap();
        let result = estimate(&source, &Destination::Local(destination.clone())).unwrap();
        let (wants, haves) = r_n113_request(&source, &destination);
        let oracle = upload_pack_oracle(&source, &wants, &haves, &shallow_of(&destination));
        assert_eq!(result.thin_pack, oracle);
        assert_eq!(result.destination_shallow_count, 2);
    }
}
