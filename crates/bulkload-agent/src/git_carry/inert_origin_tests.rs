//! #216 (OI-1003-Q129, OI-1003-Q135): a standalone restore carries a
//! non-HTTPS origin as preserved-only configuration instead of refusing, and
//! every refusal comes before the destination exists.
//!
//! Each origin shape restores a complete standalone checkout. Only a plain
//! HTTPS origin activates; any other leaves the restored repository with no
//! `origin` remote and no branch naming one, and the activation receipt
//! names `remote.origin.url`, the origin's fetch refspec and
//! `branch.*.remote` as preserved-only. A refused mapping or a refusal while
//! the checkout is materialized leaves no destination and no stage, so the
//! corrected rerun restores.

use std::fs;
use std::path::{Path, PathBuf};

use super::{
    attach_standalone_payload, export_repository, git, linked_destination, metadata, output,
    restore_bundle, restore_bundle_configured, text,
};
use crate::BulkloadRefusal;

// (activated keys, preserved-only keys, mapping): the receipt's codec.
type Activation = (Vec<String>, Vec<String>, Option<(String, String)>);

fn fresh(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bulkload-inert-origin-{name}-{}",
        std::process::id()
    ));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir(&root).unwrap();
    fs::canonicalize(root).unwrap()
}

// A source with a commit, staged and unstaged dirt, an untracked file,
// tracking configuration for its branch, one unsafe key, and `origins` as its
// `remote.origin.url` values (none for an empty slice). Returns the source,
// its branch's full ref, and its capture bundle.
fn captured(root: &Path, origins: &[&str]) -> (PathBuf, String, PathBuf) {
    let (source, symbolic) = dirty_source(root, origins);
    let bundle = export_repository(&source, &root.join("capture")).unwrap();
    (source, symbolic, bundle)
}

// The source `captured` exports, not yet captured.
fn dirty_source(root: &Path, origins: &[&str]) -> (PathBuf, String) {
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    output(git(&source).args(["init", "--template="])).unwrap();
    output(git(&source).args(["config", "user.name", "Test"])).unwrap();
    output(git(&source).args(["config", "user.email", "test@localhost"])).unwrap();
    fs::write(source.join("tracked"), b"base").unwrap();
    output(git(&source).args(["add", "."])).unwrap();
    output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
    let symbolic = text(git(&source).args(["symbolic-ref", "HEAD"])).unwrap();
    let branch = symbolic.strip_prefix("refs/heads/").unwrap().to_owned();
    for origin in origins {
        output(git(&source).args(["config", "--add", "remote.origin.url", origin])).unwrap();
    }
    if !origins.is_empty() {
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
    }
    output(git(&source).args(["config", "credential.helper", "!unsafe-helper"])).unwrap();
    fs::write(source.join("tracked"), b"staged").unwrap();
    output(git(&source).args(["add", "."])).unwrap();
    fs::write(source.join("tracked"), b"dirty").unwrap();
    fs::write(source.join("untracked"), b"kept").unwrap();
    (source, symbolic)
}

fn activation(destination: &Path) -> Activation {
    postcard::from_bytes(
        &fs::read(destination.join(".git/carry-config/configuration-activation.postcard")).unwrap(),
    )
    .unwrap()
}

// No private restore stage outlives its restore, refused or not.
fn no_stage_left(root: &Path) {
    for entry in fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name();
        assert!(
            !name.to_string_lossy().starts_with(".bulkload-restore-"),
            "stage left behind: {name:?}"
        );
    }
}

