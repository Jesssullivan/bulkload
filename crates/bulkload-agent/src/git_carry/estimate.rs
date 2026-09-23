//! Read-only measurement of what git carry v2 would move (R-N60 baseline).
//!
//! The destination offers its de-duplicated ref tip oids and its shallow list,
//! exactly the negotiation input of the v2 design (bulkload#48, D2). The source
//! keeps as haves the tips it also holds as objects, then walks every object
//! reachable from its refs, `HEAD` and every stash entry but not from those
//! haves. Nothing is fetched, written or updated on either side: the source runs
//! `rev-list`, `cat-file --batch-check`, `for-each-ref` and `reflog show` with
//! optional locks disabled. A remote destination is reached with
//! `ssh -T -oBatchMode=yes HOST` and runs only
//! `git -C PATH for-each-ref '--format=%(objectname)'` and, through `bash -s`,
//! `cat` of its shallow file.
//!
//! This is the lower-bound first round of negotiation. Ancestor probing of
//! uncovered tips (`GitHaveQuery`) is not modelled, so a destination tip the
//! source lacks contributes nothing, even where the destination holds much of
//! that tip's history. `objectsize:disk` is the source's stored size of each
//! object (a pack delta or a compressed loose object), not the size of the thin
//! pack a later `pack-objects` would build.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{git, oid, output, shallow};
use crate::{BulkloadRefusal, Result};

/// Where the destination's tips are read from.
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

/// The shallow file of a remote repository, by convention: `PATH/shallow` for
/// a bare repository named `*.git`, otherwise `PATH/.git/shallow`.
fn remote_shallow_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if Path::new(trimmed)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("git"))
    {
        format!("{trimmed}/shallow")
    } else {
        format!("{trimmed}/.git/shallow")
    }
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
    /// Listed by the walk but absent from the object store (partial clones).
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

/// What git carry v2 would have to move from one source to one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarryEstimate {
    /// Distinct oids among the destination's refs.
    pub destination_tip_count: usize,
    /// Lines in the destination's shallow file.
    pub destination_shallow_count: usize,
    /// Destination tips that exist as objects in the source.
    pub haves_used: usize,
    /// Lines in the source's shallow file.
    pub source_shallow_count: usize,
    /// Stash reflog entries walked in addition to the refs and `HEAD`.
    pub stash_entries: usize,
    /// The whole closure of the source's refs, `HEAD` and stash entries.
    pub source: Tally,
    /// That closure minus everything reachable from the haves.
    pub missing: Tally,
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
                self.destination_tip_count - self.haves_used
            ),
            format!("missing_objects={}", self.missing.objects()),
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
        lines
    }
}

/// Measure, read-only, what carrying `source` to `destination` would move.
///
/// # Errors
/// Refuses an unreadable repository, an unreachable remote, or Git output that
/// is not the shape these commands promise.
pub fn estimate(source: &Path, destination: &Destination) -> Result<CarryEstimate> {
    let (tips, destination_shallow) = match destination {
        Destination::Local(path) => local_offer(path)?,
        Destination::Remote { host, path } => remote_offer(host, path)?,
    };
    let haves = present(source, &tips)?;
    let stash = stash_entries(source)?;
    let source_shallow = shallow_lines(&shallow::frontier(source)?)?;
    let closure = walk(source, &stash, &[])?;
    let missing = if haves.is_empty() {
        closure
    } else {
        walk(source, &stash, &haves)?
    };
    Ok(CarryEstimate {
        destination_tip_count: tips.len(),
        destination_shallow_count: destination_shallow.len(),
        haves_used: haves.len(),
        source_shallow_count: source_shallow.len(),
        stash_entries: stash.len(),
        source: closure,
        missing,
    })
}

fn tip_set(bytes: &[u8]) -> Result<BTreeSet<String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let mut tips = BTreeSet::new();
    for line in text.lines() {
        if !oid(line) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        tips.insert(line.to_owned());
    }
    Ok(tips)
}

fn shallow_lines(bytes: &[u8]) -> Result<Vec<String>> {
    Ok(tip_set(bytes)?.into_iter().collect())
}

type Offer = (BTreeSet<String>, Vec<String>);

fn local_offer(repository: &Path) -> Result<Offer> {
    let tips = tip_set(&output(
        git(repository).args(["for-each-ref", "--format=%(objectname)"]),
    )?)?;
    let shallow = shallow_lines(&shallow::frontier(repository)?)?;
    Ok((tips, shallow))
}

fn ssh(host: &str, command: &str, script: &[u8]) -> Result<std::process::Output> {
    let mut child = Command::new("ssh")
        .args(["-T", "-oBatchMode=yes", host, command])
        .stdin(if script.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| BulkloadRefusal::GitUnavailable)?;
    // The script is a few hundred bytes and `bash -s` reads it before it
    // answers, so writing it first cannot deadlock.
    let written = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(script));
    let result = child.wait_with_output()?;
    written?;
    match result.status.code() {
        Some(255) | None => Err(BulkloadRefusal::GitUnavailable),
        Some(_) => Ok(result),
    }
}

