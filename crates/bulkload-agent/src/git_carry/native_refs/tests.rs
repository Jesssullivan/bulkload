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
