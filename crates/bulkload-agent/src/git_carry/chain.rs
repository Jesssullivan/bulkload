//! Auto-prerequisite chains for v1 capture bundles (WP2 PR 2, OI-1003-Q15).
//!
//! Without a shared plan base, a changed capture used to run
//! `bundle create --all`, re-packing the item's whole history from the source
//! on every rerun. A capture that follows a retained capture of the same
//! checkout now declares that capture's tips as its prerequisites (`^tip`),
//! so it packs only what is new since then.
//!
//! **Which tips.** Only a retained tip whose commit the *source* object store
//! holds is a prerequisite (peeled to its commit, checked by
//! `cat-file --batch-check` on the source, with lazy fetch off). The capture's
//! own metadata commits (staged, worktree, filesystem rows) live only in its
//! bundle, so declaring them would make the next capture's blob-reuse fetch,
//! and every later chain link, depend on objects no private repository holds.
//! Source-held prerequisites keep every link fetchable wherever the source
//! object store is an alternate. A retained tip the source no longer holds
//! (a branch deleted and pruned) is simply not excluded: the capture packs
//! more, never less. No source-held tip at all means a self-contained bundle.
//!
//! **Bounded depth.** A self-contained bundle has depth 0; a bundle chained on
//! a depth-d bundle has depth d + 1. A capture whose prior is already at
//! [`CHAIN_DEPTH_LIMIT`] re-bases: it writes a self-contained bundle, and the
//! chain starts again from it. Every restore therefore stages and fetches at
//! most `CHAIN_DEPTH_LIMIT + 1` bundles, and a broken link costs at most one
//! full re-pack on the next capture.
//!
//! **Restore verifies the chain.** [`flatten`] stages every link privately,
//! checks each against its recorded digest, requires the oldest to be
//! self-contained and every later one's prerequisites to be satisfied by the
//! links before it (`GIT_INVENTORY_MISSING_PREREQUISITE` otherwise), and
//! writes one self-contained bundle whose advertised refs must equal the head
//! bundle's exactly. The restore and import verbs read only that bundle.

use crate::refuse::RefuseAt as _;
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::{
    bundle_object_format, copy_hashing, estimate, git, input, oid, output, shared, text,
    verify_bundle, PrivateDir, StagedBundle,
};
use crate::{BulkloadRefusal, Result};

/// The deepest a capture chain may grow before a capture re-bases.
///
/// Eight: a rerun-heavy item (a live checkout captured after each work lane)
/// re-packs its full history at most once per nine changed passes, and a
/// restore stages and fetches at most nine bundles. The bound is on restore
/// work and on the blast radius of one lost link, not on correctness.
pub const CHAIN_DEPTH_LIMIT: u32 = 8;

/// Refs a bundle advertises (`<oid> <refname>` header lines), in header order.
/// Only the header is read: no pack byte is fetched.
fn advertised(bundle: &Path) -> Result<Vec<(String, String)>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(bundle)
        .refuse_at("git_carry::chain::advertised")?;
    let mut source = BufReader::new(file);
    let mut consumed = 0usize;
    let mut refs = Vec::new();
    loop {
        let mut line = Vec::new();
        let count = source
            .by_ref()
            .take(1024 * 1024)
            .read_until(b'\n', &mut line)
            .refuse_at("git_carry::chain::advertised")?;
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
            return Ok(refs);
        }
        if consumed == count || line.starts_with(b"-") || line.starts_with(b"@") {
            continue;
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
            .trim_end_matches('\n');
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        refs.push((value.to_owned(), name.to_owned()));
    }
}

/// The commits a capture chained on `prior` may declare as prerequisites:
/// each of `prior`'s advertised tips, peeled to a commit, that the `source`
/// object store holds. Empty means the capture must be self-contained.
///
/// # Errors
/// Refuses a malformed `prior` header or a failed source read.
pub(super) fn source_held_tips(source: &Path, prior: &Path) -> Result<BTreeSet<String>> {
    let tips: BTreeSet<String> = advertised(prior)?
        .into_iter()
        .map(|(value, _)| value)
        .collect();
    if tips.is_empty() {
        return Ok(BTreeSet::new());
    }
    let request = tips.iter().fold(String::new(), |mut lines, value| {
        lines.push_str(value);
        lines.push_str("^{commit}\n");
        lines
    });
    // A read-only batch query of the source object store through the
    // estimate's hardened source wrapper (WP0(b), S2): resolved git dir,
    // ceiling-fenced discovery, no optional locks, no maintenance, and lazy
    // fetch off, so a promisor source never fetches to answer it. `input`
    // drains answers while it writes requests, so a prior advertising
    // thousands of refs cannot fill the answer pipe and hang the capture.
    let repository = estimate::Repository::local(source)?;
    let answer = input(
        estimate::hardened(&repository)
            .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
        request.as_bytes(),
    )?;
    let answer =
        std::str::from_utf8(&answer).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let mut held = BTreeSet::new();
    for line in answer.lines() {
        // A missing name answers `<name> missing`; only `<oid> commit` counts.
        if let Some((value, "commit")) = line.split_once(' ') {
            if !oid(value) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            held.insert(value.to_owned());
        }
    }
    Ok(held)
}

