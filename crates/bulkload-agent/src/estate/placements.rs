//! `estate-placements [--undo] [--record FILE]... PRIVATE_STATE`.
//!
//! Every file a clean-source apply or a beside retirement placed into a
//! live checkout, and every file-level beside it landed, listed from the
//! `.files` records in STATE (by identity), and, with `--undo`, taken back:
//! each one still as it was placed (identity, size and time) is removed,
//! then each directory the pass made that is empty now; anything changed
//! since is kept and counted (review of #291, item 2).

// Receipt paths print escaped and quoted, as every estate receipt's do.
#![allow(clippy::unnecessary_debug_formatting)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use crate::git_carry::converge::files;
use crate::refuse::RefuseAt as _;
use crate::Result;

/// Run it (module docs); each line goes to `out`.
///
/// # Errors
/// Whatever reading STATE or a record, or an undo, refuses.
pub fn run(
    state: &Path,
    (records, undo): (&[PathBuf], bool),
    out: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut chosen: Vec<PathBuf> = records.to_vec();
    if chosen.is_empty() {
        for entry in std::fs::read_dir(state).refuse_at("estate::placements")? {
            let path = entry.refuse_at("estate::placements")?.path();
            if path.extension().is_some_and(|ext| ext == "files") {
                chosen.push(path);
            }
        }
    }
    chosen.sort();
    let (mut placed, mut besides) = (0_usize, 0_usize);
    for path in &chosen {
        let Some(record) = files::read_record(path)? else {
            continue;
        };
        out(&format!(
            "estate-placements record={path:?} item={} live={:?} done={} undone={} placed={} made-dirs={} held-derived={}",
            record.item,
            Path::new(OsStr::from_bytes(&record.live)),
            record.done,
            record.undone,
            record.placed.len(),
            record.made.len(),
            record.held.len(),
        ))?;
        for entry in &record.placed {
            out(&format!(
                "estate-placements placed={:?} beside={}",
                Path::new(OsStr::from_bytes(&entry.path)),
                entry.beside
            ))?;
            if entry.beside {
                besides += 1;
            } else {
                placed += 1;
            }
        }
        for made in &record.made {
            out(&format!(
                "estate-placements made-dir={:?}",
                Path::new(OsStr::from_bytes(&made.path))
            ))?;
        }
        if undo && !record.undone {
            let done = files::undo(path)?;
            out(&format!(
                "estate-placements undone record={path:?} removed={} kept={} dirs-removed={}",
                done.removed, done.kept, done.dirs
            ))?;
        }
    }
    out(&format!(
        "estate-placements summary records={} placed={placed} beside={besides} mode={}",
        chosen.len(),
        if undo { "undo" } else { "list" }
    ))
}
