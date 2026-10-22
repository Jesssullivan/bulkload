//! The Claude project-key map `estate-move --sessions` pulls the source's
//! `~/.claude` with (#280 finding 9: "no duplicates").
//!
//! Claude Code lists a session under the key of `realpath(cwd)`
//! ([`key::project_key`]), so a source's `projects/<key(S)>` is a second
//! copy on the destination once the re-seat places it under the key of its
//! lane's destination. The map lands it there in the first place: for each
//! lane `S → D` (the run's `--map` pairs, the plan's items and
//! `SOURCE_HOME/git → DEST_HOME/git`, exactly the lanes [`Lanes`] gives the
//! re-seat), `projects/<key(S)>` becomes `projects/<key(D)>`, `D` resolved
//! to its real path on this host as [`Lanes::lane`] resolves it. The re-seat
//! then finds every mapped session in place and copies nothing.
//!
//! Keys are lossy (every byte outside `[A-Za-z0-9]` is `-`), so a key is
//! mapped only when exactly one lane source keys to it and no other source
//! key maps to its destination key. Otherwise it stays unmapped and is
//! reported typed ([`Held`]): the re-seat resolves its sessions by each
//! transcript's `cwd`, as before, and never merges two keys.
//!
//! [`proof`] is the report-only listing for the source-key directories an
//! earlier pass already landed (they are never removed: removing a
//! transcript is forbidden): per file, whether the destination key holds a
//! byte-identical copy, or one the file is a byte prefix of.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use super::{claude, key, Lanes, Options, SCHEMA};
use crate::closure::json_string;
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// One mapped key: `projects/<from>` lands at `projects/<to>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyMap {
    pub from: String,
    pub to: String,
    /// The lane source that keys to `from`.
    pub cwd: PathBuf,
}

/// Why a key stays unmapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// More than one lane source keys to it.
    AmbiguousSourceKey,
    /// Another source key maps to its destination key.
    DestKeyCollision,
    /// Its lane's destination did not resolve.
    LaneUnresolved,
}

impl Held {
    /// The reason's report word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::AmbiguousSourceKey => "ambiguous-source-key",
            Self::DestKeyCollision => "dest-key-collision",
            Self::LaneUnresolved => "lane-unresolved",
        }
    }
}

/// One unmapped key, with its reason, the lane sources that key to it and
/// the destination key when one was computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldKey {
    pub key: String,
    pub reason: Held,
    pub cwds: Vec<PathBuf>,
    pub to: Option<String>,
}

/// The derived map of one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyMaps {
    pub maps: Vec<KeyMap>,
    pub held: Vec<HeldKey>,
    /// Lane sources whose key is their destination's (one home path on both
    /// hosts): nothing to map.
    pub same: u64,
    /// After the pull: maps the landed sessions of their lane did not use
    /// ([`unused`]).
    pub unused: Vec<Unused>,
}

impl KeyMaps {
    /// The maps as `pull --map-prefix` pairs relative to `.claude`.
    #[must_use]
    pub fn prefixes(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.maps
            .iter()
            .map(|map| {
                (
                    format!("projects/{}", map.from).into_bytes(),
                    format!("projects/{}", map.to).into_bytes(),
                )
            })
            .collect()
    }

    /// The destination key `from` is mapped to.
    #[must_use]
    pub fn to(&self, from: &str) -> Option<&str> {
        self.maps
            .iter()
            .find(|map| map.from == from)
            .map(|map| map.to.as_str())
    }
}

