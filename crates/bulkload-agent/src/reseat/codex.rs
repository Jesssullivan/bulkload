//! Codex's half of the re-seat (R0, TIN-6587).
//!
//! The landed `state_5.sqlite` is the run's backup-API snapshot of the
//! source's thread index; it is opened read-only and immutable, and only
//! when its identity changed since the ledger last read it (its threads'
//! identity columns are cached in the ledger, S3). Per thread row, oldest
//! first (a fork's parent and a spawned thread's parent are older than it,
//! so they are registered first):
//!
//! 1. A row the destination's index already holds for the thread that this
//!    ledger did not register is never replaced: the same `cwd` and rollout
//!    path is adopted, anything else is `RESEAT_INDEX_CONFLICT`, and no
//!    second rollout is placed beside the destination's own. One change is
//!    ruled (OI-1003-Q198 (1)): a row whose `cwd` is a non-canonical
//!    spelling of the lane (sting's `/home/jess/git/…` through the `~/git`
//!    symlink, which the cwd-filtered picker never matches) gets **only its
//!    `cwd` column** re-spelled to the canonical path, in a `BEGIN
//!    IMMEDIATE` transaction, only while it still holds the old value, with
//!    the old value ledgered first; `--undo` puts it back the same way.
//!    A row that names this thread's rollout in another form or place
//!    (Codex's `.jsonl.zst`, a `.jsonl.gz`, the same file name in another
//!    directory, or the plain file while a compressed form sits beside it)
//!    is the destination holding the thread: nothing is placed, and it is
//!    `destination-adopted` when the file the row names holds the landed
//!    stream's whole lines (exactly or continued, the R0 check's rule),
//!    `destination-holds` when a destination file is there unproved, and
//!    `destination-row-only` when only the row is; none is a refusal
//!    (operator M2 round 2, 2026-10-17). A compressed form with no row is
//!    `destination-holds` (`rowless`).
//! 2. Its rollout goes to the same place under the live destination
//!    `.codex` (`sessions/YYYY/MM/DD/…` or `archived_sessions/…`) **as plain
//!    `.jsonl`**: a landed `.jsonl.zst` is decompressed. Codex resumes a
//!    paginated thread only from the plain file (it stats `<name>.jsonl`
//!    and never opens the `.zst`), and one destination form keeps a thread
//!    one file when the source compresses a rollout between passes. The
//!    identity is the decompressed stream. A `.jsonl.gz` is kept in the
//!    landing (`RESEAT_UNREADABLE_FORM`): Codex cannot read it, and so is a
//!    truncated or corrupt `.zst` (that file only).
//! 3. Its row is inserted into the destination's `state_5.sqlite`
//!    (`BEGIN IMMEDIATE`, a 30 s busy timeout, beside a running app-server)
//!    with two changes: `cwd` is the lane's destination working directory
//!    (the picker matches it exactly) and `rollout_path` the rollout's real
//!    destination path; the source's project and section links are cleared
//!    (they name rows of the source's own tables). Its `thread_dynamic_tools`
//!    and `thread_spawn_edges` rows go with it. Rows commit only after their
//!    rollouts are durable.
//!
//! A landed rollout with no row in the snapshot (a thread created between
//! the snapshot and the rollout copy, or one Codex never indexed) follows
//! the same destination rule (a destination row, or another form of its
//! rollout there, is the destination's: `RESEAT_INDEX_CONFLICT`, nothing
//! placed). Otherwise it is placed and gets a row built from its header
//! (OI-1003-Q198 D): `session_meta` (id, time, `source`, provider, version,
//! originator, history mode, agent fields), the first `turn_context`
//! (approval, sandbox, model, effort) and the first user prompt (title and
//! preview, which the picker needs), with the lane's `cwd`. A header that
//! cannot fill a column the destination requires leaves it row-less,
//! `RESEAT_INDEX_ROW_ABSENT`. Its lane is its `session_meta` `cwd`.
//!
//! Before any thread, every destination row whose `cwd` is spelled through
//! the destination home's `git` symlink or a `--map` destination that is a
//! symlink (sting's `/home/jess/git/…`) gets its `cwd` re-spelled to the
//! real path the same ledgered, reversible way, whether or not a landed
//! thread maps to it (and with nothing landed, [`sweep_alone`]): only when the nearest existing level of the old
//! spelling resolves to the same level of the new one, so a row that
//! reaches a different directory through another link is never touched
//! (operator M2 round 2, 2026-10-17, extending OI-1003-Q198 (1)).
//!
//! A thread whose rollout is recorded outside every `--codex-source-root`
//! (or whose landed rollout is absent) is `destination-row-only` when the
//! destination's index has a row for it, else `RESEAT_LANE_UNMAPPED`
//! (`rollout-root`) as before.
//!
//! The destination must already have an index (`RESEAT_INDEX_ABSENT`
//! otherwise, before anything is written): Codex's first start backfills a
//! fresh index from the rollouts it finds once, under their recorded
//! source `cwd`, archived, and never again.
//!
//! `thread_history_1.sqlite` is a projection Codex rebuilds from rollouts and
//! is not carried; `session_index.jsonl` and `history.jsonl` stay in the
//! landing (the picker reads neither).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension as _};

use super::ledger::{Identity, Ledger, Session};
use super::place::{self, Outcome, Placer};
use super::{Kind, Lanes, Options, Summary, SCHEMA};
use crate::closure::json_string;
use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The thread index file.
pub const STATE_DB: &str = "state_5.sqlite";
/// The tool name in ledger rows and report lines.
pub const TOOL: &str = "codex";
/// Threads registered per destination transaction.
const BATCH: usize = 256;
/// Columns that link a row to the source's own tables: cleared.
const SOURCE_LINKS: &[&str] = &[
    "project_id",
    "thread_section_id",
    "section_position",
    "section_entered_at_ms",
];
/// The rollout trees under `.codex`.
pub const TREES: [&str; 2] = ["sessions", "archived_sessions"];
/// The longest `session_meta` line read for an unindexed rollout's `cwd`.
const META_LIMIT: u64 = 8 * 1024 * 1024;

fn sql<T>(outcome: rusqlite::Result<T>) -> Result<T> {
    outcome.map_err(|_| BulkloadRefusal::SqliteStateChanged)
}

/// Open the landed snapshot read-only and immutable: nothing is written
/// beside it, not even a `-shm`. Its size counts as landed bytes read.
///
/// # Errors
/// `PATH_NOT_PORTABLE` for a non-UTF-8 path; `SQLITE_STATE_CHANGED` when
/// it cannot be opened.
pub fn open_snapshot(path: &Path) -> Result<Connection> {
    let text = super::key::utf8(path)?;
    let mut uri = String::from("file:");
    for character in text.chars() {
        match character {
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            '%' => uri.push_str("%25"),
            other => uri.push(other),
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    let conn = sql(Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ))?;
    // Reading it: its pages are landed bytes (S3 counts them).
    sql(
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        }),
    )?;
    if let Ok(meta) = std::fs::metadata(path) {
        counters::add(Counter::ReseatLandingRead, meta.len());
    }
    Ok(conn)
}

/// A table's columns: `(name, not null without a default)`.
fn columns(conn: &Connection, table: &str) -> Result<Vec<(String, bool)>> {
    let mut statement = sql(conn.prepare(&format!("PRAGMA table_info(\"{table}\")")))?;
    let rows = sql(statement.query_map([], |row| {
        let not_null: i64 = row.get(3)?;
        let default: Option<String> = row.get(4)?;
        let primary: i64 = row.get(5)?;
        Ok((
            row.get::<_, String>(1)?,
            not_null != 0 && default.is_none() && primary == 0,
        ))
    }))?;
    sql(rows.collect::<rusqlite::Result<Vec<_>>>())
}

/// The columns both tables hold, refusing a destination column the
/// source cannot fill (`SCHEMA_MISMATCH`).
fn shared(source: &[(String, bool)], dest: &[(String, bool)]) -> Result<Vec<String>> {
    let names: BTreeSet<&str> = source.iter().map(|(name, _)| name.as_str()).collect();
    if dest
        .iter()
        .any(|(name, required)| *required && !names.contains(name.as_str()))
    {
        return Err(BulkloadRefusal::SchemaMismatch);
    }
    Ok(dest
        .iter()
        .filter(|(name, _)| names.contains(name.as_str()))
        .map(|(name, _)| name.clone())
        .collect())
}

fn quoted(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",")
}

/// One landed thread: its identity columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: String,
    pub rollout: String,
    pub cwd: String,
}

/// Every thread row of the landed snapshot, oldest first (identity
/// columns only).
fn read_threads(source: &Connection) -> Result<Vec<Thread>> {
    let mut statement =
        sql(source.prepare("SELECT id, rollout_path, cwd FROM threads ORDER BY created_at, id"))?;
    let rows = sql(statement.query_map([], |row| {
        Ok(Thread {
            id: row.get(0)?,
            rollout: row.get(1)?,
            cwd: row.get(2)?,
        })
    }))?;
    sql(rows.collect::<rusqlite::Result<Vec<_>>>())
}

/// Every landed thread row, oldest first, for `--check` (read-only: no
/// cache is written). An absent snapshot has none.
///
/// # Errors
/// `SQLITE_STATE_CHANGED` when the snapshot cannot be read.
pub fn landed_threads(path: &Path) -> Result<Vec<Thread>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    read_threads(&open_snapshot(path)?)
}

/// The landed snapshot, opened only when a pass needs it.
struct Snapshot {
    path: PathBuf,
    conn: Option<Connection>,
}

impl Snapshot {
    fn conn(&mut self) -> Result<&Connection> {
        if self.conn.is_none() {
            self.conn = Some(open_snapshot(&self.path)?);
        }
        self.conn
            .as_ref()
            .ok_or(BulkloadRefusal::ContractSelfInconsistent)
    }

