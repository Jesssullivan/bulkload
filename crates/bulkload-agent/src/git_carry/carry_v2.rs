//! Git carry v2, M1: negotiated thin packs instead of bundles (R-N60).
//!
//! This is bulkload#48's transport-independent library layer; the v5 wire's
//! Git sub-stream carries its values once W4's frames land.
//!
//! **Negotiation (R-N113, R-N116, R-N75).** The destination offers its tips
//! (every ref and worktree `HEAD`, de-duplicated), its shallow frontier and
//! whether it is a partial clone ([`Offer`], read by the same probe as
//! `git-carry-estimate`). The source keeps as haves *exactly* the offered tips
//! it holds, ordered ancestors first: the topological order of their peeled
//! commits, with held tips that peel to no commit (a tag of a tree, a ref to a
//! blob) last (spike D1; they cannot make upload-pack drop a have). The first
//! round is [`FirstRound`]. A partial-clone destination, a shallow one whose
//! frontier differs from the source's (R-N75), or a full one when the source
//! is shallow (R-N131), is refused `GIT_HAVES_UNPROVABLE` before anything is
//! listed or sent; a shallow file is never written into a full destination.
//!
//! **Object list.** `rev-list --objects-edge --missing=print <wants> --not
//! <haves>` (`--objects-edge-aggressive` for a shallow destination, as
//! upload-pack's `--shallow` selects), with `pack.useSparse=false` and
//! `pack.useBitmaps=false` pinned, as the estimate pins them, plus
//! `pack.threads=2` and `pack.windowMemory=64m`. An object the walk reaches
//! but the source lacks refuses before anything is sent. The `-<oid>` edge
//! lines become pack-objects' preferred bases, so every segment is thin.
//!
//! **Segments.** The object lines are assigned to segments of about
//! [`DEFAULT_SEGMENT_CAP`] of stored size (`%(objectsize:disk)`), in
//! (type, basename, path) order so one file's versions stay together across
//! renames (spike Q2); each segment then keeps rev-list order. A list that
//! fits one segment is therefore packed exactly as a single list, and its pack
//! is byte-identical to upload-pack's under the same git build and pins. Each
//! segment is `pack-objects --stdout --delta-base-offset` fed the edge lines
//! and its own object lines (list mode, no `--thin`: spike Q1 caveat 1), so a
//! delta's base is inside the segment or an edge object the destination holds,
//! and each segment indexes alone with `index-pack --fix-thin`.
//!
//! **Persistence and resume.** A [`PackPlan`] is persisted as `<pack_id>.list`
//! in a private [`ListStore`]; `pack_id` is the BLAKE3 of its bytes, which
//! record the request, the cap, the edges and every segment's lines as raw
//! bytes (paths need not be UTF-8). `GitResume{pack_id, next_segment}` reloads
//! the list and packs only segments `>= next_segment`. Pack bytes depend on
//! the git build and `pack.threads`, so a segment's BLAKE3 is recorded as sent
//! ([`SegmentReceipt`]) and resume never requires a regenerated segment to
//! match it (spike D6); `index-pack` recomputing every oid is the integrity
//! check.
//!
//! **Stderr (R-N121).** Every child runs with `LC_ALL=C`. A child's stderr is
//! classified, never echoed; with a [`StderrStore`] its raw bytes are kept
//! privately under a keyed digest.

// `Refused` carries a `BulkloadRefusal`, whose path-carrying variants
// (nested-repository custody, #53) put it just over clippy's 128-byte
// large-error threshold. A refusal is the cold path; the estimate keeps the
// same shape for the same reason.
#![allow(clippy::result_large_err)]

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::estimate::{
    child_refusal, drain, hardened, has_control, local_probe, run_probe, stash_entries, Refused,
    Repository, StderrStore,
};
use crate::BulkloadRefusal;

mod ingest;
mod journal;
mod lists;
mod negotiate;
mod plan;
mod send;

