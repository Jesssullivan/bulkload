//! One common Git closure plus workspace-specific prerequisite bundles.
//!
//! Shared bases are transport dependencies, not a replacement for each
//! workspace's staged, dirty, ignored and filesystem metadata capture.

use crate::counters::CountedSync as _;
use crate::refuse::RefuseAt as _;
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{capture_refs, git, oid, output, prepare_private, refs, set_ref, text};
use crate::{BulkloadRefusal, Result};

fn stash_history(repo: &Path, inventory: &str) -> Result<Vec<u8>> {
    if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
        output(git(repo).args(["reflog", "show", "--format=%H", "refs/stash"]))
    } else {
        Ok(Vec::new())
    }
}

/// Capture refs, stash history and HEAD once without walking workspace payloads.
///
/// This optimistic snapshot does not freeze writers. Subsequent workspace
/// captures may include new commits absent from this base; those travel in
/// their delta. The caller owns digest binding, retention and import ordering.
///
/// # Errors
/// Refuses changing refs/stashes/HEAD, invalid Git state or occupied capture paths.
pub fn export_base(repo: &Path, capture: &Path) -> Result<PathBuf> {
    let repo = fs::canonicalize(repo).refuse_at("git_carry::shared::export_base")?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(capture)
        .refuse_at("git_carry::shared::export_base")?;
    let capture = fs::canonicalize(capture).refuse_at("git_carry::shared::export_base")?;
    if capture.starts_with(&repo) {
        return Err(BulkloadRefusal::GitAuthorityOutsideRoot);
    }
    let inventory = refs(&repo)?;
    let boundary = super::shallow::frontier(&repo)?;
    let stashes = stash_history(&repo, &inventory)?;
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let private = prepare_private(&repo, &capture)?;
    if !boundary.is_empty() {
        super::metadata(&private, "shallow-frontier-v1", &boundary)?;
    }
    capture_refs(&private, &inventory, &stashes)?;
    set_ref(&private, "refs/carry-export/shared-base-head", &head)?;
    let bundle = capture.join("base.bundle");
    write_bundle(&private, &bundle, None)?;
    if inventory != refs(&repo)?
        || boundary != super::shallow::frontier(&repo)?
        || stashes != stash_history(&repo, &inventory)?
        || head != text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    output(git(&private).args(["bundle", "verify"]).arg(&bundle))?;
    Ok(bundle)
}

fn prerequisite_commits(private: &Path, base: &Path) -> Result<BTreeSet<String>> {
    output(git(private).args(["bundle", "verify"]).arg(base))?;
    let heads = text(git(private).args(["bundle", "list-heads"]).arg(base))?;
    let mut commits = BTreeSet::new();
    for line in heads.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) || !name.starts_with("refs/carry-export/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        // A ref may legally point to a tree/blob. Only commits can be bundle
        // prerequisites; non-commit objects remain in the workspace pack.
        output(git(private).args(["cat-file", "-e", value]))?;
        if let Ok(commit) =
            text(git(private).args(["rev-parse", "--verify", &format!("{value}^{{commit}}")]))
        {
            if !oid(&commit) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            commits.insert(commit);
        }
    }
    if commits.is_empty() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(commits)
}

/// Whether a bundle declares prerequisite commits that require retained custody.
///
/// # Errors
/// Refuses malformed or oversized bundle headers and unavailable files.
pub fn requires_base(bundle: &Path) -> Result<bool> {
    Ok(!prerequisites(bundle)?.is_empty())
}

