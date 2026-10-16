//! Codex's half of the re-seat (R0, TIN-6587).
//!
//! The landed `state_5.sqlite` is the run's backup-API snapshot of the
//! source's thread index; it is opened read-only and immutable. Per thread
//! row, oldest first (a fork's parent and a spawned thread's parent are
//! older than it, so they are registered first):
//!
//! 1. Its rollout goes, byte-identical, to the same place under the live
//!    destination `.codex` (`sessions/YYYY/MM/DD/…` or
//!    `archived_sessions/…`), in the form landed (`.jsonl` or
//!    `.jsonl.zst`). A `.jsonl.gz` is kept in the landing as a file
//!    (`RESEAT_UNREADABLE_FORM`): Codex cannot read it.
//! 2. Its row is inserted into the destination's `state_5.sqlite`
//!    (`BEGIN IMMEDIATE`, a 30 s busy timeout, beside a running app-server)
//!    with two changes: `cwd` is the lane's destination working directory
//!    (the picker matches it exactly) and `rollout_path` the rollout's real
//!    destination path; the source's project and section links are cleared
//!    (they name rows of the source's own tables). Its `thread_dynamic_tools`
//!    and `thread_spawn_edges` rows go with it. Insert only: a row already
//!    there for the thread is adopted when it names the same `cwd` and
//!    rollout, and otherwise reported (`RESEAT_INDEX_CONFLICT`) and never
//!    updated. Rows commit only after their rollouts are durable.
//!
//! `thread_history_1.sqlite` is a projection Codex rebuilds from rollouts and
//! is not carried; `session_index.jsonl` and `history.jsonl` stay in the
//! landing (the picker reads neither).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension as _};

use super::ledger::Ledger;
use super::place::{Outcome, Placer};
use super::{Kind, Lanes, Options, Summary, Unmapped, SCHEMA};
use crate::closure::json_string;
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

fn sql<T>(outcome: rusqlite::Result<T>) -> Result<T> {
    outcome.map_err(|_| BulkloadRefusal::SqliteStateChanged)
}

/// Open the landed snapshot read-only and immutable: nothing is written
/// beside it, not even a `-shm`.
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
    sql(Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ))
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

/// One landed thread row.
pub struct Thread {
    pub id: String,
    pub rollout: String,
    pub cwd: String,
    values: Vec<Value>,
}

/// Every thread row of the landed snapshot, oldest first, with the values
/// of `names`.
fn threads(source: &Connection, names: &[String]) -> Result<Vec<Thread>> {
    let mut wanted = vec!["id".to_owned(), "rollout_path".to_owned(), "cwd".to_owned()];
    wanted.extend(names.iter().cloned());
    let mut statement = sql(source.prepare(&format!(
        "SELECT {} FROM threads ORDER BY created_at, id",
        quoted(&wanted)
    )))?;
    let rows = sql(statement.query_map([], |row| {
        let mut values = Vec::with_capacity(names.len());
        for index in 0..names.len() {
            values.push(row.get::<_, Value>(index + 3)?);
        }
        Ok(Thread {
            id: row.get(0)?,
            rollout: row.get(1)?,
            cwd: row.get(2)?,
            values,
        })
    }))?;
    sql(rows.collect::<rusqlite::Result<Vec<_>>>())
}