pub use ingest::{Ingest, IngestPlan, IngestReceipt, RefUpdate, SegmentAck, Target};
pub use journal::JournalStore;
pub use lists::ListStore;
pub use negotiate::{first_round, FirstRound};
pub use plan::{PackPlan, DEFAULT_SEGMENT_CAP};
pub use send::SegmentReceipt;

/// A carry v2 result: a value, or a refusal whose child stderr is classified.
pub type Outcome<T> = std::result::Result<T, Refused>;

/// The pack pins every M1 pack-building call runs under, as the estimate and
/// the upload-pack oracle do (R-N97; spike plan change 4).
pub const PACK_PINS: &str =
    "pack.useSparse=false pack.useBitmaps=false pack.threads=2 pack.windowMemory=64m";

/// The source repository, as the offer probe resolved it.
#[derive(Debug)]
pub struct Source {
    repository: Repository,
    root: PathBuf,
    common: PathBuf,
    tips: BTreeSet<String>,
    shallow: BTreeSet<String>,
    git_build: String,
}

impl Source {
    /// Probe `path`, which must be a repository's root (a work tree's top
    /// level or a bare repository's git dir), read-only.
    ///
    /// # Errors
    /// Refuses a path with a control character or that is not a repository
    /// root, a Git too old to honour `GIT_NO_LAZY_FETCH`, a `store` whose
    /// state dir lies inside the repository, and Git output of the wrong
    /// shape.
    pub fn probe(path: &Path, store: Option<&StderrStore>) -> Outcome<Self> {
        if has_control(path.as_os_str()) {
            return Err(BulkloadRefusal::PathNotPortable.into());
        }
        let inside =
            |paths: &[&Path]| store.is_some_and(|store| paths.iter().any(|at| store.is_inside(at)));
        if inside(&[path]) {
            return Err(overlap());
        }
        let probe = run_probe(&mut local_probe(path), store)?;
        if inside(&[&probe.root, &probe.git_dir, &probe.common]) {
            return Err(overlap());
        }
        let repository = Repository {
            git_dir: probe.git_dir,
            ceiling: probe.ceiling,
        };
        let version = run_child(
            hardened(&repository).arg("version"),
            &[],
            store,
            "git_version_failed",
            |stdout| {
                let mut text = String::new();
                stdout.read_to_string(&mut text)?;
                Ok(text)
            },
        )?;
        let git_build = version
            .trim()
            .strip_prefix("git version ")
            .filter(|build| build.bytes().all(|b| (b' '..=b'~').contains(&b)))
            .ok_or(BulkloadRefusal::GitUnavailable)?
            .to_owned();
        Ok(Self {
            repository,
            root: probe.root,
            common: probe.common,
            tips: probe.tips,
            shallow: probe.shallow,
            git_build,
        })
    }

    /// What `git-carry-estimate` wants from this source: every probed tip and
    /// every stash reflog entry. The M1 gate compares against the estimate
    /// over the same wants.
    ///
    /// # Errors
    /// Git output of the wrong shape.
    pub fn wants(&self) -> Outcome<BTreeSet<String>> {
        let mut wants = self.tips.clone();
        wants.extend(stash_entries(&self.repository)?);
        Ok(wants)
    }

    /// The git build that packs, as `git version` names it. Pack bytes are
    /// reproducible only for the same build and pins (spike plan change 6),
    /// so receipts record it beside each segment's digest.
    #[must_use]
    pub fn git_build(&self) -> &str {
        &self.git_build
    }

    /// The source's shallow frontier (empty for a full repository).
    #[must_use]
    pub const fn shallow(&self) -> &BTreeSet<String> {
        &self.shallow
    }

    /// Whether `store`'s state dir lies inside this repository: its root,
    /// its git dir or its common dir.
    fn contains_state(&self, inside: impl Fn(&Path) -> bool) -> bool {
        [
            self.root.as_path(),
            self.repository.git_dir.as_path(),
            self.common.as_path(),
        ]
        .into_iter()
        .any(inside)
    }
}

