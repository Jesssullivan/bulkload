//! #216 (OI-1003-Q129, OI-1003-Q135): a standalone restore carries a
//! non-HTTPS origin as preserved-only configuration instead of refusing, and
//! every configuration or authority refusal comes before the destination
//! exists.
//!
//! Each origin shape restores a complete standalone checkout. Only a plain
//! HTTPS origin activates; any other leaves the restored repository with no
//! `origin` remote, and the activation receipt names `remote.origin.url` (and
//! the origin's fetch refspec) as preserved-only. A refused mapping leaves no
//! destination and no stage, so the corrected rerun restores.

use std::fs;
use std::path::{Path, PathBuf};

use super::{export_repository, git, output, restore_bundle, restore_bundle_configured, text};
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
    let bundle = export_repository(&source, &root.join("capture")).unwrap();
    (source, symbolic, bundle)
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
// refspec, `git remote` empty. The branch keeps `remote = origin`, naming a
// remote that is absent until the operator adds one.
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
    assert_eq!(
        text(git(&destination).args(["config", &format!("branch.{branch}.remote")])).unwrap(),
        "origin"
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
    assert!(activated.contains(&format!("branch.{branch}.remote")));
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
