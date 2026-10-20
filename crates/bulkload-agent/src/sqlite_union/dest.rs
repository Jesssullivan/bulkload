//! The write side: merge a source store's rows into the destination's live
//! store (see the module documentation).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bulkload_proto::rows::{
    self, Go, Reply, Request, Row, Store, TableSchema, Value, Want, ROWS_VERSION,
};
use rusqlite::{Connection, OpenFlags};

use super::ledger::{Entry, Ledger, Marks, Pending};
use super::{quote, read_message, read_store, sql, text, write_message, Bind, MIGRATIONS};
use crate::counters::{self, Counter};
use crate::{BulkloadRefusal, Result};

/// Rows per write transaction at most.
pub const BATCH_ROWS: usize = 256;

/// A write transaction is committed once it has been open this long, so a
/// live writer on the destination waits at most about this.
pub const BATCH_WALL: Duration = Duration::from_millis(20);

/// The busy timeout on the destination store: how long one try for its
/// write lock waits for a live writer.
pub const BUSY: Duration = Duration::from_secs(5);

/// How long a batch keeps trying for the write lock in all.
const BEGIN_WALL: Duration = Duration::from_mins(5);

/// Natural-key marks are saved after this many committed batches, and at
/// the table's end.
const MARK_EVERY: u64 = 32;

/// Columns that order two versions of one row, in preference order.
const VERSION_COLUMNS: [&str; 3] = ["updated_at_ms", "updated_at", "updated_at_ordinal"];

/// Columns a changed row's time is read from for a rerun, in preference
/// order.
const TIME_COLUMNS: [&str; 2] = ["updated_at_ms", "updated_at"];

/// One union's inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The source store's name in the ledger (`HOST:PATH`).
    pub source: String,
    /// The source database's absolute path on its host.
    pub source_db: Vec<u8>,
    /// The destination's live store.
    pub dest: PathBuf,
    /// The private state directory the ledger lives in.
    pub state: PathBuf,
    /// `--map SRC=DST`: path prefixes re-spelled in path columns.
    pub maps: Vec<(Vec<u8>, Vec<u8>)>,
    /// `--full`: read every row, whatever the marks say.
    pub full: bool,
}

/// What one union did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnionStats {
    /// The source store was the one the last pass ended with: nothing read.
    pub unchanged: bool,
    pub tables: u64,
    /// Rows received from the source.
    pub received: u64,
    /// Rows the destination did not hold, inserted.
    pub inserted: u64,
    /// Rows the destination held equal: nothing written.
    pub adopted: u64,
    /// Destination rows replaced by a later version, each ledgered first.
    pub replaced: u64,
    /// Source rows the destination's version outranked, ledgered.
    pub kept: u64,
    /// Source rows a constraint refused, ledgered.
    pub refused: u64,
    /// Path values re-spelled through `--map`.
    pub respelled: u64,
    /// Write transactions committed.
    pub batches: u64,
    /// The longest write transaction.
    pub longest_batch: Duration,
    /// How long the source held its read snapshot.
    pub held_ns: u64,
    /// `foreign_key_check` rows after the pass.
    pub fk_violations: u64,
}

/// How a table's rows are matched.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    /// By its primary key's columns.
    Natural(Vec<usize>),
    /// `INTEGER PRIMARY KEY AUTOINCREMENT` at this column: a new key here.
    Surrogate(usize),
    /// No declared key: the rowid, a new one here.
    Rowid,
}

/// One table's merge plan.
#[derive(Debug, Clone)]
struct Plan {
    name: String,
    raw: Vec<u8>,
    columns: Vec<String>,
    raw_columns: Vec<Vec<u8>>,
    key: Key,
    version: Option<usize>,
    time: Option<Vec<u8>>,
    uniques: Vec<Vec<usize>>,
    paths: Vec<usize>,
}