/// What a destination offers: its de-duplicated tips (every ref and every
/// worktree's `HEAD` and per-worktree refs), its shallow frontier, and
/// whether it is a partial clone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Offer {
    /// Tip oids.
    pub tips: BTreeSet<String>,
    /// Shallow frontier oids (empty for a full repository).
    pub shallow: BTreeSet<String>,
    /// Whether the destination is a partial clone (R-N75 refuses it).
    pub partial: bool,
}

impl Offer {
    /// Read the offer of the repository rooted at `path` on this host, with
    /// the estimate's probe (read-only).
    ///
    /// # Errors
    /// As [`Source::probe`].
    pub fn probe(path: &Path, store: Option<&StderrStore>) -> Outcome<Self> {
        if has_control(path.as_os_str()) {
            return Err(BulkloadRefusal::PathNotPortable.into());
        }
        let inside =
            |paths: &[&Path]| store.is_some_and(|store| paths.iter().any(|at| store.is_inside(at)));
        if inside(&[path]) {
            return Err(overlap());
        }
        let probe = run_probe(&mut local_probe(path), store)?;
        if inside(&[&probe.root, &probe.git_dir, &probe.common]) {
            return Err(overlap());
        }
        Ok(Self {
            tips: probe.tips,
            shallow: probe.shallow,
            partial: probe.partial,
        })
    }
}

const fn overlap() -> Refused {
    Refused::because(
        BulkloadRefusal::SnapshotRootsOverlap,
        "state_dir_inside_repository",
    )
}

/// The source command every M1 pack-building call uses: the estimate's
/// hardened invocation on the probed git dir (`GIT_NO_LAZY_FETCH`,
/// `--no-optional-locks`, `LC_ALL=C`, hooks and auto-gc off,
/// `pack.threads=2`, `pack.windowMemory=64m`) plus `pack.useSparse=false` and
/// `pack.useBitmaps=false`, with `GIT_QUARANTINE_PATH` stripped like every
/// other redirecting variable.
fn pinned(source: &Source) -> Command {
    let mut command = hardened(&source.repository);
    command
        .args(["-c", "pack.useSparse=false", "-c", "pack.useBitmaps=false"])
        .env_remove("GIT_QUARANTINE_PATH");
    command
}

/// Run `command` with `input` on stdin (written from a thread, so a large
/// answer never deadlocks against an unread request), hand its stdout to
/// `read`, and drain its stderr into the classifier and, with a `store`, a
/// private capture (R-N121). If `read` stops early, stdout is closed so the
/// child ends on its own; the child is always waited for, never signalled.
///
/// A failure of `read` wins over the child's status (a closed sink makes the
/// child fail too). A non-zero exit refuses `GIT_INVENTORY_MALFORMED` with
/// `reason` and the stderr class.
fn run_child<T>(
    command: &mut Command,
    input: &[u8],
    store: Option<&StderrStore>,
    reason: &'static str,
    read: impl FnOnce(&mut dyn Read) -> crate::Result<T>,
) -> Outcome<T> {
    let capture = store.map(StderrStore::capture).transpose()?;
    let spawned = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = spawned else {
        if let (Some(store), Some(capture)) = (store, capture) {
            store.discard(capture);
        }
        return Err(Refused::because(BulkloadRefusal::GitUnavailable, reason));
    };
    let (Some(mut stdin), Some(mut stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        child.wait()?;
        if let (Some(store), Some(capture)) = (store, capture) {
            store.discard(capture);
        }
        return Err(BulkloadRefusal::Io(None).into());
    };
    let (written, drained, result) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let reader = scope.spawn(move || drain(stderr, capture));
        let result = read(&mut stdout);
        drop(stdout);
        (writer.join(), reader.join(), result)
    });
    let status = child.wait()?;
    let drained = drained.map_err(|_| BulkloadRefusal::Io(None))?;
    if let Err(error) = result {
        if let (Some(store), (_, _, Some(capture), _)) = (store, drained) {
            store.discard(capture);
        }
        return Err(error.into());
    }
    if !status.success() {
        return Err(child_refusal(
            BulkloadRefusal::GitInventoryMalformed,
            Some(reason),
            store,
            drained,
        ));
    }
    if let (Some(store), (_, _, Some(capture), _)) = (store, drained) {
        store.discard(capture);
    }
    // A child that succeeded without reading all of its input did not see
    // the whole request.
    written.map_err(|_| BulkloadRefusal::Io(None))??;
    result.map_err(Refused::from)
}

