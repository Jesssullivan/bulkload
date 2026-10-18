//! The rebuildable roots a capture omitted, read from what the capture
//! carries in band, for a capture that has no `.omissions` sidecar (M2
//! round 2, d).
//!
//! The `{bundle}.omissions` sidecar is newer than the omission itself: every
//! capture since `bf10700c` (authored 2026-09-22) omits the rebuildable set and names
//! what it omitted in its own bundle, as the metadata ref
//! `refs/carry-export/rebuildable-omissions-v1` (a parentless commit whose
//! tree's `value` blob is the postcard list), and only when it omitted
//! something. The sidecar arrived with `3fe1f075` (2026-10-16). A corpus
//! captured by an engine between the two (the 2026-10-16 in-place batches,
//! captured on neo by `5beb0314`) has no sidecar for any item, and
//! `closure-report` listed every item `unknown`.
//!
//! This reader answers from the bundle's header, which every capture bundle
//! lists its metadata refs in (a ref table moves only native refs out of it,
//! and a thin bundle lists every private ref beside its prerequisites):
//!
//! - no omission ref: the capture omitted nothing (an engine before
//!   `bf10700c` never omitted, and a later one names what it omits);
//! - the ref: its value, read from a repository that holds that object,
//!   the item's destination repository once its import ran, by the oid the
//!   header names, never from the pack (which would mean indexing the whole
//!   bundle);
//! - a shallow custody envelope: its inventory is inside the custody
//!   manifest, read from the same repository, then as above.
//!
//! Reads only: the header from the corpus, objects with `cat-file`,
//! `rev-parse` and `ls-tree` in the holder (`--no-optional-locks`); nothing
//! is unbundled or written. A holder is read only with a `git` at or over
//! the destination minimum (2.46, so `GIT_NO_LAZY_FETCH` keeps a missing
//! object from being fetched) and only when it is not a partial clone, and
//! only the shape a capture writes is taken (a commit whose tree is one
//! `100644 blob` entry named `value`). Objects are not hashed again (see
//! [`verified_value`]). An answer that needs what the holder does not have,
//! or that does not have that shape, is `Unknown`, with its reason, never
//! zero. The header is read strictly: a line or a capability a capture
//! header does not have, a malformed oid, or no ref line at all is
//! `header-unreadable`. Like the sidecar, the reader does not check the
//! bundle's own digest: a capture whose pack was corrupted after its header
//! still reads as its header says.

use crate::refuse::RefuseAt as _;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use super::batch_objects::BatchObjects;
use super::{
    destination_git_ready, git, oid, output, partial_clone, shallow, text, RebuildableOmission,
    REBUILDABLE_METADATA,
};
use crate::{BulkloadRefusal, Result};

/// The most bytes a metadata `value` blob may hold (an omission list or a
/// custody manifest).
const VALUE_CAP: u64 = 16 * 1024 * 1024;

/// What a capture's bundle says it omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InBand {
    /// The omitted roots (empty: none).
    Known(Vec<RebuildableOmission>),
    /// Not answerable from what is at hand; the reason.
    Unknown(&'static str),
}

/// The bundle header cannot be read, is not a capture bundle's, or names
/// the omission list or a custody envelope by a malformed oid.
pub const HEADER_UNREADABLE: &str = "header-unreadable";
/// The omission ref's value was not read: no holder was offered.
pub const VALUE_NOT_READ: &str = "value-not-read";
/// The omission ref's metadata commit is not in the holder (its import has
/// not run).
pub const VALUE_NOT_HELD: &str = "value-not-held";
/// The holder has the metadata commit, but it does not peel to a tree of
/// the one-entry shape a capture writes, its `value` is not a blob, is over
/// the cap, or does not decode.
pub const VALUE_UNREADABLE: &str = "value-unreadable";
/// A shallow custody envelope's manifest was not read: no holder offered.
pub const ENVELOPE_NOT_READ: &str = "envelope-not-read";
/// A shallow custody envelope whose manifest is not in the holder.
pub const ENVELOPE_NOT_HELD: &str = "envelope-not-held";
/// A shallow custody envelope's manifest is held but unreadable (as
/// [`VALUE_UNREADABLE`]).
pub const ENVELOPE_UNREADABLE: &str = "envelope-unreadable";
/// The holder is a partial clone: a read could fault in a lazy fetch (a
/// write into it), so it is not read.
pub const HOLDER_PARTIAL_CLONE: &str = "holder-partial-clone";
/// The `git` on PATH is older than the destination minimum (2.46), which
/// `GIT_NO_LAZY_FETCH` (2.45) needs to keep a read from fetching.
pub const HOLDER_GIT_TOO_OLD: &str = "holder-git-too-old";

