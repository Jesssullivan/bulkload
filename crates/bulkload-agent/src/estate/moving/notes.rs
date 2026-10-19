//! `estate-move`'s notes phase (2026-10-19; operator: "NOTHING is EVER
//! excluded", "no intermidiary stage patterns").
//!
//! `estate-move-source` notes what is under a `SOURCE_ROOT` and is no Git
//! checkout ([`super::Note`]). Before this phase `estate-move` only counted
//! the notes (`not_moved=`) and failed its gate on them; tonight's passes
//! noted about 9,600 entries that no verb moved. This module reads the
//! source half's notes file back and decides, per note, without writing:
//!
//! - **carried** through the existing transport (`pull` from HOST with
//!   `--beside .LABEL`, or `copy` on this host), to the live path the source
//!   half mapped it to by the same `--map` rules as items: a `not-git-file`
//!   as one `--file` session, a `symlink` as one `--file` session of the
//!   link itself (its target bytes, never followed), a `not-git-tree` as one
//!   tree session, and a `prunable` worktree or a directory its repository
//!   does not list (`not-in-census`) as one tree session of its contents
//!   (custody as files: the worktree is never registered again);
//! - **covered**: a note at or under a planned item's source checkout (the
//!   item carries it) or under another carried tree (that tree carries it)
//!   is not carried twice;
//! - **held**, typed, pending review: a nested repository held as custody
//!   only (`GIT_NEST_CUSTODY_ONLY`), an unreadable entry (its code), a
//!   vanished root or a prunable worktree whose directory is gone
//!   (`SOURCE_ROOT_MISSING`);
//! - **unaccounted**, which no review closes: a line whose path does not
//!   decode, a note from a source half before this change (`not-git`: a
//!   directory and a count of plain files directly in it, which may hold
//!   repositories too, so it cannot be carried whole; rerun the source
//!   half), and a tree that holds a planned item (never carried twice).
//!
//! A note no transfer accounts for is one closure row
//! ([`crate::closure::RowKind::SourceNote`]); the rows of a pass are kept in
//! `MOVE_STATE/notes.rows` ([`write_rows`]) so `closure-dispose
//! --source-notes` can review one by its identity and instance.

use std::collections::BTreeMap;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::closure::{Disposition, NoteDetail, NoteRow};
use crate::estate::Item;
use crate::{BulkloadRefusal, Result};

/// One plain file directly in a directory that holds a repository.
pub const FILE: &str = "not-git-file";
/// The outermost directory of a tree that holds no repository.
pub const TREE: &str = "not-git-tree";
/// A symlink directly in a directory that holds a repository.
pub const LINK: &str = "symlink";
/// Note words that say too little to carry, from a source half before
/// 2026-10-19.
///
/// `not-git` named a directory and the count of the plain files directly in
/// it, and that directory may hold repositories too.
pub const LEGACY: &[&str] = &["not-git"];

/// `bytes` escaped for a notes line: lossless, printable ASCII only, never
/// a tab or a newline (`u8::escape_ascii`; [`unescape`] reads it back).
#[must_use]
pub fn escape(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string()
}

/// [`escape`] read back: `None` for an escape it never writes (an older
/// line's `\u{..}`, a dangling backslash).
#[must_use]
pub fn unescape(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte != b'\\' {
            out.push(byte);
            at += 1;
            continue;
        }
        let (value, width) = match *bytes.get(at + 1)? {
            next @ (b'\\' | b'\'' | b'"') => (next, 2),
            b'n' => (b'\n', 2),
            b't' => (b'\t', 2),
            b'r' => (b'\r', 2),
            b'x' => {
                let hex = std::str::from_utf8(bytes.get(at + 2..at + 4)?).ok()?;
                (u8::from_str_radix(hex, 16).ok()?, 4)
            }
            _ => return None,
        };
        out.push(value);
        at += width;
    }
    Some(out)
}

fn path_of(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

/// One line of `move-source.notes`, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub what: String,
    pub count: u64,
    pub code: Option<String>,
    /// The note's source path; `None` when the line's path does not decode.
    pub path: Option<PathBuf>,
    /// The live path the source half mapped it to; `None` on a line from a
    /// source half before 2026-10-19.
    pub live: Option<PathBuf>,
    /// The line as written: the identity of one whose path does not decode.
    pub text: String,
}

