//! REFS-SCALE and the v1 ref table's compatibility laws (OI-1003-Q54, #178;
//! `docs/plans/2026-10-05-v1-header.md`).
//!
//! **REFS-SCALE.** For a generated source repository (loose and packed refs,
//! packed refs shadowed by loose ones, annotated tags, a tag of a tag, refs to
//! a tree and a blob, chained symrefs, a per-worktree ref, a stash, canonical
//! carry namespaces that copy the native refs, SHA-1 or SHA-256), a v1
//! capture followed by an import restores exactly the source's ref set: every
//! ref at its object, a canonical carry ref at its own name and every other
//! ref under the import's namespace, with the same peeled objects. The bundle
//! header stays under the cap and within a stated bound per distinct object,
//! and the capture's private repository holds one ref per distinct object,
//! not one per carried ref. The fixed row runs 131,072 refs.
//!
//! **Red/green.** The same capture in the old format (one header line per
//! carried ref, written by [`legacy_bundle`]) has a header over the cap at
//! that size, and every reader refuses it `GIT_INVENTORY_OVER_CAP`; the new
//! format carries it. The old code itself refused 119,761 carry-shaped refs
//! `GIT_INVENTORY_MALFORMED` (lane note, before-binary run).
//!
//! **Old-format compatibility.** An old-format capture imports to exactly the
//! refs the new format imports (generated rows, and 65,536 refs, whose
//! refspecs no longer fit one argv), and a capture written by the old binary
//! itself (`tests/fixtures/v1-old-format`) imports to exactly the refs that
//! binary imported. A new capture chains on an old-format prior and flattens.
//!
//! **Old readers refuse the new format.** The header-name rule of every
//! reader before the ref table, frozen from main `818926a`
//! ([`read_before_the_table`]), refuses every kind of new-format capture, so
//! a pre-change build refuses one instead of importing its tip refs as the
//! source's refs.
//!
//! **Distinct objects, not refs.** The header law and the import's cost are
//! per distinct object. These rows hold few distinct objects per ref (the
//! blahaj shape), where the header is far smaller than the old format's;
//! with one object per ref it is only about 1.3x smaller. The distinct-heavy
//! regime (one commit per ref, the import's CPU bounded per distinct object,
//! a chained pass and its fallback) is `tests/refs_scale_distinct.rs`. A
//! thin header over the cap is written self-contained
//! ([`a_chained_pass_over_the_thin_cap_is_written_self_contained`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use super::{
    canonical_tail, carried_heads, chain, export_repository, export_repository_with_drift, git,
    import_bundle, input, output, ref_table, refs, shared, stage_bundle, text, ExportOptions,
};
use crate::BulkloadRefusal;

/// The source slug every import here uses; no generated namespace uses it.
const SOURCE: &str = "neo";
/// A header line per distinct object: `<oid> refs/carry-export/ref-tip-v1/<oid>`.
const TIP_LINE_SHA1: usize = 40 + 1 + 29 + 40 + 1;
const TIP_LINE_SHA256: usize = 64 + 1 + 29 + 64 + 1;
/// The capture metadata, table and signature lines of a header, generously.
const HEADER_FIXED: usize = 2048;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "bulkload-refs-scale-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    Scratch(root)
}

// A hardened Git child in `repo` with a fixed identity and instant.
fn g(repo: &Path, args: &[&str]) -> String {
    text(
        git(repo)
            .env("GIT_AUTHOR_NAME", "Refs Scale")
            .env("GIT_AUTHOR_EMAIL", "refs-scale@localhost")
            .env("GIT_COMMITTER_NAME", "Refs Scale")
            .env("GIT_COMMITTER_EMAIL", "refs-scale@localhost")
            .env("GIT_AUTHOR_DATE", "1759622400 +0000")
            .env("GIT_COMMITTER_DATE", "1759622400 +0000")
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args),
    )
    .unwrap()
}

fn gi(repo: &Path, args: &[&str], stdin: &[u8]) -> Vec<u8> {
    input(git(repo).args(args), stdin).unwrap()
}

/// One generated source repository's shape.
#[derive(Debug, Clone)]
struct Shape {
    /// Commits on a linear history: the distinct commit targets.
    commits: usize,
    /// Annotated tags (tag objects) on those commits.
    annotated: usize,
    /// Branches and lightweight tags, each.
    native: usize,
    /// Canonical carry namespaces, each a copy of every native ref.
    namespaces: usize,
    /// Per mille of the planned refs left loose; the rest are packed.
    loose_per_mille: u32,
    /// Packed refs a loose ref of the same name then shadows.
    shadowed: usize,
    /// Run `pack-refs --all` over everything before the shadowing refs.
    pack_all: bool,
    stash: bool,
    sha256: bool,
}

struct Source {
    _root: Scratch,
    path: PathBuf,
    /// The source's refs as `for-each-ref` lists them: what a capture reads.
    inventory: BTreeMap<String, String>,
    /// The stash reflog's commits.
    stashes: Vec<String>,
}

impl Source {
    fn distinct(&self) -> usize {
        self.inventory
            .values()
            .chain(&self.stashes)
            .collect::<BTreeSet<_>>()
            .len()
    }