/// Why a holder read gave no answer.
enum Missed {
    NotHeld,
    Unreadable,
    Refused(&'static str),
}

/// The rebuildable roots the capture in `bundle` omitted, from its header
/// and, when the header names an omission list or a custody envelope, from
/// `holder`'s object store. Reads only.
///
/// A holder is read only with a `git` at or over the destination minimum
/// (so `GIT_NO_LAZY_FETCH` holds) and only when it is not a partial clone;
/// only the shape a capture writes is taken (see [`verified_value`]).
#[must_use]
pub fn omissions(bundle: &Path, holder: Option<&Path>) -> InBand {
    let Ok(heads) = header(bundle) else {
        return InBand::Unknown(HEADER_UNREADABLE);
    };
    let heads = match value_of(&heads, shallow::CUSTODY) {
        Err(()) => return InBand::Unknown(HEADER_UNREADABLE),
        Ok(None) => heads,
        Ok(Some(custody)) => {
            let Some(holder) = holder else {
                return InBand::Unknown(ENVELOPE_NOT_READ);
            };
            let manifest = verified_value(holder, custody).and_then(|bytes| {
                postcard::from_bytes::<(Vec<u8>, String, String)>(&bytes)
                    .map_err(|_| Missed::Unreadable)
            });
            match manifest {
                Ok((_, inventory, _)) => inventory,
                Err(Missed::NotHeld) => return InBand::Unknown(ENVELOPE_NOT_HELD),
                Err(Missed::Unreadable) => return InBand::Unknown(ENVELOPE_UNREADABLE),
                Err(Missed::Refused(reason)) => return InBand::Unknown(reason),
            }
        }
    };
    let name = format!("refs/carry-export/{REBUILDABLE_METADATA}");
    let commit = match value_of(&heads, &name) {
        Err(()) => return InBand::Unknown(HEADER_UNREADABLE),
        Ok(None) => return InBand::Known(Vec::new()),
        Ok(Some(commit)) => commit,
    };
    let Some(holder) = holder else {
        return InBand::Unknown(VALUE_NOT_READ);
    };
    let omitted = verified_value(holder, commit).and_then(|bytes| {
        postcard::from_bytes::<Vec<RebuildableOmission>>(&bytes).map_err(|_| Missed::Unreadable)
    });
    match omitted {
        Ok(omitted) => InBand::Known(omitted),
        Err(Missed::NotHeld) => InBand::Unknown(VALUE_NOT_HELD),
        Err(Missed::Unreadable) => InBand::Unknown(VALUE_UNREADABLE),
        Err(Missed::Refused(reason)) => InBand::Unknown(reason),
    }
}

/// The value of `name` in `<oid> <name>` lines: `Ok(None)` when no line
/// names it, `Err` when one names it by a malformed oid (never read as
/// absent).
fn value_of<'a>(heads: &'a str, name: &str) -> std::result::Result<Option<&'a str>, ()> {
    for line in heads.lines() {
        if let Some((value, found)) = line.split_once(' ') {
            if found == name {
                return if oid(value) { Ok(Some(value)) } else { Err(()) };
            }
        }
    }
    Ok(None)
}

