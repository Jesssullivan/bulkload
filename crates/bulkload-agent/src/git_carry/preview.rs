//! The plan's preview of a B2 consolidation.
//!
//! SPEC rules 3, 4 and 7 (2026-10-21): what a pass would do to each item, from the source's item
//! snapshot, the live repository, and one read-only negotiation with the
//! source ([`answer`], the `negotiate` verb). Nothing here writes a
//! repository: every Git child is a read, the source's included.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{git, input, oid, output};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "git_carry::preview";

/// The largest file the negotiation returns whole for a three-way preview;
/// a larger one is counted unknown.
pub const FILE_CAP: u64 = 4 << 20;

/// One repository's refs question to the source.
///
/// For each `(live, source)`
/// value pair, whether the live value is an ancestor of the source's; and
/// the bytes the source would send for `wants` when the destination has
/// `haves`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefsAsk {
    pub repo: PathBuf,
    pub pairs: Vec<(String, String)>,
    pub wants: Vec<String>,
    pub haves: Vec<String>,
}

/// The source's answer to a [`RefsAsk`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefsAnswer {
    /// Per pair: `Some(true)` when the live value is the source value's
    /// ancestor, `Some(false)` when it is not (or the source lacks it),
    /// `None` when the source could not tell.
    pub ancestry: Vec<Option<bool>>,
    /// The on-disk bytes of the objects the wants reach and the haves do
    /// not (`rev-list --objects --disk-usage`); `None` when it could not
    /// be measured.
    pub fetch_bytes: Option<u64>,
}

/// One worktree's files question: the source's status, and its bytes and
/// merge base for every path that may differ from the destination's
/// checkout at `head` with `dirty` paths changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesAsk {
    pub worktree: PathBuf,
    /// The destination's HEAD after the pass's ref union and switch.
    pub head: Option<String>,
    /// The destination worktree's changed and untracked paths.
    pub dirty: Vec<Vec<u8>>,
    /// Whether file bytes are wanted (a consolidation), or the status only
    /// (a first landing).
    pub bytes: bool,
}

/// What a source path's file holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theirs {
    Absent,
    Bytes(Vec<u8>),
    /// Over [`FILE_CAP`], a link or a special file: not previewed.
    Unknown,
}

/// The source's side of one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAnswer {
    pub path: Vec<u8>,
    pub theirs: Theirs,
    /// The path's bytes at the merge base of the two HEADs, when there is
    /// one and it holds the path.
    pub base: Option<Vec<u8>>,
}

/// A source status entry's class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Class {
    /// Tracked and changed (staged or not).
    Changed,
    Untracked,
    Ignored,
}

impl Class {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Untracked => "untracked",
            Self::Ignored => "ignored",
        }
    }
}

/// The source's answer to a [`FilesAsk`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesAnswer {
    pub status: Vec<(Class, Vec<u8>)>,
    /// Whether the paths the two HEADs differ in are known (the source
    /// holds the destination's HEAD, or they are equal).
    pub diff_known: bool,
    pub files: Vec<FileAnswer>,
    /// The source's worktree could not be read: the refusal's code and its
    /// detail (the errno of an unreadable root, a Git child's stderr
    /// class), 2026-10-22.
    pub unreadable: Option<(String, String)>,
}

/// A negotiation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    Refs(Vec<RefsAsk>),
    Files(Vec<FilesAsk>),
}

/// A negotiation answer, one per question in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    Refs(Vec<RefsAnswer>),
    Files(Vec<FilesAnswer>),
}

/// Answer `ask` on the source. Reads only.
#[must_use]
pub fn answer(ask: &Ask) -> Answer {
    match ask {
        Ask::Refs(asks) => Answer::Refs(asks.iter().map(refs_answer).collect()),
        Ask::Files(asks) => Answer::Files(asks.iter().map(files_answer).collect()),
    }
}

