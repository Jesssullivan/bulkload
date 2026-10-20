//! `--map-prefix SRC_REL=DST_REL` on `pull` and `copy` (#280 finding 9, the
//! operator's "no duplicates": a source path lands once, at its
//! destination's name).
//!
//! A receive-side rename, the wire unchanged: an entry at `SRC_REL`, or
//! under `SRC_REL/`, is decided, keyed (`row_key` over the mapped row),
//! recorded and published at `DST_REL` plus the rest of its path. Matching
//! is by whole path components and the longest source prefix wins. The
//! source still walks, reads and ledgers its own path; only the destination
//! names it otherwise.
//!
//! Refused `FIELD_DOMAIN_VIOLATION` before the session: a side that is not
//! a relative path of whole components (empty, `.`, `..`, a NUL, a leading
//! or trailing `/`), two maps with one source, two maps whose destinations
//! coincide, and a destination whose parent is not where its source's
//! parent lands (so the walk, which offers a directory before its entries,
//! has always made it).
//!
//! In a session the destinations belong to their maps: a destination path
//! is landed only through the map whose destination is the deepest one
//! holding it ([`PrefixMap::owner`]), and a path no destination holds only
//! unmapped. An entry that would reach a destination any other way (the
//! source holds the destination's name itself, or a shorter map's image
//! falls inside a longer map's destination) would merge two source paths
//! into one: it is refused `DESTINATION_OCCUPIED`, typed, with every entry
//! under it, whatever the walk order, and nothing is merged. Each map
//! replaces one prefix, so it never lands two of its own paths at one name;
//! with the owner rule no two seats of a session ever meet at one name.
//!
//! The session records each map it used as a `mapped` entry of its transfer
//! record (source path, destination path), accounted like `applied`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::{BulkloadRefusal, Result};

/// One `SRC_REL=DST_REL` pair, relative paths as raw bytes.
pub type Map = (Vec<u8>, Vec<u8>);

/// The maps of one session, longest source first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrefixMap {
    maps: Vec<Map>,
}

/// Whether `path` is `prefix` or lies under it, by whole components.
#[must_use]
pub fn within(path: &[u8], prefix: &[u8]) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.first() == Some(&b'/'))
}

