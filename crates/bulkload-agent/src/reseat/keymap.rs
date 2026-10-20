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

use std::collections::BTreeMap;
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
    sources.push(options.source_home.join("git"));
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
    let mut out = KeyMaps::default();
    let mut candidates = Vec::new();
    for (from, found) in by_key {
        let cwds: Vec<PathBuf> = found.iter().map(|(cwd, _)| cwd.clone()).collect();
        match found.as_slice() {
            [(_, Some(to))] if *to == from => out.same += 1,
            [(cwd, Some(to))] => candidates.push(KeyMap {
                from,
                to: to.clone(),
                cwd: cwd.clone(),
            }),
            [(_, None)] => out.held.push(HeldKey {
                key: from,
                reason: Held::LaneUnresolved,
                cwds,
                to: None,
            }),
            _ => out.held.push(HeldKey {
                key: from,
                reason: Held::AmbiguousSourceKey,
                cwds,
                to: None,
            }),
        }
    }
    let mut targets: BTreeMap<String, u64> = BTreeMap::new();
    for map in &candidates {
        *targets.entry(map.to.clone()).or_default() += 1;
    }
    for map in candidates {
        if targets.get(&map.to).copied().unwrap_or(0) > 1 {
            out.held.push(HeldKey {
                key: map.from,
                reason: Held::DestKeyCollision,
                cwds: vec![map.cwd],
                to: Some(map.to),
            });
        } else {
            out.maps.push(map);
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
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
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
                Some(to) => status(&path, meta.len(), &projects.join(to).join(&rel), suffix)?,
            };
            *summary.files.entry(word).or_default() += 1;
            emit(&format!(
                "{{\"schema\":\"{SCHEMA}\",\"kind\":\"key-map-proof\",\"tool\":\"claude\",\"source_key\":{},\"dest_key\":{},\"basis\":\"{basis}\",\"path\":{},\"size\":{},\"status\":\"{word}\"}}",
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