    fn objects(&self) -> PathBuf {
        self.path.join(".git/objects")
    }
}

fn namespace_digest(k: usize) -> String {
    blake3::hash(format!("refs-scale namespace {k}").as_bytes())
        .to_hex()
        .to_string()
}

#[allow(clippy::too_many_lines)] // One generator, read top to bottom.
fn generate(name: &str, shape: &Shape) -> Source {
    let root = scratch(name);
    let path = root.0.join("source");
    let format = if shape.sha256 { "sha256" } else { "sha1" };
    g(
        &root.0,
        &[
            "init",
            "--quiet",
            "--template=",
            "-b",
            "main",
            &format!("--object-format={format}"),
            "source",
        ],
    );
    // History, annotated tags and one loose blob in one fast-import.
    let mut stream = String::new();
    for i in 0..shape.commits {
        let message = format!("commit {i} of {name}\n");
        let content = format!("version {i} of {name}\n");
        write!(
            stream,
            "commit refs/heads/main\nmark :{}\ncommitter Refs Scale <refs-scale@localhost> {} +0000\ndata {}\n{message}M 100644 inline f{}\ndata {}\n{content}\n",
            i + 1,
            1_759_622_400 + i,
            message.len(),
            i % 8,
            content.len(),
        )
        .unwrap();
    }
    for j in 0..shape.annotated {
        let message = format!("annotated {j}\n");
        write!(
            stream,
            "tag a{j:05}\nfrom :{}\ntagger Refs Scale <refs-scale@localhost> 1759622400 +0000\ndata {}\n{message}\n",
            j % shape.commits + 1,
            message.len(),
        )
        .unwrap();
    }
    stream.push_str("blob\nmark :999999\ndata 9\nref blob\n\n");
    let marks = root.0.join("marks");
    gi(
        &path,
        &[
            "fast-import",
            "--quiet",
            &format!("--export-marks={}", marks.display()),
        ],
        stream.as_bytes(),
    );
    let mut by_mark = BTreeMap::new();
    for line in fs::read_to_string(&marks).unwrap().lines() {
        let (mark, value) = line.split_once(' ').unwrap();
        by_mark.insert(mark[1..].parse::<usize>().unwrap(), value.to_owned());
    }
    let commits: Vec<String> = (1..=shape.commits).map(|i| by_mark[&i].clone()).collect();
    let blob = by_mark[&999_999].clone();
    g(&path, &["reset", "--quiet", "--hard", "main"]);
    let annotated: Vec<(String, String)> = g(
        &path,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/tags/",
        ],
    )
    .lines()
    .map(|line| {
        let (name, value) = line.split_once(' ').unwrap();
        (name.to_owned(), value.to_owned())
    })
    .collect();
    // The native refs every namespace copies.
    let mut native: Vec<(String, String)> = Vec::new();
    for i in 0..shape.native {
        native.push((
            format!("refs/heads/b{i:05}"),
            commits[i % shape.commits].clone(),
        ));
        native.push((
            format!("refs/tags/l{i:05}"),
            commits[(i * 7) % shape.commits].clone(),
        ));
    }
    native.extend(annotated.iter().cloned());
    let mut planned: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in &native {
        if !annotated.iter().any(|(tag, _)| tag == name) {
            planned.insert(name.clone(), value.clone());
        }
    }
    let slugs = ["sting-c1", "neo-20260922-c2a", "lab_x"];
    for k in 0..shape.namespaces {
        let prefix = format!("refs/carry/v1/{}/{}/", slugs[k % 3], namespace_digest(k));
        for (name, value) in &native {
            planned.insert(format!("{prefix}{name}"), value.clone());
        }
        for (suffix, value) in [
            ("head", &commits[k % shape.commits]),
            ("worktree", &commits[0]),
        ] {
            planned.insert(format!("{prefix}{suffix}"), value.clone());
        }
    }
    // Split by a stable hash of the name: packed, or loose.
    let (loose, packed): (Vec<_>, Vec<_>) = planned.iter().partition(|(name, _)| {
        u32::from_le_bytes(
            blake3::hash(name.as_bytes()).as_bytes()[..4]
                .try_into()
                .unwrap(),
        ) % 1000
            < shape.loose_per_mille
    });
    let mut packed_file = String::from("# pack-refs with: sorted \n");
    for (name, value) in &packed {
        writeln!(packed_file, "{value} {name}").unwrap();
    }
    fs::write(path.join(".git/packed-refs"), packed_file).unwrap();
    let mut transaction = Vec::new();
    for (name, value) in &loose {
        transaction.extend_from_slice(format!("create {name}\0{value}\0").as_bytes());
    }
    if !transaction.is_empty() {
        gi(&path, &["update-ref", "--stdin", "-z"], &transaction);
    }
    // The odd ones out.
    let tag = &annotated.first().unwrap().1;
    let tag_of_tag = String::from_utf8(gi(
        &path,
        &["mktag"],
        format!(
            "object {tag}\ntype tag\ntag tag-of-tag\ntagger Refs Scale <refs-scale@localhost> 1759622400 +0000\n\nnested\n"
        )
        .as_bytes(),
    ))
    .unwrap();
    #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax, not interpolation.
    let tree = g(&path, &["rev-parse", "main^{tree}"]);
    for (name, value) in [
        ("refs/tags/tag-of-tag", tag_of_tag.trim()),
        ("refs/trees/root", tree.as_str()),
        ("refs/blobs/one", blob.as_str()),
        ("refs/remotes/origin/main", commits[0].as_str()),
        ("refs/worktree/pinned", commits[1 % shape.commits].as_str()),
        ("refs/carry/v1/odd", commits[2 % shape.commits].as_str()),
    ] {
        g(&path, &["update-ref", name, value]);
    }
    g(
        &path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    g(
        &path,
        &[
            "symbolic-ref",
            "refs/symbolic/second",
            "refs/remotes/origin/HEAD",
        ],
    );
    if shape.pack_all {
        g(&path, &["pack-refs", "--all"]);
    }
    // Packed refs that a loose ref of the same name shadows.
    for (index, (name, _)) in packed.iter().take(shape.shadowed).enumerate() {
        g(
            &path,
            &["update-ref", name, &commits[(index + 3) % shape.commits]],
        );
    }
    if shape.stash {
        fs::write(path.join("f0"), b"stashed edit\n").unwrap();
        g(&path, &["stash", "push", "--quiet", "-m", "scale"]);
    }
    let inventory: BTreeMap<String, String> = g(
        &path,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
    .lines()
    .map(|line| {
        let (name, value) = line.split_once(' ').unwrap();
        (name.to_owned(), value.to_owned())
    })
    .collect();
    let stashes = if shape.stash {
        g(&path, &["reflog", "show", "--format=%H", "refs/stash"])
            .lines()
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    Source {
        _root: root,
        path,
        inventory,
        stashes,
    }
}

// Every ref under `refs/carry/v1/` of `repo`, with what it peels to.
fn imported(repo: &Path) -> BTreeMap<String, (String, String)> {
    g(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname) %(*objectname)",
            "refs/carry/v1/",
        ],
    )
    .lines()
    .map(|line| {
        let mut fields = line.split(' ');
        let name = fields.next().unwrap().to_owned();
        let value = fields.next().unwrap().to_owned();
        let peeled = fields.next().unwrap_or_default().to_owned();
        (name, (value, peeled))
    })
    .collect()
}