/// Every non-empty line of a notes file ([`super::render_notes`]), in
/// order. A line is kept whatever it holds, so nothing in it is dropped
/// unaccounted.
#[must_use]
pub fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut words = line.splitn(4, ' ');
            let what = words.next().unwrap_or_default();
            let count = words.next().and_then(|count| count.parse().ok());
            let code = words.next().filter(|code| *code != "none");
            let rest = words.next();
            let (path, live) = match rest.map(|rest| rest.split_once('\t')) {
                Some(Some((path, live))) => (Some(path), Some(live)),
                Some(None) => (rest, None),
                None => (None, None),
            };
            Line {
                what: what.to_owned(),
                count: count.unwrap_or(1),
                code: code.map(str::to_owned),
                path: path.and_then(unescape).map(path_of),
                live: live.and_then(unescape).map(path_of),
                text: line.to_owned(),
            }
        })
        .collect()
}

/// A note's identity: what a review names and what its transfer state is
/// named by (64 lowercase hex).
#[must_use]
pub fn id(line: &Line) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload.source-note.v1\0");
    hasher.update(line.what.as_bytes());
    hasher.update(&[0]);
    if let Some(path) = &line.path {
        hasher.update(&[1]);
        hasher.update(path.as_os_str().as_bytes());
    } else {
        hasher.update(&[2]);
        hasher.update(line.text.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// How a carried note's transfer takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `--file`: one regular file.
    File,
    /// `--file`: one symlink, the link itself.
    Link,
    /// A tree session of the directory.
    Tree,
}

/// What the notes phase does with a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Carried through the transport to `live` (or beside it).
    Carry { shape: Shape, live: PathBuf },
    /// Accounted by something else: `covered-by-item`, `covered-by-tree`.
    Covered(&'static str),
    /// Not carried, with this code: pending review when it is typed.
    Held(String),
    /// Not carried, and no review closes it: the reason.
    Unaccounted(&'static str),
}

impl Plan {
    /// The plan's word on an `estate-move note=` line.
    #[must_use]
    pub const fn word(&self) -> &str {
        match self {
            Self::Carry {
                shape: Shape::File, ..
            } => "carry-file",
            Self::Carry {
                shape: Shape::Link, ..
            } => "carry-link",
            Self::Carry {
                shape: Shape::Tree, ..
            } => "carry-tree",
            Self::Covered(why) | Self::Unaccounted(why) => why,
            Self::Held(_) => "held",
        }
    }
}

/// One note and what the phase does with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    pub line: Line,
    pub id: String,
    pub plan: Plan,
}

// The plan a note's own line gives, before the plan's items are read.
fn first_plan(line: &Line) -> Plan {
    let carried = |shape| {
        line.live
            .clone()
            .map_or(Plan::Unaccounted("note-legacy"), |live| Plan::Carry {
                shape,
                live,
            })
    };
    if line.path.is_none() {
        return Plan::Unaccounted("note-path-unreadable");
    }
    match line.what.as_str() {
        FILE => carried(Shape::File),
        LINK => carried(Shape::Link),
        // A prunable worktree whose directory is gone has nothing to carry.
        "prunable" if line.code.is_some() => {
            Plan::Held(line.code.clone().unwrap_or_default())
        }
        TREE | "prunable" | "not-in-census" => carried(Shape::Tree),
        "nested" => Plan::Held(BulkloadRefusal::GitNestCustodyOnly.code().to_owned()),
        "unreadable" => Plan::Held(line.code.clone().unwrap_or_else(|| "IO".to_owned())),
        "source-vanished" => Plan::Held(
            line.code
                .clone()
                .unwrap_or_else(|| BulkloadRefusal::SourceRootMissing.code().to_owned()),
        ),
        word if LEGACY.contains(&word) => Plan::Unaccounted("note-legacy"),
        _ => Plan::Unaccounted("note-unknown"),
    }
}

/// Decide every note of `lines` against the plan's `items` (pure; see the
/// module docs). Two lines naming one note (its kind and path) are one
/// note, their counts summed.
#[must_use]
pub fn decide(lines: &[Line], items: &[Item]) -> Vec<Decided> {
    let mut decided: Vec<Decided> = Vec::with_capacity(lines.len());
    let mut by_id: BTreeMap<String, usize> = BTreeMap::new();
    for line in lines {
        let id = id(line);
        if let Some(earlier) = by_id.get(&id).and_then(|at| decided.get_mut(*at)) {
            earlier.line.count = earlier.line.count.saturating_add(line.count);
            continue;
        }
        by_id.insert(id.clone(), decided.len());
        decided.push(Decided {
            line: line.clone(),
            id,
            plan: first_plan(line),
        });
    }
    // An item's checkout carries what is under it; a tree that holds an
    // item is never carried whole (the item would land twice).
    for note in &mut decided {
        let (Plan::Carry { shape, .. }, Some(path)) = (&note.plan, &note.line.path) else {
            continue;
        };
        if items.iter().any(|item| path.starts_with(&item.source)) {
            note.plan = Plan::Covered("covered-by-item");
        } else if *shape == Shape::Tree
            && items
                .iter()
                .any(|item| item.source.starts_with(path) && item.source != *path)
        {
            note.plan = Plan::Unaccounted("note-holds-item");
        }
    }
    // A carried tree carries what is under it: the first of two notes of one
    // path carries it.
    let trees: Vec<(usize, PathBuf)> = decided
        .iter()
        .enumerate()
        .filter_map(|(at, note)| match (&note.plan, &note.line.path) {
            (
                Plan::Carry {
                    shape: Shape::Tree, ..
                },
                Some(path),
            ) => Some((at, path.clone())),
            _ => None,
        })
        .collect();
    for (at, note) in decided.iter_mut().enumerate() {
        let (Plan::Carry { .. }, Some(path)) = (&note.plan, &note.line.path) else {
            continue;
        };
        if trees.iter().any(|(tree_at, tree)| {
            *tree_at != at && path.starts_with(tree) && (path != tree || *tree_at < at)
        }) {
            note.plan = Plan::Covered("covered-by-tree");
        }
    }
    decided
}

/// Where a carried note lands, and whether that is beside its live path.
///
/// Its live path, or `<live>SUFFIX` when what the live path holds cannot
/// take it as it is (R2.2: nothing is ever overwritten). A tree lands into a live
/// directory holding no repository (a lossless union: equal files adopted,
/// differing ones beside, nothing removed) and beside anything else there
/// (a Git checkout, a file, a symlink, which a tree session never follows).
/// A file or a link lands at a live path that is absent or not a directory
/// (the session adopts an equal one and lands a differing one beside), and
/// beside a directory. Reads the live path only.
#[must_use]
pub fn landing(shape: Shape, live: &Path, suffix: &str) -> (PathBuf, bool) {
    let beside = || (crate::git_carry::beside::path(live, suffix), true);
    let Ok(meta) = std::fs::symlink_metadata(live) else {
        return (live.to_path_buf(), false);
    };
    let fits = match shape {
        Shape::Tree => meta.is_dir() && super::presence(live) != super::Presence::Git,
        Shape::File | Shape::Link => !meta.is_dir(),
    };
    if fits {
        (live.to_path_buf(), false)
    } else {
        beside()
    }
}

/// A note no transfer of the pass accounts for, as kept in
/// `MOVE_STATE/notes.rows`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowRecord {
    pub item: String,
    pub what: String,
    pub path: Vec<u8>,
    pub count: u64,
    /// `covered-by-item`, `covered-by-tree`, `covered-in-place` (on this
    /// host, its live path is its own path), `held`, `refused`, the carry
    /// it was planned (a pass that stopped before it) or the reason it is
    /// unaccounted.
    pub outcome: String,
    /// The hold's code (`held`, `refused`).
    pub code: Option<String>,
}