/// Merge one source store into the destination's live store over a row
/// stream (`input` from the source, `output` to it).
///
/// # Errors
/// `SQLITE_UNION_TARGET_ABSENT`, `SQLITE_SCHEMA_MISMATCH` (nothing written),
/// `SQLITE_UNION_SCHEMA_UNSUPPORTED`, the source's refusal, the destination
/// store's failure (`SQLITE_BACKUP_FAILED`), `SQLITE_STATE_CHANGED` when its
/// write lock stays busy past the batch wall, and
/// `SQLITE_INTEGRITY_CHECK_FAILED` when its `integrity_check` is not `ok`
/// after the pass.
#[allow(
    clippy::too_many_lines,
    reason = "the session's messages, in order, read top to bottom"
)]
pub fn consolidate<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    options: &Options,
) -> Result<UnionStats> {
    let (dest, seat) = open_dest(&options.dest)?;
    let store_conn = super::ledger::open_store(&options.state)?;
    let ledger = Ledger::new(
        &store_conn,
        &options.source,
        &options.dest.to_string_lossy(),
    )?;
    let (store, schema_version) = read_dest(&dest)?;
    let plans = plan(&dest, &store)?;
    for plan in &plans {
        recover(&dest, &ledger, plan)?;
    }
    let last = if options.full {
        None
    } else {
        ledger.identity()?
    };
    write_message(
        output,
        &Request {
            version: ROWS_VERSION,
            schema: rows::rows_id(),
            db: options.source_db.clone(),
            last,
        },
    )?;
    let (identity, source_store) = match read_message::<Reply, R>(input)? {
        Reply::Hello {
            version,
            schema,
            identity,
            store,
        } if version == ROWS_VERSION && schema == rows::rows_id() => (identity, store),
        Reply::Hello { .. } => return Err(BulkloadRefusal::SchemaMismatch),
        Reply::Refused { code, sqlite_code } => return Err(source_refusal(&code, sqlite_code)),
        _ => return Err(BulkloadRefusal::ProtocolStateViolation),
    };
    let mut stats = UnionStats::default();
    let Some(source_store) = source_store else {
        if last != Some(identity) {
            return Err(BulkloadRefusal::ProtocolStateViolation);
        }
        stats.unchanged = true;
        return Ok(stats);
    };
    if source_store != store {
        let _ = write_message(output, &Go::Stop);
        return Err(BulkloadRefusal::SqliteSchemaMismatch);
    }
    let mut wants = Vec::with_capacity(plans.len());
    for plan in &plans {
        let marks = marks_for(&ledger, plan, options.full)?;
        wants.push(Want {
            table: plan.raw.clone(),
            time: plan.time.clone(),
            after_rowid: marks.after_rowid,
            after_time: marks.after_time,
        });
    }
    write_message(output, &Go::Tables(wants))?;
    let mut merge = Merge {
        dest: &dest,
        path: &options.dest,
        seat,
        ledger: &ledger,
        maps: &options.maps,
        schema_version,
        stats,
    };
    let mut current: Option<Table<'_>> = None;
    loop {
        match read_message::<Reply, R>(input)? {
            Reply::Table {
                name,
                columns,
                time_high,
            } => {
                if current.is_some() {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                let plan = plans
                    .iter()
                    .find(|plan| plan.raw == name)
                    .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
                if columns != plan.raw_columns {
                    return Err(BulkloadRefusal::SqliteSchemaMismatch);
                }
                let marks = marks_for(merge.ledger, plan, options.full)?;
                current = Some(Table {
                    plan,
                    marks,
                    time_high,
                    batches: 0,
                });
            }
            Reply::Rows(rows) => {
                let table = current
                    .as_mut()
                    .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
                merge.rows(table, &rows)?;
            }
            Reply::TableEnd { .. } => {
                let table = current
                    .take()
                    .ok_or(BulkloadRefusal::ProtocolStateViolation)?;
                merge.end(table)?;
            }
            Reply::End { held_ns, .. } => {
                if current.is_some() {
                    return Err(BulkloadRefusal::ProtocolStateViolation);
                }
                merge.stats.held_ns = held_ns;
                break;
            }
            Reply::Refused { code, sqlite_code } => {
                return Err(source_refusal(&code, sqlite_code));
            }
            Reply::Hello { .. } => return Err(BulkloadRefusal::ProtocolStateViolation),
        }
    }
    let mut stats = merge.stats;
    ledger.set_identity(&identity)?;
    stats.fk_violations = check(&dest)?;
    Ok(stats)
}

/// A table's marks for this pass. `--full` reads every natural-key row
/// again; a surrogate-key table keeps its rowid mark even then, since its
/// rows are inserted with new keys and must never be read twice.
fn marks_for(ledger: &Ledger<'_>, plan: &Plan, full: bool) -> Result<Marks> {
    if full && matches!(plan.key, Key::Natural(_)) {
        Ok(Marks::default())
    } else {
        ledger.marks(&plan.name)
    }
}

/// The live store's `(dev, ino)` at its path, checked to be this user's
/// regular file (a link is never followed).
fn seat_of(path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    match std::fs::symlink_metadata(path) {
        Ok(found) if found.is_file() => {
            if found.uid() != crate::io::sys::effective_uid() {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
            Ok((found.dev(), found.ino()))
        }
        Ok(_) => Err(BulkloadRefusal::DestinationKindMismatch),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(BulkloadRefusal::SqliteUnionTargetAbsent)
        }
        Err(error) => Err(BulkloadRefusal::Io(error.raw_os_error())),
    }
}

/// Open the live destination store by path, then see it is still the file
/// seen before the open (the rule #297 states for the homes' stores):
/// never created, never followed through a link, never switched out of its
/// journal mode. Its commits are
/// `synchronous=FULL` (a ledger mark is never ahead of the rows it names),
/// it enforces foreign keys, and it never checkpoints on close.
fn open_dest(path: &Path) -> Result<(Connection, (u64, u64))> {
    let seat = seat_of(path)?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(sql)?;
    conn.busy_timeout(BUSY).map_err(sql)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
        .map_err(sql)?;
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .map_err(sql)?;
    if seat_of(path)? != seat {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    Ok((conn, seat))
}

/// The destination's schema and `schema_version`, read in one snapshot.
fn read_dest(dest: &Connection) -> Result<(Store, i64)> {
    dest.execute_batch("BEGIN").map_err(sql)?;
    let read = read_store(dest).and_then(|store| {
        dest.query_row("PRAGMA schema_version", [], |row| row.get(0))
            .map(|version| (store, version))
            .map_err(sql)
    });
    let _ = dest.execute_batch("COMMIT");
    read
}

/// Every merged table's plan, parents before children.
fn plan(dest: &Connection, store: &Store) -> Result<Vec<Plan>> {
    let mut plans = BTreeMap::new();
    let mut parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut surrogates = BTreeSet::new();
    for table in &store.tables {
        if table.name == MIGRATIONS.as_bytes() {
            continue;
        }
        if table.sql.starts_with(b"virtual:") || table.sql.starts_with(b"shadow:") {
            return Err(BulkloadRefusal::SqliteUnionSchemaUnsupported);
        }
        let plan = table_plan(dest, table)?;
        if matches!(plan.key, Key::Surrogate(_) | Key::Rowid) {
            surrogates.insert(plan.name.clone());
        }
        let mut statement = dest
            .prepare(&format!("PRAGMA foreign_key_list({})", quote(&plan.name)))
            .map_err(sql)?;
        let referenced = statement
            .query_map([], |row| row.get::<_, String>(2))
            .map_err(sql)?
            .collect::<rusqlite::Result<BTreeSet<_>>>()
            .map_err(sql)?;
        parents.insert(plan.name.clone(), referenced);
        plans.insert(plan.name.clone(), plan);
    }
    // A key that means nothing on another host cannot be referenced.
    if parents
        .values()
        .any(|referenced| referenced.iter().any(|parent| surrogates.contains(parent)))
    {
        return Err(BulkloadRefusal::SqliteUnionSchemaUnsupported);
    }
    // Parents first; a cycle is broken by name order (its rows may then be
    // refused by the foreign key, and are ledgered).
    let mut ordered = Vec::with_capacity(plans.len());
    let mut placed = BTreeSet::new();
    while placed.len() < plans.len() {
        let ready: Vec<String> = plans
            .keys()
            .filter(|name| !placed.contains(*name))
            .filter(|name| {
                parents.get(*name).is_none_or(|referenced| {
                    referenced.iter().all(|parent| {
                        parent == *name || placed.contains(parent) || !plans.contains_key(parent)
                    })
                })
            })
            .cloned()
            .collect();
        let next = if ready.is_empty() {
            plans
                .keys()
                .find(|name| !placed.contains(*name))
                .cloned()
                .into_iter()
                .collect()
        } else {
            ready
        };
        for name in next {
            placed.insert(name.clone());
            if let Some(plan) = plans.get(&name) {
                ordered.push(plan.clone());
            }
        }
    }
    Ok(ordered)
}

fn table_plan(dest: &Connection, table: &TableSchema) -> Result<Plan> {
    let name = text(&table.name)?.to_owned();
    let mut columns = Vec::with_capacity(table.columns.len());
    let mut names = Vec::with_capacity(table.columns.len());
    let mut key = Vec::new();
    for (at, column) in table.columns.iter().enumerate() {
        if column.hidden != 0 {
            return Err(BulkloadRefusal::SqliteUnionSchemaUnsupported);
        }
        let column_name = text(&column.name)?;
        columns.push(quote(column_name));
        names.push(column_name.to_owned());
        if column.pk != 0 {
            key.push((column.pk, at));
        }
    }
    key.sort_unstable();
    let key: Vec<usize> = key.into_iter().map(|(_, at)| at).collect();
    let autoincrement = String::from_utf8_lossy(&table.sql)
        .to_ascii_uppercase()
        .contains("AUTOINCREMENT");
    let key = match key.as_slice() {
        [] if table.rowid => Key::Rowid,
        [] => return Err(BulkloadRefusal::SqliteUnionSchemaUnsupported),
        [only]
            if table.rowid
                && autoincrement
                && table
                    .columns
                    .get(*only)
                    .is_some_and(|column| column.decl.eq_ignore_ascii_case(b"INTEGER")) =>
        {
            Key::Surrogate(*only)
        }
        _ => Key::Natural(key),
    };
    let find = |wanted: &[&str]| {
        wanted
            .iter()
            .find_map(|candidate| names.iter().position(|name| name == candidate))
    };
    let version = find(&VERSION_COLUMNS);
    let time = match key {
        Key::Natural(_) => find(&TIME_COLUMNS)
            .and_then(|at| table.columns.get(at))
            .map(|column| column.name.clone()),
        Key::Surrogate(_) | Key::Rowid => None,
    };
    let paths = names
        .iter()
        .enumerate()
        .filter(|(_, name)| *name == "cwd" || *name == "path" || name.ends_with("_path"))
        .map(|(at, _)| at)
        .collect();
    Ok(Plan {
        uniques: uniques(dest, &name, &names)?,
        name,
        raw: table.name.clone(),
        columns,
        raw_columns: table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        key,
        version,
        time,
        paths,
    })
}

/// The table's `UNIQUE` constraints other than its primary key, as column
/// positions (an index over an expression or a partial index is left out:
/// a row it refuses is ledgered as a constraint refusal).
fn uniques(dest: &Connection, table: &str, names: &[String]) -> Result<Vec<Vec<usize>>> {
    let mut list = dest
        .prepare(&format!("PRAGMA index_list({})", quote(table)))
        .map_err(sql)?;
    let indexes = list
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    let mut found = Vec::new();
    'indexes: for (index, unique, origin, partial) in indexes {
        if unique == 0 || origin == "pk" || partial != 0 {
            continue;
        }
        let mut info = dest
            .prepare(&format!("PRAGMA index_info({})", quote(&index)))
            .map_err(sql)?;
        let columns = info
            .query_map([], |row| row.get::<_, Option<String>>(2))
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        let mut positions = Vec::with_capacity(columns.len());
        for column in columns {
            let Some(at) = column.and_then(|column| names.iter().position(|name| *name == column))
            else {
                continue 'indexes;
            };
            positions.push(at);
        }
        found.push(positions);
    }
    Ok(found)
}

/// A pending surrogate batch from an interrupted pass: confirmed when its
/// rows are in the destination as ledgered, else dropped so they are read
/// again. Either way no row is inserted twice.
fn recover(dest: &Connection, ledger: &Ledger<'_>, plan: &Plan) -> Result<()> {
    let Some(pending) = ledger.pending(&plan.name)? else {
        return Ok(());
    };
    let values = non_key_columns(plan);
    let mut statement = dest
        .prepare(&format!(
            "SELECT {} FROM {} WHERE rowid BETWEEN ?1 AND ?2 ORDER BY rowid",
            values.join(", "),
            quote(&plan.name)
        ))
        .map_err(sql)?;
    let found = statement
        .query_map([pending.low, pending.high], |row| {
            (0..values.len())
                .map(|at| row.get_ref(at).map(super::value_of))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    if digest(&found) == pending.digest {
        let mut marks = ledger.marks(&plan.name)?;
        marks.after_rowid = Some(
            marks
                .after_rowid
                .map_or(pending.to_rowid, |at| at.max(pending.to_rowid)),
        );
        ledger.confirm(&plan.name, &marks)
    } else {
        ledger.abandon(&plan.name)
    }
}

/// The columns of `plan` a surrogate row is compared and inserted by: every
/// one but its key.
fn non_key_columns(plan: &Plan) -> Vec<String> {
    plan.columns
        .iter()
        .enumerate()
        .filter(|(at, _)| !matches!(plan.key, Key::Surrogate(key) if key == *at))
        .map(|(_, column)| column.clone())
        .collect()
}

pub(super) fn digest<T: serde::Serialize + ?Sized>(value: &T) -> [u8; 32] {
    *blake3::hash(&postcard::to_stdvec(value).unwrap_or_default()).as_bytes()
}

/// A source refusal by its code, as the source sent it.
fn source_refusal(code: &str, sqlite_code: Option<i32>) -> BulkloadRefusal {
    [
        BulkloadRefusal::SourceVanished,
        BulkloadRefusal::SqliteSourceAsRoot,
        BulkloadRefusal::SqliteSourceNotOwner,
        BulkloadRefusal::SqliteUnionSourceNotWal,
        BulkloadRefusal::SqliteUnionSchemaUnsupported,
        BulkloadRefusal::SqliteStateChanged,
        BulkloadRefusal::BudgetExceeded,
        BulkloadRefusal::SchemaMismatch,
        BulkloadRefusal::PathNotAbsolute,
        BulkloadRefusal::FieldDomainViolation,
        BulkloadRefusal::TransportLost,
        BulkloadRefusal::FrameCodec,
        BulkloadRefusal::SqliteBackupFailed(sqlite_code),
    ]
    .into_iter()
    .find(|refusal| refusal.code() == code)
    .unwrap_or(BulkloadRefusal::ProtocolStateViolation)
}

/// The destination's checks after a pass: `integrity_check` must say `ok`;
/// the `foreign_key_check` rows are counted.
fn check(dest: &Connection) -> Result<u64> {
    let mut statement = dest.prepare("PRAGMA integrity_check").map_err(sql)?;
    let verdict = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    counters::bump(Counter::DestSqliteUnionChecked);
    if verdict != ["ok"] {
        return Err(BulkloadRefusal::SqliteIntegrityCheckFailed);
    }
    let mut statement = dest.prepare("PRAGMA foreign_key_check").map_err(sql)?;
    let violations = statement.query_map([], |_| Ok(())).map_err(sql)?.count();
    Ok(u64::try_from(violations).unwrap_or(u64::MAX))
}

/// The table being merged.
struct Table<'p> {
    plan: &'p Plan,
    marks: Marks,
    time_high: Option<Value>,
    batches: u64,
}

/// The merge's state.
struct Merge<'m> {
    dest: &'m Connection,
    /// The live store's path and the `(dev, ino)` it was opened as: every
    /// batch sees it still there before writing.
    path: &'m Path,
    seat: (u64, u64),
    ledger: &'m Ledger<'m>,
    maps: &'m [(Vec<u8>, Vec<u8>)],
    schema_version: i64,
    stats: UnionStats,
}

/// What one batch did to its rows.
#[derive(Default)]
struct Batch {
    consumed: usize,
    entries: Vec<Entry>,
    inserted: Vec<(i64, Vec<Value>)>,
    to_rowid: Option<i64>,
}

impl Merge<'_> {
    fn rows(&mut self, table: &mut Table<'_>, rows: &[Row]) -> Result<()> {
        self.stats.received = self
            .stats
            .received
            .saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX));
        let mut rest = rows;
        while !rest.is_empty() {
            let consumed = self.batch(table, rest)?;
            rest = rest.get(consumed..).unwrap_or_default();
        }
        Ok(())
    }

    /// One write transaction over the first rows of `rows`; how many it took.
    fn batch(&mut self, table: &mut Table<'_>, rows: &[Row]) -> Result<usize> {
        self.begin()?;
        let started = Instant::now();
        let mut batch = Batch::default();
        let applied = self
            .apply(table.plan, rows, started, &mut batch)
            .and_then(|()| {
                let pending =
                    if batch.inserted.is_empty() || matches!(table.plan.key, Key::Natural(_)) {
                        None
                    } else {
                        let low = batch.inserted.first().map_or(0, |(at, _)| *at);
                        let high = batch.inserted.last().map_or(0, |(at, _)| *at);
                        let values: Vec<&Vec<Value>> =
                            batch.inserted.iter().map(|(_, values)| values).collect();
                        Some(Pending {
                            to_rowid: batch.to_rowid.unwrap_or(0),
                            low,
                            high,
                            digest: digest(&values),
                        })
                    };
                // Ledgered before the commit that changes the destination: a
                // displaced row is never lost, and a surrogate batch never
                // goes in twice.
                self.ledger.before_commit(
                    &batch.entries,
                    pending
                        .as_ref()
                        .map(|pending| (table.plan.name.as_str(), pending)),
                )
            });
        if let Err(refusal) = applied {
            let _ = self.dest.execute_batch("ROLLBACK");
            return Err(refusal);
        }
        if let Err(error) = self.dest.execute_batch("COMMIT") {
            let _ = self.dest.execute_batch("ROLLBACK");
            return Err(sql(error));
        }
        let took = started.elapsed();
        self.stats.batches += 1;
        self.stats.longest_batch = self.stats.longest_batch.max(took);
        counters::bump(Counter::DestSqliteUnionBatches);
        counters::add(
            Counter::DestSqliteUnionWriteNs,
            u64::try_from(took.as_nanos()).unwrap_or(u64::MAX),
        );
        if let Some(to) = batch.to_rowid {
            table.marks.after_rowid = Some(table.marks.after_rowid.map_or(to, |at| at.max(to)));
        }
        table.batches += 1;
        // A rowid mark alone; the time mark moves only at the table's end.
        // A surrogate batch's mark confirms its pending inserts at once. A
        // table with a time column keeps its marks until its end: a pass
        // cut short then reads it again from the marks it had, so no row
        // changed between passes is ever below both marks unread.
        let surrogate = !matches!(table.plan.key, Key::Natural(_));
        if surrogate || (table.plan.time.is_none() && table.batches.is_multiple_of(MARK_EVERY)) {
            let mut marks = self.ledger.marks(&table.plan.name)?;
            marks.after_rowid = table.marks.after_rowid;
            self.ledger.confirm(&table.plan.name, &marks)?;
        }
        Ok(batch.consumed)
    }

    /// Take the destination's write lock, waiting out a live writer within
    /// [`BEGIN_WALL`], and see its schema unchanged.
    fn begin(&self) -> Result<()> {
        // The store opened is still the one at its path (#297's rule).
        if seat_of(self.path)? != self.seat {
            return Err(BulkloadRefusal::SqliteStateChanged);
        }
        let started = Instant::now();
        let mut backoff = Duration::from_millis(10);
        loop {
            match self.dest.execute_batch("BEGIN IMMEDIATE") {
                Ok(()) => break,
                Err(error) if crate::provider_sqlite::busy_failure(&error) => {
                    if started.elapsed() > BEGIN_WALL {
                        return Err(BulkloadRefusal::SqliteStateChanged);
                    }
                    counters::bump(Counter::DestSqliteUnionBusy);
                    std::thread::sleep(backoff);
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(1));
                }
                Err(error) => return Err(sql(error)),
            }
        }
        let version: rusqlite::Result<i64> =
            self.dest
                .query_row("PRAGMA schema_version", [], |row| row.get(0));
        match version {
            Ok(version) if version == self.schema_version => Ok(()),
            Ok(_) => {
                let _ = self.dest.execute_batch("ROLLBACK");
                Err(BulkloadRefusal::SqliteSchemaMismatch)
            }
            Err(error) => {
                let _ = self.dest.execute_batch("ROLLBACK");
                Err(sql(error))
            }
        }
    }

    fn apply(
        &mut self,
        plan: &Plan,
        rows: &[Row],
        started: Instant,
        batch: &mut Batch,
    ) -> Result<()> {
        for row in rows {
            if batch.consumed >= BATCH_ROWS
                || (batch.consumed > 0 && started.elapsed() >= BATCH_WALL)
            {
                break;
            }
            batch.consumed += 1;
            if let Some(rowid) = row.rowid {
                batch.to_rowid = Some(batch.to_rowid.map_or(rowid, |at| at.max(rowid)));
            }
            let mut values = row.values.clone();
            if values.len() != plan.columns.len() {
                return Err(BulkloadRefusal::ProtocolStateViolation);
            }
            for at in &plan.paths {
                if let Some(value) = values.get_mut(*at) {
                    if respell(self.maps, value) {
                        self.stats.respelled += 1;
                        counters::bump(Counter::DestSqliteRowsRespelled);
                    }
                }
            }
            match &plan.key {
                Key::Natural(key) => self.natural(plan, key, &values, batch)?,
                Key::Surrogate(_) | Key::Rowid => self.surrogate(plan, &values, batch)?,
            }
        }
        Ok(())
    }

    fn natural(
        &mut self,
        plan: &Plan,
        key: &[usize],
        values: &[Value],
        batch: &mut Batch,
    ) -> Result<()> {
        let Some(held) = self.select(plan, key, values)? else {
            return self.insert(plan, values, None, batch);
        };
        if held.as_slice() == values {
            self.stats.adopted += 1;
            counters::bump(Counter::DestSqliteRowsAdopted);
            return Ok(());
        }
        let key_json = json_of(plan, key, values);
        match newer(plan.version, values, &held) {
            Order::Later => {
                let assignments: Vec<String> = plan
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| !key.contains(at))
                    .map(|(_, column)| format!("{column} = ?"))
                    .collect();
                if assignments.is_empty() {
                    return Ok(());
                }
                let mut bound: Vec<Bind<'_>> = values
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| !key.contains(at))
                    .map(|(_, value)| Bind(value))
                    .collect();
                bound.extend(key.iter().filter_map(|at| values.get(*at)).map(Bind));
                let statement = format!(
                    "UPDATE {} SET {} WHERE {}",
                    quote(&plan.name),
                    assignments.join(", "),
                    matching(plan, key)
                );
                match self
                    .dest
                    .execute(&statement, rusqlite::params_from_iter(bound.iter()))
                {
                    Ok(_) => {
                        self.stats.replaced += 1;
                        counters::bump(Counter::DestSqliteRowsReplaced);
                        batch.entries.push(Entry {
                            table: plan.name.clone(),
                            key: key_json,
                            kept: "source",
                            reason: "newer".to_owned(),
                            digest: digest(&held),
                            row: row_json(plan, &held),
                            other: Some(digest(values)),
                        });
                        Ok(())
                    }
                    Err(error) => self.refuse(plan, key_json, values, Some(&held), &error, batch),
                }
            }
            order => {
                self.stats.kept += 1;
                counters::bump(Counter::DestSqliteRowsKept);
                batch.entries.push(Entry {
                    table: plan.name.clone(),
                    key: key_json,
                    kept: "dest",
                    reason: order.word().to_owned(),
                    digest: digest(values),
                    row: row_json(plan, values),
                    other: Some(digest(&held)),
                });
                Ok(())
            }
        }
    }

    fn surrogate(&mut self, plan: &Plan, values: &[Value], batch: &mut Batch) -> Result<()> {
        let skip = match plan.key {
            Key::Surrogate(at) => Some(at),
            _ => None,
        };
        let content = |row: &[Value]| -> Vec<Value> {
            row.iter()
                .enumerate()
                .filter(|(at, _)| Some(*at) != skip)
                .map(|(_, value)| value.clone())
                .collect()
        };
        for unique in &plan.uniques {
            if let Some(held) = self.select(plan, unique, values)? {
                if content(&held) == content(values) {
                    self.stats.adopted += 1;
                    counters::bump(Counter::DestSqliteRowsAdopted);
                } else {
                    self.stats.kept += 1;
                    counters::bump(Counter::DestSqliteRowsKept);
                    batch.entries.push(Entry {
                        table: plan.name.clone(),
                        key: json_of(plan, unique, values),
                        kept: "dest",
                        reason: "unique".to_owned(),
                        digest: digest(values),
                        row: row_json(plan, values),
                        other: Some(digest(&held)),
                    });
                }
                return Ok(());
            }
        }
        self.insert(plan, values, skip, batch)
    }

    /// The destination's row whose `key` columns equal `values`'.
    fn select(&self, plan: &Plan, key: &[usize], values: &[Value]) -> Result<Option<Vec<Value>>> {
        let statement = format!(
            "SELECT {} FROM {} WHERE {}",
            plan.columns.join(", "),
            quote(&plan.name),
            matching(plan, key)
        );
        let mut statement = self.dest.prepare_cached(&statement).map_err(sql)?;
        let bound: Vec<Bind<'_>> = key
            .iter()
            .filter_map(|at| values.get(*at))
            .map(Bind)
            .collect();
        let mut found = statement
            .query(rusqlite::params_from_iter(bound.iter()))
            .map_err(sql)?;
        let Some(row) = found.next().map_err(sql)? else {
            return Ok(None);
        };
        let held = (0..plan.columns.len())
            .map(|at| row.get_ref(at).map(super::value_of))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        Ok(Some(held))
    }

    /// Insert a row the destination does not hold; `skip` is a surrogate
    /// key, which the destination assigns.
    fn insert(
        &mut self,
        plan: &Plan,
        values: &[Value],
        skip: Option<usize>,
        batch: &mut Batch,
    ) -> Result<()> {
        let columns: Vec<&String> = plan
            .columns
            .iter()
            .enumerate()
            .filter(|(at, _)| Some(*at) != skip)
            .map(|(_, column)| column)
            .collect();
        let inserted: Vec<Value> = values
            .iter()
            .enumerate()
            .filter(|(at, _)| Some(*at) != skip)
            .map(|(_, value)| value.clone())
            .collect();
        let statement = if columns.is_empty() {
            format!("INSERT INTO {} DEFAULT VALUES", quote(&plan.name))
        } else {
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                quote(&plan.name),
                columns
                    .iter()
                    .map(|column| column.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                vec!["?"; columns.len()].join(", ")
            )
        };
        let outcome = self
            .dest
            .prepare_cached(&statement)
            .and_then(|mut statement| {
                statement.execute(rusqlite::params_from_iter(inserted.iter().map(Bind)))
            });
        match outcome {
            Ok(_) => {
                self.stats.inserted += 1;
                counters::bump(Counter::DestSqliteRowsInserted);
                if !matches!(plan.key, Key::Natural(_)) {
                    batch
                        .inserted
                        .push((self.dest.last_insert_rowid(), inserted));
                }
                Ok(())
            }
            Err(error) => {
                let key = match &plan.key {
                    Key::Natural(key) => json_of(plan, key, values),
                    Key::Surrogate(_) | Key::Rowid => "{}".to_owned(),
                };
                self.refuse(plan, key, values, None, &error, batch)
            }
        }
    }

    /// A row a constraint refused is ledgered; any other failure stops the
    /// pass.
    fn refuse(
        &mut self,
        plan: &Plan,
        key: String,
        values: &[Value],
        held: Option<&Vec<Value>>,
        error: &rusqlite::Error,
        batch: &mut Batch,
    ) -> Result<()> {
        let rusqlite::Error::SqliteFailure(failure, _) = error else {
            return Err(BulkloadRefusal::SqliteBackupFailed(None));
        };
        if failure.code != rusqlite::ErrorCode::ConstraintViolation {
            return Err(BulkloadRefusal::SqliteBackupFailed(Some(
                failure.extended_code,
            )));
        }
        self.stats.refused += 1;
        counters::bump(Counter::DestSqliteRowsRefused);
        batch.entries.push(Entry {
            table: plan.name.clone(),
            key,
            kept: "dest",
            reason: format!("constraint-{}", failure.extended_code),
            digest: digest(values),
            row: row_json(plan, values),
            other: held.map(digest),
        });
        Ok(())
    }

    /// A table read to its end: its rowid mark, and its time mark at the
    /// snapshot's high value (every row the last marks wanted was read).
    fn end(&mut self, table: Table<'_>) -> Result<()> {
        let mut marks = Marks {
            after_rowid: table.marks.after_rowid,
            after_time: None,
        };
        if table.plan.time.is_some() {
            // An empty or all-NULL column: every row with a time is wanted
            // next time (no integer or text sorts below this).
            marks.after_time = Some(match table.time_high {
                Some(Value::Null) | None => Value::Integer(i64::MIN),
                Some(high) => high,
            });
        }
        self.ledger.confirm(&table.plan.name, &marks)?;
        self.stats.tables += 1;
        Ok(())
    }
}