/// Whether `bytes` is a whole object name as Git prints one: 40 or 64
/// lowercase hex digits. Offers arrive off the wire, so nothing looser (an
/// uppercase digit, a revision expression, an option) is accepted.
fn is_oid(bytes: &[u8]) -> bool {
    matches!(bytes.len(), 40 | 64) && bytes.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Read `reader` as `\n`-terminated lines of raw bytes, calling `line` on
/// each without its terminator. A final unterminated line is refused: every
/// Git listing this module reads ends its lines.
fn lines(
    reader: &mut dyn Read,
    mut line: impl FnMut(&[u8]) -> crate::Result<()>,
) -> crate::Result<()> {
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, reader);
    let mut buffer = Vec::with_capacity(256);
    loop {
        buffer.clear();
        let read = std::io::BufRead::read_until(&mut reader, b'\n', &mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        let Some(body) = buffer.strip_suffix(b"\n") else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        line(body)?;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn git_at(repo: &Path) -> Command {
        let mut command = super::super::git(repo);
        command
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@invalid");
        command
    }

    /// #73 round-2 D1, the reviewer's case 6 (`r2_probe.rs`): a round built
    /// by hand inside the crate, claiming a full destination (or another
    /// frontier) for a shallow source, is refused by `PackPlan::build`
    /// before anything is listed. Outside the crate such a round cannot be
    /// built at all: the fields are private.
    #[test]
    fn a_hand_built_round_cannot_skip_the_shallow_rules() {
        let root = std::env::temp_dir().join(format!(
            "bulkload-carry-v2-d1-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let origin = root.join("origin");
        super::super::output(
            git_at(&root)
                .args(["init", "-q", "-b", "main"])
                .arg(&origin),
        )
        .unwrap();
        for index in 0..3 {
            std::fs::write(origin.join("f.txt"), format!("{index}\n")).unwrap();
            super::super::output(git_at(&origin).args(["add", "f.txt"])).unwrap();
            super::super::output(git_at(&origin).args(["commit", "-q", "-m", "c"])).unwrap();
        }
        let url = format!("file://{}", origin.display());
        let shallow = root.join("shallow");
        super::super::output(
            git_at(&root)
                .args(["clone", "-q", "--depth", "1", url.as_str()])
                .arg(&shallow),
        )
        .unwrap();
        let source = Source::probe(&shallow, None).unwrap();
        assert!(!source.shallow().is_empty());
        let wants: Vec<String> = source.wants().unwrap().into_iter().collect();
        let other = "1".repeat(40);
        for (frontier, reason) in [
            (Vec::new(), "source_shallow_destination_full"),
            (vec![other], "destination_shallow_frontier_differs"),
        ] {
            let round = FirstRound {
                wants: wants.clone(),
                haves: Vec::new(),
                shallow: frontier,
                destination_tips: 0,
                non_commit_haves: 0,
            };
            let refused = PackPlan::build(&source, &round, DEFAULT_SEGMENT_CAP, None).unwrap_err();
            assert_eq!(refused.refusal, BulkloadRefusal::GitHavesUnprovable);
            assert_eq!(refused.reason, Some(reason));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
