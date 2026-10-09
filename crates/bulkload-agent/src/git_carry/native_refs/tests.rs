//! P84 REF-DECISION: [`decide`] equals the design's table 7.2 (as amended),
//! row by row and exhaustively over its finite domain, and for every
//! permutation of item order inside one apply a ref bulkload landed before
//! ends at the newest capture's value (`NoStaleRegress`). Plus the stash
//! table (7.3, amended by finding 4) and the plan's namespace filters.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines
)]

use super::*;
use crate::test_support::prop_config;
use proptest::prelude::*;

fn oid(byte: char) -> String {
    byte.to_string().repeat(40)
}

fn live(value: &str, t: i128) -> LandedRef {
    LandedRef {
        oid: Some(value.to_owned()),
        observed_ns: t,
    }
}

fn tomb(t: i128) -> LandedRef {
    LandedRef {
        oid: None,
        observed_ns: t,
    }
}

fn input<'a>(
    native: Option<&'a str>,
    row: Option<&'a LandedRef>,
    carried: Option<&'a str>,
    t: i128,
    class: Class,
) -> Input<'a> {
    Input {
        native,
        row,
        carried,
        t,
        class,
        checked_out: false,
    }
}

/// The table's named rows, each written out by hand.
#[test]
fn p84_the_named_rows() {
    let (x, y, v) = (oid('a'), oid('b'), oid('c'));
    let (x, y, v) = (x.as_str(), y.as_str(), v.as_str());
    let row_x = live(x, 10);
    let row_y = live(y, 10);
    let local = Class::Local;
    // R1: create.
    assert_eq!(
        decide(input(None, None, Some(v), 20, local)),
        Decision {
            write: Write::Create(v.into()),
            row: Row::Set(live(v, 20)),
            line: Line::Created
        }
    );
    // R2: adopt, no write.
    assert_eq!(
        decide(input(Some(x), None, Some(x), 20, local)),
        Decision {
            write: Write::None,
            row: Row::Set(live(x, 20)),
            line: Line::Adopted
        }
    );
    // R3: the destination's own value is kept.
    assert_eq!(
        decide(input(Some(x), None, Some(v), 20, local)).line,
        Line::KeptDestination
    );
    // R4: verify; the row's instant moves forward only.
    assert_eq!(
        decide(input(Some(x), Some(&row_x), Some(x), 20, local)),
        Decision {
            write: Write::None,
            row: Row::Set(live(x, 20)),
            line: Line::None
        }
    );
    assert_eq!(
        decide(input(Some(x), Some(&row_x), Some(x), 5, local)).row,
        Row::Keep
    );
    // R5: owned and newer: compare-and-swap.
    assert_eq!(
        decide(input(Some(x), Some(&row_x), Some(v), 20, local)),
        Decision {
            write: Write::Update {
                old: x.into(),
                new: v.into()
            },
            row: Row::Set(live(v, 20)),
            line: Line::Moved
        }
    );
    // R6: an older capture never regresses an owned ref.
    for carried in [Some(v), None] {
        for t in [5, 10] {
            assert_eq!(
                decide(input(Some(x), Some(&row_x), carried, t, local)),
                keep(Line::Stale)
            );
        }
    }
    // R7: deleted at the source.
    assert_eq!(
        decide(input(
            Some(x),
            Some(&row_x),
            None,
            20,
            Class::RemoteTracking
        )),
        Decision {
            write: Write::Delete(x.into()),
            row: Row::Set(tomb(20)),
            line: Line::Pruned
        }
    );
    for class in [Class::Local, Class::Tag, Class::Other] {
        assert_eq!(
            decide(input(Some(x), Some(&row_x), None, 20, class)),
            keep(Line::DeletedAtSource)
        );
    }
    // R8: moved at the destination: kept, whatever the capture says.
    for carried in [Some(v), Some(x), None] {
        assert_eq!(
            decide(input(Some(x), Some(&row_y), carried, 20, local)),
            keep(Line::ModifiedAtDestination)
        );
    }
    // R9: deleted at the destination: kept absent.
    assert_eq!(
        decide(input(None, Some(&row_x), Some(v), 20, local)),
        keep(Line::DeletedAtDestination)
    );
    // R10: not bulkload's, not carried.
    assert_eq!(
        decide(input(Some(x), None, None, 20, local)),
        keep(Line::None)
    );
    // A tombstone: an older capture never resurrects (finding 8); a newer
    // one creates.
    let gone = tomb(10);
    assert_eq!(
        decide(input(None, Some(&gone), Some(v), 10, Class::RemoteTracking)),
        keep(Line::Stale)
    );
    assert_eq!(
        decide(input(None, Some(&gone), Some(v), 20, Class::RemoteTracking)).write,
        Write::Create(v.into())
    );
    // The guard (finding 1): a worktree's HEAD branch is never moved here.
    let mut guarded = input(Some(x), Some(&row_x), Some(v), 20, local);
    guarded.checked_out = true;
    assert_eq!(decide(guarded), keep(Line::CheckedOutElsewhere));
}

