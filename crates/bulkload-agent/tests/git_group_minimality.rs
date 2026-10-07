//! P64 PACK-MINIMALITY and P65 THIN-DELTA for v1 capture bundles (Q42 lane
//! L1; OI-1003-Q42, OI-1003-Q44, OI-1003-Q45, R-N13).
//!
//! **P64, what the writer guarantees.** For every capture bundle `B` an
//! estate pass publishes, grouped (a shared plan base) or chained (WP2):
//!
//! ```text
//! packed(B) ⊆ reach(refs(B))
//! packed(B) ∩ (C(B) ∪ under(T(B))) = ∅
//! ```
//!
//! `reach` is the full object closure. `C(B)` is the commits `B`'s
//! prerequisites reach. `T(B)` is the commits of `C(B)` whose trees git's
//! `--objects-edge-aggressive` walk marks: every prerequisite, every ref of
//! `B` whose commit is in `C(B)` (HEAD's, through `--all`), and every parent
//! in `C(B)` of a commit `B` packs (an edge). `under` is every tree and blob
//! beneath those trees. So an item bundle holds no copy of a blob in its
//! base's tip trees or in its own HEAD's tree, and an edited blob deltas
//! against that copy (P65). Only commits are prerequisites, so an annotated
//! tag object, or a ref to a tree or blob, is outside `C(B)`.
//!
//! **What P64 does not bound** (OI-1003-Q42 reading (a): the law is what
//! the walk guarantees, not the full closure of the prerequisites). Each
//! row reports `packed(B) ∩ reach(prerequisites(B))`, the full-closure
//! overlap, and pins it: empty, except where a row below pins its cost.
//! - Content the prerequisites hold only deeper in history, outside every
//!   `T(B)` tree: a blob a `checkout <old> -- path`, `restore --source`,
//!   `revert` or stash brings back. It is packed again, whole unless a
//!   preferred base is similar. The `revert_to_older_content` rows pin one
//!   such blob, packed whole, as the only overlap.
//! - The capture's snapshot payload. Untracked and ignored files sit only
//!   in the staged and worktree snapshot trees, which no prerequisite holds
//!   (a prior capture's worktree commit is never one, and a shared base
//!   carries none), so they are outside `reach(prerequisites(B))` and P64
//!   allows them. An unchanged untracked file is therefore packed again,
//!   whole, on every pass that recaptures its item. The
//!   `unchanged_untracked_payload` rows pin that cost (#174 tracks
//!   deltaing or excluding it against the prior capture's worktree tree).
//!
//! **P65.** A small edit to a large tracked blob, committed or not, packs as
//! a delta against the copy its prerequisites hold, grouped and chained. The
//! pack is then thin, and every row's apply restores it byte for byte through
//! `index-pack --fix-thin` (an import's fetch, or `chain::flatten`).
//!
//! **Grouped items chain (Q42 lane L6b, fix 2).** A later capture of a
//! grouped item declares its prior capture's source-held tips beside the
//! plan base's commits, and carries both sidecars (`.prior`, `.base`); every
//! row asserts it of the moved item. P68 (`tests/git_grouped_chain.rs`)
//! holds the byte bound over twelve passes.
//!
//! **Thin reuse.** The `thin_reuse_third_pass` rows run a third pass whose
//! retained capture is a thin bundle, so its blob-reuse fetch completes the
//! pack from the source store. That pass reuses every unchanged seat (no
//! `reuse_unavailable`; `source_bytes_read` is the one edited seat), counts
//! the completed base in `read_source_capture_reuse_bytes` (R25), and, when
//! chained, restores through a flatten of two thin links.
//!
//! The rows are a fixed table (OI-1003-Q7: no fuzzing), one test per
//! (layout, mutation). Each runs the verb binary end to end: a first pass, the
//! mutation, a later pass, the law over every item bundle of every pass,
//! then `estate-apply` and a byte comparison of every restored workspace.
//! Each row prints one `P64 ...` line per bundle with its bytes and object
//! counts (`--nocapture`), the lane note's measurements.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod git_group;