impl RowRecord {
    /// The record of `note`, settled by its plan, or by a carry refused
    /// before its transfer ran (`refused`, with the refusal's code).
    #[must_use]
    pub fn of(note: &Decided, refused: Option<&BulkloadRefusal>) -> Self {
        let (outcome, code) = match (refused, &note.plan) {
            (Some(refusal), _) => ("refused".to_owned(), Some(refusal.code().to_owned())),
            (None, Plan::Held(code)) => ("held".to_owned(), Some(code.clone())),
            (None, plan) => (plan.word().to_owned(), None),
        };
        Self::settled(note, outcome, code)
    }

    /// The record of `note` settled as `outcome` (with `code`).
    #[must_use]
    pub fn settled(note: &Decided, outcome: String, code: Option<String>) -> Self {
        Self {
            item: note.id.clone(),
            what: note.line.what.clone(),
            path: note.line.path.as_ref().map_or_else(
                || note.line.text.as_bytes().to_vec(),
                |path| path.as_os_str().as_bytes().to_vec(),
            ),
            count: note.line.count,
            outcome,
            code,
        }
    }

    /// The refusal instance a review of this row names: the note, how it
    /// was settled and with which code, and the entries it stands for.
    #[must_use]
    pub fn instance(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"bulkload.source-note-instance.v1\0");
        for part in [
            self.item.as_bytes(),
            self.outcome.as_bytes(),
            self.code.as_deref().unwrap_or("").as_bytes(),
        ] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        hasher.update(&self.count.to_le_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// The closure row: `covered`; a typed hold pending review; anything
    /// else unaccounted.
    #[must_use]
    pub fn row(&self) -> NoteRow {
        let disposition = match (self.outcome.as_str(), &self.code) {
            (covered, _) if covered.starts_with("covered-") => Disposition::Covered,
            // A carry the pass never reached (its phase stopped first).
            (carry, _) if carry.starts_with("carry-") => {
                Disposition::Unaccounted("note-not-carried")
            }
            ("held" | "refused", Some(code)) if crate::outcome::is_typed_code(code) => {
                Disposition::RefusedPendingReview(code.clone())
            }
            ("held" | "refused", _) => Disposition::Unaccounted("note-hold-untyped"),
            (other, _) => Disposition::Unaccounted(reason(other)),
        };
        NoteRow {
            item: self.item.clone(),
            instance: self.instance(),
            disposition,
            detail: NoteDetail {
                what: self.what.clone(),
                path: self.path.clone(),
                count: self.count,
                outcome: self.outcome.clone(),
            },
        }
    }
}

// An unaccounted reason as the closure row keeps it.
fn reason(word: &str) -> &'static str {
    const REASONS: &[&str] = &[
        "note-path-unreadable",
        "note-legacy",
        "note-unknown",
        "note-holds-item",
        "note-hold-untyped",
        "note-not-carried",
    ];
    REASONS
        .iter()
        .find(|known| **known == word)
        .copied()
        .unwrap_or("note-unaccounted")
}