/// Exit status the shallow probe uses for "this repository is not shallow".
const NOT_SHALLOW: i32 = 3;

fn remote_offer(host: &str, path: &str) -> Result<Offer> {
    if !remote_host(host) || !remote_path(path) || !path.starts_with('/') {
        return Err(BulkloadRefusal::PathNotPortable);
    }
    // Single quotes mean the same thing to POSIX sh and to fish for this
    // character set; `%(objectname)` must be quoted for fish.
    let refs = ssh(
        host,
        &format!("git -C '{path}' for-each-ref '--format=%(objectname)'"),
        &[],
    )?;
    if !refs.status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let tips = tip_set(&refs.stdout)?;
    // `bash -s` with the script on stdin sidesteps the login shell (fish on
    // sting) and tells "no shallow file" apart from a failed read.
    let script = format!(
        "set -eu\nf='{}'\nif [ -e \"$f\" ]; then exec cat -- \"$f\"; fi\nexit {NOT_SHALLOW}\n",
        remote_shallow_path(path)
    );
    let shallow = ssh(host, "bash -s", script.as_bytes())?;
    let shallow = match shallow.status.code() {
        Some(0) => shallow_lines(&shallow.stdout)?,
        Some(NOT_SHALLOW) => Vec::new(),
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    };
    Ok((tips, shallow))
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
    let written = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(bytes));
        let result = child.wait_with_output();
        (writer.join(), result)
    });
    let (writer, result) = written;
    writer.map_err(|_| BulkloadRefusal::Io(None))??;
    let result = result?;
    if !result.status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(result.stdout)
}

/// Destination tips that exist as objects in `source`, in oid order.
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
        git(source).args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
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
    let stash = output(git(source).args(["for-each-ref", "--format=%(objectname)", "refs/stash"]))?;
    if stash.is_empty() {
        return Ok(Vec::new());
    }
    let entries = output(git(source).args(["reflog", "show", "--format=%H", "refs/stash"]))?;
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

/// `rev-list --objects --missing=allow-any --all <stash> --not <haves>`,
/// streamed straight into `cat-file --batch-check` and tallied by type.
///
/// `--all` covers every ref and `HEAD`. Stash entries and the negated haves go
/// through stdin, which `rev-list` reads completely before walking, so writing
/// it first cannot deadlock; a `--not` read from stdin never negates `--all`.
fn walk(source: &Path, stash: &[String], haves: &[String]) -> Result<Tally> {
    let mut request = String::new();
    for value in stash {
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
    let mut list = git(source)
        .args([
            "rev-list",
            "--objects",
            "--no-object-names",
            "--missing=allow-any",
            "--all",
            "--stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut list_input = list.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
    let written = list_input.write_all(request.as_bytes());
    drop(list_input);
    let list_output = list.stdout.take().ok_or(BulkloadRefusal::Io(None))?;
    let check = git(source)
        .args([
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize:disk)",
        ])
        .stdin(Stdio::from(list_output))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    // Always reap the walker, whatever happened to the checker.
    let (tally, check_status) = match check {
        Ok(mut check) => {
            let tally = check
                .stdout
                .take()
                .ok_or(BulkloadRefusal::Io(None))
                .and_then(|stdout| tally(BufReader::new(stdout)));
            (tally, check.wait())
        }
        Err(error) => (Err(error.into()), Ok(std::process::ExitStatus::default())),
    };
    let list_status = list.wait()?;
    written?;
    let tally = tally?;
    if !list_status.success() || !check_status?.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(tally)
}

fn tally(reader: impl std::io::BufRead) -> Result<Tally> {
    let mut tally = Tally::default();
    for line in reader.lines() {
        tally.add(&line?)?;
    }
    Ok(tally)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::super::text;
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

    #[test]
    fn shallow_destination_is_reported() {
        let fixture = Fixture::new("shallow-destination");
        let source = fixture.repo("source");
        commit(&source, "one.txt", "1");
        let second = commit(&source, "two.txt", "2");
        let destination = fixture.shallow_clone(&source, "destination", 1);
        let result = estimate(&source, &Destination::Local(destination)).unwrap();
        assert_eq!(result.destination_shallow_count, 1);
        assert_eq!(result.source_shallow_count, 0);
        assert!(result.haves_used >= 1);
        assert_eq!(
            result.missing.objects(),
            hand_count(&source, &["--all", "--not", &second])
        );
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
        assert_eq!(remote_shallow_path("/srv/r"), "/srv/r/.git/shallow");
        assert_eq!(remote_shallow_path("/srv/r.git/"), "/srv/r.git/shallow");
    }

    #[test]
    fn estimate_lines_are_key_value() {
        let mut result = CarryEstimate {
            destination_tip_count: 3,
            destination_shallow_count: 0,
            haves_used: 2,
            source_shallow_count: 0,
            stash_entries: 0,
            source: Tally::default(),
            missing: Tally::default(),
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
    }
}