/// The prerequisite object names a bundle's header declares (`-<oid>` lines),
/// in header order. Empty for a self-contained bundle.
///
/// # Errors
/// Refuses malformed or oversized bundle headers and unavailable files.
pub fn prerequisites(bundle: &Path) -> Result<Vec<String>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(bundle)
        .refuse_at("git_carry::shared::prerequisites")?;
    let mut source = BufReader::new(file);
    let mut consumed = 0usize;
    let mut prerequisite = Vec::new();
    loop {
        let mut line = Vec::new();
        let count = source
            .by_ref()
            .take(1024 * 1024)
            .read_until(b'\n', &mut line)
            .refuse_at("git_carry::shared::prerequisites")?;
        if consumed == 0 && line != b"# v2 git bundle\n" && line != b"# v3 git bundle\n" {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        consumed = consumed
            .checked_add(count)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if count == 0 || !line.ends_with(b"\n") || consumed > 16 * 1024 * 1024 {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if line == b"\n" {
            return Ok(prerequisite);
        }
        if let Some(rest) = line.strip_prefix(b"-") {
            let name = rest
                .split(|b| *b == b' ' || *b == b'\n')
                .next()
                .and_then(|name| std::str::from_utf8(name).ok())
                .filter(|name| oid(name))
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            prerequisite.push(name.to_owned());
        }
    }
}

/// What packing one capture cost (WP2, OI-1003-Q15). Also added to the
/// process counters `write_source_pack_bytes`, `write_source_pack_objects`
/// and `read_source_pack_readback_bytes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackStats {
    /// Bytes git wrote for the capture's pack (the bundle as git created it,
    /// or the raw pack of a shallow envelope): the logical measure of what it
    /// read from the object stores to write them.
    pub bytes: u64,
    /// Objects in that pack, from its header.
    pub objects: u64,
    /// The packing child's own storage reads: a lower bound (counters notes).
    pub storage_read: u64,
}

impl PackStats {
    /// Measure a pack git just wrote at `path` (a bundle, or a raw pack when
    /// `raw`), add it to the process counters and return it.
    pub(super) fn record(path: &Path, raw: bool, storage_read: u64) -> Result<Self> {
        use crate::counters::{add, Counter};
        let stats = Self {
            bytes: fs::symlink_metadata(path)
                .refuse_at("git_carry::shared::record")?
                .len(),
            objects: pack_object_count(path, raw)?,
            storage_read,
        };
        add(Counter::SourcePackWrite, stats.bytes);
        add(Counter::SourcePackObjects, stats.objects);
        add(Counter::SourcePackReadback, stats.storage_read);
        Ok(stats)
    }
}

