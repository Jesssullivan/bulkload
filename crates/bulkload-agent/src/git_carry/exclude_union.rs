//! `estate-apply --exclude-union` (M2 round 2, item 2, 2026-10-17).
//!
//! A first linked landing keeps the destination repository's common
//! `info/exclude` only when it is the capture's (or a newer one the main
//! item set, finding 12); anything else refuses
//! `GIT_IGNORE_POLICY_CONFLICT` and the file is never overwritten. The
//! in-place run of 2026-10-16 refused 250 new linked worktrees this way, in
//! repositories whose source exclude was a pure superset of the
//! destination's. `--exclude-union` is the opt-in, recorded write that
//! lets those land:
//!
//! - **Append only.** The captured lines the destination's exclude lacks
//!   are appended after its last line, each once, in captured order. Lines
//!   are compared as Git reads them: a leading UTF-8 byte order mark is
//!   skipped, one trailing CR is dropped, and trailing spaces not escaped
//!   by a backslash are trimmed; a line that is empty after that is
//!   skipped. A line appended is the captured line's bytes (its byte order
//!   mark, if it was the first line, left out). No existing byte is
//!   removed, changed or reordered: the old file is a byte prefix of the
//!   new one. When nothing is missing nothing is written, so a rerun, and
//!   every later linked item of the same repository, is a no-op.
//! - **Only where order cannot matter.** Git decides an ignore by the last
//!   matching pattern, so appending is a pure union of the two policies
//!   only when neither holds a negation (`!pattern`; `\!` is a literal and
//!   does not count): an appended pattern could re-ignore a path an earlier
//!   destination negation re-included, and an appended negation could
//!   re-include what a destination pattern ignores. Where a line would be
//!   appended, a negation on either side keeps the refusal (with nothing
//!   missing nothing is written, so no negation matters).
//! - **Never a ledgered or a shared file.** Where a line would be appended,
//!   the refusal is kept for: an exclude a bulkload ledger at the
//!   repository records (any label's main workspace ledger, or a ref
//!   ledger's exclude row; that main item's converge owns it, and an
//!   append would fail its proof); an `info` that is not a real directory
//!   (a symlink is never written through); an exclude that is not a regular
//!   file, has more than one link, or another owner; and an exclude that
//!   changed between the decision and the write.
//! - **Repository-wide.** `info/exclude` is read by every worktree of the
//!   repository: the append changes what the main checkout and every other
//!   linked worktree ignore (an untracked file a new pattern matches leaves
//!   `git status`, and `git add -A` and `git stash -u` skip it).
//! - **Serialized.** Settling, deciding and appending run under a
//!   repository-wide lock (`$GIT_COMMON_DIR/bulkload/exclude.lock`), so two
//!   applies from different SOURCE labels never race the read and the
//!   rename. Git itself takes no lock on the file.
//! - **Recorded, write-ahead.** The new bytes go to a temporary in `info`
//!   (created, written, given the old file's mode with `fchmod`, synced).
//!   Then the append is recorded in STATE
//!   (`{item}-{blake3(source)}.exclude-union`, [`ExcludeUnion`]) as an
//!   intent naming the path, the digests before and after, the lines, the
//!   temporary's name and its identity (device and inode). Then the
//!   temporary is renamed over the exclude and `info` is synced, and the
//!   entry is marked `appended`. A crash between leaves an intent that the
//!   next run under the flag settles by identity, never by content:
//!   `appended` when the exclude is the recorded inode, `not-written` when
//!   the temporary is still there (it is removed), otherwise left an intent
//!   (the file was replaced since; the record does not guess). A crash
//!   before the intent can leave a `.bulkload-exclude-*` temporary no
//!   record names; Git never reads it.
//! - **Counted until reported.** The receipt of the landing that finishes
//!   carries `exclude-appended=<lines>`: every appended line the item's
//!   record holds that no finished landing has reported yet, so an append
//!   a later refusal or a crash cut off from its receipt is counted by the
//!   rerun that lands.
//!
//! Without the flag nothing here runs and the refusal is unchanged; an
//! intent left by a crash settles on the next run under the flag.
//! `estate-diff` reports the item `ignore-policy-conflict` as before and,
//! where the flag would land it, the component `exclude-union=<lines>`
//! (items of one repository each count the lines; the apply appends them
//! once), or `exclude-union-refused=<CODE>` where the flag would not.