use git_group::{
    apply_and_compare, capture, edit_notes, feed, fixture, git, inspect, law, of_item,
    overwrite_large, run, settle, side_commit, Fixture, Layout, Mutation, Pass, LARGE, PAYLOAD,
    ROUND,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What a row expects of the bundles one later pass published.
struct Expect<'a> {
    name: &'a str,
    pass: u32,
    /// The moved checkout's item.
    item: &'a str,
    /// The edited large blob (P65), if the row edits it.
    edited: Option<&'a str>,
    /// The one object the moved item's bundle may share with the full
    /// closure of its prerequisites (P64's pinned leftover).
    leftover: Option<&'a str>,
    /// An unchanged payload blob the moved item's bundle packs whole again.
    payload: Option<&'a str>,
}

/// The law and every row pin over one later pass. Returns the moved item's
/// bundle from it, if that item was recaptured.
fn later_pass(
    fixture: &Fixture,
    fresh: &BTreeSet<PathBuf>,
    expect: &Expect<'_>,
) -> Option<PathBuf> {
    let (name, pass) = (expect.name, expect.pass);
    assert!(!fresh.is_empty(), "{name}: pass {pass} recaptured nothing");
    let mut deltas = 0;
    let mut thin = 0;
    let mut moved = Vec::new();
    for bundle in fresh {
        let (seen, overlap) = law(fixture, name, pass, bundle);
        let mine = of_item(bundle, expect.item);
        if mine {
            moved.push(bundle.clone());
        }
        assert!(
            !seen.packed.contains_key(&fixture.large),
            "{name}: a pass {pass} bundle re-packed the large blob"
        );
        match expect.leftover.filter(|_| mine) {
            Some(value) => {
                // Pinned: one blob the prerequisites hold only in an older
                // commit's tree, packed again whole.
                assert_eq!(
                    overlap,
                    BTreeSet::from([value.to_owned()]),
                    "P64 {name} pass {pass}: the full-closure overlap"
                );
                let packed = &seen.packed[value];
                assert!(
                    !packed.delta && packed.in_pack >= ROUND as u64,
                    "P64 {name}: the reverted blob's cost moved: {packed:?}"
                );
            }
            None => assert!(
                overlap.is_empty(),
                "P64 {name} pass {pass}: full-closure overlap {overlap:?}"
            ),
        }
        if let Some(value) = expect.payload.filter(|_| mine) {
            let packed = &seen.packed[value];
            assert!(
                !packed.delta && packed.in_pack >= PAYLOAD as u64,
                "P64 {name}: the unchanged payload's cost moved: {packed:?}"
            );
        }
        // P65: the edited large blob, wherever packed, is a small delta.
        if let Some(packed) = expect.edited.and_then(|value| seen.packed.get(value)) {
            assert!(
                packed.delta && packed.in_pack < (LARGE / 64) as u64,
                "P65 {name}: the edited large blob packed whole: {packed:?}"
            );
            deltas += 1;
            thin += usize::from(seen.appended > 0);
        }
    }
    if expect.edited.is_some() {
        assert!(deltas > 0, "P65 {name}: no bundle packed the edited blob");
        // The delta's base is the prerequisites' copy, so the pack is thin
        // and the apply below restores through index-pack --fix-thin.
        assert_eq!(thin, deltas, "P65 {name}: a delta was not against the base");
    }
    assert!(moved.len() <= 1, "{name}: pass {pass}: {moved:?}");
    // Re-pinned for L6b (fix 2): a grouped item's later capture chains too.
    if let Some(bundle) = moved.first() {
        assert_chained(fixture, bundle);
    }
    moved.pop()
}

fn blob_at(fixture: &Fixture, revision: &str) -> String {
    run(git(&fixture.source).args(["rev-parse", revision]))
}

fn hashed(checkout: &Path, path: &str) -> String {
    feed(
        git(checkout).args(["hash-object", "--stdin"]),
        &std::fs::read(checkout.join(path)).unwrap(),
    )
}