/// `"a" IS ? AND "b" IS ?` over `key`.
fn matching(plan: &Plan, key: &[usize]) -> String {
    key.iter()
        .filter_map(|at| plan.columns.get(*at))
        .map(|column| format!("{column} IS ?"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// How the source's version of a row orders against the destination's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    Later,
    Older,
    Same,
    Unversioned,
}

impl Order {
    const fn word(self) -> &'static str {
        match self {
            Self::Later => "newer",
            Self::Older => "older",
            Self::Same => "same-version",
            Self::Unversioned => "unversioned",
        }
    }
}

/// The source row's version against the destination's: only a strictly
/// later one replaces a row.
fn newer(version: Option<usize>, source: &[Value], dest: &[Value]) -> Order {
    let Some(at) = version else {
        return Order::Unversioned;
    };
    let ordering = match (source.get(at), dest.get(at)) {
        (Some(Value::Integer(a)), Some(Value::Integer(b))) => a.cmp(b),
        (Some(Value::Real(a)), Some(Value::Real(b))) => {
            match f64::from_bits(*a).partial_cmp(&f64::from_bits(*b)) {
                Some(ordering) => ordering,
                None => return Order::Unversioned,
            }
        }
        (Some(Value::Text(a)), Some(Value::Text(b))) => a.cmp(b),
        _ => return Order::Unversioned,
    };
    match ordering {
        std::cmp::Ordering::Greater => Order::Later,
        std::cmp::Ordering::Less => Order::Older,
        std::cmp::Ordering::Equal => Order::Same,
    }
}

/// Re-spell a path value through the longest mapped prefix it is equal to
/// or under (A-055).
fn respell(maps: &[(Vec<u8>, Vec<u8>)], value: &mut Value) -> bool {
    let Value::Text(path) = value else {
        return false;
    };
    for (from, to) in maps {
        let Some(rest) = path.strip_prefix(from.as_slice()) else {
            continue;
        };
        if !(rest.is_empty() || rest.first() == Some(&b'/')) {
            continue;
        }
        let mut spelled = to.clone();
        spelled.extend_from_slice(rest);
        if spelled == *path {
            return false;
        }
        *path = spelled;
        return true;
    }
    false
}

/// `--map SRC=DST` pairs, longest source first, without trailing slashes.
#[must_use]
pub fn maps(pairs: &[(Vec<u8>, Vec<u8>)]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let trim = |path: &[u8]| -> Vec<u8> {
        let mut path = path.to_vec();
        while path.len() > 1 && path.last() == Some(&b'/') {
            path.pop();
        }
        path
    };
    let mut maps: Vec<(Vec<u8>, Vec<u8>)> = pairs
        .iter()
        .map(|(from, to)| (trim(from), trim(to)))
        .collect();
    maps.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    maps
}

/// The `key` columns of `values` as a JSON object.
fn json_of(plan: &Plan, key: &[usize], values: &[Value]) -> String {
    let fields: Vec<String> = key
        .iter()
        .filter_map(|at| Some((plan.raw_columns.get(*at)?, values.get(*at)?)))
        .map(|(name, value)| format!("{}:{}", json_text(name), json_value(value)))
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// A whole row as a JSON object.
fn row_json(plan: &Plan, values: &[Value]) -> String {
    let every: Vec<usize> = (0..plan.raw_columns.len()).collect();
    json_of(plan, &every, values)
}

fn json_text(bytes: &[u8]) -> String {
    crate::closure::json_string(&String::from_utf8_lossy(bytes))
}

/// One value as JSON: text that is not UTF-8 and blobs as hex objects, so
/// the ledger keeps every byte.
fn json_value(value: &Value) -> String {
    let hex = |bytes: &[u8]| {
        bytes
            .iter()
            .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            })
    };
    match value {
        Value::Null => "null".to_owned(),
        Value::Integer(integer) => integer.to_string(),
        Value::Real(bits) => {
            let real = f64::from_bits(*bits);
            if real.is_finite() {
                format!("{real:?}")
            } else {
                format!("{{\"real_bits\":\"{bits:016x}\"}}")
            }
        }
        Value::Text(bytes) => std::str::from_utf8(bytes).map_or_else(
            |_| format!("{{\"text_hex\":\"{}\"}}", hex(bytes)),
            crate::closure::json_string,
        ),
        Value::Blob(bytes) => format!("{{\"blob_hex\":\"{}\"}}", hex(bytes)),
    }
}