// The object count a pack header declares: `PACK`, version 2 or 3, count, all
// big-endian. A bundle's pack follows its header's blank line.
fn pack_object_count(path: &Path, raw: bool) -> Result<u64> {
    const SITE: &str = "git_carry::shared::pack_object_count";
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .refuse_at(SITE)?;
    let mut source = BufReader::new(file);
    if !raw {
        let mut consumed = 0usize;
        loop {
            let mut line = Vec::new();
            let count = source
                .by_ref()
                .take(1024 * 1024)
                .read_until(b'\n', &mut line)
                .refuse_at(SITE)?;
            consumed = consumed
                .checked_add(count)
                .ok_or(BulkloadRefusal::BudgetExceeded)?;
            if count == 0 || !line.ends_with(b"\n") || consumed > 16 * 1024 * 1024 {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            if line == b"\n" {
                break;
            }
        }
    }
    let mut header = [0u8; 12];
    source.read_exact(&mut header).refuse_at(SITE)?;
    let (magic, rest) = header.split_at(4);
    let (version, count) = rest.split_at(4);
    if magic != b"PACK" || !matches!(version, [0, 0, 0, 2 | 3]) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let count: [u8; 4] = count
        .try_into()
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    Ok(u64::from(u32::from_be_bytes(count)))
}

// `git bundle create` through the measured child path. A failed child refuses
// GIT_CHILD_FAILED with its stderr class (WP3, R-N121).
fn create_bundle(command: &mut Command, bundle: &Path, stdin: Option<&[u8]>) -> Result<PackStats> {
    let storage_read = super::pack_child(command.stdout(Stdio::null()), stdin)?;
    PackStats::record(bundle, false, storage_read)
}

// A self-contained bundle of every private ref.
fn write_full(private: &Path, bundle: &Path) -> Result<PackStats> {
    create_bundle(
        git(private)
            .args(["bundle", "create"])
            .arg(bundle)
            .arg("--all"),
        bundle,
        None,
    )
}

/// Write a capture bundle whose prerequisites are `prior`'s source-held tips
/// (see `chain`). Returns its pack cost and whether it declared any
/// prerequisite; `false` means it is self-contained (a shallow capture, or no
/// source-held tip).
pub(super) fn write_chained(
    private: &Path,
    bundle: &Path,
    source: &Path,
    prior: &Path,
) -> Result<(PackStats, bool)> {
    let boundary = super::shallow::frontier(private)?;
    if !boundary.is_empty() {
        return Ok((
            super::shallow::write_bundle(private, bundle, &boundary)?,
            false,
        ));
    }
    let commits = super::chain::source_held_tips(source, prior)?;
    if commits.is_empty() {
        return Ok((write_full(private, bundle)?, false));
    }
    Ok((write_excluding_tip_trees(private, bundle, &commits)?, true))
}

// A bundle declaring `commits` as prerequisites whose pack excludes every
// object reachable from them, *including through their trees*.
//
// `git bundle create ^tip` only marks the trees of edge commits (parents of
// packed commits) uninteresting. A capture's staged and worktree commits are
// parentless, so their trees would re-pack every blob they share with HEAD:
// all tracked content, every pass. `rev-list --objects-edge-aggressive`
// marks the tree of every excluded tip uninteresting instead. The object list
// is packed by `pack-objects`, and the header (signature, prerequisites,
// every private ref) is written here, as the shared-base path rewrites it.
// `bundle verify` in the caller checks the result like any other bundle.
// Both children go through the measured, classified child path: a failed
// one refuses GIT_CHILD_FAILED with its stderr class (WP3, R-N121).
//
// A failed write removes its pending object list and header, so a refused
// pass leaves no partial file beside the bundle path.
fn write_excluding_tip_trees(
    private: &Path,
    bundle: &Path,
    commits: &BTreeSet<String>,
) -> Result<PackStats> {
    let written = write_excluding_tip_trees_pending(private, bundle, commits);
    if written.is_err() {
        for leftover in ["objects-pending", "header-pending"] {
            // Best effort: the refusal being returned is the one that matters,
            // and a path never created is already clean.
            let _ = fs::remove_file(bundle.with_extension(leftover));
        }
    }
    written
}

fn write_excluding_tip_trees_pending(
    private: &Path,
    bundle: &Path,
    commits: &BTreeSet<String>,
) -> Result<PackStats> {
    const SITE: &str = "git_carry::shared::write_excluding_tip_trees_pending";
    let listing = bundle.with_extension("objects-pending");
    let list = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&listing)
        .refuse_at(SITE)?;
    let exclusions = commits.iter().fold(String::new(), |mut result, value| {
        result.push('^');
        result.push_str(value);
        result.push('\n');
        result
    });
    let list_read = super::pack_child(
        git(private)
            .args(["rev-list", "--objects-edge-aggressive", "--all", "--stdin"])
            .stdout(Stdio::from(list.try_clone().refuse_at(SITE)?)),
        Some(exclusions.as_bytes()),
    )?;
    let mut objects = Vec::new();
    fs::File::open(&listing)
        .refuse_at(SITE)?
        .read_to_end(&mut objects)
        .refuse_at(SITE)?;
    fs::remove_file(&listing).refuse_at(SITE)?;
    // Edge lines (`-<oid>`) name excluded commits, not objects to pack.
    let objects: Vec<u8> = objects
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| !line.starts_with(b"-"))
        .flatten()
        .copied()
        .collect();
    let format = text(git(private).args(["rev-parse", "--show-object-format"]))?;
    let mut header = match format.as_str() {
        "sha1" => b"# v2 git bundle\n".to_vec(),
        "sha256" => b"# v3 git bundle\n@object-format=sha256\n".to_vec(),
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    };
    for value in commits {
        writeln!(header, "-{value} shared base").refuse_at(SITE)?;
    }
    writeln!(header, "{}\n", refs(private)?).refuse_at(SITE)?;
    let pending = bundle.with_extension("header-pending");
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pending)
        .refuse_at(SITE)?;
    target.write_all(&header).refuse_at(SITE)?;
    target.flush().refuse_at(SITE)?;
    let pack_read = super::pack_child(
        git(private)
            .args(["pack-objects", "--stdout", "--delta-base-offset"])
            .stdout(Stdio::from(target.try_clone().refuse_at(SITE)?)),
        Some(&objects),
    )?;
    target.sync_file_counted().refuse_at(SITE)?;
    fs::rename(&pending, bundle).refuse_at(SITE)?;
    fs::File::open(bundle.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
        .refuse_at(SITE)?
        .sync_dir_counted()
        .refuse_at(SITE)?;
    PackStats::record(bundle, false, list_read.saturating_add(pack_read))
}

