//! Read-only measurement of what git carry v2 would move (R-N60 baseline;
//! R-N74 gate metric; R-N75 refusals).
//!
//! One probe script, [`PROBE_SCRIPT`], reads a repository's offer: every ref
//! tip, every worktree's `HEAD` and per-worktree refs (`refs/worktree/`,
//! `refs/bisect/`, `refs/rewritten/`, found through each administrative
//! directory under `worktrees/`, so a pruned-away checkout still counts), its
//! shallow frontier and whether it is a partial clone. The probe first proves
//! the path is the repository's root: a work tree's top level or a bare
//! repository's git dir, with discovery fenced by `GIT_CEILING_DIRECTORIES`.
//! The same text runs through `bash -s` on this host for the source and a local
//! destination, and through one `ssh` session for a remote destination, so the
//! remote tips and shallow frontier come from one connection.
//!
//! The source keeps as haves the destination tips it holds as objects. Its wants
//! are its own probed tips plus every stash entry. It then:
//! - walks the wants minus the haves (`rev-list --objects --missing=print`),
//!   counting objects it does not hold and tallying `%(objectsize:disk)` of the
//!   rest (informational: a stored size, not a sent size);
//! - streams `pack-objects --stdout --thin --revs --delta-base-offset` over the
//!   same wants and haves into a byte counter. That count,
//!   `missing_thin_pack_bytes`, is the W6 M1 gate metric (R-N74). The pack
//!   never reaches disk.
//!
//! Nothing is fetched, written or updated on either side. Every Git call runs
//! with `GIT_NO_LAZY_FETCH=1`, `--no-optional-locks`, `maintenance.auto=false`,
//! `gc.auto=0` and `core.hooksPath=/dev/null`, and the probe refuses a Git older
//! than 2.44, which would ignore `GIT_NO_LAZY_FETCH`.
//!
//! A destination whose refs cannot prove it holds their history is refused
//! (R-N75): a partial clone, or a shallow repository whose frontier differs
//! from the source's. Matching frontiers still estimate.
//!
//! This is the lower-bound first round of negotiation. Ancestor probing of
//! uncovered tips (`GitHaveQuery`) is not modelled, so a destination tip the
//! source lacks contributes nothing, even where the destination holds much of
//! that tip's history.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
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

/// One line of at most [`DETAIL_LIMIT`] characters from a child's stderr.
fn detail(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let words: Vec<String> = text.split_whitespace().map(redact).collect();
    let line = words.join(" ");
    if line.is_empty() {
        return None;
    }
    let mut bounded: String = line
        .chars()
        .filter(|c| !c.is_control())
        .take(DETAIL_LIMIT)
        .collect();
    if line.chars().count() > DETAIL_LIMIT {
        bounded.push_str("...");
    }
    Some(bounded)
}

/// Drop anything credential-shaped from one word of diagnostic text.
fn redact(word: &str) -> String {
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
    let lower = word.to_ascii_lowercase();
    if TOKEN_PREFIXES.iter().any(|prefix| word.contains(prefix))
        || ["password", "passwd", "token", "secret", "authorization"]
            .iter()
            .any(|key| lower.contains(key) && (word.contains('=') || word.contains(':')))
    {
        return "[redacted]".to_owned();
    }
    if let Some((scheme, rest)) = word.split_once("://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        if let Some((_, host)) = authority.rsplit_once('@') {
            let tail = rest.get(authority.len()..).unwrap_or("");
            return format!("{scheme}://[redacted]@{host}{tail}");
        }
    }
    word.to_owned()
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
            "informational=source_history_bytes,missing_bytes_disk,missing_*_bytes_disk".to_owned(),
        );
        lines
    }
}