fn fresh_repository(root: &Path, name: &str, sha256: bool) -> PathBuf {
    let repository = root.join(name);
    let format = if sha256 { "sha256" } else { "sha1" };
    g(
        root,
        &[
            "init",
            "--quiet",
            "--bare",
            "--template=",
            &format!("--object-format={format}"),
            name,
        ],
    );
    repository
}

/// Import `bundle` into a fresh repository and check that it holds exactly
/// the source's refs, each at its object and with its peeled object, plus
/// the stash commits and the capture's own metadata. Returns the import.
fn check_exact(
    source: &Source,
    bundle: &Path,
    root: &Path,
    name: &str,
) -> BTreeMap<String, (String, String)> {
    let destination =
        fresh_repository(root, name, source.inventory.values().any(|v| v.len() == 64));
    let count = import_bundle(&destination, bundle, SOURCE).unwrap();
    let restored = imported(&destination);
    assert_eq!(count, restored.len(), "{name}: one ref per carried line");
    let own = restored
        .keys()
        .find_map(|ref_name| {
            let rest = ref_name.strip_prefix(&format!("refs/carry/v1/{SOURCE}/"))?;
            let (digest, leaf) = rest.split_once('/')?;
            (leaf == "head" && digest.len() == 64)
                .then(|| format!("refs/carry/v1/{SOURCE}/{digest}/"))
        })
        .unwrap();
    let peeled_in_source: BTreeMap<String, String> = g(
        &source.path,
        &["for-each-ref", "--format=%(refname) %(*objectname)"],
    )
    .lines()
    .map(|line| {
        // `text` trims the last line's trailing space when nothing peels.
        let (ref_name, peeled) = line.split_once(' ').unwrap_or((line, ""));
        (ref_name.to_owned(), peeled.to_owned())
    })
    .collect();
    let mut expected = BTreeMap::new();
    for (ref_name, value) in &source.inventory {
        let canonical = ref_name
            .strip_prefix("refs/carry/v1/")
            .is_some_and(canonical_tail);
        let target = if canonical {
            ref_name.clone()
        } else {
            format!("{own}{ref_name}")
        };
        expected.insert(target, (value.clone(), peeled_in_source[ref_name].clone()));
    }
    for stash in &source.stashes {
        expected
            .entry(format!("{own}stashes/{stash}"))
            .or_insert_with(|| (stash.clone(), String::new()));
    }
    for (ref_name, value) in &restored {
        if ref_name
            .strip_prefix(&own)
            .is_some_and(|leaf| !leaf.contains('/'))
        {
            expected.insert(ref_name.clone(), value.clone());
        }
    }
    let missing: Vec<&String> = expected
        .keys()
        .filter(|key| restored.get(*key) != expected.get(*key))
        .take(5)
        .collect();
    let extra: Vec<&String> = restored
        .keys()
        .filter(|key| !expected.contains_key(*key))
        .take(5)
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty() && restored.len() == expected.len(),
        "{name}: missing or different {missing:?}, unexpected {extra:?}"
    );
    restored
}