/// Whether `side` is a relative path of whole components.
fn relative(side: &[u8]) -> bool {
    !side.is_empty()
        && !side.contains(&0)
        && side
            .split(|byte| *byte == b'/')
            .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

/// The parent of a relative path (`""` for one component).
fn parent(path: &[u8]) -> &[u8] {
    path.iter()
        .rposition(|byte| *byte == b'/')
        .and_then(|at| path.get(..at))
        .unwrap_or_default()
}

/// One `SRC_REL=DST_REL` argument, split at its first `=`.
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` without an `=`.
pub fn parse(value: &[u8]) -> Result<Map> {
    let at = value
        .iter()
        .position(|byte| *byte == b'=')
        .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    Ok((
        value.get(..at).unwrap_or_default().to_vec(),
        value.get(at + 1..).unwrap_or_default().to_vec(),
    ))
}

impl PrefixMap {
    /// The maps `maps`, checked (see the module).
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` for a set the module refuses.
    pub fn new(mut maps: Vec<Map>) -> Result<Self> {
        if maps
            .iter()
            .any(|(source, destination)| !relative(source) || !relative(destination))
        {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        for (at, (source, destination)) in maps.iter().enumerate() {
            for (other_source, other_destination) in maps.iter().skip(at + 1) {
                if source == other_source || destination == other_destination {
                    return Err(BulkloadRefusal::FieldDomainViolation);
                }
            }
        }
        maps.sort_by_key(|(source, _)| {
            std::cmp::Reverse(source.split(|byte| *byte == b'/').count())
        });
        let map = Self { maps };
        for (source, destination) in &map.maps {
            let landed = map
                .apply(parent(source))
                .map_or_else(|| parent(source).to_vec(), |(_, path)| path);
            if landed != parent(destination) {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
        }
        Ok(map)
    }

    /// Whether there is no map.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    /// The maps, longest source first.
    #[must_use]
    pub fn maps(&self) -> &[Map] {
        &self.maps
    }

    /// `path` under the longest map whose source holds it: that map's index
    /// and the destination path, or `None` when no map applies.
    #[must_use]
    pub fn apply(&self, path: &[u8]) -> Option<(usize, Vec<u8>)> {
        self.maps
            .iter()
            .enumerate()
            .find(|(_, (source, _))| within(path, source))
            .map(|(at, (source, destination))| {
                let mut mapped = destination.clone();
                mapped.extend_from_slice(path.get(source.len()..).unwrap_or_default());
                (at, mapped)
            })
    }

    /// `path` mapped, or as it is.
    #[must_use]
    pub fn path(&self, path: Vec<u8>) -> Vec<u8> {
        self.apply(&path).map_or(path, |(_, mapped)| mapped)
    }

    /// The map that owns the destination path `path`: the one whose
    /// destination is the deepest holding it (destinations are distinct, so
    /// it is one), or `None` when no destination holds it.
    #[must_use]
    pub fn owner(&self, path: &[u8]) -> Option<usize> {
        self.maps
            .iter()
            .enumerate()
            .filter(|(_, (_, destination))| within(path, destination))
            .max_by_key(|(_, (_, destination))| destination.len())
            .map(|(at, _)| at)
    }

    /// The destination path of the source path `path` and the map that
    /// lands it, or `Err(path)` when the owner rule refuses it: its
    /// destination's owner is not the map that would land it.
    ///
    /// # Errors
    /// The source path, for the caller's `DESTINATION_OCCUPIED`.
    pub fn land(&self, path: Vec<u8>) -> std::result::Result<(Option<usize>, Vec<u8>), Vec<u8>> {
        let (by, landed) = match self.apply(&path) {
            Some((at, landed)) => (Some(at), landed),
            None => (None, path.clone()),
        };
        if self.owner(&landed) == by {
            Ok((by, landed))
        } else {
            Err(path)
        }
    }
}

/// The process's maps (`pull --map-prefix`, `estate-move --sessions`).
static MAPS: Mutex<Vec<Map>> = Mutex::new(Vec::new());

/// The maps by canonical destination root, for tests that run sessions
/// with and without them in one process.
static OVERRIDE: Mutex<Vec<(PathBuf, Vec<Map>)>> = Mutex::new(Vec::new());

/// Run every session of this process with `maps` (empty: none).
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for a set [`PrefixMap::new`] refuses; nothing
/// is set then.
pub fn set_map_prefixes(maps: Vec<Map>) -> Result<()> {
    PrefixMap::new(maps.clone())?;
    *MAPS.lock().unwrap_or_else(PoisonError::into_inner) = maps;
    Ok(())
}

/// Run sessions into the canonical destination `root` with `maps`
/// (`None`: the process setting again). Test hook.
#[doc(hidden)]
pub fn set_map_prefix(root: &Path, maps: Option<Vec<Map>>) {
    let mut overrides = OVERRIDE.lock().unwrap_or_else(PoisonError::into_inner);
    overrides.retain(|(at, _)| at != root);
    if let Some(maps) = maps {
        overrides.push((root.to_path_buf(), maps));
    }
}

/// The maps of sessions into the canonical destination `root`.
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for an override [`PrefixMap::new`] refuses.
pub(super) fn for_root(root: &Path) -> Result<PrefixMap> {
    let maps = OVERRIDE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(at, _)| at == root)
        .map_or_else(
            || MAPS.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            |(_, maps)| maps.clone(),
        );
    PrefixMap::new(maps)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn pair(source: &str, destination: &str) -> Map {
        (source.as_bytes().to_vec(), destination.as_bytes().to_vec())
    }

    #[test]
    fn whole_components_match_and_the_longest_source_wins() {
        let map = PrefixMap::new(vec![
            pair("projects/-Users-jess-git-lab", "p2/-srv-git-lab"),
            pair("projects", "p2"),
            pair("projects/-Users-jess", "p2/-home-jess"),
        ])
        .unwrap();
        let mapped = |path: &str| map.apply(path.as_bytes()).map(|(_, path)| path);
        assert_eq!(
            mapped("projects/-Users-jess-git-lab/a.jsonl"),
            Some(b"p2/-srv-git-lab/a.jsonl".to_vec())
        );
        assert_eq!(
            mapped("projects/-Users-jess-git-lab"),
            Some(b"p2/-srv-git-lab".to_vec())
        );
        // A name that only starts with the source is no match of it.
        assert_eq!(
            mapped("projects/-Users-jess-git-lab2/a.jsonl"),
            Some(b"p2/-Users-jess-git-lab2/a.jsonl".to_vec())
        );
        assert_eq!(
            mapped("projects/-Users-jess/x"),
            Some(b"p2/-home-jess/x".to_vec())
        );
        assert_eq!(mapped("projectsx/a"), None);
        let landed = |path: &str| map.land(path.as_bytes().to_vec()).map(|(_, path)| path);
        assert_eq!(
            landed("projects/-Users-jess-git-lab/a.jsonl"),
            Ok(b"p2/-srv-git-lab/a.jsonl".to_vec())
        );
        assert_eq!(landed("projects"), Ok(b"p2".to_vec()));
        assert_eq!(landed("other/f"), Ok(b"other/f".to_vec()));
        // Through the shorter map into the longer one's destination: two
        // source paths at one name, refused whatever the walk order.
        assert_eq!(
            landed("projects/-srv-git-lab/a.jsonl"),
            Err(b"projects/-srv-git-lab/a.jsonl".to_vec())
        );
        // The source holds a destination's name itself.
        assert_eq!(landed("p2/x"), Err(b"p2/x".to_vec()));
    }

    #[test]
    fn a_set_that_could_land_two_names_at_one_or_lose_a_parent_is_refused() {
        let refused = |maps: Vec<Map>| {
            assert_eq!(
                PrefixMap::new(maps),
                Err(BulkloadRefusal::FieldDomainViolation)
            );
        };
        // Coinciding destinations.
        refused(vec![pair("p/a", "p/c"), pair("p/b", "p/c")]);
        // One source twice.
        refused(vec![pair("p/a", "p/c"), pair("p/a", "p/d")]);
        // Not whole components.
        for (source, destination) in [
            ("", "p/c"),
            ("p/a", ""),
            ("/p/a", "p/c"),
            ("p/a/", "p/c"),
            ("p//a", "p/c"),
            ("p/./a", "p/c"),
            ("p/a", "p/../c"),
            ("p/a\0", "p/c"),
        ] {
            refused(vec![pair(source, destination)]);
        }
        // A destination whose parent the walk does not make.
        refused(vec![pair("p/a", "q/a")]);
        refused(vec![pair("p", "q"), pair("p/a", "p/b")]);
        // Its parent where its source's parent lands: accepted.
        PrefixMap::new(vec![pair("p", "q"), pair("p/a", "q/b")]).unwrap();
        PrefixMap::new(vec![pair("a", "b")]).unwrap();
        assert_eq!(parse(b"a=b=c").unwrap(), pair("a", "b=c"));
        assert_eq!(parse(b"ab"), Err(BulkloadRefusal::FieldDomainViolation));
    }
}