/// Whether `name` is a ref name a capture header may list: under `refs/`,
/// no space, nothing Git forbids in a header line.
fn ref_name(name: &str) -> bool {
    name.starts_with("refs/")
        && !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte > b' ' && byte != 0x7f && !matches!(byte, b'~' | b'^' | b':' | b'\\'))
}

/// A bundle's ref lines (`<oid> <name>`), read from its header without the
/// pack, under the readers' header cap. Strict: the signature, then (v3)
/// only the `@object-format=` capability, `-<oid>[ <comment>]`
/// prerequisites, at least one `<oid> <refname>` line and the blank line.
/// Anything else (a filter capability, a line that is neither) is not a
/// capture header.
///
/// # Errors
/// An unreadable file, a header that is not a capture bundle's or over the
/// cap.
fn header(bundle: &Path) -> Result<String> {
    const SITE: &str = "git_carry::in_band::header";
    let malformed = || BulkloadRefusal::GitInventoryMalformed;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(bundle)
        .refuse_at(SITE)?;
    let cap = u64::try_from(super::shared::HEADER_CAP).unwrap_or(u64::MAX);
    let mut source = BufReader::new(file.take(cap.saturating_add(1)));
    let mut heads = String::new();
    let mut line = Vec::new();
    source.read_until(b'\n', &mut line).refuse_at(SITE)?;
    let v3 = match line.as_slice() {
        b"# v2 git bundle\n" => false,
        b"# v3 git bundle\n" => true,
        _ => return Err(malformed()),
    };
    loop {
        line.clear();
        let read = source.read_until(b'\n', &mut line).refuse_at(SITE)?;
        if read == 0 || !line.ends_with(b"\n") {
            // No blank line within the cap: not a header this reader takes.
            return Err(malformed());
        }
        let text = std::str::from_utf8(&line).map_err(|_| malformed())?;
        let text = text.strip_suffix('\n').ok_or_else(malformed)?;
        if text.is_empty() {
            return if heads.is_empty() {
                Err(malformed())
            } else {
                Ok(heads)
            };
        }
        if let Some(capability) = text.strip_prefix('@') {
            if !v3 || !matches!(capability, "object-format=sha1" | "object-format=sha256") {
                return Err(malformed());
            }
        } else if let Some(prerequisite) = text.strip_prefix('-') {
            let value = prerequisite
                .split_once(' ')
                .map_or(prerequisite, |(value, _)| value);
            if !oid(value) {
                return Err(malformed());
            }
        } else {
            match text.split_once(' ') {
                Some((value, name)) if oid(value) && ref_name(name) => {
                    heads.push_str(text);
                    heads.push('\n');
                }
                _ => return Err(malformed()),
            }
        }
    }
}