/// The schema of `MOVE_STATE/notes.rows`.
const ROWS_SCHEMA: &str = "bulkload.source-note-rows.v1";

#[derive(Serialize, Deserialize)]
struct RowsFile {
    schema: String,
    rows: Vec<RowRecord>,
}

/// Write the pass's note rows at `path` (temporary, fsync, rename).
///
/// # Errors
/// IO; `FRAME_CODEC` for rows postcard cannot encode.
pub fn write_rows(path: &Path, rows: &[RowRecord]) -> Result<()> {
    crate::estate::write(
        path,
        &RowsFile {
            schema: ROWS_SCHEMA.to_owned(),
            rows: rows.to_vec(),
        },
    )
}

/// The note rows [`write_rows`] wrote at `path`.
///
/// # Errors
/// `SCHEMA_MISMATCH` for another file; the read's refusals.
pub fn read_rows(path: &Path) -> Result<Vec<RowRecord>> {
    let file: RowsFile = crate::estate::read(path)?;
    if file.schema != ROWS_SCHEMA {
        return Err(BulkloadRefusal::SchemaMismatch);
    }
    Ok(file.rows)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    fn line(what: &str, path: &str, live: Option<&str>) -> Line {
        let text = live.map_or_else(
            || format!("{what} 1 none {}", escape(path.as_bytes())),
            |live| {
                format!(
                    "{what} 1 none {}\t{}",
                    escape(path.as_bytes()),
                    escape(live.as_bytes())
                )
            },
        );
        parse(&text).pop().unwrap()
    }

    fn item(source: &str) -> Item {
        Item {
            source: PathBuf::from(source),
            repository: PathBuf::from(source),
            workspace: Some(PathBuf::from(source)),
        }
    }

    #[test]
    fn a_path_round_trips_whatever_its_bytes() {
        let raw: Vec<u8> = b"/src/git/a b\t\n\\\"'\xff\xfe\x01\x7f/z".to_vec();
        let escaped = escape(&raw);
        assert!(!escaped.contains('\t') && !escaped.contains('\n'));
        assert_eq!(unescape(&escaped).unwrap(), raw);
        assert_eq!(unescape("bad\\u{1f600}"), None);
        assert_eq!(unescape("dangling\\"), None);
        let note = super::super::Note {
            path: path_of(raw.clone()),
            what: FILE,
            count: 1,
            code: None,
        };
        let maps = vec![(PathBuf::from("/src/git"), PathBuf::from("/dst/git"))];
        let lines = parse(&super::super::render_notes(&[note], &maps));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].path.as_ref().unwrap().as_os_str().as_bytes(), raw);
        let mut live = b"/dst/git".to_vec();
        live.extend_from_slice(raw.strip_prefix(b"/src/git".as_slice()).unwrap());
        assert_eq!(lines[0].live.as_ref().unwrap().as_os_str().as_bytes(), live);
        // The counting reader still reads the new lines.
        assert_eq!(
            super::super::note_counts(&super::super::render_notes(
                &[super::super::Note {
                    path: PathBuf::from("/x"),
                    what: TREE,
                    count: 7,
                    code: None,
                }],
                &[]
            ))
            .get(TREE),
            Some(&7)
        );
    }

    #[test]
    fn each_kind_of_note_gets_its_plan() {
        let items = [item("/src/git/lab")];
        let lines = vec![
            line(FILE, "/src/git/receipt.txt", Some("/dst/git/receipt.txt")),
            line(LINK, "/src/git/link", Some("/dst/git/link")),
            line(TREE, "/src/git/archive", Some("/dst/git/archive")),
            line("prunable", "/src/git/kept-wt", Some("/dst/git/kept-wt")),
            line(
                "not-in-census",
                "/src/git/moved-wt",
                Some("/dst/git/moved-wt"),
            ),
            line("nested", "/src/git/lab/vendor/inner", None),
            line("unreadable", "/src/git/locked", None),
            line("source-vanished", "/src/gone", None),
            // Under an item's checkout: the item carries it.
            line(
                FILE,
                "/src/git/lab/stray.txt",
                Some("/dst/git/lab/stray.txt"),
            ),
            // Under a carried tree: the tree carries it.
            line(
                LINK,
                "/src/git/archive/inner-link",
                Some("/dst/git/archive/inner-link"),
            ),
            // A tree that holds a planned item is never carried whole.
            line(TREE, "/src/git", Some("/dst/git")),
            // A prunable worktree whose directory is gone.
            parse("prunable 1 SOURCE_ROOT_MISSING /src/git/gone-wt\t/dst/git/gone-wt")
                .pop()
                .unwrap(),
            // Older source halves.
            line("not-git", "/src/git", None),
            line(FILE, "/src/git/old.txt", None),
            line("what-is-this", "/src/x", Some("/dst/x")),
        ];
        let plans: Vec<String> = decide(&lines, &items)
            .iter()
            .map(|note| match &note.plan {
                Plan::Held(code) => format!("held:{code}"),
                plan => plan.word().to_owned(),
            })
            .collect();
        assert_eq!(
            plans,
            [
                "carry-file",
                "carry-link",
                "carry-tree",
                "carry-tree",
                "carry-tree",
                "held:GIT_NEST_CUSTODY_ONLY",
                "held:IO",
                "held:SOURCE_ROOT_MISSING",
                "covered-by-item",
                "covered-by-tree",
                "note-holds-item",
                "held:SOURCE_ROOT_MISSING",
                "note-legacy",
                "note-legacy",
                "note-unknown",
            ]
        );
        // One note named twice is one note, its counts summed; a path that
        // does not decode keeps its line as its identity.
        let mut twice = line("unreadable", "/src/git/locked", None);
        twice.count = 2;
        let decided = decide(&[line("unreadable", "/src/git/locked", None), twice], &[]);
        assert_eq!(decided.len(), 1);
        assert_eq!(decided[0].line.count, 3);
        let broken = parse("symlink 1 none bad\\u{1}\t/dst/x").pop().unwrap();
        assert_eq!(broken.path, None);
        assert_eq!(
            decide(&[broken], &[])[0].plan,
            Plan::Unaccounted("note-path-unreadable")
        );
    }

    #[test]
    fn a_held_note_is_typed_and_its_review_binds_its_instance() {
        let decided = decide(&[line("nested", "/src/git/lab/inner", None)], &[]);
        let record = RowRecord::of(&decided[0], None);
        let row = record.row();
        assert_eq!(
            row.disposition,
            Disposition::RefusedPendingReview("GIT_NEST_CUSTODY_ONLY".to_owned())
        );
        assert_eq!(row.item.len(), 64);
        assert_eq!(row.instance.len(), 64);
        // Another count or another code is another instance.
        let mut more = record.clone();
        more.count = 2;
        assert_ne!(more.instance(), record.instance());
        let refused = RowRecord::of(
            &decided[0],
            Some(&BulkloadRefusal::GitDestinationParentMissing),
        );
        assert_eq!(refused.outcome, "refused");
        assert_ne!(refused.instance(), record.instance());
        // An untyped hold, a covered note and a legacy note.
        let untyped = RowRecord::of(&decide(&[line("unreadable", "/x", None)], &[])[0], None);
        assert_eq!(
            untyped.row().disposition,
            Disposition::Unaccounted("note-hold-untyped")
        );
        let covered = decide(
            &[line(FILE, "/src/git/lab/f", Some("/dst/git/lab/f"))],
            &[item("/src/git/lab")],
        );
        assert_eq!(
            RowRecord::of(&covered[0], None).row().disposition,
            Disposition::Covered
        );
        let legacy = RowRecord::of(&decide(&[line("not-git", "/src/git", None)], &[])[0], None);
        assert_eq!(
            legacy.row().disposition,
            Disposition::Unaccounted("note-legacy")
        );
        // The rows file round-trips and is typed.
        let base = std::env::temp_dir().join(format!("bulkload-notes-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("notes.rows");
        write_rows(&path, &[record.clone(), untyped]).unwrap();
        assert_eq!(read_rows(&path).unwrap()[0], record);
        std::fs::write(base.join("other"), b"\x00\x01").unwrap();
        assert!(read_rows(&base.join("other")).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_carried_note_lands_live_or_beside() {
        let base =
            std::env::temp_dir().join(format!("bulkload-notes-landing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("plain")).unwrap();
        std::fs::create_dir_all(base.join("repo/.git")).unwrap();
        std::fs::write(base.join("file"), b"x").unwrap();
        std::os::unix::fs::symlink(base.join("plain"), base.join("link")).unwrap();
        let base = std::fs::canonicalize(&base).unwrap();
        let at = |shape, name: &str| {
            let (path, beside) = landing(shape, &base.join(name), ".L");
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                beside,
            )
        };
        assert_eq!(at(Shape::Tree, "absent"), ("absent".to_owned(), false));
        assert_eq!(at(Shape::Tree, "plain"), ("plain".to_owned(), false));
        assert_eq!(at(Shape::Tree, "repo"), ("repo.L".to_owned(), true));
        assert_eq!(at(Shape::Tree, "file"), ("file.L".to_owned(), true));
        assert_eq!(at(Shape::Tree, "link"), ("link.L".to_owned(), true));
        assert_eq!(at(Shape::File, "file"), ("file".to_owned(), false));
        assert_eq!(at(Shape::File, "plain"), ("plain.L".to_owned(), true));
        assert_eq!(at(Shape::Link, "link"), ("link".to_owned(), false));
        std::fs::remove_dir_all(&base).unwrap();
    }
}