// The table, restated as the design's prose reads it, for the exhaustive
// comparison.
fn table(input: Input<'_>) -> Decision {
    if input.checked_out {
        return keep(Line::CheckedOutElsewhere);
    }
    let tombstone = input.row.filter(|row| row.oid.is_none());
    if tombstone.is_some_and(|row| row.observed_ns >= input.t) && input.carried.is_some() {
        return keep(Line::Stale);
    }
    if input.native.is_none()
        && input.carried.is_none()
        && tombstone.is_some_and(|row| row.observed_ns < input.t)
    {
        return set(Write::None, None, input.t, Line::None);
    }
    let owned = input.row.filter(|row| row.oid.is_some());
    let Some(native) = input.native else {
        return match (owned, input.carried) {
            (Some(_), _) => keep(Line::DeletedAtDestination),
            (None, None) => keep(Line::None),
            (None, Some(v)) => set(Write::Create(v.to_owned()), Some(v), input.t, Line::Created),
        };
    };
    let Some(row) = owned else {
        return match input.carried {
            None => keep(Line::None),
            Some(v) if v == native => set(Write::None, Some(native), input.t, Line::Adopted),
            Some(_) => keep(Line::KeptDestination),
        };
    };
    if row.oid.as_deref() != Some(native) {
        return keep(Line::ModifiedAtDestination);
    }
    if input.carried == Some(native) {
        return if input.t > row.observed_ns {
            set(Write::None, Some(native), input.t, Line::None)
        } else {
            keep(Line::None)
        };
    }
    if input.t <= row.observed_ns {
        return keep(Line::Stale);
    }
    match input.carried {
        Some(v) => set(
            Write::Update {
                old: native.to_owned(),
                new: v.to_owned(),
            },
            Some(v),
            input.t,
            Line::Moved,
        ),
        None if input.class == Class::RemoteTracking => set(
            Write::Delete(native.to_owned()),
            None,
            input.t,
            Line::Pruned,
        ),
        None => keep(Line::DeletedAtSource),
    }
}