/// The ids of `ids` the store of `repo` holds.
///
/// # Errors
/// A Git child's refusal.
pub fn held(repo: &Path, ids: &[String]) -> Result<BTreeSet<String>> {
    let ids: Vec<&String> = ids.iter().filter(|id| oid(id)).collect();
    if ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let request = ids.iter().fold(String::new(), |mut all, id| {
        all.push_str(id);
        all.push('\n');
        all
    });
    let answered = input(
        git(repo).args(["cat-file", "--batch-check=%(objectname)"]),
        request.as_bytes(),
    )?;
    Ok(String::from_utf8_lossy(&answered)
        .lines()
        .filter(|line| oid(line))
        .map(str::to_owned)
        .collect())
}

// A Git child whose exit code is the answer (0 or 1), run once more with
// the commit-graph off when it fails otherwise (a stale graph).
fn code(repo: &Path, args: &[&str]) -> Option<i32> {
    let ran = git(repo).args(args).output().ok()?;
    if let Some(code @ (0 | 1)) = ran.status.code() {
        return Some(code);
    }
    let mut again = git(repo);
    super::without_graph(&mut again);
    again
        .args(args)
        .output()
        .ok()?
        .status
        .code()
        .filter(|code| matches!(code, 0 | 1))
}

/// Whether `ancestor` is an ancestor of `of` in `repo`; `None` when Git
/// cannot tell.
#[must_use]
pub fn is_ancestor(repo: &Path, ancestor: &str, of: &str) -> Option<bool> {
    if !oid(ancestor) || !oid(of) {
        return None;
    }
    code(repo, &["merge-base", "--is-ancestor", ancestor, of]).map(|code| code == 0)
}

fn refs_answer(ask: &RefsAsk) -> RefsAnswer {
    let mut ids: Vec<String> = ask.pairs.iter().map(|(live, _)| live.clone()).collect();
    ids.extend(ask.haves.iter().cloned());
    let Ok(held) = held(&ask.repo, &ids) else {
        return RefsAnswer {
            ancestry: vec![None; ask.pairs.len()],
            fetch_bytes: None,
        };
    };
    let ancestry = ask
        .pairs
        .iter()
        .map(|(live, source)| {
            if held.contains(live) {
                is_ancestor(&ask.repo, live, source)
            } else {
                // An ancestor of the source's value is in its store.
                Some(false)
            }
        })
        .collect();
    let mut request = String::new();
    for want in ask.wants.iter().filter(|id| oid(id)) {
        request.push_str(want);
        request.push('\n');
    }
    request.push_str("--not\n");
    for have in ask.haves.iter().filter(|have| held.contains(*have)) {
        request.push_str(have);
        request.push('\n');
    }
    let fetch_bytes = if ask.wants.is_empty() {
        Some(0)
    } else {
        input(
            git(&ask.repo).args([
                "rev-list",
                "--objects",
                "--disk-usage",
                "--ignore-missing",
                "--stdin",
            ]),
            request.as_bytes(),
        )
        .ok()
        .and_then(|out| String::from_utf8_lossy(&out).trim().parse().ok())
    };
    RefsAnswer {
        ancestry,
        fetch_bytes,
    }
}

/// A worktree's status: changed, untracked and ignored paths (ignored
/// directories as one entry each). Reads only.
///
/// # Errors
/// A Git child's refusal.
pub fn status(worktree: &Path, ignored: bool) -> Result<Vec<(Class, Vec<u8>)>> {
    let mut args = vec![
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=all",
    ];
    if ignored {
        args.push("--ignored=traditional");
    }
    let listed = output(git(worktree).args(&args))?;
    let mut fields = listed.split(|byte| *byte == 0);
    let mut found = Vec::new();
    while let Some(field) = fields.next() {
        let (Some(xy), Some(path)) = (field.get(..2), field.get(3..)) else {
            continue;
        };
        let class = match xy {
            b"??" => Class::Untracked,
            b"!!" => Class::Ignored,
            _ => Class::Changed,
        };
        // A rename or copy names its origin in the next field.
        if matches!(xy.first(), Some(b'R' | b'C')) {
            fields.next();
        }
        found.push((class, path.to_vec()));
    }
    Ok(found)
}