fn row(layout: Layout, mutation: Mutation) {
    let name = format!("{layout:?}-{mutation:?}");
    let fixture = fixture(&name, layout, mutation);
    let moved = fixture.moved.as_path();
    let first = capture(&fixture);
    assert_eq!(
        first.bundles.len(),
        fixture.items.len(),
        "{:?}",
        first.bundles
    );
    let item = first.receipt(moved).item.clone();
    let payload = (mutation == Mutation::UntrackedPayload).then(|| hashed(moved, "payload.bin"));
    for bundle in &first.bundles {
        let (seen, overlap) = law(&fixture, &name, 1, bundle);
        assert!(
            overlap.is_empty(),
            "P64 {name} pass 1: full-closure overlap {overlap:?}"
        );
        // Item bundles of a group hold no copy of the base's blobs; a
        // chained first pass is self-contained and carries it once.
        if layout == Layout::Grouped {
            assert!(
                !seen.packed.contains_key(&fixture.large),
                "{name}: an item bundle re-packed the base's large blob"
            );
        }
        if let Some(value) = payload.as_deref().filter(|_| of_item(bundle, &item)) {
            let packed = &seen.packed[value];
            assert!(
                !packed.delta && packed.in_pack >= PAYLOAD as u64,
                "P64 {name}: the untracked payload's first cost: {packed:?}"
            );
        }
    }
    if mutation == Mutation::FirstPass {
        apply_and_compare(&fixture);
        return;
    }
    let mut leftover = None;
    match mutation {
        Mutation::FirstPass => unreachable!(),
        Mutation::HeadMove => {
            run(git(moved).args(["checkout", "-q", "--detach", &fixture.history[1]]));
        }
        Mutation::SideCommit => {
            let tip = run(git(&fixture.source).args(["rev-parse", "refs/heads/side"]));
            side_commit(&fixture.source, &tip, "side2.txt", b"side 1\n");
        }
        Mutation::DirRename => {
            run(git(&fixture.source).args(["mv", "tree/a", "tree/moved"]));
            run(git(&fixture.source).args(["commit", "-q", "-m", "rename"]));
        }
        Mutation::LargeCommitted => {
            overwrite_large(&fixture.source);
            run(git(&fixture.source).args(["commit", "-q", "-am", "large edit"]));
        }
        Mutation::LargeWorktree => overwrite_large(moved),
        Mutation::RevertOlder => {
            // c1's copy: in no prerequisite's tip tree, no edge tree and not
            // HEAD's (c2 grouped, c3 chained), yet reachable from them all.
            run(git(moved).args(["checkout", &fixture.history[1], "--", "rounds.bin"]));
            leftover = Some(blob_at(
                &fixture,
                &format!("{}:rounds.bin", fixture.history[1]),
            ));
        }
        Mutation::UntrackedPayload => edit_notes(moved),
        Mutation::ThinReuse => {
            overwrite_large(moved);
            // The third pass reuses this pass's capture: no seat may be racy
            // against its start.
            settle();
        }
    }
    let second = capture(&fixture);
    let edited = match mutation {
        Mutation::LargeCommitted => Some(fixture.source.as_path()),
        Mutation::LargeWorktree | Mutation::ThinReuse => Some(moved),
        _ => None,
    }
    .map(|checkout| hashed(checkout, "big.bin"));
    let mut expect = Expect {
        name: &name,
        pass: 2,
        item: &item,
        edited: edited.as_deref(),
        leftover: leftover.as_deref(),
        payload: payload.as_deref(),
    };
    let link = later_pass(&fixture, &second.fresh(&first), &expect);
    // A mutation in the moved checkout recaptures its item, so every pin on
    // that item's bundle ran.
    let touched = matches!(
        mutation,
        Mutation::HeadMove
            | Mutation::LargeWorktree
            | Mutation::RevertOlder
            | Mutation::UntrackedPayload
            | Mutation::ThinReuse
    );
    assert!(
        !touched || link.is_some(),
        "{name}: the moved item was not recaptured"
    );
    if let Some(link) = link.filter(|_| mutation == Mutation::ThinReuse) {
        thin_reuse(&fixture, &second, &link, &mut expect);
    }
    apply_and_compare(&fixture);
}

// A later capture's bundle names its prior capture (`.prior`), and in a
// group the plan base too (`.base`). Re-pinned for Q42 lane L6b (fix 2,
// OI-1003-Q63): a later capture of a grouped item is no longer a delta on
// the plan base alone. Like a chained item's, it chains on its prior
// capture, and it stays bound to the base.
fn assert_chained(fixture: &Fixture, bundle: &Path) {
    let sidecar = |extension: &str| {
        let mut path = bundle.as_os_str().to_owned();
        path.push(extension);
        PathBuf::from(path)
    };
    assert!(
        sidecar(".prior").exists(),
        "{}: not chained",
        bundle.display()
    );
    assert_eq!(
        sidecar(".base").exists(),
        fixture.items.len() > 1,
        "{}: bound to a plan base exactly in a group",
        bundle.display()
    );
}