    /// The snapshot's threads: from the ledger's cache while the
    /// snapshot's identity is unchanged (nothing read, S3), else read and
    /// cached.
    fn threads(&mut self, ledger: &mut Ledger) -> Result<Vec<Thread>> {
        let Ok(meta) = std::fs::symlink_metadata(&self.path) else {
            return Ok(Vec::new());
        };
        let identity = Identity::of(&meta);
        if let Some(cached) = ledger.snapshot_threads(&self.path, identity)? {
            return Ok(cached
                .into_iter()
                .map(|(id, rollout, cwd)| Thread { id, rollout, cwd })
                .collect());
        }
        let threads = read_threads(self.conn()?)?;
        let rows: Vec<(String, String, String)> = threads
            .iter()
            .map(|thread| {
                (
                    thread.id.clone(),
                    thread.rollout.clone(),
                    thread.cwd.clone(),
                )
            })
            .collect();
        ledger.put_snapshot_threads(&self.path, identity, &rows)?;
        Ok(threads)
    }
}

/// Where a recorded rollout path lands: the path relative to `.codex`,
/// when it is under one of the source roots.
#[must_use]
pub fn rollout_rel(options: &Options, rollout: &str) -> Option<PathBuf> {
    let rollout = Path::new(rollout);
    options.codex_source_roots.iter().find_map(|root| {
        rollout
            .strip_prefix(root)
            .ok()
            .filter(|rel| TREES.iter().any(|tree| rel.starts_with(tree)))
            .map(Path::to_path_buf)
    })
}

/// The landed form of `rel`, relative to the landed `.codex`: as recorded,
/// or compressed or decompressed since the snapshot (`.jsonl`,
/// `.jsonl.zst`, `.jsonl.gz`).
#[must_use]
pub fn landed_form(landing_codex: &Path, rel: &Path) -> Option<PathBuf> {
    let text = rel.to_str()?;
    let plain = text
        .strip_suffix(".zst")
        .or_else(|| text.strip_suffix(".gz"))
        .unwrap_or(text);
    let forms = [
        text.to_owned(),
        plain.to_owned(),
        format!("{plain}.zst"),
        format!("{plain}.gz"),
    ];
    forms
        .into_iter()
        .map(PathBuf::from)
        .find(|form| landing_codex.join(form).symlink_metadata().is_ok())
}

/// The destination form of a landed rollout: plain `.jsonl`.
#[must_use]
pub fn dest_form(landed: &Path) -> PathBuf {
    place::plain(landed)
}

/// Whether the landed `.codex` holds anything to re-seat (a snapshot or a
/// rollout tree).
///
/// # Errors
/// `RESEAT_LANDING_ABSENT` when the landed `.codex` is not a directory.
pub fn landed_anything(options: &Options) -> Result<bool> {
    if !std::fs::metadata(&options.landing_codex).is_ok_and(|meta| meta.is_dir()) {
        return Err(BulkloadRefusal::ReseatLandingAbsent);
    }
    Ok(options.landing_codex.join(STATE_DB).is_file()
        || TREES.iter().any(|tree| {
            std::fs::read_dir(options.landing_codex.join(tree))
                .is_ok_and(|mut entries| entries.next().is_some())
        }))
}

/// Refuse before anything is written when the destination has no index to
/// register landed threads in.
///
/// # Errors
/// `RESEAT_INDEX_ABSENT`.
pub fn preflight(options: &Options) -> Result<()> {
    if options.dest_codex.join(STATE_DB).is_file() {
        Ok(())
    } else {
        Err(BulkloadRefusal::ReseatIndexAbsent)
    }
}

/// The bytes a pass would write placing the landed rollouts (the
/// free-space check; an upper bound: every unindexed rollout counts).
///
/// # Errors
/// Snapshot, ledger and I/O refusals.
pub fn planned(options: &Options, lanes: &Lanes, ledger: Option<&Ledger>) -> Result<u64> {
    let Ok(root) = std::fs::canonicalize(&options.dest_codex) else {
        return Ok(0);
    };
    let path = options.landing_codex.join(STATE_DB);
    let cached = match (ledger, std::fs::symlink_metadata(&path)) {
        (Some(ledger), Ok(meta)) => ledger.snapshot_threads(&path, Identity::of(&meta))?,
        _ => None,
    };
    let threads = match cached {
        Some(rows) => rows
            .into_iter()
            .map(|(id, rollout, cwd)| Thread { id, rollout, cwd })
            .collect(),
        None => landed_threads(&path)?,
    };
    let mut named = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut total = 0_u64;
    for thread in &threads {
        ids.insert(thread.id.clone());
        let Some(rel) = rollout_rel(options, &thread.rollout)
            .and_then(|rel| landed_form(&options.landing_codex, &rel))
        else {
            continue;
        };
        named.insert(rel.clone());
        if lanes.lane(Path::new(&thread.cwd))?.is_none() && !options.by_id(&thread.cwd) {
            continue;
        }
        total = total.saturating_add(place::planned(
            ledger,
            &root.join(dest_form(&rel)),
            &options.landing_codex.join(&rel),
            &options.replace_archived,
        )?);
    }
    for file in unindexed(options, &named)? {
        let Some(id) = &file.id else {
            continue;
        };
        if ids.contains(id)
            || file
                .rel
                .extension()
                .is_some_and(|extension| extension == "gz")
        {
            continue;
        }
        total = total.saturating_add(place::planned(
            ledger,
            &root.join(dest_form(&file.rel)),
            &options.landing_codex.join(&file.rel),
            &options.replace_archived,
        )?);
    }
    Ok(total)
}

/// A thread waiting for its row.
struct Pending {
    thread: usize,
    cwd: String,
    rollout: PathBuf,
    outcome: Outcome,
}

/// One landed rollout file under a rollout tree, relative to `.codex`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unindexed {
    pub rel: PathBuf,
    pub id: Option<String>,
}

/// The thread ID in a rollout file name (`rollout-<ts>-<uuid>.jsonl…`).
#[must_use]
pub fn rollout_id(name: &str) -> Option<String> {
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl.gz"))
        .or_else(|| name.strip_suffix(".jsonl"))?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    let shape = id.char_indices().all(|(at, character)| {
        if matches!(at, 8 | 13 | 18 | 23) {
            character == '-'
        } else {
            character.is_ascii_hexdigit()
        }
    });
    shape.then(|| id.to_owned())
}

/// Every landed rollout-tree file no snapshot row's landed form names.
///
/// # Errors
/// The I/O refusal of a listing.
pub fn unindexed(options: &Options, named: &BTreeSet<PathBuf>) -> Result<Vec<Unindexed>> {
    let mut out = Vec::new();
    for tree in TREES {
        let base = options.landing_codex.join(tree);
        for rel in super::claude::walk(&base)? {
            let rel = Path::new(tree).join(rel);
            if named.contains(&rel) {
                continue;
            }
            let id = rel
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(rollout_id);
            out.push(Unindexed { rel, id });
        }
    }
    Ok(out)
}

/// The `cwd` in a landed rollout's `session_meta` line (its first line).
///
/// # Errors
/// I/O refusals; `RESEAT_UNREADABLE_FORM` for a stream that cannot be
/// decompressed.
pub fn meta_cwd(path: &Path) -> Result<Option<String>> {
    let landing = place::Landing::open(path)?;
    let reader = landing.reader()?;
    let mut line = Vec::new();
    for byte in std::io::BufReader::new(reader.take(META_LIMIT)).bytes() {
        let byte = byte.map_err(|error| {
            if place::compressed(path) {
                BulkloadRefusal::ReseatUnreadableForm
            } else {
                crate::refuse::io(&error, "reseat::codex::meta_cwd")
            }
        })?;
        if byte == b'\n' {
            break;
        }
        line.push(byte);
    }
    Ok(super::json::nested_string(&line, &["payload", "cwd"]))
}

/// The live destination index a pass writes.
struct Dest {
    root: PathBuf,
    db: PathBuf,
    conn: Connection,
}