#[test]
fn p84_decide_is_the_table_over_its_whole_domain() {
    let (x, y, v) = (oid('a'), oid('b'), oid('c'));
    let values = [None, Some(x.as_str()), Some(y.as_str())];
    let rows = [None, Some(live(&x, 10)), Some(live(&y, 10)), Some(tomb(10))];
    let carried = [None, Some(x.as_str()), Some(v.as_str())];
    let mut checked = 0;
    for native in values {
        for row in &rows {
            for carried in carried {
                for t in [9, 10, 11] {
                    for class in [
                        Class::Local,
                        Class::Tag,
                        Class::RemoteTracking,
                        Class::Other,
                    ] {
                        for checked_out in [false, true] {
                            let input = Input {
                                native,
                                row: row.as_ref(),
                                carried,
                                t,
                                class,
                                checked_out,
                            };
                            assert_eq!(decide(input), table(input), "{input:?}");
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 3 * 4 * 3 * 3 * 4 * 2);
}

const NAMES: [&str; 4] = [
    "refs/heads/b",
    "refs/tags/t",
    "refs/remotes/o/r",
    "refs/notes/n",
];

// Apply one item's plan to a simulated destination.
fn apply(
    native: &mut BTreeMap<String, String>,
    ledger: &mut RefLedger,
    carried: &BTreeMap<String, String>,
    t: i128,
) {
    let planned = plan(native, ledger, carried, t, &BTreeSet::new());
    for op in &planned.ops {
        assert_eq!(
            native.get(&op.name),
            op.old.as_ref(),
            "a compare-and-swap that fails"
        );
        match &op.new {
            Some(new) => native.insert(op.name.clone(), new.clone()),
            None => native.remove(&op.name),
        };
    }
    *ledger = planned.ledger;
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for rest in permutations(n - 1) {
        for at in 0..=rest.len() {
            let mut next = rest.clone();
            next.insert(at, n - 1);
            out.push(next);
        }
    }
    out
}

proptest! {
    #![proptest_config(prop_config(48))]

    /// P84 (NoStaleRegress): items of one apply, each a capture at its own
    /// instant, applied in every order, leave each ref bulkload landed
    /// before at the newest capture's value: for a remote-tracking ref the
    /// newest capture's (absent when it lacks it, R7); for every other kind
    /// the newest capture that carries it (R7 keeps the rest).
    #[test]
    fn p84_every_item_order_ends_at_the_newest_capture(
        items in proptest::collection::vec(
            proptest::collection::vec(proptest::option::of(0u8..4), 4),
            1..5,
        ),
    ) {
        let landed: BTreeMap<String, String> =
            NAMES.iter().map(|name| ((*name).to_owned(), oid('0'))).collect();
        let captures: Vec<(i128, BTreeMap<String, String>)> = items
            .iter()
            .enumerate()
            .map(|(index, values)| {
                let carried = NAMES
                    .iter()
                    .zip(values)
                    .filter_map(|(name, value)| {
                        value.map(|value| ((*name).to_owned(), oid(char::from(b'a' + value))))
                    })
                    .collect();
                (i128::try_from(index).unwrap() + 1, carried)
            })
            .collect();
        let mut expected = BTreeMap::new();
        for name in NAMES {
            let newest = captures.iter().max_by_key(|(t, _)| *t).unwrap();
            let value = if Class::of(name) == Class::RemoteTracking {
                newest.1.get(name).cloned()
            } else {
                captures
                    .iter()
                    .filter(|(_, carried)| carried.contains_key(name))
                    .max_by_key(|(t, _)| *t)
                    .map_or_else(|| Some(oid('0')), |(_, carried)| carried.get(name).cloned())
            };
            if let Some(value) = value {
                expected.insert(name.to_owned(), value);
            }
        }
        for order in permutations(captures.len()) {
            let mut native = BTreeMap::new();
            let mut ledger = RefLedger { created: true, ..RefLedger::default() };
            apply(&mut native, &mut ledger, &landed, 0);
            for index in &order {
                let (t, carried) = &captures[*index];
                apply(&mut native, &mut ledger, carried, *t);
            }
            prop_assert_eq!(&native, &expected, "order {:?}", order);
        }
    }
}

#[test]
fn the_plan_leaves_bulkload_and_per_worktree_namespaces_and_symrefs_alone() {
    for name in [
        "refs/carry/v1/neo/x/refs/heads/main",
        "refs/carry-export/head",
        "refs/stash",
        "refs/bisect/bad",
        "refs/worktree/x",
        "refs/rewritten/x",
        "refs/remotes/origin/HEAD",
        "HEAD",
    ] {
        assert!(!restorable(name), "{name}");
    }
    for name in [
        "refs/heads/main",
        "refs/tags/v1",
        "refs/remotes/origin/main",
        "refs/notes/commits",
        "refs/replace/x",
        "refs/custom/x",
    ] {
        assert!(restorable(name), "{name}");
    }
    let carried: BTreeMap<String, String> = [
        ("refs/heads/main", 'a'),
        ("refs/heads/feat", 'b'),
        ("refs/stash", 'c'),
    ]
    .into_iter()
    .map(|(name, byte)| (name.to_owned(), oid(byte)))
    .collect();
    let checked: BTreeSet<String> = ["refs/heads/feat".to_owned()].into();
    let planned = plan(
        &BTreeMap::new(),
        &RefLedger::default(),
        &carried,
        1,
        &checked,
    );
    let names: Vec<&str> = planned.ops.iter().map(|op| op.name.as_str()).collect();
    assert_eq!(names, vec!["refs/heads/main"]);
    assert_eq!(planned.custody.0.get("ref-checked-out-elsewhere"), Some(&1));
}

/// P84, review 2 finding 3: a create whose name sits above or below a ref
/// that stays is kept back (`ref-name-conflict`, its row untouched); a
/// pruned remote-tracking ref frees its name for its successor, deleted in
/// a transaction of its own first.
#[test]
fn p84_a_create_above_or_below_a_ref_that_stays_is_kept_back() {
    assert!(name_conflict("refs/heads/foo/bar", ["refs/heads/foo"]));
    assert!(name_conflict("refs/heads/foo", ["refs/heads/foo/bar"]));
    assert!(!name_conflict("refs/heads/foo", ["refs/heads/foobar"]));
    assert!(!name_conflict("refs/heads/foo-bar", ["refs/heads/foo"]));
    assert!(!name_conflict("refs/heads/foo", ["refs/heads/foo"]));
    let (x, v) = (oid('a'), oid('c'));
    let names = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    };
    let native = names(&[
        ("refs/heads/foo", &x),
        ("refs/heads/up/down", &x),
        ("refs/remotes/o/release", &x),
    ]);
    let ledger = RefLedger {
        created: true,
        refs: native
            .keys()
            .map(|name| (name.clone(), live(&x, 1)))
            .collect(),
        ..RefLedger::default()
    };
    let carried = names(&[
        ("refs/heads/foo/bar", &v),
        ("refs/heads/up", &v),
        ("refs/remotes/o/release/1.0", &v),
    ]);
    let planned = plan(&native, &ledger, &carried, 2, &BTreeSet::new());
    let ops: Vec<(&str, Option<&str>, Option<&str>)> = planned
        .ops
        .iter()
        .map(|op| (op.name.as_str(), op.old.as_deref(), op.new.as_deref()))
        .collect();
    assert_eq!(
        ops,
        vec![
            ("refs/remotes/o/release", Some(x.as_str()), None),
            ("refs/remotes/o/release/1.0", None, Some(v.as_str())),
        ]
    );
    assert_eq!(planned.custody.0.get("ref-name-conflict"), Some(&2));
    assert_eq!(planned.custody.0.get("ref-deleted-at-source"), Some(&2));
    assert!(!planned.ledger.refs.contains_key("refs/heads/foo/bar"));
    assert!(!planned.ledger.refs.contains_key("refs/heads/up"));
    assert_eq!(
        planned.ledger.refs.get("refs/heads/foo"),
        Some(&live(&x, 1))
    );
}

#[test]
fn the_stash_table() {
    let (a, b, c) = (oid('a'), oid('b'), oid('c'));
    let carried = |values: &[&String]| -> Vec<(String, String)> {
        values
            .iter()
            .map(|value| ((*value).clone(), format!("On main: {value}")))
            .collect()
    };
    let landed = |order: &[&String], t| LandedStash {
        order: order.iter().map(|value| (*value).clone()).collect(),
        observed_ns: t,
    };
    // T1: restored into an empty destination.
    let (op, word) = stash_decision(&[], None, &carried(&[&b, &a]), 5);
    assert_eq!(word, Some("stash-restored"));
    assert_eq!(op.unwrap().new, vec![b.clone(), a.clone()]);
    // T2: the destination's own stash is kept.
    assert_eq!(
        stash_decision(std::slice::from_ref(&c), None, &carried(&[&a]), 5),
        (None, Some("stash-kept-destination"))
    );
    let s0 = landed(&[&a], 5);
    // T3: nothing moved.
    assert_eq!(
        stash_decision(std::slice::from_ref(&a), Some(&s0), &carried(&[&a]), 9),
        (None, None)
    );
    // T4: pushed at the source.
    let (op, word) = stash_decision(std::slice::from_ref(&a), Some(&s0), &carried(&[&b, &a]), 9);
    assert_eq!(word, Some("stash-appended"));
    assert_eq!(op.unwrap().old, vec![a.clone()]);
    // T5: dropped or rewritten at the source.
    assert_eq!(
        stash_decision(std::slice::from_ref(&a), Some(&s0), &carried(&[&b]), 9).1,
        Some("stash-rewritten")
    );
    // T6: changed at the destination: kept.
    assert_eq!(
        stash_decision(&[c, a.clone()], Some(&s0), &carried(&[&b, &a]), 9),
        (None, Some("stash-modified-at-destination"))
    );
    // Finding 4: an older capture never regresses a newer stash.
    let newer = landed(&[&b, &a], 9);
    assert_eq!(
        stash_decision(&[b, a.clone()], Some(&newer), &carried(&[&a]), 5),
        (None, Some("stash-stale"))
    );
}

// ---- the ref step against a real repository (the convergence lane's fixes,
// 2026-10-09; EstateConverge.tla's mutations named per test) -----------------

/// A repository with commits `A` and `B` on `main`, `main` at `B`, and a
/// branch `topic` at `A`; the ledger directory of SOURCE `neo`.
struct Repo {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    ledgers: std::path::PathBuf,
    a: String,
    b: String,
}

fn run(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(dir.path()).unwrap().join("repo");
        std::fs::create_dir(&path).unwrap();
        run(&path, &["init", "--quiet", "--template=", "-b", "main"]);
        std::fs::write(path.join("f"), b"a\n").unwrap();
        run(&path, &["add", "f"]);
        run(&path, &["commit", "--quiet", "-m", "A"]);
        let a = run(&path, &["rev-parse", "HEAD"]);
        run(&path, &["branch", "topic"]);
        std::fs::write(path.join("f"), b"b\n").unwrap();
        run(&path, &["commit", "--quiet", "-am", "B"]);
        let b = run(&path, &["rev-parse", "HEAD"]);
        let ledgers = super::super::landed::ensure_dir(&path.join(".git"), "neo").unwrap();
        Self {
            _dir: dir,
            path,
            ledgers,
            a,
            b,
        }
    }

    fn value(&self, name: &str) -> Option<String> {
        let out = std::process::Command::new("git")
            .args(["-C"])
            .arg(&self.path)
            .args(["rev-parse", "-q", "--verify", name])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
    }

    /// A pending ref intent of `ops` (and `stash`), as a crash leaves it.
    fn intent(&self, ops: Vec<RefOp>, stash: Option<StashOp>, stash_at: Option<Vec<String>>) {
        let ledger = RefLedger {
            created: true,
            ..RefLedger::default()
        };
        super::super::landed::write(
            &self.ledgers.join(super::super::landed::REFS_INTENT),
            &RefIntent {
                ops,
                stash,
                ledger,
                stash_at,
            },
        )
        .unwrap();
    }

    fn pending(&self) -> bool {
        self.ledgers
            .join(super::super::landed::REFS_INTENT)
            .exists()
    }
}

fn op(name: &str, old: Option<&str>, new: Option<&str>) -> RefOp {
    RefOp {
        name: name.to_owned(),
        old: old.map(str::to_owned),
        new: new.map(str::to_owned),
        before: None,
        after: new.map(|value| live(value, 2)),
    }
}

/// The blocker (review 3 finding 10; mutation `settle_moves_checked_out`):
/// a ref intent pending from a crash moves `topic`; the operator then
/// checks `topic` out. The settle must not move it under the operator's
/// HEAD: its row is restored and it is reported checked out elsewhere.
/// Before the fix `finish` redid the op and moved the branch.
#[test]
fn a_settle_never_moves_a_branch_checked_out_since_the_crash() {
    let repo = Repo::new();
    repo.intent(
        vec![op("refs/heads/topic", Some(&repo.a), Some(&repo.b))],
        None,
        None,
    );
    run(&repo.path, &["switch", "--quiet", "topic"]);
    let custody = settle(&repo.path, &repo.ledgers).unwrap();
    assert_eq!(repo.value("refs/heads/topic"), Some(repo.a.clone()));
    assert_eq!(run(&repo.path, &["rev-parse", "HEAD"]), repo.a);
    assert_eq!(custody.0.get("ref-checked-out-elsewhere"), Some(&1));
    assert!(!repo.pending());
}

/// Review 3 findings 2 and 10 (mutations `txn_no_symref_verify`,
/// `rebase_unguarded`): a transaction refuses an op on a branch some
/// worktree has checked out, or holds for a rebase or bisect, read under
/// every worktree's HEAD lock; and a HEAD another writer has locked (a
/// `git switch` in progress) refuses it too. Before the fix the
/// transaction checked nothing and moved the branch.
#[test]
fn a_transaction_never_moves_a_checked_out_or_held_branch() {
    let repo = Repo::new();
    let ops = [op("refs/heads/topic", Some(&repo.a), Some(&repo.b))];
    // Checked out by a linked worktree.
    let linked = repo.path.with_file_name("linked");
    run(
        &repo.path,
        &[
            "worktree",
            "add",
            "--quiet",
            linked.to_str().unwrap(),
            "topic",
        ],
    );
    assert_eq!(
        transact(&repo.path, &ops).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    assert_eq!(repo.value("refs/heads/topic"), Some(repo.a.clone()));
    // Held by a bisect started on it (its HEAD is still on it here), then
    // by a bisect alone once the worktree is detached.
    run(&linked, &["bisect", "start"]);
    run(&linked, &["switch", "--quiet", "--detach"]);
    assert_eq!(
        transact(&repo.path, &ops).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    run(&linked, &["bisect", "reset", "HEAD"]);
    // Free now, but another writer holds the linked worktree's HEAD.
    let lock = repo.path.join(".git/worktrees/linked/HEAD.lock");
    std::fs::write(&lock, b"").unwrap();
    assert_eq!(
        transact(&repo.path, &ops).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    assert_eq!(repo.value("refs/heads/topic"), Some(repo.a.clone()));
    assert!(lock.exists(), "another writer's lock is never removed");
    std::fs::remove_file(&lock).unwrap();
    transact(&repo.path, &ops).unwrap();
    assert_eq!(repo.value("refs/heads/topic"), Some(repo.b.clone()));
    // Every HEAD lock this process took is gone.
    assert!(!repo.path.join(".git/HEAD.lock").exists());
    assert!(!lock.exists());
}

/// Review 3 finding 1 (mutation `finish_after_all_deletes`): o/x -> o/x/y
/// with the operator moving o/x while the ref intent is pending. The
/// delete of o/x will not run, so the create of o/x/y beside it is dropped
/// (`ref-name-conflict`), not retried forever. Before the fix `finish`
/// counted every delete as run and refused on every rerun.
#[test]
fn a_create_beside_a_delete_that_will_not_run_is_dropped() {
    let repo = Repo::new();
    run(&repo.path, &["update-ref", "refs/remotes/o/x", &repo.a]);
    repo.intent(
        vec![
            op("refs/remotes/o/x", Some(&repo.a), None),
            op("refs/remotes/o/x/y", None, Some(&repo.a)),
        ],
        None,
        None,
    );
    run(&repo.path, &["update-ref", "refs/remotes/o/x", &repo.b]);
    let custody = settle(&repo.path, &repo.ledgers).unwrap();
    assert_eq!(repo.value("refs/remotes/o/x"), Some(repo.b.clone()));
    assert_eq!(repo.value("refs/remotes/o/x/y"), None);
    assert_eq!(custody.0.get("ref-name-conflict"), Some(&1));
    assert_eq!(custody.0.get("ref-modified-at-destination"), Some(&1));
    assert!(!repo.pending());
}

/// The earlier report's finding 3 and review 3 finding 8 (mutation
/// `stale_lock_kept`): a Git child killed inside its transaction leaves
/// `<ref>.lock` (holding the new value) or `refs/stash.lock`; the settle
/// removes a lock whose content proves it bulkload's and finishes the
/// intent. A lock that is not provably bulkload's stays, and the settle
/// refuses typed. Before the fix nothing removed the lock: every settle
/// refused, and a killed stash store cleared the stash row.
#[test]
fn a_settle_removes_a_killed_childs_lock_and_keeps_anyone_elses() {
    let repo = Repo::new();
    let lock = repo.path.join(".git/refs/heads/topic.lock");
    // Someone else's lock (another value): kept, the settle refuses typed.
    repo.intent(
        vec![op("refs/heads/topic", Some(&repo.a), Some(&repo.b))],
        None,
        None,
    );
    std::fs::write(&lock, format!("{}\n", repo.a)).unwrap();
    assert_eq!(
        settle(&repo.path, &repo.ledgers).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    assert!(lock.exists());
    assert!(repo.pending());
    // The killed child's own lock: its new value.
    std::fs::write(&lock, format!("{}\n", repo.b)).unwrap();
    let custody = settle(&repo.path, &repo.ledgers).unwrap();
    assert!(!lock.exists());
    assert_eq!(repo.value("refs/heads/topic"), Some(repo.b.clone()));
    assert_eq!(custody.0.get("git-lock-removed"), Some(&1));
    assert!(!repo.pending());
    // A killed stash store: refs/stash.lock holding the entry it stored.
    std::fs::write(repo.path.join("f"), b"stashed\n").unwrap();
    run(&repo.path, &["stash", "push", "--quiet", "-m", "s"]);
    let entry = repo.value("refs/stash").unwrap();
    run(&repo.path, &["stash", "drop", "--quiet"]);
    repo.intent(
        Vec::new(),
        Some(StashOp {
            old: Vec::new(),
            new: vec![entry.clone()],
            messages: vec!["On main: s".to_owned()],
            observed_ns: 2,
        }),
        None,
    );
    let stash_lock = repo.path.join(".git/refs/stash.lock");
    std::fs::write(&stash_lock, format!("{entry}\n")).unwrap();
    let custody = settle(&repo.path, &repo.ledgers).unwrap();
    assert!(!stash_lock.exists());
    assert_eq!(stash_reflog(&repo.path).unwrap(), vec![entry]);
    assert_eq!(custody.0.get("stash-modified-at-destination"), None);
}

/// The earlier report's finding 4 (mutation `stash_settle_any_suffix`): a
/// stash op pending from a crash, and the operator drops an entry. The
/// settle continues the stash only from the reflog its intent records as
/// reached, or one store past it; the drop is kept and reported. Before
/// the fix a settle continued from any suffix of the new order and stored
/// the dropped entry again.
#[test]
fn a_stash_settle_never_undoes_an_operators_drop() {
    let repo = Repo::new();
    let mut entries = Vec::new();
    for message in ["one", "two"] {
        std::fs::write(repo.path.join("f"), message).unwrap();
        run(&repo.path, &["stash", "push", "--quiet", "-m", message]);
        entries.insert(0, repo.value("refs/stash").unwrap());
    }
    // The destination holds [one]; the capture appends two: [two, one].
    run(&repo.path, &["stash", "drop", "--quiet", "stash@{0}"]);
    let (one, two) = (entries[1].clone(), entries[0].clone());
    let stash = StashOp {
        old: vec![one.clone()],
        new: vec![two.clone(), one.clone()],
        messages: vec!["On main: two".to_owned(), "On main: one".to_owned()],
        observed_ns: 2,
    };
    assert_eq!(
        stash_path(&stash),
        vec![vec![one.clone()], vec![two.clone(), one.clone()]]
    );
    repo.intent(Vec::new(), Some(stash), None);
    // The operator drops `one` after the crash: the reflog is empty.
    run(&repo.path, &["stash", "drop", "--quiet"]);
    let custody = settle(&repo.path, &repo.ledgers).unwrap();
    assert_eq!(stash_reflog(&repo.path).unwrap(), Vec::<String>::new());
    assert_eq!(custody.0.get("stash-modified-at-destination"), Some(&1));
    // The row names what bulkload last landed, so the next capture's
    // decision keeps the operator's empty stash too (with no row, an empty
    // stash read as never restored and was restored again).
    let row = super::super::landed::read::<RefLedger>(
        &repo.ledgers.join(super::super::landed::REFS_LANDED),
    )
    .unwrap()
    .unwrap()
    .stash;
    assert_eq!(
        row.as_ref().map(|row| row.order.clone()),
        Some(vec![one.clone()])
    );
    let carried = [
        (two.clone(), "On main: two".to_owned()),
        (one.clone(), "On main: one".to_owned()),
    ];
    assert_eq!(
        stash_decision(&[], row.as_ref(), &carried, 3),
        (None, Some("stash-modified-at-destination"))
    );
    // A crash between a store and its progress record: the reflog is one
    // store past the progress, and the settle goes on from there.
    let rewrite = StashOp {
        old: Vec::new(),
        new: vec![two.clone(), one.clone()],
        messages: vec!["On main: two".to_owned(), "On main: one".to_owned()],
        observed_ns: 2,
    };
    run(
        &repo.path,
        &["update-ref", "--create-reflog", "refs/stash", &one],
    );
    repo.intent(Vec::new(), Some(rewrite.clone()), None);
    let custody = settle(&repo.path, &repo.ledgers).unwrap();
    assert_eq!(
        stash_reflog(&repo.path).unwrap(),
        vec![two.clone(), one.clone()]
    );
    assert_eq!(custody.0.get("stash-modified-at-destination"), None);
    // Its progress past [one], and the operator drops back to [one]: an
    // earlier state bulkload landed is no edit, and the store is made again.
    run(&repo.path, &["stash", "drop", "--quiet"]);
    repo.intent(Vec::new(), Some(rewrite), Some(vec![one.clone()]));
    settle(&repo.path, &repo.ledgers).unwrap();
    assert_eq!(stash_reflog(&repo.path).unwrap(), vec![two, one]);
}

/// Review 3 finding 7 (mutation `refs_unsynced`): the directories a ref
/// write's renames changed, synced before any ledger names the write.
#[test]
fn a_ref_write_syncs_the_directories_its_renames_changed() {
    let (git_dir, common) = (Path::new("/r/.git/worktrees/w"), Path::new("/r/.git"));
    let dirs = ref_dirs(git_dir, common, &["refs/heads/a/b", "HEAD"]);
    for expected in [
        "/r/.git",
        "/r/.git/refs/heads/a",
        "/r/.git/logs/refs/heads/a",
        "/r/.git/worktrees/w",
        "/r/.git/worktrees/w/logs",
    ] {
        assert!(dirs.contains(Path::new(expected)), "{expected}: {dirs:?}");
    }
}

// ---- review 4 (2026-10-09, later) --------------------------------------------

/// Set `path`'s modification time an hour back, as a lock a killed child
/// left long ago.
fn age(path: &Path) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_hours(1);
    file.set_times(std::fs::FileTimes::new().set_modified(past))
        .unwrap();
}

/// Review 4 (mutation `empty_lock_removed`): an empty lock is never
/// provably bulkload's. A live `update-ref --stdin` session that has
/// prepared a delete holds `packed-refs.lock` and the delete's own lock,
/// both empty, for as long as it likes; a settle whose intent deletes must
/// leave them and refuse typed. Before the fix an empty lock older than one
/// timestamp tick was removed, and two writers then raced on packed-refs.
#[test]
fn an_empty_lock_is_never_removed_even_when_old() {
    use std::io::Write as _;
    let repo = Repo::new();
    run(&repo.path, &["update-ref", "refs/remotes/o/x", &repo.a]);
    run(&repo.path, &["update-ref", "refs/heads/z", &repo.a]);
    repo.intent(
        vec![op("refs/remotes/o/x", Some(&repo.a), None)],
        None,
        None,
    );
    let mut live = std::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(&repo.path)
        .args(["update-ref", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = live.stdin.take().unwrap();
    stdin
        .write_all(format!("start\ndelete refs/heads/z {}\nprepare\n", repo.a).as_bytes())
        .unwrap();
    stdin.flush().unwrap();
    let packed = repo.path.join(".git/packed-refs.lock");
    let started = std::time::Instant::now();
    while !packed.exists() {
        assert!(started.elapsed() < std::time::Duration::from_mins(1));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Older than one timestamp tick, as the code before the fix required.
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    assert_eq!(
        settle(&repo.path, &repo.ledgers).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    assert!(packed.exists(), "a live writer's packed-refs.lock is kept");
    assert!(repo.path.join(".git/refs/heads/z.lock").exists());
    assert!(repo.pending());
    // The live session commits; the settle then goes through.
    stdin.write_all(b"commit\n").unwrap();
    drop(stdin);
    assert!(live.wait().unwrap().success());
    assert_eq!(repo.value("refs/heads/z"), None);
    settle(&repo.path, &repo.ledgers).unwrap();
    assert_eq!(repo.value("refs/remotes/o/x"), None);
    assert!(!repo.pending());
    // A killed delete's lock: empty and old. It stays, and the settle
    // refuses typed until the operator removes it, as Git instructs.
    run(&repo.path, &["update-ref", "refs/remotes/o/x", &repo.a]);
    repo.intent(
        vec![op("refs/remotes/o/x", Some(&repo.a), None)],
        None,
        None,
    );
    let lock = repo.path.join(".git/refs/remotes/o/x.lock");
    std::fs::write(&lock, b"").unwrap();
    age(&lock);
    assert_eq!(
        settle(&repo.path, &repo.ledgers).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    assert!(lock.exists());
    std::fs::remove_file(&lock).unwrap();
    settle(&repo.path, &repo.ledgers).unwrap();
    assert_eq!(repo.value("refs/remotes/o/x"), None);
}

/// Review 4 (mutation `packed_new_kept`): a killed delete of a packed ref
/// leaves `packed-refs.lock` (empty) and `packed-refs.new`. The lock is not
/// provably bulkload's and stays; once it is gone (the operator removed it,
/// as Git instructs), the settle removes the orphan `packed-refs.new`,
/// which Git creates exclusively, and goes through. Before the fix it
/// stayed, and every rerun's delete failed on it.
#[test]
fn an_orphan_packed_refs_new_is_removed_once_no_lock_holds_it() {
    let repo = Repo::new();
    run(&repo.path, &["update-ref", "refs/remotes/o/x", &repo.a]);
    run(&repo.path, &["pack-refs", "--all"]);
    repo.intent(
        vec![op("refs/remotes/o/x", Some(&repo.a), None)],
        None,
        None,
    );
    let (lock, new) = (
        repo.path.join(".git/packed-refs.lock"),
        repo.path.join(".git/packed-refs.new"),
    );
    std::fs::write(&lock, b"").unwrap();
    std::fs::write(&new, b"# pack-refs with: peeled fully-peeled sorted \n").unwrap();
    age(&lock);
    age(&new);
    assert_eq!(
        settle(&repo.path, &repo.ledgers).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    assert!(lock.exists() && new.exists());
    let _ = std::fs::remove_file(&lock);
    let custody = settle(&repo.path, &repo.ledgers).unwrap();
    assert!(!new.exists());
    assert_eq!(repo.value("refs/remotes/o/x"), None);
    assert_eq!(custody.0.get("git-lock-removed"), Some(&1));
    assert!(!repo.pending());
}

/// Review 4 (mutation `lock_written_after_create`): a lock bulkload takes
/// (every worktree's `HEAD.lock`, the index lock) holds its marker from the
/// moment its name exists, so no power loss can leave it empty (an empty
/// lock is not provably bulkload's, and wedged every rerun). Before the fix
/// the lock was created, then written, never synced. A temporary a crash
/// left before publishing is bulkload's and swept.
#[test]
fn a_lock_bulkload_takes_holds_its_marker_from_the_moment_it_exists() {
    let repo = Repo::new();
    let linked = repo.path.with_file_name("linked");
    run(
        &repo.path,
        &[
            "worktree",
            "add",
            "--quiet",
            linked.to_str().unwrap(),
            "topic",
        ],
    );
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = std::rc::Rc::clone(&seen);
    LOCK_VISIBLE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |lock: &Path| {
            record
                .borrow_mut()
                .push((lock.to_path_buf(), std::fs::read(lock).unwrap()));
        }));
    });
    let held = HeadLocks::take(&repo.path).unwrap();
    LOCK_VISIBLE.with(|hook| *hook.borrow_mut() = None);
    let seen = seen.borrow().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    for (lock, content) in &seen {
        assert_eq!(content.as_slice(), HEAD_LOCK_MARKER, "{}", lock.display());
    }
    // Held: a second taker refuses, and nothing of its attempt is left.
    assert_eq!(
        HeadLocks::take(&repo.path).unwrap_err(),
        BulkloadRefusal::GitConvergeInterrupted
    );
    drop(held);
    let git_dir = repo.path.join(".git");
    let leftovers = |dir: &Path| {
        std::fs::read_dir(dir)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(LOCK_TEMP_PREFIX)
            })
            .count()
    };
    assert_eq!(leftovers(&git_dir), 0);
    assert!(!git_dir.join("HEAD.lock").exists());
    // A temporary a crash left between writing and publishing: swept.
    std::fs::write(
        git_dir.join(format!("{LOCK_TEMP_PREFIX}1-0")),
        HEAD_LOCK_MARKER,
    )
    .unwrap();
    assert_eq!(sweep_lock_temps(&git_dir).unwrap(), 1);
    assert_eq!(leftovers(&git_dir), 0);
}

/// Review 4 (mutation `stash_reflog_unsynced`): a stash store's reflog,
/// which `update-ref` appends to and never syncs, is synced with the
/// directories before any record names the store; no other reflog is read
/// back, so no other is synced.
#[test]
fn a_stash_store_syncs_its_reflog() {
    let common = Path::new("/r/.git");
    assert_eq!(
        ref_files(common, &["refs/stash"]),
        vec![PathBuf::from("/r/.git/logs/refs/stash")]
    );
    assert!(ref_files(common, &["refs/heads/a", "HEAD"]).is_empty());
}

/// Review 4 (finding 7 of the review; mutation `symref_names_unplanned`): a
/// create beside an operator's symbolic ref (`refs/heads/alias` ->
/// `refs/heads/main`, then the source's `alias/x`) is dropped
/// (`ref-name-conflict`), in the plan and in a settle, never retried
/// forever. Before the fix the symbolic name was not among the names after
/// the step, and update-ref failed on every pass.
#[test]
fn a_create_beside_a_symbolic_ref_is_dropped() {
    let repo = Repo::new();
    run(
        &repo.path,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    let (native, symbolic) = native_refs(&repo.path).unwrap();
    let carried: BTreeMap<String, String> =
        [("refs/heads/alias/x".to_owned(), repo.a.clone())].into();
    let planned = plan_names(
        &native,
        &RefLedger {
            created: true,
            ..RefLedger::default()
        },
        &carried,
        2,
        &symbolic,
        (&symbolic, None),
    );
    assert!(planned.ops.is_empty(), "{:?}", planned.ops);
    assert_eq!(planned.custody.0.get("ref-name-conflict"), Some(&1));
    repo.intent(
        vec![op("refs/heads/alias/x", None, Some(&repo.a))],
        None,
        None,
    );
    let custody = settle(&repo.path, &repo.ledgers).unwrap();
    assert_eq!(custody.0.get("ref-name-conflict"), Some(&1));
    assert_eq!(repo.value("refs/heads/alias/x"), None);
    assert!(!repo.pending());
}