// The third pass of a thin-reuse row: its retained capture, `link`, is thin.
fn thin_reuse(fixture: &Fixture, second: &Pass, link: &Path, expect: &mut Expect<'_>) {
    let name = expect.name;
    let moved = fixture.moved.as_path();
    edit_notes(moved);
    let third = capture(fixture);
    let receipt = third.receipt(moved);
    // Every unchanged seat, the thin-delta'd large blob among them, comes
    // from the retained capture: only the edited file is read (R25).
    assert_eq!(receipt.reuse_unavailable, None, "{name}: {receipt:?}");
    assert_eq!(
        receipt.source_bytes_read,
        std::fs::metadata(moved.join("notes.txt")).unwrap().len(),
        "{name}: {receipt:?}"
    );
    // The reuse fetch completed `link` from the source store: the base it
    // appended (the whole large blob) is counted beside the bundle.
    let reuse_read = third.counters["read_source_capture_reuse_bytes"];
    eprintln!(
        "P64 row={name} pass=3 reuse_read={reuse_read} retained={} readback={} \
         source_bytes_read={}",
        std::fs::metadata(link).unwrap().len(),
        third.counters["read_source_pack_readback_bytes"],
        receipt.source_bytes_read,
    );
    assert!(
        reuse_read >= std::fs::metadata(link).unwrap().len() + LARGE as u64,
        "{name}: the reuse fetch's completed base went uncounted: {reuse_read}"
    );
    expect.pass = 3;
    let head = later_pass(fixture, &third.fresh(second), expect)
        .unwrap_or_else(|| panic!("{name}: pass 3 did not recapture the moved item"));
    // Both links after the first are thin, and the head names its
    // predecessor: a chained apply flattens two thin links.
    for bundle in [link, head.as_path()] {
        let seen = inspect(fixture, bundle);
        assert!(
            seen.appended > 0,
            "{name}: {} is not thin",
            bundle.display()
        );
        assert!(!seen.prerequisites.is_empty(), "{}", bundle.display());
        assert_chained(fixture, bundle);
    }
}

#[test]
fn p64_grouped_first_pass() {
    row(Layout::Grouped, Mutation::FirstPass);
}

#[test]
fn p64_grouped_head_move() {
    row(Layout::Grouped, Mutation::HeadMove);
}

#[test]
fn p64_grouped_side_commit() {
    row(Layout::Grouped, Mutation::SideCommit);
}

#[test]
fn p64_grouped_directory_rename() {
    row(Layout::Grouped, Mutation::DirRename);
}

#[test]
fn p64_p65_grouped_committed_large_edit() {
    row(Layout::Grouped, Mutation::LargeCommitted);
}

#[test]
fn p64_p65_grouped_worktree_large_edit() {
    row(Layout::Grouped, Mutation::LargeWorktree);
}

#[test]
fn p64_grouped_revert_to_older_content() {
    row(Layout::Grouped, Mutation::RevertOlder);
}

#[test]
fn p64_grouped_unchanged_untracked_payload() {
    row(Layout::Grouped, Mutation::UntrackedPayload);
}

#[test]
fn p64_p65_grouped_thin_reuse_third_pass() {
    row(Layout::Grouped, Mutation::ThinReuse);
}

#[test]
fn p64_chained_first_pass() {
    row(Layout::Chained, Mutation::FirstPass);
}

#[test]
fn p64_chained_head_move() {
    row(Layout::Chained, Mutation::HeadMove);
}

#[test]
fn p64_chained_side_commit() {
    row(Layout::Chained, Mutation::SideCommit);
}

#[test]
fn p64_chained_directory_rename() {
    row(Layout::Chained, Mutation::DirRename);
}

#[test]
fn p64_p65_chained_committed_large_edit() {
    row(Layout::Chained, Mutation::LargeCommitted);
}

#[test]
fn p64_p65_chained_worktree_large_edit() {
    row(Layout::Chained, Mutation::LargeWorktree);
}

#[test]
fn p64_chained_revert_to_older_content() {
    row(Layout::Chained, Mutation::RevertOlder);
}

#[test]
fn p64_chained_unchanged_untracked_payload() {
    row(Layout::Chained, Mutation::UntrackedPayload);
}

#[test]
fn p64_p65_chained_thin_reuse_third_pass() {
    row(Layout::Chained, Mutation::ThinReuse);
}