/// Re-seat every landed Codex thread.
///
/// # Errors
/// `SCHEMA_MISMATCH` when the destination's index has a required column
/// the snapshot lacks; ledger, `SQLite` and I/O refusals. A thread's own
/// refusal is reported.
#[allow(clippy::too_many_lines)]
pub fn reseat(
    options: &Options,
    lanes: &Lanes,
    ledger: &mut Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let root = super::root(&options.dest_codex)?;
    let mut snapshot = Snapshot {
        path: options.landing_codex.join(STATE_DB),
        conn: None,
    };
    let db = root.join(STATE_DB);
    let conn = sql(Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ))?;
    sql(conn.busy_timeout(std::time::Duration::from_secs(30)))?;
    let mut dest = Dest { root, db, conn };
    settle_cwd_intents(&dest, ledger)?;
    sweep_spellings(options, ledger, &mut dest, summary, emit)?;
    let all = snapshot.threads(ledger)?;
    let mut named = BTreeSet::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut respells: Vec<(String, String, String)> = Vec::new();
    let mut columns_shared: Option<Vec<String>> = None;
    for (index, thread) in all.iter().enumerate() {
        let (rel, dest_cwd, in_lane) = match locate(options, lanes, thread)? {
            Ok(located) => located,
            Err((word, refusal, rel)) => {
                if let Some(rel) = &rel {
                    named.insert(rel.clone());
                }
                let unrooted = rel.is_none() && refusal == BulkloadRefusal::ReseatLaneUnmapped;
                let row = if unrooted || word == "landing-absent" {
                    row_of(&dest.conn, &thread.id)?.map(|(_, path)| path)
                } else {
                    None
                };
                if row.is_some() {
                    // The destination's index holds the thread; nothing of
                    // the landing ties to it here (operator M2 round 2).
                    let proof = if unrooted {
                        "rollout-root"
                    } else {
                        "landing-absent"
                    };
                    let dest_cwd = lane_cwd(options, lanes, &thread.cwd)?;
                    held(
                        summary,
                        emit,
                        (&thread.id, &thread.cwd),
                        dest_cwd.as_deref(),
                        Proved::RowOnly(proof),
                        row.as_deref().map(Path::new),
                    )?;
                    continue;
                }
                let counted = if unrooted {
                    format!("{}:rollout-root", refusal.code())
                } else if word == "refused" {
                    refusal.code().to_owned()
                } else {
                    word.to_owned()
                };
                summary.session(TOOL, &counted);
                emit(&thread_line(
                    (&thread.id, &thread.cwd),
                    None,
                    word,
                    Some(&refusal),
                    unrooted.then_some("rollout-root"),
                ))?;
                continue;
            }
        };
        named.insert(rel.clone());
        let dest_rel = dest_form(&rel);
        let rollout = dest.root.join(&dest_rel);
        if !ours(ledger, &dest, &thread.id, &dest_cwd, &rollout)? {
            // A row (or another form of the rollout) the destination holds
            // that this ledger did not register: adopted when it already
            // names this placement, its `cwd` re-spelled when that is the
            // only difference and only a spelling (OI-1003-Q198 (1)), and
            // nothing placed beside it otherwise.
            let claim = claim(
                options,
                ledger,
                &dest,
                &thread.id,
                (&dest_cwd, in_lane),
                &rollout,
            )?;
            if let Some(old) = claim.respell {
                respells.push((thread.id.clone(), old, dest_cwd.clone()));
            }
            if let Some(holding) = &claim.holds {
                let landed = options.landing_codex.join(&rel);
                let proved = prove(
                    ledger,
                    holding,
                    (&thread.id, &thread.cwd),
                    &landed,
                    &dest_cwd,
                )?;
                held(
                    summary,
                    emit,
                    (&thread.id, &thread.cwd),
                    Some(&dest_cwd),
                    proved,
                    holding.row(),
                )?;
                continue;
            }
            if !claim.placeable {
                summary.session(TOOL, "index-conflict");
                *summary.rows.entry("conflict".to_owned()).or_default() += 1;
                emit(&thread_line(
                    (&thread.id, &thread.cwd),
                    Some(&dest_cwd),
                    "kept",
                    Some(&BulkloadRefusal::ReseatIndexConflict),
                    Some("conflict"),
                ))?;
                continue;
            }
        }
        let landed = options.landing_codex.join(&rel);
        let placer = Placer::new(ledger, &dest.root).with_archived(&options.replace_archived);
        let outcome = placer
            .place(&landed, &dest_rel, Kind::Rollout, &thread.id, None)?
            .outcome;
        summary.file(TOOL, outcome);
        summary.session(TOOL, outcome.word());
        if !outcome.landed() {
            emit(&thread_line(
                (&thread.id, &thread.cwd),
                Some(&dest_cwd),
                outcome.word(),
                outcome.refusal().as_ref(),
                None,
            ))?;
            continue;
        }
        if ours(ledger, &dest, &thread.id, &dest_cwd, &rollout)? {
            *summary.rows.entry("unchanged".to_owned()).or_default() += 1;
            emit(&thread_line(
                (&thread.id, &thread.cwd),
                Some(&dest_cwd),
                outcome.word(),
                None,
                Some("unchanged"),
            ))?;
            continue;
        }
        pending.push(Pending {
            thread: index,
            cwd: dest_cwd,
            rollout,
            outcome,
        });
        if pending.len() >= BATCH {
            respell(&mut dest, ledger, &mut respells, summary, emit)?;
            let index = Index {
                snapshot: &mut snapshot,
                dest: &mut dest.conn,
                db: &dest.db,
                names: &mut columns_shared,
            };
            register(index, &all, &mut pending, ledger, summary, emit)?;
        }
    }
    respell(&mut dest, ledger, &mut respells, summary, emit)?;
    let index = Index {
        snapshot: &mut snapshot,
        dest: &mut dest.conn,
        db: &dest.db,
        names: &mut columns_shared,
    };
    register(index, &all, &mut pending, ledger, summary, emit)?;
    let ids: BTreeSet<&str> = all.iter().map(|thread| thread.id.as_str()).collect();
    reseat_unindexed(
        options,
        lanes,
        ledger,
        &mut dest,
        (&named, &ids),
        summary,
        emit,
    )
}

/// Whether this ledger registered (or adopted) thread `id`'s row with this
/// `cwd` and rollout in this index.
fn ours(ledger: &Ledger, dest: &Dest, id: &str, cwd: &str, rollout: &Path) -> Result<bool> {
    Ok(ledger
        .thread_row(id)?
        .is_some_and(|(db, known_cwd, path)| db == dest.db && known_cwd == cwd && path == rollout))
}

/// The destination row of thread `id`: `(cwd, rollout_path)`.
fn row_of(dest: &Connection, id: &str) -> Result<Option<(String, String)>> {
    sql(dest
        .query_row(
            "SELECT cwd, rollout_path FROM threads WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional())
}

/// What the destination already holds for a thread this ledger did not
/// register.
struct Claim {
    /// The rollout may be placed (and the row adopted or registered).
    placeable: bool,
    /// The row's `cwd`, a non-canonical spelling of the lane, to re-spell.
    respell: Option<String>,
    /// The destination holds the thread in another form or place: nothing
    /// is placed, the holding is proved or reported.
    holds: Option<Holding>,
}

/// How the destination holds a thread this ledger did not register.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Holding {
    /// Its row (with the lane's `cwd`) names this file: the thread's
    /// rollout in another form, spelling or directory.
    Row(PathBuf),
    /// No row, but a compressed form of the rollout at its place.
    Rowless,
}

impl Holding {
    fn row(&self) -> Option<&Path> {
        match self {
            Self::Row(path) => Some(path),
            Self::Rowless => None,
        }
    }
}

/// `path` with `suffix` appended to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The compressed forms Codex and the archive keep beside a plain rollout.
const FORMS: [&str; 2] = [".zst", ".gz"];

/// Whether a compressed form of the plain rollout `rollout` is at its
/// place (any node: a symlink into an archive counts).
fn form_beside(rollout: &Path) -> bool {
    FORMS
        .iter()
        .any(|suffix| std::fs::symlink_metadata(with_suffix(rollout, suffix)).is_ok())
}

/// How a destination row's `rollout_path` names the plain rollout this
/// pass would place at `rollout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Names {
    /// That plain file, its directory spelled any way that resolves to the
    /// same directory.
    Plain,
    /// A compressed form of it there (`.zst`, `.gz`).
    Form,
    /// The same rollout file name (any form) in another directory.
    Elsewhere,
    /// Another file.
    Other,
}

pub(super) fn names(recorded: &str, rollout: &Path) -> Names {
    let recorded = Path::new(recorded);
    if recorded == rollout {
        return Names::Plain;
    }
    let (Some(name), Some(want)) = (recorded.file_name(), rollout.file_name()) else {
        return Names::Other;
    };
    let name_text = name.to_string_lossy();
    let stem = FORMS
        .iter()
        .find_map(|suffix| name_text.strip_suffix(suffix))
        .unwrap_or(&name_text);
    if std::ffi::OsStr::new(stem) != want {
        return Names::Other;
    }
    let here = recorded.parent() == rollout.parent()
        || match (recorded.parent(), rollout.parent()) {
            (Some(a), Some(b)) => {
                matches!((resolved(a), resolved(b)), (Some(a), Some(b)) if a == b)
            }
            _ => false,
        };
    if !here {
        Names::Elsewhere
    } else if name == want {
        Names::Plain
    } else {
        Names::Form
    }
}

/// Where `path` resolves: its nearest existing ancestor's real path with
/// the missing levels after it (each a plain name), so two spellings of a
/// directory not made yet still compare equal through the link above it.
fn resolved(path: &Path) -> Option<PathBuf> {
    let existing = crate::space::existing_ancestor(path).ok()?;
    let tail = path.strip_prefix(&existing).ok()?;
    if tail
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let real = std::fs::canonicalize(&existing).ok()?;
    Some(tail.components().fold(real, |at, part| at.join(part)))
}

/// Whether `cwd` is another spelling of the canonical directory `canonical`
/// (through a symlink, such as sting's `~/git`): it resolves to it.
fn same_directory(cwd: &str, canonical: &str) -> bool {
    Path::new(cwd).is_absolute()
        && std::fs::canonicalize(cwd).is_ok_and(|real| real == Path::new(canonical))
}