// A relative path inside a worktree: no root, no `..`, no empty part.
fn contained(path: &[u8]) -> bool {
    !path.is_empty()
        && !path.starts_with(b"/")
        && path
            .split(|byte| *byte == b'/')
            .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

fn path_of(root: &Path, rel: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    root.join(std::ffi::OsStr::from_bytes(rel))
}

/// What the regular file at `root/rel` holds, at most [`FILE_CAP`] bytes.
#[must_use]
pub fn file_at(root: &Path, rel: &[u8]) -> Theirs {
    let path = path_of(root, rel);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Theirs::Absent,
        Ok(meta) if meta.is_file() && meta.len() <= FILE_CAP => {
            fs::read(&path).map_or(Theirs::Unknown, Theirs::Bytes)
        }
        Ok(meta) if meta.is_dir() => Theirs::Absent,
        _ => Theirs::Unknown,
    }
}

fn blob_at(repo: &Path, commit: &str, rel: &[u8]) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt as _;
    let mut spec = format!("{commit}:").into_bytes();
    spec.extend_from_slice(rel);
    let ran = git(repo)
        .args(["cat-file", "blob"])
        .arg(std::ffi::OsString::from_vec(spec))
        .output()
        .ok()?;
    ran.status.success().then_some(ran.stdout)
}

fn head(repo: &Path) -> Option<String> {
    let ran = git(repo)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .ok()?;
    let head = String::from_utf8_lossy(&ran.stdout).trim().to_owned();
    (ran.status.success() && oid(&head)).then_some(head)
}

fn files_answer(ask: &FilesAsk) -> FilesAnswer {
    let unreadable = |refusal: &BulkloadRefusal| FilesAnswer {
        unreadable: Some((refusal.code().to_owned(), format!("{refusal:?}"))),
        ..FilesAnswer::default()
    };
    if let Err(error) = fs::read_dir(&ask.worktree) {
        return unreadable(&BulkloadRefusal::SourceRootUnreadable(error.raw_os_error()));
    }
    let status = match status(&ask.worktree, true) {
        Ok(status) => status,
        Err(refusal) => return unreadable(&refusal),
    };
    if !ask.bytes {
        return FilesAnswer {
            status,
            diff_known: true,
            ..FilesAnswer::default()
        };
    }
    let source_head = head(&ask.worktree);
    let mut candidates: BTreeSet<Vec<u8>> = status
        .iter()
        .filter(|(class, _)| *class != Class::Ignored)
        .map(|(_, path)| path.clone())
        .collect();
    candidates.extend(ask.dirty.iter().cloned());
    let dest_head = ask.head.as_deref().filter(|head| oid(head));
    let held_head = dest_head.is_some_and(|head| {
        held(&ask.worktree, &[head.to_owned()]).is_ok_and(|held| held.contains(head))
    });
    let mut diff_known = dest_head.is_none() || dest_head == source_head.as_deref();
    let base =
        if let (Some(dest), Some(ours), true) = (dest_head, source_head.as_deref(), held_head) {
            if dest != ours {
                if let Ok(names) =
                    output(git(&ask.worktree).args(["diff", "--name-only", "-z", dest, ours]))
                {
                    candidates.extend(
                        names
                            .split(|byte| *byte == 0)
                            .filter(|name| !name.is_empty())
                            .map(<[u8]>::to_vec),
                    );
                    diff_known = true;
                }
            }
            output(git(&ask.worktree).args(["merge-base", dest, ours]))
                .ok()
                .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
                .filter(|base| oid(base))
        } else {
            None
        };
    let files = candidates
        .into_iter()
        .filter(|path| contained(path))
        .map(|path| FileAnswer {
            theirs: file_at(&ask.worktree, &path),
            base: base
                .as_deref()
                .and_then(|base| blob_at(&ask.worktree, base, &path)),
            path,
        })
        .collect();
    FilesAnswer {
        status,
        diff_known,
        files,
        unreadable: None,
    }
}

/// A live repository's refs, `(name, value)`.
///
/// # Errors
/// A Git child's refusal.
pub fn live_refs(repo: &Path) -> Result<BTreeMap<String, String>> {
    let listed = output(git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
    Ok(String::from_utf8_lossy(&listed)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(value, name)| (name.to_owned(), value.to_owned()))
        .collect())
}