/// The key map of `options`' lanes (see the module): every lane source
/// (`options.maps`' sources and `SOURCE_HOME/git`) keyed, each resolved
/// through [`Lanes`].
///
/// # Errors
/// None today; a lane that does not resolve is [`Held::LaneUnresolved`].
pub fn derive(options: &Options) -> Result<KeyMaps> {
    let lanes = Lanes::new(options);
    let mut sources: Vec<PathBuf> = options
        .maps
        .iter()
        .map(|(source, _)| source.clone())
        .collect();
    let source_git = options.source_home.join("git");
    sources.push(source_git.clone());
    // The per-repo lanes, derived (no flag per repo): a session's cwd is a
    // repository under the source's git root far more often than the root
    // itself, and Claude Code keys it by the repository's own path. Each
    // directory (or link to one) directly under the destination of that
    // root is a lane `SOURCE_HOME/git/NAME`, its destination resolved to
    // its real path here and keyed by it (an archive link keys by the
    // archive path). An explicit --map for the same source wins as before.
    if let Ok(Some(lane)) = lanes.lane(&source_git) {
        if let Ok(entries) = std::fs::read_dir(&lane.dest_cwd) {
            for entry in entries.flatten() {
                if std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()) {
                    sources.push(source_git.join(entry.file_name()));
                }
            }
        }
    }
    sources.sort();
    sources.dedup();
    // Source key → (lane source, its destination key or none).
    let mut by_key: BTreeMap<String, Vec<(PathBuf, Option<String>)>> = BTreeMap::new();
    for source in sources {
        let Some(text) = source.to_str() else {
            continue;
        };
        let to = match lanes.lane(&source) {
            Ok(Some(lane)) => Some(lane.key),
            Ok(None) => continue,
            Err(_) => None,
        };
        by_key
            .entry(key::project_key(text))
            .or_default()
            .push((source, to));
    }
    let known: BTreeSet<String> = by_key.keys().cloned().collect();
    let mut out = KeyMaps::default();
    let mut candidates = Vec::new();
    // Source keys that land as they are (a same-key lane, a held key): their
    // own name is taken at the destination, as a mapped key's target is.
    let mut native: Vec<String> = Vec::new();
    for (from, found) in by_key {
        let cwds: Vec<PathBuf> = found.iter().map(|(cwd, _)| cwd.clone()).collect();
        match found.as_slice() {
            [(_, Some(to))] if *to == from => {
                out.same += 1;
                native.push(from);
            }
            [(cwd, Some(to))] => candidates.push(KeyMap {
                from,
                to: to.clone(),
                cwd: cwd.clone(),
            }),
            [(_, None)] => {
                native.push(from.clone());
                out.held.push(HeldKey {
                    key: from,
                    reason: Held::LaneUnresolved,
                    cwds,
                    to: None,
                });
            }
            _ => {
                native.push(from.clone());
                out.held.push(HeldKey {
                    key: from,
                    reason: Held::AmbiguousSourceKey,
                    cwds,
                    to: None,
                });
            }
        }
    }
    let pruned = pruned_lane_maps(options, &lanes, &known, (&candidates, &native));
    candidates.extend(pruned);
    // A destination key two keys would land at (two maps, or a map and a
    // key that lands as it is) holds every map to it; a held key then lands
    // as it is too, so this runs until nothing more is held (#294 review 3:
    // same-key lanes were not counted, and the native key was refused on
    // every pass).
    loop {
        let mut targets: BTreeMap<&str, u64> = BTreeMap::new();
        for key in &native {
            *targets.entry(key.as_str()).or_default() += 1;
        }
        for map in &candidates {
            *targets.entry(map.to.as_str()).or_default() += 1;
        }
        let (kept, held): (Vec<KeyMap>, Vec<KeyMap>) = candidates
            .clone()
            .into_iter()
            .partition(|map| targets.get(map.to.as_str()).copied().unwrap_or(0) <= 1);
        if held.is_empty() {
            out.maps = kept;
            break;
        }
        for map in held {
            native.push(map.from.clone());
            out.held.push(HeldKey {
                key: map.from,
                reason: Held::DestKeyCollision,
                cwds: vec![map.cwd],
                to: Some(map.to),
            });
        }
        candidates = kept;
    }
    Ok(out)
}