/// The destination's claim on thread `id`, whose rollout this pass would
/// place at `rollout` with `cwd` `dest_cwd` (a lane's when `in_lane`).
///
/// - A row naming this `cwd` and the plain rollout here (its directory
///   spelled any way that resolves to this one): placeable (adopted),
///   unless the plain file is absent and a compressed form sits beside it.
/// - A row whose `cwd` only spells the lane differently: re-spelled
///   (OI-1003-Q198 (1), unless `--undo` put it back or re-spelling is off),
///   then judged as above.
/// - A row with the lane's `cwd` naming this rollout in another form or
///   directory (or the plain file with a form beside and none of its
///   own): held ([`Holding::Row`]); nothing placed.
/// - Any other row: not placeable (`RESEAT_INDEX_CONFLICT`).
/// - No row, but another form of the rollout at the destination (Codex's
///   `.jsonl.zst`, an archive's `.jsonl.gz`): held ([`Holding::Rowless`]),
///   nothing placed; the destination's form is the destination's
///   (re-review of `8a89f9a`, MAJOR).
fn claim(
    options: &Options,
    ledger: &Ledger,
    dest: &Dest,
    id: &str,
    (dest_cwd, in_lane): (&str, bool),
    rollout: &Path,
) -> Result<Claim> {
    let beside = form_beside(rollout);
    match row_of(&dest.conn, id)? {
        Some((cwd, path)) => {
            let undone = ledger
                .cwd_update(id)?
                .is_some_and(|row| row.state == "undone" || row.state == "undo-kept");
            let respell = (cwd != dest_cwd
                && in_lane
                && options.respell_cwd
                && !undone
                && same_directory(&cwd, dest_cwd))
            .then_some(cwd.clone());
            let cwd_holds = cwd == dest_cwd || respell.is_some();
            let named = names(&path, rollout);
            let placeable = cwd_holds
                && named == Names::Plain
                && (!beside || std::fs::symlink_metadata(rollout).is_ok());
            let holds = (cwd_holds && !placeable && named != Names::Other)
                .then(|| Holding::Row(PathBuf::from(&path)));
            Ok(Claim {
                placeable,
                respell,
                holds,
            })
        }
        None => Ok(Claim {
            placeable: !beside,
            respell: None,
            holds: beside.then_some(Holding::Rowless),
        }),
    }
}

/// What a holding's proof found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proved {
    /// `destination-adopted`: the file the destination's row names holds
    /// the landed stream's whole lines, exactly or followed by what the
    /// destination wrote (`exactly`, `continued`). A landing with no whole
    /// line yet is held by any file (the R0 check's rule).
    Adopted(&'static str),
    /// `destination-holds`: a destination file of the thread is there but
    /// does not prove it (`rowless`: a compressed form with no row;
    /// `row-file-absent`: the row's file is absent with a form of it
    /// beside; `unreadable`: it could not be opened or read; or the
    /// check's own `not-regular`, `unreadable-form`, `size-differs`,
    /// `hash-differs`, `ends-mid-line`, `landing-not-regular`).
    Held(&'static str),
    /// `destination-row-only`: only a row, no destination file tied to the
    /// landing (`absent`: the row's file is not there; `rollout-root`,
    /// `landing-absent`: nothing landed ties to it). Unproved: the R0
    /// check fails it.
    RowOnly(&'static str),
}

/// The outcome words (thread lines, `Summary` sessions and rows, and the
/// ledger's session cache): distinct from the placement words.
const DESTINATION_ADOPTED: &str = "destination-adopted";
const DESTINATION_HOLDS: &str = "destination-holds";
const DESTINATION_ROW_ONLY: &str = "destination-row-only";

/// A file's identity as cached text, or `absent`.
fn identity_text(path: &Path) -> String {
    std::fs::symlink_metadata(path).map_or_else(
        |_| "absent".to_owned(),
        |meta| {
            let identity = Identity::of(&meta);
            format!(
                "{}:{}:{}:{}",
                identity.dev, identity.ino, identity.size, identity.mtime
            )
        },
    )
}

/// The proof's reason words, as `'static` (the cache stores the text).
const REASONS: [&str; 11] = [
    "row-file-absent",
    "rowless",
    "exactly",
    "continued",
    "absent",
    "not-regular",
    "unreadable-form",
    "size-differs",
    "hash-differs",
    "ends-mid-line",
    "landing-not-regular",
];

fn reason(word: &str) -> Option<&'static str> {
    REASONS.iter().copied().find(|known| *known == word)
}

/// Prove a holding: does the file the destination's row names hold the
/// landed stream's whole lines (the R0 check's rule)? Cached in the
/// session row by both files' identities, so an unchanged rerun reads
/// nothing (S3).
fn prove(
    ledger: &Ledger,
    holding: &Holding,
    (id, source_cwd): (&str, &str),
    landed: &Path,
    dest_cwd: &str,
) -> Result<Proved> {
    let Holding::Row(path) = holding else {
        return Ok(Proved::Held("rowless"));
    };
    let landed_identity = std::fs::symlink_metadata(landed)
        .ok()
        .map(|meta| Identity::of(&meta));
    // Both files' identities: the landing's device and inode with the
    // destination file's whole identity (the size and mtime are columns).
    let dest_identity = format!(
        "{}|{}",
        landed_identity.map_or_else(
            || "absent".to_owned(),
            |identity| format!("{}:{}", identity.dev, identity.ino)
        ),
        identity_text(path)
    );
    let cached = ledger.session(TOOL, id)?.filter(|row| {
        row.landing == landed
            && landed_identity.is_some_and(|now| {
                Some(now.size) == row.landed_size && Some(now.mtime) == row.landed_mtime
            })
            && row.dest_key.as_deref() == Some(dest_identity.as_str())
    });
    if let Some(row) = cached {
        if let Some(word) = row.slug.as_deref().and_then(reason) {
            match row.outcome.as_str() {
                DESTINATION_ADOPTED => return Ok(Proved::Adopted(word)),
                DESTINATION_HOLDS => return Ok(Proved::Held(word)),
                DESTINATION_ROW_ONLY => return Ok(Proved::RowOnly(word)),
                _ => {}
            }
        }
    }
    let gz = path.extension().is_some_and(|extension| extension == "gz");
    let proved = if gz {
        Proved::Held("unreadable-form")
    } else {
        // A destination file that cannot be opened or read (a mode-000
        // `.zst`) is this thread's outcome, never the pass's refusal.
        match super::check::holds_lines(landed, path) {
            Ok(Ok(word)) => Proved::Adopted(word),
            Ok(Err("absent")) => {
                let plain = plain_name(path);
                if std::fs::symlink_metadata(&plain).is_ok() || form_beside(&plain) {
                    Proved::Held("row-file-absent")
                } else {
                    Proved::RowOnly("absent")
                }
            }
            Ok(Err(word)) => Proved::Held(word),
            Err(_) => return Ok(Proved::Held("unreadable")),
        }
    };
    let (outcome, word) = proved.words();
    ledger.put_session(&Session {
        tool: TOOL.to_owned(),
        id: id.to_owned(),
        landing: landed.to_path_buf(),
        source_cwd: Some(source_cwd.to_owned()),
        dest_cwd: Some(PathBuf::from(dest_cwd)),
        dest_key: Some(dest_identity),
        slug: Some(word.to_owned()),
        outcome: outcome.to_owned(),
        landed_size: landed_identity.map(|identity| identity.size),
        landed_mtime: landed_identity.map(|identity| identity.mtime),
    })?;
    Ok(proved)
}

impl Proved {
    /// `(outcome word, proof word)`.
    const fn words(self) -> (&'static str, &'static str) {
        match self {
            Self::Adopted(word) => (DESTINATION_ADOPTED, word),
            Self::Held(word) => (DESTINATION_HOLDS, word),
            Self::RowOnly(word) => (DESTINATION_ROW_ONLY, word),
        }
    }
}

/// `path` without a `.zst` or `.gz` suffix.
fn plain_name(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    FORMS
        .iter()
        .find_map(|suffix| text.strip_suffix(suffix))
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// Report a thread the destination holds: `destination-adopted` (proved),
/// `destination-holds` or `destination-row-only` (unproved, `"proved":
/// false`); none is a refusal, and only the first passes the R0 check.
fn held(
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
    (id, source_cwd): (&str, &str),
    dest_cwd: Option<&str>,
    proved: Proved,
    rollout: Option<&Path>,
) -> Result<()> {
    let (outcome, proof) = proved.words();
    let is_adopted = matches!(proved, Proved::Adopted(_));
    summary.session(TOOL, outcome);
    *summary.rows.entry(outcome.to_owned()).or_default() += 1;
    emit(&format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"thread\",\"tool\":\"{TOOL}\",\"id\":{},\"source_cwd\":{},\"dest_cwd\":{},\"outcome\":\"{outcome}\",\"row\":\"{outcome}\",\"refused\":null,\"proved\":{is_adopted},\"proof\":\"{proof}\",\"dest_rollout\":{}}}",
        json_string(id),
        json_string(source_cwd),
        dest_cwd.map_or_else(|| "null".to_owned(), json_string),
        rollout.map_or_else(|| "null".to_owned(), |path| json_string(&path.to_string_lossy())),
    ))
}

/// The destination `cwd` a thread recorded in `cwd` gets: its lane's, or
/// its own when placed by ID; `None` when it has neither.
fn lane_cwd(options: &Options, lanes: &Lanes, cwd: &str) -> Result<Option<String>> {
    Ok(match lanes.lane(Path::new(cwd))? {
        Some(lane) => Some(super::key::utf8(&lane.dest_cwd)?.to_owned()),
        None if options.by_id(cwd) => Some(cwd.to_owned()),
        None => None,
    })
}

/// The symlinks a destination row's `cwd` may be spelled through: the
/// destination home's `git` and each `--map` destination, each only when
/// it is a symlink, with its real path; longest first.
#[must_use]
pub fn spelling_links(options: &Options) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    let candidates = std::iter::once(options.dest_home.join("git"))
        .chain(options.maps.iter().map(|(_, dest)| dest.clone()));
    for link in candidates {
        if !std::fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()) {
            continue;
        }
        let Ok(real) = std::fs::canonicalize(&link) else {
            continue;
        };
        if real != link && !out.iter().any(|(known, _)| *known == link) {
            out.push((link, real));
        }
    }
    out.sort_by_key(|(link, _)| std::cmp::Reverse(link.components().count()));
    out
}