/// The worktrees of the live `repo`, each with the branch it has checked
/// out (`None` detached).
///
/// # Errors
/// A Git child's refusal.
pub fn worktrees(repo: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    use std::os::unix::ffi::OsStrExt as _;
    let listed = output(git(repo).args(["worktree", "list", "--porcelain", "-z"]))?;
    let mut found: Vec<(PathBuf, Option<String>)> = Vec::new();
    for field in listed.split(|byte| *byte == 0) {
        if let Some(at) = field.strip_prefix(b"worktree ") {
            found.push((PathBuf::from(std::ffi::OsStr::from_bytes(at)), None));
        } else if let (Some(branch), Some(last)) =
            (field.strip_prefix(b"branch "), found.last_mut())
        {
            last.1 = Some(String::from_utf8_lossy(branch).into_owned());
        }
    }
    Ok(found)
}

/// What the ref union would do to one ref (the words of
/// [`super::union::RefAction`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefPreview {
    pub name: String,
    pub word: &'static str,
    /// The value the live ref would hold after the union.
    pub after: String,
}

/// Preview [`super::union::refs`] for `source_refs` into the live `repo`.
///
/// `forwards(live, source)` says whether the live value is an ancestor of
/// the source's (the source's answer); the reverse is read here, where the
/// live store must hold the source's value.
///
/// # Errors
/// A Git child's refusal.
pub fn refs_preview(
    repo: &Path,
    source_refs: &[(String, String)],
    forwards: &dyn Fn(&str, &str) -> Option<bool>,
) -> Result<Vec<RefPreview>> {
    let live = live_refs(repo)?;
    let checked_out = worktrees(repo)?;
    let mut done = Vec::new();
    for (name, value) in source_refs {
        if name == "refs/stash" || name.starts_with("refs/remotes/") {
            continue;
        }
        let (word, after) = match live.get(name) {
            None => ("created", value.clone()),
            Some(ours) if ours == value => ("same", value.clone()),
            Some(ours) if forwards(ours, value) == Some(true) => {
                match checked_out
                    .iter()
                    .find(|(_, branch)| branch.as_deref() == Some(name.as_str()))
                {
                    None => ("forwarded", value.clone()),
                    Some((worktree, _)) if super::union::is_clean(worktree)? => {
                        ("switched", value.clone())
                    }
                    Some(_) => ("tracked", ours.clone()),
                }
            }
            Some(ours) if is_ancestor(repo, value, ours) == Some(true) => ("kept", ours.clone()),
            Some(ours) => ("tracked", ours.clone()),
        };
        done.push(RefPreview {
            name: name.clone(),
            word,
            after,
        });
    }
    Ok(done)
}

/// One path's verdict in a consolidation preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// New at the live path: written.
    Land,
    /// Both sides changed it; Git's three-way merge takes it cleanly.
    Merge,
    /// A conflicting hunk, or no merge base holding it: held typed.
    Conflict,
    /// Equal already, or the live side already holds the source's change.
    Same,
    /// Too large, a link, or unreadable: not previewed.
    Unknown,
}

impl Verdict {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Land => "land",
            Self::Merge => "merged",
            Self::Conflict => "conflict",
            Self::Same => "same",
            Self::Unknown => "unknown",
        }
    }
}

/// The verdict for `answer` against the live worktree `live`, merging in
/// `scratch` (swept, as the pass's merges are).
///
/// # Errors
/// A Git child's refusal; IO.
pub fn verdict(live: &Path, answer: &FileAnswer, scratch: &Path) -> Result<Verdict> {
    let theirs = match &answer.theirs {
        // The source no longer holds it: the pass removes nothing.
        Theirs::Absent => return Ok(Verdict::Same),
        Theirs::Unknown => return Ok(Verdict::Unknown),
        Theirs::Bytes(bytes) => bytes,
    };
    let ours = match file_at(live, &answer.path) {
        Theirs::Absent => return Ok(Verdict::Land),
        Theirs::Unknown => return Ok(Verdict::Unknown),
        Theirs::Bytes(bytes) => bytes,
    };
    if ours == *theirs {
        return Ok(Verdict::Same);
    }
    let Some(base) = &answer.base else {
        return Ok(Verdict::Conflict);
    };
    if *base == ours {
        return Ok(Verdict::Merge);
    }
    if base == theirs {
        return Ok(Verdict::Same);
    }
    Ok(
        match super::union::merge_file(scratch, base, &ours, theirs)? {
            Some(merged) if merged == ours => Verdict::Same,
            Some(_) => Verdict::Merge,
            None => Verdict::Conflict,
        },
    )
}

