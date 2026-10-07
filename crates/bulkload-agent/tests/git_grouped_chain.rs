//! P68 GROUPED-CHAIN: a grouped item chains under its plan base (Q42 lane
//! L6b, fix 2; OI-1003-Q42, OI-1003-Q46, OI-1003-Q62, OI-1003-Q63, R-N72).
//!
//! Before fix 2 every changed capture of a grouped item was a delta on the
//! plan base alone, so pass `k` re-packed everything committed since the
//! base: bytes grew as `k × round`. Under fix 2 the capture also declares
//! the source-held tips of its own prior capture, so it packs one round.
//!
//! **The table.** Two fixtures (no fuzzing, OI-1003-Q7): Probe-2 (the main
//! checkout plus the `wt` worktree) and P64-3 (P64's three-item group).
//! `N = 12` passes follow the first. Pass `k` commits a fresh incompressible
//! 512 KiB `rounds.bin` and a `notes.txt` edit in main; an even `k` also
//! commits in `wt`.
//!
//! **Per recaptured item** at pass `k`, with `B` its bundle, `P` its record's
//! bundle from the pass before, `H` the commits of `P`'s advertised tips the
//! source holds and `Bc` the base's commits:
//!
//! 1. `{B}.prior` names `P` at `P`'s depth (so `B` is one deeper),
//!    `{B}.base` names the group base, and `B`'s declared prerequisites are
//!    exactly `H ∪ Bc`.
//! 2. P64's law holds over `B`.
//! 3. Against the *prior*, not against `B`'s own prerequisites (which pass
//!    vacuously for a bundle that declares only the base): no packed commit
//!    is in `rev-list(H ∪ Bc)`, and no packed object lies under a tree of
//!    `H`.
//! 4. Flatness: `bytes(B) ≤ ROUND + 64 KiB`, from the bundle *file*. The
//!    table prints the maximum over `k`.
//!
//! **Re-base captures are excluded and pinned (OI-1003-Q62).** The policy
//! keeps depth limit 8 and root window 0, so the 9th changed capture
//! re-bases: a delta on the plan base alone (`.base`, no `.prior`) that
//! re-packs every round since the base. That cost is pinned here; only
//! L8's re-root removes it.
//!
//! **Restores** at `k ∈ {1, 2, 9, 10, 12}`: the destination is wiped,
//! `estate-apply` runs with a fresh state directory, and every item is
//! compared byte for byte.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod git_group;