/// The real spelling of `cwd` when it differs from it only through one of
/// `links` (the longest that prefixes it): the link's real path joined
/// with the rest, and only when the nearest existing level of `cwd`
/// resolves to the same level of that spelling (nothing else on the way
/// is a link elsewhere). `None` otherwise.
#[must_use]
pub fn respelled(cwd: &str, links: &[(PathBuf, PathBuf)]) -> Option<String> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return None;
    }
    let (link, real) = links.iter().find(|(link, _)| path.starts_with(link))?;
    let rest = path.strip_prefix(link).ok()?;
    if rest
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let new = rest
        .components()
        .fold(real.clone(), |at, part| at.join(part));
    let text = new.to_str()?;
    if text == cwd {
        return None;
    }
    let existing = crate::space::existing_ancestor(path).ok()?;
    let tail = existing.strip_prefix(link).ok()?;
    let expected = tail
        .components()
        .fold(real.clone(), |at, part| at.join(part));
    (std::fs::canonicalize(&existing).ok()? == expected).then(|| text.to_owned())
}

/// Rows to re-spell, `(id, old cwd, new cwd)`, and the count skipped.
type Spelled = (Vec<(String, String, String)>, u64);

/// The destination rows [`respelled`] would re-spell through `links`, as
/// `(id, old, new)`: every row, mapped lane or not, except one this
/// ledger's `--undo` put back (operator M2 round 2, 2026-10-17); and how
/// many rows were skipped because their `id` or `cwd` is not UTF-8 text.
/// In-lane rows spelled through another link (resolving to the lane) are
/// re-spelled per thread instead and are not in this list.
///
/// # Errors
/// `SQLite` and ledger refusals.
pub fn spelled_rows(
    links: &[(PathBuf, PathBuf)],
    dest: &Connection,
    ledger: Option<&Ledger>,
) -> Result<Spelled> {
    if links.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let mut statement = sql(dest.prepare("SELECT id, cwd FROM threads ORDER BY id"))?;
    let rows = sql(statement.query_map([], |row| {
        let text = |at: usize| -> rusqlite::Result<Option<String>> {
            Ok(row.get_ref(at)?.as_str().ok().map(str::to_owned))
        };
        Ok((text(0)?, text(1)?))
    }))?;
    let mut out = Vec::new();
    let mut skipped = 0_u64;
    for row in rows {
        let (Some(id), Some(cwd)) = sql(row)? else {
            skipped += 1;
            continue;
        };
        let Some(new) = respelled(&cwd, links) else {
            continue;
        };
        if let Some(ledger) = ledger {
            if ledger
                .cwd_update(&id)?
                .is_some_and(|row| row.state == "undone" || row.state == "undo-kept")
            {
                continue;
            }
        }
        out.push((id, cwd, new));
    }
    Ok((out, skipped))
}

/// Re-spell every symlink-spelled destination row ([`spelled_rows`]), in
/// batches, the ledgered way [`respell`] does.
fn sweep_spellings(
    options: &Options,
    ledger: &mut Ledger,
    dest: &mut Dest,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    if !options.respell_cwd {
        return Ok(());
    }
    let links = spelling_links(options);
    let (rows, skipped) = spelled_rows(&links, &dest.conn, Some(ledger))?;
    if skipped > 0 {
        *summary
            .rows
            .entry("respell-not-text".to_owned())
            .or_default() += skipped;
    }
    for batch in rows.chunks(BATCH) {
        // A link retargeted since the rows were read: stop, re-spell
        // nothing more against the old target.
        if spelling_links(options) != links {
            *summary
                .rows
                .entry("respell-link-moved".to_owned())
                .or_default() += batch.len() as u64;
            emit(&format!(
                "{{\"schema\":\"{SCHEMA}\",\"kind\":\"respell-stopped\",\"tool\":\"{TOOL}\",\"reason\":\"link-moved\"}}"
            ))?;
            break;
        }
        let mut queued = batch.to_vec();
        respell(dest, ledger, &mut queued, summary, emit)?;
    }
    Ok(())
}

/// Re-spell every symlink-spelled row of the destination index when there
/// is no landed thread to re-seat (the sweep runs whenever the index
/// exists, operator M2 round 2 review).
///
/// # Errors
/// Ledger, `SQLite` and I/O refusals.
pub fn sweep_alone(
    options: &Options,
    ledger: &mut Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    if !options.respell_cwd || !options.dest_codex.join(STATE_DB).is_file() {
        return Ok(());
    }
    let root = super::root(&options.dest_codex)?;
    let db = root.join(STATE_DB);
    let conn = sql(Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ))?;
    sql(conn.busy_timeout(std::time::Duration::from_secs(30)))?;
    let mut dest = Dest { root, db, conn };
    settle_cwd_intents(&dest, ledger)?;
    sweep_spellings(options, ledger, &mut dest, summary, emit)
}

/// The rows a pass would re-spell, read-only (`--check`): 0 without a
/// destination index or with re-spelling off.
///
/// # Errors
/// `SQLite` and ledger refusals.
pub fn respell_pending(options: &Options, ledger: Option<&Ledger>) -> Result<u64> {
    if !options.respell_cwd {
        return Ok(0);
    }
    let Ok(root) = std::fs::canonicalize(&options.dest_codex) else {
        return Ok(0);
    };
    let db = root.join(STATE_DB);
    if !db.is_file() {
        return Ok(0);
    }
    let conn = sql(Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ))?;
    let (rows, _) = spelled_rows(&spelling_links(options), &conn, ledger)?;
    Ok(rows.len() as u64)
}

/// Re-spell the queued rows' `cwd` (only that column) in one destination
/// transaction, each only while it still holds the old value, with every
/// old value ledgered first (OI-1003-Q198 (1)).
fn respell(
    dest: &mut Dest,
    ledger: &mut Ledger,
    queued: &mut Vec<(String, String, String)>,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    if queued.is_empty() {
        return Ok(());
    }
    let rows = core::mem::take(queued);
    ledger.put_cwd_intents(&dest.db, &rows)?;
    let mut changed = Vec::with_capacity(rows.len());
    {
        let transaction = sql(dest
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        for (id, old, new) in &rows {
            changed.push(
                sql(transaction.execute(
                    "UPDATE threads SET cwd = ?3 WHERE id = ?1 AND cwd = ?2",
                    params![id, old, new],
                ))? == 1,
            );
        }
        sql(transaction.commit())?;
    }
    fault_point!(ReseatAfterCwdUpdate);
    let marks: Vec<(String, &str)> = rows
        .iter()
        .zip(&changed)
        .map(|((id, _, _), changed)| (id.clone(), if *changed { "applied" } else { "skipped" }))
        .collect();
    ledger.mark_cwd(&marks)?;
    for ((id, old, new), changed) in rows.iter().zip(&changed) {
        let word = if *changed {
            "respelled"
        } else {
            "changed-since"
        };
        *summary.rows.entry(word.to_owned()).or_default() += 1;
        emit(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"row-update\",\"tool\":\"{TOOL}\",\"id\":{},\"column\":\"cwd\",\"old\":{},\"new\":{},\"outcome\":\"{word}\"}}",
            json_string(id),
            json_string(old),
            json_string(new),
        ))?;
    }
    Ok(())
}

/// Settle re-spellings a crash left as `intent`: `applied` when the
/// destination row holds the new value, `skipped` when it still holds the
/// old one.
fn settle_cwd_intents(dest: &Dest, ledger: &mut Ledger) -> Result<()> {
    let mut marks = Vec::new();
    for row in ledger.cwd_updates()? {
        if row.state != "intent" || row.db != dest.db {
            continue;
        }
        match row_of(&dest.conn, &row.id)? {
            Some((cwd, _)) if cwd == row.new_cwd => marks.push((row.id, "applied")),
            Some((cwd, _)) if cwd == row.old_cwd => marks.push((row.id, "skipped")),
            _ => {}
        }
    }
    if marks.is_empty() {
        Ok(())
    } else {
        ledger.mark_cwd(&marks)
    }
}