/// The maps of lanes no source path is left for (2026-10-22: homes-20261021
/// held 30 sessions "under another key no map covers"; their worktrees were
/// pruned on both hosts, so no directory derives their lane).
///
/// A key is bytewise compositional (`key(S/rest)` is `key(S)` then
/// `key(/rest)`), so a destination project key `key(D)+rest`, under the
/// lane root `S -> D` whose destination key is its longest such prefix, is
/// exactly where a session of `S/rest` lands. Its source key `key(S)+rest`
/// is mapped there when no lane source keys to it already and no other map
/// or native key lands at that destination key: the derived lanes always
/// win, and nothing is ever merged across two keys.
fn pruned_lane_maps(
    options: &Options,
    lanes: &Lanes,
    known: &BTreeSet<String>,
    (candidates, native): (&[KeyMap], &[String]),
) -> Vec<KeyMap> {
    use std::os::unix::ffi::OsStrExt as _;
    // The lane roots: the run's --map sources and the git root, each as
    // (source root key, destination root key, source root).
    let roots = options
        .maps
        .iter()
        .map(|(source, _)| source.clone())
        .chain(std::iter::once(options.source_home.join("git")));
    let pairs: Vec<(String, String, PathBuf)> = roots
        .filter_map(|root| {
            let lane = lanes.lane(&root).ok()??;
            Some((key::project_key(root.to_str()?), lane.key, root))
        })
        .collect();
    let taken: BTreeSet<&str> = candidates
        .iter()
        .map(|map| map.to.as_str())
        .chain(native.iter().map(String::as_str))
        .collect();
    let mut out: Vec<KeyMap> = Vec::new();
    let Ok(entries) = std::fs::read_dir(options.dest_claude.join("projects")) else {
        return out;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(to) = std::str::from_utf8(entry.file_name().as_bytes()).map(str::to_owned) else {
            continue;
        };
        if taken.contains(to.as_str()) {
            continue;
        }
        let Some((from_root, dest_root, root)) = pairs
            .iter()
            .filter(|(_, dest_root, _)| {
                to.len() > dest_root.len()
                    && to.starts_with(dest_root.as_str())
                    && to.as_bytes().get(dest_root.len()) == Some(&b'-')
            })
            .max_by_key(|(_, dest_root, _)| dest_root.len())
        else {
            continue;
        };
        let from = format!("{from_root}{}", &to[dest_root.len()..]);
        if from == to || known.contains(&from) || out.iter().any(|map| map.from == from) {
            continue;
        }
        out.push(KeyMap {
            from,
            to,
            cwd: root.clone(),
        });
    }
    out.sort_by(|a, b| a.from.cmp(&b.from));
    out
}

/// A derived map no landed session used (#294 review 4).
///
/// The source keyed sessions of its lane otherwise (an older Claude Code's
/// key, which kept `.`; a differently cased path on a case-insensitive
/// source), and they landed under that other key, to be re-seated as
/// before: a second copy. Named so the duplicate is visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unused {
    /// The derived map's source key, and the key it lands at.
    pub from: String,
    pub to: String,
    /// The key the source's sessions of that lane landed under.
    pub seen: String,
    /// The `cwd` one of them records.
    pub cwd: String,
}

/// The derived maps (`maps`) that sessions of their lane did not land
/// through.
///
/// Each source-keyed directory in the pull's listing (`listed`,
/// paths relative to `.claude`) that no map moved, whose name is a map's
/// source key but for case, or whose first listed transcript's `cwd` lanes
/// to a map's destination key. Reads the first `cwd` of one transcript per
/// such directory under `options.dest_claude`. A source path reached
/// through a symlink keys by its target, whose lane is another one: not
/// found here.
///
/// # Errors
/// The read refusal of a transcript.
pub fn unused(options: &Options, maps: &KeyMaps, listed: &[Vec<u8>]) -> Result<Vec<Unused>> {
    let (Some(source), Some(destination)) = (
        options.source_home.to_str().map(key::project_key),
        options.dest_home.to_str().map(key::project_key),
    ) else {
        return Ok(Vec::new());
    };
    let lanes = Lanes::new(options);
    let mut listed_in: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in listed {
        let Some(rest) = std::str::from_utf8(path)
            .ok()
            .and_then(|path| path.strip_prefix("projects/"))
        else {
            continue;
        };
        let Some((dir, name)) = rest.split_once('/') else {
            continue;
        };
        if name.contains('/')
            || Path::new(name)
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            || !source_keyed(&source, &destination, dir)
            || maps.maps.iter().any(|map| map.to == dir)
        {
            continue;
        }
        listed_in.entry(dir).or_default().push(name);
    }
    let mut out = Vec::new();
    for (dir, names) in listed_in {
        // The first of the key's listed transcripts still at its landed
        // path: one the pull refused, or a fold moved on, is not there
        // (`ENOENT`), or is a link (`ELOOP`); its own pull or fold row
        // accounts for it, and a key with none left has nothing to note.
        let mut found = None;
        for name in names {
            let path = options.dest_claude.join("projects").join(dir).join(name);
            match claude::first_cwd(&path) {
                Ok(cwd) => {
                    found = Some(cwd.unwrap_or_default());
                    break;
                }
                Err(BulkloadRefusal::Io(Some(libc::ENOENT | libc::ELOOP | libc::ENOTDIR))) => {}
                Err(refusal) => return Err(refusal),
            }
        }
        let Some(cwd) = found else {
            continue;
        };
        let key = match lanes.lane(Path::new(&cwd)) {
            Ok(Some(lane)) if !cwd.is_empty() => Some(lane.key),
            _ => None,
        };
        if let Some(map) = maps.maps.iter().find(|map| {
            map.from != dir
                && (map.from.eq_ignore_ascii_case(dir) || key.as_deref() == Some(map.to.as_str()))
        }) {
            out.push(Unused {
                from: map.from.clone(),
                to: map.to.clone(),
                seen: dir.to_owned(),
                cwd,
            });
        }
    }
    Ok(out)
}