use std::fs;
use std::io::Write as _;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::landed;
use crate::counters::CountedSync as _;
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// One append's state in the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppendState {
    /// Recorded before the rename; not yet known to have happened.
    Intent,
    /// The exclude was the recorded inode when settled (or the rename
    /// returned).
    Appended,
    /// The rename never happened: its temporary was still there.
    NotWritten,
}

/// One append to one exclude file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Append {
    /// The exclude file's absolute path.
    pub path: Vec<u8>,
    /// blake3 of the file before the append (of nothing when it was absent).
    pub before: [u8; 32],
    /// blake3 of the file after it.
    pub after: [u8; 32],
    /// The lines appended, without their newline.
    pub lines: Vec<Vec<u8>>,
    /// The temporary's name in `info`.
    pub temporary: Vec<u8>,
    /// The temporary's (device, inode): the exclude's once renamed.
    pub identity: (u64, u64),
    pub state: AppendState,
    /// A finished landing's receipt counted it.
    pub reported: bool,
}

/// The appends one item's applies made (`{item}-{source}.exclude-union` in
/// STATE). Appended to, never rewritten but for settling a state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludeUnion {
    /// `estate::id` of the item.
    pub item: String,
    pub appends: Vec<Append>,
}

impl landed::Record for ExcludeUnion {
    const MAGIC: &'static [u8; 16] = b"bulkload-exunio2";
}

/// The record at `path`, if any (for reports and tests).
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_record(path: &Path) -> Result<Option<ExcludeUnion>> {
    landed::read::<ExcludeUnion>(path)
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// The lines of an exclude as Git splits them: a leading byte order mark
/// skipped, newlines removed (the one a final newline ends is not a line
/// of its own).
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let empty = bytes.is_empty();
    body.split(|byte| *byte == b'\n').filter(move |_| !empty)
}

/// A line as Git compares it: one trailing CR dropped, then trailing
/// spaces not escaped by a backslash trimmed (`trim_trailing_spaces`).
fn key(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut end = line.len();
    while end > 0 && line.get(end - 1) == Some(&b' ') {
        // An odd run of backslashes before this space escapes it.
        let slashes = line
            .get(..end - 1)
            .unwrap_or_default()
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count();
        if slashes % 2 == 1 {
            break;
        }
        end -= 1;
    }
    line.get(..end).unwrap_or_default()
}

fn negation(line: &[u8]) -> bool {
    key(line).first() == Some(&b'!')
}