/// Turn a staged chained capture into one self-contained staged bundle.
///
/// `links` are the bundles it chains on, oldest first, each with the digest
/// its capture recorded. The oldest must be self-contained; each later one,
/// and finally `head`, must have every prerequisite satisfied by the links
/// before it. The result advertises exactly `head`'s refs, carries `head`'s
/// digest (the capture it stands for), and lives in its own private directory
/// beside the oldest link, removed when it drops.
///
/// # Errors
/// `DIGEST_MISMATCH` for a link whose bytes are not the recorded ones,
/// `RECEIPT_BINDING_INVALID` for an oldest link that is not self-contained,
/// `GIT_INVENTORY_MISSING_PREREQUISITE` for a link the chain cannot satisfy,
/// and `GIT_INVENTORY_MALFORMED` when the flattened refs are not `head`'s.
pub fn flatten(head: StagedBundle, links: &[(PathBuf, [u8; 32])]) -> Result<StagedBundle> {
    let (oldest, _) = links
        .first()
        .ok_or(BulkloadRefusal::ReceiptBindingInvalid)?;
    if links.len() > usize::try_from(CHAIN_DEPTH_LIMIT).unwrap_or(usize::MAX) {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    let directory = PrivateDir::create(oldest.parent())?;
    let heads = text(
        git(directory.path())
            .args(["bundle", "list-heads"])
            .arg(head.path()),
    )?;
    let format = bundle_object_format(&heads)?;
    let repository = directory.path().join("chain.git");
    output(
        git(directory.path())
            .args([
                "init",
                "--bare",
                "--quiet",
                "--template=",
                &format!("--object-format={format}"),
            ])
            .arg(&repository),
    )?;
    for (index, (link, digest)) in links.iter().enumerate() {
        let staged = directory.path().join(format!("link-{index}.bundle"));
        if copy_hashing(link, &staged)? != *digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        if index == 0 && !shared::prerequisites(&staged)?.is_empty() {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        verify_bundle(&repository, &staged)?;
        output(
            git(&repository)
                .args(["fetch", "--no-tags", "--quiet"])
                .arg(&staged)
                .arg(format!("+refs/*:refs/carry-chain/{index}/*")),
        )?;
    }
    verify_bundle(&repository, head.path())?;
    output(
        git(&repository)
            .args(["fetch", "--no-tags", "--quiet"])
            .arg(head.path())
            .arg("+refs/*:refs/*"),
    )?;
    // The links' refs only carried their objects here; none is restored.
    let transient = text(
        git(&repository)
            .args(["for-each-ref", "--format=%(refname)"])
            .arg("refs/carry-chain/"),
    )?;
    let deletions = transient.lines().fold(String::new(), |mut lines, name| {
        lines.push_str("delete ");
        lines.push_str(name);
        lines.push('\n');
        lines
    });
    if !deletions.is_empty() {
        input(
            git(&repository).args(["update-ref", "--stdin"]),
            deletions.as_bytes(),
        )?;
    }
    let flat = directory.path().join("capture.bundle");
    output(
        git(&repository)
            .args(["bundle", "create"])
            .arg(&flat)
            .arg("--all"),
    )?;
    crate::counters::add(
        crate::counters::Counter::BundleStageWrite,
        fs::symlink_metadata(&flat)
            .refuse_at("git_carry::chain::flatten")?
            .len(),
    );
    let sorted = |listing: &str| {
        let mut entries: Vec<String> = listing.lines().map(str::to_owned).collect();
        entries.sort_unstable();
        entries
    };
    let flattened = text(git(&repository).args(["bundle", "list-heads"]).arg(&flat))?;
    if sorted(&flattened) != sorted(&heads) || !shared::prerequisites(&flat)?.is_empty() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let digest = head.digest();
    // The flattened refs are exactly `head`'s, so is its bare marker.
    let bare = head.bare;
    // The head's own stage is no longer read; remove it now.
    drop(head);
    Ok(StagedBundle {
        _directory: directory,
        bundle: flat,
        digest,
        bare,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::os::unix::fs::DirBuilderExt;
    use std::process::Command;

    /// Liveness (R33: a refusal is a value, never a hang): a prior that
    /// advertises thousands of refs answers in full. Writing every
    /// `cat-file --batch-check` request before reading any answer blocked
    /// both processes once the answer pipe filled (about 2-4k refs on
    /// Linux, fewer on Darwin), holding the plan lock forever.
    #[test]
    fn source_held_tips_answers_a_prior_with_many_refs() {
        const TIPS: usize = 12_000;
        let root =
            std::env::temp_dir().join(format!("tcfs-chain-many-refs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&source)
                .args([
                    "-c",
                    "user.name=Bulkload test",
                    "-c",
                    "user.email=test@localhost",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "--template=", "-b", "main"]);
        fs::write(source.join("file"), b"held").unwrap();
        git(&["add", "file"]);
        git(&["commit", "-m", "held"]);
        let head = git(&["rev-parse", "HEAD"]);
        // A header-only prior: one held tip and thousands the source lacks.
        let mut header = String::from("# v2 git bundle\n");
        writeln!(header, "{head} refs/heads/main").unwrap();
        for index in 0..TIPS {
            let missing = blake3::hash(format!("tip {index}").as_bytes()).to_hex();
            writeln!(header, "{} refs/tags/t{index}", &missing[..40]).unwrap();
        }
        header.push('\n');
        let prior = root.join("prior.bundle");
        fs::write(&prior, header).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let (queried, bundle) = (source, prior);
        std::thread::spawn(move || {
            let _ = sender.send(source_held_tips(&queried, &bundle));
        });
        let answered = receiver
            .recv_timeout(std::time::Duration::from_mins(2))
            .expect("source_held_tips must return, not block on a full pipe")
            .unwrap();
        assert_eq!(answered, BTreeSet::from([head]));
        let _ = fs::remove_dir_all(&root);
    }
}