fn header_len(bundle: &Path) -> usize {
    let bytes = fs::read(bundle).unwrap();
    bytes.windows(2).position(|pair| pair == b"\n\n").unwrap() + 2
}

/// The header law: under the cap, and within the fixed lines plus one tip
/// line per distinct object. The private repository holds one ref per
/// distinct object, the table and the metadata: never one per carried ref.
fn check_header(source: &Source, capture: &Path, bundle: &Path) -> usize {
    let header = header_len(bundle);
    let line = if source.inventory.values().any(|v| v.len() == 64) {
        TIP_LINE_SHA256
    } else {
        TIP_LINE_SHA1
    };
    let bound = HEADER_FIXED + line * source.distinct();
    assert!(header <= shared::HEADER_CAP, "header {header} over the cap");
    assert!(
        header <= bound,
        "header {header} over {bound} for {} distinct objects",
        source.distinct()
    );
    let private = refs(&capture.join("repository.git")).unwrap();
    let tips = private
        .lines()
        .filter(|line| line.contains(ref_table::TIP_PREFIX))
        .count();
    assert_eq!(tips, source.distinct());
    assert!(
        private.lines().count() <= tips + 16,
        "{} private refs",
        private.lines().count()
    );
    header
}

/// The same capture in the old format: the private repository's ref table
/// expanded back into one ref per carried line (written as a `packed-refs`
/// file, for speed), its table and tip refs deleted, and `bundle create
/// --all`, as the old writer ran it.
fn legacy_bundle(capture: &Path) -> PathBuf {
    let private = capture.join("repository.git");
    let listed = refs(&private).unwrap();
    let heads = ref_table::expand(&private, &listed).unwrap().unwrap();
    let mut carried = BTreeMap::new();
    for line in heads.lines() {
        let (value, name) = line.split_once(' ').unwrap();
        if name
            .strip_prefix("refs/carry-export/")
            .is_some_and(|rest| rest.contains('/'))
        {
            carried.insert(name.to_owned(), value.to_owned());
        }
    }
    let mut packed = String::from("# pack-refs with: sorted \n");
    for (name, value) in &carried {
        writeln!(packed, "{value} {name}").unwrap();
    }
    fs::write(private.join("packed-refs"), packed).unwrap();
    let mut deletions = String::new();
    for line in listed.lines() {
        let name = line.split_once(' ').unwrap().1;
        if ref_table::is_table(name) || name.starts_with(ref_table::TIP_PREFIX) {
            writeln!(deletions, "delete {name}").unwrap();
        }
    }
    gi(&private, &["update-ref", "--stdin"], deletions.as_bytes());
    let bundle = capture.join("legacy.bundle");
    output(
        git(&private)
            .args(["bundle", "create", "--quiet"])
            .arg(&bundle)
            .arg("--all"),
    )
    .unwrap();
    bundle
}

/// One REFS-SCALE row: capture, the header law, exact import; with
/// `legacy`, the old format of the same capture imports to the same refs.
fn row(name: &str, shape: &Shape, legacy: bool) -> (Source, usize) {
    let source = generate(name, shape);
    let work = scratch(&format!("{name}-work"));
    let capture = work.0.join("capture");
    let started = std::time::Instant::now();
    let bundle = export_repository(&source.path, &capture).unwrap();
    let captured = started.elapsed();
    let header = check_header(&source, &capture, &bundle);
    let restored = check_exact(&source, &bundle, &work.0, "new");
    eprintln!(
        "REFS-SCALE row={name} refs={} distinct={} header={header} bundle={} capture_wall_ms={}",
        source.inventory.len(),
        source.distinct(),
        fs::metadata(&bundle).unwrap().len(),
        captured.as_millis(),
    );
    if legacy {
        let old = legacy_bundle(&capture);
        assert_eq!(
            check_exact(&source, &old, &work.0, "old"),
            restored,
            "{name}: the old format imports to other refs"
        );
    }
    (source, header)
}

fn shape() -> impl proptest::strategy::Strategy<Value = Shape> {
    use proptest::prelude::*;
    (
        (1usize..40, 1usize..12, 0usize..60, 0usize..12),
        (
            0u32..=1000,
            0usize..8,
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
        ),
    )
        .prop_map(
            |(
                (commits, annotated, native, namespaces),
                (loose, shadowed, pack_all, stash, sha256),
            )| Shape {
                commits: commits.max(3),
                annotated,
                native,
                namespaces,
                loose_per_mille: loose,
                shadowed,
                pack_all,
                stash,
                sha256,
            },
        )
}

proptest::proptest! {
    #![proptest_config(crate::test_support::prop_config(4))]

    /// REFS-SCALE over generated shapes: exact restore, the header law, and
    /// the old format of the same capture importing to the same refs.
    #[test]
    fn refs_scale_generated_repositories_restore_exactly(shape in shape()) {
        row("generated", &shape, true);
    }
}