/// The `value` blob of a capture metadata commit, read from `holder`: the
/// commit peeled to its tree (which must not be the oid itself: a tree or a
/// blob named in a commit's place is not a capture's), the tree exactly one
/// `100644 blob <oid>\tvalue` entry, the blob read as a blob under the cap.
///
/// Not hashed again (review of #264, finding 4, left open): Git trusts its
/// own object store, and re-hashing here needs either a SHA-1/SHA-256
/// implementation the agent does not carry or a writer subcommand the S2
/// contract's pinned writer table does not admit (bulkload#188). A loose
/// object planted in the destination under the metadata oid is therefore
/// read as what it says.
fn verified_value(holder: &Path, commit: &str) -> std::result::Result<Vec<u8>, Missed> {
    if destination_git_ready().is_err() {
        return Err(Missed::Refused(HOLDER_GIT_TOO_OLD));
    }
    match partial_clone(holder) {
        Ok(true) => return Err(Missed::Refused(HOLDER_PARTIAL_CLONE)),
        Ok(false) => {}
        Err(_) => return Err(Missed::NotHeld),
    }
    if output(git(holder).args(["cat-file", "-e", commit])).is_err() {
        return Err(Missed::NotHeld);
    }
    let tree = text(git(holder).args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{commit}^{{tree}}"),
    ]))
    .map_err(|_| Missed::Unreadable)?;
    if !oid(&tree) || tree.len() != commit.len() || tree == commit {
        return Err(Missed::Unreadable);
    }
    let entries = output(git(holder).args(["ls-tree", "-z", "--full-tree", &tree]))
        .map_err(|_| Missed::Unreadable)?;
    let entry = std::str::from_utf8(&entries).map_err(|_| Missed::Unreadable)?;
    let blob = entry
        .strip_prefix("100644 blob ")
        .and_then(|rest| rest.strip_suffix("\tvalue\0"))
        .filter(|blob| oid(blob) && blob.len() == commit.len())
        .ok_or(Missed::Unreadable)?;
    let mut reader = BatchObjects::new(holder).map_err(|_| Missed::Unreadable)?;
    let mut bytes = Vec::new();
    reader
        .copy_into(blob, &mut bytes, Some(VALUE_CAP))
        .map_err(|_| Missed::Unreadable)?;
    reader.finish().map_err(|_| Missed::Unreadable)?;
    Ok(bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::process::Command;

    fn bundle(dir: &Path, name: &str, body: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    fn header_only(body: &str) -> InBand {
        let dir = tempfile::tempdir().unwrap();
        omissions(&bundle(dir.path(), "b.bundle", body.as_bytes()), None)
    }

    #[test]
    fn a_header_without_the_omission_ref_is_known_empty() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        assert_eq!(
            header_only(&format!(
                "# v2 git bundle\n-{b} shared base\n-{b}\n{a} refs/carry-export/head\n\nPACK"
            )),
            InBand::Known(Vec::new())
        );
        let long = "e".repeat(64);
        assert_eq!(
            header_only(&format!(
                "# v3 git bundle\n@object-format=sha256\n{long} refs/carry-export/head\n\n"
            )),
            InBand::Known(Vec::new())
        );
    }

    #[test]
    fn a_named_list_or_envelope_without_a_holder_is_unknown_never_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = bundle(
            dir.path(),
            "b.bundle",
            format!(
                "# v2 git bundle\n{} refs/carry-export/rebuildable-omissions-v1\n\nPACK",
                "c".repeat(40)
            )
            .as_bytes(),
        );
        assert_eq!(omissions(&path, None), InBand::Unknown(VALUE_NOT_READ));
        // A holder that is no repository: not held.
        assert_eq!(
            omissions(&path, Some(dir.path())),
            InBand::Unknown(VALUE_NOT_HELD)
        );
        let envelope = bundle(
            dir.path(),
            "e.bundle",
            format!(
                "# v2 git bundle\n{} refs/carry-export/shallow-custody-v1\n\nPACK",
                "d".repeat(40)
            )
            .as_bytes(),
        );
        assert_eq!(
            omissions(&envelope, None),
            InBand::Unknown(ENVELOPE_NOT_READ)
        );
        assert_eq!(
            omissions(&envelope, Some(dir.path())),
            InBand::Unknown(ENVELOPE_NOT_HELD)
        );
    }

    /// Review of #264, findings 1 and 2: a header this reader does not
    /// fully understand is unreadable, never read as "nothing omitted".
    #[test]
    fn a_header_that_is_not_a_capture_header_is_unreadable() {
        let a = "a".repeat(40);
        for body in [
            "not a bundle\n\n".to_owned(),
            "# v2 git bundle\nno blank line".to_owned(),
            "# v2 git bundle\nzzzz refs/carry-export/rebuildable-omissions-v1\n\n".to_owned(),
            "# v2 git bundle\nNOTANOID refs/carry-export/shallow-custody-v1\n\n".to_owned(),
            "# v2 git bundle\ngarbage\n\n".to_owned(),
            "# v2 git bundle\n\n".to_owned(),
            format!("# v2 git bundle\n-{a} shared base\n\n"),
            format!("# v2 git bundle\n-zz shared base\n{a} refs/carry-export/head\n\n"),
            format!("# v3 git bundle\n@filter=blob:none\n{a} refs/carry-export/head\n\n"),
            format!("# v2 git bundle\n@object-format=sha1\n{a} refs/carry-export/head\n\n"),
            format!("# v2 git bundle\n{a} not-a-ref\n\n"),
        ] {
            assert_eq!(
                header_only(&body),
                InBand::Unknown(HEADER_UNREADABLE),
                "{body:?}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            omissions(&dir.path().join("absent.bundle"), None),
            InBand::Unknown(HEADER_UNREADABLE)
        );
        // A name that matches with a malformed oid is never absent.
        assert_eq!(
            value_of(
                "zzzz refs/carry-export/rebuildable-omissions-v1\n",
                "refs/carry-export/rebuildable-omissions-v1"
            ),
            Err(())
        );
        assert_eq!(
            value_of(
                "x refs/other\n",
                "refs/carry-export/rebuildable-omissions-v1"
            ),
            Ok(None)
        );
    }

    fn git_in(repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> String {
        use std::io::Write as _;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(bytes) = stdin {
            child.stdin.take().unwrap().write_all(bytes).unwrap();
        }
        drop(child.stdin.take());
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
    }

    /// A metadata commit in `repo` whose tree is `entries` (mktree lines).
    fn metadata_commit(repo: &Path, entries: &str) -> String {
        let tree = git_in(repo, &["mktree"], Some(entries.as_bytes()));
        git_in(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@localhost",
                "commit-tree",
                &tree,
                "-m",
                "rebuildable-omissions-v1",
            ],
            None,
        )
    }

    /// The list is read from a holder, every object hashed again, and only
    /// the one-entry shape a capture writes is taken (review findings 4, 5);
    /// a partial clone is never read (finding 3).
    #[test]
    fn a_named_list_is_read_verified_from_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("holder");
        fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-q", "--template="], None);
        let list = vec![RebuildableOmission {
            rel_path: b"node_modules".to_vec(),
            bytes: 11,
            entries: 2,
        }];
        let value = git_in(
            &repo,
            &["hash-object", "-w", "--stdin"],
            Some(&postcard::to_allocvec(&list).unwrap()),
        );
        let junk = git_in(
            &repo,
            &["hash-object", "-w", "--stdin"],
            Some(b"\xff\xff\xff"),
        );
        let named = |commit: &str, name: &str| {
            bundle(
                dir.path(),
                name,
                format!("# v2 git bundle\n{commit} refs/carry-export/rebuildable-omissions-v1\n\n")
                    .as_bytes(),
            )
        };
        let good = metadata_commit(&repo, &format!("100644 blob {value}\tvalue\n"));
        assert_eq!(
            omissions(&named(&good, "good.bundle"), Some(&repo)),
            InBand::Known(list)
        );
        let two = metadata_commit(
            &repo,
            &format!("100644 blob {value}\tvalue\n100644 blob {value}\tvalue2\n"),
        );
        let renamed = metadata_commit(&repo, &format!("100644 blob {value}\tother\n"));
        let undecodable = metadata_commit(&repo, &format!("100644 blob {junk}\tvalue\n"));
        for (commit, name) in [(&two, "two"), (&renamed, "renamed"), (&undecodable, "junk")] {
            assert_eq!(
                omissions(&named(commit, name), Some(&repo)),
                InBand::Unknown(VALUE_UNREADABLE),
                "{name}"
            );
        }
        // The blob named where a commit should be: not a commit's bytes.
        assert_eq!(
            omissions(&named(&value, "blob"), Some(&repo)),
            InBand::Unknown(VALUE_UNREADABLE)
        );
        assert_eq!(
            omissions(&named(&"f".repeat(40), "absent"), Some(&repo)),
            InBand::Unknown(VALUE_NOT_HELD)
        );
        git_in(
            &repo,
            &["config", "core.repositoryformatversion", "1"],
            None,
        );
        git_in(
            &repo,
            &["config", "extensions.partialClone", "origin"],
            None,
        );
        assert_eq!(
            omissions(&named(&good, "good.bundle"), Some(&repo)),
            InBand::Unknown(HOLDER_PARTIAL_CLONE)
        );
    }
}
