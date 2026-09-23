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

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{BulkloadRefusal, Result};

mod batch_objects;
mod raw_tree;
pub mod registered;
mod shallow;
pub mod shared;

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
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
    command
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "pack.threads=2",
            "-c",
            "pack.windowMemory=64m",
            "-C",
        ])
        .arg(repo);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}

fn output(command: &mut Command) -> Result<Vec<u8>> {
    let Output { status, stdout, .. } = command.output()?;
    if !status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(stdout)
}

fn text(command: &mut Command) -> Result<String> {
    String::from_utf8(output(command)?)
        .map(|s| s.trim_end().to_owned())
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)
}

fn input(command: &mut Command, bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or(BulkloadRefusal::Io(None))?
        .write_all(bytes)?;
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(result.stdout)
}

fn metadata(private: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let value = input(git(private).args(["hash-object", "-w", "--stdin"]), bytes)?;
    let value = std::str::from_utf8(&value)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim();
    let tree = input(
        git(private).args(["mktree", "-z"]),
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

fn commit_tree(private: &Path, tree: &str, label: &str) -> Result<String> {
    text(
        git(private)
            .env("GIT_AUTHOR_NAME", "Bulkload archival capture")
            .env("GIT_AUTHOR_EMAIL", "bulkload@localhost")
            .env("GIT_COMMITTER_NAME", "Bulkload archival capture")
            .env("GIT_COMMITTER_EMAIL", "bulkload@localhost")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .args(["commit-tree", tree, "-m", label]),
    )
}

// Build a raw tree without running attributes/filters or starting Git per file.
fn capture_tree(private: &Path, repo: &Path, _index: &Path) -> Result<String> {
    let rows = filesystem_rows(repo)?;
    let (tree, _) = raw_tree::capture(private, repo, &rows)?;
    if rows != filesystem_rows(repo)? {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    Ok(tree)
}

fn capture_refs(repo: &Path, private: &Path, inventory: &str) -> Result<()> {
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
    let stash = output(git(repo).args(["reflog", "show", "--format=%H", "refs/stash"]));
    if let Ok(stash) = stash {
        let stash = String::from_utf8(stash).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        for value in stash.lines() {
            if !oid(value) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            let name = format!("refs/carry-export/stashes/{value}");
            pending.entry(name).or_insert_with(|| value.to_owned());
        }
    } else if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
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
    Ok(reusable_capture_key_with_custody(repo, policy)?.0)
}

/// [`reusable_capture_key_with_policy`], plus the nested-repository custody
/// that key was computed over.
///
/// A receipt for a reuse hit names exactly the nests the retained capture
/// recorded: equal keys mean an equal custody sidecar (R-N73).
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn reusable_capture_key_with_custody(
    repo: &Path,
    policy: CapturePolicy,
) -> Result<([u8; 32], Vec<NestedRepository>)> {
    use std::os::unix::ffi::OsStrExt;
    let repo = fs::canonicalize(repo)?;
    let common = common_repository(&repo)?;
    let inventory = refs(&repo)?;
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let symbolic = git(&repo).args(["symbolic-ref", "-q", "HEAD"]).output()?;
    if !symbolic.status.success() && symbolic.status.code() != Some(1) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let (_, index, gitlinks) = source_index(&repo)?;
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
    // A sidecar, hashed only when present: repos without nested worktrees keep
    // their existing keys, so retained captures are not re-done for a schema.
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
    // A sidecar, hashed only when present: repos without a foreign nested
    // repository or a gitlink keep their exact keys. The nested HEAD is part of
    // it: a foreign object we do not carry moving is still drift to name.
    let nested_repositories = nested_custody(&census.nested_repositories, &gitlinks);
    if !nested_repositories.is_empty() {
        let nested =
            postcard::to_allocvec(&nested_repositories).map_err(|_| BulkloadRefusal::FrameCodec)?;
        hash.update(NESTED_REPOSITORIES_DOMAIN);
        hash.update(
            &u64::try_from(nested.len())
                .map_err(|_| BulkloadRefusal::BudgetExceeded)?
                .to_le_bytes(),
        );
        hash.update(&nested);
    }
    // A sidecar, hashed only when present, and only the omitted roots -- never
    // their sizes. A repository with no omission keeps the key it already had,
    // and a build writing inside an omitted root cannot move this key, which is
    // the whole point: rebuildable churn must not refuse a 40-minute capture.
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
    Ok((*hash.finalize().as_bytes(), nested_repositories))
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
    repair_missing_index_inner(bundle, repo, source, receipt, |_| Ok(()))
}

fn repair_missing_index_inner(
    bundle: &Path,
    repo: &Path,
    source: &str,
    receipt: &Path,
    before_publish: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let repo = fs::canonicalize(repo)?;
    let bundle = fs::canonicalize(bundle)?;
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
    let heads = shallow::headers(&repo, &bundle)?;
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
    fs::File::open(receipt.join("original-administration.postcard"))?.sync_all()?;
    fs::File::open(&receipt)?.sync_all()?;
    fs::File::open(&receipt_parent)?.sync_all()?;
    import_bundle(&repo, &bundle, source)?;
    // A gitlink in the staged tree is the captured index entry, restored as
    // is: reading it needs no submodule commit (R-N73, B2).
    let private_index = receipt.join("captured.index");
    output(
        git(&repo)
            .env("GIT_INDEX_FILE", &private_index)
            .args(["read-tree", &format!("{}^{{tree}}", find("staged")?)]),
    )?;
    fs::set_permissions(&private_index, fs::Permissions::from_mode(0o600))?;
    fs::File::open(&private_index)?.sync_all()?;
    fs::File::open(&receipt)?.sync_all()?;
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
    fs::File::open(&admin)?.sync_all()?;
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
        fs::File::open(self.path.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?)?.sync_all()?;
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
    Ok(export_repository_inner(repo, capture, None, CapturePolicy::default())?.bundle)
}

/// Export one worktree under an explicit capture policy and prerequisite.
///
/// The returned [`Export`] names every omitted rebuildable root and its
/// measured size, and every nested repository or gitlink whose content the
/// capture does not carry: the same custody the bundle's metadata refs carry.
///
/// # Errors
/// Refuses everything [`export_repository`] refuses, plus invalid prerequisites.
pub fn export_repository_with_policy(
    repo: &Path,
    capture: &Path,
    prerequisite: Option<&Path>,
    policy: CapturePolicy,
) -> Result<Export> {
    export_repository_inner(repo, capture, prerequisite, policy)
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
    Ok(export_repository_inner(repo, capture, Some(base), CapturePolicy::default())?.bundle)
}

fn export_repository_inner(
    repo: &Path,
    capture: &Path,
    prerequisite: Option<&Path>,
    policy: CapturePolicy,
) -> Result<Export> {
    use std::os::unix::fs::DirBuilderExt;
    let repo = fs::canonicalize(repo)?;
    fs::DirBuilder::new().mode(0o700).create(capture)?;
    let capture = fs::canonicalize(capture)?;
    if capture.starts_with(&repo) {
        return Err(BulkloadRefusal::GitAuthorityOutsideRoot);
    }
    let before_refs = refs(&repo)?;
    let configuration = source_configuration(&repo)?;
    let boundary = shallow::frontier(&repo)?;
    let common = common_repository(&repo)?;
    let census = capture_census(&repo, &common, policy)?;
    let seats = &census.rows;
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let (index_path, before_index, gitlinks) = source_index(&repo)?;
    let private = prepare_private(&repo, &capture)?;
    capture_refs(&repo, &private, &before_refs)?;
    set_ref(&private, "refs/carry-export/head", &head)?;
    let symbolic_head = text(git(&repo).args(["symbolic-ref", "-q", "HEAD"])).unwrap_or_default();
    metadata(&private, "head-symbolic", symbolic_head.as_bytes())?;
    let exclude_path = PathBuf::from(text(git(&repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?);
    let exclude = match fs::read(exclude_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    metadata(&private, "exclude", &exclude)?;
    if !boundary.is_empty() {
        metadata(&private, "shallow-frontier-v1", &boundary)?;
    }
    metadata(
        &private,
        "configuration-v1",
        &postcard::to_allocvec(&configuration).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    let index = capture.join("index");
    fs::write(&index, &before_index)?;
    let staged = text(snapshot_command(&private, &repo, &index).arg("write-tree"))?;
    set_ref(
        &private,
        "refs/carry-export/staged",
        &commit_tree(&private, &staged, "bulkload staged tree")?,
    )?;
    let (tree, _) = raw_tree::capture(&private, &repo, seats)?;
    if before_refs != refs(&repo)?
        || configuration != source_configuration(&repo)?
        || boundary != shallow::frontier(&repo)?
        || before_index != fs::read(index_path)?
        || head != text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?
        || census != capture_census(&repo, &common, policy)?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    set_ref(
        &private,
        "refs/carry-export/worktree",
        &commit_tree(
            &private,
            &tree,
            "bulkload worktree including untracked and ignored files",
        )?,
    )?;
    metadata(
        &private,
        "filesystem-v1",
        &postcard::to_allocvec(seats).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    // Typed custody for registered worktrees nested inside this checkout. Their
    // bytes are captured as their own estate items; this manifest names them so
    // the receipt and the parity audit can account for the skipped subtrees.
    if !census.nested_worktrees.is_empty() {
        metadata(
            &private,
            NESTED_WORKTREES_METADATA,
            &postcard::to_allocvec(&census.nested_worktrees)
                .map_err(|_| BulkloadRefusal::FrameCodec)?,
        )?;
    }
    // Typed custody for foreign repositories nested under this checkout and for
    // gitlink index entries. Neither's content is carried: a nested repository
    // (a vendored checkout, a tool cache, a build dependency, a populated
    // submodule) must be captured as its own estate item, and a gitlink names a
    // commit this bundle does not hold (the entry itself is in the staged
    // tree). Directory nests hold still or the census comparison above
    // refuses; gitlinks hold still or the index does.
    let nested_repositories = nested_custody(&census.nested_repositories, &gitlinks);
    nested_repositories_metadata(&private, &nested_repositories)?;
    // Omission is recorded, never silent. Sizes are measured once, here, and
    // deliberately excluded from both the reusable key and the before/after
    // census comparison: they are custody evidence about bytes this capture
    // chose not to carry, not an assertion that those bytes held still.
    let omitted = measure_omissions(&repo, &census.omitted)?;
    if !omitted.is_empty() {
        metadata(
            &private,
            REBUILDABLE_METADATA,
            &postcard::to_allocvec(&omitted).map_err(|_| BulkloadRefusal::FrameCodec)?,
        )?;
    }
    let bundle = capture.join("capture.bundle");
    shared::write_bundle(&private, &bundle, prerequisite)?;
    output(git(&private).args(["bundle", "verify"]).arg(&bundle))?;
    Ok(Export {
        bundle,
        omitted,
        nested_repositories,
    })
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

/// A completed export: the bundle, and the custody for what it did not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The written and verified capture bundle.
    pub bundle: PathBuf,
    /// Rebuildable roots omitted from the capture, with measured sizes.
    pub omitted: Vec<RebuildableOmission>,
    /// Foreign nested repositories and gitlinks whose content is not carried.
    pub nested_repositories: Vec<NestedRepository>,
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
    filesystem_census(root, Some(common), policy)
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
                    "nested-repository path=\"{path}\" kind=Directory gitdir={:?} admin=\"{}\" head={head} unpushed={unpushed} remotes={} ignored-carried={}",
                    self.gitdir_kind,
                    self.admin.escape_ascii(),
                    if self.remotes { "yes" } else { "none" },
                    self.ignored_carried,
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
fn foreign_nest(
    root: &Path,
    directory: &Path,
    common: &Path,
    outer: &OuterIndex,
    policy: CapturePolicy,
    rel_path: Vec<u8>,
    gitdir_kind: GitdirKind,
) -> Result<(NestedRepository, NestSeats)> {
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
    let seats = nest_ignored_seats(root, directory, &rel_path, &ignored, policy)?;
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
    let flagged = debug
        .split(|b| *b == b'\n')
        .filter_map(|line| {
            line.windows(8)
                .position(|window| window == b"\tflags: ")
                .and_then(|at| line.get(at + 8..))
        })
        .any(|flags| flags != b"0");
    if flagged {
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
    let mut overrides: Vec<(std::ffi::OsString, &str)> = vec![
        ("core.fsmonitor".into(), "false"),
        ("core.untrackedCache".into(), "false"),
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
    for marker in MARKERS {
        let path = text(git(directory).args([
            "--git-dir=.git",
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            marker,
        ]))?;
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
    Ok(filesystem_census(root, None, CapturePolicy::including_rebuildable())?.rows)
}

// A checkout census: registered worktrees of `common` and foreign repositories
// nested below `root` are recorded as custody and not descended; a malformed
// nested .git still refuses. Full fidelity: nothing rebuildable is omitted.
fn repository_census(root: &Path, common: &Path) -> Result<Census> {
    filesystem_census(root, Some(common), CapturePolicy::including_rebuildable())
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
    common: &Path,
    outer: &OuterIndex,
    policy: CapturePolicy,
) -> Result<Option<NestedCustody>> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
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
        let (custody, seats) = foreign_nest(
            root,
            directory,
            common,
            outer,
            policy,
            rel_path()?,
            GitdirKind::Directory,
        )?;
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
        let (custody, seats) = foreign_nest(
            root,
            directory,
            common,
            outer,
            policy,
            rel_path()?,
            GitdirKind::PointerFile,
        )?;
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

fn filesystem_census(root: &Path, common: Option<&Path>, policy: CapturePolicy) -> Result<Census> {
    use bulkload_proto::FileKind;
    use std::os::unix::ffi::OsStrExt;
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
                    .map(|common| nested_administration(root, &path, common, &outer, policy))
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
                file.sync_all()?;
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
        directory.sync_all()?;
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
            .sync_all()?;
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
    let before_index = fs::read(&index_path)?;
    let flags = text(git(repo).args(["ls-files", "--debug"]))?;
    if flags
        .lines()
        .filter_map(|line| line.split_once("\tflags: "))
        .any(|(_, flags)| flags != "0")
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    if !text(git(repo).args(["rev-parse", "--shared-index-path"]))?.is_empty() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let entries = output(git(repo).args(["ls-files", "--stage", "-z"]))?;
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
        });
    }
    Ok(())
}

fn prepare_private(repo: &Path, capture: &Path) -> Result<PathBuf> {
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
        format!("{objects}\n"),
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
    if !source_slug(source) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let bundle = fs::canonicalize(bundle)?;
    output(git(repo).args(["bundle", "verify"]).arg(&bundle))?;
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
            fs::File::open(entry.path())?.sync_all()?;
        } else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    fs::File::open(root)?.sync_all()?;
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
    import_bundle(&private, &retained, source)?;
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
    import_bundle(repository, &retained, source)?;
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
    fs::File::open(admin.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?.sync_all()?;
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
    fs::File::open(&receipt_parent)?.sync_all()?;
    let admin = if let Some(repository) = &repository {
        prepare_linked_attachment(repository, &destination, source, &receipt, &private, &heads)?
    } else {
        let (from, to) = mapping.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        activate_standalone_configuration(&private, &heads, &receipt, Some((from, to)))?;
        private.clone()
    };
    let pointer = write_git_pointer(&receipt, &admin)?;
    sync_private_tree(&receipt)?;
    fs::File::open(&receipt_parent)?.sync_all()?;
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
    fs::File::open(&destination)?.sync_all()?;
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
    fs::File::open(admin)?.sync_all()?;
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
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let bundle = fs::canonicalize(bundle)?;
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let destination = fs::canonicalize(destination)?;
    // Read bundle headers without assuming the destination's object format.
    let heads = text(
        git(&destination)
            .args(["bundle", "list-heads"])
            .arg(&bundle),
    )?;
    if !shallow::is_custody(&heads) {
        capture_revision(&heads, "configuration-v1")?;
    }
    let format = bundle_object_format(&heads)?;
    output(git(&destination).args(["init", "--template=", &format!("--object-format={format}")]))?;
    import_bundle(&destination, &bundle, source)?;
    let heads = shallow::headers(&destination, &bundle)?;
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
    restore_entries(&destination, &entries)?;
    let staged = find("staged")?;
    restore_gitlink_directories(&destination, &staged)?;
    output(git(&destination).args(["read-tree", &format!("{staged}^{{tree}}")]))?;
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
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    if destination.symlink_metadata().is_ok() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let parent = fs::canonicalize(
        destination
            .parent()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    )?;
    let destination = parent.join(
        destination
            .file_name()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    );
    let bundle = fs::canonicalize(bundle)?;
    let repository = fs::canonicalize(repository)?;
    import_bundle(&repository, &bundle, source)?;
    let heads = shallow::headers(&repository, &bundle)?;
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
    let entries = output(git(&destination).args(["ls-tree", "-r", "-z", &find("worktree")?]))?;
    restore_entries(&destination, &entries)?;
    restore_gitlink_directories(&destination, &find("staged")?)?;
    output(git(&destination).args(["read-tree", &format!("{}^{{tree}}", find("staged")?)]))?;
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

// N7 (R-N73): a gitlink's content is its own estate item, but its seat is
// not: without a directory at the gitlink path Git reports the submodule as
// deleted. An empty directory is exactly what `git clone` without
// --recurse-submodules leaves. Created before the captured modes are applied,
// so a read-only parent is still writable here; existing directories (an
// unpopulated submodule's captured seat) are kept.
fn restore_gitlink_directories(destination: &Path, staged: &str) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    let entries = output(git(destination).args(["ls-tree", "-r", "-z", staged]))?;
    for entry in entries
        .split(|b| *b == 0)
        .filter(|entry| entry.starts_with(b"160000 "))
    {
        let relative = entry
            .iter()
            .position(|b| *b == b'\t')
            .and_then(|tab| entry.get(tab + 1..))
            .map(|path| Path::new(std::ffi::OsStr::from_bytes(path)))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if relative.as_os_str().is_empty() || relative.components().any(|part| !matches!(part, Component::Normal(name) if !name.as_bytes().eq_ignore_ascii_case(b".git"))) {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let mut current = destination.to_path_buf();
        for part in relative.components() {
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
    }
    Ok(())
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
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), path)?;
        }
        "100644" | "100755" => {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            objects.copy_into(value, &mut file, None)?;
            file.set_permissions(fs::Permissions::from_mode(if mode == "100755" {
                0o755
            } else {
                0o644
            }))?;
            file.sync_all()?;
        }
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
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
    // and decide what "clean" means. A nest (or a populated submodule inside
    // it) whose config sets one refuses, and the command never runs.
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
        // A populated submodule inside the nest, with its own hostile filter.
        let sub = nest.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".gitattributes"), b"* filter=evil2\n").unwrap();
        committed(&sub, b"sub");
        output(git(&nest).args(["add", "sub"])).unwrap();
        commit(&nest, "submodule");
        stat_dirty(&nest.join("tracked"));
        stat_dirty(&sub.join("tracked"));
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
        output(git(&nest).args(["config", "--remove-section", "filter.ev.il"])).unwrap();
        assert_eq!(nested_repositories(&source).unwrap().len(), 1);

        // Only the submodule inside the nest configures one.
        output(git(&sub).args(["config", "filter.evil2.clean", &touch("sub-clean")])).unwrap();
        refuses_everywhere(&source, &root.join("capture-sub"));
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

    // R-N83: a nest's own fsmonitor command and hooks never run, its index
    // (and untracked cache) is never written, and its ignore-submodules
    // config cannot hide a dirty submodule inside it.
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
        let sub = nest.join("sub");
        committed(&sub, b"sub");
        output(git(&nest).args(["add", "sub"])).unwrap();
        commit(&nest, "submodule");
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

        output(git(&nest).args(["config", "diff.ignoreSubmodules", "all"])).unwrap();
        output(git(&nest).args(["config", "submodule.sub.ignore", "all"])).unwrap();
        fs::write(sub.join("untracked"), b"inside the submodule").unwrap();
        refuses_everywhere(&source, &root.join("capture-dirty-sub"));
        assert_eq!(markers_fired(&markers), Vec::<std::ffi::OsString>::new());
        fs::remove_dir_all(root).unwrap();
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