/// Each transcript's lane key directly under the key directory `root`
/// (`files` relative to it), by session ID: `None` when it records no
/// `cwd` or that `cwd` has no lane.
fn session_lanes(
    lanes: &Lanes,
    root: &Path,
    files: &[PathBuf],
) -> Result<BTreeMap<String, Option<String>>> {
    let mut sessions = BTreeMap::new();
    for rel in files {
        let Some(sid) = rel
            .to_str()
            .and_then(|name| name.strip_suffix(".jsonl"))
            .filter(|sid| !sid.contains('/'))
        else {
            continue;
        };
        let path = root.join(rel);
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            continue;
        }
        let lane = claude::first_cwd(&path)?.and_then(|cwd| {
            lanes
                .lane(Path::new(&cwd))
                .ok()
                .flatten()
                .map(|lane| lane.key)
        });
        sessions.insert(sid.to_owned(), lane);
    }
    Ok(sessions)
}

/// What [`proof`] found, by status word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProofSummary {
    pub dirs: u64,
    pub files: BTreeMap<&'static str, u64>,
}

impl ProofSummary {
    /// The count of files with `status`.
    #[must_use]
    pub fn of(&self, status: &str) -> u64 {
        self.files.get(status).copied().unwrap_or(0)
    }
}

/// Whether `dir` is a key of a path under the source home (its key, or that
/// key and a `-`), when the two homes key differently.
fn source_keyed(source: &str, destination: &str, dir: &str) -> bool {
    source != destination
        && (dir == source
            || dir
                .strip_prefix(source)
                .is_some_and(|rest| rest.starts_with('-')))
}

/// BLAKE3 of the first `length` bytes of `path` (`None` when it holds
/// fewer), never following a link.
fn prefix_digest(path: &Path, length: u64) -> Result<Option<[u8; 32]>> {
    let file = crate::hash::open_nofollow(path)?;
    let mut reader = file.take(length);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut read_total = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .refuse_at("reseat::keymap::digest")?;
        if read == 0 {
            break;
        }
        read_total += read as u64;
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
    Ok((read_total == length).then(|| *hasher.finalize().as_bytes()))
}

/// How the destination key holds `file` at `at` (and beside it, at
/// `<at><suffix>`).
fn status(file: &Path, size: u64, at: &Path, suffix: Option<&str>) -> Result<&'static str> {
    let digest = prefix_digest(file, size)?;
    let holds = |path: &Path| -> Result<Option<&'static str>> {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return Ok(None);
        };
        if !meta.is_file() {
            return Ok(Some("dest-other"));
        }
        if meta.len() < size {
            return Ok(Some("differs"));
        }
        let held = prefix_digest(path, size)?;
        Ok(Some(match (held == digest, meta.len() == size) {
            (true, true) => "identical",
            (true, false) => "prefix-extended",
            (false, _) => "differs",
        }))
    };
    let main = holds(at)?;
    if matches!(main, Some("identical" | "prefix-extended")) {
        return Ok(main.unwrap_or("absent"));
    }
    if let Some(suffix) = suffix {
        let mut beside = at.as_os_str().to_owned();
        beside.push(suffix);
        match holds(Path::new(&beside))? {
            Some("identical") => return Ok("identical-beside"),
            Some("prefix-extended") => return Ok("prefix-extended-beside"),
            _ => {}
        }
    }
    Ok(match main {
        None => "absent",
        Some("dest-other") => {
            if std::fs::symlink_metadata(at).is_ok_and(|meta| meta.file_type().is_symlink()) {
                "dest-link"
            } else {
                "dest-other"
            }
        }
        Some(word) => word,
    })
}