/// The sibling paths of `place` a pass from before SPEC rule 3 left
/// (`<name>.neo-*`, `<name>.sting-*`, `<name>.<label>`): duplicates the
/// rule retires into the live path.
///
/// # Errors
/// IO reading the parent directory.
pub fn asides(place: &Path, label: &str) -> Result<Vec<PathBuf>> {
    use std::os::unix::ffi::OsStrExt as _;
    let (Some(parent), Some(name)) = (place.parent(), place.file_name()) else {
        return Ok(Vec::new());
    };
    let mut prefix = name.as_bytes().to_vec();
    prefix.push(b'.');
    let read = match fs::read_dir(parent) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    let mut found = Vec::new();
    for entry in read {
        let entry = entry.refuse_at(SITE)?;
        let file_name = entry.file_name();
        let Some(suffix) = file_name.as_bytes().strip_prefix(prefix.as_slice()) else {
            continue;
        };
        if suffix.starts_with(b"neo-")
            || suffix.starts_with(b"sting-")
            || suffix == label.as_bytes()
        {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// Frame `value` for the negotiation wire: its postcard length, then it.
///
/// # Errors
/// `FRAME_CODEC` when it does not encode.
pub fn frame<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let body = postcard::to_allocvec(value).map_err(|_| BulkloadRefusal::FrameCodec)?;
    let mut framed = (body.len() as u64).to_le_bytes().to_vec();
    framed.extend(body);
    Ok(framed)
}

/// Read one [`frame`] from `reader`.
///
/// # Errors
/// `FRAME_CODEC` when it does not decode; IO.
pub fn unframe<T: for<'de> Deserialize<'de>>(reader: &mut impl std::io::Read) -> Result<T> {
    let mut length = [0u8; 8];
    reader.read_exact(&mut length).refuse_at(SITE)?;
    let length =
        usize::try_from(u64::from_le_bytes(length)).map_err(|_| BulkloadRefusal::FrameCodec)?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).refuse_at(SITE)?;
    postcard::from_bytes(&body).map_err(|_| BulkloadRefusal::FrameCodec)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn only_contained_paths_are_read() {
        assert!(contained(b"a/b"));
        for path in [
            &b""[..],
            b"/etc/passwd",
            b"../x",
            b"a/../b",
            b"a//b",
            b"./a",
        ] {
            assert!(!contained(path), "{path:?}");
        }
    }

    #[test]
    fn a_frame_round_trips() {
        let ask = Ask::Refs(vec![RefsAsk {
            repo: PathBuf::from("/r"),
            pairs: vec![("a".into(), "b".into())],
            ..RefsAsk::default()
        }]);
        let framed = frame(&ask).unwrap();
        let back: Ask = unframe(&mut framed.as_slice()).unwrap();
        assert_eq!(back, ask);
    }

    // 2026-10-22: an unreadable source worktree is typed with its errno.
    #[test]
    fn an_unreadable_worktree_is_typed_with_its_errno() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("w");
        fs::create_dir(&worktree).unwrap();
        fs::set_permissions(&worktree, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&worktree).is_ok();
        let answer = answer(&Ask::Files(vec![FilesAsk {
            worktree: worktree.clone(),
            ..FilesAsk::default()
        }]));
        fs::set_permissions(&worktree, fs::Permissions::from_mode(0o700)).unwrap();
        if readable {
            return; // root reads anything: nothing to prove here
        }
        let Answer::Files(files) = answer else {
            panic!("not a files answer");
        };
        let (code, detail) = files[0].unreadable.clone().unwrap();
        assert_eq!(code, "SOURCE_ROOT_UNREADABLE");
        assert!(detail.contains("13"), "{detail}");
    }
}