/// The captured lines `existing` lacks, in captured order, each once;
/// empty when it holds them all.
///
/// # Errors
/// `GIT_IGNORE_POLICY_CONFLICT` when either side holds a negation (an
/// append would not be a pure union) and a line is missing; with nothing
/// missing nothing is written, so a negation is no obstacle.
pub fn missing(existing: &[u8], captured: &[u8]) -> Result<Vec<Vec<u8>>> {
    let held: std::collections::BTreeSet<&[u8]> = lines(existing).map(key).collect();
    let mut seen = std::collections::BTreeSet::new();
    let missing: Vec<Vec<u8>> = lines(captured)
        .filter(|line| {
            let key = key(line);
            !key.is_empty() && !held.contains(key) && seen.insert(key)
        })
        .map(<[u8]>::to_vec)
        .collect();
    if !missing.is_empty() && lines(existing).chain(lines(captured)).any(negation) {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(missing)
}

/// `existing` with `append` after its last line (a newline added first
/// when it does not end in one).
#[must_use]
pub fn appended(existing: &[u8], append: &[Vec<u8>]) -> Vec<u8> {
    let mut out = existing.to_vec();
    if !out.is_empty() && out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    for line in append {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// Whether a bulkload ledger at `common` records its exclude: any label's
/// main workspace ledger, or a ref ledger's exclude row.
///
/// # Errors
/// IO; a ref ledger that does not decode.
pub fn ledgered(common: &Path) -> Result<bool> {
    const SITE: &str = "git_carry::exclude_union::ledgered";
    let root = common.join(landed::LEDGER_ROOT);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in entries {
        let entry = entry.refuse_at(SITE)?;
        if !entry.file_type().refuse_at(SITE)?.is_dir() {
            continue;
        }
        let label = entry.path();
        if landed::exists(&label.join(landed::WORKSPACE_LANDED))? {
            return Ok(true);
        }
        if landed::read::<landed::RefLedger>(&label.join(landed::REFS_LANDED))?
            .is_some_and(|ledger| ledger.exclude.is_some())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The exclude a union may write: its `info` directory, opened without
/// following a link, and the file in it, if present.
struct Opened {
    info: fs::File,
    file: Option<(crate::io::Stat, Vec<u8>)>,
}

fn conflict_or_io(error: &std::io::Error, site: &'static str) -> BulkloadRefusal {
    match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => BulkloadRefusal::GitIgnorePolicyConflict,
        _ => crate::refuse::io(error, site),
    }
}

fn c_name(bytes: &[u8]) -> Result<std::ffi::CString> {
    crate::io::c_name(bytes).map_err(|_| BulkloadRefusal::PathNotPortable)
}

/// Open `path` (`<common>/info/exclude`) for a union: `info` a real
/// directory (made, 0755, and its parent synced, when `create` and it is
/// absent), the exclude a regular file with one link this user owns, or
/// absent. Its bytes are read through the descriptor it was checked on.
///
/// # Errors
/// `GIT_IGNORE_POLICY_CONFLICT` for any other shape; IO.
fn open(path: &Path, create: bool) -> Result<Option<Opened>> {
    const SITE: &str = "git_carry::exclude_union::open";
    let (Some(info), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    };
    if name != "exclude" || info.file_name().is_none_or(|info| info != "info") {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    let common = info
        .parent()
        .ok_or(BulkloadRefusal::GitIgnorePolicyConflict)?;
    let common = fs::File::from(
        crate::io::sys::open_dir_path_nofollow(common).map_err(|e| conflict_or_io(&e, SITE))?,
    );
    let info_name = c_name(b"info")?;
    let info = match crate::io::sys::open_dir_at(common.as_fd(), &info_name) {
        Ok(fd) => fs::File::from(fd),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
            if !create {
                return Ok(None);
            }
            match crate::io::sys::mkdirat(common.as_fd(), &info_name, 0o755) {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
                Err(error) => return Err(crate::refuse::io(&error, SITE)),
            }
            common.sync_dir_counted().refuse_at(SITE)?;
            fs::File::from(
                crate::io::sys::open_dir_at(common.as_fd(), &info_name)
                    .map_err(|e| conflict_or_io(&e, SITE))?,
            )
        }
        Err(error) => return Err(conflict_or_io(&error, SITE)),
    };
    let file = read_at(&info)?;
    Ok(Some(Opened { info, file }))
}

/// The exclude in `info`: its `fstat` and bytes, read through one
/// descriptor opened without following a link; `None` when absent.
fn read_at(info: &fs::File) -> Result<Option<(crate::io::Stat, Vec<u8>)>> {
    const SITE: &str = "git_carry::exclude_union::read_at";
    let name = c_name(b"exclude")?;
    let fd: OwnedFd = match crate::io::sys::open_read_at(info.as_fd(), &name) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(None),
        Err(error) => return Err(conflict_or_io(&error, SITE)),
    };
    let stat = crate::io::sys::fstat(&fd).refuse_at(SITE)?;
    if !stat.is_file() || stat.nlink != 1 || stat.uid != crate::io::sys::effective_uid() {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut fs::File::from(fd), &mut bytes).refuse_at(SITE)?;
    Ok(Some((stat, bytes)))
}

/// The union's lines for `path` (`existing` read from it): what
/// [`missing`] says, after the checks a write needs (see the module docs).
/// Read-only.
///
/// # Errors
/// `GIT_IGNORE_POLICY_CONFLICT` where the union does not apply.
pub fn plan(common: &Path, path: &Path, existing: &[u8], captured: &[u8]) -> Result<Vec<Vec<u8>>> {
    let append = missing(existing, captured)?;
    if append.is_empty() {
        return Ok(append);
    }
    let held = match open(path, false)? {
        None => None,
        Some(opened) => opened.file.map(|(_, bytes)| bytes),
    };
    if held.unwrap_or_default() != existing || ledgered(common)? {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(append)
}

/// The repository-wide lock settling, deciding and appending run under
/// (`$GIT_COMMON_DIR/bulkload/exclude.lock`), held until dropped.
///
/// # Errors
/// IO.
pub fn lock(common: &Path) -> Result<fs::File> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    const SITE: &str = "git_carry::exclude_union::lock";
    let directory = common.join("bulkload");
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&directory)
        .refuse_at(SITE)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("exclude.lock"))
        .refuse_at(SITE)?;
    loop {
        // SAFETY: the owned descriptor stays open for the lock's lifetime.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(crate::refuse::io(&error, SITE));
        }
    }
}