/// REFS-SCALE's fixed row: 131,072 refs over about 1,200 distinct objects,
/// blahaj-shaped (157 namespaces copying the native refs, annotated tags
/// among them). The new format carries and restores it exactly with a header
/// of a few hundred KB; its old format is over the cap, and every reader
/// refuses it as a typed size refusal.
#[test]
fn refs_scale_131072_refs_carry_and_the_old_format_is_refused_typed() {
    let shape = Shape {
        commits: 1_000,
        annotated: 160,
        native: 357,
        namespaces: 157,
        loose_per_mille: 50,
        shadowed: 64,
        pack_all: false,
        stash: true,
        sha256: false,
    };
    let source = generate("fixed", &shape);
    assert!(
        source.inventory.len() >= 130_000,
        "{} refs",
        source.inventory.len()
    );
    let work = scratch("fixed-work");
    let capture = work.0.join("capture");
    let bundle = export_repository(&source.path, &capture).unwrap();
    let header = check_header(&source, &capture, &bundle);
    assert!(header < 1024 * 1024, "{header}");
    check_exact(&source, &bundle, &work.0, "new");
    // Red: the old format of this capture. Its header is over the cap, and
    // the readers refuse it as a size, never as a malformed inventory.
    let old = legacy_bundle(&capture);
    let old_header = header_len(&old);
    eprintln!(
        "REFS-SCALE row=fixed refs={} distinct={} header={header} old_header={old_header}",
        source.inventory.len(),
        source.distinct(),
    );
    assert!(old_header > shared::HEADER_CAP, "{old_header}");
    assert_eq!(
        shared::requires_base(&old),
        Err(BulkloadRefusal::GitInventoryOverCap)
    );
    assert_eq!(
        chain::advertised(&old).map(|refs| refs.len()),
        Err(BulkloadRefusal::GitInventoryOverCap)
    );
    assert_eq!(
        shared::PackStats::record(&old, false, 0),
        Err(BulkloadRefusal::GitInventoryOverCap)
    );
}

/// A SHA-256 source carries and restores exactly too, in either format: its
/// tip lines are 159 B, and its table lines carry 64-hex object names.
#[test]
fn a_sha256_repository_restores_exactly_in_either_format() {
    let shape = Shape {
        commits: 9,
        annotated: 3,
        native: 12,
        namespaces: 4,
        loose_per_mille: 400,
        shadowed: 3,
        pack_all: false,
        stash: true,
        sha256: true,
    };
    let (source, _) = row("sha256", &shape, true);
    assert!(source.inventory.values().all(|value| value.len() == 64));
}

/// Old-format compatibility at a size the old import could not fetch: 65,536
/// refs, whose refspecs no longer fit one argv. The old format's header is
/// under the cap, and it imports to exactly the refs the new format does.
#[test]
fn an_old_format_capture_of_65536_refs_imports_like_the_new_format() {
    let shape = Shape {
        commits: 500,
        annotated: 40,
        native: 240,
        namespaces: 125,
        loose_per_mille: 100,
        shadowed: 16,
        pack_all: false,
        stash: false,
        sha256: false,
    };
    let (source, _) = row("old-65536", &shape, true);
    assert!(
        source.inventory.len() >= 65_536,
        "{}",
        source.inventory.len()
    );
}

/// A capture the old binary wrote (main `818926a`, `git-export`) imports to
/// exactly the refs that binary's own `git-import` laid down (`imported.refs`,
/// `for-each-ref --format='%(objectname) %(*objectname) %(objecttype)
/// %(refname)'`), and restores a workspace.
#[test]
fn a_capture_written_by_the_old_binary_imports_exactly() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1-old-format");
    let work = scratch("old-binary");
    let bundle = work.0.join("capture.bundle");
    fs::copy(fixtures.join("capture.bundle"), &bundle).unwrap();
    // The fixture is the old format: no ref table, one line per carried ref.
    let listed = g(&work.0, &["bundle", "list-heads", bundle.to_str().unwrap()]);
    assert!(!listed.contains(ref_table::TABLE_REF), "{listed}");
    assert_eq!(listed.lines().count(), 23);
    let destination = fresh_repository(&work.0, "imported", false);
    assert_eq!(import_bundle(&destination, &bundle, SOURCE).unwrap(), 23);
    let restored = g(
        &destination,
        &[
            "for-each-ref",
            "--format=%(objectname) %(*objectname) %(objecttype) %(refname)",
        ],
    );
    let expected = fs::read_to_string(fixtures.join("imported.refs")).unwrap();
    assert_eq!(restored, expected.trim_end());
    let workspace = work.0.join("restored");
    super::restore_bundle(&bundle, &workspace, SOURCE).unwrap();
    assert_eq!(
        fs::read(workspace.join("tracked")).unwrap(),
        b"tracked v2\n"
    );
    assert_eq!(
        fs::read(workspace.join("ignored.txt")).unwrap(),
        b"ignored payload\n"
    );
    assert_eq!(g(&workspace, &["symbolic-ref", "HEAD"]), "refs/heads/main");
}