// The restored checkout is complete: status, staged and unstaged diffs, the
// untracked file and HEAD all match the source, and the source's local
// configuration is retained verbatim in the receipt.
fn assert_complete(source: &Path, destination: &Path) {
    for args in [
        vec!["status", "--porcelain"],
        vec!["diff", "--cached", "--binary"],
        vec!["diff", "--binary"],
        vec!["rev-parse", "HEAD"],
        vec!["symbolic-ref", "HEAD"],
    ] {
        assert_eq!(
            output(git(source).args(&args)).unwrap(),
            output(git(destination).args(&args)).unwrap(),
            "{args:?}"
        );
    }
    assert_eq!(fs::read(destination.join("untracked")).unwrap(), b"kept");
    assert_eq!(
        fs::read(source.join(".git/config")).unwrap(),
        fs::read(destination.join(".git/carry-config/source-config")).unwrap()
    );
    assert!(text(git(destination).args(["config", "credential.helper"])).is_err());
}

// A non-HTTPS origin restores with no `origin` remote: no URL, no fetch
// refspec, `git remote` empty. The branch's `remote = origin` is
// preserved-only too, so nothing names `origin`, which Git would otherwise
// read as a path; `merge` stays, naming no remote by itself.
fn assert_inert(origins: &[&str], name: &str) {
    let root = fresh(name);
    let (source, symbolic, bundle) = captured(&root, origins);
    let branch = symbolic.strip_prefix("refs/heads/").unwrap();
    let destination = root.join("restored");
    restore_bundle(&bundle, &destination, "neo").unwrap();
    assert_complete(&source, &destination);
    assert!(text(git(&destination).args(["config", "remote.origin.url"])).is_err());
    assert!(text(git(&destination).args(["config", "remote.origin.fetch"])).is_err());
    assert_eq!(text(git(&destination).args(["remote"])).unwrap(), "");
    assert!(text(git(&destination).args(["config", &format!("branch.{branch}.remote")])).is_err());
    assert_eq!(
        text(git(&destination).args(["config", &format!("branch.{branch}.merge")])).unwrap(),
        symbolic
    );
    let (activated, preserved, mapping) = activation(&destination);
    assert!(!activated
        .iter()
        .any(|key| key.starts_with("remote.origin.")));
    assert_eq!(
        preserved
            .iter()
            .filter(|key| *key == "remote.origin.url")
            .count(),
        origins.len()
    );
    assert!(preserved.iter().any(|key| key == "remote.origin.fetch"));
    assert!(preserved.iter().any(|key| key == "credential.helper"));
    assert!(preserved.contains(&format!("branch.{branch}.remote")));
    assert!(!activated.contains(&format!("branch.{branch}.remote")));
    assert_eq!(mapping, None);
    no_stage_left(&root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_scp_style_origin_restores_inert() {
    assert_inert(&["yoga:git/x"], "scp");
    assert_inert(&["git@github.com:o/r"], "scp-user");
}

#[test]
fn an_ssh_origin_restores_inert() {
    assert_inert(&["ssh://example.test/estate.git"], "ssh");
}

#[test]
fn an_absolute_local_path_origin_restores_inert() {
    assert_inert(&["/srv/fast-local/jess/git/xoruby-2026"], "absolute");
}

#[test]
fn a_relative_local_path_origin_restores_inert() {
    assert_inert(&["../x"], "relative");
}

// One non-HTTPS value among several leaves the whole origin inert: it is
// never partly activated.
#[test]
fn a_mixed_multi_url_origin_restores_inert() {
    assert_inert(&["https://example.test/a.git", "yoga:git/a"], "mixed");
}

#[test]
fn an_https_origin_is_still_activated() {
    let root = fresh("https");
    let origin = "https://github.com/Jesssullivan/bulkload.git";
    let (source, symbolic, bundle) = captured(&root, &[origin]);
    let branch = symbolic.strip_prefix("refs/heads/").unwrap();
    let destination = root.join("restored");
    restore_bundle(&bundle, &destination, "neo").unwrap();
    assert_complete(&source, &destination);
    assert_eq!(
        text(git(&destination).args(["config", "remote.origin.url"])).unwrap(),
        origin
    );
    assert_eq!(
        text(git(&destination).args(["config", "remote.origin.fetch"])).unwrap(),
        "+refs/heads/*:refs/remotes/origin/*"
    );
    assert_eq!(
        text(git(&destination).args(["config", &format!("branch.{branch}.merge")])).unwrap(),
        symbolic
    );
    let (activated, preserved, _) = activation(&destination);
    assert!(activated.iter().any(|key| key == "remote.origin.url"));
    assert!(activated.iter().any(|key| key == "remote.origin.fetch"));
    assert!(activated.contains(&format!("branch.{branch}.remote")));
    assert!(!preserved
        .iter()
        .any(|key| key.starts_with("remote.origin.")));
    no_stage_left(&root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_source_without_an_origin_restores_without_one() {
    let root = fresh("none");
    let (source, _, bundle) = captured(&root, &[]);
    let destination = root.join("restored");
    restore_bundle(&bundle, &destination, "neo").unwrap();
    assert_complete(&source, &destination);
    assert_eq!(text(git(&destination).args(["remote"])).unwrap(), "");
    let (activated, preserved, _) = activation(&destination);
    assert!(!activated
        .iter()
        .chain(&preserved)
        .any(|key| key.starts_with("remote.origin.")));
    no_stage_left(&root);
    fs::remove_dir_all(root).unwrap();
}

// The mapping path keeps its semantics, and its refusals come before the
// destination exists: a rerun is never GIT_DESTINATION_OCCUPIED.
#[test]
fn a_refused_mapping_leaves_no_destination_and_the_rerun_restores() {
    let root = fresh("mapping");
    let from = "/srv/fast-local/jess/git/xoruby-2026";
    let (source, _, bundle) = captured(&root, &[from]);
    let upstream = root.join("upstream");
    fs::create_dir(&upstream).unwrap();
    output(git(&upstream).args(["init", "--template="])).unwrap();
    let destination = root.join("restored");
    let refused = [
        (
            Path::new("/srv/fast-local/jess/git/other"),
            upstream.as_path(),
            BulkloadRefusal::GitAuthorityChanged,
        ),
        (
            Path::new("relative/x"),
            upstream.as_path(),
            BulkloadRefusal::PathNotAbsolute,
        ),
    ];
    for (mapped_from, to, refusal) in refused {
        assert_eq!(
            restore_bundle_configured(&bundle, &destination, "neo", Some((mapped_from, to))),
            Err(refusal.clone())
        );
        assert!(destination.symlink_metadata().is_err(), "{refusal:?}");
        no_stage_left(&root);
    }
    // A `to` that is not a repository refuses before anything is written.
    let not_a_repository = root.join("not-a-repository");
    fs::create_dir(&not_a_repository).unwrap();
    assert!(restore_bundle_configured(
        &bundle,
        &destination,
        "neo",
        Some((Path::new(from), &not_a_repository))
    )
    .is_err());
    assert!(destination.symlink_metadata().is_err());
    no_stage_left(&root);
    // A capture with no origin cannot satisfy a mapping.
    let bare_root = root.join("no-origin");
    fs::create_dir(&bare_root).unwrap();
    let (_, _, no_origin) = captured(&bare_root, &[]);
    let elsewhere = root.join("elsewhere");
    assert_eq!(
        restore_bundle_configured(
            &no_origin,
            &elsewhere,
            "neo",
            Some((Path::new(from), &upstream))
        ),
        Err(BulkloadRefusal::GitInventoryMalformed)
    );
    assert!(elsewhere.symlink_metadata().is_err());
    restore_bundle_configured(
        &bundle,
        &destination,
        "neo",
        Some((Path::new(from), &upstream)),
    )
    .unwrap();
    assert_complete(&source, &destination);
    assert_eq!(
        text(git(&destination).args(["config", "remote.origin.url"])).unwrap(),
        upstream.to_str().unwrap()
    );
    let (activated, _, mapping) = activation(&destination);
    assert!(activated.iter().any(|key| key == "remote.origin.url"));
    assert_eq!(
        mapping,
        Some((from.to_owned(), upstream.to_str().unwrap().to_owned()))
    );
    // Occupied is still refused by type, and writes nothing.
    assert_eq!(
        restore_bundle(&bundle, &destination, "neo"),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    no_stage_left(&root);
    fs::remove_dir_all(root).unwrap();
}

// The carried worktree can hold an `origin` entry: here a bare repository
// the source keeps untracked. With the origin inert, nothing names
// `origin`, so Git never reads it as a path: push has no destination, pull
// no tracking information, and the carried repository is never written.
#[test]
fn an_inert_origin_never_resolves_to_a_carried_origin_path() {
    let root = fresh("carried-origin");
    let (source, symbolic) = dirty_source(&root, &["yoga:git/x"]);
    let carried = source.join("origin");
    output(git(&source).args(["init", "-q", "--bare", "--template=", "origin"])).unwrap();
    output(git(&source).args(["push", "-q", "./origin", &format!("HEAD:{symbolic}")])).unwrap();
    let base = text(git(&carried).args(["rev-parse", &symbolic])).unwrap();
    let bundle = export_repository(&source, &root.join("capture")).unwrap();
    let destination = root.join("restored");
    restore_bundle(&bundle, &destination, "neo").unwrap();
    let restored_origin = destination.join("origin");
    assert_eq!(
        text(git(&restored_origin).args(["rev-parse", &symbolic])).unwrap(),
        base
    );
    output(git(&destination).args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", "next"]))
        .unwrap();
    assert!(output(git(&destination).args(["push"])).is_err());
    assert!(output(git(&destination).args(["pull", "--ff-only"])).is_err());
    output(git(&destination).args(["fetch"])).unwrap();
    assert_eq!(
        text(git(&restored_origin).args(["rev-parse", &symbolic])).unwrap(),
        base
    );
    assert!(text(git(&destination).args(["rev-parse", "--verify", "FETCH_HEAD"])).is_err());
    fs::remove_dir_all(root).unwrap();
}

// A refusal while the checkout is materialized (here a filesystem row whose
// kind disagrees with the carried worktree) leaves no destination and no
// stage, so the rerun with a sound capture restores instead of refusing
// GIT_DESTINATION_OCCUPIED.
#[test]
fn a_materialization_refusal_leaves_no_destination_and_the_rerun_restores() {
    let root = fresh("materialize");
    let (source, _, bundle) = captured(&root, &["yoga:git/x"]);
    let private = root.join("capture/repository.git");
    let mut rows: Vec<crate::RowSchema> = postcard::from_bytes(
        &output(git(&private).args(["show", "refs/carry-export/filesystem-v1:value"])).unwrap(),
    )
    .unwrap();
    let row = rows
        .iter_mut()
        .find(|row| row.rel_path == b"tracked")
        .unwrap();
    row.kind = bulkload_proto::FileKind::Symlink;
    output(git(&private).args(["update-ref", "-d", "refs/carry-export/filesystem-v1"])).unwrap();
    metadata(
        &private,
        "filesystem-v1",
        &postcard::to_allocvec(&rows).unwrap(),
    )
    .unwrap();
    let malformed = root.join("malformed.bundle");
    output(
        git(&private)
            .args(["bundle", "create", "-q"])
            .arg(&malformed)
            .arg("--all"),
    )
    .unwrap();
    let destination = root.join("restored");
    assert_eq!(
        restore_bundle(&malformed, &destination, "neo"),
        Err(BulkloadRefusal::GitInventoryMalformed)
    );
    assert!(destination.symlink_metadata().is_err());
    no_stage_left(&root);
    restore_bundle(&bundle, &destination, "neo").unwrap();
    assert_complete(&source, &destination);
    no_stage_left(&root);
    fs::remove_dir_all(root).unwrap();
}

// A single-component relative destination is under the current directory,
// never a parent missing.
#[test]
fn a_relative_destination_has_the_current_directory_as_its_parent() {
    assert_eq!(
        linked_destination(Path::new("restored-relative")),
        Ok(fs::canonicalize(".").unwrap().join("restored-relative"))
    );
    assert_eq!(
        linked_destination(Path::new("./restored-relative")),
        Ok(fs::canonicalize(".").unwrap().join("restored-relative"))
    );
}

// Attach standalone: a mapping that is not absolute, or whose `to` is not a
// repository, refuses before the receipt exists. A captured origin other
// than `from` refuses after the receipt is written but leaves the payload
// without `.git`; a rerun into that receipt is a typed collision, and one
// into a new receipt with the corrected mapping attaches.
#[test]
fn an_attachment_refuses_its_mapping_before_writing_the_receipt() {
    let root = fresh("attach");
    let from = "/srv/fast-local/jess/git/xoruby-2026";
    let (_, _, bundle) = captured(&root, &[from]);
    let upstream = root.join("upstream");
    fs::create_dir(&upstream).unwrap();
    output(git(&upstream).args(["init", "--template="])).unwrap();
    let payload = root.join("payload");
    restore_bundle_configured(&bundle, &payload, "neo", Some((Path::new(from), &upstream)))
        .unwrap();
    fs::rename(payload.join(".git"), root.join("original-git")).unwrap();
    let not_a_repository = root.join("not-a-repository");
    fs::create_dir(&not_a_repository).unwrap();
    let receipt = root.join("receipt");
    assert_eq!(
        attach_standalone_payload(
            &bundle,
            &payload,
            "neo",
            &receipt,
            Path::new("relative/x"),
            &upstream
        ),
        Err(BulkloadRefusal::PathNotAbsolute)
    );
    assert_eq!(
        attach_standalone_payload(
            &bundle,
            &payload,
            "neo",
            &receipt,
            Path::new(from),
            Path::new("relative/upstream")
        ),
        Err(BulkloadRefusal::PathNotAbsolute)
    );
    assert!(attach_standalone_payload(
        &bundle,
        &payload,
        "neo",
        &receipt,
        Path::new(from),
        &not_a_repository
    )
    .is_err());
    assert!(receipt.symlink_metadata().is_err());
    assert!(payload.join(".git").symlink_metadata().is_err());
    assert_eq!(
        attach_standalone_payload(
            &bundle,
            &payload,
            "neo",
            &receipt,
            Path::new("/srv/fast-local/jess/git/other"),
            &upstream
        ),
        Err(BulkloadRefusal::GitAuthorityChanged)
    );
    assert!(payload.join(".git").symlink_metadata().is_err());
    assert!(receipt.is_dir());
    assert_eq!(
        attach_standalone_payload(
            &bundle,
            &payload,
            "neo",
            &receipt,
            Path::new(from),
            &upstream
        ),
        Err(BulkloadRefusal::GitDestinationOccupied)
    );
    assert!(payload.join(".git").symlink_metadata().is_err());
    attach_standalone_payload(
        &bundle,
        &payload,
        "neo",
        &root.join("receipt-2"),
        Path::new(from),
        &upstream,
    )
    .unwrap();
    assert_eq!(
        text(git(&payload).args(["config", "remote.origin.url"])).unwrap(),
        upstream.to_str().unwrap()
    );
    fs::remove_dir_all(root).unwrap();
}

// R-N119: on a file system without an exclusive rename, the checkout is
// published into a fresh directory entry by entry, and is just as complete.
#[test]
fn a_restore_publishes_without_an_exclusive_rename() {
    let root = fresh("no-exclusive-rename");
    let (source, _, bundle) = captured(&root, &["yoga:git/x"]);
    let destination = root.join("restored");
    crate::io::force_rename_unsupported(true);
    let restored = restore_bundle(&bundle, &destination, "neo");
    crate::io::force_rename_unsupported(false);
    restored.unwrap();
    assert_complete(&source, &destination);
    no_stage_left(&root);
    fs::remove_dir_all(root).unwrap();
}