pub(super) fn write_bundle(
    private: &Path,
    bundle: &Path,
    base: Option<&Path>,
) -> Result<PackStats> {
    let boundary = super::shallow::frontier(private)?;
    if !boundary.is_empty() {
        // A shallow frontier is not a bundle prerequisite. Preserve the entire
        // locally available shallow closure as explicit custody instead.
        return super::shallow::write_bundle(private, bundle, &boundary);
    }
    let Some(base) = base else {
        return write_full(private, bundle);
    };
    let commits = prerequisite_commits(private, base)?;
    write_with_prerequisites(private, bundle, &commits)
}

// A bundle excluding everything reachable from `commits`, which it declares
// as its prerequisites. A failed `bundle create` refuses GIT_CHILD_FAILED
// with its stderr class (WP3, R-N121).
fn write_with_prerequisites(
    private: &Path,
    bundle: &Path,
    commits: &BTreeSet<String>,
) -> Result<PackStats> {
    const SITE: &str = "git_carry::shared::write_with_prerequisites";
    let exclusions = commits.iter().fold(String::new(), |mut result, value| {
        result.push('^');
        result.push_str(value);
        result.push('\n');
        result
    });
    let stats = create_bundle(
        git(private)
            .args(["bundle", "create"])
            .arg(bundle)
            .args(["--all", "--stdin"]),
        bundle,
        Some(exclusions.as_bytes()),
    )?;

    // Git omits excluded ref tips from bundle headers. Our HEAD may be exactly
    // a base tip, and staged/worktree commits deliberately have no parents.
    // Retain every advertised workspace ref and explicitly declare the base
    // commits needed by their trees. The pack remains entirely Git-generated.
    let mut source = BufReader::new(fs::File::open(bundle).refuse_at(SITE)?);
    let mut header = Vec::new();
    let mut consumed = 0usize;
    loop {
        let mut line = Vec::new();
        let count = source
            .by_ref()
            .take(1024 * 1024)
            .read_until(b'\n', &mut line)
            .refuse_at(SITE)?;
        consumed = consumed
            .checked_add(count)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if count == 0 || !line.ends_with(b"\n") || consumed > 16 * 1024 * 1024 {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if line == b"\n" {
            break;
        }
        if line.starts_with(b"# v") || line.starts_with(b"@") {
            header.extend_from_slice(&line);
        }
    }
    if !header.starts_with(b"# v2 git bundle\n") && !header.starts_with(b"# v3 git bundle\n") {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    for value in commits {
        writeln!(header, "-{value} shared base").refuse_at(SITE)?;
    }
    writeln!(header, "{}\n", refs(private)?).refuse_at(SITE)?;
    let pending = bundle.with_extension("header-pending");
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pending)
        .refuse_at(SITE)?;
    target.write_all(&header).refuse_at(SITE)?;
    std::io::copy(&mut source, &mut target).refuse_at(SITE)?;
    target.sync_file_counted().refuse_at(SITE)?;
    fs::rename(&pending, bundle).refuse_at(SITE)?;
    fs::File::open(bundle.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
        .refuse_at(SITE)?
        .sync_dir_counted()
        .refuse_at(SITE)?;
    Ok(stats)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn run(repo: &Path, args: &[&str]) {
        output(
            git(repo)
                .args([
                    "-c",
                    "user.name=Bulkload test",
                    "-c",
                    "user.email=bulkload@localhost",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args),
        )
        .unwrap();
    }

    #[test]
    fn shared_base_preserves_workspace_refs_and_requires_imported_history() {
        let root = std::env::temp_dir().join(format!(
            "tcfs-shared-git-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        run(&source, &["init", "--template="]);
        let mut random = 1u32;
        let history: Vec<u8> = (0..262_144)
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random.to_le_bytes().first().copied().unwrap()
            })
            .collect();
        fs::write(source.join("history"), &history).unwrap();
        fs::write(source.join("file"), b"base").unwrap();
        run(&source, &["add", "."]);
        run(&source, &["commit", "-m", "base"]);
        let head = text(git(&source).args(["rev-parse", "HEAD"])).unwrap();
        let base = export_base(&source, &root.join("base")).unwrap();
        fs::write(source.join("file"), b"staged").unwrap();
        run(&source, &["add", "file"]);
        fs::write(source.join("file"), b"dirty").unwrap();
        fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(source.join("ignored"), b"private ignored payload").unwrap();
        let status = output(git(&source).args(["status", "--porcelain=v1", "-z"])).unwrap();
        let delta =
            super::super::export_repository_with_prerequisite(&source, &root.join("delta"), &base)
                .unwrap();
        let standalone =
            super::super::export_repository(&source, &root.join("standalone")).unwrap();
        assert!(fs::metadata(&delta).unwrap().len() < fs::metadata(&standalone).unwrap().len());
        assert!(
            text(git(&source).args(["bundle", "list-heads"]).arg(&delta))
                .unwrap()
                .lines()
                .any(|line| line == format!("{head} refs/carry-export/head"))
        );
        assert_eq!(
            status,
            output(git(&source).args(["status", "--porcelain=v1", "-z"])).unwrap()
        );
        let destination = root.join("destination");
        fs::create_dir(&destination).unwrap();
        run(&destination, &["init", "--template="]);
        assert!(super::super::import_bundle(&destination, &delta, "neo").is_err());
        super::super::import_bundle(&destination, &base, "neo").unwrap();
        let restored = root.join("restored");
        super::super::restore_linked(&delta, &destination, &restored, "neo").unwrap();
        assert_eq!(
            head,
            text(git(&restored).args(["rev-parse", "HEAD"])).unwrap()
        );
        assert_eq!(fs::read(restored.join("history")).unwrap(), history);
        assert_eq!(fs::read(restored.join("file")).unwrap(), b"dirty");
        assert_eq!(
            output(git(&restored).args(["show", ":file"])).unwrap(),
            b"staged"
        );
        assert_eq!(
            fs::read(restored.join("ignored")).unwrap(),
            b"private ignored payload"
        );
        let linked_source = root.join("linked-source");
        output(
            git(&source)
                .args(["worktree", "add", "--detach"])
                .arg(&linked_source)
                .arg(&head),
        )
        .unwrap();
        fs::write(linked_source.join("file"), b"other workspace").unwrap();
        let second = super::super::export_repository_with_prerequisite(
            &linked_source,
            &root.join("second-delta"),
            &base,
        )
        .unwrap();
        let second_restored = root.join("second-restored");
        super::super::restore_linked(&second, &destination, &second_restored, "neo").unwrap();
        assert_eq!(
            fs::read(second_restored.join("file")).unwrap(),
            b"other workspace"
        );
        assert_eq!(fs::read(restored.join("file")).unwrap(), b"dirty");
        fs::remove_dir_all(root).unwrap();
    }
}