/// Measure, read-only, what carrying `source` to `destination` would move.
///
/// # Errors
/// Refuses a path that is not a repository root, a destination whose refs do
/// not prove their history (R-N75), an unreachable remote, or Git output that
/// is not the shape these commands promise. A refusal raised by a child
/// process carries a bounded excerpt of its stderr.
pub fn estimate(
    source: &Path,
    destination: &Destination,
) -> std::result::Result<CarryEstimate, Refused> {
    let own = run_probe(&mut local_probe(source))?;
    let offer = match destination {
        Destination::Local(path) => run_probe(&mut local_probe(path))?,
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
    if !offer.shallow.is_empty() && offer.shallow != own.shallow {
        return Err(Refused::new(
            BulkloadRefusal::GitHavesUnprovable,
            Some("destination is shallow at a frontier other than the source's".to_owned()),
        ));
    }
    let haves = present(source, &offer.tips)?;
    let stash = stash_entries(source)?;
    let mut wants = own.tips;
    wants.extend(stash.iter().cloned());
    let closure = walk(source, &revisions(&wants, &[]))?;
    let (missing, thin_pack) = if haves.is_empty() {
        (
            closure,
            thin_pack(source, &revisions(&wants, &[]), closure)?,
        )
    } else {
        let request = revisions(&wants, &haves);
        let missing = walk(source, &request)?;
        (missing, thin_pack(source, &request, missing)?)
    };
    Ok(CarryEstimate {
        destination_tip_count: offer.tips.len(),
        destination_shallow_count: offer.shallow.len(),
        haves_used: haves.len(),
        source_shallow_count: own.shallow.len(),
        source_partial: own.partial,
        stash_entries: stash.len(),
        source: closure,
        missing,
        thin_pack,
    })
}

/// [`git`] plus the estimate's no-write, no-network hardening (F1):
/// `--no-optional-locks`, `gc.auto=0` and `core.hooksPath=/dev/null` come
/// from [`git`]; this adds `maintenance.auto=false` and `GIT_NO_LAZY_FETCH`.
fn hardened(repository: &Path) -> Command {
    let mut command = git(repository);
    command
        .args(["-c", "maintenance.auto=false"])
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
}

/// Exit status of [`PROBE_SCRIPT`] when its argument is not a repository root.
const PROBE_NOT_A_REPOSITORY: i32 = 4;
/// Exit status of [`PROBE_SCRIPT`] when Git is older than 2.44.
const PROBE_GIT_TOO_OLD: i32 = 5;

/// The offer probe, run by `bash -s -- PATH` locally and over ssh alike.
///
/// POSIX sh, so bash 3.2 (macOS) and bash 5 (sting) read it the same way. It
/// runs no program but Git (`git version`, `rev-parse`, `config --get*` and
/// `for-each-ref`) and reads the shallow file with the shell's `read`. Output: one `partial 0|1` line, then `shallow`
/// and `tip` lines carrying one oid each, then `end`.
pub const PROBE_SCRIPT: &str = r#"set -eu
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS GIT_CEILING_DIRECTORIES GIT_DISCOVERY_ACROSS_FILESYSTEM
export GIT_NO_LAZY_FETCH=1 GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0 GIT_NO_REPLACE_OBJECTS=1 GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
g() { git --no-optional-locks -c maintenance.auto=false -c gc.auto=0 -c core.hooksPath=/dev/null -c core.fsmonitor=false "$@"; }
version=$(git version) || exit 5
version=${version#git version }
major=${version%%.*}
minor=${version#*.}
minor=${minor%%[!0-9]*}
case "$major" in ''|*[!0-9]*) exit 5 ;; esac
case "$minor" in ''|*[!0-9]*) exit 5 ;; esac
if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 44 ]; }; then exit 5; fi
[ "$#" -eq 1 ] || exit 2
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
common=$(g -C "$root" rev-parse --path-format=absolute --git-common-dir)
shallow=$(g -C "$root" rev-parse --path-format=absolute --git-path shallow)
partial=0
if value=$(g -C "$root" config --get extensions.partialClone); then
  [ -z "$value" ] || partial=1
else
  [ "$?" -eq 1 ] || exit 1
fi
if value=$(g -C "$root" config --type=bool --get-regexp '^remote\..*\.promisor$'); then
  case "$value" in *' true'*) partial=1 ;; esac
else
  [ "$?" -eq 1 ] || exit 1
fi
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
            None if line == "end" => ended = true,
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        }
    }
    if !ended {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    probe.partial = partial.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
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

/// Destination tips that exist as objects in `source`, in oid order. With
/// `GIT_NO_LAZY_FETCH` a partial source answers `missing` for a tip only its
/// promisor holds instead of fetching it (F1).
fn present(source: &Path, tips: &BTreeSet<String>) -> Result<Vec<String>> {
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
            haves.push(value.to_owned());
        }
    }
    if answered != tips.len() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(haves)
}

/// Every stash reflog entry, newest first; empty when there is no stash.
fn stash_entries(source: &Path) -> Result<Vec<String>> {
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
/// tallied by type.
fn walk(source: &Path, request: &str) -> Result<Tally> {
    let mut list = hardened(source)
        .args([
            "rev-list",
            "--objects",
            "--no-object-names",
            "--missing=print",
            "--stdin",
        ])
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
/// `pack.useSparse=false`: Git's default sparse edge marking may pack objects
/// the haves already reach (one extra tree or blob for bulkload in the cohort
/// run), so the pack would no longer be exactly the walked set. A pack whose
/// header count still differs from the walk is refused, not reported.
fn thin_pack(source: &Path, request: &str, missing: Tally) -> Result<ThinPack> {
    if missing.objects() == 0 {
        return Ok(ThinPack::default());
    }
    let mut pack = hardened(source)
        .args([
            "-c",
            "pack.useSparse=false",
            "pack-objects",
            "--stdout",
            "--thin",
            "--revs",
            "--delta-base-offset",
            "--missing=allow-any",
            "-q",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let pipes = (pack.stdin.take(), pack.stdout.take());
    let counted = std::thread::scope(|scope| {
        let (Some(mut stdin), Some(stdout)) = pipes else {
            return Err(BulkloadRefusal::Io(None));
        };
        let writer = scope.spawn(move || stdin.write_all(request.as_bytes()));
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
                    || line == "version=${version#git version }",
                "{line}"
            );
        }
        assert!(remote_command("/srv/r").starts_with("env GIT_NO_LAZY_FETCH=1 "));
        let source = hardened(Path::new("/nonexistent"));
        let args: Vec<_> = source.get_args().filter_map(|arg| arg.to_str()).collect();
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
}