use git_group::{
    apply_into, capture, compare, feed, fixture, git, law, noise, of_item, run, Fixture, Layout,
    Mutation,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `rounds.bin` of one pass: incompressible, so a re-pack shows in bytes.
const ROUND: u64 = 512 * 1024;
/// What one chained capture may add to its round: commits, trees, the
/// `notes.txt` edits, the capture's own snapshot commits and its header.
const SLACK: u64 = 64 * 1024;
/// Passes after the first.
const PASSES: u32 = 12;
/// `chain::CHAIN_DEPTH_LIMIT`: a capture whose prior is this deep re-bases.
const DEPTH_LIMIT: u8 = 8;
/// The passes whose corpus is restored and compared.
const RESTORES: [u32; 5] = [1, 2, 9, 10, 12];

fn sidecar(bundle: &Path, extension: &str) -> PathBuf {
    let mut path = bundle.as_os_str().to_owned();
    path.push(".");
    path.push(extension);
    PathBuf::from(path)
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// The corpus file a `.base` or `.prior` sidecar names: both records are a
/// postcard struct whose first field is that name (its length as a LEB128
/// varint, then the name).
fn named(record: &Path) -> String {
    let bytes = std::fs::read(record).unwrap();
    let (mut length, mut shift, mut start) = (0usize, 0u32, 0usize);
    for byte in &bytes {
        length |= usize::from(byte & 0x7f) << shift;
        shift += 7;
        start += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    String::from_utf8(bytes[start..start + length].to_vec()).unwrap()
}

/// The depth a `.prior` sidecar records for the link it names: its last
/// field, one byte for any depth within the limit.
fn link_depth(prior: &Path) -> u8 {
    *std::fs::read(prior).unwrap().last().unwrap()
}

/// The object names a bundle's header advertises.
fn advertised(bundle: &Path) -> Vec<String> {
    let bytes = std::fs::read(bundle).unwrap();
    let end = bytes.windows(2).position(|pair| pair == b"\n\n").unwrap();
    std::str::from_utf8(&bytes[..end])
        .unwrap()
        .lines()
        .skip(1)
        .filter(|line| !line.starts_with('-') && !line.starts_with('@'))
        .map(|line| line.split(' ').next().unwrap().to_owned())
        .collect()
}

/// The commits `tips` peel to that the source object store holds.
fn source_commits(fixture: &Fixture, tips: &[String]) -> BTreeSet<String> {
    let request = tips.iter().fold(String::new(), |mut lines, tip| {
        lines.push_str(tip);
        lines.push_str("^{commit}\n");
        lines
    });
    feed(
        git(&fixture.source).args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
        request.as_bytes(),
    )
    .lines()
    .filter_map(|line| line.strip_suffix(" commit"))
    .map(str::to_owned)
    .collect()
}

fn listed(fixture: &Fixture, args: &[&str], values: &BTreeSet<String>) -> BTreeSet<String> {
    if values.is_empty() {
        return BTreeSet::new();
    }
    let request = values.iter().cloned().collect::<Vec<_>>().join("\n");
    feed(
        git(&fixture.source).args(args).arg("--stdin"),
        request.as_bytes(),
    )
    .lines()
    .filter_map(|line| line.split(' ').next())
    .filter(|value| !value.is_empty())
    .map(str::to_owned)
    .collect()
}

/// The group's one plan base bundle in the corpus.
fn plan_base(fixture: &Fixture) -> PathBuf {
    let mut bases: Vec<PathBuf> = std::fs::read_dir(&fixture.corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = file_name(path);
            name.starts_with("shared-") && name.ends_with(".bundle")
        })
        .collect();
    assert_eq!(bases.len(), 1, "one plan base: {bases:?}");
    bases.pop().unwrap()
}

/// One item's latest capture: its bundle and its chain depth.
struct Held {
    item: String,
    bundle: PathBuf,
    depth: u8,
}

// Pass `k`'s commits: a fresh round and a notes edit in main, and on an even
// pass a notes edit in `wt`. Tracked files only, so each checkout's
// untracked scratch file stays untracked.
fn commit_round(fixture: &Fixture, k: u32) {
    let main = fixture.source.as_path();
    std::fs::write(
        main.join("rounds.bin"),
        noise(usize::try_from(ROUND).unwrap(), 5_000 + k),
    )
    .unwrap();
    let mut notes = std::fs::read(main.join("notes.txt")).unwrap();
    notes.extend_from_slice(format!("pass {k}\n").as_bytes());
    std::fs::write(main.join("notes.txt"), notes).unwrap();
    run(git(main).args(["commit", "-q", "-am", &format!("round {k}")]));
    if k.is_multiple_of(2) {
        let wt = fixture.items[1].0.as_path();
        let mut notes = std::fs::read(wt.join("notes.txt")).unwrap();
        notes.extend_from_slice(format!("wt pass {k}\n").as_bytes());
        std::fs::write(wt.join("notes.txt"), notes).unwrap();
        run(git(wt).args(["commit", "-q", "-am", &format!("wt {k}")]));
    }
}

// Wipe the destination, apply the corpus under a fresh state directory and
// compare every item byte for byte.
fn restore(fixture: &Fixture, k: u32) {
    for directory in [&fixture.dest, &fixture.restored] {
        std::fs::remove_dir_all(directory).unwrap();
        std::fs::create_dir(directory).unwrap();
    }
    run(git(&fixture.dest).args(["init", "-q", "--template="]));
    apply_into(fixture, &fixture.root.join(format!("applied-{k}")));
    compare(fixture);
}

/// What one table row measured.
#[derive(Default)]
struct Measured {
    /// Every violated clause, one line each.
    violations: Vec<String>,
    /// The largest chained (not re-base) capture, in bundle file bytes.
    flat_max: u64,
    /// Every re-base capture's bundle file bytes.
    rebases: Vec<u64>,
    /// Chained captures checked.
    chained: usize,
}

// Clauses 1 to 4 over `bundle`, pass `k`'s capture of the item `held`.
// Returns the depth it stands at.
fn check(
    fixture: &Fixture,
    name: &str,
    k: u32,
    held: &Held,
    bundle: &Path,
    measured: &mut Measured,
) -> u8 {
    let bytes = std::fs::metadata(bundle).unwrap().len();
    let (prior, base) = (sidecar(bundle, "prior"), sidecar(bundle, "base"));
    let label = format!("P68 {name} k={k} item={}", &held.item[..8]);
    let shared = plan_base(fixture);
    let based = base.exists() && named(&base) == file_name(&shared);
    if !based {
        measured
            .violations
            .push(format!("{label}: .base does not name the plan base"));
    }
    if held.depth == DEPTH_LIMIT {
        // OI-1003-Q62: the re-base capture is a delta on the plan base
        // alone. Its cost is pinned by the caller, not held to flatness.
        if prior.exists() {
            measured
                .violations
                .push(format!("{label}: a re-base capture has a .prior"));
        }
        eprintln!("{label} basis=Base rebase=true depth=0 bytes={bytes}");
        measured.rebases.push(bytes);
        let _ = law(fixture, name, k + 1, bundle);
        return 0;
    }
    // Clause 2.
    let (seen, _) = law(fixture, name, k + 1, bundle);
    // Clause 1.
    let chained = prior.exists();
    if !chained {
        measured.violations.push(format!("{label}: no .prior"));
    } else if named(&prior) != file_name(&held.bundle) || link_depth(&prior) != held.depth {
        measured.violations.push(format!(
            "{label}: .prior names {} at depth {}, not {} at depth {}",
            named(&prior),
            link_depth(&prior),
            file_name(&held.bundle),
            held.depth
        ));
    }
    let tips = source_commits(fixture, &advertised(&held.bundle));
    let base_commits = source_commits(fixture, &advertised(&shared));
    let expected: BTreeSet<String> = tips.union(&base_commits).cloned().collect();
    let declared: BTreeSet<String> = seen.prerequisites.iter().cloned().collect();
    if declared != expected {
        measured.violations.push(format!(
            "{label}: declares {} prerequisites, not the {} of H ∪ Bc ({} held tips, {} base \
             commits)",
            declared.len(),
            expected.len(),
            tips.len(),
            base_commits.len()
        ));
    }
    // Clause 3, against the prior.
    let reached = listed(fixture, &["rev-list"], &expected);
    let under = listed(fixture, &["rev-list", "--objects", "--no-walk"], &tips);
    let repacked = seen
        .packed
        .iter()
        .filter(|(value, packed)| {
            (packed.kind == "commit" && reached.contains(*value)) || under.contains(*value)
        })
        .count();
    if repacked > 0 {
        measured.violations.push(format!(
            "{label}: packs {repacked} objects the prior or the base already holds"
        ));
    }
    // Clause 4, from the bundle file.
    if bytes > ROUND + SLACK {
        measured.violations.push(format!(
            "{label}: {bytes} bytes, over the flat bound {}",
            ROUND + SLACK
        ));
    }
    let depth = if chained { held.depth + 1 } else { 0 };
    eprintln!(
        "{label} basis={} rebase=false depth={depth} bytes={bytes} prerequisites={} repacked={repacked}",
        if chained { "BaseAndChain" } else { "Base" },
        declared.len(),
    );
    if chained {
        measured.flat_max = measured.flat_max.max(bytes);
        measured.chained += 1;
    }
    depth
}

fn table(name: &str, layout: Layout) {
    let fixture = fixture(name, layout, Mutation::FirstPass);
    let mut before = capture(&fixture);
    assert_eq!(before.bundles.len(), fixture.items.len());
    let mut held: Vec<Held> = fixture
        .items
        .iter()
        .map(|(checkout, _)| {
            let item = before.receipt(checkout).item.clone();
            let bundle = before
                .bundles
                .iter()
                .find(|bundle| of_item(bundle, &item))
                .unwrap()
                .clone();
            // A first capture is a delta on the plan base alone.
            assert!(sidecar(&bundle, "base").exists(), "{}", bundle.display());
            assert!(!sidecar(&bundle, "prior").exists(), "{}", bundle.display());
            Held {
                item,
                bundle,
                depth: 0,
            }
        })
        .collect();
    let mut measured = Measured::default();
    for k in 1..=PASSES {
        commit_round(&fixture, k);
        let pass = capture(&fixture);
        let fresh = pass.fresh(&before);
        // Main moved, and the ref it moved is every item's: all recapture.
        assert_eq!(fresh.len(), fixture.items.len(), "pass {k}: {fresh:?}");
        for item in &mut held {
            let bundle = fresh
                .iter()
                .find(|bundle| of_item(bundle, &item.item))
                .unwrap_or_else(|| panic!("pass {k}: item {} not recaptured", item.item));
            item.depth = check(&fixture, name, k, item, bundle, &mut measured);
            item.bundle.clone_from(bundle);
        }
        before = pass;
        if RESTORES.contains(&k) {
            restore(&fixture, k);
        }
    }
    eprintln!(
        "P68 {name} chained={} flat_max={} bound={} rebases={:?}",
        measured.chained,
        measured.flat_max,
        ROUND + SLACK,
        measured.rebases
    );
    assert!(
        measured.violations.is_empty(),
        "P68 {name}: {} violations:\n{}",
        measured.violations.len(),
        measured.violations.join("\n")
    );
    // OI-1003-Q62: every item re-bases once in 12 passes, on its 9th
    // changed capture, and that capture re-packs the nine rounds since the
    // plan base. This is the cost L8's re-root removes.
    let items = fixture.items.len();
    assert_eq!(measured.rebases.len(), items, "{:?}", measured.rebases);
    assert_eq!(
        measured.chained,
        items * usize::try_from(PASSES - 1).unwrap()
    );
    for bytes in &measured.rebases {
        assert!(
            (9 * ROUND..=9 * (ROUND + SLACK)).contains(bytes),
            "P68 {name}: the re-base cost moved: {bytes}"
        );
    }
}

#[test]
fn p68_probe_2_grouped_chain() {
    table("probe-2", Layout::Pair);
}

#[test]
fn p68_p64_3_grouped_chain() {
    table("p64-3", Layout::Grouped);
}