/// Put back the old `cwd` of every row this ledger re-spelled
/// (`estate-reseat --undo`), each only while it still holds the value
/// re-seat wrote; one changed since is kept (`undo-kept`,
/// `RESEAT_DEST_CONTINUED`).
///
/// # Errors
/// Ledger and `SQLite` refusals.
pub fn undo(ledger: &mut Ledger, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<u64> {
    let rows: Vec<_> = ledger
        .cwd_updates()?
        .into_iter()
        .filter(|row| row.state == "applied" || row.state == "intent")
        .collect();
    let mut by_db: BTreeMap<PathBuf, Vec<super::ledger::CwdUpdate>> = BTreeMap::new();
    for row in rows {
        by_db.entry(row.db.clone()).or_default().push(row);
    }
    let mut undone = 0;
    for (db, rows) in by_db {
        let mut conn = sql(Connection::open_with_flags(
            &db,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ))?;
        sql(conn.busy_timeout(std::time::Duration::from_secs(30)))?;
        let mut words = Vec::with_capacity(rows.len());
        {
            let transaction =
                sql(conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
            for row in &rows {
                let changed = sql(transaction.execute(
                    "UPDATE threads SET cwd = ?2 WHERE id = ?1 AND cwd = ?3",
                    params![row.id, row.old_cwd, row.new_cwd],
                ))?;
                let now: Option<String> = sql(transaction
                    .query_row(
                        "SELECT cwd FROM threads WHERE id = ?1",
                        params![row.id],
                        |found| found.get(0),
                    )
                    .optional())?;
                words.push(
                    if changed == 1 || now.as_deref() == Some(row.old_cwd.as_str()) {
                        "undone"
                    } else {
                        "undo-kept"
                    },
                );
            }
            sql(transaction.commit())?;
        }
        fault_point!(ReseatAfterUndoCwd);
        let marks: Vec<(String, &str)> = rows
            .iter()
            .zip(&words)
            .map(|(row, word)| (row.id.clone(), *word))
            .collect();
        ledger.mark_cwd(&marks)?;
        for (row, word) in rows.iter().zip(&words) {
            undone += u64::from(*word == "undone");
            let refused = if *word == "undo-kept" {
                json_string(BulkloadRefusal::ReseatDestContinued.code())
            } else {
                "null".to_owned()
            };
            emit(&format!(
                "{{\"schema\":\"{SCHEMA}\",\"kind\":\"undo\",\"tool\":\"{TOOL}\",\"id\":{},\"column\":\"cwd\",\"restored\":{},\"outcome\":\"{word}\",\"refused\":{refused}}}",
                json_string(&row.id),
                json_string(&row.old_cwd),
            ))?;
        }
    }
    Ok(undone)
}

/// A landed session the pass refused: cached by its landed file's identity
/// so a rerun does not read it again (S3; re-review of `8a89f9a`).
fn remember_refused(
    ledger: &Ledger,
    (id, landed): (&str, &Path),
    cwd: &str,
    refusal: &BulkloadRefusal,
) -> Result<()> {
    let identity = std::fs::symlink_metadata(landed)
        .ok()
        .map(|meta| Identity::of(&meta));
    ledger.put_session(&Session {
        tool: TOOL.to_owned(),
        id: id.to_owned(),
        landing: landed.to_path_buf(),
        source_cwd: Some(cwd.to_owned()),
        outcome: refusal.code().to_owned(),
        landed_size: identity.map(|identity| identity.size),
        landed_mtime: identity.map(|identity| identity.mtime),
        ..Session::default()
    })
}

/// The recorded `cwd` of an unindexed landed rollout: from its session row
/// (a found `cwd` always; "none recorded" while the landed file is
/// unchanged), else from its `session_meta` line.
fn unindexed_cwd(ledger: &Ledger, id: &str, landed: &Path) -> Result<Option<String>> {
    let cached = ledger
        .session(TOOL, id)?
        .filter(|row| row.landing == landed);
    if let Some(row) = cached {
        match row.source_cwd {
            Some(cwd) if !cwd.is_empty() => return Ok(Some(cwd)),
            Some(_) => {
                let now = std::fs::symlink_metadata(landed)
                    .ok()
                    .map(|meta| Identity::of(&meta));
                if now.is_some_and(|now| {
                    Some(now.size) == row.landed_size && Some(now.mtime) == row.landed_mtime
                }) {
                    return Ok(None);
                }
            }
            None => {}
        }
    }
    match meta_cwd(landed) {
        Ok(cwd) => Ok(cwd.filter(|cwd| !cwd.is_empty())),
        Err(BulkloadRefusal::ReseatUnreadableForm) => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Place every landed rollout no snapshot row names, register a row built
/// from its header (OI-1003-Q198 D), and report it.
#[allow(clippy::too_many_lines)]
fn reseat_unindexed(
    options: &Options,
    lanes: &Lanes,
    ledger: &mut Ledger,
    dest: &mut Dest,
    (named, ids): (&BTreeSet<PathBuf>, &BTreeSet<&str>),
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    for file in unindexed(options, named)? {
        let landed = options.landing_codex.join(&file.rel);
        let shown_id = file
            .id
            .clone()
            .unwrap_or_else(|| file.rel.to_string_lossy().into_owned());
        let Some(id) = file.id.clone() else {
            summary.session(TOOL, "unreadable-form");
            emit(&thread_line(
                (&shown_id, ""),
                None,
                "unreadable-form",
                Some(&BulkloadRefusal::ReseatUnreadableForm),
                Some("absent"),
            ))?;
            continue;
        };
        if ids.contains(id.as_str()) {
            // Another form of a thread the snapshot names: it stays in the
            // landing beside the form placed (`--check` fails a longer one).
            summary.session(TOOL, "extra-form");
            emit(&thread_line((&id, ""), None, "extra-form", None, None))?;
            continue;
        }
        if file
            .rel
            .extension()
            .is_some_and(|extension| extension == "gz")
        {
            summary.session(TOOL, "unreadable-form");
            emit(&thread_line(
                (&id, ""),
                None,
                "unreadable-form",
                Some(&BulkloadRefusal::ReseatUnreadableForm),
                Some("absent"),
            ))?;
            continue;
        }
        let Some(cwd) = unindexed_cwd(ledger, &id, &landed)? else {
            remember_refused(
                ledger,
                (&id, &landed),
                "",
                &BulkloadRefusal::ReseatLaneUnresolved,
            )?;
            summary.session(TOOL, BulkloadRefusal::ReseatLaneUnresolved.code());
            emit(&thread_line(
                (&id, ""),
                None,
                "refused",
                Some(&BulkloadRefusal::ReseatLaneUnresolved),
                Some("absent"),
            ))?;
            continue;
        };
        let (dest_cwd, in_lane) = match lanes.lane(Path::new(&cwd))? {
            Some(lane) => (super::key::utf8(&lane.dest_cwd)?.to_owned(), true),
            None if options.by_id(&cwd) => (cwd.clone(), false),
            None => {
                remember_refused(
                    ledger,
                    (&id, &landed),
                    &cwd,
                    &BulkloadRefusal::ReseatLaneUnmapped,
                )?;
                summary.session(TOOL, BulkloadRefusal::ReseatLaneUnmapped.code());
                emit(&thread_line(
                    (&id, &cwd),
                    None,
                    "refused",
                    Some(&BulkloadRefusal::ReseatLaneUnmapped),
                    Some("absent"),
                ))?;
                continue;
            }
        };
        let dest_rel = dest_form(&file.rel);
        let rollout = dest.root.join(&dest_rel);
        let was_ours = ours(ledger, dest, &id, &dest_cwd, &rollout)?;
        if !was_ours {
            // The rule of the snapshot's threads (re-review of `8a89f9a`,
            // MAJOR): a destination row or form for this thread is the
            // destination's; nothing is placed beside it.
            let claim = claim(options, ledger, dest, &id, (&dest_cwd, in_lane), &rollout)?;
            if let Some(old) = claim.respell {
                let mut queued = vec![(id.clone(), old, dest_cwd.clone())];
                respell(dest, ledger, &mut queued, summary, emit)?;
            }
            if let Some(holding) = &claim.holds {
                let proved = prove(ledger, holding, (&id, &cwd), &landed, &dest_cwd)?;
                held(
                    summary,
                    emit,
                    (&id, &cwd),
                    Some(&dest_cwd),
                    proved,
                    holding.row(),
                )?;
                continue;
            }
            if !claim.placeable {
                summary.session(TOOL, "index-conflict");
                *summary.rows.entry("conflict".to_owned()).or_default() += 1;
                emit(&thread_line(
                    (&id, &cwd),
                    Some(&dest_cwd),
                    "kept",
                    Some(&BulkloadRefusal::ReseatIndexConflict),
                    Some("conflict"),
                ))?;
                continue;
            }
        }
        let placer = Placer::new(ledger, &dest.root).with_archived(&options.replace_archived);
        let outcome = placer
            .place(&landed, &dest_rel, Kind::Rollout, &id, None)?
            .outcome;
        summary.file(TOOL, outcome);
        summary.session(TOOL, outcome.word());
        let identity = std::fs::symlink_metadata(&landed)
            .ok()
            .map(|meta| Identity::of(&meta));
        ledger.put_session(&Session {
            tool: TOOL.to_owned(),
            id: id.clone(),
            landing: landed.clone(),
            source_cwd: Some(cwd.clone()),
            dest_cwd: Some(PathBuf::from(&dest_cwd)),
            outcome: outcome.word().to_owned(),
            landed_size: identity.map(|identity| identity.size),
            landed_mtime: identity.map(|identity| identity.mtime),
            ..Session::default()
        })?;
        if !outcome.landed() {
            *summary.rows.entry("absent".to_owned()).or_default() += 1;
            emit(&thread_line(
                (&id, &cwd),
                Some(&dest_cwd),
                outcome.word(),
                outcome.refusal().as_ref(),
                Some("absent"),
            ))?;
            continue;
        }
        if was_ours {
            *summary.rows.entry("unchanged".to_owned()).or_default() += 1;
            emit(&thread_line(
                (&id, &cwd),
                Some(&dest_cwd),
                outcome.word(),
                None,
                Some("unchanged"),
            ))?;
            continue;
        }
        let row = match row_of(&dest.conn, &id)? {
            // A crash after the row's commit, or a row already naming it.
            Some((row_cwd, path))
                if row_cwd == dest_cwd && names(&path, &rollout) == Names::Plain =>
            {
                "adopted"
            }
            Some(_) => "conflict",
            None => synthesize(dest, &id, &rollout, &dest_cwd, &file.rel)?,
        };
        *summary.rows.entry(row.to_owned()).or_default() += 1;
        if row == "adopted" || row == "synthesized" {
            ledger.put_thread_rows(&dest.db, &[(id.clone(), dest_cwd.clone(), rollout, row)])?;
        }
        let refusal = match row {
            "conflict" => Some(BulkloadRefusal::ReseatIndexConflict),
            "absent" => Some(BulkloadRefusal::ReseatIndexRowAbsent),
            _ => None,
        };
        emit(&thread_line(
            (&id, &cwd),
            Some(&dest_cwd),
            outcome.word(),
            refusal.as_ref(),
            Some(row),
        ))?;
    }
    Ok(())
}

/// What a rollout's first lines say about its thread: enough to build the
/// row Codex would have made for it (OI-1003-Q198 D).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub id: Option<String>,
    pub cwd: Option<String>,
    /// `session_meta`'s timestamp, milliseconds since the epoch.
    pub created_ms: Option<i64>,
    /// The `source` column: a string as is, an object as its JSON text.
    pub source: Option<String>,
    pub model_provider: Option<String>,
    pub cli_version: Option<String>,
    pub originator: Option<String>,
    pub thread_source: Option<String>,
    pub history_mode: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub agent_path: Option<String>,
    /// The first `turn_context`'s approval policy, sandbox policy (JSON
    /// text), model and effort.
    pub approval: Option<String>,
    pub sandbox: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The user's first prompt: the first `user_message` event, else the
    /// first user message that is not a wrapped context block.
    pub prompt: Option<String>,
    /// Whether a `user_message` event was seen.
    pub user_event: bool,
}

/// The most lines a header read takes.
const HEADER_LINES: usize = 512;

fn text(line: &[u8], path: &[&str]) -> Option<String> {
    super::json::nested_string(line, path)
}

/// A JSON value as a column: a string's text, anything else its JSON.
fn column_text(raw: &[u8]) -> Option<String> {
    super::json::raw_string(raw).or_else(|| String::from_utf8(raw.to_vec()).ok())
}

/// The first user message's text that is the user's own (not a
/// `<environment_context>`-style block or the AGENTS.md preamble).
fn user_text(line: &[u8]) -> Option<String> {
    let content = super::json::nested_raw(line, &["payload", "content"])?;
    super::json::elements(content)
        .into_iter()
        .filter_map(|element| super::json::nested_string(element, &["text"]))
        .find(|text| {
            let text = text.trim_start();
            !text.is_empty() && !text.starts_with('<') && !text.starts_with("# AGENTS.md")
        })
}

/// The header of a placed destination rollout (plain `.jsonl`; at most
/// [`HEADER_LINES`] lines and [`META_LIMIT`] bytes), counted as destination
/// bytes read.
fn dest_header(path: &Path) -> Result<Header> {
    struct Counted(std::fs::File);
    impl Read for Counted {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.0.read(buffer)?;
            counters::add_len(Counter::ReseatDestRead, read);
            Ok(read)
        }
    }
    let file = crate::hash::open_nofollow(path)?;
    let mut reader = std::io::BufReader::new(Counted(file).take(META_LIMIT));
    header_of(&mut reader, false)
}

fn header_of(reader: &mut dyn std::io::BufRead, compressed: bool) -> Result<Header> {
    let mut out = Header::default();
    let mut fallback = None;
    let (mut meta, mut context) = (false, false);
    let mut line = Vec::new();
    for _ in 0..HEADER_LINES {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).map_err(|error| {
            if compressed {
                BulkloadRefusal::ReseatUnreadableForm
            } else {
                crate::refuse::io(&error, "reseat::codex::header")
            }
        })?;
        if read == 0 {
            break;
        }
        match text(&line, &["type"]).as_deref() {
            Some("session_meta") if !meta => {
                meta = true;
                out.id = text(&line, &["payload", "id"]);
                out.cwd = text(&line, &["payload", "cwd"]);
                out.created_ms = text(&line, &["payload", "timestamp"])
                    .or_else(|| text(&line, &["timestamp"]))
                    .and_then(|stamp| epoch_ms(&stamp));
                let source = super::json::nested_raw(&line, &["payload", "source"]);
                out.source = source.and_then(column_text);
                out.model_provider = text(&line, &["payload", "model_provider"]);
                out.cli_version = text(&line, &["payload", "cli_version"]);
                out.originator = text(&line, &["payload", "originator"]);
                out.thread_source = text(&line, &["payload", "thread_source"]);
                out.history_mode = text(&line, &["payload", "history_mode"]);
                let spawn = ["subagent", "thread_spawn"];
                for (field, slot) in [
                    ("agent_nickname", &mut out.agent_nickname),
                    ("agent_role", &mut out.agent_role),
                    ("agent_path", &mut out.agent_path),
                ] {
                    *slot = text(&line, &["payload", field]).or_else(|| {
                        source.and_then(|source| {
                            super::json::nested_string(source, &[spawn[0], spawn[1], field])
                        })
                    });
                }
            }
            Some("turn_context") if !context => {
                context = true;
                out.approval = text(&line, &["payload", "approval_policy"]);
                out.sandbox = super::json::nested_raw(&line, &["payload", "sandbox_policy"])
                    .and_then(|raw| String::from_utf8(raw.to_vec()).ok());
                out.model = text(&line, &["payload", "model"]);
                out.effort = text(&line, &["payload", "effort"]);
            }
            Some("event_msg")
                if text(&line, &["payload", "type"]).as_deref() == Some("user_message") =>
            {
                if !out.user_event {
                    out.user_event = true;
                    out.prompt = text(&line, &["payload", "message"]);
                }
            }
            Some("response_item")
                if fallback.is_none()
                    && text(&line, &["payload", "type"]).as_deref() == Some("message")
                    && text(&line, &["payload", "role"]).as_deref() == Some("user") =>
            {
                fallback = user_text(&line);
            }
            _ => {}
        }
        if meta && context && out.user_event {
            break;
        }
    }
    if out.prompt.is_none() {
        out.prompt = fallback;
    }
    Ok(out)
}

/// Milliseconds since the epoch of an RFC 3339 UTC timestamp
/// (`2026-10-16T10:00:00.000Z`), or `None` for another shape.
#[must_use]
pub fn epoch_ms(stamp: &str) -> Option<i64> {
    let number = |range: core::ops::Range<usize>| -> Option<i64> {
        let digits = stamp.get(range)?;
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| digits.parse().ok())
            .flatten()
    };
    let bytes = stamp.as_bytes();
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || !stamp.ends_with('Z')
    {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let millis = match stamp.get(19..stamp.len() - 1) {
        Some("") => 0,
        Some(fraction) if fraction.starts_with('.') => {
            let digits: String = fraction.chars().skip(1).take(3).collect();
            format!("{digits:0<3}").parse().ok()?
        }
        _ => return None,
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000) + millis)
}

