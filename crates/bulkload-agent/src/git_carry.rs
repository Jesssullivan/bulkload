//! Git-native archival union. Native refs, HEAD, index and checkout are never written.
//!
//! Bundles preserve staged state separately from the worktree (including ignored
//! files), minus the fixed rebuildable set in [`REBUILDABLE_DIRECTORIES`], which
//! is recorded as custody instead of carried. Clean foreign repositories nested
//! under the worktree and gitlink (submodule) index entries are likewise
//! recorded as [`NestedRepository`] custody: their tracked content and history
//! are never carried (each is its own estate item), their ignored files are
//! carried as ordinary seats (R-N89). A gitlink entry stays in the staged tree.
//! This is not Git administration reconstruction. Capture is optimistic, not an
//! atomic filesystem snapshot.

use crate::counters::CountedSync as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{BulkloadRefusal, Result};

mod batch_objects;
pub mod carry_v2;
pub mod chain;
pub mod estimate;
mod raw_tree;
pub mod registered;
mod shallow;
pub mod shared;
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]
mod source_inert_tests;

/// The one Git hardening table (WP1 PR 1, S2; OI-1003-Q16). Every Git child
/// the v1 carry and the estimate spawn is built from it by [`git`], and the
/// estimate's [`estimate::PROBE_SCRIPT`] preamble is tested against it, so
/// the local builder and the remote probe cannot drift apart again.
pub(crate) mod git_env {
    /// Inherited variables that would redirect Git at another repository,
    /// object store, ceiling or configuration. Each is removed.
    pub const CLEARED: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ];

    /// Variables every Git child runs with: no prompt, no system or global
    /// configuration, no replace objects, no lazy (promisor) fetch, no
    /// optional locks, and the C locale so every parsed line is stable.
    pub const SET: &[(&str, &str)] = &[
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("LC_ALL", "C"),
        ("LANGUAGE", ""),
    ];

    /// `-c` overrides every Git child runs with: no hooks, no fsmonitor, no
    /// automatic gc or maintenance, and bounded pack resources.
    ///
    /// `fastimport.unpackLimit=0` keeps every `fast-import` pack whole. Below
    /// the limit (100 objects by default) fast-import explodes its pack into
    /// loose objects through Git's object writer, which freshens (re-stamps)
    /// any copy an alternate already holds: the source's, for a capture's raw
    /// tree (S2, #162). Its own pack store never freshens.
    pub const CONFIG: &[&str] = &[
        "core.hooksPath=/dev/null",
        "core.fsmonitor=false",
        "gc.auto=0",
        "maintenance.auto=false",
        "pack.threads=2",
        "pack.windowMemory=64m",
        "fastimport.unpackLimit=0",
    ];

    /// The object store a capture's private repository writes into, a
    /// sibling of its `objects` (S2, #162).
    ///
    /// Git freshens (re-stamps with `utime`) any existing copy of an object
    /// it is asked to write, in its own store or in any alternate. The
    /// private repository reads the source's store through
    /// `objects/info/alternates`, so a writer that could see it would write
    /// to the source. Every `hash-object -w`, `mktree` and `write-tree` of a
    /// capture therefore runs with `GIT_OBJECT_DIRECTORY` here
    /// ([`super::git_writer`]), and this store borrows nothing. The private
    /// repository's own store lists it as its first alternate, so every
    /// reader (bundle, pack, fetch, fast-import, update-ref) sees both.
    pub const WRITE_STORE: &str = "objects-written";
}

/// A Git child for the repository at `repo`, hardened from [`git_env`]:
/// `--no-optional-locks`, every `-c` override, every cleared and set
/// variable, and `GIT_CEILING_DIRECTORIES` at `repo`'s parent, so discovery
/// never climbs above the path it was given.
fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    for key in git_env::CLEARED {
        command.env_remove(key);
    }
    command.arg("--no-optional-locks");
    for value in git_env::CONFIG {
        command.args(["-c", value]);
    }
    command.arg("-C").arg(repo);
    for (key, value) in git_env::SET {
        command.env(key, value);
    }
    // A relative or root path has no absolute parent to stop at; Git then
    // discovers as it would have, and the explicit `-C` still names the root.
    if let Some(ceiling) = std::path::absolute(repo)
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        command.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
    command
}

/// A Git child that writes objects into the capture's private repository at
/// `private` (absolute, as [`prepare_private`] returns it) and sees no other
/// store: [`git`], plus `GIT_OBJECT_DIRECTORY` at its
/// [`git_env::WRITE_STORE`]. The source's object store is then strictly
/// read-only to the capture (S2, #162).
fn git_writer(private: &Path) -> Command {
    let mut command = git(private);
    writing_privately(&mut command, private);
    command
}

// Point `command`'s object writes at `private`'s write store; see [`git_writer`].
fn writing_privately<'a>(command: &'a mut Command, private: &Path) -> &'a mut Command {
    command.env("GIT_OBJECT_DIRECTORY", private.join(git_env::WRITE_STORE))
}

/// Whether the repository at `repo` is a partial clone.
///
/// That is a promisor remote, a partial-clone filter or
/// `extensions.partialClone` in its configuration (common, worktree and every
/// linked worktree's), or a `.promisor` pack in its object store or any
/// alternate (depth 5, as the estimate probe reads).
///
/// Reading such a repository can fault in a lazy fetch, a network write the
/// source never asked for; v1 carry refuses it before reading anything else
/// (`GIT_SOURCE_PARTIAL_CLONE`, WP1 PR 1, S2).
///
/// # Errors
/// Refuses a repository Git cannot read.
pub fn partial_clone(repo: &Path) -> Result<bool> {
    let common = common_repository(repo)?;
    let mut scopes = vec![
        None,
        Some(common.join("config")),
        Some(common.join("config.worktree")),
    ];
    if let Ok(worktrees) = fs::read_dir(common.join("worktrees")) {
        for admin in worktrees {
            scopes.push(Some(admin?.path().join("config.worktree")));
        }
    }
    for scope in scopes {
        if scope.as_ref().is_some_and(|file| !file.is_file()) {
            continue;
        }
        for (promisor, query) in [
            (false, &["--get-regexp", r"^extensions\.partialclone$"][..]),
            (
                true,
                &["--type=bool", "--get-regexp", r"^remote\..*\.promisor$"][..],
            ),
            (
                false,
                &["--get-regexp", r"^(remote\..*|core)\.partialclonefilter$"][..],
            ),
        ] {
            let mut command = git(repo);
            command.arg("config");
            if let Some(file) = &scope {
                command.args(["--includes", "--file"]).arg(file);
            }
            let result = command.args(query).output()?;
            match result.status.code() {
                Some(0) => {}
                Some(1) => continue,
                _ => return Err(BulkloadRefusal::GitInventoryMalformed),
            }
            // `key value` lines; a promisor counts only when it is true.
            if result.stdout.split(|byte| *byte == b'\n').any(|line| {
                let value = line
                    .iter()
                    .position(|byte| *byte == b' ')
                    .and_then(|space| line.get(space + 1..))
                    .unwrap_or_default();
                if promisor {
                    value == b"true"
                } else {
                    !value.is_empty()
                }
            }) {
                return Ok(true);
            }
        }
    }
    let objects = PathBuf::from(text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ]))?);
    promisor_packs(&objects, 0)
}

// A `.promisor` pack in `store` or, through `info/alternates`, in any store it
// borrows from, at most five levels deep (the estimate probe's bound).
fn promisor_packs(store: &Path, depth: u8) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt as _;
    match fs::read_dir(store.join("pack")) {
        Ok(entries) => {
            for entry in entries {
                if Path::new(&entry?.file_name())
                    .extension()
                    .is_some_and(|extension| extension == "promisor")
                {
                    return Ok(true);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let alternates = match fs::read(store.join("info/alternates")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if depth >= 5 {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    for line in alternates.split(|byte| *byte == b'\n') {
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        if line.starts_with(b"\"") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let alternate = store.join(std::ffi::OsStr::from_bytes(line));
        if alternate.is_dir() && promisor_packs(&alternate, depth + 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Refuse a partial-clone source before any other read (WP1 PR 1).
///
/// # Errors
/// `GIT_SOURCE_PARTIAL_CLONE` for a partial clone; whatever
/// [`partial_clone`] refuses.
pub fn refuse_partial_clone(repo: &Path) -> Result<()> {
    if partial_clone(repo)? {
        return Err(BulkloadRefusal::GitSourcePartialClone);
    }
    Ok(())
}

fn output(command: &mut Command) -> Result<Vec<u8>> {
    let Output { status, stdout, .. } = command.output()?;
    if !status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(stdout)
}

// `git bundle verify`, with the refusal split by cause (#106): a bundle whose
// declared prerequisite commits the repository does not hold refuses
// GIT_INVENTORY_MISSING_PREREQUISITE, so a run report names the cause and its
// recovery (fetch the named commits as objects, then retry). Any other
// failure stays GIT_INVENTORY_MALFORMED.
fn verify_bundle(repo: &Path, bundle: &Path) -> Result<()> {
    if git(repo)
        .args(["bundle", "verify"])
        .arg(bundle)
        .output()?
        .status
        .success()
    {
        return Ok(());
    }
    for prerequisite in shared::prerequisites(bundle)? {
        let held = git(repo)
            .args(["cat-file", "-e", &format!("{prerequisite}^{{commit}}")])
            .output()?
            .status
            .success();
        if !held {
            return Err(BulkloadRefusal::GitInventoryMissingPrerequisite);
        }
    }
    Err(BulkloadRefusal::GitInventoryMalformed)
}

fn text(command: &mut Command) -> Result<String> {
    String::from_utf8(output(command)?)
        .map(|s| s.trim_end().to_owned())
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)
}

/// Run `command` with `bytes` on stdin and return its stdout.
///
/// `bytes` are written from a scoped thread while this thread drains stdout:
/// a child that answers per request (`cat-file --batch-check`, `hash-object
/// --stdin`) stops reading once its stdout pipe fills, so writing every
/// request before reading any answer would block both processes forever
/// (64 KiB pipes on Linux, 16 KiB on Darwin; a few thousand refs).
fn input(command: &mut Command, bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
    let (written, result) = std::thread::scope(|scope| {
        // `stdin` moves into the writer and closes when it returns, so the
        // child sees end of input exactly once every request is written.
        let writer = scope.spawn(move || stdin.write_all(bytes));
        let result = child.wait_with_output();
        (writer.join(), result)
    });
    let result = result?;
    written.map_err(|_| BulkloadRefusal::WorkerLost)??;
    if !result.status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(result.stdout)
}

/// Run a git child that packs objects (`bundle create`, `pack-objects`),
/// feeding it `stdin`, and reap it with `wait4` so its own resource usage is
/// measured. Returns whether it succeeded and its storage reads in bytes
/// (`ru_inblock` x 512; see the `counters` module notes for why this is a
/// lower bound, and only a lower bound on Darwin). The caller sets stdout;
/// stderr is discarded as `output` discards it.
fn pack_child(command: &mut Command, stdin: Option<&[u8]>) -> Result<(bool, u64)> {
    use std::io::Write;
    use std::process::Stdio;
    command.stderr(Stdio::null()).stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command.spawn()?;
    // The child is reaped below whatever the write did, so a failed write
    // never leaves an unreaped child behind; its error wins afterwards.
    let written = match (stdin, child.stdin.take()) {
        (Some(bytes), Some(mut pipe)) => pipe.write_all(bytes).map_err(BulkloadRefusal::from),
        (Some(_), None) => Err(BulkloadRefusal::Io(None)),
        (None, _) => Ok(()),
    };
    let reaped = reap(&child);
    written?;
    reaped
}

// wait4 on a child std has not waited for. std never reaps a child on drop,
// so after this the `Child` handle is only dropped, never waited on.
fn reap(child: &std::process::Child) -> Result<(bool, u64)> {
    let pid = libc::pid_t::try_from(child.id()).map_err(|_| BulkloadRefusal::Io(None))?;
    let mut status: libc::c_int = 0;
    // SAFETY: `rusage` is a plain C struct of integers; all-zero is a valid
    // value, and wait4 overwrites it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `pid` is this process's own child, spawned by the caller and
        // not yet reaped (std waits only when asked, and nothing asked). Both
        // out-pointers are valid, exclusive borrows for the duration of the call.
        let reaped = unsafe { libc::wait4(pid, &raw mut status, 0, &raw mut usage) };
        if reaped == pid {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    let success = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
    let blocks = u64::try_from(usage.ru_inblock).unwrap_or(0);
    Ok((success, blocks.saturating_mul(512)))
}

fn metadata(private: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let value = input(
        git_writer(private).args(["hash-object", "-w", "--stdin"]),
        bytes,
    )?;
    let value = std::str::from_utf8(&value)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim();
    let tree = input(
        git_writer(private).args(["mktree", "-z"]),
        format!("100644 blob {value}\tvalue\0").as_bytes(),
    )?;
    let tree = std::str::from_utf8(&tree)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim();
    set_ref(
        private,
        &format!("refs/carry-export/{name}"),
        &commit_tree(private, tree, name)?,
    )
}

fn refs(repo: &Path) -> Result<String> {
    text(git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]))
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn set_ref(repo: &Path, name: &str, value: &str) -> Result<()> {
    if !oid(value) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    output(git(repo).args(["update-ref", name, value, ""]))?;
    Ok(())
}

fn snapshot_command(private: &Path, worktree: &Path, index: &Path) -> Command {
    let mut command = git(worktree);
    command
        .env("GIT_DIR", private)
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_INDEX_FILE", index);
    command.args(["-c", "core.bare=false"]);
    command
}

/// Author and committer of every archival commit: the fixed identity and
/// instant (2000-01-01T00:00:00Z) `commit-tree` was always run under.
const ARCHIVAL_SIGNATURE: &str = "Bulkload archival capture <bulkload@localhost> 946684800 +0000";

// A parentless archival commit of `tree` in the private repository's write
// store (S2, #162).
fn commit_tree(private: &Path, tree: &str, label: &str) -> Result<String> {
    commit_object(&mut git_writer(private), tree, label)
}

// A parentless commit of `tree` under [`ARCHIVAL_SIGNATURE`], written by
// `writer` with `hash-object -t commit`: byte for byte what `git commit-tree
// -m label` writes under that identity (pinned by a test), without requiring
// `tree` in the writer's store. A capture's writer sees only its write store,
// and a raw tree fast-import found in a source pack is not stored there.
fn commit_object(writer: &mut Command, tree: &str, label: &str) -> Result<String> {
    if !oid(tree) || label.contains('\n') {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let commit = format!(
        "tree {tree}\nauthor {ARCHIVAL_SIGNATURE}\ncommitter {ARCHIVAL_SIGNATURE}\n\n{label}\n"
    );
    let value = input(
        writer.args(["hash-object", "-t", "commit", "-w", "--stdin"]),
        commit.as_bytes(),
    )?;
    let value = std::str::from_utf8(&value)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim_end();
    if !oid(value) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(value.to_owned())
}

// Build a raw tree without running attributes/filters or starting Git per file.
fn capture_tree(private: &Path, repo: &Path, _index: &Path) -> Result<String> {
    let rows = filesystem_rows(repo)?;
    let pass = raw_tree::capture(private, repo, &rows, &raw_tree::Reuse::new())?;
    // Verification, not capture: a destination changing under the audit is
    // never tolerated as drift, exactly as before drift tolerance existed.
    if !pass.drift.is_empty() || rows != filesystem_rows(repo)? {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    Ok(pass.tree)
}

// `stash` is the reflog read with the carried authority, never re-read here:
// the bundle carries exactly the stash commits the snapshot saw.
fn capture_refs(private: &Path, inventory: &str, stash: &[u8]) -> Result<()> {
    let mut pending = std::collections::BTreeMap::new();
    for line in inventory.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        // Older carry spellings remain fully recoverable, once. They are
        // never discarded merely because provenance predates this schema.
        let exported = name
            .strip_prefix("refs/carry/v1/")
            .filter(|tail| canonical_tail(tail))
            .map_or_else(
                || format!("refs/carry-export/{name}"),
                |tail| format!("refs/carry-export/union/v1/{tail}"),
            );
        if !oid(value) || exported.as_bytes().contains(&0) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if pending.insert(exported, value.to_owned()).is_some() {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    let stash = std::str::from_utf8(stash).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    for value in stash.lines() {
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let name = format!("refs/carry-export/stashes/{value}");
        pending.entry(name).or_insert_with(|| value.to_owned());
    }
    // One create-only Git transaction instead of one process per ref. NUL
    // framing preserves legal quote characters without command interpolation.
    let mut commands = Vec::new();
    for (name, value) in pending {
        commands.extend_from_slice(b"create ");
        commands.extend_from_slice(name.as_bytes());
        commands.push(0);
        commands.extend_from_slice(value.as_bytes());
        commands.push(0);
    }
    if !commands.is_empty() {
        input(
            git(private).args(["update-ref", "--stdin", "-z"]),
            &commands,
        )?;
    }
    Ok(())
}

fn source_slug(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 64
        && source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn canonical_tail(tail: &str) -> bool {
    let mut parts = tail.splitn(3, '/');
    let source = parts.next().unwrap_or_default();
    let snapshot = parts.next().unwrap_or_default();
    let suffix = parts.next().unwrap_or_default();
    source_slug(source) && snapshot.len() == 64 && oid(snapshot) && !suffix.is_empty()
}

/// Canonical common repository used to serialize applies sharing Git storage.
///
/// # Errors
/// Refuses unavailable or malformed Git administration.
pub fn common_repository(repo: &Path) -> Result<PathBuf> {
    let common = text(git(repo).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]))?;
    Ok(fs::canonicalize(common)?)
}

/// The typed inputs of a reusable capture key.
///
/// The key is an opaque digest and cannot say *what* moved. These parts can, so
/// a caller re-reading them after a pass can tell drift it may tolerate (the ref
/// inventory and the worktree census: what a sibling agent lane or a build tool
/// produces) from Git authority it must refuse (HEAD, the index, configuration,
/// the shallow boundary, nested worktree custody, the omitted rebuildable roots).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyParts {
    repo: Vec<u8>,
    common: Vec<u8>,
    inventory: String,
    head: String,
    symbolic: Vec<u8>,
    index: Vec<u8>,
    exclude: Vec<u8>,
    stash: Vec<u8>,
    rows: Vec<crate::RowSchema>,
    configuration: Vec<(String, Vec<u8>)>,
    boundary: Vec<u8>,
    nested: Vec<NestedWorktree>,
    nested_repositories: Vec<NestedRepository>,
    omitted: Vec<Vec<u8>>,
    identities: Vec<(u64, u64)>,
}

/// Reusable-key domain for the authority digest of a capture's key parts.
const CAPTURE_AUTHORITY_DOMAIN: &[u8] = b"tcfs-git-capture-authority-v1\0";

fn framed(hash: &mut blake3::Hasher, bytes: &[u8]) -> Result<()> {
    hash.update(
        &u64::try_from(bytes.len())
            .map_err(|_| BulkloadRefusal::BudgetExceeded)?
            .to_le_bytes(),
    );
    hash.update(bytes);
    Ok(())
}

impl KeyParts {
    /// The apparent bytes of every regular-file seat this census names: the
    /// worktree payload a capture under these parts would stream, before
    /// compression and without history. An estimate for space planning
    /// (#101), never a byte count of a bundle.
    #[must_use]
    pub fn census_bytes(&self) -> u64 {
        self.rows
            .iter()
            .filter(|row| row.kind == bulkload_proto::FileKind::Regular)
            .fold(0u64, |total, row| total.saturating_add(row.size))
    }

    /// Foreign nested repositories and gitlinks these parts were computed
    /// over: the custody a capture under the same key records (R-N73). A
    /// receipt for a reuse hit names exactly these.
    #[must_use]
    pub fn nested_repositories(&self) -> &[NestedRepository] {
        &self.nested_repositories
    }

    /// The reusable capture key these parts hash to.
    ///
    /// The domain tag, the input order, the u64-length framing and the
    /// present-only sidecar domains are exactly what they were before drift
    /// tolerance existed, so retained captures of unchanged checkouts keep their
    /// keys bit-for-bit and are never re-done.
    ///
    /// # Errors
    /// Refuses inputs too large to frame or encode.
    pub fn digest(&self) -> Result<[u8; 32]> {
        let rows = postcard::to_allocvec(&self.rows).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let configuration =
            postcard::to_allocvec(&self.configuration).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"tcfs-git-reusable-capture-v1\0");
        for bytes in [
            self.repo.as_slice(),
            self.common.as_slice(),
            self.inventory.as_bytes(),
            self.head.as_bytes(),
            &self.symbolic,
            &self.index,
            &self.exclude,
            &self.stash,
            &rows,
            &configuration,
            &self.boundary,
        ] {
            framed(&mut hash, bytes)?;
        }
        // A sidecar, hashed only when present: repos without nested worktrees keep
        // their existing keys, so retained captures are not re-done for a schema.
        if !self.nested.is_empty() {
            let nested =
                postcard::to_allocvec(&self.nested).map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(NESTED_WORKTREES_DOMAIN);
            framed(&mut hash, &nested)?;
        }
        // A sidecar, hashed only when present, in the place it has always had:
        // repos without a foreign nested repository or a gitlink keep their
        // exact keys. Each nest's HEAD, cleanliness, unpushed count and
        // carried-ignored count are in it (R-N73, R-N89).
        if !self.nested_repositories.is_empty() {
            let nested = postcard::to_allocvec(&self.nested_repositories)
                .map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(NESTED_REPOSITORIES_DOMAIN);
            framed(&mut hash, &nested)?;
        }
        // A sidecar, hashed only when present, and only the omitted roots -- never
        // their sizes. A repository with no omission keeps the key it already had,
        // and a build writing inside an omitted root cannot move this key, which is
        // the whole point: rebuildable churn must not refuse a 40-minute capture.
        if !self.omitted.is_empty() {
            let omitted =
                postcard::to_allocvec(&self.omitted).map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(REBUILDABLE_DOMAIN);
            framed(&mut hash, &omitted)?;
        }
        for (dev, ino) in &self.identities {
            for value in [i128::from(*dev), i128::from(*ino)] {
                hash.update(&value.to_le_bytes());
            }
        }
        Ok(*hash.finalize().as_bytes())
    }

    /// Digest of every key input outside the two drift-tolerant ones.
    ///
    /// Two passes with equal authority differ, if at all, only in the ref
    /// inventory and the worktree census. A later pass with the same authority
    /// as a retained drifted capture extends that capture: it re-reads exactly
    /// the seats whose identity moved and reuses every other blob (R25).
    ///
    /// # Errors
    /// Refuses inputs too large to frame or encode.
    pub fn authority(&self) -> Result<[u8; 32]> {
        let configuration =
            postcard::to_allocvec(&self.configuration).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let nested =
            postcard::to_allocvec(&self.nested).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let omitted =
            postcard::to_allocvec(&self.omitted).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let identities =
            postcard::to_allocvec(&self.identities).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let mut hash = blake3::Hasher::new();
        hash.update(CAPTURE_AUTHORITY_DOMAIN);
        for bytes in [
            self.repo.as_slice(),
            self.common.as_slice(),
            self.head.as_bytes(),
            &self.symbolic,
            &self.index,
            &self.exclude,
            &self.stash,
            &configuration,
            &self.boundary,
            &nested,
            &omitted,
            &identities,
        ] {
            framed(&mut hash, bytes)?;
        }
        // Appended only when present, so a checkout without nests keeps the
        // authority digest its retained `.parts` sidecar recorded.
        if !self.nested_repositories.is_empty() {
            let nested = postcard::to_allocvec(&self.nested_repositories)
                .map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(NESTED_REPOSITORIES_DOMAIN);
            framed(&mut hash, &nested)?;
        }
        Ok(*hash.finalize().as_bytes())
    }

    /// Whether every key input outside the two drift-tolerant ones is unchanged.
    ///
    /// The ref inventory and the worktree census are the concurrency classes a
    /// sibling agent lane or a build tool produces. Everything else moving is
    /// Git authority changing under the capture and stays a refusal (R-N30).
    #[must_use]
    pub fn drift_only(&self, other: &Self) -> bool {
        self.repo == other.repo
            && self.common == other.common
            && self.head == other.head
            && self.symbolic == other.symbolic
            && self.index == other.index
            && self.exclude == other.exclude
            && self.stash == other.stash
            && self.configuration == other.configuration
            && self.boundary == other.boundary
            && self.nested == other.nested
            && self.nested_repositories == other.nested_repositories
            && self.omitted == other.omitted
            && self.identities == other.identities
    }

    /// Whether any seat in this census is racy against a pass that began at
    /// `started_ns`: its mtime or ctime is at or after that start less
    /// [`RACY_GRANULARITY_NS`], or later than `now_ns`.
    ///
    /// An equal key is a stat-identity claim about every seat at once. A racy
    /// seat can be rewritten at the same size inside one timestamp tick without
    /// moving the key, so a capture with one is never reused whole (R-N76).
    /// `now_ns` is this pass's own clock reading: a seat stamped later than
    /// it is from the future and racy too.
    #[must_use]
    pub fn racy_since(&self, started_ns: i128, now_ns: i128) -> bool {
        self.rows.iter().any(|row| racy(row, started_ns, now_ns))
    }

    /// Whether any seat is stamped later than `now_ns`, this pass's own clock.
    ///
    /// Such a seat is racy on every pass until the clock passes its stamp, so
    /// it blocks every whole-capture reuse; the receipt names it (round-3 N5).
    #[must_use]
    pub fn stamped_after(&self, now_ns: i128) -> bool {
        self.rows
            .iter()
            .any(|row| row.mtime_ns > now_ns || row.ctime_ns > now_ns)
    }

    /// Whether these parts name exactly the authority an export carried.
    #[must_use]
    pub fn carries(&self, carried: &CarriedAuthority) -> bool {
        self.head == carried.head
            && self.symbolic == carried.symbolic
            && self.index == carried.index
            && self.exclude == carried.exclude
            && self.stash == carried.stash
            && self.configuration == carried.configuration
            && self.boundary == carried.boundary
    }

    /// Everything that moved across a whole capture: between these pre-pass
    /// parts and the inventory `export` snapshotted, under the export itself
    /// (already in [`Export::drift`]), between the export's last ref read and
    /// `later`, and between these parts and `later` directly.
    ///
    /// The export's own window is not the capture's. A ref deleted before the
    /// export's snapshot and re-created after its last ref read leaves `self`
    /// equal to `later`, yet the bundle lacks it; only comparing the keyed
    /// inventories with the export's own names it (R-N72).
    ///
    /// Git authority is never drift. HEAD, the symbolic HEAD, the index, the
    /// exclude file, the stash reflog, the configuration or the shallow
    /// frontier that moved away and back between a key and the export's
    /// snapshot or end, leaving the two keys equal, still refuses: the bundle
    /// carries what the source held at neither key.
    ///
    /// # Errors
    /// Refuses [`BulkloadRefusal::GitAuthorityChanged`] unless both key parts
    /// name exactly the authority the export carried, as
    /// [`KeyParts::drift_to`] does, and on malformed inventories.
    pub fn drift_across(&self, export: &Export, later: &Self) -> Result<CaptureDrift> {
        if !self.carries(&export.authority) || !later.carries(&export.authority) {
            return Err(BulkloadRefusal::GitAuthorityChanged);
        }
        let mut drift = self.drift_to(later)?;
        drift.extend(reference_drift(&self.inventory, &export.refs_before)?);
        drift.extend(reference_drift(&export.refs_after, &later.inventory)?);
        drift.seal()?;
        Ok(drift)
    }

    /// Everything that moved between these pre-pass parts and `later`, the
    /// parts re-read after the pass.
    ///
    /// An export takes its own ref and census snapshot, later than a caller's
    /// pre-pass key. A ref deleted in between is invisible to the export, yet
    /// the pre-pass key still names it. The result is empty exactly when the
    /// two parts are equal, and only then may the pre-pass key be recorded as
    /// a clean reuse key; otherwise the capture is recorded as drifted, and a
    /// later pass must re-capture or extend it, never reuse it (R-N72).
    ///
    /// Seats compare on the whole census row here, directory timestamps
    /// included: the pre-pass key hashes every field of every row.
    ///
    /// # Errors
    /// Refuses [`BulkloadRefusal::GitAuthorityChanged`] when anything outside
    /// the ref inventory and the worktree census moved, or when the parts
    /// differ in a way no drift row can name.
    pub fn drift_to(&self, later: &Self) -> Result<CaptureDrift> {
        if !self.drift_only(later) {
            return Err(BulkloadRefusal::GitAuthorityChanged);
        }
        let mut drift = CaptureDrift::default();
        drift.extend(reference_drift(&self.inventory, &later.inventory)?);
        drift.extend(seat_drift_by(&self.rows, &later.rows, |a, b| a == b));
        drift.seal()?;
        // Fail closed: a key that moved must name what moved.
        if drift.is_empty() != (self == later) {
            return Err(BulkloadRefusal::GitAuthorityChanged);
        }
        Ok(drift)
    }
}

/// The Git authority one capture carries: HEAD, the symbolic HEAD, the index
/// bytes, the exclude file, the stash reflog, the configuration files and the
/// shallow frontier.
///
/// The reusable key hashes exactly these values, and an export carries exactly
/// the values it read once, at its snapshot. A caller compares them with its
/// pre- and post-pass key parts: authority that moved away and back between
/// those reads is a stale bundle behind an equal key (R-N72).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedAuthority {
    head: String,
    symbolic: Vec<u8>,
    index: Vec<u8>,
    exclude: Vec<u8>,
    stash: Vec<u8>,
    configuration: Vec<(String, Vec<u8>)>,
    boundary: Vec<u8>,
    /// Gitlinks (and collapsed gitlinks) of this index and HEAD: custody the
    /// capture records, derived from the same read (R-N73, R-N110).
    gitlinks: Vec<NestedRepository>,
}

// One read of the authority a key hashes and an export carries. The stash
// reflog is read only when `inventory` lists refs/stash, exactly as the key
// always read it. No census walk.
fn read_authority(repo: &Path, inventory: &str) -> Result<CarriedAuthority> {
    let head = text(git(repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let symbolic = git(repo).args(["symbolic-ref", "-q", "HEAD"]).output()?;
    if !symbolic.status.success() && symbolic.status.code() != Some(1) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let (_, index, gitlinks) = source_index(repo)?;
    let exclude_path = text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?;
    let exclude = match fs::read(exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let stash = if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
        output(git(repo).args(["reflog", "show", "--format=%H", "refs/stash"]))?
    } else {
        Vec::new()
    };
    Ok(CarriedAuthority {
        head,
        symbolic: symbolic.stdout,
        index,
        exclude,
        stash,
        configuration: source_configuration(repo)?,
        boundary: shallow::frontier(repo)?,
        gitlinks,
    })
}

/// The typed inputs of the reusable capture key for `repo`, default policy.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn capture_key_parts(repo: &Path) -> Result<KeyParts> {
    capture_key_parts_with_policy(repo, CapturePolicy::default())
}

/// [`capture_key_parts`] under an explicit capture policy.
///
/// This reads Git metadata and filesystem metadata, not ordinary file contents.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn capture_key_parts_with_policy(repo: &Path, policy: CapturePolicy) -> Result<KeyParts> {
    capture_key_parts_with_planned(repo, policy, &[])
}

/// [`capture_key_parts_with_policy`] for a checkout some of whose nested
/// repositories are planned as their own estate items (R-N114).
///
/// `planned` holds the canonical sources of those items: such a nest keeps
/// its custody row and every refusal, but contributes no seats.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn capture_key_parts_with_planned(
    repo: &Path,
    policy: CapturePolicy,
    planned: &[PathBuf],
) -> Result<KeyParts> {
    use std::os::unix::ffi::OsStrExt;
    let repo = fs::canonicalize(repo)?;
    let common = common_repository(&repo)?;
    let inventory = refs(&repo)?;
    let authority = read_authority(&repo, &inventory)?;
    let CarriedAuthority {
        head,
        symbolic,
        index,
        exclude,
        stash,
        configuration,
        boundary,
        gitlinks,
    } = authority;
    let census = capture_census_planned(&repo, &common, policy, planned)?;
    let mut identities = Vec::with_capacity(2);
    for directory in [&repo, &common] {
        let identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(directory)?);
        identities.push((identity.dev, identity.ino));
    }
    Ok(KeyParts {
        repo: repo.as_os_str().as_bytes().to_vec(),
        common: common.as_os_str().as_bytes().to_vec(),
        inventory,
        head,
        symbolic,
        index,
        exclude,
        stash,
        rows: census.rows,
        configuration,
        boundary,
        nested: census.nested_worktrees,
        nested_repositories: nested_custody(&census.nested_repositories, &gitlinks),
        omitted: census.omitted,
        identities,
    })
}

/// Identity of a reusable capture, including new transit refs and full census.
///
/// This reads Git metadata and filesystem metadata, not ordinary file contents.
/// Callers must compare before/after keys and retain the successful bundle.
/// It is not a filesystem journal, atomic snapshot, or a no-rewalk claim.
///
/// Uses the default [`CapturePolicy`], which omits the fixed rebuildable set.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn reusable_capture_key(repo: &Path) -> Result<[u8; 32]> {
    reusable_capture_key_with_policy(repo, CapturePolicy::default())
}

/// [`reusable_capture_key`] under an explicit capture policy.
///
/// A policy that carries the rebuildable set produces exactly the key the
/// pre-omission agent produced, so retained full-fidelity captures stay reusable.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn reusable_capture_key_with_policy(repo: &Path, policy: CapturePolicy) -> Result<[u8; 32]> {
    capture_key_parts_with_policy(repo, policy)?.digest()
}

/// Reconstruct a missing index from a same-HEAD capture without touching payload.
///
/// Receipt must be new, outside the worktree, and on the index filesystem for
/// atomic create-only publication. Existing indexes always refuse. This restores
/// captured staging, not a claim that the destination's lost staging was known.
///
/// # Errors
/// Refuses HEAD/admin changes, active Git operations, occupied index/receipt,
/// unsupported capture/index state, or cross-filesystem atomic publication.
pub fn repair_missing_index(
    bundle: &Path,
    repo: &Path,
    source: &str,
    receipt: &Path,
) -> Result<()> {
    repair_missing_index_inner(bundle, repo, source, receipt, |_| Ok(()), |_| Ok(()))
}

/// [`repair_missing_index`], first handing `bind` the staged digest (#95).
///
/// `bind` runs before anything is written, so a caller that records the repair
/// in an apply ledger can name the exact capture, or refuse an unplanned one.
///
/// # Errors
/// `bind`'s refusal, then everything [`repair_missing_index`] refuses.
pub fn repair_missing_index_bound(
    bundle: &Path,
    repo: &Path,
    source: &str,
    receipt: &Path,
    bind: impl FnOnce(&[u8; 32]) -> Result<()>,
) -> Result<()> {
    repair_missing_index_inner(bundle, repo, source, receipt, bind, |_| Ok(()))
}

fn repair_missing_index_inner(
    bundle: &Path,
    repo: &Path,
    source: &str,
    receipt: &Path,
    bind: impl FnOnce(&[u8; 32]) -> Result<()>,
    before_publish: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let staged = stage_bundle(bundle)?;
    bind(&staged.digest())?;
    let bundle = staged.path();
    let repo = fs::canonicalize(repo)?;
    let admin = PathBuf::from(text(git(&repo).args(["rev-parse", "--absolute-git-dir"]))?);
    let index = PathBuf::from(text(git(&repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "index",
    ]))?);
    require_missing(&index)?;
    for name in [
        "index.lock",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        require_missing(&admin.join(name))?;
    }
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let heads = shallow::headers(&repo, bundle)?;
    let find = |suffix: &str| -> Result<String> {
        heads
            .lines()
            .find_map(|line| {
                line.split_once(' ')
                    .filter(|(_, name)| *name == format!("refs/carry-export/{suffix}"))
            })
            .map(|(value, _)| value.to_owned())
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)
    };
    if find("head")? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    let controls = admin_controls(&admin)?;
    let admin_identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(&admin)?);
    let receipt_parent =
        fs::canonicalize(receipt.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?;
    let receipt = receipt_parent.join(
        receipt
            .file_name()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    );
    if receipt.starts_with(&repo) || receipt.starts_with(&admin) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    fs::DirBuilder::new().mode(0o700).create(&receipt)?;
    fs::write(
        receipt.join("original-administration.postcard"),
        postcard::to_allocvec(&(head.clone(), true, &controls))
            .map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    fs::File::open(receipt.join("original-administration.postcard"))?.sync_file_counted()?;
    fs::File::open(&receipt)?.sync_dir_counted()?;
    fs::File::open(&receipt_parent)?.sync_dir_counted()?;
    import_verified(&repo, bundle, source)?;
    // A gitlink in the staged tree is the captured index entry, restored as
    // is: reading it needs no submodule commit (R-N73, B2).
    let private_index = receipt.join("captured.index");
    output(
        git(&repo)
            .env("GIT_INDEX_FILE", &private_index)
            .args(["read-tree", &format!("{}^{{tree}}", find("staged")?)]),
    )?;
    restore_intent_to_add(&repo, Some(&private_index), &heads)?;
    fs::set_permissions(&private_index, fs::Permissions::from_mode(0o600))?;
    fs::File::open(&private_index)?.sync_file_counted()?;
    fs::File::open(&receipt)?.sync_dir_counted()?;
    let reservation = IndexReservation::acquire(admin.join("index.lock"))?;
    let current_identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(&admin)?);
    if (admin_identity.dev, admin_identity.ino) != (current_identity.dev, current_identity.ino)
        || controls != admin_controls(&admin)?
        || text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))? != head
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    before_publish(&index)?;
    if controls != admin_controls(&admin)?
        || text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))? != head
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    fs::hard_link(&private_index, &index)?;
    fs::File::open(&admin)?.sync_dir_counted()?;
    if text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    reservation.release()
}

/// Cooperates with native Git's index lock. The held descriptor identifies the
/// exact inode owned by this operation; a replaced/pre-existing lock is never
/// removed. Error paths release only this reservation, never another writer's.
struct IndexReservation {
    path: PathBuf,
    file: fs::File,
    released: bool,
}

impl IndexReservation {
    fn acquire(path: PathBuf) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self {
            path,
            file,
            released: false,
        })
    }

    fn remove_owned(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let original = self.file.metadata()?;
        let current = fs::symlink_metadata(&self.path)?;
        if !current.is_file() || (original.dev(), original.ino()) != (current.dev(), current.ino())
        {
            return Err(BulkloadRefusal::GitAuthorityChanged);
        }
        fs::remove_file(&self.path)?;
        fs::File::open(self.path.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?)?
            .sync_dir_counted()?;
        Ok(())
    }

    fn release(mut self) -> Result<()> {
        self.remove_owned()?;
        self.released = true;
        Ok(())
    }
}

impl Drop for IndexReservation {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.remove_owned();
        }
    }
}

fn require_missing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(BulkloadRefusal::GitDestinationOccupied),
    }
}

fn admin_controls(admin: &Path) -> Result<Vec<(String, Option<Vec<u8>>)>> {
    ["HEAD", "commondir", "gitdir", "config", "config.worktree"]
        .into_iter()
        .map(|name| {
            let bytes = match fs::read(admin.join(name)) {
                Ok(value) => Some(value),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            Ok((name.to_owned(), bytes))
        })
        .collect()
}

/// Export one worktree and all repository refs into a new, private directory.
///
/// Uses the default [`CapturePolicy`], which omits the fixed rebuildable set
/// and records it as custody. Use [`export_repository_with_policy`] for full
/// fidelity.
///
/// # Errors
/// Refuses unsupported/unmerged indexes and changing refs/index/worktree. A
/// gitlink (submodule) entry or a clean foreign nested repository is recorded
/// as [`NestedRepository`] custody; its tracked content and history are its
/// own capture, its ignored files are carried here (R-N89). A nest with any
/// staged, unstaged or untracked change, hidden index flags, a stash, a
/// detached-only commit, an operation in progress, filter commands, or paths
/// under it that this repository tracks refuses (R-N73, R-N83). On refusal
/// the private capture is retained for diagnosis; source state is never
/// changed.
pub fn export_repository(repo: &Path, capture: &Path) -> Result<PathBuf> {
    Ok(refusing_drift(export_repository_inner(
        repo,
        capture,
        &ExportOptions::default(),
    )?)?
    .bundle)
}

/// Export one worktree under an explicit capture policy and prerequisite.
///
/// The returned [`Export`] names every omitted rebuildable root and its
/// measured size, and every nested repository or gitlink whose content the
/// capture does not carry: the same custody the bundle's metadata refs carry.
/// Drift under the pass is still a refusal here; [`export_repository_with_drift`]
/// is the tolerant entry point.
///
/// # Errors
/// Refuses everything [`export_repository`] refuses, plus invalid prerequisites.
pub fn export_repository_with_policy(
    repo: &Path,
    capture: &Path,
    prerequisite: Option<&Path>,
    policy: CapturePolicy,
) -> Result<Export> {
    refusing_drift(export_repository_inner(
        repo,
        capture,
        &ExportOptions {
            prerequisite,
            policy,
            reuse: None,
            planned: &[],
            chain: None,
        },
    )?)
}

/// Export workspace state without repacking a shared base's commit closure.
///
/// The base must be transported and imported before this prerequisite bundle.
/// Callers must bind both bundles' digests in their durable capture record.
///
/// # Errors
/// Refuses invalid prerequisites or any state refused by standalone capture.
pub fn export_repository_with_prerequisite(
    repo: &Path,
    capture: &Path,
    base: &Path,
) -> Result<PathBuf> {
    Ok(refusing_drift(export_repository_inner(
        repo,
        capture,
        &ExportOptions {
            prerequisite: Some(base),
            policy: CapturePolicy::default(),
            reuse: None,
            planned: &[],
            chain: None,
        },
    )?)?
    .bundle)
}

/// Export one worktree and all repository refs, tolerating concurrent drift.
///
/// Refs appearing, moving or disappearing in the shared ref store, and worktree
/// seats appearing, changing or vanishing under the byte pass, are reported as
/// [`Export::drift`] instead of refusing the capture (R25: the host need not
/// halt agent work or git work). A drifted seat's bytes are carried by neither
/// the worktree tree nor `filesystem-v1`; the drift list is the statement that
/// they are absent, and the next pass re-reads exactly those seats (R-N28).
/// Git authority moving -- HEAD, the index, configuration, the shallow
/// boundary, nested worktree custody, the omitted rebuildable roots -- is still
/// a refusal (R-N30).
///
/// # Errors
/// Refuses changing Git authority, unsupported Git or filesystem state, invalid
/// prerequisites, and drift larger than [`DRIFT_ROW_LIMIT`].
pub fn export_repository_with_drift(
    repo: &Path,
    capture: &Path,
    options: &ExportOptions<'_>,
) -> Result<Export> {
    match export_repository_inner(repo, capture, options)? {
        Exported::Captured(export) => Ok(*export),
        Exported::ObjectStoreRewritten(_) => Err(BulkloadRefusal::GitAuthorityChanged),
    }
}

/// What a drift-tolerant export produced (WP1 PR 4).
#[derive(Debug)]
pub enum Exported {
    /// A bundle, clean or with drift rows.
    Captured(Box<Export>),
    /// No bundle: a Git child failed while the source's pack listing changed
    /// under the pass (a `gc`, `repack` or `prune` racing the capture), so the
    /// failure is the rewrite, not the repository. Drift custody with one
    /// [`DriftKind::ObjectStoreRewritten`] row; the next pass captures the
    /// rewritten store. Never a refusal (S5, R-N30).
    ObjectStoreRewritten(CaptureDrift),
}

/// The estate capture's export: [`export_repository_with_drift`], plus
/// object-store rewrites as custody.
///
/// A Git child failure under an object-store rewrite is reported as
/// [`Exported::ObjectStoreRewritten`] drift custody instead of a refusal.
///
/// # Errors
/// Refuses everything [`export_repository_with_drift`] refuses, except a Git
/// child failure while the source's pack listing changed.
pub fn export_repository_with_custody(
    repo: &Path,
    capture: &Path,
    options: &ExportOptions<'_>,
) -> Result<Exported> {
    export_repository_inner(repo, capture, options)
}

// The pre-drift contract for callers that never asked for tolerance.
fn refusing_drift(export: Exported) -> Result<Export> {
    match export {
        Exported::Captured(export) if export.drift.is_empty() => Ok(*export),
        _ => Err(BulkloadRefusal::GitAuthorityChanged),
    }
}

/// The identity of the source's pack listing: every entry of
/// `<common>/objects/pack` by name, inode and size, hashed in name order. A
/// `gc`, `repack` or `prune --expire` writes or removes packs, so it changes
/// this; reading through the store never does. A missing pack directory is
/// the empty listing.
fn pack_listing(common: &Path) -> Result<[u8; 32]> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let mut entries = Vec::new();
    match fs::read_dir(common.join("objects/pack")) {
        Ok(listing) => {
            for entry in listing {
                let entry = entry?;
                // A pack removed between the listing and its stat is the
                // rewrite itself; record it as absent.
                let (ino, size) = entry
                    .metadata()
                    .map_or((0, u64::MAX), |meta| (meta.ino(), meta.size()));
                entries.push((entry.file_name().as_bytes().to_vec(), ino, size));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    entries.sort_unstable();
    let mut hash = blake3::Hasher::new();
    for (name, ino, size) in &entries {
        framed(&mut hash, name)?;
        hash.update(&ino.to_le_bytes());
        hash.update(&size.to_le_bytes());
    }
    Ok(*hash.finalize().as_bytes())
}

fn export_repository_inner(
    repo: &Path,
    capture: &Path,
    options: &ExportOptions<'_>,
) -> Result<Exported> {
    use std::os::unix::fs::DirBuilderExt;
    let repo = fs::canonicalize(repo)?;
    fs::DirBuilder::new().mode(0o700).create(capture)?;
    let capture = fs::canonicalize(capture)?;
    if capture.starts_with(&repo) {
        return Err(BulkloadRefusal::GitAuthorityOutsideRoot);
    }
    // S2 (WP1 PR 1): a partial clone could lazily fetch under any read below.
    refuse_partial_clone(&repo)?;
    #[cfg(test)]
    mid_pass::fire(&repo, mid_pass::Stage::Snapshot);
    // WP1 PR 4: the pack listing is read with the authority, before any Git
    // child of the pass. The private repository reads the source's objects
    // through `alternates`, so a `gc` or `prune` racing the pass can make one
    // of its children fail; when the listing moved, that failure is drift
    // custody, not a malformed repository.
    let common = common_repository(&repo)?;
    let packs = pack_listing(&common)?;
    match export_pass(&repo, &capture, &common, options) {
        Err(BulkloadRefusal::GitInventoryMalformed) if pack_listing(&common)? != packs => {
            let mut drift = CaptureDrift::default();
            drift.extend([DriftRow::seat(
                DriftKind::ObjectStoreRewritten,
                b"objects/pack",
            )]);
            drift.seal()?;
            Ok(Exported::ObjectStoreRewritten(drift))
        }
        exported => exported.map(|export| Exported::Captured(Box::new(export))),
    }
}

// One export pass over the canonical `repo` into the canonical `capture`.
fn export_pass(
    repo: &Path,
    capture: &Path,
    common: &Path,
    options: &ExportOptions<'_>,
) -> Result<Export> {
    // Before the census, so every seat the census stamps is judged against it.
    let started_ns = pass_start_ns();
    let before_refs = refs(repo)?;
    // Read once and carried exactly: nothing below re-reads HEAD, the symbolic
    // HEAD, the index, exclude, the stash reflog, configuration or the frontier
    // for the bundle's content, only to compare at the end of the pass.
    let authority = read_authority(repo, &before_refs)?;
    let census = capture_census_planned(repo, common, options.policy, options.planned)?;
    let seats = &census.rows;
    let private = carry_authority(repo, capture, &before_refs, &authority)?;
    let index = capture.join("index");
    // A bare repository carries no index (S4, #162); Git reads the absent
    // file as the empty index, so its staged tree is the empty tree.
    if !authority.index.is_empty() {
        fs::write(&index, &authority.index)?;
    }
    // Built in the write store, which cannot see the source's (S2, #162), so
    // the index's blobs are not looked up (`--missing-ok`). Packing the bundle
    // below reads both stores and still needs every blob it carries.
    let staged = text(
        writing_privately(&mut snapshot_command(&private, repo, &index), &private)
            .args(["write-tree", "--missing-ok"]),
    )?;
    set_ref(
        &private,
        "refs/carry-export/staged",
        &commit_tree(&private, &staged, "bulkload staged tree")?,
    )?;
    #[cfg(test)]
    mid_pass::fire(repo, mid_pass::Stage::BytePass);
    // Seats a retained capture already holds at this exact identity are emitted
    // by object name. Nothing is opened for them and no source byte is re-read.
    let (reuse, reuse_unavailable) = offered_reuse(
        &private,
        options.reuse,
        &authority.boundary,
        seats,
        started_ns,
    )?;
    let pass = raw_tree::capture(&private, repo, seats, &reuse)?;
    let after = capture_census_planned(repo, common, options.policy, options.planned)?;
    // Git authority moving under the capture is never drift. The nested
    // worktree and nested repository censuses are compared apart from the
    // seats precisely because they carry each nested HEAD (and a foreign
    // nest's cleanliness, unpushed count and carried-ignored count), which
    // must keep its refusal (R-N73, B4); so is the omitted set, because it is
    // a key input a rebuild can move.
    let refs_after = refs(repo)?;
    if !authority_held(repo, &authority, &refs_after)?
        || census.nested_worktrees != after.nested_worktrees
        || census.nested_repositories != after.nested_repositories
        || census.omitted != after.omitted
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    let drift = observed_drift(pass.drift, &before_refs, &refs_after, seats, &after.rows)?;
    // A seat whose identity moved only after its bytes were streamed is already
    // in the tree; withdraw it rather than present bytes the pass cannot vouch for.
    let withdrawn = drift.withdrawn(seats);
    let tree = raw_tree::prune(&private, &pass.tree, &withdrawn)?;
    set_ref(
        &private,
        "refs/carry-export/worktree",
        &commit_tree(
            &private,
            &tree,
            "bulkload worktree including untracked and ignored files",
        )?,
    )?;
    // filesystem-v1 must never name a seat whose bytes were not captured: a
    // restore replays it as a mode and shape claim about the restored payload.
    let absent: std::collections::BTreeSet<&[u8]> = withdrawn.iter().map(Vec::as_slice).collect();
    let carried: Vec<&crate::RowSchema> = seats
        .iter()
        .filter(|row| !absent.contains(row.rel_path.as_slice()))
        .collect();
    metadata(
        &private,
        "filesystem-v1",
        &postcard::to_allocvec(&carried).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    // #106: intent-to-add entries, which the staged tree cannot hold, are
    // carried as index custody beside it, read from the carried index bytes.
    record_intent_to_add(&private, repo, &index, &carried)?;
    let nested_repositories = custody_metadata(&private, &census, &authority)?;
    // Omission is recorded, never silent. Sizes are measured once, here, and
    // deliberately excluded from both the reusable key and the before/after
    // census comparison: they are custody evidence about bytes this capture
    // chose not to carry, not an assertion that those bytes held still.
    let omitted = measure_omissions(repo, &census.omitted)?;
    if !omitted.is_empty() {
        metadata(
            &private,
            REBUILDABLE_METADATA,
            &postcard::to_allocvec(&omitted).map_err(|_| BulkloadRefusal::FrameCodec)?,
        )?;
    }
    mark_drift(&private, &drift)?;
    let bundle = capture.join("capture.bundle");
    // A plan base wins; otherwise a retained capture's source-held tips are
    // the prerequisites (WP2); otherwise the bundle is self-contained.
    let (pack, chained) = match (options.prerequisite, options.chain) {
        (None, Some(prior)) => shared::write_chained(&private, &bundle, repo, prior)?,
        (base, _) => (shared::write_bundle(&private, &bundle, base)?, false),
    };
    output(git(&private).args(["bundle", "verify"]).arg(&bundle))?;
    #[cfg(test)]
    mid_pass::fire(repo, mid_pass::Stage::AfterPass);
    Ok(Export {
        bundle,
        omitted,
        drift,
        bytes_read: pass.bytes_read,
        started_ns,
        reuse_unavailable,
        refs_before: before_refs,
        refs_after,
        authority,
        nested_repositories,
        pack,
        chained,
    })
}

// In-band, emitted only when the pass raced something: a clean capture's ref
// set, and therefore its bundle, is exactly what it was before drift tolerance
// existed. Every restore and import verb reads this marker back and refuses
// the bundle with CAPTURE_DRIFTED, whatever sidecars exist.
fn mark_drift(private: &Path, drift: &CaptureDrift) -> Result<()> {
    if drift.is_empty() {
        return Ok(());
    }
    metadata(
        private,
        CAPTURE_DRIFT_METADATA,
        &postcard::to_allocvec(drift).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )
}

// HEAD, the symbolic head, the exclude file, the shallow frontier and the
// configuration: administration every capture carries, before the byte pass.
fn capture_administration(private: &Path, authority: &CarriedAuthority) -> Result<()> {
    // Exactly the bytes the snapshot read; the metadata holds the ref name
    // without the trailing newline, as it always did.
    let symbolic = std::str::from_utf8(&authority.symbolic)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim_end();
    metadata(private, "head-symbolic", symbolic.as_bytes())?;
    metadata(private, "exclude", &authority.exclude)?;
    if !authority.boundary.is_empty() {
        metadata(private, "shallow-frontier-v1", &authority.boundary)?;
    }
    metadata(
        private,
        "configuration-v1",
        &postcard::to_allocvec(&authority.configuration)
            .map_err(|_| BulkloadRefusal::FrameCodec)?,
    )
}

// Everything one pass observed moving: what the byte pass saw seat by seat,
// what the ref store did, and what the post-pass census says about the seats.
fn observed_drift(
    in_pass: Vec<DriftRow>,
    refs_before: &str,
    refs_after: &str,
    seats_before: &[crate::RowSchema],
    seats_after: &[crate::RowSchema],
) -> Result<CaptureDrift> {
    let mut drift = CaptureDrift::default();
    drift.extend(in_pass);
    drift.extend(reference_drift(refs_before, refs_after)?);
    drift.extend(seat_drift(seats_before, seats_after));
    drift.seal()?;
    Ok(drift)
}

/// Wall-clock nanoseconds since the epoch.
///
/// This is the reference a pass judges racy seats against. A clock before the
/// epoch yields zero, which makes every seat racy: the failure mode is a
/// re-read, never a stale reuse.
#[must_use]
pub fn pass_start_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i128::try_from(elapsed.as_nanos()).unwrap_or(0))
}

// What a pass may reuse from the retained capture it was offered, if any.
fn offered_reuse(
    private: &Path,
    offered: Option<RetainedCapture<'_>>,
    boundary: &[u8],
    seats: &[crate::RowSchema],
    now_ns: i128,
) -> Result<(raw_tree::Reuse, Option<ReuseUnavailable>)> {
    Ok(match offered {
        None => (raw_tree::Reuse::new(), None),
        // Shallow custody carries no worktree tree; say so rather than
        // silently re-reading every seat.
        Some(_) if !boundary.is_empty() => {
            (raw_tree::Reuse::new(), Some(ReuseUnavailable::Shallow))
        }
        Some(retained) => match reusable_blobs(private, &retained, seats, now_ns)? {
            Ok(reuse) => (reuse, None),
            Err(why) => (raw_tree::Reuse::new(), Some(why)),
        },
    })
}

/// Whether a bundle's own headers carry the in-band drift marker.
///
/// A capture that drifted under its export omits the drifted seats' bytes; the
/// marker is the bundle's own statement of that, independent of any corpus
/// sidecar. A plain bundle lists `refs/carry-export/capture-drift-v1`; a
/// shallow envelope lifts it into its headers as `shallow-drift-v1`. Only the
/// headers are read: no pack is fetched and nothing is written.
fn drift_marked(heads: &str) -> bool {
    let plain = format!("refs/carry-export/{CAPTURE_DRIFT_METADATA}");
    heads.lines().any(|line| {
        line.split_once(' ')
            .is_some_and(|(_, name)| name == plain || name == shallow::DRIFT_MARKER)
    })
}

// A directory this process created exclusively (mode 0700, named by a
// process-wide counter, never reused), removed when dropped. Created in the
// first `near` directory that accepts it, else in TMPDIR.
#[derive(Debug)]
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create(near: Option<&Path>) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp = std::env::temp_dir();
        for parent in near.into_iter().chain(std::iter::once(temp.as_path())) {
            loop {
                let candidate = parent.join(format!(
                    ".bulkload-staged-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::DirBuilder::new().mode(0o700).create(&candidate) {
                    Ok(()) => return Ok(Self(candidate)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    // This parent refuses a private directory; try the next.
                    Err(_) => break,
                }
            }
        }
        Err(BulkloadRefusal::Io(None))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        // Best effort: a leftover private copy is disk, never custody.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A bundle staged for one restore or import: a private copy whose headers
/// were checked for the drift marker, and the only path the verb reads.
///
/// Checking one path and importing it again later is a window in which the
/// file can be replaced. The copy lives in a private directory next to the
/// bundle (the corpus, for an estate apply), falling back to TMPDIR, and is
/// written by this process while it hashes the bytes: it is a full copy, never
/// a clone, and costs one sequential read and write of the bundle. The check,
/// the digest and every later read see the same bytes. The directory is
/// removed when the stage is dropped.
#[derive(Debug)]
pub struct StagedBundle {
    // Held only so dropping the stage removes its private directory.
    _directory: PrivateDir,
    bundle: PathBuf,
    digest: [u8; 32],
}

impl StagedBundle {
    /// The staged copy every later read of this verb uses.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.bundle
    }

    /// blake3 of the staged bytes, computed while they were copied.
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

// Copy `source` to a new private file while hashing it: one read, one write,
// counted as `read_bundle_stage_bytes`, `blake3_bundle_stage_bytes` and
// `write_bundle_stage_bytes`.
fn copy_hashing(source: &Path, destination: &Path) -> Result<[u8; 32]> {
    use crate::counters::{add_len, update, Counter};
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    let mut from = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)?;
    let mut to = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let count = from.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        add_len(Counter::BundleStageRead, count);
        let chunk = buffer.get(..count).ok_or(BulkloadRefusal::FrameCodec)?;
        update(&mut hash, Counter::HashBundleStage, chunk);
        to.write_all(chunk)?;
        add_len(Counter::BundleStageWrite, count);
    }
    Ok(*hash.finalize().as_bytes())
}

/// Stage `bundle` privately and refuse it if it carries the drift marker.
///
/// # Errors
/// Refuses [`BulkloadRefusal::CaptureDrifted`] for a marked bundle, and
/// unreadable or malformed bundles.
pub fn stage_bundle(bundle: &Path) -> Result<StagedBundle> {
    let source = fs::canonicalize(bundle)?;
    let directory = PrivateDir::create(source.parent())?;
    let path = directory.path().join("capture.bundle");
    let digest = copy_hashing(&source, &path)?;
    let heads = text(
        git(directory.path())
            .args(["bundle", "list-heads"])
            .arg(&path),
    )?;
    if drift_marked(&heads) {
        return Err(BulkloadRefusal::CaptureDrifted);
    }
    #[cfg(test)]
    mid_pass::fire(&source, mid_pass::Stage::BundleChecked);
    Ok(StagedBundle {
        _directory: directory,
        bundle: path,
        digest,
    })
}

// A private repository holding the snapshot's refs and exactly the authority
// the snapshot read, before any worktree byte is captured.
fn carry_authority(
    repo: &Path,
    capture: &Path,
    inventory: &str,
    authority: &CarriedAuthority,
) -> Result<PathBuf> {
    let private = prepare_private(repo, capture)?;
    // prepare_private installs the frontier it read; it must be the carried one.
    if shallow::frontier(&private)? != authority.boundary {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    capture_refs(&private, inventory, &authority.stash)?;
    set_ref(&private, "refs/carry-export/head", &authority.head)?;
    capture_administration(&private, authority)?;
    Ok(private)
}

// The whole carried authority, re-read against the same inventory rule the
// snapshot used: a stash reflog rewritten under refs/stash, a symbolic HEAD or
// an exclude file moving under the pass all refuse.
fn authority_held(repo: &Path, carried: &CarriedAuthority, inventory: &str) -> Result<bool> {
    Ok(read_authority(repo, inventory)? == *carried)
}

/// Transient refs a retained capture is fetched under. Never carried on.
const REUSE_NAMESPACE: &str = "refs/carry-reuse/";

/// Blobs a retained capture of this same checkout already holds.
///
/// Only seats whose `StatIdentity` is unchanged since that capture qualify:
/// exactly the freshness tuple `RowSchema::stat_identity` documents, and only
/// when the retained row was not racy (see [`RACY_GRANULARITY_NS`]). A seat
/// that drifted in the retained pass is absent from its tree and is therefore
/// read again here, which is the whole point of the incremental pass (R-N28).
///
/// A retained capture that cannot be fetched or decoded degrades to no reuse:
/// the pass reads every seat, and the result says why. Whatever happened, every
/// `refs/carry-reuse/*` ref is deleted before this returns, so the bundle this
/// pass writes with `--all` can never carry them; if that cannot be proved,
/// this refuses.
fn reusable_blobs(
    private: &Path,
    retained: &RetainedCapture<'_>,
    seats: &[crate::RowSchema],
    now_ns: i128,
) -> Result<std::result::Result<raw_tree::Reuse, ReuseUnavailable>> {
    let attempt = retained_blobs(private, retained, seats, now_ns);
    clear_reuse_refs(private)?;
    Ok(attempt.map_err(|_| ReuseUnavailable::RetainedUnreadable))
}

// Delete every transient reuse ref itself (never what a symbolic one names),
// then prove none remains. Any failure is the private repository's ref
// inventory not being what the capture requires: GIT_INVENTORY_MALFORMED.
fn clear_reuse_refs(private: &Path) -> Result<()> {
    let listed = |private: &Path| -> Result<String> {
        text(
            git(private)
                .args(["for-each-ref", "--format=%(refname)"])
                .arg(REUSE_NAMESPACE),
        )
    };
    for name in listed(private)?.lines() {
        output(git(private).args(["update-ref", "--no-deref", "-d", name]))?;
    }
    if !listed(private)?.is_empty() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(())
}

fn retained_blobs(
    private: &Path,
    retained: &RetainedCapture<'_>,
    seats: &[crate::RowSchema],
    now_ns: i128,
) -> Result<raw_tree::Reuse> {
    use bulkload_proto::FileKind;
    use std::process::Stdio;
    let mut reuse = raw_tree::Reuse::new();
    // The fetch reads the whole retained bundle, whatever it then keeps.
    crate::counters::add(
        crate::counters::Counter::SourceCaptureReuseRead,
        fs::symlink_metadata(retained.bundle)?.len(),
    );
    if !git(private)
        .args(["fetch", "--no-tags", "--quiet"])
        .arg(retained.bundle)
        .args([
            "+refs/carry-export/worktree:refs/carry-reuse/worktree",
            "+refs/carry-export/filesystem-v1:refs/carry-reuse/filesystem-v1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success()
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let held: Vec<crate::RowSchema> = postcard::from_bytes(&output(
        git(private).args(["show", "refs/carry-reuse/filesystem-v1:value"]),
    )?)
    .map_err(|_| BulkloadRefusal::FrameCodec)?;
    let held: std::collections::BTreeMap<&[u8], &crate::RowSchema> = held
        .iter()
        .map(|row| (row.rel_path.as_slice(), row))
        .collect();
    let current: std::collections::BTreeMap<&[u8], &crate::RowSchema> = seats
        .iter()
        .map(|row| (row.rel_path.as_slice(), row))
        .collect();
    #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax, not interpolation.
    let entries =
        output(git(private).args(["ls-tree", "-r", "-z", "refs/carry-reuse/worktree^{tree}"]))?;
    for entry in entries.split(|byte| *byte == 0).filter(|e| !e.is_empty()) {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let (header, path) = entry.split_at(tab);
        let path = path
            .get(1..)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let header =
            std::str::from_utf8(header).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let mut fields = header.split(' ');
        let (Some(mode), Some("blob"), Some(object)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        if !oid(object) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let (Some(row), Some(before)) = (current.get(path), held.get(path)) else {
            continue;
        };
        if row.kind == FileKind::Regular
            && !racy(before, retained.started_ns, now_ns)
            && seat_equivalent(before, row)
            && raw_tree::mode_of(row)? == mode
        {
            reuse.insert(path.to_vec(), object.to_owned());
        }
    }
    Ok(reuse)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod mid_pass {
    //! Test-only injection points inside one export, keyed by the captured
    //! checkout so parallel tests never fire each other's hooks.
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Where in the export an armed hook runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Stage {
        /// Before the export takes its own ref and census snapshot: the window
        /// between a caller's pre-pass key and the export's snapshot.
        Snapshot,
        /// After the census and ref snapshot, before the byte pass.
        BytePass,
        /// After the export's last ref read, before it returns: the window
        /// between the export's snapshot and a caller's post-pass key.
        AfterPass,
        /// Fired by the estate after a capture record is written and before a
        /// stale drift record is retired.
        RecordWritten,
        /// Keyed by a bundle, not a checkout: after its drift check, before a
        /// restore reads it again.
        BundleChecked,
        /// After the index bytes are read, before they are validated.
        IndexRead,
    }

    type Hook = Box<dyn FnOnce() + Send>;
    static ARMED: Mutex<Vec<(PathBuf, Stage, Hook)>> = Mutex::new(Vec::new());

    pub fn arm(repo: &Path, hook: impl FnOnce() + Send + 'static) {
        arm_at(repo, Stage::BytePass, hook);
    }

    pub fn arm_at(repo: &Path, stage: Stage, hook: impl FnOnce() + Send + 'static) {
        let repo = std::fs::canonicalize(repo).expect("armed checkout exists");
        ARMED
            .lock()
            .expect("hooks")
            .push((repo, stage, Box::new(hook)));
    }

    pub fn fire(repo: &Path, stage: Stage) {
        let hook = {
            let mut armed = ARMED.lock().expect("hooks");
            armed
                .iter()
                .position(|(path, armed_stage, _)| path == repo && *armed_stage == stage)
                .map(|index| armed.remove(index).2)
        };
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// Directory names whose contents a capture omits, at any depth below a root.
///
/// Every entry is a build or tool output directory whose contents a standard
/// command regenerates from bytes the capture *does* carry, so omitting them
/// loses no state that cannot be rebuilt offline from the same checkout:
///
/// - `target` -- `cargo build` / `mvn package` output.
/// - `node_modules` -- `npm|pnpm|yarn install` output, pinned by the lockfile.
/// - `.venv`, `venv` -- Python virtual environments, rebuilt from the lockfile.
/// - `__pycache__` -- interpreter bytecode cache, rewritten on next import.
/// - `.direnv` -- direnv's layout/Nix profile cache, rebuilt by `direnv reload`.
/// - `.pytest_cache`, `.mypy_cache`, `.ruff_cache` -- tool caches, rebuilt on
///   the next run of the tool that wrote them.
/// - `.gradle` -- project-local Gradle build cache.
/// - `.next`, `.turbo`, `.parcel-cache`, `.swc` -- JavaScript bundler output
///   and build caches, rebuilt by the next build.
/// - `.terraform` -- provider plugins and module cache, rebuilt by
///   `terraform init`. Local *state* lives in `terraform.tfstate` beside it,
///   which is an ordinary carried file.
///
/// Deliberately absent: `build` and `dist` (too many repositories track them),
/// `.cargo/registry` (not a name, and a vendored registry may be the only
/// offline copy), and the `bazel-*` convenience symlinks -- the walk records a
/// symlink as one row and never descends it, so they already cost nothing and
/// omitting them would discard real state for no saving.
///
/// A name matches only when the seat is a real directory (not a symlink to
/// one) and Git tracks nothing beneath it; see [`CapturePolicy`].
pub const REBUILDABLE_DIRECTORIES: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".direnv",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".gradle",
    ".next",
    ".turbo",
    ".parcel-cache",
    ".swc",
    ".terraform",
];

/// Metadata ref naming the rebuildable roots a capture omitted.
const REBUILDABLE_METADATA: &str = "rebuildable-omissions-v1";
/// Reusable-key domain for the omission sidecar; only hashed when present.
const REBUILDABLE_DOMAIN: &[u8] = b"tcfs-git-rebuildable-omissions-v1\0";

/// What a capture carries beyond tracked content.
///
/// The default omits [`REBUILDABLE_DIRECTORIES`]; untracked and ignored files
/// everywhere else are still carried, exactly as before. `include_rebuildable`
/// restores full fidelity and reproduces the pre-omission capture key byte for
/// byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CapturePolicy {
    /// Carry the rebuildable set instead of recording it as custody.
    pub include_rebuildable: bool,
}

impl CapturePolicy {
    /// The full-fidelity policy: carry everything, omit nothing.
    #[must_use]
    pub const fn including_rebuildable() -> Self {
        Self {
            include_rebuildable: true,
        }
    }
}

/// One rebuildable root a capture omitted, with the size it did not carry.
///
/// Sizes are a single stat pass taken after the seats census. Entries that
/// vanish during that pass are simply not counted: a build rewriting its own
/// output is the expected condition, and custody evidence must not refuse.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RebuildableOmission {
    /// Omitted root relative to the captured checkout, as raw OS bytes.
    pub rel_path: Vec<u8>,
    /// Apparent bytes of regular files below the root, at measurement time.
    pub bytes: u64,
    /// Seats below the root, at measurement time.
    pub entries: u64,
}

/// What a capture may exclude, may reuse and must not carry.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExportOptions<'a> {
    /// A shared base bundle whose commit closure this capture may exclude.
    pub prerequisite: Option<&'a Path>,
    /// Which rebuildable roots become custody instead of seats.
    pub policy: CapturePolicy,
    /// A retained capture of this same checkout whose blobs may be reused.
    ///
    /// Every seat whose `StatIdentity` is unchanged since that capture, and
    /// was not racy when it was taken, is emitted by object name instead of
    /// being opened: the R25 never-rewalk clause, measured by
    /// [`Export::bytes_read`].
    pub reuse: Option<RetainedCapture<'a>>,
    /// Canonical sources of nested repositories planned as their own estate
    /// items (R-N114). Such a nest keeps its custody row and every refusal,
    /// but its seats belong to its own item, never to this capture.
    pub planned: &'a [PathBuf],
    /// A retained capture bundle of this checkout whose source-held tips
    /// become this bundle's prerequisites, so only what is new since it is
    /// packed (WP2, see [`chain`]). Ignored when `prerequisite` is set. The
    /// caller owns the chain's custody and depth bound.
    pub chain: Option<&'a Path>,
}

/// A retained capture offered for blob reuse, with the instant its pass began.
#[derive(Debug, Clone, Copy)]
pub struct RetainedCapture<'a> {
    /// The retained, verified capture bundle.
    pub bundle: &'a Path,
    /// [`Export::started_ns`] of the pass that wrote `bundle`.
    pub started_ns: i128,
}

/// Timestamp granularity the racy-seat guard allows for, in nanoseconds.
///
/// Two seconds covers the coarsest filesystem a checkout may sit on (FAT and
/// SMB at 2 s; HFS+, ext3 and many NFS servers at 1 s) and the jiffy-coarse
/// clock Linux stamps inodes with. The cost of erring wide is one re-read of a
/// seat written in the two seconds before a pass.
pub const RACY_GRANULARITY_NS: i128 = 2_000_000_000;

// Racy, exactly as Git defines it for its index: a seat stamped at or after
// the capture pass start, less one timestamp tick, can be rewritten at the same
// size in that same tick without its StatIdentity moving. Its identity
// therefore cannot vouch for the captured bytes, and it is never reused by
// identity; the next pass reads it again. A seat stamped later than this
// pass's own clock reading comes from a clock this pass cannot order against,
// and fails closed as racy too.
pub(crate) const fn racy(row: &crate::RowSchema, started_ns: i128, now_ns: i128) -> bool {
    let window = started_ns.saturating_sub(RACY_GRANULARITY_NS);
    row.mtime_ns >= window
        || row.ctime_ns >= window
        || row.mtime_ns > now_ns
        || row.ctime_ns > now_ns
}

/// Why a pass that was offered a retained capture read every seat anyway.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReuseUnavailable {
    /// The checkout is shallow: its bundle is shallow-graph custody, which
    /// carries no worktree tree to reuse blobs from.
    Shallow,
    /// The retained capture could not be fetched or decoded. Its transient
    /// refs were deleted and the pass read every seat.
    RetainedUnreadable,
    /// The retained capture predates the recorded pass start, so no seat in it
    /// can be proved free of racy timestamps.
    PassStartUnrecorded,
    /// A seat is stamped later than this pass's own clock, so it is racy on
    /// every pass and the whole capture is never reused until the clock passes
    /// its stamp. Other seats may still be reused one by one.
    FutureStamp,
}

impl ReuseUnavailable {
    /// The receipt value, as in `reuse_unavailable=shallow`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Shallow => "shallow",
            Self::RetainedUnreadable => "retained-unreadable",
            Self::PassStartUnrecorded => "pass-start-unrecorded",
            Self::FutureStamp => "future-stamp",
        }
    }
}

/// A completed export: the bundle, the custody for what it did not carry, what
/// raced it, and what it had to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The written and verified capture bundle.
    pub bundle: PathBuf,
    /// Rebuildable roots omitted from the capture, with measured sizes.
    pub omitted: Vec<RebuildableOmission>,
    /// Foreign nested repositories and gitlinks whose content is not carried.
    pub nested_repositories: Vec<NestedRepository>,
    /// Refs and seats that changed under the pass. Empty is the ordinary case.
    pub drift: CaptureDrift,
    /// Bytes streamed from source file descriptors during this pass.
    pub bytes_read: u64,
    /// Wall-clock nanoseconds since the epoch, taken before the census. A
    /// retained capture's seats stamped within [`RACY_GRANULARITY_NS`] of this
    /// instant are racy and never reused by identity.
    pub started_ns: i128,
    /// Set when a retained capture was offered and no blob could be reused.
    pub reuse_unavailable: Option<ReuseUnavailable>,
    /// The ref inventory this export snapshotted and carried.
    pub refs_before: String,
    /// The ref inventory this export read last, after its byte pass.
    pub refs_after: String,
    /// The Git authority this export read once and carried.
    pub authority: CarriedAuthority,
    /// What packing the bundle cost (WP2): its pack's bytes and objects, and
    /// the packing child's storage reads.
    pub pack: shared::PackStats,
    /// Whether the bundle declares a retained capture's source-held tips as
    /// prerequisites ([`ExportOptions::chain`]). A restore must then supply
    /// that capture's chain ([`chain::flatten`]).
    pub chained: bool,
}

/// One metadata census of a checkout: typed seats plus custody for what the
/// capture does not carry (nested worktrees, omitted rebuildable roots).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Census {
    rows: Vec<crate::RowSchema>,
    /// Registered worktrees of the same repository nested below the root;
    /// their HEADs are part of the census, their bytes are their own item's.
    nested_worktrees: Vec<NestedWorktree>,
    /// Foreign repositories nested below the root (directory nests only; the
    /// index contributes gitlinks separately). Their HEADs are part of the
    /// census, their bytes are their own item's.
    nested_repositories: Vec<NestedRepository>,
    /// Omitted roots, relative to the census root. Sizes are not part of the
    /// census: a rebuildable root's contents are exactly what must not be able
    /// to invalidate a capture in flight.
    omitted: Vec<Vec<u8>>,
}

/// Rebuildable roots a capture of `repo` would omit, with their measured sizes.
///
/// This is the custody the capture records instead of their bytes, available to
/// receipt and parity-audit tooling without performing a capture.
///
/// # Errors
/// Refuses any Git or filesystem state capture itself refuses.
pub fn rebuildable_omissions(repo: &Path) -> Result<Vec<RebuildableOmission>> {
    let repo = fs::canonicalize(repo)?;
    let common = common_repository(&repo)?;
    let census = capture_census(&repo, &common, CapturePolicy::default())?;
    measure_omissions(&repo, &census.omitted)
}

// Apparent size and seat count below one omitted root. A vanished entry is
// rebuildable churn, not a fault: skip it rather than refuse the capture.
fn measure_omissions(root: &Path, omitted: &[Vec<u8>]) -> Result<Vec<RebuildableOmission>> {
    use std::os::unix::ffi::OsStrExt;
    let mut measured = Vec::with_capacity(omitted.len());
    for rel_path in omitted {
        let start = safe_destination(root, Path::new(std::ffi::OsStr::from_bytes(rel_path)))?;
        let (mut bytes, mut entries) = (0u64, 0u64);
        let mut pending = vec![start];
        while let Some(directory) = pending.pop() {
            let listing = match fs::read_dir(&directory) {
                Ok(listing) => listing,
                Err(error) if vanished(&error) => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in listing {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) if vanished(&error) => continue,
                    Err(error) => return Err(error.into()),
                };
                let path = entry.path();
                let meta = match fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) if vanished(&error) => continue,
                    Err(error) => return Err(error.into()),
                };
                entries = entries.saturating_add(1);
                if meta.is_dir() {
                    pending.push(path);
                } else if meta.is_file() {
                    bytes = bytes.saturating_add(meta.len());
                }
            }
        }
        measured.push(RebuildableOmission {
            rel_path: rel_path.clone(),
            bytes,
            entries,
        });
    }
    Ok(measured)
}

fn vanished(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
}

// A capture-side census: rebuildable roots below `root` become custody instead
// of seats unless the policy asks for full fidelity. Attach and comparison
// paths keep the strict full census; they verify payload that is already here.
fn capture_census(root: &Path, common: &Path, policy: CapturePolicy) -> Result<Census> {
    capture_census_planned(root, common, policy, &[])
}

// R-N114: nests planned as their own items keep custody but carry no seats.
fn capture_census_planned(
    root: &Path,
    common: &Path,
    policy: CapturePolicy,
    planned: &[PathBuf],
) -> Result<Census> {
    // S4 (#162): a bare repository's root is its own administration. It has
    // no worktree, so no seat, nest or omission to census.
    if root == common && bare(root)? {
        return Ok(Census {
            rows: Vec::new(),
            nested_worktrees: Vec::new(),
            nested_repositories: Vec::new(),
            omitted: Vec::new(),
        });
    }
    filesystem_census(root, Some(common), policy, planned)
}

// Whether `repo` is a bare repository, by Git's own verdict: a non-bare
// `.git` directory given as a root is not one.
fn bare(repo: &Path) -> Result<bool> {
    Ok(text(git(repo).args(["rev-parse", "--is-bare-repository"]))? == "true")
}

// A name on the fixed list is only rebuildable if Git tracks nothing beneath
// it. A repository that really does track `target/...` keeps its bytes; the
// omission claim stays provable per repository instead of merely asserted.
fn rebuildable_root(root: &Path, relative: &Path, name: &std::ffi::OsStr) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    if !REBUILDABLE_DIRECTORIES
        .iter()
        .any(|candidate| name.as_bytes() == candidate.as_bytes())
    {
        return Ok(false);
    }
    let mut pathspec = std::ffi::OsString::from(":(top,literal)");
    pathspec.push(relative.as_os_str());
    Ok(output(git(root).args(["ls-files", "-z", "--"]).arg(pathspec))?.is_empty())
}

/// Metadata ref naming registered worktrees nested inside a captured checkout.
const NESTED_WORKTREES_METADATA: &str = "nested-worktrees-v1";
/// Reusable-key domain for the nested-worktree sidecar; only hashed when present.
const NESTED_WORKTREES_DOMAIN: &[u8] = b"tcfs-git-nested-worktrees-v1\0";
/// Largest `.git` gitdir-pointer file this census will read.
const GITDIR_POINTER_LIMIT: u64 = 64 * 1024;

/// Metadata ref naming the refs and seats that drifted under one capture pass.
const CAPTURE_DRIFT_METADATA: &str = "capture-drift-v1";
/// Largest drift list a single capture may report.
///
/// Unbounded drift is indistinguishable from a rebuild of the checkout and must
/// never be silently truncated into a capture that claims to be complete.
pub const DRIFT_ROW_LIMIT: usize = 65_536;

/// What changed under a capture pass that did not have to refuse.
///
/// Ref and seat drift are the two concurrency classes an agent lane produces:
/// a sibling worktree creating branches in the shared ref store, and a build
/// tool rewriting a file between the census and its byte pass. Neither is
/// corruption and neither is Git authority moving under the capture.
#[non_exhaustive]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum DriftKind {
    /// A ref absent from the pre-pass inventory appeared.
    RefAdded,
    /// A ref in the pre-pass inventory moved to another object.
    RefChanged,
    /// A ref in the pre-pass inventory was deleted.
    RefRemoved,
    /// A seat absent from the pre-pass census appeared.
    SeatAdded,
    /// A seat's identity changed under the pass; the capture does not vouch
    /// for its bytes.
    SeatChanged,
    /// A seat in the pre-pass census was removed.
    SeatRemoved,
    /// The source's object store was rewritten under the pass (a `gc`,
    /// `repack` or `prune` changed its pack listing) and a Git child reading
    /// through it failed: nothing was captured, and the next pass captures
    /// the rewritten store (WP1 PR 4, S5). Named `objects/pack`.
    ObjectStoreRewritten,
}

impl DriftKind {
    /// Whether this row names a filesystem seat rather than a ref.
    #[must_use]
    pub const fn is_seat(self) -> bool {
        matches!(
            self,
            Self::SeatAdded | Self::SeatChanged | Self::SeatRemoved
        )
    }

    // Whether the named seat was in the census yet its bytes are absent.
    const fn withdraws_payload(self) -> bool {
        matches!(self, Self::SeatChanged | Self::SeatRemoved)
    }
}

/// One drifted ref or seat.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct DriftRow {
    /// What kind of drift this is.
    pub kind: DriftKind,
    /// Refname for ref kinds, checkout-relative path for seat kinds, raw bytes.
    pub name: Vec<u8>,
}

impl DriftRow {
    pub(crate) fn seat(kind: DriftKind, name: &[u8]) -> Self {
        Self {
            kind,
            name: name.to_vec(),
        }
    }

    fn reference(kind: DriftKind, name: &str) -> Self {
        Self {
            kind,
            name: name.as_bytes().to_vec(),
        }
    }

    /// One receipt line. Debug-escaped: receipts must not carry raw newlines.
    #[must_use]
    #[allow(clippy::unnecessary_debug_formatting)] // Escaping is the point.
    pub fn display(&self) -> String {
        use std::os::unix::ffi::OsStrExt;
        format!(
            "{:?} {:?}",
            self.kind,
            Path::new(std::ffi::OsStr::from_bytes(&self.name))
        )
    }
}

/// Everything one capture pass observed changing under it.
///
/// An empty list is the ordinary case and is never carried, hashed or written:
/// a checkout nothing raced against produces exactly the bundle it produced
/// before drift tolerance existed. Drift is never part of the reusable key; it
/// describes a pass, not a repository state.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaptureDrift {
    /// Drifted refs and seats, sorted by (kind, name), deduplicated.
    pub rows: Vec<DriftRow>,
}

impl CaptureDrift {
    /// Whether this pass raced against nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many refs and seats drifted.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.len()
    }

    /// One receipt line per drifted ref and seat.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.rows.iter().map(DriftRow::display).collect()
    }

    /// Seats whose bytes this capture deliberately does not carry.
    ///
    /// Directories hold no bytes and are never withdrawn from the restored
    /// shape; a seat that appeared mid-pass was never in the census at all.
    #[must_use]
    pub fn withdrawn(&self, seats: &[crate::RowSchema]) -> Vec<Vec<u8>> {
        use bulkload_proto::FileKind;
        let directories: std::collections::BTreeSet<&[u8]> = seats
            .iter()
            .filter(|row| row.kind == FileKind::Directory)
            .map(|row| row.rel_path.as_slice())
            .collect();
        self.rows
            .iter()
            .filter(|row| {
                row.kind.withdraws_payload() && !directories.contains(row.name.as_slice())
            })
            .map(|row| row.name.clone())
            .collect()
    }

    fn extend(&mut self, rows: impl IntoIterator<Item = DriftRow>) {
        self.rows.extend(rows);
    }

    /// Fold in drift observed over a wider window than the export's own.
    ///
    /// # Errors
    /// Refuses drift larger than [`DRIFT_ROW_LIMIT`].
    pub(crate) fn merge(&mut self, other: Self) -> Result<()> {
        self.rows.extend(other.rows);
        self.seal()
    }

    // Deterministic order, exactly as the census sorts its seats, and a hard
    // budget: drift larger than the cap is a rebuild, not a captured pass.
    fn seal(&mut self) -> Result<()> {
        self.rows.sort_unstable();
        self.rows.dedup();
        if self.rows.len() > DRIFT_ROW_LIMIT {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        Ok(())
    }
}

// Keyed on refname: a lane creating branches in a shared ref store is drift,
// never a refusal. HEAD is not in `for-each-ref` and keeps its own comparison.
fn reference_drift(before: &str, after: &str) -> Result<Vec<DriftRow>> {
    let index = |inventory: &str| -> Result<std::collections::BTreeMap<String, String>> {
        inventory
            .lines()
            .map(|line| {
                line.split_once(' ')
                    .map(|(value, name)| (name.to_owned(), value.to_owned()))
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)
            })
            .collect()
    };
    let (before, after) = (index(before)?, index(after)?);
    let mut rows = Vec::new();
    for (name, value) in &before {
        match after.get(name) {
            None => rows.push(DriftRow::reference(DriftKind::RefRemoved, name)),
            Some(current) if current != value => {
                rows.push(DriftRow::reference(DriftKind::RefChanged, name));
            }
            Some(_) => (),
        }
    }
    for name in after.keys().filter(|name| !before.contains_key(*name)) {
        rows.push(DriftRow::reference(DriftKind::RefAdded, name));
    }
    Ok(rows)
}

// Directories carry no bytes and their timestamps move whenever any child is
// created or removed, so they drift only on kind, mode or link target. Regular
// files and symlinks drift on the full StatIdentity the freshness cache keys on.
fn seat_equivalent(a: &crate::RowSchema, b: &crate::RowSchema) -> bool {
    use bulkload_proto::FileKind;
    a.kind == b.kind
        && a.mode == b.mode
        && a.link_target == b.link_target
        && (a.kind == FileKind::Directory
            || (a.stat_identity() == b.stat_identity() && a.nlink == b.nlink))
}

fn seat_drift(before: &[crate::RowSchema], after: &[crate::RowSchema]) -> Vec<DriftRow> {
    seat_drift_by(before, after, seat_equivalent)
}

fn seat_drift_by(
    before: &[crate::RowSchema],
    after: &[crate::RowSchema],
    same: impl Fn(&crate::RowSchema, &crate::RowSchema) -> bool,
) -> Vec<DriftRow> {
    fn index(rows: &[crate::RowSchema]) -> std::collections::BTreeMap<&[u8], &crate::RowSchema> {
        rows.iter()
            .map(|row| (row.rel_path.as_slice(), row))
            .collect()
    }
    let (before, after) = (index(before), index(after));
    let mut rows = Vec::new();
    for (path, row) in &before {
        match after.get(path) {
            None => rows.push(DriftRow::seat(DriftKind::SeatRemoved, path)),
            Some(current) if !same(row, current) => {
                rows.push(DriftRow::seat(DriftKind::SeatChanged, path));
            }
            Some(_) => (),
        }
    }
    for path in after.keys().filter(|path| !before.contains_key(*path)) {
        rows.push(DriftRow::seat(DriftKind::SeatAdded, path));
    }
    rows
}

/// A registered linked worktree of the same repository nested inside the checkout.
///
/// Its subtree is not walked or carried by the enclosing capture: it is its own
/// estate item. This row is the enclosing capture's custody statement for it.
/// Claude Code's `<repo>/.claude/worktrees/<name>` convention is the common case.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NestedWorktree {
    /// Worktree root relative to the captured checkout, as raw OS bytes.
    pub rel_path: Vec<u8>,
    /// Name under the common `worktrees/` administration directory.
    pub worktree_name: String,
    /// HEAD of the nested worktree at census time.
    pub head_oid: String,
}

/// Registered worktrees of the same repository nested inside `repo`.
///
/// This is the custody the enclosing capture records instead of their bytes.
///
/// # Errors
/// Refuses malformed nested Git administration, exactly as capture does.
pub fn nested_worktrees(repo: &Path) -> Result<Vec<NestedWorktree>> {
    let repo = fs::canonicalize(repo)?;
    let common = common_repository(&repo)?;
    Ok(repository_census(&repo, &common)?.nested_worktrees)
}

/// Metadata ref naming foreign repositories and gitlinks nested in a capture.
const NESTED_REPOSITORIES_METADATA: &str = "nested-repositories-v1";
/// Reusable-key domain for the nested-repository sidecar; only hashed when present.
const NESTED_REPOSITORIES_DOMAIN: &[u8] = b"tcfs-git-nested-repositories-v1\0";

/// How a nested repository was found.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum NestedRepositoryKind {
    /// A directory under the worktree holding a `.git` of another repository.
    Directory,
    /// A mode `160000` index entry (a submodule), whether or not populated.
    Gitlink,
    /// A path HEAD's tree names as both a gitlink and a tree: Git's own index
    /// resolution collapses the gitlink. The capture carries the index as Git
    /// resolved it and names the collapse (R-N110).
    CollapsedGitlink,
}

/// What the nested `.git` is, for a [`NestedRepositoryKind::Directory`] nest.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum GitdirKind {
    /// `.git` is a directory: an independent repository (a vendored checkout,
    /// a tool's git cache, a fetched build dependency).
    Directory,
    /// `.git` is a `gitdir:` pointer file resolving to administration of a
    /// different repository (its linked worktree, or a populated submodule).
    PointerFile,
    /// Not applicable: the nest is a gitlink, found in the index.
    None,
}

/// A repository nested under the captured checkout that is NOT a registered
/// worktree of the same repository, or a gitlink entry in its index.
///
/// Its tracked content and history are never carried by the enclosing
/// capture, and the commit a gitlink names is not bundled (the gitlink entry
/// itself stays in the staged tree, exactly as indexed). A clean directory
/// nest's ignored files are carried as ordinary seats (R-N89); nothing else
/// below it is walked.
/// The nested repository must be its own estate item; this row is the
/// enclosing capture's custody statement for it (R-N32, R-N73).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct NestedRepository {
    /// Nest root relative to the captured checkout, as raw OS bytes.
    pub rel_path: Vec<u8>,
    /// Whether the nest was found on disk or in the index.
    pub kind: NestedRepositoryKind,
    /// HEAD of the nested repository at census time (a directory nest), or the
    /// commit the gitlink records. `None` for an unborn or missing HEAD.
    pub head_oid: Option<String>,
    /// What the on-disk `.git` is; [`GitdirKind::None`] for a gitlink.
    pub gitdir_kind: GitdirKind,
    /// The nest's resolved administration (its gitdir), canonical and
    /// absolute, as raw OS bytes. It may live outside the estate item (a
    /// gitfile), which is why the receipt names it (N6). Empty for a gitlink.
    pub admin: Vec<u8>,
    /// Commits reachable from HEAD or any ref (branches, tags, notes, ...)
    /// and from none of the nest's remote-tracking refs: work that exists
    /// only in this nest (R-N73, R-N83, N2). With no remote at all
    /// ([`Self::remotes`] false) that is every commit, reported as
    /// `unpushed=all(<count>)`. Always 0 for a gitlink.
    pub unpushed: u64,
    /// Whether the nest has any configured remote. Always false for a gitlink.
    pub remotes: bool,
    /// Ignored files and symlinks inside the nest that this capture carries
    /// as its own seats (R-N89), beside the checkout's own. Rebuildable roots
    /// inside the nest are omitted and recorded like the checkout's. Always 0
    /// for a gitlink.
    pub ignored_carried: u64,
    /// The nest is planned as its own estate item (R-N114): its seats are that
    /// item's, this capture carries none of them, and restore leaves its
    /// directory for that item to create. Always false for a gitlink.
    pub own_item: bool,
}

impl NestedRepository {
    /// One receipt line naming this nest.
    ///
    /// The path is byte-escaped (`\n`, `\"`, `\xNN`) inside quotes, so a
    /// newline or quote in a directory name can never forge a second line.
    #[must_use]
    pub fn receipt_line(&self) -> String {
        let path = self.rel_path.escape_ascii();
        let head = self.head_oid.as_deref().unwrap_or("unborn");
        match self.kind {
            NestedRepositoryKind::Gitlink => {
                format!("nested-repository path=\"{path}\" kind=Gitlink head={head}")
            }
            NestedRepositoryKind::CollapsedGitlink => {
                format!("gitlink-collapsed path=\"{path}\" head-gitlink={head} carried-as=index")
            }
            NestedRepositoryKind::Directory => {
                let unpushed = if self.remotes {
                    self.unpushed.to_string()
                } else {
                    format!("all({})", self.unpushed)
                };
                format!(
                    "nested-repository path=\"{path}\" kind=Directory gitdir={:?} admin=\"{}\" head={head} unpushed={unpushed} remotes={} ignored-carried={}{}",
                    self.gitdir_kind,
                    self.admin.escape_ascii(),
                    if self.remotes { "yes" } else { "none" },
                    self.ignored_carried,
                    if self.own_item { " seats=own-item" } else { "" },
                )
            }
        }
    }
}

/// Foreign repositories nested inside `repo`, and gitlinks in its index.
///
/// This is the custody the enclosing capture records instead of their content.
///
/// # Errors
/// Refuses malformed nested Git administration, exactly as capture does.
pub fn nested_repositories(repo: &Path) -> Result<Vec<NestedRepository>> {
    let repo = fs::canonicalize(repo)?;
    let common = common_repository(&repo)?;
    let census = repository_census(&repo, &common)?;
    let (_, _, gitlinks) = source_index(&repo)?;
    Ok(nested_custody(&census.nested_repositories, &gitlinks))
}

// Typed custody for what this capture names but does not carry, as in-bundle
// metadata refs written only when non-empty. Registered worktrees nested in
// the checkout are captured as their own estate items. Foreign repositories
// nested under it and gitlink index entries are not carried either: a nested
// repository (a vendored checkout, a tool cache, a build dependency, a
// populated submodule) is its own estate item, and a gitlink names a commit
// this bundle does not hold (the entry itself is in the staged tree).
// Directory nests hold still or the census comparison refuses; gitlinks hold
// still or the index does. Returns the nested-repository custody.
fn custody_metadata(
    private: &Path,
    census: &Census,
    authority: &CarriedAuthority,
) -> Result<Vec<NestedRepository>> {
    if !census.nested_worktrees.is_empty() {
        metadata(
            private,
            NESTED_WORKTREES_METADATA,
            &postcard::to_allocvec(&census.nested_worktrees)
                .map_err(|_| BulkloadRefusal::FrameCodec)?,
        )?;
    }
    let nested_repositories = nested_custody(&census.nested_repositories, &authority.gitlinks);
    nested_repositories_metadata(private, &nested_repositories)?;
    Ok(nested_repositories)
}

// The in-bundle custody ref, written only when there is custody to record, so
// a repository without nests keeps a byte-identical bundle.
fn nested_repositories_metadata(private: &Path, nested: &[NestedRepository]) -> Result<()> {
    if nested.is_empty() {
        return Ok(());
    }
    metadata(
        private,
        NESTED_REPOSITORIES_METADATA,
        &postcard::to_allocvec(nested).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )
}

// The sidecar both the key and the bundle carry: directory nests from the
// census and gitlinks from the index, in one sorted list. A populated
// submodule appears twice, once per kind, which is the truth of it.
fn nested_custody(
    directories: &[NestedRepository],
    gitlinks: &[NestedRepository],
) -> Vec<NestedRepository> {
    let mut custody = Vec::with_capacity(directories.len() + gitlinks.len());
    custody.extend_from_slice(directories);
    custody.extend_from_slice(gitlinks);
    custody.sort();
    custody
}

// HEAD of a nested repository, read through its own `.git` with the hardened
// invocation (no hooks, no fsmonitor, no global or system config; the nest's
// own config IS read, which rev-parse cannot turn into execution). An unborn
// HEAD is `None`;
// a `.git` Git cannot read at all is malformed inventory.
fn nested_head(directory: &Path) -> Result<Option<String>> {
    let result = git(directory)
        .args(["--git-dir=.git", "rev-parse", "--verify", "-q", "HEAD"])
        .output()?;
    if result.status.success() {
        let head = String::from_utf8(result.stdout)
            .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
            .trim_end()
            .to_owned();
        if !oid(&head) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        return Ok(Some(head));
    }
    if result.status.code() == Some(1) {
        return Ok(None);
    }
    Err(BulkloadRefusal::GitInventoryMalformed)
}

// A directory whose `.git` (a directory, or a pointer file that does not
// resolve into our `worktrees/`) may be a foreign repository. It is custody
// only when all of these hold; otherwise it is malformed inventory (R-N73):
//
// - F7: Git must be able to read it, and its common directory must not be
//   this repository's. Checked for both `.git` kinds, through `--git-dir` so
//   discovery can never walk up into the enclosing checkout.
// - B1: the enclosing repository tracks nothing under the nest path. A
//   gitlink at exactly the nest path (a populated submodule) is the one
//   permitted entry; any other tracked path under it would be dropped from
//   the capture, since nothing below a nest is walked.
// - B3: the nest is clean: `status --porcelain=v2 --untracked-files=all` is
//   empty, so its worktree is exactly its HEAD and nothing below it is lost
//   by not walking it. Its unpushed count is recorded with its HEAD.
// - R-N83 (a): the nest holds no stash (refs/stash absent, stash reflog
//   empty) and no commit reachable only from a detached HEAD. Each refuses
//   with its own typed refusal, since both are work the capture would lose.
// What a census knows while classifying nests: the checkout's common
// directory, its index (read lazily), the capture policy, and the nests
// planned as their own items (R-N114).
#[derive(Clone, Copy)]
struct NestScope<'a> {
    common: &'a Path,
    outer: &'a OuterIndex,
    policy: CapturePolicy,
    planned: &'a [PathBuf],
}

fn foreign_nest(
    root: &Path,
    directory: &Path,
    scope: NestScope<'_>,
    rel_path: Vec<u8>,
    gitdir_kind: GitdirKind,
) -> Result<(NestedRepository, NestSeats)> {
    let NestScope {
        common,
        outer,
        policy,
        planned,
    } = scope;
    let seen = text(git(directory).args([
        "--git-dir=.git",
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ]))?;
    let seen = fs::canonicalize(seen).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    if seen == common {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let admin = fs::canonicalize(text(git(directory).args([
        "--git-dir=.git",
        "rev-parse",
        "--absolute-git-dir",
    ]))?)
    .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    // N6: administration inside a rebuildable root the nest itself is not
    // under is administration the capture omits while naming the nest.
    if [&admin, &seen]
        .iter()
        .any(|admin| admin_in_rebuildable_root(root, &rel_path, admin))
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    if tracked_under(root, &rel_path, directory, outer)? {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let head_oid = nested_head(directory)?;
    if nest_has_stash(directory)? {
        return Err(BulkloadRefusal::GitNestStashed);
    }
    if nest_detached_unreachable(directory, head_oid.as_deref())? {
        return Err(BulkloadRefusal::GitNestDetachedUnreachable);
    }
    if nest_operation_in_progress(directory)? {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // N1: status trusts the index; an entry flagged assume-unchanged or
    // skip-worktree, a sparse checkout, or a split index can hide an edit.
    if nest_index_hides_changes(directory)? {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // R-N115: a populated submodule inside a nest is refused by name.
    if let Some(path) = nest_populated_submodule(directory)? {
        return Err(BulkloadRefusal::GitNestPopulatedSubmodule(
            [rel_path.as_slice(), b"/", &path].concat(),
        ));
    }
    // B1 (round 3): a built-in conversion is a clean filter with no command.
    if let Some(path) = nest_conversion_attribute(directory)? {
        return Err(BulkloadRefusal::GitNestConversionAttribute(
            [rel_path.as_slice(), b"/", &path].concat(),
        ));
    }
    // Ignored entries are listed too: they are carried seats, not dirt (R-N89).
    let status = output(nest_status(directory)?.args([
        "--git-dir=.git",
        "--work-tree=.",
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
        "--ignored=traditional",
        "--ignore-submodules=none",
    ]))?;
    let mut ignored = Vec::new();
    for entry in status.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        match entry.strip_prefix(b"! ") {
            Some(path) if !path.is_empty() => ignored.push(path.to_vec()),
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        }
    }
    // R-N114: a nest planned as its own item owns its seats; this capture
    // names it and carries none of them.
    let own_item = planned.iter().any(|item| item.as_path() == directory);
    let seats = if own_item {
        NestSeats {
            rows: Vec::new(),
            omitted: Vec::new(),
            carried: 0,
        }
    } else {
        nest_ignored_seats(root, directory, &rel_path, &ignored, policy)?
    };
    let (unpushed, remotes) = unpushed_commits(directory, head_oid.as_deref())?;
    Ok((
        NestedRepository {
            rel_path,
            kind: NestedRepositoryKind::Directory,
            head_oid,
            gitdir_kind,
            admin: std::os::unix::ffi::OsStrExt::as_bytes(admin.as_os_str()).to_vec(),
            unpushed,
            remotes,
            ignored_carried: seats.carried,
            own_item,
        },
        seats,
    ))
}

// The first directory on an ignored path inside a nest that is a rebuildable
// root: on the fixed list, and the nest tracks nothing beneath it. None when
// the policy carries the rebuildable set. Answers are cached per prefix.
fn nest_rebuildable_root(
    directory: &Path,
    parts: &[&[u8]],
    directories: usize,
    policy: CapturePolicy,
    cache: &mut std::collections::BTreeMap<Vec<u8>, bool>,
) -> Result<Option<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt;
    if policy.include_rebuildable {
        return Ok(None);
    }
    let mut prefix = Vec::new();
    for part in parts.iter().take(directories) {
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(part);
        if !REBUILDABLE_DIRECTORIES
            .iter()
            .any(|candidate| candidate.as_bytes() == *part)
        {
            continue;
        }
        let omit = if let Some(omit) = cache.get(&prefix) {
            *omit
        } else {
            let mut pathspec = std::ffi::OsString::from(":(top,literal)");
            pathspec.push(std::ffi::OsStr::from_bytes(&prefix));
            let omit = output(
                git(directory)
                    .args(["--git-dir=.git", "ls-files", "-z", "--"])
                    .arg(pathspec),
            )?
            .is_empty();
            cache.insert(prefix.clone(), omit);
            omit
        };
        if omit {
            return Ok(Some(prefix));
        }
    }
    Ok(None)
}

// A clean nest's ignored content, as this capture's own seats (R-N89).
struct NestSeats {
    // Rows relative to the enclosing checkout: the nest root and every
    // directory leading to a carried file, then the file or symlink itself.
    rows: Vec<crate::RowSchema>,
    // Rebuildable roots inside the nest, omitted exactly as the checkout's own.
    omitted: Vec<Vec<u8>>,
    // Carried files and symlinks.
    carried: u64,
}

// R-N89: every file `status --ignored` lists in a clean nest is carried the
// way the enclosing capture carries its own ignored files: as census rows,
// which raw_tree reads with its fstat sandwich and O_NOFOLLOW, and which
// restore writes back under the nest path. A path under a rebuildable root
// inside the nest (a directory on the fixed list the nest tracks nothing
// beneath) is omitted and recorded instead, unless the policy carries the
// rebuildable set. An ignored nested repository (a `dir/` entry) outside such
// a root refuses GIT_NEST_INNER_REPOSITORY naming it (R-N111); a `.git` or
// dot-dot component, or a listed path lstat disagrees with, refuses too:
// nothing listed is dropped silently.
fn nest_ignored_seats(
    root: &Path,
    directory: &Path,
    nest: &[u8],
    ignored: &[Vec<u8>],
    policy: CapturePolicy,
) -> Result<NestSeats> {
    use bulkload_proto::FileKind;
    use std::os::unix::ffi::OsStrExt;
    let mut rows = std::collections::BTreeMap::<Vec<u8>, crate::RowSchema>::new();
    let mut omitted = std::collections::BTreeSet::new();
    let mut rebuildable = std::collections::BTreeMap::<Vec<u8>, bool>::new();
    let mut carried = 0u64;
    let joined = |relative: &[u8]| -> Vec<u8> {
        let mut path = nest.to_vec();
        if !relative.is_empty() {
            path.push(b'/');
            path.extend_from_slice(relative);
        }
        path
    };
    let seat = |relative: &[u8],
                rows: &mut std::collections::BTreeMap<Vec<u8>, crate::RowSchema>|
     -> Result<FileKind> {
        let outer = joined(relative);
        if let Some(row) = rows.get(&outer) {
            return Ok(row.kind);
        }
        let path = root.join(std::ffi::OsStr::from_bytes(&outer));
        let row = seat_row(&path, &outer, &fs::symlink_metadata(&path)?)?;
        let kind = row.kind;
        rows.insert(outer, row);
        Ok(kind)
    };
    for entry in ignored {
        let (path, is_directory) = entry
            .strip_suffix(b"/")
            .map_or((entry.as_slice(), false), |path| (path, true));
        let parts: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
        if parts.iter().any(|part| {
            part.is_empty() || *part == b"." || *part == b".." || part.eq_ignore_ascii_case(b".git")
        }) {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let directories = if is_directory {
            parts.len()
        } else {
            parts.len().saturating_sub(1)
        };
        if let Some(root) =
            nest_rebuildable_root(directory, &parts, directories, policy, &mut rebuildable)?
        {
            omitted.insert(joined(&root));
            continue;
        }
        if is_directory {
            // R-N111: an ignored repository inside the nest is named, escaped
            // and typed. Any other `dir/` entry (Git lists one only when it
            // does not descend) is still refused, generically.
            let inner = directory
                .join(std::ffi::OsStr::from_bytes(path))
                .join(".git");
            return Err(match fs::symlink_metadata(inner) {
                Ok(_) => BulkloadRefusal::GitNestInnerRepository(joined(path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    BulkloadRefusal::GitInventoryMalformed
                }
                Err(error) => error.into(),
            });
        }
        // The nest root and each directory on the way are seats too, so
        // restore recreates them with their captured modes.
        let mut prefix = Vec::new();
        if seat(&prefix, &mut rows)? != FileKind::Directory {
            return Err(BulkloadRefusal::GitAuthorityChanged);
        }
        for part in parts.iter().take(directories) {
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(part);
            if seat(&prefix, &mut rows)? != FileKind::Directory {
                return Err(BulkloadRefusal::GitAuthorityChanged);
            }
        }
        if seat(path, &mut rows)? == FileKind::Directory {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        carried = carried
            .checked_add(1)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
    }
    Ok(NestSeats {
        rows: rows.into_values().collect(),
        omitted: omitted.into_iter().collect(),
        carried,
    })
}

// N1 (R-N73, R-N83): the refusal `source_index` applies to the enclosing
// repository, applied to a nest before its status is believed. Any non-zero
// index entry flag (assume-unchanged, skip-worktree, intent-to-add), a sparse
// checkout, or a split index means status may not see every change.
fn nest_index_hides_changes(directory: &Path) -> Result<bool> {
    let debug = output(git(directory).args(["--git-dir=.git", "ls-files", "--debug"]))?;
    // #131: classified as the enclosing repository's entries are, so the
    // fsmonitor validity bit is no flag here either.
    let flagged: Vec<Result<EntryFlags>> = debug
        .split(|b| *b == b'\n')
        .filter_map(|line| {
            line.windows(8)
                .position(|window| window == b"\tflags: ")
                .and_then(|at| line.get(at + 8..))
        })
        .map(|flags| {
            std::str::from_utf8(flags)
                .map_err(|_| BulkloadRefusal::GitInventoryMalformed)
                .and_then(entry_flags)
        })
        .filter(|flags| *flags != Ok(EntryFlags::Plain))
        .collect();
    if !flagged.is_empty() {
        // #106: a nest's index is not carried, so its intent-to-add entries
        // cannot be; say so by cause rather than as a malformed inventory.
        if flagged
            .iter()
            .all(|flags| *flags == Ok(EntryFlags::IntentToAdd))
        {
            return Err(BulkloadRefusal::GitInventoryIntentToAdd);
        }
        return Ok(true);
    }
    let sparse = git(directory)
        .args(["--git-dir=.git", "config", "--bool", "core.sparseCheckout"])
        .output()?;
    match sparse.status.code() {
        Some(0) if sparse.stdout.trim_ascii() != b"false" => return Ok(true),
        Some(0 | 1) => {}
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    }
    Ok(
        !output(git(directory).args(["--git-dir=.git", "rev-parse", "--shared-index-path"]))?
            .trim_ascii()
            .is_empty(),
    )
}

// R-N115 (round 4 N1): the first gitlink in a nest's index whose path holds
// anything but an empty directory: a populated submodule, a repository, files
// left under an unpopulated gitlink, or a file or symlink at the path. Git's
// status never descends a gitlink path, so nothing there is dirt, ignored or
// a seat: it would be carried by nobody. Only absence or an empty directory
// (what `git clone` without --recurse-submodules leaves) passes.
fn nest_populated_submodule(directory: &Path) -> Result<Option<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt;
    let staged = output(git(directory).args(["--git-dir=.git", "ls-files", "-z", "--stage"]))?;
    for entry in staged
        .split(|b| *b == 0)
        .filter(|entry| entry.starts_with(b"160000 "))
    {
        let path = entry
            .iter()
            .position(|b| *b == b'\t')
            .and_then(|tab| entry.get(tab + 1..))
            .filter(|path| !path.is_empty())
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let seat = directory.join(std::ffi::OsStr::from_bytes(path));
        match fs::symlink_metadata(&seat) {
            Ok(meta) if meta.is_dir() => {
                // R5-5: a directory that cannot be listed cannot be proved
                // empty; it refuses by the same typed code, never as Io(13).
                match fs::read_dir(&seat) {
                    Ok(mut entries) => {
                        if entries.next().is_some() {
                            return Ok(Some(path.to_vec()));
                        }
                    }
                    Err(_) => return Ok(Some(path.to_vec())),
                }
            }
            Ok(_) => return Ok(Some(path.to_vec())),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

// B1 (round 3, R-N73): the first tracked path in a nest whose attributes
// (in-tree .gitattributes, info/attributes, the attributes file) ask for a
// built-in conversion between worktree and index: `ident`, a
// `working-tree-encoding`, or `text`/`eol`/`crlf` set to anything but unset.
// Each makes status compare converted bytes, so an edit can hide inside the
// conversion (a same-size edit inside `$Id$`, a re-encode, a CRLF rewrite).
fn nest_conversion_attribute(directory: &Path) -> Result<Option<Vec<u8>>> {
    let tracked =
        output(git(directory).args(["--git-dir=.git", "--work-tree=.", "ls-files", "-z"]))?;
    if tracked.is_empty() {
        return Ok(None);
    }
    let attributes = input(
        git(directory).args([
            "--git-dir=.git",
            "--work-tree=.",
            "check-attr",
            "-z",
            "--stdin",
            "ident",
            "working-tree-encoding",
            "text",
            "eol",
            "crlf",
        ]),
        &tracked,
    )?;
    let mut fields = attributes.split(|b| *b == 0);
    while let (Some(path), Some(attribute), Some(info)) =
        (fields.next(), fields.next(), fields.next())
    {
        if path.is_empty() {
            break;
        }
        let converts = match attribute {
            b"ident" => info == b"set",
            b"working-tree-encoding" | b"text" | b"eol" | b"crlf" => {
                info != b"unspecified" && info != b"unset"
            }
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        };
        if converts {
            return Ok(Some(path.to_vec()));
        }
    }
    Ok(None)
}

/// How deep populated submodules inside a nest are followed for filter
/// neutralisation before the nest is refused as malformed.
const NEST_SUBMODULE_DEPTH: usize = 8;

// R-N83 / N4: a nest is foreign, so its configuration is hostile input, and
// `status` is the one command the census runs there that can execute
// configured programs. The hardened `git()` already pins core.fsmonitor=false,
// core.hooksPath=/dev/null and --no-optional-locks (no index or untracked-cache
// write). A nest whose config (or that of any populated submodule status
// would recurse into) sets a filter clean, smudge or process command is
// refused before status runs: such a filter would both execute nest-chosen
// code and decide what "clean" means (a lying clean filter hides an edit).
// As defence in depth against a config written between that check and
// status, every filter driver seen is also neutralised: an empty command runs
// nothing and required=false keeps it from failing the read. The overrides
// travel as GIT_CONFIG_COUNT command-scope config, which outranks the nest's
// own files and which Git passes down to the submodule statuses it spawns;
// env keys also avoid any `-c key=value` parsing ambiguity in a hostile name.
fn nest_status(directory: &Path) -> Result<Command> {
    use std::os::unix::ffi::OsStrExt;
    let mut drivers = std::collections::BTreeSet::new();
    if filter_drivers(directory, 0, &mut drivers)? {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // B1 (round 3): the nest's own config must not weaken what status
    // compares. Stat checks at full strength, modes and symlinks compared,
    // no line-ending conversion.
    let mut overrides: Vec<(std::ffi::OsString, &str)> = vec![
        ("core.fsmonitor".into(), "false"),
        ("core.untrackedCache".into(), "false"),
        ("core.trustctime".into(), "true"),
        ("core.checkStat".into(), "default"),
        ("core.fileMode".into(), "true"),
        ("core.ignoreStat".into(), "false"),
        ("core.autocrlf".into(), "false"),
        ("core.symlinks".into(), "true"),
        // Round 4 N2: a repository created on case-insensitive APFS keeps
        // core.ignoreCase=true when moved to a case-sensitive volume, and
        // status then drops an untracked file whose name case-folds to a
        // tracked one. False fails safe on a case-insensitive volume: at
        // worst a case difference shows as dirt and the nest refuses.
        ("core.ignoreCase".into(), "false"),
    ];
    for name in &drivers {
        for (variable, value) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            let mut key = std::ffi::OsString::from("filter.");
            key.push(std::ffi::OsStr::from_bytes(name));
            key.push(".");
            key.push(variable);
            overrides.push((key, value));
        }
    }
    let mut command = git(directory);
    command.env("GIT_CONFIG_COUNT", overrides.len().to_string());
    for (index, (key, value)) in overrides.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    Ok(command)
}

// Every `filter.<name>.*` subsection visible to the repository at `directory`
// (includes followed, as status would), and recursively to each populated
// submodule in its index. Returns whether any of them sets a command.
fn filter_drivers(
    directory: &Path,
    depth: usize,
    drivers: &mut std::collections::BTreeSet<Vec<u8>>,
) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    if depth > NEST_SUBMODULE_DEPTH {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let listed = git(directory)
        .args([
            "--git-dir=.git",
            "config",
            "-z",
            "--get-regexp",
            r"^filter\.",
        ])
        .output()?;
    match listed.status.code() {
        Some(0 | 1) => {}
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    }
    let mut commands = false;
    for entry in listed
        .stdout
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
    {
        let key = entry.split(|b| *b == b'\n').next().unwrap_or_default();
        let (name, variable) = key
            .strip_prefix(b"filter.")
            .and_then(|rest| {
                let dot = rest.iter().rposition(|b| *b == b'.')?;
                Some((rest.get(..dot)?, rest.get(dot + 1..)?))
            })
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        commands |= matches!(variable, b"clean" | b"smudge" | b"process");
        drivers.insert(name.to_vec());
    }
    let staged = output(git(directory).args(["--git-dir=.git", "ls-files", "-z", "--stage"]))?;
    for entry in staged
        .split(|b| *b == 0)
        .filter(|entry| entry.starts_with(b"160000 "))
    {
        let path = entry
            .iter()
            .position(|b| *b == b'\t')
            .and_then(|tab| entry.get(tab + 1..))
            .map(|path| Path::new(std::ffi::OsStr::from_bytes(path)))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let submodule = directory.join(path);
        match fs::symlink_metadata(submodule.join(".git")) {
            Ok(_) => commands |= filter_drivers(&submodule, depth + 1, drivers)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(commands)
}

// N2: commits reachable from HEAD or any ref and from none of the nest's
// remote-tracking refs, and whether it has any remote at all. With no remote
// nothing is known to exist elsewhere, so the count is every commit (the
// receipt says `unpushed=all(<count>)`, N6).
fn unpushed_commits(directory: &Path, head: Option<&str>) -> Result<(u64, bool)> {
    let remotes = !output(git(directory).args(["--git-dir=.git", "remote"]))?.is_empty();
    let mut command = git(directory);
    command.args(["--git-dir=.git", "rev-list", "--count"]);
    command.args(head);
    command.arg("--all");
    if remotes {
        command.args(["--not", "--remotes"]);
    }
    Ok((count(&mut command)?, remotes))
}

// N2: a merge, cherry-pick, revert, rebase, sequencer or bisect in progress
// is unfinished work that status does not report as dirt.
fn nest_operation_in_progress(directory: &Path) -> Result<bool> {
    const MARKERS: [&str; 8] = [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_START",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ];
    // D2: one rev-parse resolves every marker (one path per line, in order);
    // a count that disagrees (a newline in the administration path) is
    // malformed inventory rather than a guess.
    let mut command = git(directory);
    command.args(["--git-dir=.git", "rev-parse", "--path-format=absolute"]);
    for marker in MARKERS {
        command.args(["--git-path", marker]);
    }
    let resolved = text(&mut command)?;
    let paths: Vec<&str> = resolved.lines().collect();
    if paths.len() != MARKERS.len() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    for path in paths {
        match fs::symlink_metadata(path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

// N6: whether `admin` sits under the enclosing checkout inside a directory on
// the rebuildable list that the nest itself is not inside. Such a root is
// omitted (or rebuilt) wholesale, so the nest's administration would not
// survive while the capture names the nest as custody.
fn admin_in_rebuildable_root(root: &Path, nest: &[u8], admin: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(relative) = admin.strip_prefix(root) else {
        return false;
    };
    let nest = Path::new(std::ffi::OsStr::from_bytes(nest));
    let mut prefix = PathBuf::new();
    for component in relative.components() {
        prefix.push(component);
        let name = component.as_os_str().as_bytes();
        if REBUILDABLE_DIRECTORIES
            .iter()
            .any(|candidate| candidate.as_bytes() == name)
            && !nest.starts_with(&prefix)
        {
            return true;
        }
    }
    false
}

fn count(command: &mut Command) -> Result<u64> {
    text(command)?
        .parse::<u64>()
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)
}

// R-N83 (a): `refs/stash` present, or a stash reflog with entries. The reflog
// is read through Git's own path resolution (a linked worktree's lives in its
// common directory); a backend that reports a reflog without a file (reftable)
// is taken at its word.
fn nest_has_stash(directory: &Path) -> Result<bool> {
    if !output(git(directory).args([
        "--git-dir=.git",
        "for-each-ref",
        "--format=%(refname)",
        "refs/stash",
    ]))?
    .is_empty()
    {
        return Ok(true);
    }
    let log = text(git(directory).args([
        "--git-dir=.git",
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "logs/refs/stash",
    ]))?;
    match fs::symlink_metadata(&log) {
        Ok(meta) => return Ok(!meta.is_file() || meta.len() > 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let exists = git(directory)
        .args(["--git-dir=.git", "reflog", "exists", "refs/stash"])
        .status()?;
    match exists.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(BulkloadRefusal::GitInventoryMalformed),
    }
}

// R-N83 (a): a detached HEAD whose commit no local branch, tag or
// remote-tracking ref reaches. A detached HEAD Git cannot resolve is
// malformed inventory, never an unborn one.
fn nest_detached_unreachable(directory: &Path, head: Option<&str>) -> Result<bool> {
    let symbolic = git(directory)
        .args(["--git-dir=.git", "symbolic-ref", "-q", "HEAD"])
        .output()?;
    match symbolic.status.code() {
        Some(0) => return Ok(false),
        Some(1) => {}
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    }
    let head = head.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    Ok(count(git(directory).args([
        "--git-dir=.git",
        "rev-list",
        "--count",
        head,
        "--not",
        "--branches",
        "--tags",
        "--remotes",
    ]))? > 0)
}

// The enclosing repository's index, read once per census and only if a nest
// is found: (is a gitlink, path) per entry.
#[derive(Default)]
struct OuterIndex(std::cell::OnceCell<Vec<(bool, Vec<u8>)>>);

impl OuterIndex {
    fn entries(&self, root: &Path) -> Result<&[(bool, Vec<u8>)]> {
        if let Some(entries) = self.0.get() {
            return Ok(entries);
        }
        let listed = output(git(root).args(["ls-files", "-z", "--stage"]))?;
        let mut entries = Vec::new();
        for entry in listed.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
            let path = entry
                .iter()
                .position(|b| *b == b'\t')
                .and_then(|tab| entry.get(tab + 1..))
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            entries.push((entry.starts_with(b"160000 "), path.to_vec()));
        }
        Ok(self.0.get_or_init(|| entries))
    }
}

// Whether the enclosing repository's index holds any path at or below the
// nest other than a single gitlink at exactly the nest (B1). First the same
// literal, top-anchored pathspec as `rebuildable_root`; then, because that
// pathspec is byte-exact and a filesystem may fold case (APFS, NTFS) or
// Unicode normalisation (APFS), every index path whose leading components
// could name the nest under such folding (same bytes ignoring ASCII case, or
// any non-ASCII byte) is resolved through the filesystem itself and compared
// by (dev, inode) with the nest directory (N3). The filesystem is the
// authority on which directory a tracked path lands in.
fn tracked_under(root: &Path, rel: &[u8], directory: &Path, outer: &OuterIndex) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let mut pathspec = std::ffi::OsString::from(":(top,literal)");
    pathspec.push(std::ffi::OsStr::from_bytes(rel));
    let entries = output(
        git(root)
            .args(["ls-files", "-z", "--stage", "--"])
            .arg(pathspec),
    )?;
    let literal = entries
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
                return true;
            };
            !(entry.starts_with(b"160000 ") && entry.get(tab + 1..) == Some(rel))
        });
    if literal {
        return Ok(true);
    }
    let nest = fs::symlink_metadata(directory)?;
    let depth = rel.split(|b| *b == b'/').count();
    let mut resolved = std::collections::BTreeMap::<&[u8], bool>::new();
    for (gitlink, path) in outer.entries(root)? {
        let components = path.split(|b| *b == b'/').count();
        if components < depth {
            continue;
        }
        let end = path
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'/')
            .nth(depth - 1)
            .map_or(path.len(), |(at, _)| at);
        let Some(prefix) = path.get(..end) else {
            continue;
        };
        if prefix == rel
            || !(prefix.eq_ignore_ascii_case(rel) || !prefix.is_ascii() || !rel.is_ascii())
        {
            continue;
        }
        let same = if let Some(same) = resolved.get(prefix) {
            *same
        } else {
            let same = match fs::symlink_metadata(root.join(std::ffi::OsStr::from_bytes(prefix))) {
                Ok(meta) => meta.dev() == nest.dev() && meta.ino() == nest.ino(),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    false
                }
                Err(error) => return Err(error.into()),
            };
            resolved.insert(prefix, same);
            same
        };
        if same && !(*gitlink && components == depth) {
            return Ok(true);
        }
    }
    Ok(false)
}

// One directory below the root whose `.git` classified as custody.
enum NestedCustody {
    Worktree(NestedWorktree),
    Repository(NestedRepository, NestSeats),
}

// Reuse the transport's typed filesystem seats instead of treating Git's
// executable-bit-only tree modes as complete filesystem metadata. .git is
// administration owned by Git-native capture; on these attach and comparison
// paths (no custody) any nested .git refuses: restored payload never has one.
fn filesystem_rows(root: &Path) -> Result<Vec<crate::RowSchema>> {
    Ok(filesystem_census(root, None, CapturePolicy::including_rebuildable(), &[])?.rows)
}

// A checkout census: registered worktrees of `common` and foreign repositories
// nested below `root` are recorded as custody and not descended; a malformed
// nested .git still refuses. Full fidelity: nothing rebuildable is omitted.
fn repository_census(root: &Path, common: &Path) -> Result<Census> {
    filesystem_census(
        root,
        Some(common),
        CapturePolicy::including_rebuildable(),
        &[],
    )
}

// Classify a directory below the root that contains an entry named .git.
// A `.git` directory is a foreign nested repository. A regular gitdir-pointer
// file resolving to a worktree administration directory of `common`,
// registered back to this checkout, is a nested worktree of the same
// repository; a pointer resolving to administration of a different repository
// is a foreign nested repository. A symlink, an unreadable `.git`, an
// unresolvable pointer, or a pointer into `common` that is not a registered
// worktree keeps the refusal: that is malformed inventory, not custody.
fn nested_administration(
    root: &Path,
    directory: &Path,
    scope: NestScope<'_>,
) -> Result<Option<NestedCustody>> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let common = scope.common;
    let pointer = directory.join(".git");
    let meta = match fs::symlink_metadata(&pointer) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let rel_path = || -> Result<Vec<u8>> {
        Ok(directory
            .strip_prefix(root)
            .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
            .as_os_str()
            .as_bytes()
            .to_vec())
    };
    if meta.is_dir() {
        let (custody, seats) =
            foreign_nest(root, directory, scope, rel_path()?, GitdirKind::Directory)?;
        return Ok(Some(NestedCustody::Repository(custody, seats)));
    }
    if !meta.is_file() || meta.len() > GITDIR_POINTER_LIMIT {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut contents = String::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&pointer)?
        .take(GITDIR_POINTER_LIMIT)
        .read_to_string(&mut contents)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let target = contents
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("gitdir: "))
        .map(str::trim_end)
        .filter(|value| !value.is_empty())
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    // Any unresolvable pointer is malformed inventory, never a transient IO fault.
    let admin = fs::canonicalize(directory.join(target))
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    if !admin.is_dir() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let worktrees = common.join("worktrees");
    if admin.parent() != Some(worktrees.as_path()) {
        // Not our administration at all: a worktree of another repository, or
        // a populated submodule (`<common>/modules/<name>`).
        let (custody, seats) =
            foreign_nest(root, directory, scope, rel_path()?, GitdirKind::PointerFile)?;
        return Ok(Some(NestedCustody::Repository(custody, seats)));
    }
    let worktree_name = admin
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?
        .to_owned();
    // Registration is the administration's back-pointer to this very checkout.
    let registered = fs::read_to_string(admin.join("gitdir"))
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    if fs::canonicalize(registered.trim_end()).ok() != Some(fs::canonicalize(&pointer)?) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // Git itself must agree that this is a linked worktree of the same repository.
    let seen =
        text(git(directory).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]))?;
    if fs::canonicalize(seen).ok().as_deref() != Some(common) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let head_oid = text(git(directory).args(["rev-parse", "--verify", "HEAD"]))?;
    if !oid(&head_oid) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(Some(NestedCustody::Worktree(NestedWorktree {
        rel_path: rel_path()?,
        worktree_name,
        head_oid,
    })))
}

// One typed filesystem seat from its lstat metadata. Sockets, FIFOs and
// devices are not seats a capture can carry, and refuse.
fn seat_row(path: &Path, relative: &[u8], meta: &fs::Metadata) -> Result<crate::RowSchema> {
    use bulkload_proto::FileKind;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let identity = crate::freshness::StatIdentity::from_metadata(meta);
    let kind = if meta.is_dir() {
        FileKind::Directory
    } else if meta.is_file() {
        FileKind::Regular
    } else if meta.is_symlink() {
        FileKind::Symlink
    } else {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    };
    Ok(crate::RowSchema {
        rel_path: relative.to_vec(),
        kind,
        dev: identity.dev,
        ino: identity.ino,
        size: identity.size,
        mtime_ns: identity.mtime_ns,
        ctime_ns: identity.ctime_ns,
        mode: meta.mode(),
        nlink: meta.nlink(),
        link_target: if kind == FileKind::Symlink {
            Some(fs::read_link(path)?.as_os_str().as_bytes().to_vec())
        } else {
            None
        },
        blake3: None,
    })
}

fn filesystem_census(
    root: &Path,
    common: Option<&Path>,
    policy: CapturePolicy,
    planned: &[PathBuf],
) -> Result<Census> {
    use bulkload_proto::FileKind;
    use std::os::unix::ffi::OsStrExt;
    crate::counters::bump(crate::counters::Counter::CensusWalks);
    let mut pending = vec![root.to_path_buf()];
    let mut rows = Vec::new();
    let mut nested_worktrees = Vec::new();
    let mut nested_repositories = Vec::new();
    let outer = OuterIndex::default();
    let mut omitted = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                if directory == root {
                    continue;
                }
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| BulkloadRefusal::PathEscapesRoot)?;
            let row = seat_row(&path, relative.as_os_str().as_bytes(), &meta)?;
            if row.kind == FileKind::Directory {
                // A registered nested worktree or a foreign nested repository is
                // custody, not seats: no row for its root, no descent, no
                // contents. Its own item carries them.
                match common
                    .map(|common| {
                        nested_administration(
                            root,
                            &path,
                            NestScope {
                                common,
                                outer: &outer,
                                policy,
                                planned,
                            },
                        )
                    })
                    .transpose()?
                    .flatten()
                {
                    Some(NestedCustody::Worktree(custody)) => {
                        nested_worktrees.push(custody);
                        continue;
                    }
                    Some(NestedCustody::Repository(custody, seats)) => {
                        // R-N89: the nest's ignored files are this capture's
                        // seats, exactly like the checkout's own ignored files.
                        nested_repositories.push(custody);
                        rows.extend(seats.rows);
                        omitted.extend(seats.omitted);
                        continue;
                    }
                    None => {}
                }
                // A rebuildable root is custody, not seats: no row for the root
                // itself, no descent, no contents. `cargo build` rebuilds it.
                if !policy.include_rebuildable
                    && rebuildable_root(root, relative, &entry.file_name())?
                {
                    omitted.push(relative.as_os_str().as_bytes().to_vec());
                    continue;
                }
                pending.push(path.clone());
            }
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    nested_worktrees.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    nested_repositories.sort();
    omitted.sort();
    Ok(Census {
        rows,
        nested_worktrees,
        nested_repositories,
        omitted,
    })
}

fn restore_filesystem_rows(destination: &Path, revision: &str) -> Result<()> {
    use bulkload_proto::FileKind;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let bytes = output(git(destination).args(["show", &format!("{revision}:value")]))?;
    let rows: Vec<crate::RowSchema> =
        postcard::from_bytes(&bytes).map_err(|_| BulkloadRefusal::FrameCodec)?;
    for row in &rows {
        let path = safe_destination(
            destination,
            Path::new(std::ffi::OsStr::from_bytes(&row.rel_path)),
        )?;
        match row.kind {
            FileKind::Directory => match fs::create_dir(&path) {
                Ok(()) => (),
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && fs::symlink_metadata(&path)?.is_dir() => {}
                Err(error) => return Err(error.into()),
            },
            FileKind::Regular if fs::symlink_metadata(&path)?.is_file() => {
                // Open before applying a potentially unreadable captured mode.
                let file = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)?;
                if !file.metadata()?.is_file() {
                    return Err(BulkloadRefusal::GitInventoryMalformed);
                }
                file.set_permissions(fs::Permissions::from_mode(row.mode & 0o777))?;
                file.sync_file_counted()?;
            }
            FileKind::Symlink if fs::symlink_metadata(&path)?.is_symlink() => {
                if row.link_target.as_deref() != Some(fs::read_link(&path)?.as_os_str().as_bytes())
                {
                    return Err(BulkloadRefusal::GitInventoryMalformed);
                }
            }
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        }
    }
    // Parents become readonly only after all descendants have materialized.
    for row in rows
        .iter()
        .rev()
        .filter(|row| row.kind == FileKind::Directory)
    {
        let path = safe_destination(
            destination,
            Path::new(std::ffi::OsStr::from_bytes(&row.rel_path)),
        )?;
        // Flush descendants before their parent, retaining an open descriptor
        // across modes such as 000 so durability does not require reopening it.
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)?;
        directory.set_permissions(fs::Permissions::from_mode(row.mode & 0o777))?;
        directory.sync_dir_counted()?;
    }
    // These flush payload entry creation, not the separate Git administration
    // or receipt transactions, which retain their own durability boundaries.
    for path in [
        destination,
        destination
            .parent()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    ] {
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)?
            .sync_dir_counted()?;
    }
    Ok(())
}

fn safe_destination(root: &Path, relative: &Path) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    if relative.as_os_str().is_empty() || relative.components().any(|part| !matches!(part, Component::Normal(name) if !name.as_bytes().eq_ignore_ascii_case(b".git"))) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let mut path = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(part) = components.next() {
        path.push(part);
        if components.peek().is_some() && !fs::symlink_metadata(&path)?.is_dir() {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
    }
    Ok(path)
}

/// Metadata ref carrying a capture's intent-to-add index entries (#106).
const INTENT_TO_ADD_METADATA: &str = "intent-to-add-v1";

/// `ls-files --debug` flags of an intent-to-add entry and nothing else:
/// `CE_INTENT_TO_ADD | CE_EXTENDED`.
const INTENT_TO_ADD_FLAGS: u32 = 0x2000_4000;

/// `CE_FSMONITOR_VALID` (#131): Git's in-memory mark that fsmonitor last
/// reported the entry unchanged. It is cache validity, not index state: it
/// changes nothing a capture carries, and a restore builds a fresh index
/// with no fsmonitor extension, so it is masked before an entry is
/// classified. The hardened `git()` pins `core.fsmonitor=false`, under
/// which Git does not set it, so this only guards a Git that still does.
const CE_FSMONITOR_VALID: u32 = 0x0020_0000;

/// What an index entry's `ls-files --debug` flags make it (#106, #131).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryFlags {
    /// No state flag: an ordinary entry.
    Plain,
    /// `git add -N`, and nothing else.
    IntentToAdd,
}

// Classify one entry's flags, hex as `ls-files --debug` prints them, less
// the fsmonitor validity bit. Anything else (assume-unchanged,
// skip-worktree, a stage or a flag this code does not know) refuses
// GIT_INVENTORY_MALFORMED: status may not see every change under it.
fn entry_flags(flags: &str) -> Result<EntryFlags> {
    let flags = u32::from_str_radix(flags, 16)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        & !CE_FSMONITOR_VALID;
    match flags {
        0 => Ok(EntryFlags::Plain),
        INTENT_TO_ADD_FLAGS => Ok(EntryFlags::IntentToAdd),
        _ => Err(BulkloadRefusal::GitInventoryMalformed),
    }
}

/// One intent-to-add (`git add -N`) index entry, carried as custody (#106).
///
/// Git stores such an entry with the empty blob and omits it from
/// `write-tree`, so the staged tree cannot hold it; the worktree tree holds
/// its seat's bytes, and restore re-marks the path intent-to-add.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct IntentToAdd {
    /// Path relative to the checkout root, raw bytes.
    pub rel_path: Vec<u8>,
    /// The index entry's mode (`100644`, `100755` or `120000`).
    pub mode: u32,
    /// The entry names the empty blob, as every Git since 2.x writes it.
    pub empty_blob: bool,
}

const EMPTY_BLOB_SHA1: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
const EMPTY_BLOB_SHA256: &str = "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";

// Every intent-to-add entry of the index file `index`, read against `repo`.
// Any other non-zero entry flag (assume-unchanged, skip-worktree) refuses
// GIT_INVENTORY_MALFORMED: status may not see every change under it.
// `ls-files --debug` and `ls-files --stage` list the same entries in the same
// order; a count that differs refuses rather than pair them wrongly.
fn intent_to_add_entries(repo: &Path, index: &Path) -> Result<Vec<IntentToAdd>> {
    let listed = |args: &[&str]| output(git(repo).env("GIT_INDEX_FILE", index).args(args));
    let debug = String::from_utf8(listed(&["ls-files", "--debug"])?)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let flags = debug
        .lines()
        .filter_map(|line| line.split_once("\tflags: ").map(|(_, flags)| flags))
        .map(entry_flags)
        .collect::<Result<Vec<EntryFlags>>>()?;
    if flags.iter().all(|flags| *flags == EntryFlags::Plain) {
        return Ok(Vec::new());
    }
    let stage = listed(&["ls-files", "--stage", "-z"])?;
    let entries: Vec<&[u8]> = stage
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .collect();
    if entries.len() != flags.len() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut carried = Vec::new();
    for (entry, flags) in entries.into_iter().zip(flags) {
        if flags != EntryFlags::IntentToAdd {
            continue;
        }
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let header = std::str::from_utf8(
            entry
                .get(..tab)
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
        )
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let mut fields = header.split(' ');
        let mode = fields
            .next()
            .and_then(|mode| u32::from_str_radix(mode, 8).ok())
            .filter(|mode| matches!(mode, 0o100_644 | 0o100_755 | 0o120_000))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let value = fields
            .next()
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if fields.next() != Some("0") || fields.next().is_some() {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let rel_path = entry
            .get(tab + 1..)
            .filter(|path| !path.is_empty())
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        carried.push(IntentToAdd {
            rel_path: rel_path.to_vec(),
            mode,
            empty_blob: value == EMPTY_BLOB_SHA1 || value == EMPTY_BLOB_SHA256,
        });
    }
    Ok(carried)
}

// The index mode `git add` gives a new entry for a seat with this row:
// a symlink is 120000, a regular file 100755 when its owner may execute it
// and 100644 otherwise. `None` for any other kind.
const fn index_mode_of(row: &crate::RowSchema) -> Option<u32> {
    match row.kind {
        bulkload_proto::FileKind::Symlink => Some(0o120_000),
        bulkload_proto::FileKind::Regular if row.mode & 0o100 != 0 => Some(0o100_755),
        bulkload_proto::FileKind::Regular => Some(0o100_644),
        _ => None,
    }
}

// The capture's intent-to-add custody, recorded only when there is any, so
// every other capture's bundle is unchanged. Each entry's seat must be a
// carried file or symlink: Git can mark only an existing path intent-to-add,
// so an entry whose seat is gone (`git add -N`, then `rm`) refuses by cause.
//
// #131: the seat's mode must also be the one the entry records. Restore
// re-marks the path with `git add -N`, which takes the mode from the
// restored seat, so an entry whose seat changed mode after it was marked
// (`git add -N f; chmod +x f`: the index keeps 100644, the seat is 0755)
// cannot be restored as captured. It refuses here, at capture, before any
// destination is written, rather than after a restore laid one down.
fn record_intent_to_add(
    private: &Path,
    repo: &Path,
    index: &Path,
    seats: &[&crate::RowSchema],
) -> Result<()> {
    let entries = intent_to_add_entries(repo, index)?;
    if entries.is_empty() {
        return Ok(());
    }
    for entry in &entries {
        let restorable = seats
            .iter()
            .any(|row| row.rel_path == entry.rel_path && index_mode_of(row) == Some(entry.mode));
        if !restorable {
            return Err(BulkloadRefusal::GitInventoryIntentToAdd);
        }
    }
    metadata(
        private,
        INTENT_TO_ADD_METADATA,
        &postcard::to_allocvec(&entries).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )
}

// The intent-to-add custody a capture's bundle names; empty for a capture
// with none (and for every capture taken before #106).
fn carried_intent_to_add(repo: &Path, heads: &str) -> Result<Vec<IntentToAdd>> {
    let name = format!("refs/carry-export/{INTENT_TO_ADD_METADATA}");
    let Some(value) = heads.lines().find_map(|line| {
        line.split_once(' ')
            .filter(|(_, reference)| *reference == name)
            .map(|(value, _)| value)
    }) else {
        return Ok(Vec::new());
    };
    if !oid(value) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    postcard::from_bytes(&output(
        git(repo).args(["show", &format!("{value}:value")]),
    )?)
    .map_err(|_| BulkloadRefusal::FrameCodec)
}

// #131: every carried intent-to-add entry must name a file or symlink the
// worktree tree (`ls-tree -r -z` output) restores at the entry's own mode,
// since restore re-marks it with `git add -N`, which reads the mode from the
// restored seat. Checked before a worktree byte is laid down, so a capture
// taken before the capture-side check refuses here, by cause, not after
// its destination is written.
fn intent_to_add_restorable(custody: &[IntentToAdd], worktree: &[u8]) -> Result<()> {
    if custody.is_empty() {
        return Ok(());
    }
    let mut modes = std::collections::BTreeMap::new();
    for entry in worktree
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let mode = entry
            .get(..tab)
            .and_then(|header| header.split(|b| *b == b' ').next())
            .and_then(|mode| std::str::from_utf8(mode).ok())
            .and_then(|mode| u32::from_str_radix(mode, 8).ok())
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let path = entry
            .get(tab + 1..)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        modes.insert(path, mode);
    }
    if custody
        .iter()
        .all(|entry| modes.get(entry.rel_path.as_slice()) == Some(&entry.mode))
    {
        Ok(())
    } else {
        Err(BulkloadRefusal::GitInventoryIntentToAdd)
    }
}

// Re-mark the captured intent-to-add paths in the index `index` (the
// repository's own when `None`) of the checkout at `repo`, after its staged
// tree is read and its worktree laid down (#106). Literal pathspecs, NUL
// separated, so no path is a glob or an option; `-f` because an ignored path
// can be intent-to-add. The result is read back: every path must come back
// intent-to-add at its captured mode, or the restore refuses by cause.
fn restore_intent_to_add(repo: &Path, index: Option<&Path>, heads: &str) -> Result<()> {
    let entries = carried_intent_to_add(repo, heads)?;
    if entries.is_empty() {
        return Ok(());
    }
    let command = || {
        let mut command = git(repo);
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        command.env("GIT_LITERAL_PATHSPECS", "1");
        command
    };
    let mut paths = Vec::new();
    for entry in &entries {
        if entry.rel_path.contains(&0) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        paths.extend_from_slice(&entry.rel_path);
        paths.push(0);
    }
    input(
        command().args([
            "add",
            "--intent-to-add",
            "--force",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ]),
        &paths,
    )
    .map_err(|_| BulkloadRefusal::GitInventoryIntentToAdd)?;
    let index_path = match index {
        Some(index) => index.to_owned(),
        None => PathBuf::from(text(git(repo).args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index",
        ]))?),
    };
    let mut restored = intent_to_add_entries(repo, &index_path)?;
    let mut expected: Vec<(Vec<u8>, u32)> = entries
        .into_iter()
        .map(|entry| (entry.rel_path, entry.mode))
        .collect();
    expected.sort();
    restored.sort();
    if restored
        .into_iter()
        .map(|entry| (entry.rel_path, entry.mode))
        .ne(expected)
    {
        return Err(BulkloadRefusal::GitInventoryIntentToAdd);
    }
    Ok(())
}

// The source index path, its exact bytes, and every gitlink it holds as
// custody. A gitlink names a submodule commit this repository's objects need
// not contain; the entry stays in the staged tree (Git bundles and restores a
// gitlink to an absent commit as is), the commit is never carried, and the
// submodule's own content is its own estate item (R-N73, B2).
fn source_index(repo: &Path) -> Result<(PathBuf, Vec<u8>, Vec<NestedRepository>)> {
    let index_path = PathBuf::from(text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "index",
    ]))?);
    let before_index = match fs::read(&index_path) {
        Ok(bytes) => bytes,
        // S4 (#162): a bare repository has no index and no worktree for one
        // to describe. It carries none: its staged and worktree trees are
        // empty, and its refs, HEAD and administration are its custody.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && bare(repo)? => {
            return Ok((index_path, Vec::new(), Vec::new()));
        }
        Err(error) => return Err(error.into()),
    };
    #[cfg(test)]
    mid_pass::fire(&fs::canonicalize(repo)?, mid_pass::Stage::IndexRead);
    // Validate exactly the bytes that are carried, never the live index a
    // moment later (round-4 R2): every check reads a private copy of them.
    let scratch = PrivateDir::create(None)?;
    let carried = scratch.path().join("index");
    fs::write(&carried, &before_index)?;
    let checked = |args: &[&str]| -> Result<Vec<u8>> {
        output(git(repo).env("GIT_INDEX_FILE", &carried).args(args))
    };
    // #106: intent-to-add entries are carried as index custody; any other
    // entry flag still refuses.
    intent_to_add_entries(repo, &carried)?;
    if !checked(&["rev-parse", "--shared-index-path"])?
        .trim_ascii()
        .is_empty()
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // Gitlinks are read from the carried copy too (round-4 R2): the custody
    // names exactly the entries the bundle's staged tree holds (R-N73).
    let entries = checked(&["ls-files", "--stage", "-z"])?;
    let mut gitlinks = Vec::new();
    for entry in entries
        .split(|b| *b == 0)
        .filter(|entry| entry.starts_with(b"160000 "))
    {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let header = std::str::from_utf8(
            entry
                .get(..tab)
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
        )
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let mut fields = header.split_whitespace().skip(1);
        let value = fields
            .next()
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if fields.next() != Some("0") || fields.next().is_some() {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let rel_path = entry
            .get(tab + 1..)
            .filter(|path| !path.is_empty())
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        gitlinks.push(NestedRepository {
            rel_path: rel_path.to_vec(),
            kind: NestedRepositoryKind::Gitlink,
            head_oid: Some(value.to_owned()),
            gitdir_kind: GitdirKind::None,
            admin: Vec::new(),
            unpushed: 0,
            remotes: false,
            ignored_carried: 0,
            own_item: false,
        });
    }
    collapsed_gitlinks(repo, &mut gitlinks)?;
    gitlinks.sort();
    Ok((index_path, before_index, gitlinks))
}

// R-N110: HEAD's own tree names a path as both a gitlink and a tree (a
// duplicate entry Git's tree reader tolerates), so Git's index resolution of
// HEAD collapses the gitlink. The index is carried exactly as Git resolved it;
// this names the collapse on every receipt. An unborn HEAD has nothing to
// compare. Nothing is recorded for an ordinary HEAD, so it keeps its exact key.
fn collapsed_gitlinks(repo: &Path, gitlinks: &mut Vec<NestedRepository>) -> Result<()> {
    let head = git(repo)
        .args(["rev-parse", "--verify", "-q", "HEAD^{tree}"])
        .output()?;
    match head.status.code() {
        Some(0) => {}
        Some(1) => return Ok(()),
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    }
    let listed = output(git(repo).args(["ls-tree", "-r", "-z", "--full-tree", "HEAD"]))?;
    let mut entries = Vec::new();
    for entry in listed.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let header = std::str::from_utf8(
            entry
                .get(..tab)
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
        )
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let path = entry
            .get(tab + 1..)
            .filter(|path| !path.is_empty())
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        entries.push((header, path));
    }
    for (header, rel_path) in &entries {
        if !header.starts_with("160000 ") {
            continue;
        }
        let beneath = entries.iter().any(|(_, path)| {
            path.len() > rel_path.len()
                && path.starts_with(rel_path)
                && path.get(rel_path.len()) == Some(&b'/')
        });
        if !beneath {
            continue;
        }
        let value = header
            .split_whitespace()
            .nth(2)
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        gitlinks.push(NestedRepository {
            rel_path: rel_path.to_vec(),
            kind: NestedRepositoryKind::CollapsedGitlink,
            head_oid: Some(value.to_owned()),
            gitdir_kind: GitdirKind::None,
            admin: Vec::new(),
            unpushed: 0,
            remotes: false,
            ignored_carried: 0,
            own_item: false,
        });
    }
    Ok(())
}

fn prepare_private(repo: &Path, capture: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let format = text(git(repo).args(["rev-parse", "--show-object-format"]))?;
    if !matches!(format.as_str(), "sha1" | "sha256") {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let private = capture.join("repository.git");
    output(
        git(capture)
            .args([
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
            ])
            .arg(&private),
    )?;
    // S2 (#162): writers write only the write store, which borrows nothing;
    // readers see it, then the source's store, through `alternates`. The
    // write store's entry is relative to `objects`.
    fs::DirBuilder::new()
        .mode(0o700)
        .create(private.join(git_env::WRITE_STORE))?;
    let objects = text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ]))?;
    if objects.contains(['\n', '\r']) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    fs::write(
        private.join("objects/info/alternates"),
        format!("../{}\n{objects}\n", git_env::WRITE_STORE),
    )?;
    let boundary = shallow::frontier(repo)?;
    if !boundary.is_empty() {
        fs::write(private.join("shallow"), boundary)?;
    }
    Ok(private)
}

/// Import into a content-addressed source namespace, never native branches.
///
/// Snapshot identity includes native refs and capture metadata, not carried
/// transit refs. Already canonical refs retain their original source and
/// identity across bidirectional rounds, bounding growth by distinct captures.
///
/// # Errors
/// Refuses invalid source names, non-export bundle refs, collisions or invalid
/// bundles. A failed fetch may leave unreachable objects, never changed HEAD.
pub fn import_bundle(repo: &Path, bundle: &Path, source: &str) -> Result<usize> {
    import_staged(repo, &stage_bundle(bundle)?, source)
}

/// [`import_bundle`] from a bundle this process already staged and checked.
///
/// # Errors
/// As [`import_bundle`].
pub fn import_staged(repo: &Path, staged: &StagedBundle, source: &str) -> Result<usize> {
    import_verified(repo, staged.path(), source)
}

// `import_bundle` for a verb that already staged and checked its bundle, and
// reads only the staged copy.
pub(super) fn import_verified(repo: &Path, bundle: &Path, source: &str) -> Result<usize> {
    if !source_slug(source) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let bundle = fs::canonicalize(bundle)?;
    verify_bundle(repo, &bundle)?;
    let heads = text(git(repo).args(["bundle", "list-heads"]).arg(&bundle))?;
    let unpacked = shallow::unpack(repo, &bundle, &heads)?;
    let heads = unpacked.as_ref().unwrap_or(&heads);
    let mut native: Vec<_> = heads
        .lines()
        .filter(|line| {
            !line
                .split_once(' ')
                .is_some_and(|(_, name)| name.starts_with("refs/carry-export/union/v1/"))
        })
        .collect();
    native.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tcfs-git-native-snapshot-v1\0");
    for line in native {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize().to_hex();
    let mut names = Vec::new();
    for line in heads.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let suffix = name
            .strip_prefix("refs/carry-export/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let target = if let Some(tail) = suffix.strip_prefix("union/v1/") {
            if !canonical_tail(tail) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            format!("refs/carry/v1/{tail}")
        } else {
            format!("refs/carry/v1/{source}/{digest}/{suffix}")
        };
        names.push((value.to_owned(), name.to_owned(), target));
    }
    // Fetch objects only. Compare-and-create below cannot clobber a native ref.
    if unpacked.is_none() {
        output(
            git(repo)
                .args([
                    "fetch",
                    "--no-write-fetch-head",
                    "--no-auto-maintenance",
                    "--no-tags",
                    "--no-recurse-submodules",
                ])
                .arg(&bundle)
                .args(names.iter().map(|(_, name, _)| name)),
        )?;
    }
    let inventory = text(git(repo).args([
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        "refs/carry/v1/",
    ]))?;
    let mut existing = std::collections::BTreeMap::new();
    for line in inventory.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) || existing.insert(name, value).is_some() {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    let mut desired = std::collections::BTreeMap::new();
    for (value, _, target) in &names {
        if target.contains('\0')
            || desired
                .insert(target.as_str(), value.as_str())
                .is_some_and(|prior| prior != value.as_str())
        {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    let mut transaction = Vec::new();
    for (target, value) in desired {
        let operation = match existing.get(target) {
            Some(prior) if *prior == value => "verify",
            Some(_) => return Err(BulkloadRefusal::GitDestinationOccupied),
            None => "create",
        };
        transaction.extend_from_slice(format!("{operation} {target}\0{value}\0").as_bytes());
    }
    // One atomic compare/create transaction, including unchanged refs: a
    // concurrent update or deletion refuses instead of publishing a partial set.
    if !transaction.is_empty() {
        input(
            git(repo).args(["update-ref", "--no-deref", "--stdin", "-z"]),
            &transaction,
        )?;
    }
    Ok(names.len())
}

fn capture_revision(heads: &str, suffix: &str) -> Result<String> {
    heads
        .lines()
        .find_map(|line| {
            line.split_once(' ')
                .filter(|(_, name)| *name == format!("refs/carry-export/{suffix}"))
        })
        .map(|(value, _)| value.to_owned())
        .filter(|value| oid(value))
        .ok_or(BulkloadRefusal::GitInventoryMalformed)
}

fn bundle_object_format(heads: &str) -> Result<&'static str> {
    let value = heads
        .lines()
        .next()
        .and_then(|line| line.split_once(' '))
        .map(|(value, _)| value)
        .filter(|value| oid(value))
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    Ok(if value.len() == 40 { "sha1" } else { "sha256" })
}

fn payload_shape_equal(a: &[crate::RowSchema], b: &[crate::RowSchema]) -> bool {
    use bulkload_proto::FileKind;
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.rel_path == b.rel_path
                && a.kind == b.kind
                && a.link_target == b.link_target
                && (a.kind != FileKind::Regular || a.size == b.size)
                && (a.kind == FileKind::Symlink || a.mode & 0o777 == b.mode & 0o777)
        })
}

fn sync_private_tree(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_private_tree(&entry.path())?;
        } else if entry.file_type()?.is_file() {
            fs::File::open(entry.path())?.sync_file_counted()?;
        } else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    fs::File::open(root)?.sync_dir_counted()?;
    Ok(())
}

fn prepare_attachment(
    bundle: &Path,
    destination: &Path,
    source: &str,
    receipt: &Path,
) -> Result<(PathBuf, String)> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(receipt)?;
    // Retain the actual input, not merely a pathname that may later disappear.
    let retained = receipt.join("capture.bundle");
    fs::copy(bundle, &retained)?;
    let heads = text(git(receipt).args(["bundle", "list-heads"]).arg(&retained))?;
    let format = bundle_object_format(&heads)?;
    let private = receipt.join("repository.git");
    output(
        git(receipt)
            .args([
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
            ])
            .arg(&private),
    )?;
    import_verified(&private, &retained, source)?;
    let heads = shallow::headers(&private, &retained)?;
    let head = capture_revision(&heads, "head")?;
    let symbolic = text(git(&private).args([
        "show",
        &format!("{}:value", capture_revision(&heads, "head-symbolic")?),
    ]))?;
    if symbolic.is_empty() {
        output(git(&private).args(["update-ref", "--no-deref", "HEAD", &head]))?;
    } else {
        if !symbolic.starts_with("refs/heads/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        output(git(&private).args(["check-ref-format", &symbolic]))?;
        set_ref(&private, &symbolic, &head)?;
        output(git(&private).args(["symbolic-ref", "HEAD", &symbolic]))?;
    }
    let exclude = output(git(&private).args([
        "show",
        &format!("{}:value", capture_revision(&heads, "exclude")?),
    ]))?;
    fs::create_dir_all(private.join("info"))?;
    fs::write(private.join("info/exclude"), exclude)?;
    output(
        snapshot_command(&private, destination, &private.join("index")).args([
            "read-tree",
            &format!("{}^{{tree}}", capture_revision(&heads, "staged")?),
        ]),
    )?;
    output(git(&private).args(["config", "core.bare", "false"]))?;
    output(
        git(&private)
            .args(["config", "core.worktree"])
            .arg(destination),
    )?;
    Ok((private, heads))
}

fn source_configuration(repo: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for name in ["config", "config.worktree"] {
        let path =
            text(git(repo).args(["rev-parse", "--path-format=absolute", "--git-path", name]))?;
        match fs::read(path) {
            Ok(bytes) => files.push((name.to_owned(), bytes)),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && name == "config.worktree" => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(files)
}

fn safe_configuration_value(private: &Path, key: &str, value: &str) -> bool {
    if value.contains(['\0', '\n', '\r']) {
        return false;
    }
    if matches!(key, "core.filemode" | "core.ignorecase" | "core.symlinks") {
        return matches!(value, "true" | "false");
    }
    if matches!(key, "user.name" | "user.email") {
        return true;
    }
    if key == "push.default" {
        return matches!(
            value,
            "nothing" | "current" | "upstream" | "simple" | "matching"
        );
    }
    if key == "pull.ff" {
        return matches!(value, "true" | "false" | "only");
    }
    if key == "pull.rebase" {
        return matches!(value, "true" | "false" | "merges");
    }
    if let Some((prefix, field)) = key.rsplit_once('.') {
        if prefix.starts_with("branch.") {
            return match field {
                "remote" | "pushremote" => matches!(value, "origin" | "."),
                "merge" => {
                    value.starts_with("refs/heads/")
                        && git(private)
                            .args(["check-ref-format", value])
                            .output()
                            .is_ok_and(|out| out.status.success())
                }
                "rebase" => matches!(value, "true" | "false" | "merges"),
                _ => false,
            };
        }
        if prefix == "remote.origin" && field == "fetch" {
            return value == "+refs/heads/*:refs/remotes/origin/*";
        }
    }
    false
}

fn activate_standalone_configuration(
    private: &Path,
    heads: &str,
    receipt: &Path,
    mapping: Option<(&Path, &Path)>,
) -> Result<()> {
    let mapping = mapping
        .map(|(from, to)| {
            if !from.is_absolute() || !to.is_absolute() {
                return Err(BulkloadRefusal::PathNotAbsolute);
            }
            common_repository(to)?;
            Ok((
                from.to_str()
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
                to.to_str().ok_or(BulkloadRefusal::GitInventoryMalformed)?,
            ))
        })
        .transpose()?;
    let files: Vec<(String, Vec<u8>)> = postcard::from_bytes(&output(git(private).args([
        "show",
        &format!("{}:value", capture_revision(heads, "configuration-v1")?),
    ]))?)
    .map_err(|_| BulkloadRefusal::FrameCodec)?;
    let mut activated = Vec::new();
    let mut preserved_only = Vec::new();
    let mut origin_seen = false;
    for (name, bytes) in files {
        if !matches!(name.as_str(), "config" | "config.worktree") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let retained = receipt.join(format!("source-{name}"));
        fs::write(&retained, bytes)?;
        // Worktree-local configuration may be dormant unless the source enables
        // its extension. Preserve it, but do not silently flatten its authority.
        if name == "config.worktree" {
            preserved_only.push("config.worktree (not activated)".to_owned());
            continue;
        }
        let parsed = output(
            git(private)
                .args(["config", "--no-includes", "--null", "--list", "--file"])
                .arg(&retained),
        )?;
        for entry in parsed.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
            let entry =
                std::str::from_utf8(entry).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
            let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
            let value = if key == "remote.origin.url" {
                origin_seen = true;
                match mapping {
                    Some((from, to)) if value == from => to,
                    None if safe_https_origin(value) => value,
                    _ => return Err(BulkloadRefusal::GitAuthorityChanged),
                }
            } else if safe_configuration_value(private, key, value) {
                value
            } else {
                preserved_only.push(key.to_owned());
                continue;
            };
            output(git(private).args(["config", "--local", "--add", key, value]))?;
            activated.push(key.to_owned());
        }
    }
    if mapping.is_some() && !origin_seen {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // Keys, not potentially sensitive values, explain the deliberate policy boundary.
    fs::write(
        receipt.join("configuration-activation.postcard"),
        postcard::to_allocvec(&(activated, preserved_only, mapping))
            .map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    Ok(())
}

fn safe_https_origin(value: &str) -> bool {
    let Some(address) = value.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = address.split_once('/') else {
        return false;
    };
    !host.is_empty()
        && !path.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-._~%".contains(&b))
}

fn prepare_linked_attachment(
    repository: &Path,
    destination: &Path,
    source: &str,
    receipt: &Path,
    private: &Path,
    heads: &str,
) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let retained = receipt.join("capture.bundle");
    import_verified(repository, &retained, source)?;
    let head = capture_revision(heads, "head")?;
    let symbolic = text(git(private).args([
        "show",
        &format!("{}:value", capture_revision(heads, "head-symbolic")?),
    ]))?;
    let captured_exclude = output(git(private).args([
        "show",
        &format!("{}:value", capture_revision(heads, "exclude")?),
    ]))?;
    let exclude_path = PathBuf::from(text(git(repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?);
    let exclude = match fs::read(&exclude_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if exclude != captured_exclude {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    let attached = text(git(repository).args(["worktree", "list", "--porcelain"]))?;
    let reuse = symbolic.starts_with("refs/heads/")
        && text(git(repository).args(["rev-parse", "--verify", &symbolic]))
            .is_ok_and(|tip| tip == head)
        && !attached
            .lines()
            .any(|line| line == format!("branch {symbolic}"));
    let branch = if reuse {
        symbolic
            .strip_prefix("refs/heads/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?
            .to_owned()
    } else {
        format!(
            "carry/{source}/{}",
            blake3::hash(destination.as_os_str().as_bytes()).to_hex()
        )
    };
    let prepared = receipt.join("prepared-worktree");
    let mut command = git(repository);
    command.args(["worktree", "add", "--no-checkout"]);
    if !reuse {
        command.args(["-b", &branch]);
    }
    output(
        command
            .arg("--")
            .arg(&prepared)
            .arg(if reuse { &branch } else { &head }),
    )?;
    if text(git(&prepared).args(["rev-parse", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    output(git(&prepared).args([
        "read-tree",
        &format!("{}^{{tree}}", capture_revision(heads, "staged")?),
    ]))?;
    let admin = PathBuf::from(text(
        git(&prepared).args(["rev-parse", "--absolute-git-dir"]),
    )?);
    let mut reverse = destination.join(".git").as_os_str().as_bytes().to_vec();
    if reverse.contains(&b'\n') || reverse.contains(&b'\r') {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    reverse.push(b'\n');
    fs::write(admin.join("gitdir"), reverse)?;
    // Native administrative locking prevents prune before pointer publication.
    fs::write(admin.join("locked"), b"bulkload attachment preparation\n")?;
    sync_private_tree(&admin)?;
    fs::File::open(admin.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?.sync_dir_counted()?;
    Ok(admin)
}

fn attachment_policy_matches(repository: &Path, private: &Path, heads: &str) -> Result<bool> {
    let expected = output(git(private).args([
        "show",
        &format!("{}:value", capture_revision(heads, "exclude")?),
    ]))?;
    let path = text(git(repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?;
    let actual = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if expected != actual {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(true)
}

/// Attach captured source administration to an exactly matching payload only.
///
/// Existing payload is never rewritten. New linked administration inherits the
/// explicit common repository's configuration and restores captured source
/// staging, not unknown old staging. Comparisons are
/// optimistic full censuses, not an atomic snapshot of uncooperative writers.
///
/// # Errors
/// Refuses existing .git, differing bytes/modes/empty directories, concurrent
/// changes, unsupported seats, or a receipt inside the payload/on another device.
/// Failed private preparations remain available for inspection.
pub fn attach_matching_payload(
    bundle: &Path,
    repository: &Path,
    destination: &Path,
    source: &str,
    receipt: &Path,
) -> Result<()> {
    attach_payload(bundle, Some(repository), None, destination, source, receipt)
}

/// Attach standalone administration with an explicit local-origin path mapping.
///
/// Complete source local configuration remains immutable capture data. Only
/// safe declarative keys activate; hooks, helpers, includes, extensions and
/// source worktree paths never activate. The activation receipt names omissions.
/// The receipt directory becomes live Git administration and must be retained.
///
/// # Errors
/// Refuses payload divergence, occupied .git, invalid mapping or unknown origin.
pub fn attach_standalone_payload(
    bundle: &Path,
    destination: &Path,
    source: &str,
    receipt: &Path,
    origin_from: &Path,
    origin_to: &Path,
) -> Result<()> {
    attach_payload(
        bundle,
        None,
        Some((origin_from, origin_to)),
        destination,
        source,
        receipt,
    )
}

fn attach_payload(
    bundle: &Path,
    repository: Option<&Path>,
    mapping: Option<(&Path, &Path)>,
    destination: &Path,
    source: &str,
    receipt: &Path,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let staged = stage_bundle(bundle)?;
    let bundle = staged.path();
    let destination = fs::canonicalize(destination)?;
    let repository = repository.map(fs::canonicalize).transpose()?;
    let common = repository.as_deref().map(common_repository).transpose()?;
    let receipt_parent =
        fs::canonicalize(receipt.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?;
    let receipt = receipt_parent.join(
        receipt
            .file_name()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    );
    if receipt.starts_with(&destination) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    require_missing(&destination.join(".git"))?;
    let root = fs::metadata(&destination)?;
    if !root.is_dir() || root.dev() != fs::metadata(&receipt_parent)?.dev() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let before = filesystem_rows(&destination)?;
    let (private, heads) = prepare_attachment(bundle, &destination, source, &receipt)?;
    // #106: attachment adopts an existing payload under the staged index
    // only; it does not re-mark intent-to-add paths, so it refuses a capture
    // carrying any rather than drop them.
    if !carried_intent_to_add(&private, &heads)?.is_empty() {
        return Err(BulkloadRefusal::GitInventoryIntentToAdd);
    }
    let expected: Vec<crate::RowSchema> = postcard::from_bytes(&output(git(&private).args([
        "show",
        &format!("{}:value", capture_revision(&heads, "filesystem-v1")?),
    ]))?)
    .map_err(|_| BulkloadRefusal::FrameCodec)?;
    if !payload_shape_equal(&before, &expected) {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let comparison = receipt.join("comparison.index");
    output(snapshot_command(&private, &destination, &comparison).args(["read-tree", "--empty"]))?;
    let expected_tree = text(git(&private).args([
        "rev-parse",
        &format!("{}^{{tree}}", capture_revision(&heads, "worktree")?),
    ]))?;
    // The raw reader checks each file's identity before and after its one byte
    // pass. The outer census also rejects namespace or metadata changes; a
    // second full byte pass does not make this an atomic snapshot.
    if capture_tree(&private, &destination, &comparison)? != expected_tree
        || filesystem_rows(&destination)? != before
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    fs::write(
        receipt.join("original-payload-index-absent.postcard"),
        postcard::to_allocvec(&before).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    sync_private_tree(&receipt)?;
    fs::File::open(&receipt_parent)?.sync_dir_counted()?;
    let admin = if let Some(repository) = &repository {
        prepare_linked_attachment(repository, &destination, source, &receipt, &private, &heads)?
    } else {
        let (from, to) = mapping.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        activate_standalone_configuration(&private, &heads, &receipt, Some((from, to)))?;
        private.clone()
    };
    let pointer = write_git_pointer(&receipt, &admin)?;
    sync_private_tree(&receipt)?;
    fs::File::open(&receipt_parent)?.sync_dir_counted()?;
    let current_root = fs::metadata(&destination)?;
    if root.dev() != current_root.dev()
        || root.ino() != current_root.ino()
        || filesystem_rows(&destination)? != before
        || repository
            .as_deref()
            .map(|repo| attachment_policy_matches(repo, &private, &heads))
            .transpose()?
            .is_some_and(|matches| !matches)
        || text(
            snapshot_command(&admin, &destination, &admin.join("index"))
                .args(["rev-parse", "HEAD"]),
        )? != capture_revision(&heads, "head")?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    // Atomic create-only publication cannot overwrite another writer's .git.
    fs::hard_link(pointer, destination.join(".git"))?;
    fs::File::open(&destination)?.sync_dir_counted()?;
    if filesystem_rows(&destination)? != before
        || &common_repository(&destination)? != common.as_ref().unwrap_or(&private)
        || repository
            .as_deref()
            .map(|repo| attachment_policy_matches(repo, &private, &heads))
            .transpose()?
            .is_some_and(|matches| !matches)
        || text(git(&destination).args(["rev-parse", "HEAD"]))? != capture_revision(&heads, "head")?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    if repository.is_some() {
        fs::remove_file(admin.join("locked"))?;
    }
    fs::File::open(admin)?.sync_dir_counted()?;
    Ok(())
}

fn write_git_pointer(receipt: &Path, admin: &Path) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let pointer = receipt.join("git-pointer");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pointer)?;
    let path = admin.as_os_str().as_bytes();
    if path.contains(&b'\n') || path.contains(&b'\r') {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    file.write_all(b"gitdir: ")?;
    file.write_all(path)?;
    file.write_all(b"\n")?;
    Ok(pointer)
}

/// Restore staged and unstaged state into a newly created, standalone repository.
///
/// Existing destinations are always refused, including empty directories.
/// The source bundle remains the recovery carrier if restoration is interrupted.
///
/// # Errors
/// Refuses malformed paths/modes, missing capture metadata, or an occupied target.
/// Partial new destinations are retained, never cleaned by recursive deletion.
pub fn restore_bundle(bundle: &Path, destination: &Path, source: &str) -> Result<()> {
    restore_bundle_configured(bundle, destination, source, None)
}

/// Restore an absent standalone checkout with explicit local-origin mapping.
///
/// Without a mapping, a captured HTTPS origin is retained unchanged; no origin
/// is invented when the source has none. Other origin schemes require a reviewed
/// mapping. Configuration omissions remain named in .git/carry-config receipts.
/// No network operation or captured executable configuration is activated.
///
/// # Errors
/// Refuses old captures lacking configuration, unsafe/unmapped origins, occupied
/// destinations, or any malformed filesystem/capture state.
pub fn restore_bundle_configured(
    bundle: &Path,
    destination: &Path,
    source: &str,
    mapping: Option<(&Path, &Path)>,
) -> Result<()> {
    restore_staged(&stage_bundle(bundle)?, destination, source, mapping)
}

/// [`restore_bundle_configured`] from a bundle this process already staged.
///
/// # Errors
/// As [`restore_bundle_configured`].
pub fn restore_staged(
    staged: &StagedBundle,
    destination: &Path,
    source: &str,
    mapping: Option<(&Path, &Path)>,
) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let bundle = staged.path();
    // R-N114: a destination already there is a collision, refused by type,
    // never as a bare errno.
    match fs::DirBuilder::new().mode(0o700).create(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        // Round 4 N4: the enclosing item never laid the parent down.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(BulkloadRefusal::GitDestinationParentMissing);
        }
        Err(error) => return Err(error.into()),
    }
    let destination = fs::canonicalize(destination)?;
    // Read bundle headers without assuming the destination's object format.
    let heads = text(git(&destination).args(["bundle", "list-heads"]).arg(bundle))?;
    if !shallow::is_custody(&heads) {
        capture_revision(&heads, "configuration-v1")?;
    }
    let format = bundle_object_format(&heads)?;
    output(git(&destination).args(["init", "--template=", &format!("--object-format={format}")]))?;
    import_verified(&destination, bundle, source)?;
    let heads = shallow::headers(&destination, bundle)?;
    let find = |suffix: &str| -> Result<String> {
        heads
            .lines()
            .find_map(|line| {
                line.split_once(' ')
                    .filter(|(_, name)| *name == format!("refs/carry-export/{suffix}"))
            })
            .map(|(value, _)| value.to_owned())
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)
    };
    let head = find("head")?;
    find("configuration-v1")?;
    let symbolic =
        text(git(&destination).args(["show", &format!("{}:value", find("head-symbolic")?)]))?;
    if symbolic.is_empty() {
        output(git(&destination).args(["update-ref", "--no-deref", "HEAD", &head]))?;
    } else {
        if !symbolic.starts_with("refs/heads/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        output(git(&destination).args(["check-ref-format", &symbolic]))?;
        set_ref(&destination, &symbolic, &head)?;
        output(git(&destination).args(["symbolic-ref", "HEAD", &symbolic]))?;
    }
    let exclude = output(git(&destination).args(["show", &format!("{}:value", find("exclude")?)]))?;
    fs::create_dir_all(destination.join(".git/info"))?;
    fs::write(destination.join(".git/info/exclude"), exclude)?;
    let worktree = find("worktree")?;
    let entries = output(git(&destination).args(["ls-tree", "-r", "-z", &worktree]))?;
    // #131: before any worktree byte.
    intent_to_add_restorable(&carried_intent_to_add(&destination, &heads)?, &entries)?;
    restore_entries(&destination, &entries)?;
    let staged = find("staged")?;
    restore_gitlink_directories(&destination, &staged, &heads)?;
    output(git(&destination).args(["read-tree", &format!("{staged}^{{tree}}")]))?;
    restore_intent_to_add(&destination, None, &heads)?;
    restore_filesystem_rows(&destination, &find("filesystem-v1")?)?;
    let config_receipt = destination.join(".git/carry-config");
    fs::DirBuilder::new().mode(0o700).create(&config_receipt)?;
    activate_standalone_configuration(&destination, &heads, &config_receipt, mapping)?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Restore into a new linked worktree without changing any existing checkout.
///
/// The source branch is reused only when its tip matches and Git permits a new
/// attachment; otherwise a private carry branch is created. Existing common
/// ignore policy must match the capture: it is never overwritten.
///
/// # Errors
/// Refuses occupied destinations, differing common excludes, and invalid capture.
/// Partially created worktrees are retained on failure for explicit recovery.
pub fn restore_linked(
    bundle: &Path,
    repository: &Path,
    destination: &Path,
    source: &str,
) -> Result<()> {
    restore_linked_staged(&stage_bundle(bundle)?, repository, destination, source)
}

// A new linked worktree's path, with its parent canonicalized. A parent
// that does not exist is typed, never a bare errno (round 4 N4).
fn linked_destination(destination: &Path) -> Result<PathBuf> {
    let parent = fs::canonicalize(
        destination
            .parent()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    )
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            BulkloadRefusal::GitDestinationParentMissing
        } else {
            error.into()
        }
    })?;
    Ok(parent.join(
        destination
            .file_name()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    ))
}

/// [`restore_linked`] from a bundle this process already staged and checked.
///
/// # Errors
/// As [`restore_linked`].
pub fn restore_linked_staged(
    staged: &StagedBundle,
    repository: &Path,
    destination: &Path,
    source: &str,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let bundle = staged.path();
    if destination.symlink_metadata().is_ok() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let destination = linked_destination(destination)?;
    let repository = fs::canonicalize(repository)?;
    import_verified(&repository, bundle, source)?;
    let heads = shallow::headers(&repository, bundle)?;
    let find = |suffix: &str| -> Result<String> {
        heads
            .lines()
            .find_map(|line| {
                line.split_once(' ')
                    .filter(|(_, name)| *name == format!("refs/carry-export/{suffix}"))
            })
            .map(|(value, _)| value.to_owned())
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)
    };
    let head = find("head")?;
    let symbolic =
        text(git(&repository).args(["show", &format!("{}:value", find("head-symbolic")?)]))?;
    let exclude = output(git(&repository).args(["show", &format!("{}:value", find("exclude")?)]))?;
    let exclude_path = text(git(&repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?;
    let existing_exclude = match fs::read(&exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if exclude != existing_exclude {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    // #131: before the worktree is added, so a capture whose intent-to-add
    // custody cannot be restored refuses with nothing laid down.
    let entries = output(git(&repository).args(["ls-tree", "-r", "-z", &find("worktree")?]))?;
    intent_to_add_restorable(&carried_intent_to_add(&repository, &heads)?, &entries)?;
    let attached = text(git(&repository).args(["worktree", "list", "--porcelain"]))?;
    let source_tip = text(git(&repository).args(["rev-parse", "--verify", &symbolic]));
    let reuse = symbolic.starts_with("refs/heads/")
        && source_tip.as_ref().is_ok_and(|value| value == &head)
        && !attached
            .lines()
            .any(|line| line == format!("branch {symbolic}"));
    let branch = if reuse {
        symbolic
            .strip_prefix("refs/heads/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?
            .to_owned()
    } else {
        let digest = blake3::hash(destination.as_os_str().as_bytes()).to_hex();
        format!("carry/{source}/{digest}")
    };
    let mut command = git(&repository);
    command.args(["worktree", "add", "--no-checkout"]);
    if !reuse {
        command.args(["-b", &branch]);
    }
    command
        .arg("--")
        .arg(&destination)
        .arg(if reuse { &branch } else { &head });
    output(&mut command)?;
    // --no-checkout has created administration only, not captured payload.
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))?;
    if text(git(&destination).args(["rev-parse", "--verify", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    restore_entries(&destination, &entries)?;
    restore_gitlink_directories(&destination, &find("staged")?, &heads)?;
    output(git(&destination).args(["read-tree", &format!("{}^{{tree}}", find("staged")?)]))?;
    restore_intent_to_add(&destination, None, &heads)?;
    restore_filesystem_rows(&destination, &find("filesystem-v1")?)?;
    let final_exclude = match fs::read(exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if final_exclude != exclude {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    if text(git(&destination).args(["rev-parse", "--verify", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    Ok(())
}

// N7 (R-N73): a populated submodule is a directory nest, so its seat is not
// carried, and without a directory at the gitlink path Git reports the
// submodule as deleted. For exactly those gitlinks (the census recorded a
// Directory nest at the path, per the capture's nested-repositories-v1
// custody) and only when lstat finds nothing there, restore creates the empty
// directory `git clone` without --recurse-submodules leaves. Anything already
// at the path (a carried file or symlink seat, a nest's carried ignored
// files) is left alone and never followed; a gitlink with no seat at all in
// the source (`AD`) gets none. Runs before captured modes are applied, so a
// read-only parent is still writable. A parent that is not a real directory
// refuses rather than be followed.
fn restore_gitlink_directories(destination: &Path, staged: &str, heads: &str) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    let name = format!("refs/carry-export/{NESTED_REPOSITORIES_METADATA}");
    let Some(sidecar) = heads.lines().find_map(|line| {
        line.split_once(' ')
            .filter(|(_, reference)| *reference == name)
            .map(|(value, _)| value)
    }) else {
        return Ok(());
    };
    if !oid(sidecar) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let custody: Vec<NestedRepository> = postcard::from_bytes(&output(
        git(destination).args(["show", &format!("{sidecar}:value")]),
    )?)
    .map_err(|_| BulkloadRefusal::FrameCodec)?;
    let directories: std::collections::BTreeSet<&[u8]> = custody
        .iter()
        .filter(|nest| nest.kind == NestedRepositoryKind::Directory && !nest.own_item)
        .map(|nest| nest.rel_path.as_slice())
        .collect();
    if directories.is_empty() {
        return Ok(());
    }
    let entries = output(git(destination).args(["ls-tree", "-r", "-z", staged]))?;
    for entry in entries
        .split(|b| *b == 0)
        .filter(|entry| entry.starts_with(b"160000 "))
    {
        let bytes = entry
            .iter()
            .position(|b| *b == b'\t')
            .and_then(|tab| entry.get(tab + 1..))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !directories.contains(bytes) {
            continue;
        }
        let relative = Path::new(std::ffi::OsStr::from_bytes(bytes));
        if relative.as_os_str().is_empty() || relative.components().any(|part| !matches!(part, Component::Normal(name) if !name.as_bytes().eq_ignore_ascii_case(b".git"))) {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        match fs::symlink_metadata(destination.join(relative)) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut current = destination.to_path_buf();
        for part in relative.components() {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.is_dir() => continue,
                Ok(_) => return Err(BulkloadRefusal::PathEscapesRoot),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            match fs::create_dir(&current) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(BulkloadRefusal::GitDestinationOccupied);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

// R-N114: a restore seat that already exists is a collision between items,
// refused as GIT_DESTINATION_OCCUPIED rather than an errno.
fn occupied(error: std::io::Error) -> BulkloadRefusal {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        BulkloadRefusal::GitDestinationOccupied
    } else {
        error.into()
    }
}

fn restore_entries(destination: &Path, entries: &[u8]) -> Result<()> {
    let mut objects = batch_objects::BatchObjects::new(destination)?;
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        restore_entry(destination, entry, &mut objects)?;
    }
    objects.finish()
}

fn restore_entry(
    destination: &Path,
    entry: &[u8],
    objects: &mut batch_objects::BatchObjects,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Component;
    let tab = entry
        .iter()
        .position(|b| *b == b'\t')
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let header = std::str::from_utf8(
        entry
            .get(..tab)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
    )
    .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let mut fields = header.split_whitespace();
    let mode = fields
        .next()
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    if fields.next() != Some("blob") {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let value = fields
        .next()
        .filter(|value| oid(value))
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    if fields.next().is_some() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let relative = Path::new(std::ffi::OsStr::from_bytes(
        entry
            .get(tab + 1..)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
    ));
    if relative.components().any(|part| !matches!(part, Component::Normal(name) if !name.as_bytes().eq_ignore_ascii_case(b".git"))) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let path = destination.join(relative);
    let parent = path.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?;
    let mut current = destination.to_path_buf();
    for part in parent
        .strip_prefix(destination)
        .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
        .components()
    {
        current.push(part);
        match fs::create_dir(&current) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&current)?.is_dir() {
                    return Err(BulkloadRefusal::PathEscapesRoot);
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    match mode {
        "120000" => {
            let mut target = Vec::new();
            objects.copy_into(value, &mut target, Some(65_536))?;
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), path)
                .map_err(occupied)?;
        }
        "100644" | "100755" => {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(occupied)?;
            objects.copy_into(value, &mut file, None)?;
            file.set_permissions(fs::Permissions::from_mode(if mode == "100755" {
                0o755
            } else {
                0o644
            }))?;
            file.sync_file_counted()?;
        }
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---- WP1 PR 1: one hardening table, partial-clone refusal (S2) ------

    #[test]
    fn every_git_child_carries_the_one_hardening_table() {
        let command = git(Path::new("/estate/repo"));
        let args: Vec<_> = command.get_args().filter_map(|arg| arg.to_str()).collect();
        assert_eq!(args.first(), Some(&"--no-optional-locks"));
        for config in git_env::CONFIG {
            assert!(
                args.windows(2).any(|pair| pair == ["-c", *config]),
                "{config}: {args:?}"
            );
        }
        assert!(args.ends_with(&["-C", "/estate/repo"]), "{args:?}");
        let envs: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for (key, value) in git_env::SET {
            assert_eq!(
                envs.get(std::ffi::OsStr::new(key)),
                Some(&Some(std::ffi::OsStr::new(value))),
                "{key}"
            );
        }
        for key in git_env::CLEARED {
            let expected =
                (*key == "GIT_CEILING_DIRECTORIES").then_some(std::ffi::OsStr::new("/estate"));
            assert_eq!(
                envs.get(std::ffi::OsStr::new(key)),
                Some(&expected),
                "{key}"
            );
        }
    }

    // A clone of `origin` with `--filter=blob:none`, never checked out, so
    // making the fixture faults in nothing either.
    fn partial_fixture(name: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("bulkload-partial-{name}-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let origin = root.join("origin");
        fs::create_dir(&origin).unwrap();
        output(git(&origin).args(["init", "--template="])).unwrap();
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@localhost"),
            ("commit.gpgsign", "false"),
            ("uploadpack.allowFilter", "true"),
        ] {
            output(git(&origin).args(["config", key, value])).unwrap();
        }
        fs::write(origin.join("tracked"), b"blob the clone leaves behind").unwrap();
        output(git(&origin).args(["add", "tracked"])).unwrap();
        output(git(&origin).args(["commit", "-m", "base"])).unwrap();
        let clone = root.join("clone");
        output(
            git(&root)
                .args(["clone", "--quiet", "--template=", "--no-checkout"])
                .args(["--filter=blob:none"])
                .arg(format!("file://{}", origin.display()))
                .arg(&clone),
        )
        .unwrap();
        (root, clone)
    }

    #[test]
    fn a_partial_clone_source_is_refused_before_any_read() {
        let (root, clone) = partial_fixture("export");
        assert!(partial_clone(&clone).unwrap());
        let origin = root.join("origin");
        assert!(!partial_clone(&origin).unwrap());
        let objects_before = filesystem_rows(&clone.join(".git/objects")).unwrap();
        let capture = root.join("capture");
        assert_eq!(
            export_repository(&clone, &capture).unwrap_err(),
            BulkloadRefusal::GitSourcePartialClone
        );
        // Refused before the private repository exists, and the clone's object
        // store is exactly as it was: nothing was fetched into it.
        assert!(!capture.join("repository.git").exists());
        assert_eq!(
            objects_before,
            filesystem_rows(&clone.join(".git/objects")).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn promisor_configuration_alone_marks_a_partial_clone() {
        let (root, _) = partial_fixture("config");
        let origin = root.join("origin");
        output(git(&origin).args(["config", "remote.lane.promisor", "false"])).unwrap();
        assert!(!partial_clone(&origin).unwrap());
        output(git(&origin).args(["config", "remote.lane.promisor", "true"])).unwrap();
        assert!(partial_clone(&origin).unwrap());
        output(git(&origin).args(["config", "--unset", "remote.lane.promisor"])).unwrap();
        output(git(&origin).args(["config", "core.partialCloneFilter", "blob:none"])).unwrap();
        assert!(partial_clone(&origin).unwrap());
        output(git(&origin).args(["config", "--unset", "core.partialCloneFilter"])).unwrap();
        // A `.promisor` pack reached only through an alternate.
        let borrowed = root.join("borrowed");
        fs::create_dir_all(borrowed.join("pack")).unwrap();
        fs::write(borrowed.join("pack/pack-0.promisor"), b"").unwrap();
        fs::create_dir_all(origin.join(".git/objects/info")).unwrap();
        fs::write(
            origin.join(".git/objects/info/alternates"),
            format!("{}\n", borrowed.display()),
        )
        .unwrap();
        assert!(partial_clone(&origin).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn standalone_attachment_maps_origin_and_retains_tracking_without_executable_config() {
        let root = std::env::temp_dir().join(format!("bulkload-config-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let upstream = root.join("upstream");
        for repo in [&source, &upstream] {
            fs::create_dir(repo).unwrap();
            output(git(repo).args(["init", "--template="])).unwrap();
        }
        output(git(&source).args(["config", "user.name", "Test"])).unwrap();
        output(git(&source).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(source.join("tracked"), b"base").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        let symbolic = text(git(&source).args(["symbolic-ref", "HEAD"])).unwrap();
        let branch = symbolic.strip_prefix("refs/heads/").unwrap();
        let from = Path::new("/Users/jess/git/legalab");
        output(git(&source).args(["config", "remote.origin.url"]).arg(from)).unwrap();
        output(git(&source).args([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]))
        .unwrap();
        output(git(&source).args(["config", &format!("branch.{branch}.remote"), "origin"]))
            .unwrap();
        output(git(&source).args(["config", &format!("branch.{branch}.merge"), &symbolic]))
            .unwrap();
        output(git(&source).args(["config", "credential.helper", "!unsafe-helper"])).unwrap();
        output(git(&source).args(["config", "alias.unsafe", "!unsafe-command"])).unwrap();
        fs::write(source.join("tracked"), b"staged").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        fs::write(source.join("tracked"), b"dirty").unwrap();
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let payload = root.join("payload");
        restore_bundle_configured(&bundle, &payload, "neo", Some((from, &upstream))).unwrap();
        fs::rename(payload.join(".git"), root.join("original-git")).unwrap();
        let before = filesystem_rows(&payload).unwrap();
        let receipt = root.join("receipt");
        attach_standalone_payload(&bundle, &payload, "neo", &receipt, from, &upstream).unwrap();
        assert_eq!(before, filesystem_rows(&payload).unwrap());
        assert_eq!(
            text(git(&payload).args(["config", "remote.origin.url"])).unwrap(),
            upstream.to_str().unwrap()
        );
        assert_eq!(
            text(git(&payload).args(["config", &format!("branch.{branch}.remote")])).unwrap(),
            "origin"
        );
        assert_eq!(
            text(git(&payload).args(["config", &format!("branch.{branch}.merge")])).unwrap(),
            symbolic
        );
        assert!(text(git(&payload).args(["config", "credential.helper"])).is_err());
        assert!(text(git(&payload).args(["config", "alias.unsafe"])).is_err());
        assert_eq!(
            fs::read(source.join(".git/config")).unwrap(),
            fs::read(receipt.join("source-config")).unwrap()
        );
        assert_eq!(
            output(git(&source).args(["diff", "--cached", "--binary"])).unwrap(),
            output(git(&payload).args(["diff", "--cached", "--binary"])).unwrap()
        );
        assert_eq!(
            output(git(&source).args(["diff", "--binary"])).unwrap(),
            output(git(&payload).args(["diff", "--binary"])).unwrap()
        );
        assert_https_restore(&root, &source);
        fs::remove_dir_all(root).unwrap();
    }

    fn assert_https_restore(root: &Path, source: &Path) {
        let origin =
            "https://github.com/Medical-Massage-Specialists/medical-massage-specialists-infra.git";
        output(git(source).args(["config", "remote.origin.url", origin])).unwrap();
        let capture = root.join("https-capture");
        let bundle = export_repository(source, &capture).unwrap();
        let destination = root.join("https-restored");
        restore_bundle(&bundle, &destination, "neo").unwrap();
        assert_eq!(
            text(git(&destination).args(["config", "remote.origin.url"])).unwrap(),
            origin
        );
        assert_eq!(
            text(git(&destination).args(["config", "remote.origin.fetch"])).unwrap(),
            "+refs/heads/*:refs/remotes/origin/*"
        );
        let symbolic = text(git(source).args(["symbolic-ref", "HEAD"])).unwrap();
        let branch = symbolic.strip_prefix("refs/heads/").unwrap();
        assert_eq!(
            text(git(&destination).args(["config", &format!("branch.{branch}.remote")])).unwrap(),
            "origin"
        );
        assert_eq!(
            text(git(&destination).args(["config", &format!("branch.{branch}.merge")])).unwrap(),
            symbolic
        );
        assert!(text(git(&destination).args(["config", "credential.helper"])).is_err());
        assert_eq!(
            output(git(source).args(["status", "--porcelain"])).unwrap(),
            output(git(&destination).args(["status", "--porcelain"])).unwrap()
        );
        for unsafe_origin in [
            "ext::bad",
            "https://user:secret@host/repo",
            "https://host/repo?token=secret",
            "/Users/jess/git/legalab",
        ] {
            assert!(!safe_https_origin(unsafe_origin));
        }
        let private = capture.join("repository.git");
        output(git(&private).args(["update-ref", "-d", "refs/carry-export/configuration-v1"]))
            .unwrap();
        let old_bundle = capture.join("old-format.bundle");
        output(
            git(&private)
                .args(["bundle", "create"])
                .arg(&old_bundle)
                .arg("--all"),
        )
        .unwrap();
        let old_destination = root.join("old-format-refused");
        assert!(restore_bundle(&old_bundle, &old_destination, "neo").is_err());
        assert!(!old_destination.join(".git").exists());
    }

    #[test]
    fn exact_payload_attachment_preserves_bytes_inodes_and_captured_staging() {
        use std::os::unix::fs::MetadataExt;
        let root = std::env::temp_dir().join(format!("bulkload-attach-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        output(git(&source).args(["init", "--template="])).unwrap();
        output(git(&source).args(["config", "user.name", "Test"])).unwrap();
        output(git(&source).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(source.join("tracked"), b"base\0binary").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        fs::write(source.join("tracked"), b"staged\0binary").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        fs::write(source.join("tracked"), b"unstaged\0binary").unwrap();
        fs::write(source.join("untracked"), b"kept").unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let payload = root.join("payload");
        restore_bundle(&bundle, &payload, "neo").unwrap();
        fs::rename(payload.join(".git"), root.join("old-private-git")).unwrap();
        let before = filesystem_rows(&payload).unwrap();
        let inode = fs::metadata(payload.join("tracked")).unwrap().ino();
        output(git(&source).args([
            "config",
            "remote.origin.url",
            "ssh://example.test/estate.git",
        ]))
        .unwrap();
        attach_matching_payload(&bundle, &source, &payload, "neo", &root.join("receipt")).unwrap();
        assert_eq!(
            common_repository(&source).unwrap(),
            common_repository(&payload).unwrap()
        );
        assert_eq!(
            text(git(&payload).args(["config", "remote.origin.url"])).unwrap(),
            "ssh://example.test/estate.git"
        );
        assert!(text(git(&source).args(["worktree", "list", "--porcelain"]))
            .unwrap()
            .contains(payload.to_str().unwrap()));
        assert_eq!(before, filesystem_rows(&payload).unwrap());
        assert_eq!(inode, fs::metadata(payload.join("tracked")).unwrap().ino());
        for args in [
            vec!["status", "--porcelain"],
            vec!["diff", "--cached", "--binary"],
            vec!["diff", "--binary"],
        ] {
            assert_eq!(
                output(git(&source).args(&args)).unwrap(),
                output(git(&payload).args(&args)).unwrap()
            );
        }
        assert!(attach_matching_payload(
            &bundle,
            &source,
            &payload,
            "neo",
            &root.join("occupied-receipt")
        )
        .is_err());
        let different = root.join("different");
        restore_bundle(&bundle, &different, "neo").unwrap();
        fs::rename(different.join(".git"), root.join("other-private-git")).unwrap();
        fs::write(different.join("extra"), b"destination-only").unwrap();
        assert!(attach_matching_payload(
            &bundle,
            &source,
            &different,
            "neo",
            &root.join("different-receipt")
        )
        .is_err());
        assert!(!different.join(".git").exists());
        fs::remove_file(different.join("extra")).unwrap();
        fs::write(different.join("tracked"), b"different\0bytes").unwrap();
        assert!(attach_matching_payload(
            &bundle,
            &source,
            &different,
            "neo",
            &root.join("bytes-receipt")
        )
        .is_err());
        assert!(!different.join(".git").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bidirectional_union_reaches_fixed_point_without_provenance_wrapping() {
        let root = std::env::temp_dir().join(format!("bulkload-git-union-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let a = root.join("a");
        let b = root.join("b");
        for (repo, content) in [(&a, b"source-a"), (&b, b"source-b")] {
            fs::create_dir(repo).unwrap();
            output(git(repo).args(["init", "--template="])).unwrap();
            output(git(repo).args(["config", "user.name", "Test"])).unwrap();
            output(git(repo).args(["config", "user.email", "test@localhost"])).unwrap();
            fs::write(repo.join("tracked"), content).unwrap();
            output(git(repo).args(["add", "."])).unwrap();
            output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        }
        let original_a = text(git(&a).args(["rev-parse", "HEAD"])).unwrap();
        let original_b = text(git(&b).args(["rev-parse", "HEAD"])).unwrap();
        assert_eq!(
            common_repository(&a).unwrap(),
            fs::canonicalize(a.join(".git")).unwrap()
        );
        let reusable = reusable_capture_key(&a).unwrap();
        assert_eq!(reusable, reusable_capture_key(&a).unwrap());
        fs::write(a.join("tracked"), b"dirty-a!").unwrap();
        assert_ne!(reusable, reusable_capture_key(&a).unwrap());
        fs::write(a.join("tracked"), b"source-a").unwrap();
        let mut stashes = Vec::new();
        for repo in [&a, &b] {
            fs::write(repo.join("tracked"), b"unique stash state").unwrap();
            output(git(repo).args(["stash", "push"])).unwrap();
            stashes.push(text(git(repo).args(["rev-parse", "refs/stash"])).unwrap());
        }
        // A legacy nested provenance name is kept, not silently discarded.
        let legacy = "refs/carry/sting/old/registry/carry-export/refs/carry/neo/old/heads/topic";
        let before_ref = reusable_capture_key(&a).unwrap();
        set_ref(&a, legacy, &original_a).unwrap();
        assert_ne!(before_ref, reusable_capture_key(&a).unwrap());
        let mut fixed = None;
        for round in 0..5 {
            let ab = export_repository(&a, &root.join(format!("a-{round}"))).unwrap();
            import_bundle(&b, &ab, "neo").unwrap();
            let ba = export_repository(&b, &root.join(format!("b-{round}"))).unwrap();
            import_bundle(&a, &ba, "sting").unwrap();
            let current = (refs(&a).unwrap(), refs(&b).unwrap());
            if round == 1 {
                fixed = Some(current.clone());
            }
            if round > 1 {
                assert_eq!(fixed.as_ref(), Some(&current));
            }
        }
        for repo in [&a, &b] {
            let inventory = refs(repo).unwrap();
            assert!(inventory
                .lines()
                .any(|line| line.starts_with(&original_a) && line.contains(legacy)));
            assert!(inventory.lines().any(|line| line.starts_with(&original_b)));
            for stash in &stashes {
                assert!(inventory.lines().any(|line| line.starts_with(stash)));
            }
            assert!(!inventory.contains("/refs/carry/v1/"));
        }
        assert_eq!(
            text(git(&a).args(["rev-parse", "HEAD"])).unwrap(),
            original_a
        );
        assert_eq!(
            text(git(&b).args(["rev-parse", "HEAD"])).unwrap(),
            original_b
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One end-to-end source/union/restore invariant.
    fn union_preserves_native_head_index_binary_and_stash_history() {
        let root = std::env::temp_dir().join(format!("bulkload-git-carry-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let dest = root.join("dest");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&dest).unwrap();
        for repo in [&source, &dest] {
            output(git(repo).args(["init", "--template="])).unwrap();
            output(git(repo).args(["config", "user.name", "Test"])).unwrap();
            output(git(repo).args(["config", "user.email", "test@localhost"])).unwrap();
            fs::write(repo.join("tracked"), b"base").unwrap();
            fs::write(repo.join("deleted"), b"staged deletion").unwrap();
            output(git(repo).args(["add", "."])).unwrap();
            output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        }
        for content in [b"stash-one", b"stash-two"] {
            fs::write(source.join("tracked"), content).unwrap();
            output(git(&source).args(["stash", "push"])).unwrap();
        }
        output(git(&source).args(["branch", "quote\"branch"])).unwrap();
        fs::write(source.join("tracked"), b"staged").unwrap();
        fs::remove_file(source.join("deleted")).unwrap();
        fs::write(source.join("binary"), [0, 1, 128]).unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        fs::write(source.join("tracked"), b"unstaged").unwrap();
        fs::write(source.join("binary"), [0, 255, 128]).unwrap();
        fs::write(source.join(".gitattributes"), b"*.txt text eol=lf\n").unwrap();
        fs::write(source.join("raw.txt"), b"raw\r\nbytes\r\n").unwrap();
        fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(source.join("ignored"), b"unique ignored bytes").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            // Restoration must flush final modes even when the result is readonly.
            fs::set_permissions(source.join("ignored"), fs::Permissions::from_mode(0o400)).unwrap();
            fs::create_dir_all(source.join("empty/nested")).unwrap();
            fs::set_permissions(source.join("empty"), fs::Permissions::from_mode(0o500)).unwrap();
            fs::write(source.join("executable"), b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(source.join("executable"), fs::Permissions::from_mode(0o700))
                .unwrap();
            std::os::unix::fs::symlink("ignored", source.join("symlink")).unwrap();
        }
        fs::write(dest.join("tracked"), b"destination divergence").unwrap();
        output(git(&dest).args(["add", "."])).unwrap();
        output(git(&dest).args(["-c", "commit.gpgsign=false", "commit", "-m", "destination"]))
            .unwrap();
        let index = fs::read(source.join(".git/index")).unwrap();
        let dest_index = fs::read(dest.join(".git/index")).unwrap();
        let dest_head = fs::read(dest.join(".git/HEAD")).unwrap();
        let native = refs(&dest).unwrap();
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        assert!(
            text(git(&source).args(["bundle", "list-heads"]).arg(&bundle))
                .unwrap()
                .contains("refs/carry-export/refs/heads/quote\"branch")
        );
        let count = import_bundle(&dest, &bundle, "neo").unwrap();
        assert!(count >= 6);
        assert_eq!(count, import_bundle(&dest, &bundle, "neo").unwrap());
        assert_eq!(index, fs::read(source.join(".git/index")).unwrap());
        assert_eq!(dest_index, fs::read(dest.join(".git/index")).unwrap());
        assert_eq!(dest_head, fs::read(dest.join(".git/HEAD")).unwrap());
        assert_eq!(
            fs::read(dest.join("tracked")).unwrap(),
            b"destination divergence"
        );
        for line in native.lines() {
            assert!(refs(&dest).unwrap().lines().any(|now| now == line));
        }
        let carried =
            text(git(&dest).args(["for-each-ref", "--format=%(refname)", "refs/carry/"])).unwrap();
        let worktree = carried
            .lines()
            .find(|line| line.ends_with("/worktree"))
            .unwrap();
        assert_eq!(
            output(git(&dest).args(["show", &format!("{worktree}:binary")])).unwrap(),
            [0, 255, 128]
        );
        assert_eq!(
            output(git(&dest).args(["show", &format!("{worktree}:raw.txt")])).unwrap(),
            b"raw\r\nbytes\r\n"
        );
        assert_eq!(
            output(git(&dest).args(["show", &format!("{worktree}:ignored")])).unwrap(),
            b"unique ignored bytes"
        );
        assert_eq!(
            import_bundle(&dest, &bundle, "../native"),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        let staged = carried
            .lines()
            .find(|line| line.ends_with("/staged"))
            .unwrap();
        assert_eq!(
            output(git(&dest).args(["show", &format!("{staged}:tracked")])).unwrap(),
            b"staged"
        );
        let staged_oid = text(git(&dest).args(["rev-parse", staged])).unwrap();
        let conflicting_oid = text(git(&dest).args(["rev-parse", "HEAD"])).unwrap();
        output(git(&dest).args(["update-ref", staged, &conflicting_oid, &staged_oid])).unwrap();
        let before_refusal = refs(&dest).unwrap();
        assert_eq!(
            import_bundle(&dest, &bundle, "neo"),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
        assert_eq!(refs(&dest).unwrap(), before_refusal);
        output(git(&dest).args(["update-ref", staged, &staged_oid, &conflicting_oid])).unwrap();
        output(git(&dest).args(["update-ref", "-d", staged, &staged_oid])).unwrap();
        let native_missing = "refs/heads/must-stay-absent";
        output(git(&dest).args(["symbolic-ref", staged, native_missing])).unwrap();
        assert!(import_bundle(&dest, &bundle, "neo").is_err());
        assert!(text(git(&dest).args(["rev-parse", "--verify", native_missing])).is_err());
        assert_eq!(
            text(git(&dest).args(["symbolic-ref", staged])).unwrap(),
            native_missing
        );
        output(git(&dest).args(["update-ref", "--no-deref", "-d", staged])).unwrap();
        set_ref(&dest, staged, &staged_oid).unwrap();
        assert_eq!(
            carried
                .lines()
                .filter(|line| line.contains("/stashes/"))
                .count(),
            2
        );
        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        let linked = root.join("linked");
        let exclude_path = dest.join(".git/info/exclude");
        let original_exclude = fs::read(&exclude_path).unwrap_or_default();
        fs::create_dir_all(dest.join(".git/info")).unwrap();
        fs::write(&exclude_path, b"operator-local-policy\n").unwrap();
        assert_eq!(
            restore_linked(&bundle, &dest, &linked, "neo"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        assert!(!linked.exists());
        assert_eq!(fs::read(&exclude_path).unwrap(), b"operator-local-policy\n");
        assert_eq!(dest_index, fs::read(dest.join(".git/index")).unwrap());
        assert_eq!(dest_head, fs::read(dest.join(".git/HEAD")).unwrap());
        fs::write(&exclude_path, original_exclude).unwrap();
        restore_linked(&bundle, &dest, &linked, "neo").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            for target in [&restored, &linked] {
                assert_eq!(
                    fs::metadata(target.join("ignored"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o400
                );
                assert_eq!(
                    fs::metadata(target.join("empty"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o500
                );
                assert_eq!(
                    fs::metadata(target.join("executable"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
                assert!(target.join("empty/nested").is_dir());
                assert_eq!(
                    fs::read_link(target.join("symlink")).unwrap(),
                    PathBuf::from("ignored")
                );
            }
            assert_eq!(
                fs::metadata(&linked).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for args in [
            vec![
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored",
            ],
            vec!["diff", "--binary"],
            vec!["diff", "--cached", "--binary"],
            vec!["symbolic-ref", "HEAD"],
        ] {
            assert_eq!(
                output(git(&source).args(&args)).unwrap(),
                output(git(&restored).args(&args)).unwrap(),
                "{args:?}"
            );
            if args.first() != Some(&"symbolic-ref") {
                assert_eq!(
                    output(git(&source).args(&args)).unwrap(),
                    output(git(&linked).args(&args)).unwrap(),
                    "linked {args:?}"
                );
            }
        }
        assert!(linked.join(".git").is_file());
        let common =
            text(git(&linked).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]))
                .unwrap();
        assert_eq!(
            fs::canonicalize(common).unwrap(),
            fs::canonicalize(dest.join(".git")).unwrap()
        );
        let admin = text(git(&linked).args(["rev-parse", "--absolute-git-dir"])).unwrap();
        let recorded_gitdir = PathBuf::from(
            fs::read_to_string(Path::new(&admin).join("gitdir"))
                .unwrap()
                .trim(),
        );
        assert_eq!(
            fs::canonicalize(&recorded_gitdir).unwrap(),
            fs::canonicalize(linked.join(".git")).unwrap()
        );
        assert_eq!(dest_index, fs::read(dest.join(".git/index")).unwrap());
        assert_eq!(dest_head, fs::read(dest.join(".git/HEAD")).unwrap());
        assert!(restore_linked(&bundle, &dest, &linked, "neo").is_err());
        assert_eq!(fs::read(restored.join("binary")).unwrap(), [0, 255, 128]);
        assert!(restore_bundle(&bundle, &restored, "neo").is_err());
        fs::remove_file(restored.join(".git/index")).unwrap();
        fs::write(restored.join("tracked"), b"destination-only pending change").unwrap();
        repair_missing_index(&bundle, &restored, "neo", &root.join("repair")).unwrap();
        assert_eq!(
            fs::read(restored.join("tracked")).unwrap(),
            b"destination-only pending change"
        );
        assert_eq!(
            output(git(&source).args(["diff", "--cached", "--binary"])).unwrap(),
            output(git(&restored).args(["diff", "--cached", "--binary"])).unwrap()
        );
        assert!(root
            .join("repair/original-administration.postcard")
            .is_file());
        assert!(
            repair_missing_index(&bundle, &restored, "neo", &root.join("repair-again")).is_err()
        );
        fs::remove_file(restored.join(".git/index")).unwrap();
        fs::write(restored.join(".git/index.lock"), b"another Git writer").unwrap();
        assert!(
            repair_missing_index(&bundle, &restored, "neo", &root.join("repair-locked")).is_err()
        );
        assert_eq!(
            fs::read(restored.join(".git/index.lock")).unwrap(),
            b"another Git writer"
        );
        fs::remove_file(restored.join(".git/index.lock")).unwrap();
        assert!(repair_missing_index_inner(
            &bundle,
            &restored,
            "neo",
            &root.join("repair-race"),
            |_| Ok(()),
            |index| {
                assert!(restored.join(".git/index.lock").is_file());
                assert!(output(git(&restored).args(["read-tree", "HEAD"])).is_err());
                fs::write(index, b"concurrent index publication")?;
                Ok(())
            }
        )
        .is_err());
        assert_eq!(
            fs::read(restored.join(".git/index")).unwrap(),
            b"concurrent index publication"
        );
        assert!(!restored.join(".git/index.lock").exists());
        let lock_path = root.join("reservation.lock");
        let reservation = IndexReservation::acquire(lock_path.clone()).unwrap();
        fs::rename(&lock_path, root.join("reservation-original")).unwrap();
        fs::write(&lock_path, b"replacement writer").unwrap();
        drop(reservation);
        assert_eq!(fs::read(lock_path).unwrap(), b"replacement writer");
        fs::remove_file(dest.join(".git/index")).unwrap();
        assert_eq!(
            repair_missing_index(&bundle, &dest, "neo", &root.join("repair-wrong-head")),
            Err(BulkloadRefusal::GitAuthorityChanged)
        );
        assert!(!dest.join(".git/index").exists());
        assert_eq!(
            fs::read(dest.join("tracked")).unwrap(),
            b"destination divergence"
        );
        // Only the test-owned trees regain write permission for fixture cleanup.
        for target in [&source, &restored, &linked] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(target.join("empty"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    pub(super) fn committed_repository_pub(repo: &Path, content: &[u8]) {
        committed_repository(repo, content);
    }

    fn committed_repository(repo: &Path, content: &[u8]) {
        fs::create_dir_all(repo).unwrap();
        output(git(repo).args(["init", "--template="])).unwrap();
        output(git(repo).args(["config", "user.name", "Test"])).unwrap();
        output(git(repo).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(repo.join("tracked"), content).unwrap();
        output(git(repo).args(["add", "."])).unwrap();
        output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
    }

    // Regression for the lab capture refusal: `.claude/worktrees/<name>/.git`
    // is a gitdir pointer for a registered linked worktree of the same
    // repository (Claude Code's EnterWorktree convention). It is custody, not
    // malformed inventory, and its bytes belong to its own estate item.
    #[test]
    fn registered_worktree_nested_inside_the_checkout_is_typed_custody() {
        let root =
            std::env::temp_dir().join(format!("bulkload-nested-worktree-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("lab");
        committed_repository(&source, b"lab");
        let nested = source.join(".claude/worktrees/agent-x");
        fs::create_dir_all(source.join(".claude/worktrees")).unwrap();
        fs::write(source.join(".claude/settings.json"), b"{}").unwrap();
        output(
            git(&source)
                .args(["worktree", "add", "-b", "agent-x"])
                .arg(&nested),
        )
        .unwrap();
        assert!(nested.join(".git").is_file());
        fs::write(nested.join("agent-only"), b"belongs to the nested item").unwrap();
        let nested_head = text(git(&nested).args(["rev-parse", "--verify", "HEAD"])).unwrap();

        let custody = nested_worktrees(&source).unwrap();
        assert_eq!(
            custody,
            vec![NestedWorktree {
                rel_path: b".claude/worktrees/agent-x".to_vec(),
                worktree_name: "agent-x".to_owned(),
                head_oid: nested_head,
            }]
        );
        let key = reusable_capture_key(&source).unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        // Nested payload is not this item's census; its own HEAD is.
        fs::write(nested.join("agent-only"), b"changed nested bytes").unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        output(git(&nested).args(["add", "."])).unwrap();
        output(git(&nested).args(["-c", "commit.gpgsign=false", "commit", "-m", "agent"])).unwrap();
        assert_ne!(key, reusable_capture_key(&source).unwrap());
        let key = reusable_capture_key(&source).unwrap();

        let capture = root.join("capture");
        let bundle = export_repository(&source, &capture).unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        let private = capture.join("repository.git");
        let manifest: Vec<NestedWorktree> = postcard::from_bytes(
            &output(git(&private).args([
                "show",
                &format!("refs/carry-export/{NESTED_WORKTREES_METADATA}:value"),
            ]))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest,
            vec![NestedWorktree {
                rel_path: b".claude/worktrees/agent-x".to_vec(),
                worktree_name: "agent-x".to_owned(),
                head_oid: text(git(&nested).args(["rev-parse", "--verify", "HEAD"])).unwrap(),
            }]
        );
        let seats: Vec<crate::RowSchema> = postcard::from_bytes(
            &output(git(&private).args(["show", "refs/carry-export/filesystem-v1:value"])).unwrap(),
        )
        .unwrap();
        let paths: Vec<&[u8]> = seats.iter().map(|row| row.rel_path.as_slice()).collect();
        assert!(paths.contains(&b".claude".as_slice()));
        assert!(paths.contains(&b".claude/settings.json".as_slice()));
        assert!(paths.contains(&b".claude/worktrees".as_slice()));
        assert!(!paths
            .iter()
            .any(|path| path.starts_with(b".claude/worktrees/agent-x")));
        let carried = output(git(&private).args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/carry-export/worktree",
        ]))
        .unwrap();
        assert!(!String::from_utf8(carried).unwrap().contains("agent-x"));

        // A restore of the enclosing item leaves the nested root absent for the
        // nested item's own restore; nothing of the worktree was invented.
        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        assert!(restored.join(".claude/worktrees").is_dir());
        assert!(!restored.join(".claude/worktrees/agent-x").exists());
        assert_eq!(fs::read(restored.join("tracked")).unwrap(), b"lab");
        fs::remove_dir_all(root).unwrap();
    }

    fn nested_sidecar(private: &Path) -> Option<Vec<NestedRepository>> {
        let value = git(private)
            .args([
                "show",
                &format!("refs/carry-export/{NESTED_REPOSITORIES_METADATA}:value"),
            ])
            .output()
            .unwrap();
        value
            .status
            .success()
            .then(|| postcard::from_bytes(&value.stdout).unwrap())
    }

    // The custody a restored repository holds for the nests its bundle did
    // not carry: the imported metadata ref, decoded.
    fn imported_nested_custody(repo: &Path) -> Option<Vec<NestedRepository>> {
        let refs = text(git(repo).args(["for-each-ref", "--format=%(refname)", "refs/carry/v1/"]))
            .unwrap();
        let name = refs
            .lines()
            .find(|name| name.ends_with(&format!("/{NESTED_REPOSITORIES_METADATA}")))?;
        Some(
            postcard::from_bytes(
                &output(git(repo).args(["show", &format!("{name}:value")])).unwrap(),
            )
            .unwrap(),
        )
    }

    // The expected custody for a nest on disk: its resolved administration and
    // its unpushed count as Git reports them (R-N73, R-N83, N2, N6).
    pub(super) fn observed(source: &Path, mut nest: NestedRepository) -> NestedRepository {
        use std::os::unix::ffi::OsStrExt;
        let dir = source.join(std::ffi::OsStr::from_bytes(&nest.rel_path));
        let admin =
            text(git(&dir).args(["--git-dir=.git", "rev-parse", "--absolute-git-dir"])).unwrap();
        nest.admin = fs::canonicalize(admin)
            .unwrap()
            .as_os_str()
            .as_bytes()
            .to_vec();
        nest.remotes = !output(git(&dir).args(["--git-dir=.git", "remote"]))
            .unwrap()
            .is_empty();
        let mut count = git(&dir);
        count.args(["--git-dir=.git", "rev-list", "--count", "--all"]);
        count.args(nest.head_oid.as_deref());
        if nest.remotes {
            count.args(["--not", "--remotes"]);
        }
        nest.unpushed = text(&mut count).unwrap().parse().unwrap();
        nest
    }

    pub(super) fn directory_nest(rel_path: &[u8], head: Option<String>) -> NestedRepository {
        NestedRepository {
            rel_path: rel_path.to_vec(),
            kind: NestedRepositoryKind::Directory,
            head_oid: head,
            gitdir_kind: GitdirKind::Directory,
            admin: Vec::new(),
            unpushed: 0,
            remotes: false,
            ignored_carried: 0,
            own_item: false,
        }
    }

    fn head_of(repo: &Path) -> String {
        text(git(repo).args(["rev-parse", "--verify", "HEAD"])).unwrap()
    }

    // A fresh, empty per-process fixture root.
    fn fresh(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        root
    }

    fn committed(repo: &Path, content: &[u8]) {
        committed_repository(repo, content);
    }

    fn commit(repo: &Path, message: &str) {
        output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", message])).unwrap();
    }

    fn refuses_everywhere(source: &Path, capture: &Path) {
        assert_eq!(
            reusable_capture_key(source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        assert_eq!(
            nested_repositories(source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        assert_eq!(
            export_repository(source, capture),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
    }

    // R-N73 B1, the reviewer's reproduction: the outer repository tracks
    // vendor/inner/lib.c, then a repository is initialised over it and the file
    // edited. Treating vendor/inner as foreign custody silently dropped the
    // outer's tracked file and the user's edit. It must refuse instead.
    #[test]
    fn adv_outer_tracked_file_under_nested_git_refuses_instead_of_dropping() {
        let root = fresh("bulkload-adv-tracked-under-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let inner = source.join("vendor/inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        output(git(&source).args(["add", "vendor/inner/lib.c"])).unwrap();
        commit(&source, "vendor lib");
        output(git(&inner).args(["init", "--template="])).unwrap();
        fs::write(inner.join("lib.c"), b"v2 unique user edit").unwrap();
        refuses_everywhere(&source, &root.join("capture"));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N73 B1 in isolation: the nest is perfectly clean (it commits lib.c
    // itself), so only the outer's tracking of a path under it can refuse.
    // Both a `.git` directory and a pointer-file nest are covered.
    #[test]
    fn adv_outer_tracked_file_under_a_clean_nest_refuses() {
        let root = fresh("bulkload-adv-tracked-under-clean-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let inner = source.join("vendor/inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        output(git(&source).args(["add", "vendor/inner/lib.c"])).unwrap();
        commit(&source, "vendor lib");
        // `committed` adds everything present, lib.c included.
        committed(&inner, b"inner");
        assert!(output(git(&inner).args(["status", "--porcelain"]))
            .unwrap()
            .is_empty());
        refuses_everywhere(&source, &root.join("capture-directory"));
        fs::remove_dir_all(&inner).unwrap();
        output(git(&source).args(["rm", "-q", "--cached", "vendor/inner/lib.c"])).unwrap();
        commit(&source, "untrack");
        assert!(nested_repositories(&source).unwrap().is_empty());

        // A pointer-file nest: another repository's linked worktree, with a
        // path under it tracked by the outer repository.
        let other = root.join("other");
        committed(&other, b"other");
        let foreign = source.join("vendor/foreign");
        output(
            git(&other)
                .args(["worktree", "add", "-b", "foreign"])
                .arg(&foreign),
        )
        .unwrap();
        assert!(foreign.join(".git").is_file());
        let blob = input(
            git(&source).args(["hash-object", "-w", "--stdin"]),
            b"outer copy",
        )
        .unwrap();
        let blob = String::from_utf8(blob).unwrap();
        output(git(&source).args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{},vendor/foreign/tracked", blob.trim()),
        ]))
        .unwrap();
        commit(&source, "track under foreign");
        refuses_everywhere(&source, &root.join("capture-pointer"));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N73 B2, inverted from the reviewer's reproduction: a committed
    // submodule must restore as a submodule. Stripping the gitlink from the
    // staged tree made `git diff --cached HEAD` show `D lib/vendor` after both
    // restore_bundle and repair_missing_index.
    #[test]
    fn adv_committed_gitlink_restores_without_a_staged_submodule_deletion() {
        let root = fresh("bulkload-adv-committed-gitlink");
        let source = root.join("outer");
        committed(&source, b"outer");
        let oid = head_of(&source);
        fs::create_dir_all(source.join("lib/vendor")).unwrap();
        output(git(&source).args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},lib/vendor"),
        ]))
        .unwrap();
        commit(&source, "submodule");
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        let clean = |repo: &Path| {
            assert_eq!(
                output(git(repo).args(["diff", "--cached", "--name-status", "HEAD"])).unwrap(),
                b""
            );
            assert_eq!(
                text(git(repo).args(["ls-files", "--stage", "--", "lib/vendor"])).unwrap(),
                format!("160000 {oid} 0\tlib/vendor")
            );
        };
        clean(&restored);
        fs::remove_file(restored.join(".git/index")).unwrap();
        repair_missing_index(&bundle, &restored, "neo", &root.join("repair")).unwrap();
        clean(&restored);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N73 B2: the reviewer's proof that no strip is needed. A gitlink naming
    // a commit that exists nowhere bundles, verifies and restores as-is.
    #[test]
    fn adv_gitlink_to_absent_commit_bundles_fine_without_stripping() {
        let root = fresh("bulkload-adv-absent-gitlink");
        let source = root.join("outer");
        committed(&source, b"outer");
        let absent = "0123456789abcdef0123456789abcdef01234567";
        assert!(!git(&source)
            .args(["cat-file", "-e", absent])
            .status()
            .unwrap()
            .success());
        output(git(&source).args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{absent},third_party/absent"),
        ]))
        .unwrap();
        commit(&source, "absent submodule");
        let capture = root.join("capture");
        let bundle = export_repository(&source, &capture).unwrap();
        let private = capture.join("repository.git");
        output(git(&private).args(["bundle", "verify"]).arg(&bundle)).unwrap();
        assert_eq!(
            text(git(&private).args([
                "ls-tree",
                "refs/carry-export/staged",
                "--",
                "third_party/absent"
            ]))
            .unwrap(),
            format!("160000 commit {absent}\tthird_party/absent")
        );
        assert!(!git(&private)
            .args(["cat-file", "-e", absent])
            .status()
            .unwrap()
            .success());
        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        assert_eq!(
            output(git(&restored).args(["diff", "--cached", "--name-status", "HEAD"])).unwrap(),
            b""
        );
        assert_eq!(
            text(git(&restored).args(["ls-files", "--stage", "--", "third_party/absent"])).unwrap(),
            format!("160000 {absent} 0\tthird_party/absent")
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn refuses_everywhere_with(source: &Path, capture: &Path, refusal: &BulkloadRefusal) {
        assert_eq!(reusable_capture_key(source).as_ref(), Err(refusal));
        assert_eq!(nested_repositories(source).as_ref(), Err(refusal));
        assert_eq!(export_repository(source, capture).as_ref(), Err(refusal));
    }

    // R-N83 (a): a stash in a nest is work the enclosing capture would not
    // carry. A nest whose worktree is clean but holds refs/stash, or a
    // non-empty stash reflog, refuses with the typed stash refusal.
    #[test]
    fn a_nest_with_a_stash_refuses_naming_stash() {
        let root = fresh("bulkload-stashed-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        fs::write(nest.join("tracked"), b"stashed edit").unwrap();
        output(git(&nest).args(["-c", "commit.gpgsign=false", "stash", "-q"])).unwrap();
        assert!(output(git(&nest).args(["status", "--porcelain"]))
            .unwrap()
            .is_empty());
        refuses_everywhere_with(
            &source,
            &root.join("capture-ref"),
            &BulkloadRefusal::GitNestStashed,
        );
        output(git(&nest).args(["stash", "drop", "-q"])).unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        // A stash reflog with entries but no ref still names lost work.
        fs::create_dir_all(nest.join(".git/logs/refs")).unwrap();
        fs::write(
            nest.join(".git/logs/refs/stash"),
            format!(
                "{} {} T <t@localhost> 0 +0000\tWIP\n",
                "0".repeat(40),
                head_of(&nest)
            ),
        )
        .unwrap();
        refuses_everywhere_with(
            &source,
            &root.join("capture-reflog"),
            &BulkloadRefusal::GitNestStashed,
        );
        // An empty stash reflog is no stash.
        fs::write(nest.join(".git/logs/refs/stash"), b"").unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N83 (a): a commit reachable only from a detached HEAD would be lost
    // with the nest. It refuses with the typed detached-unreachable refusal.
    #[test]
    fn a_detached_nest_with_an_unreachable_commit_refuses() {
        let root = fresh("bulkload-detached-unreachable-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        output(git(&nest).args(["checkout", "-q", "--detach"])).unwrap();
        fs::write(nest.join("tracked"), b"detached work").unwrap();
        output(git(&nest).args(["add", "tracked"])).unwrap();
        commit(&nest, "only reachable from HEAD");
        refuses_everywhere_with(
            &source,
            &root.join("capture"),
            &BulkloadRefusal::GitNestDetachedUnreachable,
        );
        // Naming it with a tag makes it reachable, and so custody again.
        output(git(&nest).args(["tag", "kept"])).unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N83 (a): a detached HEAD some branch, tag or remote-tracking ref
    // reaches is custody, and `unpushed` stays honest: it counts local-only
    // commits reachable from a branch or from HEAD itself.
    #[test]
    fn a_detached_nest_on_a_reachable_commit_is_custody_with_an_honest_count() {
        let root = fresh("bulkload-detached-reachable-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        output(git(&nest).args([
            "remote",
            "add",
            "origin",
            "https://example.invalid/inner.git",
        ]))
        .unwrap();
        output(git(&nest).args(["update-ref", "refs/remotes/origin/main", &head_of(&nest)]))
            .unwrap();
        fs::write(nest.join("tracked"), b"branch work").unwrap();
        output(git(&nest).args(["add", "tracked"])).unwrap();
        commit(&nest, "on the branch, not pushed");
        // Detached at the branch tip: reachable from the branch.
        output(git(&nest).args(["checkout", "-q", "--detach"])).unwrap();
        let unpushed = |source: &Path| {
            nested_repositories(source)
                .unwrap()
                .iter()
                .map(|nest| (nest.unpushed, nest.remotes))
                .collect::<Vec<_>>()
        };
        assert_eq!(unpushed(&source), vec![(1, true)]);
        // Detached on a commit only a local tag reaches: still custody, and
        // HEAD's local-only commit is counted rather than hidden.
        fs::write(nest.join("tracked"), b"tagged work").unwrap();
        output(git(&nest).args(["add", "tracked"])).unwrap();
        commit(&nest, "reachable from a tag only");
        output(git(&nest).args(["tag", "release"])).unwrap();
        assert_eq!(unpushed(&source), vec![(2, true)]);
        // Detached on the remote-tracking commit: the branch and tag commits
        // are still local-only and still counted (N2: every ref, not HEAD).
        output(git(&nest).args(["checkout", "-q", "--detach", "origin/main"])).unwrap();
        assert_eq!(unpushed(&source), vec![(2, true)]);
        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        assert_eq!(export.nested_repositories.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N89 (supersedes R-N83 b): ignored files inside a clean nest are
    // carried exactly as the checkout's own ignored files are, counted in the
    // receipt, part of the key through their seats, and written back under
    // the nest path on restore. A rebuildable root inside the nest is still
    // omitted and recorded. Contents are compared, never printed.
    #[test]
    fn ignored_files_in_a_clean_nest_are_carried_and_restored() {
        let root = fresh("bulkload-ignored-in-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        fs::create_dir_all(&nest).unwrap();
        fs::write(nest.join(".gitignore"), b"*.log\n/build/\n/target/\n").unwrap();
        committed(&nest, b"inner");
        fs::create_dir_all(nest.join("build/deep")).unwrap();
        fs::create_dir_all(nest.join("target/debug")).unwrap();
        let carried: [(&str, &[u8]); 4] = [
            ("a.log", b"log"),
            ("odd\n.log", b"newline name"),
            ("build/x.o", b"obj"),
            ("build/deep/y.o", b"deeper obj"),
        ];
        for (path, bytes) in carried {
            fs::write(nest.join(path), bytes).unwrap();
        }
        std::os::unix::fs::symlink("a.log", nest.join("link.log")).unwrap();
        fs::write(nest.join("target/debug/artifact"), vec![7u8; 4096]).unwrap();

        let key = reusable_capture_key(&source).unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        fs::write(nest.join("a.log"), b"longer log").unwrap();
        assert_ne!(key, reusable_capture_key(&source).unwrap());
        // Rebuildable churn inside the nest does not move the key.
        let key = reusable_capture_key(&source).unwrap();
        fs::write(nest.join("target/debug/artifact"), vec![8u8; 8192]).unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());

        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        let [nested] = export.nested_repositories.as_slice() else {
            unreachable!("one nest");
        };
        assert_eq!(nested.ignored_carried, 5);
        assert!(nested.receipt_line().ends_with(" ignored-carried=5"));
        assert_eq!(nested.receipt_line().lines().count(), 1);
        assert_eq!(
            export
                .omitted
                .iter()
                .map(|omission| omission.rel_path.as_slice())
                .collect::<Vec<_>>(),
            vec![b"vendor/inner/target".as_slice()]
        );
        let private = capture.join("repository.git");
        let paths = carried_paths(&private);
        assert!(paths.contains("vendor/inner/build/deep/y.o"));
        assert!(paths.contains("vendor/inner/link.log"));
        assert!(!paths.contains("target"));
        assert!(!paths.contains("vendor/inner/tracked"));
        assert!(!paths.contains(".gitignore"));

        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        let back = restored.join("vendor/inner");
        for (path, _) in carried {
            assert!(
                fs::read(back.join(path)).unwrap() == fs::read(nest.join(path)).unwrap(),
                "an ignored nest file did not round-trip"
            );
        }
        assert_eq!(
            fs::read_link(back.join("link.log")).unwrap(),
            Path::new("a.log")
        );
        assert!(!back.join("target").exists());
        assert!(!back.join("tracked").exists());
        assert!(!back.join(".git").exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn markers_fired(markers: &Path) -> Vec<std::ffi::OsString> {
        fs::read_dir(markers)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect()
    }

    // Stat-dirty but content-clean: status must read content, which is
    // exactly when it would run a clean filter or consult fsmonitor.
    fn stat_dirty(file: &Path) {
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    // R-N83 / N4: a nest's filter commands would both execute nest-chosen code
    // and decide what "clean" means. A nest whose config sets one refuses,
    // and the command never runs. A populated submodule inside a nest, with
    // or without its own filter, refuses by name first (R-N115).
    #[test]
    fn a_nest_with_filter_commands_refuses_without_running_them() {
        let root = fresh("bulkload-filter-nest");
        let markers = root.join("markers");
        fs::create_dir(&markers).unwrap();
        let touch = |name: &str| format!("touch {}; cat", markers.join(name).display());
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        fs::create_dir_all(&nest).unwrap();
        fs::write(nest.join(".gitattributes"), b"* filter=ev.il\n").unwrap();
        committed(&nest, b"inner");
        stat_dirty(&nest.join("tracked"));
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);

        // Dotted driver name, every command kind.
        for (key, value) in [
            ("filter.ev.il.clean", touch("nest-clean")),
            ("filter.ev.il.smudge", touch("nest-smudge")),
            ("filter.ev.il.process", touch("nest-process")),
            ("filter.ev.il.required", "true".to_owned()),
        ] {
            output(git(&nest).args(["config", key, &value])).unwrap();
        }
        refuses_everywhere(&source, &root.join("capture-nest"));
        assert_eq!(markers_fired(&markers), Vec::<std::ffi::OsString>::new());
        // Control: an unhardened status in the nest does run it.
        let _ = Command::new("git")
            .arg("-C")
            .arg(&nest)
            .args(["status", "--porcelain"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(!markers_fired(&markers).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    // R-N115: a nest holding a populated submodule refuses with a typed code
    // naming the submodule's escaped path; its hostile filter never runs. An
    // unpopulated gitlink in a nest is fine.
    #[test]
    fn a_nest_with_a_populated_submodule_refuses_by_name() {
        let root = fresh("bulkload-populated-in-nest");
        let markers = root.join("markers");
        fs::create_dir(&markers).unwrap();
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        let head = head_of(&nest);
        output(git(&nest).args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},empty"),
        ]))
        .unwrap();
        commit(&nest, "unpopulated gitlink");
        fs::create_dir(nest.join("empty")).unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        let sub = nest.join("odd\nsub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".gitattributes"), b"* filter=evil2\n").unwrap();
        committed(&sub, b"sub");
        output(git(&nest).args(["add", "odd\nsub"])).unwrap();
        commit(&nest, "populated submodule");
        output(git(&sub).args([
            "config",
            "filter.evil2.clean",
            &format!("touch {}; cat", markers.join("sub-clean").display()),
        ]))
        .unwrap();
        stat_dirty(&sub.join("tracked"));
        let expected =
            BulkloadRefusal::GitNestPopulatedSubmodule(b"vendor/inner/odd\nsub".to_vec());
        refuses_everywhere_with(&source, &root.join("capture"), &expected);
        assert_eq!(
            expected.to_string(),
            "GIT_NEST_POPULATED_SUBMODULE path=\"vendor/inner/odd\\nsub\""
        );
        assert_eq!(markers_fired(&markers), Vec::<std::ffi::OsString>::new());
        fs::remove_dir_all(root).unwrap();
    }

    // Round 4 N1 (R-N115): a gitlink path inside a nest may hold nothing or an
    // empty directory. A file left under it, a repository under it, or a file
    // or symlink at it refuses by name; none of it would be carried by anyone.
    #[test]
    fn a_nest_gitlink_path_holding_anything_but_an_empty_directory_refuses_by_name() {
        let root = fresh("bulkload-nest-gitlink-seat");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        let head = head_of(&nest);
        output(git(&nest).args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},s"),
        ]))
        .unwrap();
        commit(&nest, "gitlink s");
        let expected = BulkloadRefusal::GitNestPopulatedSubmodule(b"vendor/inner/s".to_vec());
        // Absent: status reports the submodule deleted, which is dirt.
        assert_eq!(
            nested_repositories(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        // An empty directory, as a clone without --recurse-submodules leaves.
        fs::create_dir(nest.join("s")).unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        // A file left under it.
        fs::write(nest.join("s/leftover"), b"only copy").unwrap();
        refuses_everywhere_with(&source, &root.join("capture-file-under"), &expected);
        fs::remove_file(nest.join("s/leftover")).unwrap();
        // A whole repository under it.
        committed(&nest.join("s/deep"), b"deep");
        refuses_everywhere_with(&source, &root.join("capture-repo-under"), &expected);
        fs::remove_dir_all(nest.join("s")).unwrap();
        // A file at the gitlink path itself.
        fs::write(nest.join("s"), b"a file where the submodule was").unwrap();
        refuses_everywhere_with(&source, &root.join("capture-file-at"), &expected);
        fs::remove_file(nest.join("s")).unwrap();
        // A symlink at the gitlink path, never followed.
        std::os::unix::fs::symlink(&root, nest.join("s")).unwrap();
        refuses_everywhere_with(&source, &root.join("capture-link-at"), &expected);
        fs::remove_file(nest.join("s")).unwrap();
        fs::create_dir(nest.join("s")).unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N83: a nest's own fsmonitor command and hooks never run, and its
    // index (and untracked cache) is never written.
    #[test]
    fn a_hostile_nest_config_runs_no_fsmonitor_or_hook_and_writes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let root = fresh("bulkload-hostile-nest");
        let markers = root.join("markers");
        fs::create_dir(&markers).unwrap();
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        let script = root.join("hostile.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch {}/\"$(basename \"$0\")\"\nexit 1\n",
                markers.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let hooks = root.join("hooks");
        fs::create_dir(&hooks).unwrap();
        for hook in ["post-index-change", "fsmonitor-watchman", "post-checkout"] {
            fs::copy(&script, hooks.join(hook)).unwrap();
        }
        for (key, value) in [
            ("core.fsmonitor", script.display().to_string()),
            ("core.hooksPath", hooks.display().to_string()),
            ("core.untrackedCache", "true".to_owned()),
        ] {
            output(git(&nest).args(["config", key, &value])).unwrap();
        }
        stat_dirty(&nest.join("tracked"));
        let index = fs::read(nest.join(".git/index")).unwrap();

        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        reusable_capture_key(&source).unwrap();
        export_repository(&source, &root.join("capture")).unwrap();
        assert_eq!(markers_fired(&markers), Vec::<std::ffi::OsString>::new());
        assert_eq!(fs::read(nest.join(".git/index")).unwrap(), index);
        fs::remove_dir_all(root).unwrap();
    }

    // B4 (R-N73 on #52's field-wise comparison): a nest's HEAD moving is Git
    // authority, never drift. Moved during the export's byte pass it refuses
    // the export; moved between two key reads it is outside drift_only, so
    // drift_to refuses. Nest-free drift is unaffected (the #52 suite).
    #[test]
    fn a_nest_head_moving_mid_pass_refuses() {
        let root = fresh("bulkload-nest-head-mid-pass");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        let moved = |nest: &Path, bytes: &[u8]| {
            fs::write(nest.join("tracked"), bytes).unwrap();
            output(git(nest).args(["-c", "commit.gpgsign=false", "commit", "-q", "-am", "moved"]))
                .unwrap();
        };
        let before = capture_key_parts(&source).unwrap();
        moved(&nest, b"moved between keys");
        let after = capture_key_parts(&source).unwrap();
        assert_eq!(
            before.drift_to(&after),
            Err(BulkloadRefusal::GitAuthorityChanged)
        );
        assert_ne!(before.digest().unwrap(), after.digest().unwrap());
        assert_ne!(before.authority().unwrap(), after.authority().unwrap());

        let inner = nest;
        mid_pass::arm_at(&source, mid_pass::Stage::BytePass, move || {
            moved(&inner, b"moved during the byte pass");
        });
        let refused =
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default());
        fs::remove_dir_all(root).unwrap();
        assert_eq!(
            refused.map(|_| ()),
            Err(BulkloadRefusal::GitAuthorityChanged)
        );
    }

    // R-N73 B3: a foreign nest whose worktree differs from its HEAD, staged or
    // not, is not custody. Its bytes would not be carried, so it refuses.
    #[test]
    fn a_dirty_nest_refuses() {
        let root = fresh("bulkload-dirty-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        assert_eq!(
            nested_repositories(&source).unwrap(),
            vec![observed(
                &source,
                directory_nest(b"vendor/inner", Some(head_of(&nest)))
            )]
        );
        fs::write(nest.join("tracked"), b"unstaged edit").unwrap();
        refuses_everywhere(&source, &root.join("capture-unstaged"));
        output(git(&nest).args(["add", "tracked"])).unwrap();
        refuses_everywhere(&source, &root.join("capture-staged"));
        output(git(&nest).args(["reset", "-q", "--hard"])).unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N73 B3: untracked files make a nest dirty. (Ignored files are not
    // "untracked" to `git status` and do not; the ruling names status.)
    #[test]
    fn a_nest_with_untracked_files_refuses() {
        let root = fresh("bulkload-untracked-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        fs::create_dir_all(nest.join("deep/er")).unwrap();
        fs::write(nest.join("deep/er/new"), b"only here").unwrap();
        refuses_everywhere(&source, &root.join("capture"));
        fs::remove_dir_all(nest.join("deep")).unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N73 B3: a clean nest with local commits no remote-tracking ref holds
    // is custody, and the custody (and its receipt line) counts them.
    #[test]
    fn a_clean_nest_with_unpushed_commits_is_custody_and_names_the_count() {
        let root = fresh("bulkload-unpushed-nest");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/inner");
        committed(&nest, b"inner");
        output(git(&nest).args([
            "remote",
            "add",
            "origin",
            "https://example.invalid/inner.git",
        ]))
        .unwrap();
        let pushed = head_of(&nest);
        output(git(&nest).args(["update-ref", "refs/remotes/origin/main", &pushed])).unwrap();
        for step in ["one", "two"] {
            fs::write(nest.join("tracked"), step).unwrap();
            output(git(&nest).args(["add", "tracked"])).unwrap();
            commit(&nest, step);
        }
        let expected = vec![NestedRepository {
            unpushed: 2,
            remotes: true,
            ..observed(
                &source,
                directory_nest(b"vendor/inner", Some(head_of(&nest))),
            )
        }];
        assert_eq!(nested_repositories(&source).unwrap(), expected);
        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        assert_eq!(export.nested_repositories, expected);
        assert_eq!(
            nested_sidecar(&capture.join("repository.git")),
            Some(expected.clone())
        );
        let line = expected.first().unwrap().receipt_line();
        assert!(line.contains("path=\"vendor/inner\""));
        assert!(line.contains(" unpushed=2 remotes=yes"));
        // Pushing moves the key: unpushed is custody, not decoration.
        let key = reusable_capture_key(&source).unwrap();
        output(git(&nest).args(["update-ref", "refs/remotes/origin/main", &head_of(&nest)]))
            .unwrap();
        assert_ne!(key, reusable_capture_key(&source).unwrap());
        assert_eq!(
            nested_repositories(&source)
                .unwrap()
                .iter()
                .map(|nest| nest.unpushed)
                .collect::<Vec<_>>(),
            vec![0]
        );
        fs::remove_dir_all(root).unwrap();
    }

    // R-N73 B3 / F8: a nest with no remote says so, and a path holding a
    // newline or a quote renders as one escaped line.
    #[test]
    fn nest_receipt_lines_escape_paths_and_say_when_there_is_no_remote() {
        let nest = NestedRepository {
            rel_path: b"odd\n\"name\xff".to_vec(),
            ..directory_nest(b"", None)
        };
        let line = nest.receipt_line();
        assert_eq!(line.lines().count(), 1);
        assert_eq!(
            line,
            "nested-repository path=\"odd\\n\\\"name\\xff\" kind=Directory gitdir=Directory admin=\"\" head=unborn unpushed=all(0) remotes=none ignored-carried=0"
        );
        let gitlink = NestedRepository {
            rel_path: b"lib/vendor".to_vec(),
            kind: NestedRepositoryKind::Gitlink,
            head_oid: Some("0".repeat(40)),
            gitdir_kind: GitdirKind::None,
            admin: Vec::new(),
            unpushed: 0,
            remotes: false,
            ignored_carried: 0,
            own_item: false,
        };
        assert_eq!(
            gitlink.receipt_line(),
            format!(
                "nested-repository path=\"lib/vendor\" kind=Gitlink head={}",
                "0".repeat(40)
            )
        );
    }

    // R-N73 F7: a `.git` DIRECTORY whose common directory is this repository
    // (a hand-built `commondir` administration) is not foreign custody. The
    // pointer-file branch already refused this; the directory branch did not.
    #[test]
    fn a_dot_git_directory_resolving_to_this_repository_refuses() {
        let root = fresh("bulkload-dotgit-dir-same-repository");
        let source = root.join("outer");
        committed(&source, b"outer");
        let nest = source.join("vendor/alias");
        fs::create_dir_all(nest.join(".git")).unwrap();
        fs::write(
            nest.join(".git/commondir"),
            format!("{}\n", source.join(".git").display()),
        )
        .unwrap();
        fs::write(nest.join(".git/HEAD"), format!("{}\n", head_of(&source))).unwrap();
        let seen = text(git(&nest).args([
            "--git-dir=.git",
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ]))
        .unwrap();
        assert_eq!(
            fs::canonicalize(seen).unwrap(),
            fs::canonicalize(source.join(".git")).unwrap()
        );
        refuses_everywhere(&source, &root.join("capture"));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N32, measured on neo: medical-massage-specialists-infra refused on
    // .terraform-data/edge-security/modules/zone_custom_ruleset/.git, a
    // Terraform module cache under an ignored directory. It is a foreign
    // repository: custody, not malformed inventory.
    #[test]
    fn foreign_repository_under_an_ignored_cache_is_typed_custody_not_refusal() {
        let root =
            std::env::temp_dir().join(format!("bulkload-foreign-cache-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("infra");
        committed_repository(&source, b"infra");
        fs::write(source.join(".gitignore"), b".terraform-data/\n").unwrap();
        output(git(&source).args(["add", ".gitignore"])).unwrap();
        output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-m", "ignore"]))
            .unwrap();
        let rel = b".terraform-data/edge-security/modules/zone_custom_ruleset";
        let module = source.join(std::str::from_utf8(rel).unwrap());
        committed_repository(&module, b"module");
        fs::write(
            source.join(".terraform-data/edge-security/lock"),
            b"carried",
        )
        .unwrap();
        assert!(module.join(".git").is_dir());

        // Cache-like and clean: custody, with no remote to compare against.
        let custody = nested_repositories(&source).unwrap();
        assert_eq!(
            custody,
            vec![observed(
                &source,
                directory_nest(rel, Some(head_of(&module)))
            )]
        );
        assert!(custody
            .first()
            .unwrap()
            .receipt_line()
            .ends_with(" unpushed=all(1) remotes=none ignored-carried=0"));
        assert!(nested_worktrees(&source).unwrap().is_empty());
        let key = reusable_capture_key(&source).unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        // R-N73: custody only while the nest is clean. Dirty or untracked
        // nested bytes are not carried, so the capture refuses rather than
        // lose them; the nested HEAD moving is a new key.
        fs::write(module.join("tracked"), b"changed nested bytes").unwrap();
        fs::write(module.join("untracked"), b"more nested bytes").unwrap();
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        output(git(&module).args(["add", "."])).unwrap();
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        output(git(&module).args(["-c", "commit.gpgsign=false", "commit", "-m", "moved"])).unwrap();
        assert_ne!(key, reusable_capture_key(&source).unwrap());
        let key = reusable_capture_key(&source).unwrap();

        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        let expected = vec![observed(
            &source,
            directory_nest(rel, Some(head_of(&module))),
        )];
        assert_eq!(export.nested_repositories, expected);
        let private = capture.join("repository.git");
        assert_eq!(nested_sidecar(&private), Some(expected.clone()));
        let paths = manifest_paths(&private);
        assert!(paths.contains(&b".terraform-data".to_vec()));
        assert!(paths.contains(&b".terraform-data/edge-security/lock".to_vec()));
        assert!(paths.contains(&b".terraform-data/edge-security/modules".to_vec()));
        assert!(!paths.iter().any(|path| path.starts_with(rel)));
        assert!(!carried_paths(&private).contains("zone_custom_ruleset"));

        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        assert_eq!(fs::read(restored.join("tracked")).unwrap(), b"infra");
        assert!(restored
            .join(".terraform-data/edge-security/modules")
            .is_dir());
        assert!(!restored
            .join(".terraform-data/edge-security/modules/zone_custom_ruleset")
            .exists());
        assert_eq!(imported_nested_custody(&restored), Some(expected));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N32, measured on neo: asfirewire-legalab holds googletest checkouts
    // under two CMake build trees and a SwiftPM checkout under DerivedData.
    // Every one is recorded; nothing below any of them is walked.
    #[test]
    fn three_foreign_repositories_at_different_depths_are_all_recorded_and_none_descended() {
        let root =
            std::env::temp_dir().join(format!("bulkload-foreign-depths-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("legalab");
        committed_repository(&source, b"legalab");
        let nests: [&[u8]; 3] = [
            b"build/DerivedData/SourcePackages/checkouts/eventsource",
            b"build/tests_a15/_deps/googletest-src",
            b"build/tests_build/_deps/googletest-src",
        ];
        for rel in &nests[1..] {
            let nest = source.join(std::str::from_utf8(rel).unwrap());
            committed_repository(&nest, b"gtest");
            // Committed, so the nest stays clean (R-N73) and still holds bytes
            // the enclosing census must never list.
            fs::write(nest.join("sentinel"), b"never a seat of the enclosing item").unwrap();
            output(git(&nest).args(["add", "sentinel"])).unwrap();
            output(git(&nest).args(["-c", "commit.gpgsign=false", "commit", "-m", "s"])).unwrap();
        }
        // An unborn HEAD (init, no commit, no files) is clean custody with no
        // oid, not a refusal.
        let unborn = source.join(std::str::from_utf8(nests[0]).unwrap());
        fs::create_dir_all(&unborn).unwrap();
        output(git(&unborn).args(["init", "--template="])).unwrap();
        fs::write(source.join("build/tests_build/CMakeCache.txt"), b"carried").unwrap();

        let custody = nested_repositories(&source).unwrap();
        assert_eq!(
            custody,
            vec![
                observed(&source, directory_nest(nests[0], None)),
                observed(
                    &source,
                    directory_nest(
                        nests[1],
                        Some(head_of(
                            &source.join("build/tests_a15/_deps/googletest-src")
                        ))
                    )
                ),
                observed(
                    &source,
                    directory_nest(
                        nests[2],
                        Some(head_of(
                            &source.join("build/tests_build/_deps/googletest-src")
                        ))
                    )
                ),
            ]
        );
        let common = common_repository(&source).unwrap();
        let census = capture_census(&source, &common, CapturePolicy::default()).unwrap();
        assert_eq!(census.nested_repositories, custody);
        let rows: Vec<&[u8]> = census
            .rows
            .iter()
            .map(|row| row.rel_path.as_slice())
            .collect();
        assert!(rows.contains(&b"build".as_slice()));
        assert!(rows.contains(&b"build/tests_build/_deps".as_slice()));
        assert!(rows.contains(&b"build/tests_build/CMakeCache.txt".as_slice()));
        assert!(rows.contains(&b"build/DerivedData/SourcePackages/checkouts".as_slice()));
        assert!(!rows
            .iter()
            .any(|row| nests.iter().any(|nest| row.starts_with(nest))));
        assert!(!rows.iter().any(|row| row.ends_with(b"sentinel")));

        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        assert_eq!(export.nested_repositories, custody);
        let private = capture.join("repository.git");
        assert_eq!(nested_sidecar(&private), Some(custody));
        let carried = carried_paths(&private);
        assert!(carried.contains("build/tests_build/CMakeCache.txt"));
        assert!(!carried.contains("sentinel"));
        assert!(!carried.contains("googletest-src"));
        assert!(!carried.contains("eventsource"));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N32, measured on neo: crs310-8g-2s-in holds one 160000 gitlink. It is
    // custody, and it stays in the staged tree the bundle writes (R-N73, B2):
    // Git bundles a gitlink to a commit it does not hold, and stripping it made
    // a committed submodule restore as a staged deletion.
    #[test]
    fn a_gitlink_index_entry_is_custody_and_kept_in_the_staged_tree() {
        let root = std::env::temp_dir().join(format!("bulkload-gitlink-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("crs310");
        committed_repository(&source, b"firmware");
        let commit = head_of(&source);
        // An uninitialized submodule: a gitlink in the index, an empty directory
        // on disk, exactly what `git clone` without --recurse-submodules leaves.
        fs::create_dir_all(source.join("lib/vendor")).unwrap();
        let gitlink_oid = commit.clone();
        output(git(&source).args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit},lib/vendor"),
        ]))
        .unwrap();
        let gitlink = NestedRepository {
            rel_path: b"lib/vendor".to_vec(),
            kind: NestedRepositoryKind::Gitlink,
            head_oid: Some(commit.clone()),
            gitdir_kind: GitdirKind::None,
            admin: Vec::new(),
            unpushed: 0,
            remotes: false,
            ignored_carried: 0,
            own_item: false,
        };
        let (_, before_index, gitlinks) = source_index(&source).unwrap();
        assert_eq!(gitlinks, vec![gitlink.clone()]);
        assert_eq!(nested_repositories(&source).unwrap(), vec![gitlink.clone()]);
        let key = reusable_capture_key(&source).unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());

        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        assert_eq!(export.nested_repositories, vec![gitlink.clone()]);
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        // The source index is byte-identical: the gitlink left a private copy.
        let (index_path, after_index, _) = source_index(&source).unwrap();
        assert_eq!(before_index, after_index);
        assert_eq!(fs::read(index_path).unwrap(), before_index);
        let private = capture.join("repository.git");
        let staged = String::from_utf8(
            output(git(&private).args(["ls-tree", "-r", "refs/carry-export/staged"])).unwrap(),
        )
        .unwrap();
        assert!(staged.contains("tracked"));
        assert!(staged.contains(&format!("160000 commit {commit}\tlib/vendor")));
        assert_eq!(nested_sidecar(&private), Some(vec![gitlink.clone()]));
        // The empty submodule seat is an ordinary directory row; no contents.
        let paths = manifest_paths(&private);
        assert!(paths.contains(&b"lib/vendor".to_vec()));
        assert!(!paths.iter().any(|path| path.starts_with(b"lib/vendor/")));

        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        assert_eq!(fs::read(restored.join("tracked")).unwrap(), b"firmware");
        assert!(restored.join("lib/vendor").is_dir());
        assert!(fs::read_dir(restored.join("lib/vendor"))
            .unwrap()
            .next()
            .is_none());
        let index =
            String::from_utf8(output(git(&restored).args(["ls-files", "--stage"])).unwrap())
                .unwrap();
        assert!(index.contains("tracked"));
        assert!(index.contains(&format!("160000 {gitlink_oid} 0\tlib/vendor")));
        assert_eq!(imported_nested_custody(&restored), Some(vec![gitlink]));
        // A same-HEAD index repair reads the staged tree, gitlink included.
        fs::remove_file(restored.join(".git/index")).unwrap();
        repair_missing_index(&export.bundle, &restored, "neo", &root.join("repair")).unwrap();
        let index =
            String::from_utf8(output(git(&restored).args(["ls-files", "--stage"])).unwrap())
                .unwrap();
        assert!(index.contains(&format!("160000 {gitlink_oid} 0\tlib/vendor")));
        fs::remove_dir_all(root).unwrap();
    }

    // Custody is for repositories Git can read. A symlink named .git, an
    // unresolvable pointer, and a directory named .git that is not a
    // repository are malformed inventory, exactly as before R-N32.
    #[test]
    fn a_symlinked_dot_git_still_refuses() {
        let root =
            std::env::temp_dir().join(format!("bulkload-symlinked-git-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("outer");
        committed_repository(&source, b"outer");
        let linked = source.join("linked");
        fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink(source.join(".git"), linked.join(".git")).unwrap();
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        assert_eq!(
            export_repository(&source, &root.join("capture-symlink")),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        assert_eq!(
            nested_repositories(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        fs::remove_dir_all(&linked).unwrap();
        // A pointer file that resolves to nothing.
        let stray = source.join("stray");
        fs::create_dir(&stray).unwrap();
        fs::write(
            stray.join(".git"),
            format!(
                "gitdir: {}\n",
                source.join(".git/worktrees/missing").display()
            ),
        )
        .unwrap();
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        fs::remove_dir_all(&stray).unwrap();
        // A directory named .git that Git cannot read as a repository.
        let hollow = source.join("hollow");
        fs::create_dir_all(hollow.join(".git")).unwrap();
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        fs::remove_dir_all(&hollow).unwrap();
        assert!(nested_repositories(&source).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    // A repository with no foreign nest and no gitlink hashes and encodes
    // exactly as it did before R-N32: the sidecar is only hashed and only
    // written when non-empty, no RowSchema field or variant was added, and
    // the estate Capture record gained no field.
    #[test]
    fn a_repository_without_nested_repositories_keeps_its_exact_capture_key() {
        let root = std::env::temp_dir().join(format!("bulkload-no-nests-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        rebuildable_fixture(&source);
        fs::create_dir_all(source.join("vendor/plain")).unwrap();
        fs::write(source.join("vendor/plain/file"), b"no administration here").unwrap();
        let (_, _, gitlinks) = source_index(&source).unwrap();
        assert!(gitlinks.is_empty());
        assert!(nested_repositories(&source).unwrap().is_empty());
        let common = common_repository(&source).unwrap();
        let census = capture_census(&source, &common, CapturePolicy::default()).unwrap();
        assert!(census.nested_repositories.is_empty());
        assert!(census.nested_worktrees.is_empty());
        assert_eq!(census.rows, filesystem_rows(&source).unwrap());
        assert_eq!(
            reusable_capture_key(&source).unwrap(),
            reusable_capture_key_with_policy(&source, CapturePolicy::including_rebuildable())
                .unwrap()
        );
        let export = export_repository_with_policy(
            &source,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        assert!(export.nested_repositories.is_empty());
        let private = root.join("capture/repository.git");
        assert_eq!(nested_sidecar(&private), None);
        assert!(!sidecar_present(&private));
        assert!(carried_paths(&private).contains("vendor/plain/file"));
        fs::remove_dir_all(root).unwrap();
    }

    // #595 custody is unchanged by R-N32: a registered worktree of the same
    // repository is a NestedWorktree, never a NestedRepository. A registered
    // worktree of a DIFFERENT repository placed inside the checkout, which
    // #595 refused, is now a foreign nested repository found by pointer file.
    #[test]
    fn nested_worktree_of_the_same_repository_is_still_nested_worktree_custody_not_repository_custody(
    ) {
        let root =
            std::env::temp_dir().join(format!("bulkload-same-vs-foreign-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("outer");
        let other = root.join("other");
        committed_repository(&source, b"outer");
        committed_repository(&other, b"other");
        fs::create_dir_all(source.join(".claude/worktrees")).unwrap();
        let own = source.join(".claude/worktrees/agent-x");
        output(
            git(&source)
                .args(["worktree", "add", "-b", "agent-x"])
                .arg(&own),
        )
        .unwrap();
        let foreign = source.join(".claude/worktrees/foreign");
        output(
            git(&other)
                .args(["worktree", "add", "-b", "foreign"])
                .arg(&foreign),
        )
        .unwrap();
        assert!(own.join(".git").is_file());
        assert!(foreign.join(".git").is_file());
        fs::write(foreign.join("sentinel"), b"belongs to other").unwrap();
        output(git(&foreign).args(["add", "sentinel"])).unwrap();
        output(git(&foreign).args(["-c", "commit.gpgsign=false", "commit", "-m", "s"])).unwrap();

        assert_eq!(
            nested_worktrees(&source).unwrap(),
            vec![NestedWorktree {
                rel_path: b".claude/worktrees/agent-x".to_vec(),
                worktree_name: "agent-x".to_owned(),
                head_oid: head_of(&own),
            }]
        );
        let expected = vec![observed(
            &source,
            NestedRepository {
                gitdir_kind: GitdirKind::PointerFile,
                ..directory_nest(b".claude/worktrees/foreign", Some(head_of(&foreign)))
            },
        )];
        assert_eq!(nested_repositories(&source).unwrap(), expected);
        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        assert_eq!(export.nested_repositories, expected);
        let private = capture.join("repository.git");
        assert_eq!(nested_sidecar(&private), Some(expected));
        let worktrees: Vec<NestedWorktree> = postcard::from_bytes(
            &output(git(&private).args([
                "show",
                &format!("refs/carry-export/{NESTED_WORKTREES_METADATA}:value"),
            ]))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            worktrees
                .iter()
                .map(|worktree| worktree.worktree_name.as_str())
                .collect::<Vec<_>>(),
            vec!["agent-x"]
        );
        let paths = manifest_paths(&private);
        assert!(paths.contains(&b".claude/worktrees".to_vec()));
        assert!(!paths
            .iter()
            .any(|path| path.starts_with(b".claude/worktrees/")));
        assert!(!carried_paths(&private).contains("sentinel"));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N32, measured on neo: printstack's uv cache holds repositories under
    // .tmp/uv-cache/git-v0/db/*/.git and checkouts/*/*/.git. No seat below a
    // nest, on the census or in the filesystem-v1 manifest; the parents stay.
    #[test]
    fn filesystem_rows_never_list_seats_below_a_nested_repository() {
        let root = std::env::temp_dir().join(format!("bulkload-uv-cache-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("printstack");
        committed_repository(&source, b"printstack");
        let db = source.join(".tmp/uv-cache/git-v0/db/0a1b2c");
        let checkout = source.join(".tmp/uv-cache/git-v0/checkouts/0a1b2c/3d4e5f");
        committed_repository(&db, b"db");
        committed_repository(&checkout, b"checkout");
        fs::create_dir_all(db.join("deep/er")).unwrap();
        fs::write(db.join("deep/er/leaf"), b"below a nest").unwrap();
        fs::write(checkout.join("pyproject.toml"), b"below a nest").unwrap();
        // Committed: a cache checkout is custody only while clean (R-N73).
        for nest in [&db, &checkout] {
            output(git(nest).args(["add", "."])).unwrap();
            output(git(nest).args(["-c", "commit.gpgsign=false", "commit", "-m", "s"])).unwrap();
        }
        fs::write(source.join(".tmp/uv-cache/CACHEDIR.TAG"), b"carried").unwrap();

        let common = common_repository(&source).unwrap();
        let census = capture_census(&source, &common, CapturePolicy::default()).unwrap();
        assert_eq!(census.nested_repositories.len(), 2);
        let capture = root.join("capture");
        export_repository_with_policy(&source, &capture, None, CapturePolicy::default()).unwrap();
        let manifest = manifest_paths(&capture.join("repository.git"));
        let census_paths: Vec<Vec<u8>> = census.rows.into_iter().map(|row| row.rel_path).collect();
        assert_eq!(census_paths, manifest);
        for paths in [&census_paths, &manifest] {
            assert!(paths.contains(&b".tmp/uv-cache/CACHEDIR.TAG".to_vec()));
            assert!(paths.contains(&b".tmp/uv-cache/git-v0/db".to_vec()));
            assert!(paths.contains(&b".tmp/uv-cache/git-v0/checkouts/0a1b2c".to_vec()));
            assert!(!paths
                .iter()
                .any(|path| path.starts_with(b".tmp/uv-cache/git-v0/db/0a1b2c")
                    || path.starts_with(b".tmp/uv-cache/git-v0/checkouts/0a1b2c/3d4e5f")));
            assert!(!paths
                .iter()
                .any(|path| path.ends_with(b"leaf") || path.ends_with(b"pyproject.toml")));
        }
        // The attach-side census (no custody) still refuses a nested .git: a
        // restored payload never contains one, so this is a divergence signal.
        assert_eq!(
            filesystem_rows(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Restore side: applying a bundle with nested-repository custody yields a
    // worktree without the nest, and the imported metadata ref names it.
    #[test]
    fn restoring_nested_repository_custody_leaves_the_nest_absent_and_names_it() {
        let root =
            std::env::temp_dir().join(format!("bulkload-restore-nest-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        committed_repository(&source, b"source");
        let nest = source.join("vendor/inner");
        committed_repository(&nest, b"inner");
        fs::write(source.join("vendor/README"), b"carried").unwrap();
        let expected = vec![observed(
            &source,
            directory_nest(b"vendor/inner", Some(head_of(&nest))),
        )];
        let bundle = export_repository(&source, &root.join("capture")).unwrap();

        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        assert_eq!(fs::read(restored.join("tracked")).unwrap(), b"source");
        assert_eq!(
            fs::read(restored.join("vendor/README")).unwrap(),
            b"carried"
        );
        assert!(restored.join("vendor").is_dir());
        assert!(!restored.join("vendor/inner").exists());
        assert_eq!(imported_nested_custody(&restored), Some(expected.clone()));

        let linked = root.join("linked");
        restore_linked(&bundle, &restored, &linked, "neo").unwrap();
        assert_eq!(fs::read(linked.join("vendor/README")).unwrap(), b"carried");
        assert!(!linked.join("vendor/inner").exists());
        assert_eq!(imported_nested_custody(&restored), Some(expected));
        fs::remove_dir_all(root).unwrap();
    }

    fn rebuildable_fixture(repo: &Path) {
        fs::create_dir_all(repo).unwrap();
        output(git(repo).args(["init", "--template="])).unwrap();
        output(git(repo).args(["config", "user.name", "Test"])).unwrap();
        output(git(repo).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(repo.join("tracked"), b"source of truth").unwrap();
        fs::write(repo.join(".gitignore"), b"/target\n/node_modules\n").unwrap();
        output(git(repo).args(["add", "."])).unwrap();
        output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        // An ignored file outside the rebuildable set is still carried: the
        // ruling keeps untracked AND ignored carry, minus a fixed list.
        fs::write(repo.join("ignored-but-carried"), b"operator state").unwrap();
    }

    fn rebuildable_repository(repo: &Path) {
        rebuildable_fixture(repo);
        fs::create_dir_all(repo.join("target/debug/incremental")).unwrap();
        fs::write(
            repo.join("target/debug/incremental/artifact"),
            vec![7u8; 4096],
        )
        .unwrap();
        fs::write(repo.join("target/.rustc_info.json"), b"{\"rustc\":0}").unwrap();
        fs::create_dir_all(repo.join("crates/inner/node_modules/left-pad")).unwrap();
        fs::write(
            repo.join("crates/inner/node_modules/left-pad/index.js"),
            vec![b'x'; 512],
        )
        .unwrap();
        fs::write(repo.join("crates/inner/kept.rs"), b"carried").unwrap();
    }

    fn sidecar_present(private: &Path) -> bool {
        git(private)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/carry-export/{REBUILDABLE_METADATA}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    fn carried_paths(private: &Path) -> String {
        String::from_utf8(
            output(git(private).args([
                "ls-tree",
                "-r",
                "--name-only",
                "refs/carry-export/worktree",
            ]))
            .unwrap(),
        )
        .unwrap()
    }

    fn manifest_paths(private: &Path) -> Vec<Vec<u8>> {
        let seats: Vec<crate::RowSchema> = postcard::from_bytes(
            &output(git(private).args(["show", "refs/carry-export/filesystem-v1:value"])).unwrap(),
        )
        .unwrap();
        seats.into_iter().map(|row| row.rel_path).collect()
    }

    // The measured failure this change exists for: a capture of a repository
    // holding target/ and node_modules/ carried 34 GB of rebuildable bytes and
    // was then refused outright because cargo rewrote one of them mid-pass.
    #[test]
    fn rebuildable_roots_are_omitted_recorded_and_never_carried() {
        let root =
            std::env::temp_dir().join(format!("bulkload-rebuildable-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        rebuildable_repository(&source);

        // (b) Custody names each omitted root and its size, before any capture.
        let custody = rebuildable_omissions(&source).unwrap();
        assert_eq!(
            custody
                .iter()
                .map(|row| row.rel_path.clone())
                .collect::<Vec<_>>(),
            vec![b"crates/inner/node_modules".to_vec(), b"target".to_vec()]
        );
        let sizes: Vec<(u64, u64)> = custody.iter().map(|row| (row.bytes, row.entries)).collect();
        assert_eq!(sizes.first(), Some(&(512, 2)));
        assert_eq!(sizes.get(1).map(|row| row.0), Some(4096 + 11));
        assert!(sizes.get(1).is_some_and(|row| row.1 >= 4));

        // Rebuildable churn no longer moves the key. This is exactly the write
        // that refused tonight's capture with GIT_AUTHORITY_CHANGED.
        let key = reusable_capture_key(&source).unwrap();
        fs::write(
            source.join("target/.rustc_info.json"),
            b"{\"rustc\":1,\"x\":2}",
        )
        .unwrap();
        fs::write(source.join("target/debug/incremental/fresh"), b"mid-pass").unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());

        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        let private = capture.join("repository.git");

        // (a) None of those bytes are carried, and no seat names them.
        let carried = carried_paths(&private);
        assert!(carried.contains("tracked"));
        assert!(carried.contains("ignored-but-carried"));
        assert!(carried.contains("crates/inner/kept.rs"));
        assert!(!carried.contains("target"));
        assert!(!carried.contains("node_modules"));
        let paths = manifest_paths(&private);
        assert!(paths.contains(&b"crates/inner".to_vec()));
        assert!(paths.contains(&b"ignored-but-carried".to_vec()));
        assert!(!paths
            .iter()
            .any(|path| path.starts_with(b"target")
                || path.windows(12).any(|w| w == b"node_modules")));

        // (b) The bundle's own sidecar carries the same custody.
        let manifest: Vec<RebuildableOmission> = postcard::from_bytes(
            &output(git(&private).args([
                "show",
                &format!("refs/carry-export/{REBUILDABLE_METADATA}:value"),
            ]))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest, export.omitted);
        assert_eq!(
            manifest
                .iter()
                .map(|row| row.rel_path.clone())
                .collect::<Vec<_>>(),
            vec![b"crates/inner/node_modules".to_vec(), b"target".to_vec()]
        );
        assert!(manifest.iter().all(|row| row.bytes > 0));

        // A restore leaves the rebuildable roots absent; `cargo build` remakes them.
        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        assert_eq!(
            fs::read(restored.join("tracked")).unwrap(),
            b"source of truth"
        );
        assert_eq!(
            fs::read(restored.join("ignored-but-carried")).unwrap(),
            b"operator state"
        );
        assert!(!restored.join("target").exists());
        assert!(!restored.join("crates/inner/node_modules").exists());
        assert!(restored.join("crates/inner/kept.rs").is_file());

        fs::remove_dir_all(root).unwrap();
    }

    // (d) --include-rebuildable opts back in to full fidelity: every omitted
    // byte is carried again and no custody sidecar is written.
    #[test]
    fn include_rebuildable_carries_the_whole_rebuildable_set() {
        let root = std::env::temp_dir().join(format!("bulkload-full-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        rebuildable_repository(&source);
        let capture = root.join("capture");
        let export = export_repository_with_policy(
            &source,
            &capture,
            None,
            CapturePolicy::including_rebuildable(),
        )
        .unwrap();
        assert!(export.omitted.is_empty());
        let private = capture.join("repository.git");
        let carried = carried_paths(&private);
        assert!(carried.contains("target/.rustc_info.json"));
        assert!(carried.contains("target/debug/incremental/artifact"));
        assert!(carried.contains("crates/inner/node_modules/left-pad/index.js"));
        assert!(!sidecar_present(&private));
        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        assert_eq!(
            fs::read(restored.join("crates/inner/node_modules/left-pad/index.js")).unwrap(),
            vec![b'x'; 512]
        );
        fs::remove_dir_all(root).unwrap();
    }

    // A name on the list is only rebuildable when Git tracks nothing beneath
    // it. A repository that really does track `target/...` keeps every byte.
    #[test]
    fn a_tracked_rebuildable_name_is_still_carried_in_full() {
        let root =
            std::env::temp_dir().join(format!("bulkload-tracked-target-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        fs::create_dir_all(source.join("target")).unwrap();
        fs::create_dir_all(source.join("venv")).unwrap();
        rebuildable_fixture(&source);
        fs::write(
            source.join("target/committed.txt"),
            b"this repo tracks target",
        )
        .unwrap();
        fs::write(source.join("venv/scratch"), b"nothing tracked here").unwrap();
        output(git(&source).args(["add", "-f", "target/committed.txt"])).unwrap();
        output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-m", "target"]))
            .unwrap();

        let custody = rebuildable_omissions(&source).unwrap();
        assert_eq!(
            custody
                .iter()
                .map(|row| row.rel_path.clone())
                .collect::<Vec<_>>(),
            vec![b"venv".to_vec()]
        );
        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        let carried = carried_paths(&capture.join("repository.git"));
        assert!(carried.contains("target/committed.txt"));
        assert!(!carried.contains("venv/scratch"));
        assert_eq!(export.omitted.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    // (c) A repository with nothing rebuildable in it hashes and encodes
    // exactly as it did before omission existed: the omission sidecar is only
    // hashed and only written when it is non-empty, and the full-fidelity
    // policy is byte-for-byte the pre-change code path. No RowSchema field or
    // variant was added, so every retained capture key stays valid.
    #[test]
    fn a_repository_without_rebuildable_roots_keeps_its_exact_capture_key() {
        let root = std::env::temp_dir().join(format!("bulkload-unchanged-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        rebuildable_fixture(&source);
        fs::create_dir_all(source.join("src/build")).unwrap();
        fs::write(
            source.join("src/build/kept"),
            b"build and dist are not on the list",
        )
        .unwrap();
        fs::create_dir_all(source.join("dist")).unwrap();
        fs::write(source.join("dist/kept"), b"carried").unwrap();
        // A bazel convenience symlink is one row and is never descended.
        std::os::unix::fs::symlink("/nonexistent/output-base/out", source.join("bazel-out"))
            .unwrap();

        assert!(rebuildable_omissions(&source).unwrap().is_empty());
        assert_eq!(
            reusable_capture_key(&source).unwrap(),
            reusable_capture_key_with_policy(&source, CapturePolicy::including_rebuildable())
                .unwrap()
        );
        let common = common_repository(&source).unwrap();
        let census = capture_census(&source, &common, CapturePolicy::default()).unwrap();
        assert!(census.nested_worktrees.is_empty());
        assert!(census.omitted.is_empty());
        assert_eq!(census.rows, filesystem_rows(&source).unwrap());
        assert_eq!(
            postcard::to_allocvec(&census.rows).unwrap(),
            postcard::to_allocvec(&filesystem_rows(&source).unwrap()).unwrap()
        );

        let export = export_repository_with_policy(
            &source,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        assert!(export.omitted.is_empty());
        let private = root.join("capture/repository.git");
        // No sidecar ref exists at all, so the bundle is the one it always was.
        assert!(!sidecar_present(&private));
        let carried = carried_paths(&private);
        assert!(carried.contains("src/build/kept"));
        assert!(carried.contains("dist/kept"));
        assert!(carried.contains("bazel-out"));
        fs::remove_dir_all(root).unwrap();
    }

    // ---- drift tolerance (R25, bulkload #34; R-N28/R-N30/R-N72; R-N29 deferred to bulkload#48) ----

    fn drift_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("bulkload-drift-{name}-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        committed_repository(&source, b"tracked bytes at census");
        fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(source.join("ignored"), b"ignored but carried").unwrap();
        fs::create_dir(source.join("dir")).unwrap();
        fs::write(source.join("dir/untracked"), b"untracked payload").unwrap();
        (root, source)
    }

    fn drift_ref_present(private: &Path) -> bool {
        git(private)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/carry-export/{CAPTURE_DRIFT_METADATA}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    fn drift_sidecar(private: &Path) -> CaptureDrift {
        postcard::from_bytes(
            &output(git(private).args([
                "show",
                &format!("refs/carry-export/{CAPTURE_DRIFT_METADATA}:value"),
            ]))
            .unwrap(),
        )
        .unwrap()
    }

    fn row(kind: DriftKind, name: &[u8]) -> DriftRow {
        DriftRow {
            kind,
            name: name.to_vec(),
        }
    }

    // Refusal #2 of 9/21-22: four sibling-worktree lanes created branches in
    // the shared ref store while a capture was in flight.
    #[test]
    fn refs_created_elsewhere_during_capture_are_drift_not_refusal() {
        let (root, source) = drift_fixture("refs-elsewhere");
        let head = text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap();
        let index_before = fs::read(source.join(".git/index")).unwrap();
        let (lane, tip) = (source.clone(), head.clone());
        mid_pass::arm(&source, move || {
            for number in 0..4 {
                output(git(&lane).args(["update-ref", &format!("refs/heads/lane-{number}"), &tip]))
                    .unwrap();
            }
        });
        let capture = root.join("capture");
        let export =
            export_repository_with_drift(&source, &capture, &ExportOptions::default()).unwrap();
        assert_eq!(
            export.drift.rows,
            (0..4)
                .map(|number| row(
                    DriftKind::RefAdded,
                    format!("refs/heads/lane-{number}").as_bytes()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            head,
            text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap()
        );
        assert_eq!(index_before, fs::read(source.join(".git/index")).unwrap());
        output(git(&source).args(["bundle", "verify"]).arg(&export.bundle)).unwrap();
        let private = capture.join("repository.git");
        assert_eq!(drift_sidecar(&private), export.drift);
        // The lanes' refs were not in the captured inventory and are not in the bundle.
        let heads = text(
            git(&source)
                .args(["bundle", "list-heads"])
                .arg(&export.bundle),
        )
        .unwrap();
        assert!(!heads.contains("lane-"));
        // The pre-drift contract is untouched for callers that never asked for tolerance.
        let again = source.clone();
        mid_pass::arm(&source, move || {
            output(git(&again).args(["update-ref", "refs/heads/lane-4", "HEAD"])).unwrap();
        });
        assert!(matches!(
            export_repository(&source, &root.join("strict")),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // Refusal #1 of 9/21-22: cargo rewrote .rustc_info.json between the census
    // and that seat's byte pass.
    #[test]
    fn a_file_rewritten_during_the_raw_pass_is_drift_not_refusal() {
        let (root, source) = drift_fixture("file-rewritten");
        let rewritten = source.join("tracked");
        mid_pass::arm(&source, move || {
            fs::write(&rewritten, b"{\"rustc\":1,\"rewritten\":true}").unwrap();
        });
        let capture = root.join("capture");
        let export =
            export_repository_with_drift(&source, &capture, &ExportOptions::default()).unwrap();
        assert_eq!(
            export.drift.rows,
            vec![row(DriftKind::SeatChanged, b"tracked")]
        );
        let private = capture.join("repository.git");
        // Absent from the tree and from filesystem-v1: the drift list is the
        // statement that the seat's bytes are not here (R-N28).
        assert!(!carried_paths(&private)
            .lines()
            .any(|path| path == "tracked"));
        assert!(!manifest_paths(&private).contains(&b"tracked".to_vec()));
        assert!(carried_paths(&private).contains("dir/untracked"));
        assert!(manifest_paths(&private).contains(&b"ignored".to_vec()));
        output(git(&private).args(["fsck", "--strict", "--no-dangling"])).unwrap();
        output(git(&source).args(["bundle", "verify"]).arg(&export.bundle)).unwrap();
        assert_eq!(drift_sidecar(&private), export.drift);
        fs::remove_dir_all(root).unwrap();
    }

    // The operator-requested test: a build truncating a tracked file, an agent
    // creating a new file, and a lane creating a branch, all under one pass.
    #[test]
    fn concurrent_ref_creation_and_file_mutation_during_one_capture_succeed_with_drift() {
        let (root, source) = drift_fixture("concurrent");
        let inside = source.clone();
        mid_pass::arm(&source, move || {
            fs::write(inside.join("tracked"), b"").unwrap();
            fs::write(inside.join("dir/appeared"), b"created mid-pass").unwrap();
            output(git(&inside).args(["update-ref", "refs/heads/concurrent-lane", "HEAD"]))
                .unwrap();
        });
        let capture = root.join("capture");
        let export =
            export_repository_with_drift(&source, &capture, &ExportOptions::default()).unwrap();
        assert_eq!(
            export.drift.rows,
            vec![
                row(DriftKind::RefAdded, b"refs/heads/concurrent-lane"),
                row(DriftKind::SeatAdded, b"dir/appeared"),
                row(DriftKind::SeatChanged, b"tracked"),
            ]
        );
        assert_eq!(export.drift.len(), 3);
        output(git(&source).args(["bundle", "verify"]).arg(&export.bundle)).unwrap();
        // Capture never writes the source: it is exactly its post-mutation state.
        assert_eq!(fs::read(source.join("tracked")).unwrap(), b"");
        assert_eq!(
            fs::read(source.join("dir/appeared")).unwrap(),
            b"created mid-pass"
        );
        assert_eq!(
            text(git(&source).args(["rev-parse", "--verify", "refs/heads/concurrent-lane"]))
                .unwrap(),
            text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap()
        );
        let private = capture.join("repository.git");
        let carried = carried_paths(&private);
        assert!(!carried.lines().any(|path| path == "tracked"));
        assert!(!carried.contains("appeared"));
        assert!(carried.contains("dir/untracked"));
        // The bundle is not complete, and says so in-band: no restore verb
        // presents it as a workspace (re-review finding 3).
        let restored = root.join("restored");
        assert!(matches!(
            restore_bundle(&export.bundle, &restored, "neo"),
            Err(BulkloadRefusal::CaptureDrifted)
        ));
        assert!(!restored.exists());
        fs::remove_dir_all(root).unwrap();
    }

    // (a)/(b) tolerance must not leak into HEAD (R-N30).
    #[test]
    fn head_moving_during_capture_still_refuses() {
        let (root, source) = drift_fixture("head-moves");
        let inside = source.clone();
        mid_pass::arm(&source, move || {
            output(git(&inside).args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "committed mid-pass",
            ]))
            .unwrap();
        });
        assert!(matches!(
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default()),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // Separability from the index (R-N30): HEAD pinned, index rewritten.
    #[test]
    fn index_rewritten_during_capture_still_refuses() {
        let (root, source) = drift_fixture("index-rewritten");
        let head = text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap();
        let inside = source.clone();
        mid_pass::arm(&source, move || {
            fs::write(inside.join("staged-mid-pass"), b"added to the index").unwrap();
            output(git(&inside).args(["add", "staged-mid-pass"])).unwrap();
        });
        assert!(matches!(
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default()),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        assert_eq!(
            head,
            text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    // The Census split holds: a nested worktree's HEAD is custody, not seats,
    // and it moving under the pass is still a refusal.
    #[test]
    fn nested_worktree_head_moving_during_capture_still_refuses() {
        let (root, source) = drift_fixture("nested-head");
        let nested = source.join(".claude/worktrees/agent-x");
        fs::create_dir_all(source.join(".claude/worktrees")).unwrap();
        output(
            git(&source)
                .args(["worktree", "add", "-b", "agent-x"])
                .arg(&nested),
        )
        .unwrap();
        assert_eq!(nested_worktrees(&source).unwrap().len(), 1);
        let inside = nested;
        mid_pass::arm(&source, move || {
            output(git(&inside).args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "agent committed mid-pass",
            ]))
            .unwrap();
        });
        assert!(matches!(
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default()),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // The no-invalidation proof for every retained bundle: a clean capture has
    // exactly the ref set it had before drift tolerance existed, no drift ref,
    // and a deterministic shape across passes.
    #[test]
    fn a_clean_capture_emits_no_drift_ref_and_keeps_the_pre_change_bundle_shape() {
        let (root, source) = drift_fixture("clean");
        let symbolic = text(git(&source).args(["symbolic-ref", "HEAD"])).unwrap();
        let mut expected: Vec<String> = [
            "configuration-v1",
            "exclude",
            "filesystem-v1",
            "head",
            "head-symbolic",
            "staged",
            "worktree",
        ]
        .iter()
        .map(|name| format!("refs/carry-export/{name}"))
        .chain(std::iter::once(format!("refs/carry-export/{symbolic}")))
        .collect();
        expected.sort();
        let key = reusable_capture_key(&source).unwrap();
        let mut shapes = Vec::new();
        for pass in ["first", "second"] {
            let capture = root.join(pass);
            let export =
                export_repository_with_drift(&source, &capture, &ExportOptions::default()).unwrap();
            assert!(export.drift.is_empty());
            let private = capture.join("repository.git");
            assert!(!drift_ref_present(&private));
            let names: Vec<String> = refs(&private)
                .unwrap()
                .lines()
                .map(|line| line.split_once(' ').unwrap().1.to_owned())
                .collect();
            assert_eq!(names, expected);
            let heads: Vec<String> = text(
                git(&source)
                    .args(["bundle", "list-heads"])
                    .arg(&export.bundle),
            )
            .unwrap()
            .lines()
            .map(|line| line.split_once(' ').unwrap().1.to_owned())
            .collect();
            assert_eq!(heads, expected);
            shapes.push((
                text(git(&private).args(["rev-parse", "refs/carry-export/worktree^{tree}"]))
                    .unwrap(),
                output(git(&private).args(["show", "refs/carry-export/filesystem-v1:value"]))
                    .unwrap(),
                export.bytes_read,
            ));
        }
        assert_eq!(shapes.first(), shapes.last());
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        assert_eq!(
            key,
            legacy::reusable_capture_key_with_policy(&source, CapturePolicy::default()).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Unbounded drift is a rebuild, never a silently truncated drift list.
    #[test]
    fn drift_beyond_the_row_budget_refuses() {
        let (root, source) = drift_fixture("budget");
        fs::create_dir(source.join("burst")).unwrap();
        let burst = source.join("burst");
        mid_pass::arm(&source, move || {
            for number in 0..=DRIFT_ROW_LIMIT {
                fs::File::create(burst.join(format!("{number}"))).unwrap();
            }
        });
        assert!(matches!(
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default()),
            Err(BulkloadRefusal::BudgetExceeded)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // ENOENT is drift; EACCES is a real IO fault that tolerance must not swallow.
    #[test]
    fn a_seat_deleted_mid_pass_is_drift_but_an_unreadable_seat_still_refuses() {
        use std::os::unix::fs::PermissionsExt;
        let (root, source) = drift_fixture("deleted-vs-unreadable");
        fs::write(source.join("doomed"), b"gone before its byte pass").unwrap();
        fs::write(source.join("locked"), b"unreadable before its byte pass").unwrap();
        let doomed = source.join("doomed");
        mid_pass::arm(&source, move || fs::remove_file(&doomed).unwrap());
        let export =
            export_repository_with_drift(&source, &root.join("deleted"), &ExportOptions::default())
                .unwrap();
        assert_eq!(
            export.drift.rows,
            vec![row(DriftKind::SeatRemoved, b"doomed")]
        );
        assert!(!manifest_paths(&root.join("deleted/repository.git")).contains(&b"doomed".to_vec()));
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            // Root (as on the Linux CI runner) reads a mode-000 file, so an
            // unreadable seat cannot be produced by permissions; the refusal
            // half of this test only has meaning for an unprivileged user.
            fs::remove_dir_all(root).unwrap();
            return;
        }
        let locked = source.join("locked");
        let lock = locked.clone();
        mid_pass::arm(&source, move || {
            fs::set_permissions(&lock, fs::Permissions::from_mode(0o000)).unwrap();
        });
        let refused = export_repository_with_drift(
            &source,
            &root.join("unreadable"),
            &ExportOptions::default(),
        );
        assert!(matches!(refused, Err(BulkloadRefusal::Io(Some(_)))));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    fn held_census(private: &Path) -> Vec<crate::RowSchema> {
        postcard::from_bytes(
            &output(git(private).args(["show", "refs/carry-export/filesystem-v1:value"])).unwrap(),
        )
        .unwrap()
    }

    // R-N72 (TIN-4540) finding 2: racy timestamps, exactly as in Git. A seat
    // rewritten at the same size within one timestamp tick of the capture
    // keeps its whole StatIdentity on a coarse-timestamp filesystem, so its
    // identity cannot tell the new bytes from the captured ones. Deterministic
    // (re-review finding 7): the retained pass start and this pass's clock are
    // injected relative to the seat's own stamps, never read off the wall.
    #[test]
    fn a_same_size_rewrite_in_the_capture_tick_is_never_reused_by_identity() {
        let (root, source) = drift_fixture("racy");
        let first =
            export_repository_with_drift(&source, &root.join("first"), &ExportOptions::default())
                .unwrap();
        // The census exactly as a coarse-timestamp filesystem reports it after
        // the rewrite below: every seat at the identity the capture holds.
        let held = held_census(&root.join("first/repository.git"));
        let tracked = held.iter().find(|row| row.rel_path == b"tracked").unwrap();
        let stamp = tracked.mtime_ns.max(tracked.ctime_ns);
        fs::write(source.join("tracked"), b"TRACKED BYTES AT CENSUS").unwrap();
        let second = root.join("second");
        fs::create_dir(&second).unwrap();
        let private = prepare_private(&source, &second).unwrap();
        let reuse_at = |started_ns: i128, now_ns: i128| {
            let retained = RetainedCapture {
                bundle: &first.bundle,
                started_ns,
            };
            reusable_blobs(&private, &retained, &held, now_ns)
                .unwrap()
                .unwrap()
        };
        let later = stamp + 60 * RACY_GRANULARITY_NS;
        // Stamped in the tick the retained pass started in: racy.
        assert!(
            !reuse_at(stamp, later).contains_key(b"tracked".as_slice()),
            "a racy seat is never reused by stat identity"
        );
        assert!(!reuse_at(stamp + RACY_GRANULARITY_NS, later).contains_key(b"tracked".as_slice()));
        // Stamped well before the pass start: reused on identity alone. The
        // guard is Git's racy check, not a blanket disable of R25 reuse.
        assert!(reuse_at(later, later).contains_key(b"tracked".as_slice()));
        // Re-review finding 5: a stamp later than this pass's own clock comes
        // from a clock the pass cannot order against, and fails closed.
        assert!(!reuse_at(later, stamp - 1).contains_key(b"tracked".as_slice()));
        assert!(!refs(&private).unwrap().contains(REUSE_NAMESPACE));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 (TIN-4540) finding 4: a retained capture that cannot be decoded
    // costs only the optimization. Its refs/carry-reuse/* refs are deleted
    // before the pass continues, so `bundle create --all` never carries them.
    #[test]
    fn an_undecodable_retained_capture_degrades_to_no_reuse_and_leaks_no_refs() {
        let (root, source) = drift_fixture("undecodable-retained");
        let first =
            export_repository_with_drift(&source, &root.join("first"), &ExportOptions::default())
                .unwrap();
        let retained = root.join("first/repository.git");
        output(git(&retained).args(["update-ref", "-d", "refs/carry-export/filesystem-v1"]))
            .unwrap();
        metadata(&retained, "filesystem-v1", b"\xff not a postcard census").unwrap();
        let corrupt = root.join("corrupt.bundle");
        output(
            git(&retained)
                .args(["bundle", "create"])
                .arg(&corrupt)
                .arg("--all"),
        )
        .unwrap();
        let capture = root.join("second");
        let export = export_repository_with_drift(
            &source,
            &capture,
            &ExportOptions {
                reuse: Some(RetainedCapture {
                    bundle: &corrupt,
                    started_ns: first.started_ns,
                }),
                ..ExportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            export.reuse_unavailable,
            Some(ReuseUnavailable::RetainedUnreadable)
        );
        assert_eq!(
            export.bytes_read, first.bytes_read,
            "degraded to a full read"
        );
        assert!(!refs(&capture.join("repository.git"))
            .unwrap()
            .contains("refs/carry-reuse/"));
        let heads = text(
            git(&source)
                .args(["bundle", "list-heads"])
                .arg(&export.bundle),
        )
        .unwrap();
        assert!(!heads.contains("refs/carry-reuse/"));
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 (TIN-4540) finding 1, the reviewer's reproduction: the export's
    // own snapshot cannot see a ref deleted before it, but the pre- and
    // post-pass key parts can, and only equal parts are a clean reuse key.
    #[test]
    fn a_ref_deleted_before_the_export_snapshot_is_key_drift() {
        let (root, source) = drift_fixture("adv-stale-key");
        output(git(&source).args(["checkout", "-q", "-b", "side"])).unwrap();
        fs::write(source.join("side-only"), b"unique").unwrap();
        output(git(&source).args(["add", "side-only"])).unwrap();
        output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", "side"]))
            .unwrap();
        let side = text(git(&source).args(["rev-parse", "side"])).unwrap();
        output(git(&source).args(["checkout", "-q", "-"])).unwrap();
        let pre = capture_key_parts(&source).unwrap();
        let recorded_key = pre.digest().unwrap();
        output(git(&source).args(["update-ref", "-d", "refs/heads/side"])).unwrap();
        let export =
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default())
                .unwrap();
        let post = capture_key_parts(&source).unwrap();
        assert!(pre.drift_only(&post));
        assert!(export.drift.is_empty(), "the export alone cannot see it");
        assert_eq!(
            pre.drift_to(&post).unwrap().rows,
            vec![row(DriftKind::RefRemoved, b"refs/heads/side")]
        );
        assert!(post.drift_to(&post).unwrap().is_empty());
        // Why the pre-pass key must never be recorded as clean here: the
        // branch's return at the same commit reproduces it exactly.
        output(git(&source).args(["update-ref", "refs/heads/side", &side])).unwrap();
        assert_eq!(reusable_capture_key(&source).unwrap(), recorded_key);
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 (TIN-4540) finding 4: a shallow checkout's bundle is shallow-graph
    // custody with no worktree tree to reuse, so the pass says so.
    #[test]
    fn a_shallow_checkout_reports_reuse_unavailable_instead_of_silently_rereading() {
        let root = std::env::temp_dir().join(format!(
            "bulkload-drift-shallow-reuse-{}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let full = root.join("full");
        committed_repository(&full, b"first");
        fs::write(full.join("tracked"), b"second").unwrap();
        output(git(&full).args(["-c", "commit.gpgsign=false", "commit", "-qam", "second"]))
            .unwrap();
        let source = root.join("shallow");
        output(
            git(&root)
                .args(["clone", "-q", "--depth=1", "--no-local"])
                .arg(format!("file://{}", full.display()))
                .arg(&source),
        )
        .unwrap();
        output(git(&source).args([
            "config",
            "remote.origin.url",
            "https://example.test/shallow.git",
        ]))
        .unwrap();
        fs::write(source.join("untracked"), b"untracked payload").unwrap();
        assert!(!shallow::frontier(&source).unwrap().is_empty());
        let first =
            export_repository_with_drift(&source, &root.join("first"), &ExportOptions::default())
                .unwrap();
        assert_eq!(first.reuse_unavailable, None, "no reuse was offered");
        let second = export_repository_with_drift(
            &source,
            &root.join("second"),
            &ExportOptions {
                reuse: Some(RetainedCapture {
                    bundle: &first.bundle,
                    // Settled: nothing is racy, so shallow is the only reason.
                    started_ns: first.started_ns + 60 * RACY_GRANULARITY_NS,
                }),
                ..ExportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.reuse_unavailable, Some(ReuseUnavailable::Shallow));
        assert_eq!(second.bytes_read, first.bytes_read);
        fs::remove_dir_all(root).unwrap();
    }

    fn drift_refused<T>(result: Result<T>) -> bool {
        result.err() == Some(BulkloadRefusal::CaptureDrifted)
    }

    // R-N72 re-review finding 3: CAPTURE_DRIFTED is not an estate-only rule.
    // Every restore and import verb refuses a bundle carrying the in-band
    // capture-drift-v1 marker, before it writes anything.
    #[test]
    fn every_restore_and_import_verb_refuses_a_drift_marked_bundle() {
        let (root, source) = drift_fixture("marked-verbs");
        let lane = source.clone();
        mid_pass::arm(&source, move || {
            output(git(&lane).args(["update-ref", "refs/heads/lane", "HEAD"])).unwrap();
        });
        let export =
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default())
                .unwrap();
        assert!(!export.drift.is_empty());
        let bundle = export.bundle.as_path();
        let repo = root.join("repo");
        committed_repository(&repo, b"an unrelated repository");
        let inventory = refs(&repo).unwrap();
        let at = |name: &str| root.join(name);
        assert!(drift_refused(restore_bundle(
            bundle,
            &at("restored"),
            "neo"
        )));
        assert!(
            !at("restored").exists(),
            "refused before the destination exists"
        );
        assert!(drift_refused(restore_linked(
            bundle,
            &repo,
            &at("linked"),
            "neo"
        )));
        assert!(drift_refused(import_bundle(&repo, bundle, "neo")));
        assert!(drift_refused(repair_missing_index(
            bundle,
            &repo,
            "neo",
            &at("repair-receipt")
        )));
        assert!(drift_refused(attach_matching_payload(
            bundle,
            &repo,
            &at("attached"),
            "neo",
            &at("attach-receipt")
        )));
        assert!(drift_refused(attach_standalone_payload(
            bundle,
            &at("standalone"),
            "neo",
            &at("standalone-receipt"),
            &at("origin-from"),
            &at("origin-to")
        )));
        assert!(drift_refused(registered::restore(
            bundle,
            &repo,
            &at("registered"),
            &at("admin"),
            "neo",
            &at("registered-receipt")
        )));
        for name in [
            "linked",
            "repair-receipt",
            "attached",
            "attach-receipt",
            "standalone",
            "standalone-receipt",
            "registered",
            "registered-receipt",
        ] {
            assert!(!at(name).exists(), "{name} was written before the refusal");
        }
        assert_eq!(
            refs(&repo).unwrap(),
            inventory,
            "the repository is untouched"
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Finding 3, shallow custody: the marker rides inside the envelope's
    // inventory, and the restore still refuses before it writes anything.
    #[test]
    fn a_drift_marked_shallow_bundle_refuses_to_restore() {
        let root = std::env::temp_dir().join(format!(
            "bulkload-drift-shallow-marked-{}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let full = root.join("full");
        committed_repository(&full, b"first");
        fs::write(full.join("tracked"), b"second").unwrap();
        output(git(&full).args(["-c", "commit.gpgsign=false", "commit", "-qam", "second"]))
            .unwrap();
        let source = root.join("shallow");
        output(
            git(&root)
                .args(["clone", "-q", "--depth=1", "--no-local"])
                .arg(format!("file://{}", full.display()))
                .arg(&source),
        )
        .unwrap();
        output(git(&source).args([
            "config",
            "remote.origin.url",
            "https://example.test/shallow.git",
        ]))
        .unwrap();
        let lane = source.clone();
        mid_pass::arm(&source, move || {
            output(git(&lane).args(["update-ref", "refs/heads/lane", "HEAD"])).unwrap();
        });
        let export =
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default())
                .unwrap();
        assert!(!export.drift.is_empty());
        // Round-3 N2: the marker is visible from the envelope's own headers,
        // so the check never has to fetch the shallow pack.
        let heads = text(
            git(&root)
                .args(["bundle", "list-heads"])
                .arg(&export.bundle),
        )
        .unwrap();
        assert!(
            heads
                .lines()
                .any(|line| line.ends_with("refs/carry-export/shallow-drift-v1")),
            "{heads}"
        );
        let restored = root.join("restored");
        assert!(drift_refused(restore_bundle(
            &export.bundle,
            &restored,
            "neo"
        )));
        assert!(!restored.exists());
        fs::remove_dir_all(root).unwrap();
    }

    // R-N72 re-review finding 8: clearing the transient reuse refs deletes the
    // refs themselves, never what a symbolic one points at, and a ref that
    // cannot be deleted refuses with a Git inventory code.
    #[test]
    fn clearing_reuse_refs_never_follows_a_symbolic_ref_and_refuses_as_git_inventory() {
        let (root, source) = drift_fixture("clear-reuse");
        let capture = root.join("private");
        fs::create_dir(&capture).unwrap();
        let private = prepare_private(&source, &capture).unwrap();
        let head = text(git(&source).args(["rev-parse", "HEAD"])).unwrap();
        set_ref(&private, "refs/heads/keep", &head).unwrap();
        output(git(&private).args(["symbolic-ref", "refs/carry-reuse/sym", "refs/heads/keep"]))
            .unwrap();
        clear_reuse_refs(&private).unwrap();
        assert!(
            output(git(&private).args(["show-ref", "--verify", "refs/heads/keep"])).is_ok(),
            "a ref outside the reuse namespace was deleted through a symbolic ref"
        );
        assert!(!private.join("refs/carry-reuse/sym").exists());
        set_ref(&private, "refs/carry-reuse/worktree", &head).unwrap();
        fs::write(private.join("refs/carry-reuse/worktree.lock"), b"").unwrap();
        assert!(matches!(
            clear_reuse_refs(&private),
            Err(BulkloadRefusal::GitInventoryMalformed)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // Round-3 N4: the drift check and the restore must read the same bytes. A
    // bundle swapped for a drift-marked one after its check is never the one
    // restored.
    #[test]
    fn a_bundle_swapped_after_its_drift_check_is_never_the_one_restored() {
        let (root, source) = drift_fixture("swap-after-check");
        let clean =
            export_repository_with_drift(&source, &root.join("clean"), &ExportOptions::default())
                .unwrap();
        let lane = source.clone();
        mid_pass::arm(&source, move || {
            fs::write(lane.join("tracked"), b"rewritten mid-pass").unwrap();
        });
        let marked =
            export_repository_with_drift(&source, &root.join("marked"), &ExportOptions::default())
                .unwrap();
        assert!(!marked.drift.is_empty());
        let offered = root.join("offered.bundle");
        fs::copy(&clean.bundle, &offered).unwrap();
        let (from, to) = (marked.bundle, offered.clone());
        mid_pass::arm_at(&offered, mid_pass::Stage::BundleChecked, move || {
            fs::copy(&from, &to).unwrap();
        });
        let restored = root.join("restored");
        restore_bundle(&offered, &restored, "neo").unwrap();
        assert_eq!(
            fs::read(restored.join("tracked")).unwrap(),
            b"tracked bytes at census",
            "the restored bytes are the checked bytes"
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Round-4 R1 (reviewer's r4_shallow_envelope_with_inner_marker_only): an
    // envelope whose inner inventory carries capture-drift-v1 but whose
    // headers do not (written by an older head, or re-enveloped) still
    // refuses when it is unpacked.
    #[test]
    fn r4_shallow_envelope_with_inner_marker_only() {
        let root =
            std::env::temp_dir().join(format!("bulkload-r4-inner-only-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let full = root.join("full");
        committed_repository(&full, b"tracked");
        fs::write(full.join("tracked"), b"second").unwrap();
        output(git(&full).args(["-c", "commit.gpgsign=false", "commit", "-qam", "second"]))
            .unwrap();
        let source = root.join("shallow");
        output(
            git(&root)
                .args(["clone", "-q", "--depth=1", "--no-local"])
                .arg(format!("file://{}", full.display()))
                .arg(&source),
        )
        .unwrap();
        output(git(&source).args(["config", "remote.origin.url", "https://example.test/s.git"]))
            .unwrap();
        let lane = source.clone();
        mid_pass::arm(&source, move || {
            output(git(&lane).args(["update-ref", "refs/heads/lane", "HEAD"])).unwrap();
        });
        let export =
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default())
                .unwrap();
        assert!(!export.drift.is_empty());
        // Re-envelope without the header marker.
        let scratch = root.join("scratch.git");
        output(git(&root).args(["init", "-q", "--bare"]).arg(&scratch)).unwrap();
        output(
            git(&scratch)
                .args(["fetch", "-q"])
                .arg(&export.bundle)
                .arg("refs/carry-export/shallow-custody-v1:refs/carry-export/shallow-custody-v1"),
        )
        .unwrap();
        let forged = root.join("forged.bundle");
        output(
            git(&scratch)
                .args(["bundle", "create", "-q"])
                .arg(&forged)
                .arg("refs/carry-export/shallow-custody-v1"),
        )
        .unwrap();
        let inner = shallow::headers(&scratch, &forged).unwrap();
        assert!(inner.contains("capture-drift-v1"), "inner marker present");
        let result = restore_bundle(&forged, &root.join("restored"), "neo");
        assert_eq!(
            result.err(),
            Some(BulkloadRefusal::CaptureDrifted),
            "inner marker ignored"
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Round-4 P1: a stage lives next to its bundle (the corpus), not in
    // TMPDIR, is private, and is removed on drop.
    #[test]
    fn a_stage_lives_next_to_its_bundle_and_is_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let (root, source) = drift_fixture("stage-near");
        let export =
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default())
                .unwrap();
        let staged = stage_bundle(&export.bundle).unwrap();
        let directory = staged.path().parent().unwrap().to_path_buf();
        assert_eq!(
            directory.parent().unwrap(),
            fs::canonicalize(export.bundle.parent().unwrap()).unwrap()
        );
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let bytes = fs::read(&export.bundle).unwrap();
        assert_eq!(fs::read(staged.path()).unwrap(), bytes);
        // Hashed while it was copied: apply's digest check reads nothing twice.
        assert_eq!(staged.digest(), *blake3::hash(&bytes).as_bytes());
        drop(staged);
        assert!(!directory.exists());
        fs::remove_dir_all(root).unwrap();
    }

    // Round-4 P1: a parent that refuses a private directory falls back to
    // TMPDIR. Not permission-based, so it holds under root on CI.
    #[test]
    fn a_stage_falls_back_to_tmpdir_when_its_parent_refuses() {
        let (root, _) = drift_fixture("stage-fallback");
        let not_a_directory = root.join("plain-file");
        fs::write(&not_a_directory, b"").unwrap();
        let directory = PrivateDir::create(Some(&not_a_directory)).unwrap();
        assert_eq!(directory.path().parent().unwrap(), std::env::temp_dir());
        let path = directory.path().to_path_buf();
        drop(directory);
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    // Round-4 R2: the index checks validate the bytes that are carried, not
    // whatever the live index holds a moment later.
    #[test]
    fn the_index_checks_validate_the_carried_bytes() {
        let (root, source) = drift_fixture("index-carried");
        output(git(&source).args(["update-index", "--assume-unchanged", "tracked"])).unwrap();
        let inside = fs::canonicalize(&source).unwrap();
        mid_pass::arm_at(&source, mid_pass::Stage::IndexRead, move || {
            output(git(&inside).args(["update-index", "--no-assume-unchanged", "tracked"]))
                .unwrap();
        });
        assert_eq!(
            capture_key_parts(&source).err(),
            Some(BulkloadRefusal::GitInventoryMalformed),
            "the carried index holds an unsupported flag"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod legacy {
    //! The reusable-key derivation exactly as it was before drift tolerance
    //! (PR #597 head, 7b017af), kept verbatim so tests can prove that
    //! `KeyParts::digest` is bit-identical and retained captures stay reused.
    use super::{
        capture_census, common_repository, git, output, refs, shallow, source_configuration,
        source_index, text, CapturePolicy, NESTED_WORKTREES_DOMAIN, REBUILDABLE_DOMAIN,
    };
    use crate::{BulkloadRefusal, Result};
    use std::fs;
    use std::path::Path;

    pub fn reusable_capture_key_with_policy(
        repo: &Path,
        policy: CapturePolicy,
    ) -> Result<[u8; 32]> {
        use std::os::unix::ffi::OsStrExt;
        let repo = fs::canonicalize(repo)?;
        let common = common_repository(&repo)?;
        let inventory = refs(&repo)?;
        let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
        let symbolic = git(&repo).args(["symbolic-ref", "-q", "HEAD"]).output()?;
        if !symbolic.status.success() && symbolic.status.code() != Some(1) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let (_, index, _) = source_index(&repo)?;
        let exclude_path = text(git(&repo).args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ]))?;
        let exclude = match fs::read(exclude_path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let stash = if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
            output(git(&repo).args(["reflog", "show", "--format=%H", "refs/stash"]))?
        } else {
            Vec::new()
        };
        let census = capture_census(&repo, &common, policy)?;
        let rows = postcard::to_allocvec(&census.rows).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let configuration = postcard::to_allocvec(&source_configuration(&repo)?)
            .map_err(|_| BulkloadRefusal::FrameCodec)?;
        let boundary = shallow::frontier(&repo)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"tcfs-git-reusable-capture-v1\0");
        for bytes in [
            repo.as_os_str().as_bytes(),
            common.as_os_str().as_bytes(),
            inventory.as_bytes(),
            head.as_bytes(),
            &symbolic.stdout,
            &index,
            &exclude,
            &stash,
            &rows,
            &configuration,
            &boundary,
        ] {
            hash.update(
                &u64::try_from(bytes.len())
                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?
                    .to_le_bytes(),
            );
            hash.update(bytes);
        }
        if !census.nested_worktrees.is_empty() {
            let nested = postcard::to_allocvec(&census.nested_worktrees)
                .map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(NESTED_WORKTREES_DOMAIN);
            hash.update(
                &u64::try_from(nested.len())
                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?
                    .to_le_bytes(),
            );
            hash.update(&nested);
        }
        if !census.omitted.is_empty() {
            let omitted =
                postcard::to_allocvec(&census.omitted).map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(REBUILDABLE_DOMAIN);
            hash.update(
                &u64::try_from(omitted.len())
                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?
                    .to_le_bytes(),
            );
            hash.update(&omitted);
        }
        for directory in [&repo, &common] {
            let identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(directory)?);
            for value in [i128::from(identity.dev), i128::from(identity.ino)] {
                hash.update(&value.to_le_bytes());
            }
        }
        Ok(*hash.finalize().as_bytes())
    }
}

// The adversarial re-review of #53 at 2b64289 (R-N73, R-N83, TIN-4540), kept
// as regression tests. Helpers and assertions are the reviewer's, with the
// verdicts each finding's fix makes definite.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod review_pr53b {
    use super::*;

    fn g(repo: &Path, args: &[&str]) -> Vec<u8> {
        output(git(repo).args(args)).unwrap()
    }

    fn fresh(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rv53b-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn init(repo: &Path) {
        fs::create_dir_all(repo).unwrap();
        g(repo, &["init", "--template=", "-b", "main"]);
        g(repo, &["config", "user.name", "T"]);
        g(repo, &["config", "user.email", "t@localhost"]);
        g(repo, &["config", "commit.gpgsign", "false"]);
    }

    fn commit_all(repo: &Path, message: &str) {
        g(repo, &["add", "-A"]);
        g(repo, &["commit", "-q", "-m", message]);
    }

    // An outer repo and a clean nest `vendor/inner` with a remote-tracking ref
    // at its HEAD, so unpushed=0 remotes=yes is the baseline.
    fn outer_with_pushed_nest(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = fresh(name);
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let inner = outer.join("vendor/inner");
        init(&inner);
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        commit_all(&inner, "v1");
        g(
            &inner,
            &["remote", "add", "origin", "https://example.invalid/i.git"],
        );
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let only = nested_repositories(&outer).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].unpushed, 0);
        assert!(only[0].remotes);
        (root, outer, inner)
    }

    fn verdict(outer: &Path) -> String {
        match nested_repositories(outer) {
            Ok(n) => format!(
                "CUSTODY {:?}",
                n.iter()
                    .map(NestedRepository::receipt_line)
                    .collect::<Vec<_>>()
            ),
            Err(e) => format!("REFUSED {e:?}"),
        }
    }

    // N1: assume-unchanged hides a worktree edit from `status`.
    #[test]
    fn rv_assume_unchanged_edit_in_nest_must_refuse() {
        let (root, outer, inner) = outer_with_pushed_nest("assume");
        g(&inner, &["update-index", "--assume-unchanged", "lib.c"]);
        fs::write(inner.join("lib.c"), b"v2 unique unsaved edit").unwrap();
        assert!(g(
            &inner,
            &["status", "--porcelain=v2", "--untracked-files=all"]
        )
        .is_empty());
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(v.starts_with("REFUSED"), "{v}");
    }

    // N1: skip-worktree hides a worktree edit from `status`.
    #[test]
    fn rv_skip_worktree_edit_in_nest_must_refuse() {
        let (root, outer, inner) = outer_with_pushed_nest("skipwt");
        g(&inner, &["update-index", "--skip-worktree", "lib.c"]);
        fs::write(inner.join("lib.c"), b"v2 unique unsaved edit").unwrap();
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(v.starts_with("REFUSED"), "{v}");
    }

    // N1: a sparse checkout hides paths from `status` the same way, and a
    // split index is refused exactly as the outer repository's is.
    #[test]
    fn rv_sparse_or_split_index_nest_must_refuse() {
        let (root, outer, inner) = outer_with_pushed_nest("sparse");
        g(&inner, &["config", "core.sparseCheckout", "true"]);
        let sparse = verdict(&outer);
        g(&inner, &["config", "--unset", "core.sparseCheckout"]);
        assert!(verdict(&outer).starts_with("CUSTODY"));
        g(&inner, &["update-index", "--split-index"]);
        let split = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(sparse.starts_with("REFUSED"), "{sparse}");
        assert!(split.starts_with("REFUSED"), "{split}");
    }

    fn unpushed_after(name: &str, act: impl FnOnce(&Path)) -> (String, u64) {
        let (root, outer, inner) = outer_with_pushed_nest(name);
        act(&inner);
        let v = verdict(&outer);
        let n = nested_repositories(&outer).map_or(u64::MAX, |n| n[0].unpushed);
        fs::remove_dir_all(root).unwrap();
        (v, n)
    }

    // N2 / R-N83: a commit only a detached HEAD reaches refuses by name.
    #[test]
    fn rv_detached_head_commit_must_count_as_unpushed() {
        let (v, n) = unpushed_after("detached", |inner| {
            g(inner, &["checkout", "-q", "--detach"]);
            fs::write(inner.join("lib.c"), b"detached unique").unwrap();
            g(inner, &["commit", "-q", "-am", "detached unique"]);
        });
        assert!(n >= 1 || v.starts_with("REFUSED"), "{v}");
        assert_eq!(v, "REFUSED GitNestDetachedUnreachable");
    }

    // N2 / R-N83: a stash refuses by name.
    #[test]
    fn rv_stash_must_count_or_refuse() {
        let (v, n) = unpushed_after("stash", |inner| {
            fs::write(inner.join("lib.c"), b"stashed unique").unwrap();
            g(inner, &["stash", "-q"]);
        });
        assert!(n >= 1 || v.starts_with("REFUSED"), "{v}");
        assert_eq!(v, "REFUSED GitNestStashed");
    }

    // N2: a commit only a local tag reaches is counted.
    #[test]
    fn rv_tag_only_commit_must_count_as_unpushed() {
        let (v, n) = unpushed_after("tag", |inner| {
            g(inner, &["checkout", "-q", "--detach"]);
            fs::write(inner.join("lib.c"), b"tagged unique").unwrap();
            g(inner, &["commit", "-q", "-am", "tagged unique"]);
            g(inner, &["tag", "keep"]);
            g(inner, &["checkout", "-q", "main"]);
        });
        assert!(n >= 1 || v.starts_with("REFUSED"), "{v}");
        assert_eq!(n, 1, "{v}");
    }

    // N2 must-pass: the realistic shape, a populated submodule (gitfile into
    // the outer's .git/modules, which the outer bundle does not carry),
    // detached HEAD as `git submodule update` leaves it, one local commit on
    // top. It must refuse or count; never `unpushed=0 remotes=yes`.
    #[test]
    fn rv_submodule_detached_commit_must_count_as_unpushed() {
        let root = fresh("submodule");
        let upstream = root.join("upstream");
        init(&upstream);
        fs::write(upstream.join("lib.c"), b"v1").unwrap();
        commit_all(&upstream, "v1");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        output(
            git(&outer)
                .args(["-c", "protocol.file.allow=always", "submodule", "add", "-q"])
                .arg(&upstream)
                .arg("sub"),
        )
        .unwrap();
        g(&outer, &["commit", "-q", "-m", "add sub"]);
        let sub = outer.join("sub");
        g(&sub, &["config", "user.name", "T"]);
        g(&sub, &["config", "user.email", "t@localhost"]);
        g(&sub, &["checkout", "-q", "--detach"]);
        fs::write(sub.join("lib.c"), b"local unique work").unwrap();
        g(
            &sub,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-am", "local"],
        );
        let v = verdict(&outer);
        let n = nested_repositories(&outer).map(|n| {
            n.iter()
                .filter(|x| x.kind == NestedRepositoryKind::Directory)
                .map(|x| x.unpushed)
                .sum::<u64>()
        });
        fs::remove_dir_all(root).unwrap();
        assert!(
            matches!(n, Ok(c) if c >= 1) || v.starts_with("REFUSED"),
            "{v}"
        );
        assert!(!v.contains("unpushed=0 remotes=yes"), "{v}");
    }

    // N6: no remote means every commit is the only copy: the receipt says
    // unpushed=all(<count>) and names the resolved administration.
    #[test]
    fn rv_no_remote_receipt_reports() {
        let root = fresh("noremote");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let inner = outer.join("vendor/inner");
        init(&inner);
        fs::write(inner.join("lib.c"), b"only copy anywhere").unwrap();
        commit_all(&inner, "only");
        fs::write(inner.join("lib.c"), b"second only copy").unwrap();
        commit_all(&inner, "second");
        let v = verdict(&outer);
        let admin = inner.join(".git").display().to_string();
        fs::remove_dir_all(root).unwrap();
        assert!(v.contains(" unpushed=all(2) remotes=none"), "{v}");
        assert!(v.contains(&format!(" admin=\\\"{admin}\\\" ")), "{v}");
    }

    // N6: a gitfile nest whose administration lives outside the estate item
    // is named by its resolved gitdir; one whose administration lives inside
    // the outer's omitted rebuildable root refuses.
    #[test]
    fn rv_gitfile_pointing_outside_names_admin() {
        let root = fresh("gitfile");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let elsewhere = root.join("elsewhere");
        init(&elsewhere);
        fs::write(elsewhere.join("lib.c"), b"v1").unwrap();
        commit_all(&elsewhere, "v1");
        let inner = outer.join("vendor/inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        fs::write(
            inner.join(".git"),
            format!("gitdir: {}\n", elsewhere.join(".git").display()),
        )
        .unwrap();
        let v = verdict(&outer);
        assert!(
            v.contains(&format!(
                " admin=\\\"{}\\\" ",
                elsewhere.join(".git").display()
            )),
            "{v}"
        );
        // Pointer into the outer's own omitted rebuildable root.
        fs::remove_file(inner.join(".git")).unwrap();
        let admin = outer.join("target/inner.git");
        fs::create_dir_all(outer.join("target")).unwrap();
        g(
            &outer,
            &["init", "-q", "--bare", "--template=", "target/inner.git"],
        );
        g(
            &inner,
            &[
                "--git-dir",
                admin.to_str().unwrap(),
                "--work-tree",
                ".",
                "add",
                "-A",
            ],
        );
        output(
            git(&inner)
                .args(["--git-dir", admin.to_str().unwrap(), "--work-tree", "."])
                .args([
                    "-c",
                    "user.name=T",
                    "-c",
                    "user.email=t@l",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(["commit", "-q", "-m", "only copy"]),
        )
        .unwrap();
        fs::write(inner.join(".git"), b"gitdir: ../../target/inner.git\n").unwrap();
        let v2 = verdict(&outer);
        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&outer, &capture, None, CapturePolicy::default());
        fs::remove_dir_all(root).unwrap();
        assert!(v2.starts_with("REFUSED"), "{v2}");
        assert_eq!(export, Err(BulkloadRefusal::GitInventoryMalformed));
    }

    // N2: a rebase, merge, cherry-pick, revert or bisect in progress in a nest
    // is unfinished work; the nest refuses even when status is clean.
    #[test]
    fn rv_operation_in_progress_in_nest_must_refuse() {
        let (root, outer, inner) = outer_with_pushed_nest("inprogress");
        let head = String::from_utf8(g(&inner, &["rev-parse", "HEAD"])).unwrap();
        let mut verdicts = Vec::new();
        for marker in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "BISECT_START",
            "BISECT_LOG",
            "rebase-merge",
            "rebase-apply",
            "sequencer",
        ] {
            let path = inner.join(".git").join(marker);
            if marker.contains('-') || marker == "sequencer" {
                fs::create_dir(&path).unwrap();
            } else {
                fs::write(&path, &head).unwrap();
            }
            verdicts.push((marker, verdict(&outer)));
            if path.is_dir() {
                fs::remove_dir(&path).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
        }
        let clean = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        for (marker, v) in verdicts {
            assert!(v.starts_with("REFUSED"), "{marker}: {v}");
        }
        assert!(clean.starts_with("CUSTODY"), "{clean}");
    }

    // N3: B1 on a case-insensitive filesystem. The outer tracks
    // vendor/Inner/lib.c, the directory on disk is vendor/inner and holds a
    // clean nest. The literal pathspec is case-sensitive; the filesystem is
    // not. On a case-sensitive filesystem the two are different directories
    // and the nest is custody.
    #[test]
    fn rv_case_variant_tracked_path_under_clean_nest_must_refuse() {
        let root = fresh("icase");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        fs::create_dir_all(outer.join("vendor/Inner")).unwrap();
        fs::write(outer.join("vendor/Inner/lib.c"), b"v1").unwrap();
        commit_all(&outer, "outer");
        let probe = outer.join("vendor/INNER");
        let insensitive = probe.exists();
        fs::rename(outer.join("vendor/Inner"), outer.join("vendor/tmp")).unwrap();
        fs::rename(outer.join("vendor/tmp"), outer.join("vendor/inner")).unwrap();
        let inner = outer.join("vendor/inner");
        init(&inner);
        fs::write(inner.join("lib.c"), b"v2 outer's tracked file, edited").unwrap();
        commit_all(&inner, "nest");
        let v = verdict(&outer);
        let export = export_repository(&outer, &root.join("capture"));
        fs::remove_dir_all(root).unwrap();
        if insensitive {
            assert!(v.starts_with("REFUSED"), "{v}");
            assert_eq!(export, Err(BulkloadRefusal::GitInventoryMalformed));
        } else {
            assert!(v.starts_with("CUSTODY"), "{v}");
        }
    }

    // N3: the same with Unicode normalisation. The outer tracks a path whose
    // directory name is NFC; on disk the nest directory is the NFD spelling.
    // APFS folds the two; ext4 does not.
    #[test]
    fn rv_normalisation_variant_tracked_path_under_clean_nest_must_refuse() {
        let root = fresh("nfd");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        let nfc = "vendor/caf\u{e9}";
        let nfd = "vendor/cafe\u{301}";
        fs::create_dir_all(outer.join(nfc)).unwrap();
        fs::write(outer.join(nfc).join("lib.c"), b"v1").unwrap();
        commit_all(&outer, "outer");
        fs::rename(outer.join(nfc), outer.join("vendor/tmp")).unwrap();
        fs::rename(outer.join("vendor/tmp"), outer.join(nfd)).unwrap();
        let folded = outer.join(nfc).exists();
        let inner = outer.join(nfd);
        init(&inner);
        fs::write(inner.join("lib.c"), b"v2 edit of the outer's tracked file").unwrap();
        commit_all(&inner, "nest");
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        if folded {
            assert!(v.starts_with("REFUSED"), "{v}");
        } else {
            assert!(v.starts_with("CUSTODY"), "{v}");
        }
    }

    // R-N89: the reviewer's credential case. `.env` and untracked notes in a
    // nest whose info/exclude is `*` are ignored, so the nest is clean; both
    // are carried and round-trip byte-identical. Nothing prints contents.
    #[test]
    fn rv_ignored_secret_in_nest_is_carried_and_round_trips() {
        let (root, outer, inner) = outer_with_pushed_nest("ignored");
        fs::create_dir_all(inner.join(".git/info")).unwrap();
        fs::write(inner.join(".git/info/exclude"), b"*\n").unwrap();
        fs::write(inner.join(".env"), format!("TOK{}=unique-credential", "EN")).unwrap();
        fs::create_dir(inner.join("notes")).unwrap();
        fs::write(inner.join("notes/todo.md"), b"unique untracked work").unwrap();
        let v = verdict(&outer);
        assert!(v.starts_with("CUSTODY") && v.contains(" ignored-carried=2"));
        assert!(!v.contains("unique-credential") && !v.contains("unique untracked work"));
        let capture = root.join("capture");
        let export = export_repository(&outer, &capture).unwrap();
        let carried = String::from_utf8(g(
            &capture.join("repository.git"),
            &["ls-tree", "-r", "--name-only", "refs/carry-export/worktree"],
        ))
        .unwrap();
        let restored = root.join("restored");
        restore_bundle(&export, &restored, "neo").unwrap();
        let same = |path: &str| {
            fs::read(restored.join("vendor/inner").join(path)).unwrap()
                == fs::read(inner.join(path)).unwrap()
        };
        let (env, notes) = (same(".env"), same("notes/todo.md"));
        fs::remove_dir_all(root).unwrap();
        assert!(carried.contains("vendor/inner/.env"));
        assert!(carried.contains("vendor/inner/notes/todo.md"));
        assert!(env, ".env did not round-trip");
        assert!(notes, "notes did not round-trip");
    }

    // A gitlink whose path a (malformed, dangling) tree also holds as a tree.
    // Git's own read-tree collapses the duplicate: the index ends with sub/f
    // and no gitlink, so there is nothing for the capture to choose between.
    // The fixed verdict: no nest custody, and sub/f is carried staged and in
    // the worktree and restores byte-identical. Nothing is dropped.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn rv_gitlink_and_tree_at_same_path() {
        let root = fresh("gitlink-tree");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let head = String::from_utf8(g(&outer, &["rev-parse", "HEAD"])).unwrap();
        let head = head.trim();
        let blob = String::from_utf8(
            input(
                git(&outer).args(["hash-object", "-w", "--stdin"]),
                b"under gitlink",
            )
            .unwrap(),
        )
        .unwrap();
        let blob = blob.trim();
        let tree_f = String::from_utf8(
            input(
                git(&outer).args(["mktree"]),
                format!("100644 blob {blob}\tf\n").as_bytes(),
            )
            .unwrap(),
        )
        .unwrap();
        let file_blob = String::from_utf8(g(&outer, &["rev-parse", "HEAD:file"])).unwrap();
        let listing = format!(
            "100644 blob {}\tfile\n160000 commit {head}\tsub\n040000 tree {}\tsub\n",
            file_blob.trim(),
            tree_f.trim()
        );
        let tree = input(git(&outer).args(["mktree"]), listing.as_bytes());
        assert!(tree.is_ok());
        let _ = git(&outer)
            .args(["update-index", "--add", "--cacheinfo"])
            .arg(format!("160000,{head},sub"))
            .output()
            .unwrap();
        let _ = git(&outer)
            .args(["update-index", "--add", "--cacheinfo"])
            .arg(format!("100644,{blob},sub/f"))
            .output()
            .unwrap();
        if let Ok(t) = &tree {
            let t = String::from_utf8(t.clone()).unwrap();
            let _ = git(&outer).args(["read-tree", t.trim()]).output().unwrap();
        }
        fs::create_dir_all(outer.join("sub")).unwrap();
        fs::write(outer.join("sub/f"), b"under gitlink").unwrap();
        let index = String::from_utf8(g(&outer, &["ls-files", "--stage"])).unwrap();
        let v = verdict(&outer);
        let capture = root.join("capture");
        let e = export_repository_with_policy(&outer, &capture, None, CapturePolicy::default());
        let staged = e
            .as_ref()
            .map(|_| {
                String::from_utf8(g(
                    &capture.join("repository.git"),
                    &["ls-tree", "-r", "refs/carry-export/staged"],
                ))
                .unwrap()
            })
            .ok();
        let restored = root.join("restored");
        let back = e
            .as_ref()
            .map(|export| {
                restore_bundle(&export.bundle, &restored, "neo").unwrap();
                fs::read(restored.join("sub/f")).unwrap()
            })
            .ok();
        // R-N110: once a commit's tree names `sub` as both a gitlink and a
        // tree, Git's read-tree of HEAD collapses the index the same way; the
        // capture carries that index and names the collapse on its receipt.
        let duplicate = String::from_utf8(tree.as_ref().unwrap().clone()).unwrap();
        let commit = String::from_utf8(g(
            &outer,
            &[
                "commit-tree",
                duplicate.trim(),
                "-p",
                "HEAD",
                "-m",
                "duplicate",
            ],
        ))
        .unwrap();
        g(&outer, &["update-ref", "refs/heads/main", commit.trim()]);
        g(&outer, &["read-tree", "HEAD"]);
        let collapsed =
            format!("gitlink-collapsed path=\"sub\" head-gitlink={head} carried-as=index");
        let committed = (verdict(&outer), collapsed.clone());
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture-committed"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        let lines: Vec<String> = export
            .nested_repositories
            .iter()
            .map(NestedRepository::receipt_line)
            .collect();
        let restored = root.join("restored-committed");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        let back_committed = fs::read(restored.join("sub/f")).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(lines, vec![collapsed]);
        assert_eq!(
            export.nested_repositories.first().map(|nest| nest.kind),
            Some(NestedRepositoryKind::CollapsedGitlink)
        );
        assert_eq!(back_committed, b"under gitlink");
        assert!(!index.contains("160000"), "{index}");
        assert!(index.contains("\tsub/f"), "{index}");
        // Only a dangling tree ever named both: nothing in HEAD to compare.
        assert_eq!(v, "CUSTODY []");
        assert_eq!(committed.0, format!("CUSTODY [{:?}]", committed.1));
        assert_eq!(e.map(|e| e.nested_repositories), Ok(Vec::new()));
        assert!(staged.unwrap().contains(&format!("blob {blob}\tsub/f")));
        assert_eq!(back.unwrap(), b"under gitlink");
    }

    // R-N111: an ignored nested repository inside a clean nest, outside any
    // rebuildable root, refuses with a typed code naming the inner path
    // (escaped). Under a rebuildable root it is omitted like the rest.
    #[test]
    fn rv_ignored_inner_repository_in_nest_refuses_by_name() {
        let (root, outer, inner) = outer_with_pushed_nest("inner-repo");
        fs::write(inner.join(".gitignore"), b"vendored/\nnode_modules/\n").unwrap();
        commit_all(&inner, "ignore");
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        // Under a rebuildable root (default capture policy): omitted, custody.
        let deep = inner.join("node_modules/dep");
        init(&deep);
        let omitted = export_repository_with_policy(
            &outer,
            &root.join("capture-omitted"),
            None,
            CapturePolicy::default(),
        )
        .map(|export| {
            export
                .omitted
                .into_iter()
                .map(|omission| omission.rel_path)
                .collect::<Vec<_>>()
        });
        assert_eq!(omitted, Ok(vec![b"vendor/inner/node_modules".to_vec()]));
        let odd = inner.join("vendored/odd\nname");
        init(&odd);
        // Default capture policy: node_modules/dep is omitted, so the only
        // refusal is the ignored repository outside a rebuildable root. (The
        // full-fidelity nested_repositories() API would name node_modules/dep
        // first; that API/policy split is follow-up F6.)
        let key = reusable_capture_key(&outer);
        let export = export_repository(&outer, &root.join("capture"));
        fs::remove_dir_all(root).unwrap();
        let expected =
            BulkloadRefusal::GitNestInnerRepository(b"vendor/inner/vendored/odd\nname".to_vec());
        assert_eq!(key, Err(expected.clone()));
        assert_eq!(export, Err(expected.clone()));
        assert_eq!(expected.code(), "GIT_NEST_INNER_REPOSITORY");
        let shown = expected.to_string();
        assert_eq!(
            shown,
            "GIT_NEST_INNER_REPOSITORY path=\"vendor/inner/vendored/odd\\nname\""
        );
        assert_eq!(shown.lines().count(), 1);
    }

    // N4: filter drivers from the nest's own config must not run during the
    // clean check (the nest now refuses); fsmonitor from its config must not.
    #[test]
    fn rv_nest_config_code_execution_during_census() {
        use std::os::unix::fs::PermissionsExt;
        let (root, outer, inner) = outer_with_pushed_nest("filter");
        let filter_marker = root.join("filter-ran");
        let fsmon_marker = root.join("fsmonitor-ran");
        let fsmon = root.join("fsmon.sh");
        fs::write(
            &fsmon,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", fsmon_marker.display()),
        )
        .unwrap();
        fs::set_permissions(&fsmon, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(inner.join(".gitattributes"), b"*.c filter=evil\n").unwrap();
        g(
            &inner,
            &[
                "config",
                "filter.evil.clean",
                &format!("touch '{}'; cat", filter_marker.display()),
            ],
        );
        commit_all(&inner, "attrs");
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        g(
            &inner,
            &["config", "core.fsmonitor", fsmon.to_str().unwrap()],
        );
        let _ = fs::remove_file(&filter_marker);
        let _ = fs::remove_file(&fsmon_marker);
        // Stat-dirty, content-identical: status must re-hash through the filter.
        std::process::Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(inner.join("lib.c"))
            .status()
            .unwrap();
        let v = verdict(&outer);
        let filter_ran = filter_marker.exists();
        let fsmon_ran = fsmon_marker.exists();
        fs::remove_dir_all(root).unwrap();
        assert!(!fsmon_ran, "fsmonitor ran");
        assert!(!filter_ran, "nest clean filter executed during census: {v}");
        assert!(v.starts_with("REFUSED"), "{v}");
    }

    // N4: a lying clean filter makes a dirty nest report clean.
    #[test]
    fn rv_lying_filter_hides_dirty_nest() {
        let (root, outer, inner) = outer_with_pushed_nest("liar");
        fs::write(inner.join(".gitattributes"), b"*.c filter=liar\n").unwrap();
        commit_all(&inner, "attrs");
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        g(&inner, &["config", "filter.liar.clean", "printf v1"]);
        fs::write(inner.join("lib.c"), b"v2 unique edit").unwrap();
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(v.starts_with("REFUSED"), "{v}");
    }

    // F8 quoting: every byte class that could forge or split a receipt line.
    #[test]
    fn rv_receipt_line_escaping() {
        for path in [
            &b"a\nitem=x nested-repository path=\"forged\""[..],
            b"a\"b",
            b"a\\\"b",
            b"a\rb",
            b"\x1b[2Jclear",
            b"tab\there",
            b"\xe2\x80\xa8u2028",
        ] {
            let n = NestedRepository {
                rel_path: path.to_vec(),
                ..super::tests::directory_nest(b"", None)
            };
            let line = n.receipt_line();
            assert!(!line.contains('\n') && !line.contains('\r') && !line.contains('\x1b'));
            assert!(!line.contains('\u{2028}'));
            let inner = line
                .strip_prefix("nested-repository path=\"")
                .unwrap()
                .split(" kind=")
                .next()
                .unwrap();
            let body = inner.strip_suffix('"').unwrap();
            // No unescaped quote inside the quoted field.
            let bytes = body.as_bytes();
            for (i, b) in bytes.iter().enumerate() {
                if *b == b'"' {
                    let mut bs = 0;
                    let mut j = i;
                    while j > 0 && bytes[j - 1] == b'\\' {
                        bs += 1;
                        j -= 1;
                    }
                    assert!(bs % 2 == 1, "unescaped quote in {line}");
                }
            }
        }
    }
}

// N7 (R-N73): a POPULATED submodule restored with its directory absent,
// which Git reports as an unstaged deletion of the gitlink. Restore now
// creates an empty directory at every gitlink path, as `git clone` without
// --recurse-submodules leaves it.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod review_pr53b_b2 {
    use super::*;

    #[test]
    fn rv_populated_submodule_restore_worktree_status() {
        let root = std::env::temp_dir().join(format!("rv53b-popsub-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let mk = |repo: &Path| {
            fs::create_dir_all(repo).unwrap();
            output(git(repo).args(["init", "-q", "--template=", "-b", "main"])).unwrap();
            output(git(repo).args(["config", "user.name", "T"])).unwrap();
            output(git(repo).args(["config", "user.email", "t@l"])).unwrap();
            output(git(repo).args(["config", "commit.gpgsign", "false"])).unwrap();
        };
        let upstream = root.join("upstream");
        mk(&upstream);
        fs::write(upstream.join("lib.c"), b"v1").unwrap();
        output(git(&upstream).args(["add", "-A"])).unwrap();
        output(git(&upstream).args(["commit", "-qm", "v1"])).unwrap();
        let outer = root.join("outer");
        mk(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        output(git(&outer).args(["add", "-A"])).unwrap();
        output(git(&outer).args(["commit", "-qm", "outer"])).unwrap();
        output(
            git(&outer)
                .args(["-c", "protocol.file.allow=always", "submodule", "add", "-q"])
                .arg(&upstream)
                .arg("deps/sub"),
        )
        .unwrap();
        output(git(&outer).args(["commit", "-qm", "sub"])).unwrap();
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        assert!(export
            .nested_repositories
            .iter()
            .any(|nest| nest.kind == NestedRepositoryKind::Gitlink));
        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        let cached =
            output(git(&restored).args(["diff", "--cached", "--name-status", "HEAD"])).unwrap();
        let status = output(git(&restored).args(["status", "--porcelain"])).unwrap();
        let empty_dir = fs::read_dir(restored.join("deps/sub"))
            .unwrap()
            .next()
            .is_none();
        // The same through a linked restore.
        let linked = root.join("linked");
        restore_linked(&export.bundle, &restored, &linked, "neo").unwrap();
        let linked_status = output(git(&linked).args(["status", "--porcelain"])).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(cached.is_empty(), "{:?}", String::from_utf8_lossy(&cached));
        assert!(
            status.is_empty(),
            "restored worktree reports {:?}",
            String::from_utf8_lossy(&status)
        );
        assert!(empty_dir);
        assert!(
            linked_status.is_empty(),
            "linked worktree reports {:?}",
            String::from_utf8_lossy(&linked_status)
        );
    }
}

// The reviewer's case-insensitive restore probe (N3), kept as a regression:
// on a filesystem that folds case, the outer's tracked vendor/Inner/lib.c
// under a nest at vendor/inner now refuses instead of being dropped.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod review_pr53b_icase {
    use super::*;
    #[test]
    fn rv_icase_restore_shape() {
        let root = std::env::temp_dir().join(format!("rv53b-icase2-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let outer = root.join("outer");
        crate::git_carry::tests::committed_repository_pub(&outer, b"outer");
        fs::create_dir_all(outer.join("vendor/Inner")).unwrap();
        fs::write(outer.join("vendor/Inner/lib.c"), b"v1").unwrap();
        output(git(&outer).args(["add", "-A"])).unwrap();
        output(git(&outer).args(["-c", "commit.gpgsign=false", "commit", "-qm", "v"])).unwrap();
        let insensitive = outer.join("vendor/INNER").exists();
        fs::rename(outer.join("vendor/Inner"), outer.join("vendor/tmp")).unwrap();
        fs::rename(outer.join("vendor/tmp"), outer.join("vendor/inner")).unwrap();
        let inner = outer.join("vendor/inner");
        crate::git_carry::tests::committed_repository_pub(&inner, b"x");
        fs::write(inner.join("lib.c"), b"v2 edit of outer-tracked file").unwrap();
        output(git(&inner).args(["-c", "commit.gpgsign=false", "commit", "-qam", "n"])).unwrap();
        let export = export_repository_with_policy(
            &outer,
            &root.join("cap"),
            None,
            CapturePolicy::default(),
        );
        if insensitive {
            assert_eq!(export, Err(BulkloadRefusal::GitInventoryMalformed));
        } else {
            let restored = root.join("restored");
            restore_bundle(&export.unwrap().bundle, &restored, "neo").unwrap();
            assert!(restored.join("vendor").is_dir());
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod review_pr53c {
    // The #53 round-3 adversarial review of 9bfc58b (R-N73, TIN-4540), kept
    // as regression tests; each finding's fix makes its verdict definite.
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn g(repo: &Path, args: &[&str]) -> Vec<u8> {
        let out = git(repo)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@localhost",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn fresh(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rv53c-{name}-{}", std::process::id()));
        if root.exists() {
            let _ = std::process::Command::new("chmod")
                .args(["-R", "u+rwx"])
                .arg(&root)
                .status();
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn init(repo: &Path) {
        fs::create_dir_all(repo).unwrap();
        g(repo, &["init", "--template=", "-b", "main"]);
    }

    fn commit_all(repo: &Path, message: &str) {
        g(repo, &["add", "-A"]);
        g(repo, &["commit", "-q", "-m", message]);
    }

    // outer/file tracked; outer/vendor/inner a clean nest pushed to origin/main.
    fn outer_with_nest(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = fresh(name);
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let inner = outer.join("vendor/inner");
        init(&inner);
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        commit_all(&inner, "v1");
        g(
            &inner,
            &["remote", "add", "origin", "https://example.invalid/i.git"],
        );
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        (root, outer, inner)
    }

    fn verdict(outer: &Path) -> String {
        match nested_repositories(outer) {
            Ok(n) => format!(
                "CUSTODY {:?}",
                n.iter()
                    .map(NestedRepository::receipt_line)
                    .collect::<Vec<_>>()
            ),
            Err(e) => format!("REFUSED {e:?}"),
        }
    }

    // A. R-N73/N4 class: the built-in `ident` conversion is a lying clean
    // filter with no command. A same-size edit inside `$Id: ... $` is invisible
    // to the census status, so a dirty nest is custody.
    #[test]
    fn rv3_ident_attribute_hides_an_edit_in_a_clean_nest() {
        let (root, outer, inner) = outer_with_nest("ident");
        fs::write(inner.join(".gitattributes"), b"*.c ident\n").unwrap();
        fs::write(inner.join("a.c"), b"x $Id$ y\n").unwrap();
        commit_all(&inner, "ident");
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fs::remove_file(inner.join("a.c")).unwrap();
        g(&inner, &["checkout", "--", "a.c"]);
        let smudged = fs::read_to_string(inner.join("a.c")).unwrap();
        let start = smudged.find("$Id: ").unwrap() + 5;
        let mut edited = smudged.clone();
        edited.replace_range(
            start..start + 40,
            "UNIQUE-UNSAVED-EDIT-ZZZZZZZZZZZZZZZZZZZZ",
        );
        assert_eq!(edited.len(), smudged.len());
        fs::write(inner.join("a.c"), &edited).unwrap();
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(v.starts_with("REFUSED"), "ident edit hidden: {v}");
        assert_eq!(
            v,
            format!(
                "REFUSED {:?}",
                BulkloadRefusal::GitNestConversionAttribute(b"vendor/inner/a.c".to_vec())
            )
        );
    }

    // B. R-N73/N1 class: the nest's own config weakens what status compares.
    #[test]
    fn rv3_nest_core_filemode_false_hides_a_mode_change() {
        let (root, outer, inner) = outer_with_nest("filemode");
        g(&inner, &["config", "core.fileMode", "false"]);
        fs::set_permissions(inner.join("lib.c"), fs::Permissions::from_mode(0o755)).unwrap();
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(v.starts_with("REFUSED"), "chmod hidden: {v}");
    }

    #[test]
    fn rv3_nest_trustctime_false_hides_a_same_size_mtime_restored_edit() {
        let (root, outer, inner) = outer_with_nest("trustctime");
        g(&inner, &["config", "core.trustctime", "false"]);
        g(&inner, &["config", "core.checkStat", "minimal"]);
        let pinned = std::process::Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(inner.join("lib.c"))
            .status()
            .unwrap();
        assert!(pinned.success());
        g(&inner, &["update-index", "--refresh"]);
        std::thread::sleep(std::time::Duration::from_millis(2200));
        fs::write(inner.join("lib.c"), b"XX").unwrap();
        assert!(std::process::Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(inner.join("lib.c"))
            .status()
            .unwrap()
            .success());
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(v.starts_with("REFUSED"), "same-size edit hidden: {v}");
    }

    // B1 (round 3): text/eol/crlf and working-tree-encoding on a tracked path
    // refuse by name; an explicitly binary (-text) path does not.
    #[test]
    fn rv3_conversion_attributes_refuse_by_name_and_binary_does_not() {
        let (root, outer, inner) = outer_with_nest("conversions");
        fs::write(inner.join(".gitattributes"), b"*.bin binary\n").unwrap();
        fs::write(inner.join("blob.bin"), b"\x00\x01").unwrap();
        commit_all(&inner, "binary");
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let binary = verdict(&outer);
        let mut refused = Vec::new();
        for attributes in [
            "lib.c text\n",
            "lib.c eol=crlf\n",
            "lib.c crlf\n",
            "lib.c working-tree-encoding=UTF-16\n",
        ] {
            fs::create_dir_all(inner.join(".git/info")).unwrap();
            fs::write(inner.join(".git/info/attributes"), attributes).unwrap();
            refused.push((attributes, verdict(&outer)));
        }
        fs::remove_dir_all(root).unwrap();
        assert!(binary.starts_with("CUSTODY"), "{binary}");
        let expected = format!(
            "REFUSED {:?}",
            BulkloadRefusal::GitNestConversionAttribute(b"vendor/inner/lib.c".to_vec())
        );
        for (attributes, v) in refused {
            assert_eq!(v, expected, "{attributes}");
        }
    }
    // H. N7 regression: a gitlink whose worktree seat is a file (typechange)
    // or a symlink captures fine and must restore.
    fn gitlink_typechange(name: &str, seat: &str) -> (Result<()>, String) {
        let root = fresh(name);
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let head = String::from_utf8(g(&outer, &["rev-parse", "HEAD"])).unwrap();
        g(
            &outer,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},sub", head.trim()),
            ],
        );
        match seat {
            "file" => fs::write(outer.join("sub"), b"a file where the submodule was").unwrap(),
            "symlink" => std::os::unix::fs::symlink("file", outer.join("sub")).unwrap(),
            _ => {}
        }
        // Git's own view, success or not: with a symlink at a gitlink path
        // `git status` itself exits 128 in the source, and must match after.
        let status = |repo: &Path| {
            let out = git(repo).args(["status", "--porcelain"]).output().unwrap();
            format!(
                "{}:{}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout)
            )
        };
        let before = status(&outer);
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        let restored = root.join("restored");
        let result = restore_bundle(&export.bundle, &restored, "neo");
        let after = if result.is_ok() {
            status(&restored)
        } else {
            String::new()
        };
        fs::remove_dir_all(root).unwrap();
        (result, format!("before=[{before}] after=[{after}]"))
    }

    #[test]
    fn rv3_gitlink_with_a_file_seat_restores() {
        let (result, status) = gitlink_typechange("gl-file", "file");
        assert_eq!(result, Ok(()), "{status}");
    }

    #[test]
    fn rv3_gitlink_with_a_symlink_seat_restores() {
        let (result, status) = gitlink_typechange("gl-link", "symlink");
        assert_eq!(result, Ok(()), "{status}");
    }

    #[test]
    fn rv3_gitlink_with_no_seat_restores_its_deletion() {
        let (result, status) = gitlink_typechange("gl-none", "none");
        assert_eq!(result, Ok(()), "{status}");
        let (before, after) = status.split_once(" after=").unwrap();
        assert_eq!(
            before.trim_start_matches("before="),
            after,
            "restore invents a directory the source did not have"
        );
    }

    // B2: the populated-submodule case N7 exists for still gets its empty
    // directory, and a symlink seat at a gitlink path is never followed.
    #[test]
    fn rv3_gitlink_symlink_seat_is_restored_as_a_link_not_followed() {
        let root = fresh("gl-link-target");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let head = String::from_utf8(g(&outer, &["rev-parse", "HEAD"])).unwrap();
        g(
            &outer,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},sub", head.trim()),
            ],
        );
        let elsewhere = root.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, outer.join("sub")).unwrap();
        let export = export_repository(&outer, &root.join("capture")).unwrap();
        let restored = root.join("restored");
        restore_bundle(&export, &restored, "neo").unwrap();
        let seat = fs::symlink_metadata(restored.join("sub")).unwrap();
        let untouched = fs::read_dir(&elsewhere).unwrap().next().is_none();
        fs::remove_dir_all(root).unwrap();
        assert!(seat.is_symlink());
        assert!(untouched);
    }
    // C. R-N89 "like the outer repo's": a FIFO in the outer refuses, a FIFO
    // (untracked or ignored) in a nest is silently neither carried nor named.
    #[test]
    #[ignore = "D1, deferred by the coordinator: FIFO/socket inside a nest"]
    fn rv3_fifo_in_a_nest_is_treated_like_the_outer() {
        let (root, outer, inner) = outer_with_nest("fifo");
        let mk = |path: &Path| {
            assert!(std::process::Command::new("mkfifo")
                .arg(path)
                .status()
                .unwrap()
                .success());
        };
        mk(&outer.join("outer-fifo"));
        let outer_verdict = verdict(&outer);
        fs::remove_file(outer.join("outer-fifo")).unwrap();
        mk(&inner.join("nest-fifo"));
        let nest_verdict = verdict(&outer);
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        );
        fs::remove_dir_all(root).unwrap();
        assert!(outer_verdict.starts_with("REFUSED"), "{outer_verdict}");
        assert!(
            nest_verdict.starts_with("REFUSED"),
            "outer refuses a FIFO, nest drops it silently: {nest_verdict}; export ok={}",
            export.is_ok()
        );
    }

    fn nest_with_ignored(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let (root, outer, inner) = outer_with_nest(name);
        fs::write(inner.join(".gitignore"), b".env\nnotes/\n").unwrap();
        commit_all(&inner, "ignore");
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fs::write(
            inner.join(".env"),
            format!("TOK{}=rv3-unique-credential", "EN"),
        )
        .unwrap();
        fs::create_dir(inner.join("notes")).unwrap();
        fs::write(inner.join("notes/todo.md"), b"rv3 unique untracked work").unwrap();
        (root, outer, inner)
    }

    // D. An ignored nest file that changes between census and byte read.
    // Written against 9bfc58b, where the raw pass refused. Under #52's drift
    // custody (merged in B4) a seat that changes under the pass is drift,
    // withdrawn from the bundle and named, for the outer's own seats and, by
    // R-N89, for a nest's carried ignored seats alike: never silently kept.
    #[test]
    fn rv3_ignored_nest_file_changing_mid_capture_is_named_drift() {
        let (root, outer, inner) = nest_with_ignored("midchange");
        let common = common_repository(&outer).unwrap();
        let census = capture_census(&outer, &common, CapturePolicy::default()).unwrap();
        assert!(census
            .rows
            .iter()
            .any(|row| row.rel_path == b"vendor/inner/.env"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(
            inner.join(".env"),
            format!("TOK{}=rv3-unique-credentiaX", "EN"),
        )
        .unwrap();
        let private = prepare_private(&outer, &root).unwrap();
        let result = raw_tree::capture(&private, &outer, &census.rows, &raw_tree::Reuse::new());
        fs::remove_dir_all(root).unwrap();
        let drift = result.unwrap().drift;
        assert!(
            drift
                .iter()
                .any(|row| row.kind == DriftKind::SeatChanged && row.name == b"vendor/inner/.env"),
            "{drift:?}"
        );
    }

    // E. A parent directory swapped for a symlink after the census.
    #[test]
    fn rv3_symlinked_parent_swapped_mid_capture_refuses() {
        let (root, outer, inner) = nest_with_ignored("parentswap");
        let common = common_repository(&outer).unwrap();
        let census = capture_census(&outer, &common, CapturePolicy::default()).unwrap();
        let elsewhere = root.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("todo.md"), b"attacker bytes").unwrap();
        fs::rename(inner.join("notes"), root.join("notes.moved")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, inner.join("notes")).unwrap();
        let private = prepare_private(&outer, &root).unwrap();
        let result = raw_tree::capture(&private, &outer, &census.rows, &raw_tree::Reuse::new());
        fs::remove_dir_all(root).unwrap();
        assert!(result.is_err(), "read through a swapped parent");
    }

    // F. Ignored symlinks (to a directory, escaping the nest) are carried as
    // links, never followed, and restore as the same links.
    #[test]
    fn rv3_ignored_escaping_symlinks_are_links_not_followed() {
        let (root, outer, inner) = outer_with_nest("symlinks");
        fs::write(inner.join(".gitignore"), b"l*\n").unwrap();
        commit_all(&inner, "ignore");
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let secret = root.join("outside-secret");
        fs::write(&secret, b"rv3 outside bytes").unwrap();
        std::os::unix::fs::symlink("../../../outside-secret", inner.join("lesc")).unwrap();
        std::os::unix::fs::symlink(&root, inner.join("ldir")).unwrap();
        let v = verdict(&outer);
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        let carried = String::from_utf8(g(
            &root.join("capture/repository.git"),
            &["ls-tree", "-r", "refs/carry-export/worktree"],
        ))
        .unwrap();
        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        let lesc = fs::read_link(restored.join("vendor/inner/lesc")).unwrap();
        let ldir = fs::symlink_metadata(restored.join("vendor/inner/ldir")).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(v.contains("ignored-carried=2"), "{v}");
        assert!(carried.contains("120000 blob") && carried.contains("vendor/inner/lesc"));
        assert!(!carried.contains("rv3 outside bytes"));
        assert_eq!(lesc, Path::new("../../../outside-secret"));
        assert!(ldir.is_symlink());
    }

    // G. F5 made reachable by R-N89, through the public estate flow: the outer
    // and the nest as two planned items, the nest's workspace under the
    // outer's. The outer restore writes the nest's ignored files at the nest
    // path; the nest item then meets an occupied directory. A control nest
    // without ignored files restores in the same plan shape.
    fn estate_two_items(
        name: &str,
        with_ignored: bool,
    ) -> (Vec<(String, Option<String>)>, bool, String) {
        use crate::estate;
        let (root, outer, inner) = if with_ignored {
            nest_with_ignored(name)
        } else {
            outer_with_nest(name)
        };
        let destination = root.join("dest");
        let plan = root.join("plan");
        estate::add(&plan, &outer, &destination, Some(&destination)).unwrap();
        let nest_destination = destination.join("vendor/inner");
        estate::add(&plan, &inner, &nest_destination, Some(&nest_destination)).unwrap();
        let rows = std::sync::Mutex::new(Vec::new());
        let record = |row: &estate::Receipt| {
            rows.lock()
                .unwrap()
                .push((row.outcome.to_owned(), row.reason.clone()));
            Ok(())
        };
        estate::capture(&plan, &root.join("state"), &root.join("corpus"), 1, &record).unwrap();
        let _ = estate::apply(
            &plan,
            &root.join("corpus"),
            &root.join("applied"),
            "neo",
            1,
            &record,
        );
        let nest_repo_restored = nest_destination.join(".git").exists()
            && (!with_ignored
                || (fs::read(nest_destination.join(".env")).unwrap()
                    == fs::read(inner.join(".env")).unwrap()
                    && fs::read(nest_destination.join("notes/todo.md")).unwrap()
                        == fs::read(inner.join("notes/todo.md")).unwrap()));
        // What the restored OUTER thinks of the nest's carried secret.
        let status = if destination.join(".git").exists() {
            String::from_utf8(g(
                &destination,
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                    "--ignored",
                ],
            ))
            .unwrap()
        } else {
            String::new()
        };
        let rows = rows.into_inner().unwrap();
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+rwx"])
            .arg(&root)
            .status();
        fs::remove_dir_all(root).unwrap();
        (rows, nest_repo_restored, status)
    }

    #[test]
    fn rv3_estate_control_nest_without_ignored_files_restores_after_outer() {
        let (rows, nest_restored, _) = estate_two_items("estate-control", false);
        eprintln!("control rows: {rows:?}");
        assert!(nest_restored, "{rows:?}");
    }

    // R-N114: the nest item owns its seats. The outer carries none of the
    // nest's ignored files and names the carrying item; the `.env` restores
    // exactly once, through the nest item, and is never untracked in the
    // restored outer.
    #[test]
    fn rv3_estate_nest_with_ignored_files_restores_after_outer() {
        let (rows, nest_restored, status) = estate_two_items("estate-ignored", true);
        assert!(
            rows.iter().all(|(outcome, _)| outcome != "refused"),
            "{rows:?}"
        );
        assert!(
            !status.contains("?? vendor/inner/.env"),
            "nest secret is untracked (not ignored) in the restored outer:\n{status}"
        );
        assert!(
            nest_restored,
            "nest item cannot restore after the outer: {rows:?}"
        );
    }

    // I. Judgment (b): an ignored nested repository inside a nest refuses,
    // while the same shape one level up is custody.
    #[test]
    fn rv3_ignored_repo_inside_nest_refuses_but_outer_equivalent_is_custody() {
        let (root, outer, inner) = outer_with_nest("nest-in-nest");
        fs::write(inner.join(".gitignore"), b"cache/\n").unwrap();
        commit_all(&inner, "ignore");
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let deep = inner.join("cache/deep");
        init(&deep);
        fs::write(deep.join("x"), b"x").unwrap();
        commit_all(&deep, "x");
        let in_nest = verdict(&outer);
        fs::write(outer.join(".gitignore"), b"cache/\n").unwrap();
        commit_all(&outer, "ignore");
        fs::rename(inner.join("cache"), outer.join("cache")).unwrap();
        let in_outer = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        eprintln!("in nest: {in_nest}\nin outer: {in_outer}");
        assert!(in_nest.starts_with("REFUSED"));
        assert!(in_outer.starts_with("CUSTODY"));
    }

    // J. Credentials: no receipt line, refusal or Debug output of the custody
    // carries a carried secret's bytes or a credentialed remote URL.
    #[test]
    fn rv3_no_credential_in_receipts_or_debug() {
        let (root, outer, inner) = nest_with_ignored("creds");
        g(
            &inner,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("https://user:{}@example.invalid/i.git", "rv3-url-token"),
            ],
        );
        let custody = nested_repositories(&outer).unwrap();
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        let text = format!(
            "{custody:?}\n{:?}\n{}",
            export,
            custody
                .iter()
                .map(NestedRepository::receipt_line)
                .collect::<Vec<_>>()
                .join("\n")
        );
        fs::remove_dir_all(root).unwrap();
        for needle in [
            "rv3-unique-credential",
            "rv3-url-token",
            "rv3 unique untracked",
        ] {
            assert!(!text.contains(needle), "{needle} leaked");
        }
    }

    // (K, the RV3 reviewer key-print probe, was removed 2026-10-01: it asserted
    // nothing and returned early without RV3_FIXTURE. Key bit-identity is
    // asserted by estate.rs `KeyParts::digest must be bit-identical`.)
    // L. A nest's populated submodule is a nest-in-nest the census never
    // walks. R-N115 now refuses the nest by name, so R-N83 (stash), N1
    // (hidden index flags) and R-N89 (ignored files) cannot be skipped there.
    fn nest_with_populated_submodule(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let (root, outer, inner) = outer_with_nest(name);
        let upstream = root.join("upstream");
        init(&upstream);
        fs::write(upstream.join("s.c"), b"s1").unwrap();
        fs::write(upstream.join(".gitignore"), b".env\n").unwrap();
        commit_all(&upstream, "s1");
        g(
            &inner,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                upstream.to_str().unwrap(),
                "s",
            ],
        );
        g(&inner, &["commit", "-q", "-m", "add s"]);
        g(&inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let sub = inner.join("s");
        // R-N115: a populated submodule inside a nest now refuses by name,
        // before any of the checks this section probes could be skipped.
        assert!(
            verdict(&outer).starts_with("REFUSED GitNestPopulatedSubmodule"),
            "{}",
            verdict(&outer)
        );
        (root, outer, sub)
    }

    #[test]
    fn rv3_stash_in_a_nests_submodule_refuses() {
        let (root, outer, sub) = nest_with_populated_submodule("subm-stash");
        fs::write(sub.join("s.c"), b"stashed unique work").unwrap();
        g(&sub, &["stash", "-q"]);
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(v.starts_with("REFUSED"), "stash in nest submodule: {v}");
    }

    #[test]
    fn rv3_assume_unchanged_edit_in_a_nests_submodule_refuses() {
        let (root, outer, sub) = nest_with_populated_submodule("subm-assume");
        g(&sub, &["update-index", "--assume-unchanged", "s.c"]);
        fs::write(sub.join("s.c"), b"hidden unique edit").unwrap();
        let v = verdict(&outer);
        fs::remove_dir_all(root).unwrap();
        assert!(
            v.starts_with("REFUSED"),
            "assume-unchanged in nest submodule: {v}"
        );
    }

    #[test]
    fn rv3_ignored_file_in_a_nests_submodule_is_carried_or_refused() {
        let (root, outer, sub) = nest_with_populated_submodule("subm-ignored");
        fs::write(sub.join(".env"), format!("TOK{}=rv3-subm-credential", "EN")).unwrap();
        let v = verdict(&outer);
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        );
        let carried = export.as_ref().ok().map(|_| {
            String::from_utf8(g(
                &root.join("capture/repository.git"),
                &["ls-tree", "-r", "--name-only", "refs/carry-export/worktree"],
            ))
            .unwrap()
        });
        fs::remove_dir_all(root).unwrap();
        assert!(
            v.starts_with("REFUSED")
                || carried
                    .as_deref()
                    .is_some_and(|c| c.contains("vendor/inner/s/.env")),
            "submodule ignored file silently dropped: {v} carried={carried:?}"
        );
    }

    // M. N7 + F5 for gitlinks: the outer's populated submodule as its own
    // planned item under the outer's workspace.
    #[test]
    fn rv3_estate_populated_submodule_item_restores_after_outer() {
        use crate::estate;
        let root = fresh("estate-subm");
        let upstream = root.join("upstream");
        init(&upstream);
        fs::write(upstream.join("s.c"), b"s1").unwrap();
        commit_all(&upstream, "s1");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        g(
            &outer,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                upstream.to_str().unwrap(),
                "sub",
            ],
        );
        g(&outer, &["commit", "-q", "-m", "add sub"]);
        let sub = outer.join("sub");
        g(&sub, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let destination = root.join("dest");
        let plan = root.join("plan");
        estate::add(&plan, &outer, &destination, Some(&destination)).unwrap();
        let sub_destination = destination.join("sub");
        estate::add(&plan, &sub, &sub_destination, Some(&sub_destination)).unwrap();
        let rows = std::sync::Mutex::new(Vec::new());
        let record = |row: &estate::Receipt| {
            rows.lock()
                .unwrap()
                .push((row.outcome.to_owned(), row.reason.clone()));
            Ok(())
        };
        let captured =
            estate::capture(&plan, &root.join("state"), &root.join("corpus"), 1, &record);
        let _ = estate::apply(
            &plan,
            &root.join("corpus"),
            &root.join("applied"),
            "neo",
            1,
            &record,
        );
        let restored = sub_destination.join(".git").exists();
        let rows = rows.into_inner().unwrap();
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+rwx"])
            .arg(&root)
            .status();
        fs::remove_dir_all(root).unwrap();
        eprintln!(
            "submodule estate rows: {rows:?} capture ok={}",
            captured.is_ok()
        );
        assert!(restored, "{rows:?}");
    }
}

// The #53 round-4 adversarial review of dcfee46 (R-N73, R-N114, R-N115,
// TIN-4540), kept as regression tests. Each `rv4_*` asserts the behaviour the
// rulings require. Fixtures carry no credential-shaped literal (R-N117).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod review_pr53d {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn g(repo: &Path, args: &[&str]) -> Vec<u8> {
        let out = git(repo)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn fresh(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rv53d-{name}-{}", std::process::id()));
        if root.exists() {
            let _ = std::process::Command::new("chmod")
                .args(["-R", "u+rwx"])
                .arg(&root)
                .status();
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn cleanup(root: &Path) {
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+rwx"])
            .arg(root)
            .status();
        let _ = fs::remove_dir_all(root);
    }

    fn init(repo: &Path) {
        fs::create_dir_all(repo).unwrap();
        g(repo, &["init", "--template=", "-b", "main"]);
    }

    fn commit_all(repo: &Path, message: &str) {
        g(repo, &["add", "-A"]);
        g(repo, &["commit", "-q", "-m", message]);
    }

    fn pushed(inner: &Path) {
        g(inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    }

    fn head(repo: &Path) -> String {
        String::from_utf8(g(repo, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_owned()
    }

    // outer/file tracked; outer/vendor/inner a clean nest pushed to origin/main.
    fn outer_with_nest(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = fresh(name);
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let inner = outer.join("vendor/inner");
        init(&inner);
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        commit_all(&inner, "v1");
        g(
            &inner,
            &["remote", "add", "origin", "https://example.invalid/i.git"],
        );
        pushed(&inner);
        (root, outer, inner)
    }

    fn upstream(root: &Path, name: &str) -> PathBuf {
        let up = root.join(name);
        init(&up);
        fs::write(up.join("u"), name.as_bytes()).unwrap();
        commit_all(&up, name);
        up
    }

    fn verdict(outer: &Path) -> String {
        match nested_repositories(outer) {
            Ok(n) => format!(
                "CUSTODY {:?}",
                n.iter()
                    .map(NestedRepository::receipt_line)
                    .collect::<Vec<_>>()
            ),
            Err(e) => format!("REFUSED {e:?}"),
        }
    }

    // ---------------------------------------------------------------- R-N115
    // A nest's UNPOPULATED gitlink directory: Git's status never descends a
    // gitlink path, so files under it (and a whole repository under it) are
    // neither dirt nor ignored. The census calls the nest clean and names
    // nothing below it; `nest_populated_submodule` looks only at `<gitlink>/.git`.
    fn nest_with_unpopulated_gitlink(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let (root, outer, inner) = outer_with_nest(name);
        let up = upstream(&root, "up");
        g(
            &inner,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},s", head(&up)),
            ],
        );
        g(&inner, &["commit", "-q", "-m", "gitlink s"]);
        pushed(&inner);
        fs::create_dir(inner.join("s")).unwrap();
        (root, outer, inner)
    }

    #[test]
    fn rv4_file_inside_a_nests_unpopulated_gitlink_dir_is_not_silently_dropped() {
        let (root, outer, inner) = nest_with_unpopulated_gitlink("unpop-file");
        let baseline = verdict(&outer);
        fs::write(inner.join("s/leftover.txt"), b"rv4 unique bytes").unwrap();
        let v = verdict(&outer);
        let census = capture_census(
            &outer,
            &common_repository(&outer).unwrap(),
            CapturePolicy::default(),
        );
        let carried = census.as_ref().is_ok_and(|census| {
            census
                .rows
                .iter()
                .any(|row| row.rel_path == b"vendor/inner/s/leftover.txt")
        });
        cleanup(&root);
        eprintln!("baseline: {baseline}\nwith leftover: {v}\ncarried as seat: {carried}");
        assert!(
            v.starts_with("REFUSED") || carried,
            "a file below the nest's unpopulated gitlink is neither refused nor carried: {v}"
        );
    }

    #[test]
    fn rv4_repository_inside_a_nests_unpopulated_gitlink_dir_is_refused_by_name() {
        let (root, outer, inner) = nest_with_unpopulated_gitlink("unpop-repo");
        let deep = inner.join("s/deep");
        init(&deep);
        fs::write(deep.join("d"), b"rv4 unpushed work").unwrap();
        commit_all(&deep, "only copy");
        let v = verdict(&outer);
        cleanup(&root);
        eprintln!("repo under unpopulated gitlink: {v}");
        assert!(
            v.starts_with("REFUSED"),
            "a repository (with unpushed history) below the nest's unpopulated gitlink is custody-clean: {v}"
        );
    }

    #[test]
    fn rv4_populated_submodule_at_a_nested_path_refuses_naming_it() {
        let (root, outer, inner) = outer_with_nest("sub-deep-path");
        let up = upstream(&root, "up");
        g(
            &inner,
            &["submodule", "add", "-q", up.to_str().unwrap(), "lib/s"],
        );
        g(&inner, &["commit", "-q", "-m", "sub"]);
        pushed(&inner);
        let result = nested_repositories(&outer);
        cleanup(&root);
        assert_eq!(
            result.map(|_| ()),
            Err(BulkloadRefusal::GitNestPopulatedSubmodule(
                b"vendor/inner/lib/s".to_vec()
            ))
        );
    }

    #[test]
    fn rv4_populated_submodule_two_levels_down_refuses() {
        let (root, outer, inner) = outer_with_nest("sub-two-levels");
        let leaf = upstream(&root, "leaf");
        let mid = upstream(&root, "mid");
        g(
            &mid,
            &["submodule", "add", "-q", leaf.to_str().unwrap(), "t"],
        );
        g(&mid, &["commit", "-q", "-m", "t"]);
        g(
            &inner,
            &["submodule", "add", "-q", mid.to_str().unwrap(), "s"],
        );
        g(&inner, &["commit", "-q", "-m", "s"]);
        g(
            &inner,
            &["submodule", "update", "-q", "--init", "--recursive"],
        );
        pushed(&inner);
        let v = verdict(&outer);
        cleanup(&root);
        assert!(v.starts_with("REFUSED"), "{v}");
    }

    // -------------------------------------------------------------------- B1
    // Every attribute source status reads must be seen by the census.
    fn attr_case(name: &str, setup: impl FnOnce(&Path, &Path)) -> String {
        let (root, outer, inner) = outer_with_nest(name);
        fs::create_dir_all(inner.join("sub")).unwrap();
        fs::write(inner.join("sub/a.c"), b"a\n").unwrap();
        commit_all(&inner, "sub");
        pushed(&inner);
        setup(&root, &inner);
        let v = verdict(&outer);
        cleanup(&root);
        v
    }

    #[test]
    fn rv4_attribute_sources_all_refuse() {
        let mut results = Vec::new();
        results.push((
            "subdir .gitattributes",
            attr_case("attr-subdir", |_, inner| {
                fs::write(inner.join("sub/.gitattributes"), b"*.c ident\n").unwrap();
                commit_all(inner, "attrs");
                pushed(inner);
            }),
        ));
        results.push((
            "info/attributes",
            attr_case("attr-info", |_, inner| {
                fs::create_dir_all(inner.join(".git/info")).unwrap();
                fs::write(inner.join(".git/info/attributes"), b"* eol=crlf\n").unwrap();
            }),
        ));
        results.push((
            "nest core.attributesFile",
            attr_case("attr-file", |root, inner| {
                let file = root.join("attributes");
                fs::write(&file, b"*.c text\n").unwrap();
                g(
                    inner,
                    &["config", "core.attributesFile", file.to_str().unwrap()],
                );
            }),
        ));
        results.push((
            "macro attribute",
            attr_case("attr-macro", |_, inner| {
                fs::write(
                    inner.join(".gitattributes"),
                    b"[attr]conv text eol=lf\n*.c conv\n",
                )
                .unwrap();
                commit_all(inner, "macro");
                pushed(inner);
            }),
        ));
        results.push((
            "attr.tree",
            attr_case("attr-tree", |_, inner| {
                let blob = String::from_utf8(
                    input(
                        git(inner).args(["hash-object", "-w", "--stdin"]),
                        b"*.c ident\n",
                    )
                    .unwrap(),
                )
                .unwrap();
                let tree = String::from_utf8(
                    input(
                        git(inner).args(["mktree"]),
                        format!("100644 blob {}\t.gitattributes\n", blob.trim()).as_bytes(),
                    )
                    .unwrap(),
                )
                .unwrap();
                g(inner, &["config", "attr.tree", tree.trim()]);
            }),
        ));
        results.push((
            "include.path",
            attr_case("attr-include", |root, inner| {
                let attrs = root.join("attributes");
                fs::write(&attrs, b"*.c working-tree-encoding=UTF-16\n").unwrap();
                let include = root.join("include");
                fs::write(
                    &include,
                    format!("[core]\n\tattributesFile = {}\n", attrs.display()),
                )
                .unwrap();
                g(
                    inner,
                    &["config", "include.path", include.to_str().unwrap()],
                );
            }),
        ));
        for (case, v) in &results {
            eprintln!("{case}: {v}");
        }
        assert!(
            results.iter().all(|(_, v)| v.starts_with("REFUSED")),
            "{results:#?}"
        );
    }

    // Config scopes: config.worktree and include files set the overridden keys.
    fn config_case(name: &str, setup: impl FnOnce(&Path, &Path)) -> String {
        let (root, outer, inner) = outer_with_nest(name);
        setup(&root, &inner);
        let v = verdict(&outer);
        cleanup(&root);
        v
    }

    #[test]
    fn rv4_overridden_keys_in_every_config_scope_cannot_hide_an_edit() {
        let mut results = Vec::new();
        results.push((
            "config.worktree core.fileMode=false + chmod",
            config_case("cfg-worktree", |_, inner| {
                g(inner, &["config", "extensions.worktreeConfig", "true"]);
                g(inner, &["config", "--worktree", "core.fileMode", "false"]);
                fs::set_permissions(inner.join("lib.c"), fs::Permissions::from_mode(0o755))
                    .unwrap();
            }),
        ));
        results.push((
            "include core.autocrlf=true + CRLF edit",
            config_case("cfg-include-crlf", |root, inner| {
                let include = root.join("include");
                fs::write(&include, b"[core]\n\tautocrlf = true\n").unwrap();
                g(
                    inner,
                    &["config", "include.path", include.to_str().unwrap()],
                );
                fs::write(inner.join("lib.c"), b"v1").unwrap();
                fs::write(inner.join("crlf.txt"), b"line\n").unwrap();
                commit_all(inner, "lf");
                pushed(inner);
                fs::write(inner.join("crlf.txt"), b"line\r\n").unwrap();
            }),
        ));
        results.push((
            "includeIf core.ignoreStat=true + edit",
            config_case("cfg-includeif", |root, inner| {
                let include = root.join("include");
                fs::write(&include, b"[core]\n\tignoreStat = true\n").unwrap();
                g(
                    inner,
                    &[
                        "config",
                        "includeIf.gitdir:**/inner/.git.path",
                        include.to_str().unwrap(),
                    ],
                );
                g(inner, &["update-index", "--really-refresh"]);
                fs::write(inner.join("lib.c"), b"v2-longer").unwrap();
            }),
        ));
        results.push((
            "config.worktree core.symlinks=false + link replaced by file",
            config_case("cfg-symlinks", |_, inner| {
                std::os::unix::fs::symlink("lib.c", inner.join("link")).unwrap();
                commit_all(inner, "link");
                pushed(inner);
                g(inner, &["config", "extensions.worktreeConfig", "true"]);
                g(inner, &["config", "--worktree", "core.symlinks", "false"]);
                fs::remove_file(inner.join("link")).unwrap();
                fs::write(inner.join("link"), b"lib.c").unwrap();
            }),
        ));
        for (case, v) in &results {
            eprintln!("{case}: {v}");
        }
        assert!(
            results.iter().all(|(_, v)| v.starts_with("REFUSED")),
            "{results:#?}"
        );
    }

    // -------------------------------------------------------------------- B2
    // An `AD` submodule (added, then its directory removed) restores to the
    // same status; a directory nest with carried ignored files restores clean.
    #[test]
    fn rv4_ad_state_submodule_restores_its_own_status() {
        let root = fresh("ad-state");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let up = upstream(&root, "up");
        g(
            &outer,
            &["submodule", "add", "-q", up.to_str().unwrap(), "sub"],
        );
        fs::remove_dir_all(outer.join("sub")).unwrap();
        let before = String::from_utf8(g(&outer, &["status", "--porcelain"])).unwrap();
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        );
        let (restored_ok, after, sub_exists) = match export {
            Ok(export) => {
                let restored = root.join("restored");
                let r = restore_bundle(&export.bundle, &restored, "neo");
                let after = if r.is_ok() {
                    String::from_utf8(g(&restored, &["status", "--porcelain"])).unwrap()
                } else {
                    format!("{r:?}")
                };
                (r.is_ok(), after, restored.join("sub").exists())
            }
            Err(e) => (false, format!("export {e:?}"), false),
        };
        cleanup(&root);
        eprintln!("before=[{before}] after=[{after}] sub_exists={sub_exists}");
        assert!(restored_ok, "{after}");
        assert_eq!(before, after);
        assert!(!sub_exists, "restore invented the AD submodule's directory");
    }

    #[test]
    fn rv4_populated_submodule_with_ignored_file_restores_clean() {
        let root = fresh("sub-ignored");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let up = upstream(&root, "up");
        g(
            &outer,
            &["submodule", "add", "-q", up.to_str().unwrap(), "sub"],
        );
        g(&outer, &["commit", "-q", "-m", "sub"]);
        let sub = outer.join("sub");
        g(
            &sub,
            &[
                "remote",
                "set-url",
                "origin",
                "https://example.invalid/u.git",
            ],
        );
        g(&sub, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let exclude = String::from_utf8(g(
            &sub,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "info/exclude",
            ],
        ))
        .unwrap();
        fs::create_dir_all(Path::new(exclude.trim()).parent().unwrap()).unwrap();
        fs::write(exclude.trim(), b"notes.txt\n").unwrap();
        fs::write(sub.join("notes.txt"), b"rv4 ignored in sub").unwrap();
        let export = export_repository_with_policy(
            &outer,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        let notes = fs::read(restored.join("sub/notes.txt")).ok();
        let status = String::from_utf8(g(&restored, &["status", "--porcelain"])).unwrap();
        cleanup(&root);
        assert_eq!(notes.as_deref(), Some(&b"rv4 ignored in sub"[..]));
        assert!(status.is_empty(), "{status}");
    }

    // -------------------------------------------------------------------- B4
    // A nest HEAD that moves A -> B before the export and back B -> A after it:
    // the pre- and post-pass keys agree, the bundle names B. Must refuse.
    #[test]
    fn rv4_nest_head_aba_around_the_export_refuses() {
        use crate::estate;
        let (root, outer, inner) = outer_with_nest("aba");
        let a = head(&inner);
        let plan = root.join("plan");
        estate::add(&plan, &outer, &root.join("dest"), None).unwrap();
        let moved = inner.clone();
        mid_pass::arm_at(&outer, mid_pass::Stage::Snapshot, move || {
            fs::write(moved.join("lib.c"), b"v2").unwrap();
            g(&moved, &["commit", "-q", "-am", "B"]);
        });
        let back = inner;
        mid_pass::arm_at(&outer, mid_pass::Stage::AfterPass, move || {
            g(&back, &["reset", "-q", "--hard", &a]);
        });
        let rows = std::sync::Mutex::new(Vec::new());
        let result = estate::capture(
            &plan,
            &root.join("state"),
            &root.join("corpus"),
            1,
            &|row| {
                rows.lock().unwrap().push((row.outcome, row.reason.clone()));
                Ok(())
            },
        );
        cleanup(&root);
        let rows = rows.into_inner().unwrap();
        assert!(result.is_err(), "{rows:?}");
        assert_eq!(
            rows,
            vec![(
                "refused",
                Some(BulkloadRefusal::GitAuthorityChanged.to_string())
            )]
        );
    }

    // ---------------------------------------------------------------- R-N114
    type Rows = Vec<(String, Option<String>, Vec<String>)>;

    fn estate_run(
        root: &Path,
        items: &[(&Path, &Path)],
        jobs: usize,
    ) -> (Vec<Result<()>>, Rows, Rows) {
        use crate::estate;
        let plan = root.join("plan");
        let added: Vec<Result<()>> = items
            .iter()
            .map(|(source, target)| estate::add(&plan, source, target, Some(target)))
            .collect();
        let capture_rows = std::sync::Mutex::new(Vec::new());
        let apply_rows = std::sync::Mutex::new(Vec::new());
        if plan.exists() {
            let _ = estate::capture(
                &plan,
                &root.join("state"),
                &root.join("corpus"),
                jobs,
                &|row| {
                    capture_rows.lock().unwrap().push((
                        row.outcome.to_owned(),
                        row.reason.clone(),
                        row.nested.clone(),
                    ));
                    Ok(())
                },
            );
            let _ = estate::apply(
                &plan,
                &root.join("corpus"),
                &root.join("applied"),
                "neo",
                jobs,
                &|row| {
                    apply_rows.lock().unwrap().push((
                        row.outcome.to_owned(),
                        row.reason.clone(),
                        row.nested.clone(),
                    ));
                    Ok(())
                },
            );
        }
        (
            added,
            capture_rows.into_inner().unwrap(),
            apply_rows.into_inner().unwrap(),
        )
    }

    fn bare_errno(rows: &Rows) -> bool {
        rows.iter()
            .any(|row| row.1.as_deref().is_some_and(|r| r.starts_with("IO")))
    }

    #[test]
    fn rv4_nest_item_listed_before_the_outer_restores_with_two_jobs() {
        let (root, outer, inner) = outer_with_nest("order-rev");
        let target = root.join("target");
        let nest_target = target.join("vendor/inner");
        let (added, cap, app) = estate_run(&root, &[(&inner, &nest_target), (&outer, &target)], 2);
        let restored = nest_target.join(".git").exists() && target.join("file").exists();
        cleanup(&root);
        eprintln!("added={added:?}\ncapture={cap:#?}\napply={app:#?}");
        assert!(added.iter().all(Result::is_ok));
        assert!(restored, "{app:#?}");
        assert!(cap.iter().chain(&app).all(|row| row.0 != "refused"));
    }

    #[test]
    fn rv4_dotdot_target_inside_the_outer_is_refused_or_typed() {
        let (root, outer, inner) = outer_with_nest("dotdot-target");
        let target = root.join("target");
        let other = upstream(&root, "other");
        // Lexically outside target/, physically target/file: the outer's seat.
        let sneaky = root.join("elsewhere/../target/file");
        fs::create_dir(root.join("elsewhere")).unwrap();
        let (added, _cap, app) = estate_run(&root, &[(&outer, &target), (&other, &sneaky)], 1);
        let _ = inner;
        cleanup(&root);
        eprintln!("added={added:?}\napply={app:#?}");
        let refused_at_add = added
            .get(1)
            .is_some_and(|r| *r == Err(BulkloadRefusal::GitDestinationOccupied));
        assert!(
            refused_at_add || !bare_errno(&app),
            "dot-dot overlap: bare errno at apply: {app:#?}"
        );
    }

    #[test]
    fn rv4_component_prefix_and_trailing_slash() {
        use crate::estate;
        let (root, outer, inner) = outer_with_nest("prefix");
        let other = upstream(&root, "other");
        let target = root.join("T");
        let plan = root.join("plan");
        estate::add(&plan, &outer, &target, Some(&target)).unwrap();
        // T/vendor2 is inside T at a place no nest lives: refused.
        let inside = estate::add(
            &plan,
            &other,
            &root.join("T/vendor2"),
            Some(&root.join("T/vendor2")),
        );
        // T2 is a sibling, not inside T: accepted.
        let sibling = estate::add(&plan, &other, &root.join("T2"), Some(&root.join("T2")));
        // The nest at its own place, with a trailing slash: accepted.
        let slash = PathBuf::from(format!("{}/", root.join("T/vendor/inner").display()));
        let nest = estate::add(&plan, &inner, &slash, Some(&slash));
        cleanup(&root);
        assert_eq!(inside, Err(BulkloadRefusal::GitDestinationOccupied));
        assert_eq!(sibling, Ok(()));
        assert_eq!(nest, Ok(()));
    }
    // core.ignoreCase=true is what Git writes into every repository created on
    // case-insensitive APFS; the repository keeps it when it is moved or
    // restored onto a case-sensitive volume. Status then drops an untracked
    // file whose name case-folds to a tracked one. Only meaningful on a
    // case-sensitive TMPDIR (run with TMPDIR=<case-sensitive volume>, e.g. a
    // case-sensitive APFS image attached with hdiutil); elsewhere it skips.
    #[test]
    fn rv4_core_ignorecase_true_cannot_hide_an_untracked_file() {
        let (root, outer, inner) = outer_with_nest("ignorecase");
        fs::write(inner.join("probe"), b"").unwrap();
        let sensitive = fs::write(inner.join("PROBE"), b"").is_ok()
            && fs::read_dir(&inner).unwrap().count() >= 4;
        let _ = fs::remove_file(inner.join("probe"));
        let _ = fs::remove_file(inner.join("PROBE"));
        if !sensitive {
            cleanup(&root);
            eprintln!("SKIPPED: TMPDIR is case-insensitive");
            return;
        }
        g(&inner, &["config", "core.ignoreCase", "true"]);
        fs::write(inner.join("LIB.c"), b"rv4 untracked only copy").unwrap();
        let v = verdict(&outer);
        cleanup(&root);
        eprintln!("ignorecase verdict: {v}");
        assert!(
            v.starts_with("REFUSED"),
            "untracked LIB.c hidden by core.ignoreCase: {v}"
        );
    }
    // A nest planned as its own item whose own capture refuses (an ignored
    // FIFO: the outer skips the nest's seats, the nest item refuses them).
    #[test]
    fn rv4_nest_planned_but_refused() {
        let (root, outer, inner) = outer_with_nest("planned-refused");
        fs::write(inner.join(".gitignore"), b"*.fifo\nnotes.txt\n").unwrap();
        commit_all(&inner, "ignore");
        pushed(&inner);
        fs::write(inner.join("notes.txt"), b"rv4 ignored only copy").unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(inner.join("p.fifo"))
            .status()
            .unwrap()
            .success());
        let target = root.join("target");
        let nest_target = target.join("vendor/inner");
        let (added, cap, app) = estate_run(&root, &[(&outer, &target), (&inner, &nest_target)], 1);
        let notes_restored = nest_target.join("notes.txt").exists();
        cleanup(&root);
        eprintln!(
            "added={added:?}\ncapture={cap:#?}\napply={app:#?}\nnotes restored={notes_restored}"
        );
        let outer_claims_carrier = cap
            .iter()
            .any(|row| row.0 != "refused" && row.2.iter().any(|l| l.contains("carried-by=")));
        let nest_refused = cap.iter().any(|row| row.0 == "refused");
        assert!(
            !(outer_claims_carrier && nest_refused),
            "the outer names a carrying item whose capture refused; the nest's ignored file is carried by no one"
        );
    }

    // N5, stricter than the reviewer's probe: the outer refuses by type,
    // naming the nest whose own capture refused, and applying the plan never
    // surfaces a bare errno for the missing record.
    #[test]
    fn rv4_nest_planned_but_refused_names_the_refused_carrier() {
        let (root, outer, inner) = outer_with_nest("planned-refused-typed");
        fs::write(inner.join(".gitignore"), b"*.fifo\nnotes.txt\n").unwrap();
        commit_all(&inner, "ignore");
        pushed(&inner);
        fs::write(inner.join("notes.txt"), b"rv4 ignored only copy").unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(inner.join("p.fifo"))
            .status()
            .unwrap()
            .success());
        let target = root.join("target");
        let nest_target = target.join("vendor/inner");
        let (added, cap, app) = estate_run(&root, &[(&outer, &target), (&inner, &nest_target)], 2);
        cleanup(&root);
        assert!(added.iter().all(Result::is_ok), "{added:?}");
        let carrier = BulkloadRefusal::GitNestCarrierRefused(b"vendor/inner".to_vec()).to_string();
        assert!(
            cap.iter()
                .any(|row| row.0 == "refused" && row.1.as_deref() == Some(carrier.as_str())),
            "{cap:#?}"
        );
        assert!(!cap
            .iter()
            .any(|row| row.2.iter().any(|line| line.contains("carried-by="))));
        assert!(!bare_errno(&app), "{app:#?}");
        assert!(app.iter().all(|row| row.0 == "refused"), "{app:#?}");
    }
    // N4, fixed for case-sensitive volumes (CI runs as root on Linux ext4).
    // The volume's case behaviour is probed at run time and the matching
    // outcome asserted. Where case folds, TARGET/vendor/inner is the outer's
    // target/vendor/inner: it must refuse at add or restore cleanly after the
    // outer. Where case matters they are different targets and must not
    // overlap: both are added, the outer restores, and the nest item meets a
    // missing parent (root/TARGET/vendor) as a typed refusal, never an errno.
    #[test]
    fn rv4_case_folded_nest_target_is_refused_or_typed() {
        let (root, outer, inner) = outer_with_nest("case-target");
        let insensitive = {
            fs::write(root.join("probe"), b"").unwrap();
            let folds = root.join("PROBE").exists();
            fs::remove_file(root.join("probe")).unwrap();
            folds
        };
        let target = root.join("target");
        let nest_target = root.join("TARGET/vendor/inner");
        let (added, cap, app) = estate_run(&root, &[(&inner, &nest_target), (&outer, &target)], 1);
        let outer_restored = target.join("file").exists();
        cleanup(&root);
        eprintln!("insensitive={insensitive}\nadded={added:?}\ncapture={cap:#?}\napply={app:#?}");
        assert!(!bare_errno(&app), "{app:#?}");
        if insensitive {
            let refused_at_add = added
                .get(1)
                .is_some_and(|r| *r == Err(BulkloadRefusal::GitDestinationOccupied));
            assert!(
                refused_at_add || app.iter().all(|row| row.0 != "refused"),
                "case-folded overlap neither refused at add nor restored cleanly: {app:#?}"
            );
        } else {
            assert!(added.iter().all(Result::is_ok), "{added:?}");
            assert!(outer_restored, "{app:#?}");
            let parent_missing = BulkloadRefusal::GitDestinationParentMissing.to_string();
            assert!(
                app.iter().any(|row| row.0 == "workspace-restored"),
                "{app:#?}"
            );
            assert!(
                app.iter()
                    .any(|row| row.0 == "refused"
                        && row.1.as_deref() == Some(parent_missing.as_str())),
                "{app:#?}"
            );
        }
    }

    // N4: a restore whose destination's parent does not exist is refused by
    // type, standalone or linked, never as a bare errno.
    #[test]
    fn rv4_missing_destination_parent_is_typed() {
        let (root, outer, _inner) = outer_with_nest("missing-parent");
        let bundle = export_repository(&outer, &root.join("capture")).unwrap();
        let standalone = restore_bundle(&bundle, &root.join("absent/parent/dest"), "neo");
        let linked = restore_linked(&bundle, &outer, &root.join("absent/parent/linked"), "neo");
        cleanup(&root);
        assert_eq!(
            standalone,
            Err(BulkloadRefusal::GitDestinationParentMissing)
        );
        assert_eq!(linked, Err(BulkloadRefusal::GitDestinationParentMissing));
    }
}

// The #53 round-5 adversarial review of dc9f077 (R-N73, R-N114, R-N115,
// TIN-4540), kept as regression tests. Fixtures carry no credential-shaped
// literal (R-N117). R5-1 and R5-2 are deferred and their probes ignored.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::type_complexity,
    clippy::semicolon_if_nothing_returned,
    clippy::redundant_clone,
    clippy::option_if_let_else,
    clippy::literal_string_with_formatting_args,
    clippy::too_many_lines
)]
mod review_pr53e {
    use super::*;

    fn g(repo: &Path, args: &[&str]) -> Vec<u8> {
        let out = git(repo)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn fresh(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rv53e-{name}-{}", std::process::id()));
        if root.exists() {
            let _ = std::process::Command::new("chmod")
                .args(["-R", "u+rwx"])
                .arg(&root)
                .status();
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn cleanup(root: &Path) {
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+rwx"])
            .arg(root)
            .status();
        let _ = fs::remove_dir_all(root);
    }

    fn init(repo: &Path) {
        fs::create_dir_all(repo).unwrap();
        g(repo, &["init", "--template=", "-b", "main"]);
    }

    fn commit_all(repo: &Path, message: &str) {
        g(repo, &["add", "-A"]);
        g(repo, &["commit", "-q", "-m", message]);
    }

    fn pushed(inner: &Path) {
        g(inner, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    }

    fn head(repo: &Path) -> String {
        String::from_utf8(g(repo, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn mkfifo(path: &Path) {
        assert!(std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success());
    }

    fn outer_with_nest(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = fresh(name);
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let inner = outer.join("vendor/inner");
        init(&inner);
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        commit_all(&inner, "v1");
        g(
            &inner,
            &["remote", "add", "origin", "https://example.invalid/i.git"],
        );
        pushed(&inner);
        (root, outer, inner)
    }

    fn upstream(root: &Path, name: &str) -> PathBuf {
        let up = root.join(name);
        init(&up);
        fs::write(up.join("u"), name.as_bytes()).unwrap();
        commit_all(&up, name);
        up
    }

    fn verdict(outer: &Path) -> String {
        match nested_repositories(outer) {
            Ok(n) => format!(
                "CUSTODY {:?}",
                n.iter()
                    .map(NestedRepository::receipt_line)
                    .collect::<Vec<_>>()
            ),
            Err(e) => format!("REFUSED {e:?}"),
        }
    }

    // item, source, outcome, reason, nested lines
    type Rows = Vec<(String, PathBuf, String, Option<String>, Vec<String>)>;

    fn record(rows: &std::sync::Mutex<Rows>, row: &crate::estate::Receipt) {
        rows.lock().unwrap().push((
            row.item.clone(),
            row.source.clone(),
            row.outcome.to_owned(),
            row.reason.clone(),
            row.nested.clone(),
        ));
    }

    fn add_all(root: &Path, items: &[(&Path, &Path)]) -> Vec<Result<()>> {
        let plan = root.join("plan");
        items
            .iter()
            .map(|(source, target)| crate::estate::add(&plan, source, target, Some(target)))
            .collect()
    }

    fn capture_all(root: &Path, jobs: usize) -> Rows {
        let rows = std::sync::Mutex::new(Vec::new());
        let _ = crate::estate::capture(
            &root.join("plan"),
            &root.join("state"),
            &root.join("corpus"),
            jobs,
            &|row| {
                record(&rows, row);
                Ok(())
            },
        );
        rows.into_inner().unwrap()
    }

    fn apply_all(root: &Path, jobs: usize) -> Rows {
        let rows = std::sync::Mutex::new(Vec::new());
        let _ = crate::estate::apply(
            &root.join("plan"),
            &root.join("corpus"),
            &root.join("applied"),
            "neo",
            jobs,
            &|row| {
                record(&rows, row);
                Ok(())
            },
        );
        rows.into_inner().unwrap()
    }

    fn bare_errno(rows: &Rows) -> bool {
        rows.iter()
            .any(|row| row.3.as_deref().is_some_and(|r| r.starts_with("IO")))
    }

    fn row_for<'a>(
        rows: &'a Rows,
        source: &Path,
    ) -> Vec<&'a (String, PathBuf, String, Option<String>, Vec<String>)> {
        let source = fs::canonicalize(source).unwrap();
        rows.iter().filter(|row| row.1 == source).collect()
    }

    // ================================================================ N1
    // A nest with gitlink `s` (to an unrelated upstream commit), committed and
    // pushed, and nothing at the path yet.
    fn nest_with_gitlink(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let (root, outer, inner) = outer_with_nest(name);
        let up = upstream(&root, "up");
        g(
            &inner,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},s", head(&up)),
            ],
        );
        g(&inner, &["commit", "-q", "-m", "gitlink s"]);
        pushed(&inner);
        (root, outer, inner)
    }

    #[test]
    fn rv5_n1_fifo_at_gitlink_path_refuses_by_name() {
        let (root, outer, inner) = nest_with_gitlink("n1-fifo");
        mkfifo(&inner.join("s"));
        let v = verdict(&outer);
        cleanup(&root);
        eprintln!("fifo at gitlink: {v}");
        assert!(v.starts_with("REFUSED GitNestPopulatedSubmodule"), "{v}");
    }

    #[test]
    fn rv5_n1_symlink_to_empty_dir_at_gitlink_path_refuses_by_name() {
        let (root, outer, inner) = nest_with_gitlink("n1-link-empty");
        fs::create_dir(root.join("empty")).unwrap();
        std::os::unix::fs::symlink(root.join("empty"), inner.join("s")).unwrap();
        let v = verdict(&outer);
        cleanup(&root);
        eprintln!("symlink to empty dir at gitlink: {v}");
        assert!(v.starts_with("REFUSED GitNestPopulatedSubmodule"), "{v}");
    }

    #[test]
    fn rv5_n1_gitlink_dir_holding_only_an_ignored_dotfile_refuses() {
        let (root, outer, inner) = nest_with_gitlink("n1-dotfile");
        // The gitlink directory exists first, so `add -A` keeps the gitlink.
        fs::create_dir(inner.join("s")).unwrap();
        fs::write(inner.join(".gitignore"), b".DS_Store\n").unwrap();
        commit_all(&inner, "ignore");
        pushed(&inner);
        assert!(g(&inner, &["ls-files", "-s", "s"]).starts_with(b"160000 "));
        let baseline = verdict(&outer);
        fs::write(inner.join("s/.DS_Store"), b"rv5 finder bytes").unwrap();
        let v = verdict(&outer);
        cleanup(&root);
        eprintln!("baseline: {baseline}\nignored dotfile only: {v}");
        assert!(baseline.starts_with("CUSTODY"), "{baseline}");
        assert!(v.starts_with("REFUSED GitNestPopulatedSubmodule"), "{v}");
    }

    // The gitlink is `a/s`; on disk `a` is a symlink to a directory holding
    // `s` (empty, then populated).
    #[test]
    fn rv5_n1_gitlink_under_a_symlinked_parent_refuses() {
        let (root, outer, inner) = outer_with_nest("n1-symparent");
        let up = upstream(&root, "up");
        g(
            &inner,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},a/s", head(&up)),
            ],
        );
        g(&inner, &["commit", "-q", "-m", "gitlink a/s"]);
        pushed(&inner);
        fs::create_dir_all(root.join("elsewhere/s")).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), inner.join("a")).unwrap();
        let empty = verdict(&outer);
        fs::write(root.join("elsewhere/s/f"), b"rv5 bytes").unwrap();
        let full = verdict(&outer);
        cleanup(&root);
        eprintln!("symlinked parent, empty s: {empty}\nsymlinked parent, filled s: {full}");
        assert!(empty.starts_with("REFUSED"), "{empty}");
        assert!(full.starts_with("REFUSED"), "{full}");
    }

    // A file lands under the empty gitlink directory during the byte pass:
    // the after-census must refuse.
    #[test]
    fn rv5_n1_gitlink_dir_filled_mid_pass_refuses() {
        let (root, outer, inner) = nest_with_gitlink("n1-midpass");
        fs::create_dir(inner.join("s")).unwrap();
        assert!(verdict(&outer).starts_with("CUSTODY"));
        let late = inner.join("s/late");
        mid_pass::arm(&outer, move || fs::write(&late, b"rv5 late bytes").unwrap());
        let result = export_repository(&outer, &root.join("capture"));
        cleanup(&root);
        eprintln!("mid-pass fill: {result:?}");
        assert!(matches!(
            result.err(),
            Some(
                BulkloadRefusal::GitAuthorityChanged
                    | BulkloadRefusal::GitNestPopulatedSubmodule(_)
            )
        ));
    }

    // Observational: an unreadable empty gitlink directory.
    #[test]
    fn rv5_n1_unreadable_gitlink_dir_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let (root, outer, inner) = nest_with_gitlink("n1-unreadable");
        fs::create_dir(inner.join("s")).unwrap();
        fs::write(inner.join("s/hidden"), b"rv5").unwrap();
        fs::set_permissions(inner.join("s"), fs::Permissions::from_mode(0o000)).unwrap();
        let v = verdict(&outer);
        cleanup(&root);
        eprintln!("unreadable gitlink dir holding a file: {v}");
        // R5-5: typed, never a bare Io(13). Root on Linux CI can read it
        // regardless, and the file under it refuses with the same code.
        assert_eq!(
            v,
            format!(
                "REFUSED {:?}",
                BulkloadRefusal::GitNestPopulatedSubmodule(b"vendor/inner/s".to_vec())
            )
        );
    }

    // ================================================================ N5
    // outer -> A = outer/vendor/inner (planned) -> B = A/deep/b (planned,
    // ignored by A). With `fifo`, B holds an ignored FIFO: its capture refuses.
    fn chain(name: &str, fifo: bool) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let (root, outer, a) = outer_with_nest(name);
        fs::write(a.join(".gitignore"), b"deep/\n").unwrap();
        commit_all(&a, "ignore deep");
        pushed(&a);
        let b = a.join("deep/b");
        init(&b);
        fs::write(b.join("x"), b"b").unwrap();
        fs::write(b.join(".gitignore"), b"*.fifo\nnotes.txt\n").unwrap();
        commit_all(&b, "b");
        g(
            &b,
            &["remote", "add", "origin", "https://example.invalid/b.git"],
        );
        pushed(&b);
        fs::write(b.join("notes.txt"), b"rv5 B ignored only copy").unwrap();
        if fifo {
            mkfifo(&b.join("p.fifo"));
        }
        (root, outer, a, b)
    }

    #[test]
    fn rv5_n5_chain_all_clean_restores_every_level() {
        let (root, outer, a, b) = chain("n5-chain-clean", false);
        let target = root.join("target");
        let ta = target.join("vendor/inner");
        let tb = ta.join("deep/b");
        let added = add_all(&root, &[(&outer, &target), (&a, &ta), (&b, &tb)]);
        let cap = capture_all(&root, 2);
        let app = apply_all(&root, 2);
        let restored = (
            target.join("file").exists(),
            ta.join("lib.c").exists(),
            tb.join("x").exists(),
            tb.join("notes.txt").exists(),
        );
        cleanup(&root);
        eprintln!("added={added:?}\ncapture={cap:#?}\napply={app:#?}\nrestored={restored:?}");
        assert!(added.iter().all(Result::is_ok));
        assert!(cap.iter().chain(&app).all(|row| row.2 != "refused"));
        assert_eq!(restored, (true, true, true, true));
    }

    #[test]
    fn rv5_n5_chain_innermost_refusal_propagates_to_every_carrier() {
        let (root, outer, a, b) = chain("n5-chain-refused", true);
        let target = root.join("target");
        let ta = target.join("vendor/inner");
        let tb = ta.join("deep/b");
        let added = add_all(&root, &[(&outer, &target), (&a, &ta), (&b, &tb)]);
        let cap = capture_all(&root, 2);
        let app = apply_all(&root, 2);
        let (ro, ra, rb) = (
            row_for(&cap, &outer)
                .first()
                .map(|r| (r.2.clone(), r.3.clone(), r.4.clone())),
            row_for(&cap, &a)
                .first()
                .map(|r| (r.2.clone(), r.3.clone(), r.4.clone())),
            row_for(&cap, &b)
                .first()
                .map(|r| (r.2.clone(), r.3.clone(), r.4.clone())),
        );
        cleanup(&root);
        eprintln!("added={added:?}\ncapture={cap:#?}\napply={app:#?}");
        assert!(added.iter().all(Result::is_ok));
        assert_eq!(rb.as_ref().unwrap().0, "refused", "{rb:?}");
        assert_eq!(
            ra.as_ref().unwrap().1.as_deref(),
            Some(
                BulkloadRefusal::GitNestCarrierRefused(b"deep/b".to_vec())
                    .to_string()
                    .as_str()
            ),
            "{ra:?}"
        );
        assert_eq!(
            ro.as_ref().unwrap().1.as_deref(),
            Some(
                BulkloadRefusal::GitNestCarrierRefused(b"vendor/inner".to_vec())
                    .to_string()
                    .as_str()
            ),
            "{ro:?}"
        );
        assert!(!cap
            .iter()
            .any(|row| row.4.iter().any(|l| l.contains("carried-by="))));
        assert!(!bare_errno(&app), "{app:#?}");
        assert!(app.iter().all(|row| row.2 == "refused"), "{app:#?}");
    }

    // Capture succeeds for both; the nest item's bundle is then damaged
    // (digest) or removed. The outer applies first (level 0) and names the
    // nest carried-by an item whose own apply refuses.
    fn apply_side(name: &str, damage: &dyn Fn(&Path)) -> (Rows, Rows, bool, bool) {
        let (root, outer, inner) = outer_with_nest(name);
        fs::write(inner.join(".gitignore"), b"notes.txt\n").unwrap();
        commit_all(&inner, "ignore");
        pushed(&inner);
        fs::write(inner.join("notes.txt"), b"rv5 ignored only copy").unwrap();
        let target = root.join("target");
        let nest_target = target.join("vendor/inner");
        let added = add_all(&root, &[(&outer, &target), (&inner, &nest_target)]);
        assert!(added.iter().all(Result::is_ok));
        let cap = capture_all(&root, 1);
        assert!(cap.iter().all(|row| row.2 != "refused"), "{cap:#?}");
        let nest_item = row_for(&cap, &inner).first().unwrap().0.clone();
        for entry in fs::read_dir(root.join("corpus")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with(&nest_item) && name.ends_with(".bundle") {
                damage(&path);
            }
        }
        let app = apply_all(&root, 1);
        let outer_restored = target.join("file").exists();
        let notes = nest_target.join("notes.txt").exists();
        cleanup(&root);
        (cap, app, outer_restored, notes)
    }

    #[test]
    fn rv5_n5_apply_side_carrier_digest_refusal() {
        let (cap, app, outer_restored, notes) = apply_side("n5-apply-digest", &|bundle| {
            let mut bytes = fs::read(bundle).unwrap();
            bytes.push(b'\n');
            fs::write(bundle, bytes).unwrap();
        });
        eprintln!("capture={cap:#?}\napply={app:#?}\nouter restored={outer_restored} nest notes restored={notes}");
        assert!(!bare_errno(&app), "{app:#?}");
    }

    #[test]
    fn rv5_n5_apply_side_carrier_bundle_missing() {
        let (cap, app, outer_restored, notes) = apply_side("n5-apply-missing", &|bundle| {
            fs::remove_file(bundle).unwrap()
        });
        eprintln!("capture={cap:#?}\napply={app:#?}\nouter restored={outer_restored} nest notes restored={notes}");
        assert!(!bare_errno(&app), "{app:#?}");
    }

    // ================================================================ N4
    // `other` must never be restored inside `target` at a place no nest
    // lives: refused at add, or at apply by type.
    fn overlap(
        name: &str,
        sneaky: &dyn Fn(&Path) -> PathBuf,
        setup: &dyn Fn(&Path),
    ) -> (Vec<Result<()>>, Rows, bool) {
        let root = fresh(name);
        let outer = upstream(&root, "outer");
        let other = upstream(&root, "other");
        setup(&root);
        let target = root.join("d1/target");
        fs::create_dir_all(root.join("d1")).unwrap();
        let sneaky = sneaky(&root);
        let added = add_all(&root, &[(&outer, &target), (&other, &sneaky)]);
        let app = if root.join("plan").exists() {
            capture_all(&root, 1);
            apply_all(&root, 1)
        } else {
            Vec::new()
        };
        let mut inside = false;
        if let Ok(entries) = fs::read_dir(&target) {
            for entry in entries {
                let path = entry.unwrap().path();
                if path.join(".git").exists() && path.join("u").exists() {
                    inside |= fs::read(path.join("u")).unwrap() == b"other";
                }
            }
        }
        cleanup(&root);
        (added, app, inside)
    }

    #[test]
    #[ignore = "R5-1 deferred: dangling symlink parent"]
    fn rv5_n4_dangling_symlink_parent_cannot_land_inside_another_target() {
        let (added, app, inside) =
            overlap("n4-dangling", &|root| root.join("alias/sub"), &|root| {
                std::os::unix::fs::symlink(root.join("d1/target"), root.join("alias")).unwrap()
            });
        eprintln!("added={added:?}\napply={app:#?}\nother landed inside target={inside}");
        assert!(
            !inside,
            "a dangling-symlink parent smuggled an item into another target"
        );
    }

    #[test]
    #[ignore = "R5-2 deferred: NFD/Unicode-fold variants need an apply-time canonical re-check"]
    fn rv5_n4_nfd_spelling_cannot_land_inside_an_nfc_target() {
        let root = fresh("n4-nfd2");
        let outer = upstream(&root, "outer");
        let other = upstream(&root, "other");
        let target = root.join("caf\u{e9}");
        let sneaky = root.join("cafe\u{301}/sub");
        let added = add_all(&root, &[(&outer, &target), (&other, &sneaky)]);
        let app = if root.join("plan").exists() {
            capture_all(&root, 1);
            apply_all(&root, 1)
        } else {
            Vec::new()
        };
        let inside = target.join("sub/.git").exists();
        cleanup(&root);
        eprintln!("added={added:?}\napply={app:#?}\nother landed inside target={inside}");
        assert!(
            !inside,
            "an NFD spelling smuggled an item into an NFC target"
        );
    }

    #[test]
    #[ignore = "R5-2 deferred: Unicode case folding (and NFD) on APFS needs an apply-time canonical re-check"]
    fn rv5_n4_non_ascii_case_cannot_land_inside_another_target() {
        let root = fresh("n4-nonascii");
        let outer = upstream(&root, "outer");
        let other = upstream(&root, "other");
        let target = root.join("\u{c9}mile");
        let sneaky = root.join("\u{e9}mile/sub");
        let added = add_all(&root, &[(&outer, &target), (&other, &sneaky)]);
        let app = if root.join("plan").exists() {
            capture_all(&root, 1);
            apply_all(&root, 1)
        } else {
            Vec::new()
        };
        let sensitive = !root.join("\u{e9}mile").exists() || !target.exists();
        let inside = target.join("sub/.git").exists();
        cleanup(&root);
        eprintln!("added={added:?}\napply={app:#?}\nother landed inside target={inside} (volume case-sensitive={sensitive})");
        assert!(
            !inside,
            "a non-ASCII case variant smuggled an item into another target"
        );
    }

    #[test]
    fn rv5_n4_dotdot_after_a_symlink_resolves_physically() {
        let (added, app, inside) = overlap(
            "n4-dotdot-link",
            // lnk -> d1/d2, so lnk/.. is d1 physically (root lexically).
            &|root| root.join("lnk/../target/sub"),
            &|root| {
                fs::create_dir_all(root.join("d1/d2")).unwrap();
                std::os::unix::fs::symlink(root.join("d1/d2"), root.join("lnk")).unwrap();
            },
        );
        eprintln!("added={added:?}\napply={app:#?}\nother landed inside target={inside}");
        assert!(
            added.get(1) == Some(&Err(BulkloadRefusal::GitDestinationOccupied)) || !inside,
            "{added:?}"
        );
    }

    // Observational: a legitimate nest-in-place plan whose nest target is
    // spelled NFD under an NFC outer target.
    #[test]
    fn rv5_n4_legit_nest_spelled_nfd_is_typed() {
        let (root, outer, inner) = outer_with_nest("n4-legit-nfd");
        let target = root.join("caf\u{e9}");
        let nest_target = root.join("cafe\u{301}/vendor/inner");
        let added = add_all(&root, &[(&inner, &nest_target), (&outer, &target)]);
        let cap = capture_all(&root, 1);
        let app = apply_all(&root, 1);
        let restored = target.join("vendor/inner/lib.c").exists();
        cleanup(&root);
        eprintln!("added={added:?}\ncapture={cap:#?}\napply={app:#?}\nnest restored={restored}");
        assert!(!bare_errno(&app), "{app:#?}");
    }

    // ================================================================ merge
    // A checkout with gitlink lib/vendor = X in its index (and HEAD).
    fn gitlinked(name: &str) -> (PathBuf, PathBuf, String, String) {
        let root = fresh(name);
        let source = root.join("source");
        init(&source);
        fs::write(source.join("file"), b"base").unwrap();
        commit_all(&source, "base");
        let x = upstream(&root, "x");
        let y = upstream(&root, "y");
        let (x, y) = (head(&x), head(&y));
        g(
            &source,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{x},lib/vendor"),
            ],
        );
        g(&source, &["commit", "-q", "-m", "gitlink"]);
        fs::create_dir_all(source.join("lib/vendor")).unwrap();
        (root, source, x, y)
    }

    fn gitlinks_of_index(repo: &Path, index: &[u8], scratch: &Path) -> Vec<(Vec<u8>, String)> {
        let path = scratch.join("probe-index");
        fs::write(&path, index).unwrap();
        let listed = output(
            git(repo)
                .env("GIT_INDEX_FILE", &path)
                .args(["ls-files", "-s", "-z"]),
        )
        .unwrap();
        listed
            .split(|b| *b == 0)
            .filter(|e| e.starts_with(b"160000 "))
            .map(|e| {
                let tab = e.iter().position(|b| *b == b'\t').unwrap();
                let oid =
                    String::from_utf8(e[7..tab].split(|b| *b == b' ').next().unwrap().to_vec())
                        .unwrap();
                (e[tab + 1..].to_vec(), oid)
            })
            .collect()
    }

    fn custody_gitlinks(nested: &[NestedRepository]) -> Vec<(Vec<u8>, String)> {
        nested
            .iter()
            .filter(|n| n.kind == NestedRepositoryKind::Gitlink)
            .map(|n| (n.rel_path.clone(), n.head_oid.clone().unwrap()))
            .collect()
    }

    // The live index flips (X -> Y, plus a new gitlink) right after the bytes
    // are read. The key's custody must name exactly the carried bytes' gitlinks.
    #[test]
    fn rv5_merge_key_custody_is_the_carried_index_when_the_live_index_flips() {
        let (root, source, x, y) = gitlinked("merge-key-flip");
        let inside = source.clone();
        let y2 = y.clone();
        mid_pass::arm_at(&source, mid_pass::Stage::IndexRead, move || {
            g(
                &inside,
                &[
                    "update-index",
                    "--cacheinfo",
                    &format!("160000,{y2},lib/vendor"),
                ],
            );
            g(
                &inside,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{y2},lib/other"),
                ],
            );
        });
        let parts = capture_key_parts(&source).unwrap();
        let carried = gitlinks_of_index(&source, &parts.index, &root);
        let custody = custody_gitlinks(parts.nested_repositories());
        let live = gitlinks_of_index(
            &source,
            &fs::read(source.join(".git/index")).unwrap(),
            &root,
        );
        cleanup(&root);
        eprintln!("carried={carried:?}\ncustody={custody:?}\nlive={live:?}");
        assert_eq!(carried, vec![(b"lib/vendor".to_vec(), x.clone())]);
        assert_eq!(custody, carried);
        assert_ne!(live, carried, "the flip did not happen");
        let _ = y;
    }

    // Export with an index A-B-A: B between the index read and the byte pass,
    // A (byte-identical) restored before the end. The custody and the bundle's
    // staged tree must name the same gitlinks.
    #[test]
    fn rv5_merge_export_custody_matches_staged_tree_across_an_index_aba() {
        let (root, source, x, y) = gitlinked("merge-export-aba");
        let index_path = source.join(".git/index");
        let original = fs::read(&index_path).unwrap();
        let inside = source.clone();
        mid_pass::arm_at(&source, mid_pass::Stage::IndexRead, move || {
            g(
                &inside,
                &[
                    "update-index",
                    "--cacheinfo",
                    &format!("160000,{y},lib/vendor"),
                ],
            );
            g(
                &inside,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{y},lib/other"),
                ],
            );
        });
        let restore = index_path.clone();
        mid_pass::arm(&source, move || fs::write(&restore, &original).unwrap());
        let capture = root.join("capture");
        let export = export_repository_with_drift(
            &source,
            &capture,
            &ExportOptions {
                prerequisite: None,
                policy: CapturePolicy::default(),
                reuse: None,
                planned: &[],
                chain: None,
            },
        );
        let outcome = match &export {
            Ok(export) => {
                let private = capture.join("repository.git");
                let staged = String::from_utf8(
                    output(git(&private).args(["rev-parse", "refs/carry-export/staged^{tree}"]))
                        .unwrap(),
                )
                .unwrap();
                let tree =
                    output(git(&private).args(["ls-tree", "-r", "-z", staged.trim()])).unwrap();
                let staged_links: Vec<(Vec<u8>, String)> = tree
                    .split(|b| *b == 0)
                    .filter(|e| e.starts_with(b"160000 "))
                    .map(|e| {
                        let tab = e.iter().position(|b| *b == b'\t').unwrap();
                        let header = std::str::from_utf8(&e[..tab]).unwrap();
                        (
                            e[tab + 1..].to_vec(),
                            header.split_whitespace().nth(2).unwrap().to_owned(),
                        )
                    })
                    .collect();
                Some((custody_gitlinks(&export.nested_repositories), staged_links))
            }
            Err(_) => None,
        };
        cleanup(&root);
        eprintln!(
            "export={:?}\noutcome={outcome:?}",
            export.as_ref().map(|e| e.nested_repositories.len())
        );
        if let Some((custody, staged)) = outcome {
            assert_eq!(custody, staged);
            assert_eq!(custody, vec![(b"lib/vendor".to_vec(), x)]);
        } else {
            assert_eq!(export.err(), Some(BulkloadRefusal::GitAuthorityChanged));
        }
    }

    // The live index flips and stays flipped: the export refuses.
    #[test]
    fn rv5_merge_export_refuses_when_the_live_index_flips_and_stays() {
        let (root, source, _x, y) = gitlinked("merge-export-flip");
        let inside = source.clone();
        mid_pass::arm_at(&source, mid_pass::Stage::IndexRead, move || {
            g(
                &inside,
                &[
                    "update-index",
                    "--cacheinfo",
                    &format!("160000,{y},lib/vendor"),
                ],
            );
        });
        let result = export_repository(&source, &root.join("capture"));
        cleanup(&root);
        assert_eq!(result.err(), Some(BulkloadRefusal::GitAuthorityChanged));
    }

    // N4 on a case-sensitive volume: case-distinct targets are distinct, both
    // accepted at add (no false refusal), and both restore.
    #[test]
    fn rv5_n4_case_distinct_targets_on_a_case_sensitive_volume() {
        let root = fresh("n4-cs-distinct");
        fs::write(root.join("probe"), b"").unwrap();
        let sensitive = !root.join("PROBE").exists();
        if !sensitive {
            cleanup(&root);
            eprintln!("SKIPPED: TMPDIR is case-insensitive");
            return;
        }
        let a = upstream(&root, "a");
        let b = upstream(&root, "b");
        let added = add_all(
            &root,
            &[(&a, &root.join("Tgt")), (&b, &root.join("tgt/sub"))],
        );
        let app = if root.join("plan").exists() {
            capture_all(&root, 1);
            apply_all(&root, 1)
        } else {
            Vec::new()
        };
        cleanup(&root);
        eprintln!("case-sensitive added={added:?}\napply={app:#?}");
        assert!(added.iter().all(Result::is_ok), "{added:?}");
        // tgt does not exist: the second item is typed, never a bare errno.
        assert!(!bare_errno(&app), "{app:#?}");
    }

    // N4 on a case-sensitive volume: the nest lives at `Vendor/inner`; a plan
    // that restores it at T/vendor/inner (a different place on this volume)
    // is not "lined up" and should refuse at add.
    #[test]
    fn rv5_n4_source_relation_is_not_folded_on_a_case_sensitive_volume() {
        let root = fresh("n4-cs-source-fold");
        fs::write(root.join("probe"), b"").unwrap();
        if root.join("PROBE").exists() {
            cleanup(&root);
            eprintln!("SKIPPED: TMPDIR is case-insensitive");
            return;
        }
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        commit_all(&outer, "outer");
        let inner = outer.join("Vendor/inner");
        init(&inner);
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        commit_all(&inner, "v1");
        let target = root.join("T");
        let added = add_all(
            &root,
            &[(&outer, &target), (&inner, &root.join("T/vendor/inner"))],
        );
        cleanup(&root);
        eprintln!("case-sensitive source-fold added={added:?}");
        assert_eq!(
            added.get(1),
            Some(&Err(BulkloadRefusal::GitDestinationOccupied))
        );
    }
}

// #106 (OI-1002-Q25): intent-to-add entries are carried as index custody,
// and GIT_INVENTORY_MALFORMED is split by cause.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod intent_to_add_106 {
    use super::*;

    fn g(repo: &Path, args: &[&str]) -> Vec<u8> {
        output(git(repo).args(args)).unwrap()
    }

    fn fresh(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ita106-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn init(repo: &Path) {
        fs::create_dir_all(repo).unwrap();
        g(repo, &["init", "--template=", "-b", "main"]);
        g(repo, &["config", "user.name", "T"]);
        g(repo, &["config", "user.email", "t@localhost"]);
        g(repo, &["config", "commit.gpgsign", "false"]);
    }

    // A committed checkout whose index holds three intent-to-add entries: a
    // plain file, an executable in a subdirectory and a force-added ignored
    // file, beside an ordinary staged edit and an untracked file.
    fn checkout_with_intent_to_add(root: &Path) -> PathBuf {
        let source = root.join("source");
        init(&source);
        fs::write(source.join("tracked"), b"v1").unwrap();
        fs::write(source.join(".gitignore"), b"ignored.txt\n").unwrap();
        g(&source, &["add", "tracked", ".gitignore"]);
        g(&source, &["commit", "-q", "-m", "base"]);
        fs::write(source.join("tracked"), b"v2 staged").unwrap();
        g(&source, &["add", "tracked"]);
        fs::write(source.join("new"), b"intent to add").unwrap();
        fs::create_dir(source.join("bin")).unwrap();
        fs::write(source.join("bin/run"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(
            source.join("bin/run"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        fs::write(source.join("ignored.txt"), b"ignored but intended").unwrap();
        fs::write(source.join("untracked"), b"plain untracked").unwrap();
        g(&source, &["add", "-N", "new", "bin/run"]);
        g(&source, &["add", "-N", "-f", "ignored.txt"]);
        source
    }

    fn status(repo: &Path) -> Vec<u8> {
        g(repo, &["status", "--porcelain=v2", "--untracked-files=all"])
    }

    #[test]
    fn intent_to_add_entries_capture_and_restore_with_equal_status() {
        let root = fresh("standalone");
        let source = checkout_with_intent_to_add(&root);
        let before = status(&source);
        assert_eq!(
            before
                .split(|b| *b == b'\n')
                .filter(|line| line.starts_with(b"1 .A "))
                .count(),
            3,
            "{}",
            String::from_utf8_lossy(&before)
        );
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        let after = status(&restored);
        assert_eq!(
            blake3::hash(&before),
            blake3::hash(&after),
            "before:\n{}\nafter:\n{}",
            String::from_utf8_lossy(&before),
            String::from_utf8_lossy(&after)
        );
        let custody = carried_intent_to_add(
            &restored,
            &text(git(&restored).args(["bundle", "list-heads"]).arg(&bundle)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            custody
                .iter()
                .map(|entry| (entry.rel_path.as_slice(), entry.mode, entry.empty_blob))
                .collect::<Vec<_>>(),
            vec![
                (b"bin/run".as_slice(), 0o100_755, true),
                (b"ignored.txt".as_slice(), 0o100_644, true),
                (b"new".as_slice(), 0o100_644, true),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn intent_to_add_entries_restore_into_a_linked_worktree() {
        let root = fresh("linked");
        let source = checkout_with_intent_to_add(&root);
        let before = status(&source);
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let repository = root.join("repository");
        init(&repository);
        let destination = root.join("linked");
        restore_linked(&bundle, &repository, &destination, "neo").unwrap();
        assert_eq!(status(&destination), before);
        fs::remove_dir_all(root).unwrap();
    }

    // A capture without intent-to-add entries carries no custody ref, so
    // every other capture's bundle is unchanged by #106.
    #[test]
    fn a_capture_without_intent_to_add_records_no_custody() {
        let root = fresh("none");
        let source = root.join("source");
        init(&source);
        fs::write(source.join("tracked"), b"v1").unwrap();
        g(&source, &["add", "tracked"]);
        g(&source, &["commit", "-q", "-m", "base"]);
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let heads = text(git(&source).args(["bundle", "list-heads"]).arg(&bundle)).unwrap();
        assert!(!heads.contains(INTENT_TO_ADD_METADATA), "{heads}");
        fs::remove_dir_all(root).unwrap();
    }

    // Git can mark only an existing path intent-to-add: an entry whose seat
    // is gone refuses by cause, not as a malformed inventory.
    #[test]
    fn an_intent_to_add_entry_without_its_seat_refuses_by_cause() {
        let root = fresh("seatless");
        let source = checkout_with_intent_to_add(&root);
        fs::remove_file(source.join("new")).unwrap();
        assert_eq!(
            export_repository(&source, &root.join("capture")),
            Err(BulkloadRefusal::GitInventoryIntentToAdd)
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Other entry flags can hide an edit from status and still refuse as a
    // malformed inventory.
    #[test]
    fn skip_worktree_and_assume_unchanged_still_refuse_as_malformed() {
        for flag in ["--skip-worktree", "--assume-unchanged"] {
            let root = fresh(flag.trim_start_matches('-'));
            let source = checkout_with_intent_to_add(&root);
            g(&source, &["update-index", flag, "tracked"]);
            assert_eq!(
                export_repository(&source, &root.join("capture")),
                Err(BulkloadRefusal::GitInventoryMalformed),
                "{flag}"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    // A nest's index is not carried, so its intent-to-add entries refuse by
    // cause.
    #[test]
    fn intent_to_add_in_a_nest_refuses_by_cause() {
        let root = fresh("nest");
        let outer = root.join("outer");
        init(&outer);
        fs::write(outer.join("file"), b"outer").unwrap();
        g(&outer, &["add", "file"]);
        g(&outer, &["commit", "-q", "-m", "outer"]);
        let inner = outer.join("vendor/inner");
        init(&inner);
        fs::write(inner.join("lib.c"), b"v1").unwrap();
        g(&inner, &["add", "lib.c"]);
        g(&inner, &["commit", "-q", "-m", "v1"]);
        fs::write(inner.join("extra.c"), b"intended").unwrap();
        g(&inner, &["add", "-N", "extra.c"]);
        assert_eq!(
            nested_repositories(&outer),
            Err(BulkloadRefusal::GitInventoryIntentToAdd)
        );
        fs::remove_dir_all(root).unwrap();
    }

    // #131 (1): `git add -N f; chmod +x f` keeps 100644 in the index while
    // the seat is 0755, so a restore's `git add -N` would read 100755. The
    // capture refuses by cause, before anything is restored; restoring the
    // executable bit lets the same checkout capture and restore.
    #[test]
    fn an_intent_to_add_entry_whose_seat_changed_mode_refuses_at_capture() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = fresh("mode-change");
        let source = checkout_with_intent_to_add(&root);
        fs::set_permissions(source.join("new"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            export_repository(&source, &root.join("capture")),
            Err(BulkloadRefusal::GitInventoryIntentToAdd)
        );
        assert!(!root.join("restored").exists());
        fs::set_permissions(source.join("new"), fs::Permissions::from_mode(0o644)).unwrap();
        let before = status(&source);
        let bundle = export_repository(&source, &root.join("capture-clean")).unwrap();
        restore_bundle(&bundle, &root.join("restored"), "neo").unwrap();
        assert_eq!(status(&root.join("restored")), before);
        fs::remove_dir_all(root).unwrap();
    }

    // #131 (1): the restore-side check, for custody recorded before the
    // capture-side one: an entry the worktree tree restores at another mode,
    // or not at all, refuses before any worktree byte is laid down.
    #[test]
    fn intent_to_add_custody_is_checked_against_the_worktree_tree() {
        let entry = |path: &[u8], mode| IntentToAdd {
            rel_path: path.to_vec(),
            mode,
            empty_blob: true,
        };
        let blob = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let tree = format!(
            "100755 blob {blob}\tbin/run\0100644 blob {blob}\tnew\0120000 blob {blob}\tlink\0"
        );
        let tree = tree.as_bytes();
        assert_eq!(intent_to_add_restorable(&[], b"garbage"), Ok(()));
        assert_eq!(
            intent_to_add_restorable(
                &[
                    entry(b"bin/run", 0o100_755),
                    entry(b"new", 0o100_644),
                    entry(b"link", 0o120_000)
                ],
                tree
            ),
            Ok(())
        );
        for custody in [
            entry(b"new", 0o100_755),
            entry(b"bin/run", 0o100_644),
            entry(b"absent", 0o100_644),
        ] {
            assert_eq!(
                intent_to_add_restorable(std::slice::from_ref(&custody), tree),
                Err(BulkloadRefusal::GitInventoryIntentToAdd),
                "{custody:?}"
            );
        }
    }

    // #131 (2): fsmonitor's validity bit is masked before an entry is
    // classified; every other flag still refuses as a malformed inventory.
    #[test]
    fn the_fsmonitor_valid_bit_is_masked_from_entry_flags() {
        assert_eq!(entry_flags("0"), Ok(EntryFlags::Plain));
        assert_eq!(entry_flags("200000"), Ok(EntryFlags::Plain));
        assert_eq!(entry_flags("20004000"), Ok(EntryFlags::IntentToAdd));
        assert_eq!(entry_flags("20204000"), Ok(EntryFlags::IntentToAdd));
        for flags in [
            "8000", "208000", "40004000", "40204000", "1000", "4000", "", "zz",
        ] {
            assert_eq!(
                entry_flags(flags),
                Err(BulkloadRefusal::GitInventoryMalformed),
                "{flags}"
            );
        }
    }

    // #131 (2): a checkout whose own config enables a hook fsmonitor, and
    // whose index carries the fsmonitor extension and validity bits, captures
    // and restores its intent-to-add entries. The hook never runs under the
    // capture or the restore, and the restored index carries no fsmonitor
    // state.
    #[test]
    fn a_checkout_with_fsmonitor_state_carries_intent_to_add() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = fresh("fsmonitor");
        let source = checkout_with_intent_to_add(&root);
        let marker = root.join("fsmonitor-ran");
        let hook = root.join("fsmonitor-hook");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\n: > '{}'\nprintf 'token-1\\0'\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        g(
            &source,
            &["config", "core.fsmonitor", hook.to_str().unwrap()],
        );
        // Git as a person runs it, honouring the repository's fsmonitor, so
        // the index gains the extension and the validity bits.
        let plain = |args: &[&str]| {
            let done = std::process::Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(done.status.success(), "{args:?}");
            done.stdout
        };
        plain(&["status", "--porcelain=v2"]);
        plain(&["status", "--porcelain=v2"]);
        let debug = String::from_utf8(plain(&["ls-files", "--debug"])).unwrap();
        assert!(debug.contains("flags: 200000"), "{debug}");
        let index = fs::read(source.join(".git/index")).unwrap();
        assert!(index.windows(4).any(|window| window == b"FSMN"));
        fs::remove_file(&marker).unwrap();
        let before = status(&source);
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        assert_eq!(status(&restored), before);
        assert!(!marker.exists(), "the source's fsmonitor hook ran");
        let index = fs::read(restored.join(".git/index")).unwrap();
        assert!(!index.windows(4).any(|window| window == b"FSMN"));
        fs::remove_dir_all(root).unwrap();
    }

    // A thin bundle whose prerequisite the repository lacks refuses
    // GIT_INVENTORY_MISSING_PREREQUISITE; once its commits are fetched the
    // same bundle verifies.
    #[test]
    fn a_missing_prerequisite_refuses_by_cause() {
        let root = fresh("prerequisite");
        let source = root.join("source");
        init(&source);
        fs::write(source.join("tracked"), b"v1").unwrap();
        g(&source, &["add", "tracked"]);
        g(&source, &["commit", "-q", "-m", "one"]);
        fs::write(source.join("tracked"), b"v2").unwrap();
        g(&source, &["commit", "-q", "-a", "-m", "two"]);
        let thin = root.join("thin.bundle");
        g(
            &source,
            &["bundle", "create", thin.to_str().unwrap(), "HEAD~1..main"],
        );
        assert_eq!(
            shared::prerequisites(&thin).unwrap(),
            vec![text(git(&source).args(["rev-parse", "HEAD~1"])).unwrap()]
        );
        let destination = root.join("destination");
        init(&destination);
        assert_eq!(
            verify_bundle(&destination, &thin),
            Err(BulkloadRefusal::GitInventoryMissingPrerequisite)
        );
        assert_eq!(
            import_bundle(&destination, &thin, "neo"),
            Err(BulkloadRefusal::GitInventoryMissingPrerequisite)
        );
        g(
            &destination,
            &[
                "fetch",
                "-q",
                "--no-tags",
                source.to_str().unwrap(),
                "refs/heads/main:refs/prereq",
            ],
        );
        assert_eq!(verify_bundle(&destination, &thin), Ok(()));
        // A self-contained bundle declares no prerequisite.
        let full = root.join("full.bundle");
        g(
            &source,
            &["bundle", "create", full.to_str().unwrap(), "main"],
        );
        assert!(shared::prerequisites(&full).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