/// Every landed thread row, oldest first (identity columns only), for
/// `--check`.
///
/// # Errors
/// `SQLITE_STATE_CHANGED` when the snapshot cannot be read.
pub fn landed_threads(path: &Path) -> Result<Vec<Thread>> {
    threads(&open_snapshot(path)?, &[])
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
            .filter(|rel| rel.starts_with("sessions") || rel.starts_with("archived_sessions"))
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

/// A thread waiting for its row.
struct Pending {
    thread: usize,
    cwd: String,
    rollout: PathBuf,
    outcome: Outcome,
}

/// Re-seat every landed Codex thread.
///
/// # Errors
/// `SCHEMA_MISMATCH` when the destination's index has a required column
/// the snapshot lacks; ledger, `SQLite` and I/O refusals. A thread's own
/// refusal is reported.
pub fn reseat(
    options: &Options,
    lanes: &Lanes,
    ledger: &mut Ledger,
    summary: &mut Summary,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let root = super::root(&options.dest_codex)?;
    let source = open_snapshot(&options.landing_codex.join(STATE_DB))?;
    let dest_db = root.join(STATE_DB);
    let mut dest = if dest_db.is_file() {
        let conn = sql(Connection::open_with_flags(
            &dest_db,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ))?;
        sql(conn.busy_timeout(std::time::Duration::from_secs(30)))?;
        Some(conn)
    } else {
        None
    };
    let names = match &dest {
        Some(conn) => shared(&columns(&source, "threads")?, &columns(conn, "threads")?)?,
        None => Vec::new(),
    };
    let all = threads(&source, &names)?;
    let mut pending: Vec<Pending> = Vec::new();
    for (index, thread) in all.iter().enumerate() {
        let (rel, dest_cwd) = match locate(options, lanes, thread)? {
            Ok(located) => located,
            Err((word, refusal)) => {
                let counted = if word == "refused" {
                    refusal.code()
                } else {
                    word
                };
                summary.session(TOOL, counted);
                emit(&thread_line(thread, None, word, Some(&refusal), None))?;
                continue;
            }
        };
        let landed = options.landing_codex.join(&rel);
        let placer = Placer {
            ledger,
            root: &root,
        };
        let outcome = placer
            .place(&landed, &rel, Kind::Rollout, &thread.id, None)?
            .outcome;
        summary.file(TOOL, outcome);
        summary.session(TOOL, outcome.word());
        if !outcome.landed() {
            emit(&thread_line(
                thread,
                Some(&dest_cwd),
                outcome.word(),
                outcome.refusal().as_ref(),
                None,
            ))?;
            continue;
        }
        let rollout = root.join(&rel);
        let known = ledger.thread_row(&thread.id)?;
        if known
            .as_ref()
            .is_some_and(|(db, cwd, path)| *db == dest_db && *cwd == dest_cwd && *path == rollout)
        {
            *summary.rows.entry("unchanged".to_owned()).or_default() += 1;
            emit(&thread_line(
                thread,
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
            let index = Index {
                source: &source,
                dest: dest.as_mut(),
                db: &dest_db,
                names: &names,
            };
            register(index, &all, &mut pending, ledger, summary, emit)?;
        }
    }
    let index = Index {
        source: &source,
        dest: dest.as_mut(),
        db: &dest_db,
        names: &names,
    };
    register(index, &all, &mut pending, ledger, summary, emit)
}

/// The two thread indexes a batch is registered between.
struct Index<'a> {
    source: &'a Connection,
    dest: Option<&'a mut Connection>,
    db: &'a Path,
    names: &'a [String],
}

/// Where a thread's rollout lands and the `cwd` its row gets, or why it
/// is not re-seated: `(report word, refusal)`.
type Located = std::result::Result<(PathBuf, String), (&'static str, BulkloadRefusal)>;

fn locate(options: &Options, lanes: &Lanes, thread: &Thread) -> Result<Located> {
    let Some(rel) = rollout_rel(options, &thread.rollout) else {
        return Ok(Err(("refused", BulkloadRefusal::ReseatLaneUnmapped)));
    };
    let Some(rel) = landed_form(&options.landing_codex, &rel) else {
        return Ok(Err(("landing-absent", BulkloadRefusal::CaptureAbsent)));
    };
    if rel.extension().is_some_and(|extension| extension == "gz") {
        return Ok(Err((
            "unreadable-form",
            BulkloadRefusal::ReseatUnreadableForm,
        )));
    }
    let dest_cwd = match lanes.lane(Path::new(&thread.cwd))? {
        Some(lane) => super::key::utf8(&lane.dest_cwd)?.to_owned(),
        None if options.unmapped == Unmapped::ById => thread.cwd.clone(),
        None => return Ok(Err(("refused", BulkloadRefusal::ReseatLaneUnmapped))),
    };
    Ok(Ok((rel, dest_cwd)))
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
        source,
        dest,
        db: dest_db,
        names,
    } = index;
    if pending.is_empty() {
        return Ok(());
    }
    let batch = core::mem::take(pending);
    let Some(dest) = dest else {
        for item in &batch {
            *summary.rows.entry("index-absent".to_owned()).or_default() += 1;
            if let Some(thread) = all.get(item.thread) {
                emit(&thread_line(
                    thread,
                    Some(&item.cwd),
                    item.outcome.word(),
                    None,
                    Some("index-absent"),
                ))?;
            }
        }
        return Ok(());
    };
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
                Some((cwd, path)) if cwd == item.cwd && path == rollout => "adopted",
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
            thread,
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
    let values: Vec<Value> = names
        .iter()
        .zip(&thread.values)
        .map(|(name, value)| match name.as_str() {
            "cwd" => Value::Text(cwd.to_owned()),
            "rollout_path" => Value::Text(rollout.to_owned()),
            link if SOURCE_LINKS.contains(&link) => Value::Null,
            _ => value.clone(),
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

fn thread_line(
    thread: &Thread,
    dest_cwd: Option<&str>,
    outcome: &str,
    refusal: Option<&BulkloadRefusal>,
    row: Option<&str>,
) -> String {
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"thread\",\"tool\":\"{TOOL}\",\"id\":{},\"source_cwd\":{},\"dest_cwd\":{},\"outcome\":{},\"row\":{},\"refused\":{}}}",
        json_string(&thread.id),
        json_string(&thread.cwd),
        dest_cwd.map_or_else(|| "null".to_owned(), json_string),
        json_string(outcome),
        row.map_or_else(|| "null".to_owned(), json_string),
        refusal.map_or_else(|| "null".to_owned(), |refusal| json_string(refusal.code())),
    )
}