/// Insert the row Codex would make for the placed rollout at `rollout`
/// from its header (OI-1003-Q198 D): `"synthesized"`, `"adopted"` when an
/// equal row appeared meanwhile, `"conflict"` when another one did, or
/// `"absent"` when the header cannot fill a column the destination
/// requires (`RESEAT_INDEX_ROW_ABSENT`). The header is read from the
/// placed destination file (the landed bytes, read once more from the
/// destination, never from the landing).
#[allow(clippy::too_many_lines)]
fn synthesize(
    dest: &mut Dest,
    id: &str,
    rollout: &Path,
    cwd: &str,
    rel: &Path,
) -> Result<&'static str> {
    let header = match dest_header(rollout) {
        Ok(header) => header,
        Err(BulkloadRefusal::ReseatUnreadableForm) => return Ok("absent"),
        Err(refusal) => return Err(refusal),
    };
    if header.id.as_deref() != Some(id) {
        return Ok("absent");
    }
    let meta = std::fs::symlink_metadata(rollout).refuse_at("reseat::codex::synthesize")?;
    let mtime_ms = Identity::of(&meta).mtime / 1_000_000;
    let created_ms = header.created_ms.unwrap_or(mtime_ms);
    let archived = rel.starts_with("archived_sessions");
    let prompt = header.prompt.clone().unwrap_or_default();
    let owned =
        |value: Option<&String>| value.map_or(Value::Null, |text| Value::Text(text.clone()));
    let values: Vec<(&str, Value)> = vec![
        ("id", Value::Text(id.to_owned())),
        (
            "rollout_path",
            Value::Text(super::key::utf8(rollout)?.to_owned()),
        ),
        ("created_at", Value::Integer(created_ms / 1_000)),
        ("created_at_ms", Value::Integer(created_ms)),
        ("updated_at", Value::Integer(mtime_ms / 1_000)),
        ("updated_at_ms", Value::Integer(mtime_ms)),
        (
            "source",
            Value::Text(header.source.clone().unwrap_or_else(|| "cli".to_owned())),
        ),
        (
            "model_provider",
            Value::Text(
                header
                    .model_provider
                    .clone()
                    .unwrap_or_else(|| "openai".to_owned()),
            ),
        ),
        ("cwd", Value::Text(cwd.to_owned())),
        ("title", Value::Text(prompt.clone())),
        ("first_user_message", Value::Text(prompt.clone())),
        ("preview", Value::Text(prompt)),
        (
            "sandbox_policy",
            Value::Text(
                header
                    .sandbox
                    .clone()
                    .unwrap_or_else(|| "{\"type\":\"read-only\"}".to_owned()),
            ),
        ),
        (
            "approval_mode",
            Value::Text(
                header
                    .approval
                    .clone()
                    .unwrap_or_else(|| "on-request".to_owned()),
            ),
        ),
        (
            "has_user_event",
            Value::Integer(i64::from(header.user_event)),
        ),
        ("archived", Value::Integer(i64::from(archived))),
        (
            "archived_at",
            if archived {
                Value::Integer(mtime_ms / 1_000)
            } else {
                Value::Null
            },
        ),
        (
            "cli_version",
            Value::Text(header.cli_version.clone().unwrap_or_default()),
        ),
        ("originator", owned(header.originator.as_ref())),
        ("thread_source", owned(header.thread_source.as_ref())),
        (
            "history_mode",
            Value::Text(
                header
                    .history_mode
                    .clone()
                    .unwrap_or_else(|| "legacy".to_owned()),
            ),
        ),
        ("agent_nickname", owned(header.agent_nickname.as_ref())),
        ("agent_role", owned(header.agent_role.as_ref())),
        ("agent_path", owned(header.agent_path.as_ref())),
        ("model", owned(header.model.as_ref())),
        ("reasoning_effort", owned(header.effort.as_ref())),
    ];
    let table = columns(&dest.conn, "threads")?;
    let names: BTreeSet<&str> = values.iter().map(|(name, _)| *name).collect();
    if table
        .iter()
        .any(|(name, required)| *required && !names.contains(name.as_str()))
    {
        return Ok("absent");
    }
    let present: BTreeSet<&str> = table.iter().map(|(name, _)| name.as_str()).collect();
    let (columns_used, row): (Vec<String>, Vec<Value>) = values
        .into_iter()
        .filter(|(name, _)| present.contains(name))
        .map(|(name, value)| (name.to_owned(), value))
        .unzip();
    let placeholders = (1..=columns_used.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let word = {
        let transaction = sql(dest
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let existing: Option<(String, String)> = sql(transaction
            .query_row(
                "SELECT cwd, rollout_path FROM threads WHERE id = ?1",
                params![id],
                |found| Ok((found.get(0)?, found.get(1)?)),
            )
            .optional())?;
        let word = match existing {
            Some((found_cwd, path))
                if found_cwd == cwd && self::names(&path, rollout) == Names::Plain =>
            {
                "adopted"
            }
            Some(_) => "conflict",
            None => {
                sql(transaction.execute(
                    &format!(
                        "INSERT INTO threads({}) VALUES ({placeholders})",
                        quoted(&columns_used)
                    ),
                    rusqlite::params_from_iter(row),
                ))?;
                "synthesized"
            }
        };
        sql(transaction.commit())?;
        word
    };
    if word == "synthesized" {
        fault_point!(ReseatAfterSynthRowInsert);
    }
    Ok(word)
}

/// The two thread indexes a batch is registered between.
struct Index<'a> {
    snapshot: &'a mut Snapshot,
    dest: &'a mut Connection,
    db: &'a Path,
    /// The columns both `threads` tables hold, once read.
    names: &'a mut Option<Vec<String>>,
}