/// Settle the intents of `record` that name `path` by identity (see the
/// module docs). Run under [`lock`].
///
/// # Errors
/// IO; a record that does not decode.
pub fn settle(record: &Path, path: &Path) -> Result<()> {
    const SITE: &str = "git_carry::exclude_union::settle";
    let Some(mut kept) = landed::read::<ExcludeUnion>(record)? else {
        return Ok(());
    };
    let name = path.as_os_str().as_bytes();
    if !kept
        .appends
        .iter()
        .any(|append| append.state == AppendState::Intent && append.path == name)
    {
        return Ok(());
    }
    let info = match path.parent().map(crate::io::sys::open_dir_path_nofollow) {
        Some(Ok(fd)) => fs::File::from(fd),
        // No `info` (or not a directory): nothing of this record is there.
        None => return Ok(()),
        Some(Err(error)) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
        Some(Err(error)) => return Err(conflict_or_io(&error, SITE)),
    };
    let exclude = c_name(b"exclude")?;
    let now = crate::io::sys::fstatat_nofollow(info.as_fd(), &exclude)
        .ok()
        .map(|stat| (stat.node.dev, stat.node.ino));
    let mut changed = false;
    for append in &mut kept.appends {
        if append.state != AppendState::Intent || append.path != name {
            continue;
        }
        let temporary = c_name(&append.temporary)?;
        if now == Some(append.identity) {
            append.state = AppendState::Appended;
            changed = true;
        } else if crate::io::sys::fstatat_nofollow(info.as_fd(), &temporary)
            .is_ok_and(|stat| (stat.node.dev, stat.node.ino) == append.identity)
        {
            crate::io::sys::unlinkat(info.as_fd(), &temporary, false).refuse_at(SITE)?;
            info.sync_dir_counted().refuse_at(SITE)?;
            append.state = AppendState::NotWritten;
            changed = true;
        }
    }
    if changed {
        landed::write(record, &kept)?;
    }
    Ok(())
}