/// The report-only proof (see the module), one JSON line per file through
/// `emit`; nothing is written.
///
/// Every file under each source-keyed directory of the destination's
/// `projects/` (or one `maps` maps), with the destination key it belongs
/// under (the map's; else its session's lane by the transcript's `cwd`;
/// else the one lane key all the directory's transcripts agree on) and how
/// that key holds it: `identical`, `prefix-extended` (it holds these bytes
/// and more), the same `-beside` at `<path><suffix>`, `differs`, `absent`,
/// `dest-link`, `dest-other`, `in-place`, or `unresolved` (no destination
/// key).
///
/// # Errors
/// I/O refusals of the listing; a transcript whose `cwd` cannot be read.
#[allow(clippy::too_many_lines)] // Walk, judge each file, its fold, its line.
pub fn proof(
    options: &Options,
    maps: &KeyMaps,
    emit: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<ProofSummary> {
    let lanes = Lanes::new(options);
    let claude = std::fs::canonicalize(&options.dest_claude).refuse_at("reseat::keymap::proof")?;
    let projects = claude.join("projects");
    let (source, destination) = (
        options.source_home.to_str().map(key::project_key),
        options.dest_home.to_str().map(key::project_key),
    );
    let (Some(source), Some(destination)) = (source, destination) else {
        return Err(BulkloadRefusal::PathNotPortable);
    };
    let suffix = options.beside_suffix.as_deref();
    let mut summary = ProofSummary::default();
    let mut dirs: Vec<String> = Vec::new();
    match std::fs::read_dir(&projects) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.refuse_at("reseat::keymap::proof")?;
                if let Some(name) = entry.file_name().to_str() {
                    dirs.push(name.to_owned());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).refuse_at("reseat::keymap::proof"),
    }
    dirs.sort();
    for dir in dirs {
        let mapped = maps.to(&dir);
        if mapped.is_none() && !source_keyed(&source, &destination, &dir) {
            continue;
        }
        let root = projects.join(&dir);
        if !std::fs::symlink_metadata(&root).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        summary.dirs += 1;
        let files = claude::walk(&root)?;
        let sessions = session_lanes(&lanes, &root, &files)?;
        let mut agreed: Vec<&String> = sessions.values().flatten().collect();
        agreed.sort();
        agreed.dedup();
        let consensus = match agreed.as_slice() {
            [one] if sessions.values().all(Option::is_some) => Some((*one).clone()),
            _ => None,
        };
        for rel in files {
            let path = root.join(&rel);
            let Ok(node) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            // A link to a regular file (into the archive) is judged by its
            // target's bytes, as the fold merges through it; only its name
            // is ever removed.
            let link = node.file_type().is_symlink();
            let meta = if link {
                std::fs::metadata(&path).unwrap_or(node)
            } else {
                node
            };
            let first = rel
                .components()
                .next()
                .and_then(|part| part.as_os_str().to_str())
                .unwrap_or_default()
                .to_owned();
            let session = first.strip_suffix(".jsonl").unwrap_or(&first);
            let (to, basis) = match (mapped, sessions.get(session)) {
                (Some(to), _) => (Some(to.to_owned()), "key-map"),
                (None, Some(Some(lane))) => (Some(lane.clone()), "cwd"),
                (None, _) => consensus
                    .as_ref()
                    .map_or((None, "none"), |lane| (Some(lane.clone()), "cwd-consensus")),
            };
            let word = match &to {
                _ if !meta.is_file() => "source-not-regular",
                None => "unresolved",
                Some(to) if *to == dir => "in-place",
                Some(to) => {
                    let bytes = if link {
                        std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone())
                    } else {
                        path.clone()
                    };
                    status(&bytes, meta.len(), &projects.join(to).join(&rel), suffix)?
                }
            };
            *summary.files.entry(word).or_default() += 1;
            // What the next mapped pull does with it (`--consolidate`): every
            // file under a mapped source key is folded into the destination
            // key: moved when absent there; an append log unioned; another
            // file removed when the same bytes are there, else held.
            let fold = match &to {
                Some(to) if basis == "key-map" && *to != dir && meta.is_file() => {
                    let there = projects.join(to).join(&rel);
                    let is_log = rel.extension().is_some_and(|ext| ext == "jsonl");
                    match std::fs::symlink_metadata(&there) {
                        Err(_) => "move",
                        Ok(_) if is_log => "union",
                        Ok(_) if word == "identical" => "remove",
                        Ok(_) => "hold",
                    }
                }
                _ => "none",
            };
            emit(&format!(
                "{{\"schema\":\"{SCHEMA}\",\"kind\":\"key-map-proof\",\"tool\":\"claude\",\"source_key\":{},\"dest_key\":{},\"basis\":\"{basis}\",\"path\":{},\"size\":{},\"status\":\"{word}\",\"fold\":\"{fold}\",\"link\":{link}}}",
                json_string(&dir),
                to.as_deref().map_or_else(|| "null".to_owned(), json_string),
                json_string(&rel.to_string_lossy()),
                meta.len(),
            ))?;
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    fn options(base: &Path, maps: &[(&str, &str)]) -> Options {
        let mut options = Options::new(
            &base.join("Users/jess"),
            &base.join("home/jess"),
            &base.join("home/jess"),
            &base.join("state"),
        );
        options.maps = maps
            .iter()
            .map(|(source, dest)| (base.join(source), base.join(dest)))
            .collect();
        options
    }

    /// The per-repo lanes are derived, no flag per repository: each
    /// directory under the destination's git root (a link into an archive
    /// root included) maps its source key to the key of its real path.
    #[test]
    fn per_repo_lanes_are_derived_and_keyed_by_their_real_path() {
        let base = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base.path()).unwrap();
        std::fs::create_dir_all(base.join("srv/git/lab")).unwrap();
        std::fs::create_dir_all(base.join("archive/git/old-site")).unwrap();
        std::fs::create_dir_all(base.join("home/jess")).unwrap();
        std::os::unix::fs::symlink(base.join("srv/git"), base.join("home/jess/git")).unwrap();
        // A repository whose destination is a link into an archive root.
        std::os::unix::fs::symlink(
            base.join("archive/git/old-site"),
            base.join("srv/git/old-site"),
        )
        .unwrap();
        std::fs::write(base.join("srv/git/notes.txt"), b"not a repo\n").unwrap();
        let derived = derive(&options(&base, &[("Users/jess/git", "home/jess/git")])).unwrap();
        let key_of = |path: &Path| key::project_key(path.to_str().unwrap());
        let maps: Vec<(String, String)> = derived
            .maps
            .iter()
            .map(|map| (map.from.clone(), map.to.clone()))
            .collect();
        for (source, real) in [
            ("Users/jess/git/lab", base.join("srv/git/lab")),
            ("Users/jess/git/old-site", base.join("archive/git/old-site")),
        ] {
            let pair = (key_of(&base.join(source)), key_of(&real));
            assert!(maps.contains(&pair), "{pair:?} in {maps:?}");
        }
        assert!(
            !maps.iter().any(|(from, _)| from.ends_with("notes-txt")),
            "a file is no lane: {maps:?}"
        );
        assert!(derived.held.is_empty(), "{:?}", derived.held);
    }

    /// homes-20261021 (2026-10-22): a worktree pruned on both hosts derives
    /// no lane, so its sessions were held "under another key no map
    /// covers". The destination's key under a lane root maps the source
    /// key the same path keys to; a key under no lane root (the 10-15 side
    /// root) and a derived lane's own key gain nothing.
    #[test]
    fn a_pruned_lanes_key_maps_to_the_destination_key_under_its_root() {
        let base = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base.path()).unwrap();
        std::fs::create_dir_all(base.join("srv/git/lab")).unwrap();
        std::fs::create_dir_all(base.join("home/jess")).unwrap();
        std::os::unix::fs::symlink(base.join("srv/git"), base.join("home/jess/git")).unwrap();
        let options = options(&base, &[("Users/jess/git", "home/jess/git")]);
        let key = |path: &str| key::project_key(base.join(path).to_str().unwrap());
        let projects = options.dest_claude.join("projects");
        for dest in [
            "srv/git/gftb.org.worktrees/lm1-recovery",
            "srv/estate/gftb/git/gftb.org.worktrees/lm1-recovery",
            "srv/git/lab",
        ] {
            std::fs::create_dir_all(projects.join(key(dest))).unwrap();
        }
        let derived = derive(&options).unwrap();
        let maps: Vec<(String, String)> = derived
            .maps
            .iter()
            .map(|map| (map.from.clone(), map.to.clone()))
            .collect();
        assert!(
            maps.contains(&(
                key("Users/jess/git/gftb.org.worktrees/lm1-recovery"),
                key("srv/git/gftb.org.worktrees/lm1-recovery")
            )),
            "{maps:?}"
        );
        assert!(
            !maps.iter().any(|(_, to)| to.contains("estate")),
            "a side root is under no lane root: {maps:?}"
        );
        assert_eq!(
            maps.iter()
                .filter(|(_, to)| *to == key("srv/git/lab"))
                .count(),
            1,
            "the derived lane alone: {maps:?}"
        );
        assert!(derived.held.is_empty(), "{:?}", derived.held);
        crate::transfer::PrefixMap::new(derived.prefixes()).unwrap();
    }

    /// A lossy key two lane sources share, and two keys landing at one
    /// destination key, stay unmapped and typed; the rest map to the key
    /// of their lane's real destination path.
    #[test]
    fn a_shared_key_or_destination_stays_unmapped_and_typed() {
        let base = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base.path()).unwrap();
        std::fs::create_dir_all(base.join("home/jess/git")).unwrap();
        std::fs::create_dir_all(base.join("srv/git")).unwrap();
        // The destination's git is a link to the real tree, as on sting.
        std::fs::remove_dir(base.join("home/jess/git")).unwrap();
        std::os::unix::fs::symlink(base.join("srv/git"), base.join("home/jess/git")).unwrap();
        let options = options(
            &base,
            &[
                ("Users/jess/git/lab", "home/jess/git/lab"),
                ("Users/jess/git/a.b", "home/jess/git/a.b"),
                ("Users/jess/git/a-b", "home/jess/git/a-b"),
                ("Users/jess/git/old", "srv/git/lab"),
            ],
        );
        let derived = derive(&options).unwrap();
        let key = |path: &str| key::project_key(base.join(path).to_str().unwrap());
        // Lab and old both land at the real lab's key: a collision.
        let held: Vec<(String, &str)> = derived
            .held
            .iter()
            .map(|held| (held.key.clone(), held.reason.word()))
            .collect();
        assert!(held.contains(&(key("Users/jess/git/a.b"), "ambiguous-source-key")));
        assert!(held.contains(&(key("Users/jess/git/lab"), "dest-key-collision")));
        assert!(held.contains(&(key("Users/jess/git/old"), "dest-key-collision")));
        assert_eq!(held.len(), 3, "{held:?}");
        // The default lane, its destination resolved through the link.
        assert_eq!(
            derived.to(&key("Users/jess/git")),
            Some(key("srv/git").as_str())
        );
        assert_eq!(derived.maps.len(), 1, "{:?}", derived.maps);
        crate::transfer::PrefixMap::new(derived.prefixes()).unwrap();
    }

    /// #294 review 3: a key that lands as it is (a lane whose key is its
    /// destination's) takes its name at the destination, so another source
    /// key mapping there is held, typed, never refused on every pass.
    #[test]
    fn a_map_onto_a_key_that_lands_as_it_is_is_a_collision() {
        let base = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base.path()).unwrap();
        std::fs::create_dir_all(base.join("home/jess/git")).unwrap();
        let options = options(
            &base,
            &[
                ("Users/jess/git/same", "Users/jess/git/same"),
                ("Users/jess/git/other", "Users/jess/git/same"),
            ],
        );
        let derived = derive(&options).unwrap();
        let key = |path: &str| key::project_key(base.join(path).to_str().unwrap());
        assert_eq!(derived.same, 1, "{derived:?}");
        assert!(
            derived
                .held
                .iter()
                .any(|held| held.key == key("Users/jess/git/other")
                    && held.reason == Held::DestKeyCollision),
            "{derived:?}"
        );
        assert!(derived.to(&key("Users/jess/git/other")).is_none());
    }

    /// #294 review 4: sessions of a mapped lane the source keyed otherwise
    /// (an older Claude Code kept `.`; a differently cased path) did not
    /// use the map: named.
    #[test]
    fn a_map_its_sessions_did_not_use_is_named() {
        let base = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base.path()).unwrap();
        std::fs::create_dir_all(base.join("home/jess/git/lab")).unwrap();
        let options = options(&base, &[("Users/jess/git/lab", "home/jess/git/lab")]);
        let derived = derive(&options).unwrap();
        let key = |path: &str| key::project_key(base.join(path).to_str().unwrap());
        let lab = key("Users/jess/git/lab");
        let cased = key("Users/jess/git/Lab");
        // An old-style key for a worktree under the lane (`.` kept).
        let dotted = format!("{lab}-.claude-worktrees-x");
        let projects = base.join("home/jess/.claude/projects");
        let cwd = base.join("Users/jess/git/lab");
        for dir in [&cased, &dotted] {
            std::fs::create_dir_all(projects.join(dir)).unwrap();
            std::fs::write(
                projects.join(dir).join("s.jsonl"),
                format!("{{\"cwd\":\"{}\"}}\n", cwd.display()),
            )
            .unwrap();
        }
        // A key whose only listed transcript is not at its landed path (the
        // pull refused it, or a fold moved it on): nothing to read, no note.
        let gone = key("Users/jess/git/LAB");
        std::fs::create_dir_all(projects.join(&gone)).unwrap();
        // 2026-10-22: a listed transcript absent at its landing (`ENOENT`)
        // or a link there (`ELOOP`) is passed over for the key's next one,
        // never a bare `IO` refusing the step.
        std::os::unix::fs::symlink(
            projects.join(&cased).join("s.jsonl"),
            projects.join(&cased).join("a-link.jsonl"),
        )
        .unwrap();
        let mut listed: Vec<Vec<u8>> = vec![
            format!("projects/{cased}/a-absent.jsonl").into_bytes(),
            format!("projects/{cased}/a-link.jsonl").into_bytes(),
            format!("projects/{gone}/s.jsonl").into_bytes(),
        ];
        listed.extend(
            [&cased, &dotted]
                .iter()
                .map(|dir| format!("projects/{dir}/s.jsonl").into_bytes()),
        );
        let unused = unused(&options, &derived, &listed).unwrap();
        assert!(unused.iter().all(|note| note.seen != gone), "{unused:?}");
        let seen: Vec<&str> = unused.iter().map(|note| note.seen.as_str()).collect();
        assert!(seen.contains(&cased.as_str()), "{unused:?}");
        assert!(seen.contains(&dotted.as_str()), "{unused:?}");
        assert!(unused.iter().all(|note| note.from == lab));
        // A mapped landing is no note.
        let mapped = vec![format!("projects/{}/s.jsonl", key("home/jess/git/lab")).into_bytes()];
        assert!(super::unused(&options, &derived, &mapped)
            .unwrap()
            .is_empty());
    }

    /// The report-only proof over source-key directories an earlier pass
    /// left: per file, how the destination key holds it; nothing written.
    #[test]
    fn the_proof_names_how_the_destination_key_holds_each_file() {
        let base = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base.path()).unwrap();
        let mut options = options(&base, &[]);
        options.beside_suffix = Some(".neo".to_owned());
        std::fs::create_dir_all(base.join("home/jess/git/lab")).unwrap();
        let projects = base.join("home/jess/.claude/projects");
        let key = |path: &str| key::project_key(base.join(path).to_str().unwrap());
        let (neo, sting) = (key("Users/jess/git/lab"), key("home/jess/git/lab"));
        let write = |dir: &str, rel: &str, bytes: &[u8]| {
            let path = projects.join(dir).join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        write(&neo, "a.jsonl", b"a\n");
        write(&sting, "a.jsonl", b"a\n");
        write(&neo, "a/subagents/s.jsonl", b"s\n");
        write(&sting, "a/subagents/s.jsonl", b"s\n");
        write(&neo, "b.jsonl", b"b\n");
        write(&sting, "b.jsonl", b"b\nmore\n");
        write(&neo, "c.jsonl", b"c\n");
        write(&neo, "d.jsonl", b"d\n");
        write(&sting, "d.jsonl", b"D\n");
        write(&neo, "e.jsonl", b"e\n");
        write(&sting, "e.jsonl.neo", b"e\n");
        // A source key no lane maps: resolved by its transcript's cwd.
        let elsewhere = key("Users/jess/elsewhere");
        let cwd = base.join("Users/jess/elsewhere");
        write(
            &elsewhere,
            "f.jsonl",
            format!("{{\"cwd\":\"{}\"}}\n", cwd.display()).as_bytes(),
        );
        // The destination's own key: not the proof's.
        write(&sting, "own.jsonl", b"own\n");
        let maps = KeyMaps {
            maps: vec![KeyMap {
                from: neo.clone(),
                to: sting.clone(),
                cwd: base.join("Users/jess/git/lab"),
            }],
            ..KeyMaps::default()
        };
        let mut lines = Vec::new();
        let summary = proof(&options, &maps, &mut |line| {
            lines.push(line.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(summary.dirs, 2, "{lines:?}");
        for (word, count) in [
            ("identical", 2),
            ("prefix-extended", 1),
            ("absent", 1),
            ("differs", 1),
            ("identical-beside", 1),
            ("unresolved", 1),
        ] {
            assert_eq!(summary.of(word), count, "{word}: {lines:?}");
        }
        assert_eq!(summary.files.values().sum::<u64>(), 7, "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("\"path\":\"b.jsonl\"")
                    && line.contains("\"status\":\"prefix-extended\"")
                    && line.contains("\"basis\":\"key-map\"")),
            "{lines:?}"
        );
        // Nothing written: the source-key directory is as it was.
        assert_eq!(
            std::fs::read(projects.join(&neo).join("c.jsonl")).unwrap(),
            b"c\n"
        );
        assert!(!projects.join(&sting).join("c.jsonl").exists());
    }
}