/// Where a thread's rollout lands and the `cwd` its row gets, or why it
/// is not re-seated: `(report word, refusal, the landed form it named)`.
/// `(landed form, destination cwd, whether a lane gave it)`.
type Located =
    std::result::Result<(PathBuf, String, bool), (&'static str, BulkloadRefusal, Option<PathBuf>)>;

fn locate(options: &Options, lanes: &Lanes, thread: &Thread) -> Result<Located> {
    let Some(rel) = rollout_rel(options, &thread.rollout) else {
        return Ok(Err(("refused", BulkloadRefusal::ReseatLaneUnmapped, None)));
    };
    let Some(rel) = landed_form(&options.landing_codex, &rel) else {
        return Ok(Err((
            "landing-absent",
            BulkloadRefusal::CaptureAbsent,
            None,
        )));
    };
    if rel.extension().is_some_and(|extension| extension == "gz") {
        return Ok(Err((
            "unreadable-form",
            BulkloadRefusal::ReseatUnreadableForm,
            Some(rel),
        )));
    }
    let (dest_cwd, in_lane) = match lanes.lane(Path::new(&thread.cwd))? {
        Some(lane) => (super::key::utf8(&lane.dest_cwd)?.to_owned(), true),
        None if options.by_id(&thread.cwd) => (thread.cwd.clone(), false),
        None => {
            return Ok(Err((
                "refused",
                BulkloadRefusal::ReseatLaneUnmapped,
                Some(rel),
            )))
        }
    };
    Ok(Ok((rel, dest_cwd, in_lane)))
}

/// Register `pending` in one destination transaction, then ledger them.
fn register(
    index: Index<'_>,
    all: &[Thread],
    pending: &mut Vec<Pending>,
    ledger: &mut Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let Index {
        snapshot,
        dest,
        db: dest_db,
        names,
    } = index;
    if pending.is_empty() {
        return Ok(());
    }
    let batch = core::mem::take(pending);
    let source = snapshot.conn()?;
    if names.is_none() {
        *names = Some(shared(
            &columns(source, "threads")?,
            &columns(dest, "threads")?,
        )?);
    }
    let names = names
        .as_deref()
        .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
    let mut rows: Vec<&'static str> = Vec::with_capacity(batch.len());
    {
        let transaction =
            sql(dest.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        for item in &batch {
            let thread = all
                .get(item.thread)
                .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
            let rollout = super::key::utf8(&item.rollout)?;
            let existing: Option<(String, String)> = sql(transaction
                .query_row(
                    "SELECT cwd, rollout_path FROM threads WHERE id = ?1",
                    params![thread.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional())?;
            rows.push(match existing {
                Some((cwd, path))
                    if cwd == item.cwd && self::names(&path, &item.rollout) == Names::Plain =>
                {
                    "adopted"
                }
                Some(_) => "conflict",
                None => {
                    insert(&transaction, source, names, thread, &item.cwd, rollout)?;
                    "registered"
                }
            });
        }
        sql(transaction.commit())?;
    }
    fault_point!(ReseatAfterRowInsert);
    let mut ledgered = Vec::new();
    for (item, row) in batch.iter().zip(&rows) {
        let thread = all
            .get(item.thread)
            .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
        *summary.rows.entry((*row).to_owned()).or_default() += 1;
        if *row != "conflict" {
            ledgered.push((
                thread.id.clone(),
                item.cwd.clone(),
                item.rollout.clone(),
                *row,
            ));
        }
    }
    ledger.put_thread_rows(dest_db, &ledgered)?;
    for (item, row) in batch.iter().zip(&rows) {
        let thread = all
            .get(item.thread)
            .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
        let refusal = (*row == "conflict").then_some(BulkloadRefusal::ReseatIndexConflict);
        emit(&thread_line(
            (&thread.id, &thread.cwd),
            Some(&item.cwd),
            item.outcome.word(),
            refusal.as_ref(),
            Some(row),
        ))?;
    }
    Ok(())
}

/// Insert one thread's row (and its tools and spawn edge) as the snapshot
/// has it, with the destination `cwd` and rollout.
fn insert(
    transaction: &rusqlite::Transaction<'_>,
    source: &Connection,
    names: &[String],
    thread: &Thread,
    cwd: &str,
    rollout: &str,
) -> Result<()> {
    let landed: Vec<Value> = sql(source.query_row(
        &format!("SELECT {} FROM threads WHERE id = ?1", quoted(names)),
        params![thread.id],
        |row| {
            (0..names.len())
                .map(|index| row.get::<_, Value>(index))
                .collect::<rusqlite::Result<Vec<Value>>>()
        },
    ))?;
    let values: Vec<Value> = names
        .iter()
        .zip(landed)
        .map(|(name, value)| match name.as_str() {
            "cwd" => Value::Text(cwd.to_owned()),
            "rollout_path" => Value::Text(rollout.to_owned()),
            link if SOURCE_LINKS.contains(&link) => Value::Null,
            _ => value,
        })
        .collect();
    let placeholders = (1..=names.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    sql(transaction.execute(
        &format!(
            "INSERT INTO threads({}) VALUES ({placeholders})",
            quoted(names)
        ),
        rusqlite::params_from_iter(values),
    ))?;
    for (table, key) in [
        ("thread_dynamic_tools", "thread_id"),
        ("thread_spawn_edges", "child_thread_id"),
    ] {
        copy_rows(transaction, source, table, key, &thread.id)?;
    }
    Ok(())
}

fn has_table(conn: &Connection, table: &str) -> Result<bool> {
    sql(conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |row| row.get::<_, i64>(0),
    ))
    .map(|count| count > 0)
}

/// Copy `table`'s rows whose `key` is `id` from the snapshot, insert or
/// ignore.
fn copy_rows(
    transaction: &rusqlite::Transaction<'_>,
    source: &Connection,
    table: &str,
    key: &str,
    id: &str,
) -> Result<()> {
    if !has_table(source, table)? || !has_table(transaction, table)? {
        return Ok(());
    }
    let names = shared(&columns(source, table)?, &columns(transaction, table)?)?;
    if names.is_empty() {
        return Ok(());
    }
    let mut statement = sql(source.prepare(&format!(
        "SELECT {} FROM \"{table}\" WHERE \"{key}\" = ?1",
        quoted(&names)
    )))?;
    let rows = sql(statement.query_map(params![id], |row| {
        (0..names.len())
            .map(|index| row.get::<_, Value>(index))
            .collect::<rusqlite::Result<Vec<Value>>>()
    }))?;
    let placeholders = (1..=names.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    for values in rows {
        sql(transaction.execute(
            &format!(
                "INSERT OR IGNORE INTO \"{table}\"({}) VALUES ({placeholders})",
                quoted(&names)
            ),
            rusqlite::params_from_iter(sql(values)?),
        ))?;
    }
    Ok(())
}

/// The landed `session_meta` `cwd`s of unindexed rollouts the ledger has
/// cached, for `--check`.
#[must_use]
pub fn cached_cwds(
    ledger: Option<&Ledger>,
    items: &[(String, PathBuf)],
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(ledger) = ledger else {
        return out;
    };
    for (id, landed) in items {
        if let Ok(Some(row)) = ledger.session(TOOL, id) {
            if row.landing == *landed {
                if let Some(cwd) = row.source_cwd.filter(|cwd| !cwd.is_empty()) {
                    out.insert(id.clone(), cwd);
                }
            }
        }
    }
    out
}

fn thread_line(
    (id, source_cwd): (&str, &str),
    dest_cwd: Option<&str>,
    outcome: &str,
    refusal: Option<&BulkloadRefusal>,
    row: Option<&str>,
) -> String {
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"thread\",\"tool\":\"{TOOL}\",\"id\":{},\"source_cwd\":{},\"dest_cwd\":{},\"outcome\":{},\"row\":{},\"refused\":{}}}",
        json_string(id),
        json_string(source_cwd),
        dest_cwd.map_or_else(|| "null".to_owned(), json_string),
        json_string(outcome),
        row.map_or_else(|| "null".to_owned(), json_string),
        refusal.map_or_else(|| "null".to_owned(), |refusal| json_string(refusal.code())),
    )
}