/// The appended lines of `record` for `path` no finished landing has
/// reported.
///
/// # Errors
/// IO; a record that does not decode.
pub fn unreported(record: &Path, path: &Path) -> Result<u64> {
    let name = path.as_os_str().as_bytes();
    Ok(landed::read::<ExcludeUnion>(record)?.map_or(0, |kept| {
        kept.appends
            .iter()
            .filter(|append| {
                append.state == AppendState::Appended && !append.reported && append.path == name
            })
            .map(|append| u64::try_from(append.lines.len()).unwrap_or(u64::MAX))
            .fold(0, u64::saturating_add)
    }))
}

/// Mark the appended entries of `record` for `path` reported: a finished
/// landing's receipt counted them.
///
/// # Errors
/// IO; a record that does not decode.
pub fn reported(record: &Path, path: &Path) -> Result<()> {
    let Some(mut kept) = landed::read::<ExcludeUnion>(record)? else {
        return Ok(());
    };
    let name = path.as_os_str().as_bytes();
    let mut changed = false;
    for append in &mut kept.appends {
        if append.state == AppendState::Appended && !append.reported && append.path == name {
            append.reported = true;
            changed = true;
        }
    }
    if changed {
        landed::write(record, &kept)?;
    }
    Ok(())
}

/// Append `lines` to the exclude at `path`, which must still hold
/// `existing`, recorded in `record` for `item`; the new bytes. Run under
/// [`lock`].
///
/// # Errors
/// `GIT_IGNORE_POLICY_CONFLICT` when the file is not one a union writes or
/// changed since the decision; IO.
pub fn apply(
    (record, item): (&Path, &str),
    path: &Path,
    existing: &[u8],
    lines: &[Vec<u8>],
) -> Result<Vec<u8>> {
    const SITE: &str = "git_carry::exclude_union::apply";
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Opened { info, file } =
        open(path, true)?.ok_or(BulkloadRefusal::GitIgnorePolicyConflict)?;
    let (before, mode) = match &file {
        Some((stat, _)) => (Some(stat.node), stat.mode & 0o7777),
        None => (None, 0o644),
    };
    if file.map(|(_, bytes)| bytes).unwrap_or_default() != existing {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    let bytes = appended(existing, lines);
    let temporary = format!(
        ".bulkload-exclude-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let temporary_name = c_name(temporary.as_bytes())?;
    let written =
        crate::io::sys::create_excl_at(info.as_fd(), &temporary_name, 0o600).and_then(|fd| {
            let mut file = fs::File::from(fd);
            file.write_all(&bytes)?;
            // The old file's mode exactly, whatever the umask removed.
            crate::io::sys::fchmod(file.as_fd(), mode)?;
            file.sync_file_counted()?;
            crate::io::sys::fstat(file.as_fd())
        });
    let stat = match written {
        Ok(stat) => stat,
        Err(error) => {
            let _ = crate::io::sys::unlinkat(info.as_fd(), &temporary_name, false);
            return Err(crate::refuse::io(&error, SITE));
        }
    };
    let mut kept = landed::read::<ExcludeUnion>(record)?.unwrap_or_default();
    item.clone_into(&mut kept.item);
    kept.appends.push(Append {
        path: path.as_os_str().as_bytes().to_vec(),
        before: digest(existing),
        after: digest(&bytes),
        lines: lines.to_vec(),
        temporary: temporary.into_bytes(),
        identity: (stat.node.dev, stat.node.ino),
        state: AppendState::Intent,
        reported: false,
    });
    landed::write(record, &kept)?;
    fault_point!(EstateApplyExcludeIntent);
    // A writer since the decision (or a link swapped in): nothing is
    // renamed, and the intent says so.
    let unchanged = match read_at(&info) {
        Ok(Some((now, held))) => Some(now.node) == before && held == existing,
        Ok(None) => before.is_none(),
        Err(_) => false,
    };
    let renamed = if unchanged {
        let exclude = c_name(b"exclude")?;
        // SAFETY: both names are NUL-terminated and outlive the call; the
        // descriptor is live; `renameat` touches no other memory.
        let status = unsafe {
            libc::renameat(
                info.as_raw_fd(),
                temporary_name.as_ptr(),
                info.as_raw_fd(),
                exclude.as_ptr(),
            )
        };
        (status == 0)
            .then_some(())
            .ok_or_else(std::io::Error::last_os_error)
    } else {
        Err(std::io::Error::from_raw_os_error(libc::ESTALE))
    };
    if let Err(error) = renamed {
        let _ = crate::io::sys::unlinkat(info.as_fd(), &temporary_name, false);
        if let Some(last) = kept.appends.last_mut() {
            last.state = AppendState::NotWritten;
        }
        landed::write(record, &kept)?;
        return Err(if unchanged {
            crate::refuse::io(&error, SITE)
        } else {
            BulkloadRefusal::GitIgnorePolicyConflict
        });
    }
    info.sync_dir_counted().refuse_at(SITE)?;
    fault_point!(EstateApplyExcludeAppended);
    if let Some(last) = kept.appends.last_mut() {
        last.state = AppendState::Appended;
    }
    landed::write(record, &kept)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn v(lines: &[&str]) -> Vec<Vec<u8>> {
        lines.iter().map(|line| line.as_bytes().to_vec()).collect()
    }

    fn scratch(line: u32) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("bulkload-exunion-{}-{line}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("repo.git/info")).unwrap();
        fs::canonicalize(&base).unwrap()
    }

    #[test]
    fn a_superset_appends_only_the_missing_lines_in_captured_order() {
        let existing = b"*.log\nbuild/\n";
        let captured = b"# neo\n*.log\n\nscratch/\nbuild/\n.envrc\nscratch/\n";
        let lines = missing(existing, captured).unwrap();
        assert_eq!(lines, v(&["# neo", "scratch/", ".envrc"]));
        let out = appended(existing, &lines);
        assert!(out.starts_with(existing));
        assert_eq!(out, b"*.log\nbuild/\n# neo\nscratch/\n.envrc\n");
        assert!(missing(&out, captured).unwrap().is_empty());
    }

    #[test]
    fn a_file_without_a_final_newline_keeps_its_bytes() {
        let out = appended(b"a", &v(&["b"]));
        assert_eq!(out, b"a\nb\n");
        assert_eq!(missing(b"a", b"a\n").unwrap(), Vec::<Vec<u8>>::new());
        assert_eq!(appended(b"", &v(&["x"])), b"x\n");
    }

    #[test]
    fn a_negation_on_either_side_refuses_when_lines_are_missing() {
        assert_eq!(
            missing(b"*.log\n", b"*.log\n!keep.log\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        assert_eq!(
            missing(b"*.log\n!keep.log\n", b"*.log\nbuild/\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        // A literal `\!` is a pattern, not a negation.
        assert_eq!(missing(b"a\n", b"a\n\\!bang\n").unwrap(), v(&["\\!bang"]));
        // Nothing missing: nothing is written, so a negation is no obstacle.
        assert!(missing(b"!a\nb\n", b"b\n!a\n").unwrap().is_empty());
    }

    // Review of PR #260, finding 1: Git skips a leading byte order mark, so
    // a negation behind one is a negation, and a captured first line is
    // appended without it.
    #[test]
    fn a_byte_order_mark_is_skipped_as_git_skips_it() {
        assert_eq!(
            missing(b"\xEF\xBB\xBF!keep.log\n*.log\n", b"*.log\nbuild/\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        assert_eq!(
            missing(b"*.log\n", b"\xEF\xBB\xBFbuild/\n*.log\n").unwrap(),
            v(&["build/"])
        );
        assert!(missing(b"\xEF\xBB\xBFa\n", b"a\n").unwrap().is_empty());
    }

    // Review findings 9 and 10: a whitespace line other than spaces is a
    // pattern; CR and unescaped trailing spaces compare as Git reads them.
    #[test]
    fn lines_compare_as_git_reads_them() {
        assert_eq!(missing(b"a\n", b"a\n\t\n").unwrap(), v(&["\t"]));
        assert!(missing(b"a\n", b"a\r\n   \n").unwrap().is_empty());
        assert!(missing(b"a\n", b"a  \n").unwrap().is_empty());
        assert_eq!(missing(b"a\n", b"a\\ \n").unwrap(), v(&["a\\ "]));
        assert!(missing(b"a\\ \n", b"a\\  \n").unwrap().is_empty());
    }

    #[test]
    fn a_ledgered_linked_or_shared_exclude_is_never_planned() {
        let base = scratch(line!());
        let common = base.join("repo.git");
        let path = common.join("info/exclude");
        fs::write(&path, b"a\n").unwrap();
        assert_eq!(plan(&common, &path, b"a\n", b"a\nb\n").unwrap(), v(&["b"]));
        // A symlinked exclude is never written through.
        let target = base.join("elsewhere");
        fs::write(&target, b"a\n").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(
            plan(&common, &path, b"a\n", b"a\nb\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        // Nor one with a second link (review finding 5).
        fs::remove_file(&path).unwrap();
        fs::hard_link(&target, &path).unwrap();
        assert_eq!(
            plan(&common, &path, b"a\n", b"a\nb\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"a\n").unwrap();
        // A main workspace ledger under any label owns the exclude.
        let label = common.join(landed::LEDGER_ROOT).join("other");
        fs::create_dir_all(&label).unwrap();
        fs::write(common.join(landed::LEDGER_ROOT).join("stray"), b"").unwrap();
        assert!(!ledgered(&common).unwrap());
        fs::write(label.join(landed::WORKSPACE_LANDED), b"").unwrap();
        assert!(ledgered(&common).unwrap());
        assert_eq!(
            plan(&common, &path, b"a\n", b"a\nb\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        fs::remove_file(label.join(landed::WORKSPACE_LANDED)).unwrap();
        // So does a ref ledger's exclude row; one without it does not.
        let mut refs = landed::RefLedger::default();
        landed::write(&label.join(landed::REFS_LANDED), &refs).unwrap();
        assert!(!ledgered(&common).unwrap());
        refs.exclude = Some(landed::LandedExclude {
            digest: [0; 32],
            observed_ns: 1,
        });
        landed::write(&label.join(landed::REFS_LANDED), &refs).unwrap();
        assert!(ledgered(&common).unwrap());
        // Nothing missing never needs the checks.
        assert!(plan(&common, &path, b"a\nb\n", b"b\n").unwrap().is_empty());
        fs::remove_dir_all(&base).unwrap();
    }

    // Review finding 6: an `info` that is a symlink is never written
    // through, by the plan or the apply.
    #[test]
    fn a_symlinked_info_is_never_written_through() {
        let base = scratch(line!());
        let common = base.join("repo.git");
        fs::remove_dir(common.join("info")).unwrap();
        fs::create_dir(base.join("elsewhere")).unwrap();
        fs::write(base.join("elsewhere/exclude"), b"a\n").unwrap();
        std::os::unix::fs::symlink(base.join("elsewhere"), common.join("info")).unwrap();
        let path = common.join("info/exclude");
        assert_eq!(
            plan(&common, &path, b"a\n", b"a\nb\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        let record = base.join("item.exclude-union");
        assert_eq!(
            apply((&record, "item"), &path, b"a\n", &v(&["b"])),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        assert_eq!(fs::read(base.join("elsewhere/exclude")).unwrap(), b"a\n");
        assert!(read_record(&record).unwrap().is_none());
        fs::remove_dir_all(&base).unwrap();
    }

    // Review finding 5: the old file's mode is kept exactly (0664 under a
    // 022 umask), and an absent `info` is made.
    #[test]
    fn apply_keeps_the_mode_and_makes_an_absent_info() {
        let base = scratch(line!());
        let common = base.join("repo.git");
        let path = common.join("info/exclude");
        fs::write(&path, b"a\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        let record = base.join("item.exclude-union");
        apply((&record, "item"), &path, b"a\n", &v(&["b"])).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o664
        );
        fs::remove_dir_all(common.join("info")).unwrap();
        let out = apply((&record, "item"), &path, b"", &v(&["c"])).unwrap();
        assert_eq!(fs::read(&path).unwrap(), out);
        assert_eq!(out, b"c\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o644
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn apply_records_an_intent_then_the_append_and_counts_until_reported() {
        let base = scratch(line!());
        let path = base.join("repo.git/info/exclude");
        fs::write(&path, b"a\n").unwrap();
        let record = base.join("item.exclude-union");
        let out = apply((&record, "item"), &path, b"a\n", &v(&["b", "c"])).unwrap();
        assert_eq!(out, b"a\nb\nc\n");
        assert_eq!(fs::read(&path).unwrap(), out);
        let kept = read_record(&record).unwrap().unwrap();
        assert_eq!(kept.item, "item");
        assert_eq!(kept.appends.len(), 1);
        assert_eq!(kept.appends[0].state, AppendState::Appended);
        assert_eq!(kept.appends[0].lines, v(&["b", "c"]));
        // Counted until a finished landing reports it (review findings 3, 4).
        assert_eq!(unreported(&record, &path).unwrap(), 2);
        reported(&record, &path).unwrap();
        assert_eq!(unreported(&record, &path).unwrap(), 0);
        // A changed file since the decision: nothing written, refused, and
        // nothing recorded or left behind.
        assert_eq!(
            apply((&record, "item"), &path, b"a\n", &v(&["d"])),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        assert_eq!(fs::read(&path).unwrap(), b"a\nb\nc\n");
        assert_eq!(read_record(&record).unwrap().unwrap().appends.len(), 1);
        let names: Vec<_> = fs::read_dir(base.join("repo.git/info"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["exclude"]);
        fs::remove_dir_all(&base).unwrap();
    }

    // Review finding 7: intents settle by identity, never by content.
    #[test]
    fn intents_settle_by_identity() {
        let base = scratch(line!());
        let info = base.join("repo.git/info");
        let path = info.join("exclude");
        fs::write(&path, b"a\nb\n").unwrap();
        let identity = |path: &Path| {
            use std::os::unix::fs::MetadataExt as _;
            let meta = fs::symlink_metadata(path).unwrap();
            (meta.dev(), meta.ino())
        };
        fs::write(info.join(".bulkload-exclude-t1"), b"a\nc\n").unwrap();
        let intent = |temporary: &str, identity: (u64, u64)| Append {
            path: path.as_os_str().as_bytes().to_vec(),
            // A digest match must not settle anything.
            before: digest(b"a\nb\n"),
            after: digest(b"a\nb\n"),
            lines: v(&["z"]),
            temporary: temporary.as_bytes().to_vec(),
            identity,
            state: AppendState::Intent,
            reported: false,
        };
        let record = base.join("item.exclude-union");
        landed::write(
            &record,
            &ExcludeUnion {
                item: "item".to_owned(),
                appends: vec![
                    intent(".bulkload-exclude-t0", identity(&path)),
                    intent(
                        ".bulkload-exclude-t1",
                        identity(&info.join(".bulkload-exclude-t1")),
                    ),
                    intent(".bulkload-exclude-t2", (0, 0)),
                ],
            },
        )
        .unwrap();
        settle(&record, &path).unwrap();
        let states: Vec<AppendState> = read_record(&record)
            .unwrap()
            .unwrap()
            .appends
            .iter()
            .map(|append| append.state)
            .collect();
        assert_eq!(
            states,
            [
                AppendState::Appended,
                AppendState::NotWritten,
                AppendState::Intent
            ]
        );
        assert!(!info.join(".bulkload-exclude-t1").exists());
        assert_eq!(fs::read(&path).unwrap(), b"a\nb\n");
        fs::remove_dir_all(&base).unwrap();
    }
}