/// A new-format capture chains on an old-format prior: its prerequisites are
/// the prior's source-held tips, and the flattened chain restores exactly.
#[test]
fn a_new_capture_chains_on_an_old_format_prior_and_flattens_exactly() {
    let shape = Shape {
        commits: 12,
        annotated: 3,
        native: 8,
        namespaces: 3,
        loose_per_mille: 500,
        shadowed: 2,
        pack_all: true,
        stash: false,
        sha256: false,
    };
    let mut source = generate("mixed-chain", &shape);
    let work = scratch("mixed-chain-work");
    let first = work.0.join("first");
    export_repository(&source.path, &first).unwrap();
    let prior = legacy_bundle(&first);
    assert!(shared::prerequisites(&prior).unwrap().is_empty());
    fs::write(source.path.join("f1"), b"changed after the prior\n").unwrap();
    g(
        &source.path,
        &["commit", "--quiet", "-am", "after the prior"],
    );
    g(&source.path, &["update-ref", "refs/heads/added", "HEAD"]);
    source.inventory = g(
        &source.path,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
    .lines()
    .map(|line| {
        let (name, value) = line.split_once(' ').unwrap();
        (name.to_owned(), value.to_owned())
    })
    .collect();
    let export = export_repository_with_drift(
        &source.path,
        &work.0.join("second"),
        &ExportOptions {
            chain: Some(&prior),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert!(export.chained, "an old-format prior offers its tips");
    assert!(!shared::prerequisites(&export.bundle).unwrap().is_empty());
    let digest = blake3::hash(&fs::read(&prior).unwrap());
    let flat = chain::flatten(
        stage_bundle(&export.bundle).unwrap(),
        &[(prior, *digest.as_bytes())],
    )
    .unwrap();
    check_exact(&source, flat.path(), &work.0, "flattened");
    // The carried heads agree whichever way they are read.
    assert_eq!(
        sorted(&carried_heads(flat.path(), None).unwrap()),
        sorted(&carried_heads(&export.bundle, Some(&source.objects())).unwrap())
    );
}

fn sorted(lines: &str) -> Vec<String> {
    let mut lines: Vec<String> = lines.lines().map(str::to_owned).collect();
    lines.sort_unstable();
    lines
}

/// A plan base's item declares, in one batched query, exactly the peeled
/// commits of the base's refs: never the base's own ref table, never a tag,
/// tree or blob object. A base head the item's repository does not hold is a
/// typed refusal.
#[test]
fn a_plan_base_item_declares_the_peeled_base_commits_and_skips_its_table() {
    let shape = Shape {
        commits: 6,
        annotated: 2,
        native: 3,
        namespaces: 2,
        loose_per_mille: 300,
        shadowed: 1,
        pack_all: false,
        stash: false,
        sha256: false,
    };
    let source = generate("base", &shape);
    let work = scratch("base-work");
    let base = shared::export_base(&source.path, &work.0.join("base")).unwrap();
    let base_heads = chain::advertised(&base).unwrap();
    assert!(base_heads.iter().any(|(_, name)| ref_table::is_table(name)));
    let delta =
        super::export_repository_with_prerequisite(&source.path, &work.0.join("delta"), &base)
            .unwrap();
    let declared: BTreeSet<String> = shared::prerequisites(&delta).unwrap().into_iter().collect();
    let mut request = String::new();
    for value in source
        .inventory
        .values()
        .chain(std::iter::once(&g(&source.path, &["rev-parse", "HEAD"])))
    {
        writeln!(request, "{value}^{{commit}}").unwrap();
    }
    let expected: BTreeSet<String> = String::from_utf8(gi(
        &source.path,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        request.as_bytes(),
    ))
    .unwrap()
    .lines()
    .filter_map(|line| line.strip_suffix(" commit").map(str::to_owned))
    .collect();
    assert_eq!(declared, expected);
    // The item restores on top of its base.
    let destination = fresh_repository(&work.0, "based", false);
    import_bundle(&destination, &base, SOURCE).unwrap();
    import_bundle(&destination, &delta, SOURCE).unwrap();
    // A base whose heads this source never held: refused by type.
    let other = generate(
        "base-other",
        &Shape {
            commits: 4,
            ..shape
        },
    );
    let foreign = shared::export_base(&other.path, &work.0.join("foreign")).unwrap();
    assert_eq!(
        super::export_repository_with_prerequisite(
            &source.path,
            &work.0.join("foreign-delta"),
            &foreign,
        ),
        Err(BulkloadRefusal::GitInventoryMissingPrerequisite)
    );
}

/// The cap at every reader is a typed size refusal, never a malformed
/// inventory; a line over the line bound is still malformed.
#[test]
fn every_header_reader_refuses_an_over_cap_header_by_size() {
    let work = scratch("over-cap-reader");
    let bundle = work.0.join("over.bundle");
    let mut header = b"# v2 git bundle\n".to_vec();
    let line = format!(
        "{} refs/carry-export/refs/heads/{}\n",
        "a".repeat(40),
        "x".repeat(60)
    );
    while header.len() <= shared::HEADER_CAP {
        header.extend_from_slice(line.as_bytes());
    }
    header.extend_from_slice(b"\nPACK\0\0\0\x02\0\0\0\0");
    fs::write(&bundle, &header).unwrap();
    assert_eq!(
        shared::prerequisites(&bundle),
        Err(BulkloadRefusal::GitInventoryOverCap)
    );
    assert_eq!(
        chain::advertised(&bundle).map(|refs| refs.len()),
        Err(BulkloadRefusal::GitInventoryOverCap)
    );
    assert_eq!(
        shared::bundle_pack_len(&bundle),
        Err(BulkloadRefusal::GitInventoryOverCap)
    );
    let long = work.0.join("long.bundle");
    let mut header = b"# v2 git bundle\n".to_vec();
    header.extend_from_slice(&vec![b'a'; 2 * 1024 * 1024]);
    header.extend_from_slice(b"\n\n");
    fs::write(&long, header).unwrap();
    assert_eq!(
        shared::prerequisites(&long),
        Err(BulkloadRefusal::GitInventoryMalformed)
    );
}

/// Writers measure the header before they write. A thin bundle (a plan
/// base's item, a chained link) whose prerequisite lines would put it over
/// the cap is written self-contained instead, never refused while that fits.
/// A self-contained bundle whose private refs would be over the cap is
/// refused by size, and so is the thin writer's fallback to it, with nothing
/// left at the bundle path.
#[test]
fn writers_measure_an_over_cap_header_before_writing_it() {
    let work = scratch("over-cap-writer");
    let private = work.0.join("repository.git");
    g(
        &work.0,
        &["init", "--quiet", "--bare", "--template=", "repository.git"],
    );
    let blob = String::from_utf8(gi(&private, &["hash-object", "-w", "--stdin"], b"one"))
        .unwrap()
        .trim()
        .to_owned();
    g(
        &private,
        &[
            "update-ref",
            &format!("{}{blob}", ref_table::TIP_PREFIX),
            &blob,
        ],
    );
    // Thin: 320,000 prerequisite lines of 54 B, about 17.3 MB, over the cap;
    // the self-contained header is one line.
    let commits: BTreeSet<String> = (0..320_000u32)
        .map(|index| blake3::hash(&index.to_le_bytes()).to_hex()[..40].to_owned())
        .collect();
    let thin = work.0.join("thin.bundle");
    let (_, chained) =
        shared::write_excluding_tip_trees(&private, &thin, &commits, shared::HEADER_CAP).unwrap();
    assert!(
        !chained,
        "an over-cap thin header is written self-contained"
    );
    assert!(shared::prerequisites(&thin).unwrap().is_empty());
    assert!(header_len(&thin) < 1024);
    for leftover in ["objects-pending", "header-pending"] {
        assert!(!thin.with_extension(leftover).exists(), "{leftover}");
    }
    // Self-contained: 170,000 private refs of about 104 B each.
    let mut packed = String::from("# pack-refs with: sorted \n");
    let names: BTreeSet<String> = (0..170_000u32)
        .map(|index| format!("refs/carry-export/ref-tip-v1/{index:0>40}"))
        .collect();
    for name in &names {
        writeln!(packed, "{blob} {name}").unwrap();
    }
    fs::write(private.join("packed-refs"), packed).unwrap();
    let full = work.0.join("full.bundle");
    assert_eq!(
        shared::write_full(&private, &full),
        Err(BulkloadRefusal::GitInventoryOverCap)
    );
    assert!(!full.exists());
    let over = work.0.join("over.bundle");
    assert_eq!(
        shared::write_excluding_tip_trees(&private, &over, &commits, shared::HEADER_CAP)
            .map(|(_, chained)| chained),
        Err(BulkloadRefusal::GitInventoryOverCap)
    );
    for leftover in ["bundle", "objects-pending", "header-pending"] {
        assert!(!over.with_extension(leftover).exists(), "{leftover}");
    }
}

/// A chained pass whose thin header would be over the cap is written
/// self-contained, `chained == false`, and imports exactly; at its own header
/// length the same pass stays thin. The cap is lowered to this pass's thin
/// header (the production cap needs about 101,680 distinct commits on SHA-1;
/// the deep tier of `tests/refs_scale_distinct.rs` runs that size). Before,
/// the pass refused `GIT_INVENTORY_OVER_CAP`, and so did every later pass
/// offered the same chain.
#[test]
fn a_chained_pass_over_the_thin_cap_is_written_self_contained() {
    let shape = Shape {
        commits: 12,
        annotated: 2,
        native: 10,
        namespaces: 2,
        loose_per_mille: 300,
        shadowed: 1,
        pack_all: false,
        stash: false,
        sha256: false,
    };
    let mut source = generate("thin-cap", &shape);
    let work = scratch("thin-cap-work");
    let prior = export_repository(&source.path, &work.0.join("first")).unwrap();
    fs::write(source.path.join("f1"), b"changed after the prior\n").unwrap();
    g(
        &source.path,
        &["commit", "--quiet", "-am", "after the prior"],
    );
    g(&source.path, &["update-ref", "refs/heads/added", "HEAD"]);
    source.inventory = g(
        &source.path,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
    .lines()
    .map(|line| {
        let (name, value) = line.split_once(' ').unwrap();
        (name.to_owned(), value.to_owned())
    })
    .collect();
    let second = work.0.join("second");
    let export = export_repository_with_drift(
        &source.path,
        &second,
        &ExportOptions {
            chain: Some(&prior),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert!(export.chained);
    let thin = header_len(&export.bundle);
    let private = second.join("repository.git");
    let fallback = work.0.join("fallback.bundle");
    let (_, chained) =
        shared::write_chained_capped(&private, &fallback, &source.path, &prior, thin - 1).unwrap();
    assert!(!chained, "over the cap: self-contained");
    assert!(shared::prerequisites(&fallback).unwrap().is_empty());
    assert!(header_len(&fallback) < thin);
    check_exact(&source, &fallback, &work.0, "fallback");
    let kept = work.0.join("kept.bundle");
    let (_, chained) =
        shared::write_chained_capped(&private, &kept, &source.path, &prior, thin).unwrap();
    assert!(chained, "at its own header length: thin");
    assert_eq!(header_len(&kept), thin);
    assert_eq!(
        shared::prerequisites(&kept).unwrap(),
        shared::prerequisites(&export.bundle).unwrap()
    );
}

/// The header-name rule of every reader before the ref table, frozen from
/// main `818926a` (`import_verified`; `shallow::unpack` and a plan base's
/// `prerequisite_commits` check a subset): every line is `<oid>
/// refs/carry-export/<suffix>`, and a `union/v1/` suffix is a canonical tail.
/// It ran before that reader fetched or wrote anything. The count of refs it
/// would import, or its refusal. Frozen on purpose: it is what a pre-change
/// build in the field runs, whatever this tree's readers become.
fn read_before_the_table(heads: &str) -> Result<usize, BulkloadRefusal> {
    let mut count = 0usize;
    for line in heads.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let suffix = name
            .strip_prefix("refs/carry-export/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !super::oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if suffix
            .strip_prefix("union/v1/")
            .is_some_and(|tail| !canonical_tail(tail))
        {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        count += 1;
    }
    Ok(count)
}

/// Old readers refuse the new format (#178 review). A build before the ref
/// table imported a new-format capture's tip refs as if they were source
/// refs (`.../ref-tip-v1/<oid>`, none of the source's names) and reported
/// success. The table ref now sits outside `refs/carry-export/`, so the
/// pre-table rule refuses every kind of new-format capture before it writes
/// anything: self-contained, a plan base, a chained link, and a shallow
/// envelope's inner inventory. The same rule still reads the old binary's
/// own capture, and this tree's reader imports the new format exactly.
#[test]
fn a_reader_before_the_table_refuses_a_new_format_capture() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1-old-format");
    let work = scratch("pre-table-reader");
    let listed = |bundle: &Path| g(&work.0, &["bundle", "list-heads", bundle.to_str().unwrap()]);
    assert_eq!(
        read_before_the_table(&listed(&fixtures.join("capture.bundle"))),
        Ok(23)
    );
    let shape = Shape {
        commits: 6,
        annotated: 2,
        native: 4,
        namespaces: 2,
        loose_per_mille: 300,
        shadowed: 1,
        pack_all: false,
        stash: true,
        sha256: false,
    };
    let source = generate("pre-table", &shape);
    let full = export_repository(&source.path, &work.0.join("full")).unwrap();
    check_exact(&source, &full, &work.0, "current-reader");
    let base = shared::export_base(&source.path, &work.0.join("base")).unwrap();
    fs::write(source.path.join("f2"), b"after the first capture\n").unwrap();
    g(&source.path, &["commit", "--quiet", "-am", "later"]);
    let chained = export_repository_with_drift(
        &source.path,
        &work.0.join("chained"),
        &ExportOptions {
            chain: Some(&full),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert!(chained.chained);
    let table = format!(" {}", ref_table::TABLE_REF);
    for (kind, bundle) in [
        ("self-contained", full.as_path()),
        ("plan base", base.as_path()),
        ("chained link", chained.bundle.as_path()),
    ] {
        let heads = listed(bundle);
        assert!(heads.lines().any(|line| line.ends_with(&table)), "{kind}");
        assert_eq!(
            read_before_the_table(&heads),
            Err(BulkloadRefusal::GitInventoryMalformed),
            "{kind}"
        );
    }
    // A shallow capture: the envelope's own header is the custody ref, and
    // its inner inventory, which the old `shallow::unpack` checked, names
    // the table.
    let shallow = work.0.join("shallow");
    g(
        &work.0,
        &[
            "clone",
            "--quiet",
            "--depth=1",
            "--no-local",
            &format!("file://{}", source.path.display()),
            shallow.to_str().unwrap(),
        ],
    );
    g(
        &shallow,
        &[
            "config",
            "remote.origin.url",
            "https://example.test/shallow.git",
        ],
    );
    let envelope = export_repository(&shallow, &work.0.join("shallow-capture")).unwrap();
    let reader = fresh_repository(&work.0, "envelope-reader", false);
    let inner = super::shallow::headers(&reader, &envelope).unwrap();
    assert!(inner.lines().any(|line| line.ends_with(&table)));
    assert_eq!(
        read_before_the_table(&inner),
        Err(BulkloadRefusal::GitInventoryMalformed)
    );
}
