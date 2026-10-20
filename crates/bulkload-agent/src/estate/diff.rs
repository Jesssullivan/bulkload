//! `estate-diff` (#226 item 1; OI-1003-Q171/Q172; design
//! `docs/agent-notes/2026-10-15-estate-adopt-design.md`, section 3): a
//! plan's items against the destination that is there now, one JSON line
//! per row, schema [`SCHEMA`].
//!
//! Rows: one per plan item (its class, what `estate-apply` and `--adopt`
//! do there, the components that differ, per-ref rows, destination work
//! since `--since`, the capture's counted rebuildable omissions and what a
//! first landing does with its origin); one per worktree a planned
//! repository registers that no item plans (`dest-only`); with `--scan
//! DIR`, one per top-level Git directory under DIR in no planned family
//! (`dest-only`); and a summary line.
//!
//! Read-only at the destination and in every STATE directory: the classes
//! come from the same functions the apply runs
//! (`git_carry::converge::diff`), against a scratch repository in each
//! staged bundle's private directory, removed with it. The source is never
//! read.
//!
//! One import per repository (M2r2 item 3): a plan base (a repository's
//! shared capture, which every linked worktree item of it requires) is
//! staged once per run and imported once per destination object store;
//! every item's scratch borrows that import. Before, each item staged and
//! indexed the whole base again, O(items x repository). A base's stage is
//! dropped after the last item that names it. `BULKLOAD_DIFF_SHARED_IMPORT=0`
//! (exactly `0`; any other value shares) is the A/B arm: each item stages
//! and imports its base itself, as before. `estate-reseat --check` reads
//! the Git state through this function and shares the same way.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{
    bound, chain_links, filename, id, journal_path, prior_sidecar, read, retained_omissions,
    retained_parts, sealed, stage_base, Base, Capture, LinkBinding, Plan,
};
use crate::closure::json_string;
use crate::git_carry::{self, converge};
use crate::{BulkloadRefusal, Result};

/// The schema of every `estate-diff` line.
pub const SCHEMA: &str = "bulkload.estate-diff.v1";

/// What one `estate-diff` run reads beyond the plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffOptions {
    /// `--since NS`: destination work after this instant (nanoseconds since
    /// the epoch) is printed as evidence.
    pub since: Option<i128>,
    /// `--scan DIR` (repeatable): top-level Git directories under DIR in no
    /// planned family are listed `dest-only`.
    pub scan: Vec<PathBuf>,
    /// `--activate-origin PREFIX`: what a first landing would activate
    /// under the same prefixes `estate-apply` is given.
    pub activate_origin: Vec<String>,
    /// `--beside SUFFIX`: what `estate-apply --adopt --beside SUFFIX` does
    /// with a diverged item (M2 round 2 item c).
    pub beside: Option<String>,
    /// `--advance-ancestor` (operator ruling 2026-10-19): a diverged live
    /// checkout the apply would advance reads `adopt:"advance"`; one whose
    /// advance would refuse keeps `diverged` with the component
    /// `advance-refused=<CODE>` (read-only, the apply's own proof).
    pub advance_ancestor: bool,
    /// `--refs-in` (with `--beside`; operator ruling 2026-10-16 21:24Z): a
    /// diverged item whose capture has no tracked change and whose HEAD its
    /// refs bring in reads `adopt:"refs-in"` with `beside=none`, as the
    /// apply lands no tree beside for it.
    pub refs_in: bool,
    /// `--side-root ROOT` (M2r2 item e): what `estate-apply --side-root`
    /// would seat is reported (`kind:"seat"` rows), and each item spelled
    /// through such a link is answered at the directory it names, which is
    /// the tree the live path holds once seated (component
    /// `side-root-seat`).
    pub side_roots: Vec<PathBuf>,
}

/// The counts of one run, by class and by adopt answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub classes: BTreeMap<String, u64>,
    pub adopt: BTreeMap<String, u64>,
    /// Each planned workspace's class, keyed by its canonical path
    /// (`refused` for an item whose read refused): what `estate-reseat
    /// --check` reads as a lane's Git state (R0, TIN-6587).
    pub workspaces: BTreeMap<PathBuf, String>,
}

/// Report PLAN against the destination it names: one line per row,
/// handed to `line`, then the summary line.
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for an `--activate-origin` prefix that is not
/// an scp-style or `ssh://` prefix; a malformed plan. Each item's own
/// failure (a capture absent, a sealed object missing, any refusal of the
/// read) is its row's `refused` code, never the run's.
#[allow(clippy::too_many_lines)]
pub fn diff(
    plan: &Path,
    corpus: &Path,
    source: &str,
    states: &[PathBuf],
    options: &DiffOptions,
    line: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<Summary> {
    if !options
        .activate_origin
        .iter()
        .all(|prefix| git_carry::origin_prefix_allowed(prefix))
    {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let contents: Plan = read(plan)?;
    let mut summary = Summary::default();
    let mut planned: BTreeSet<PathBuf> = BTreeSet::new();
    let mut repositories: BTreeSet<PathBuf> = BTreeSet::new();
    for item in &contents.items {
        planned.insert(canonical(&item.repository));
        if let Some(workspace) = &item.workspace {
            planned.insert(canonical(workspace));
        }
        if item.repository.exists() {
            repositories.insert(canonical(&item.repository));
        }
    }
    let items = contents
        .items
        .iter()
        .map(id)
        .collect::<Result<Vec<String>>>()?;
    let mut bases = Bases::new(&contents.items, corpus)?;
    let roots = super::seat::roots(&options.side_roots)?;
    let mut seats: Vec<(PathBuf, PathBuf)> = Vec::new();
    for candidate in super::seat::candidates(&contents.items, &roots) {
        let (seat, target, refused) = match &candidate.found {
            super::seat::Found::Seatable { target, .. } => {
                seats.push((candidate.live.clone(), target.clone()));
                ("seatable", Some(target), None)
            }
            super::seat::Found::Refused(refusal) => ("refused", None, Some(refusal.code())),
        };
        line(&format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"seat\",\"path\":{},\"target\":{},\"seat\":\"{seat}\",\"refused\":{}}}",
            json_string(&converge::diff::shown(&candidate.live)),
            target.map_or_else(|| "null".to_owned(), |target| json_string(&converge::diff::shown(target))),
            refused.map_or_else(|| "null".to_owned(), json_string),
        ))?;
    }
    let mut seen = BTreeSet::new();
    for (index, item) in contents.items.iter().enumerate() {
        let identity = id(item)?;
        if !seen.insert(identity.clone()) {
            continue;
        }
        let mut text = format!(
            "{{\"schema\":\"{SCHEMA}\",\"kind\":\"item\",\"item\":{},\"source\":{},\"repository\":{},\"workspace\":{}",
            json_string(&identity),
            json_string(&converge::diff::shown(&item.source)),
            json_string(&converge::diff::shown(&item.repository)),
            item.workspace
                .as_ref()
                .map_or_else(|| "null".to_owned(), |path| json_string(&converge::diff::shown(path))),
        );
        let (seated, answered_item) = seated_item(item, &seats);
        let answered = item_row(
            &answered_item,
            (&identity, &items),
            (corpus, &mut bases),
            source,
            states,
            options,
        );
        bases.done(index);
        match answered {
            Ok((mut row, journaled, omitted)) => {
                if seated {
                    row.components.push("side-root-seat".to_owned());
                }
                if let Some(workspace) = &item.workspace {
                    summary
                        .workspaces
                        .insert(canonical(workspace), row.class.to_owned());
                }
                *summary.classes.entry(row.class.to_owned()).or_default() += 1;
                *summary.adopt.entry(row.adopt.to_owned()).or_default() += 1;
                write_row(&mut text, &row, journaled, omitted.as_deref());
            }
            Err(refusal) => {
                if let Some(workspace) = &item.workspace {
                    summary
                        .workspaces
                        .insert(canonical(workspace), "refused".to_owned());
                }
                *summary.classes.entry("refused".to_owned()).or_default() += 1;
                text.push_str(",\"class\":\"refused\",\"refused\":");
                text.push_str(&json_string(refusal.code()));
            }
        }
        text.push('}');
        line(&text)?;
    }
    // Worktrees a planned repository registers that no item plans.
    let mut family = planned.clone();
    for repository in &repositories {
        let Ok(worktrees) = converge::diff::registered_worktrees(repository) else {
            continue;
        };
        for (path, branch) in worktrees {
            let path = canonical(&path);
            family.insert(path.clone());
            if planned.contains(&path) {
                continue;
            }
            *summary.classes.entry("dest-only".to_owned()).or_default() += 1;
            line(&format!(
                "{{\"schema\":\"{SCHEMA}\",\"kind\":\"worktree\",\"path\":{},\"repository\":{},\"branch\":{},\"class\":\"dest-only\"}}",
                json_string(&converge::diff::shown(&path)),
                json_string(&converge::diff::shown(repository)),
                branch.map_or_else(|| "null".to_owned(), |branch| json_string(&branch)),
            ))?;
        }
    }
    for directory in &options.scan {
        let mut entries: Vec<PathBuf> = fs::read_dir(directory)
            .refuse_at("estate::diff::scan")?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            if !git_directory(&path) || family.contains(&canonical(&path)) {
                continue;
            }
            *summary.classes.entry("dest-only".to_owned()).or_default() += 1;
            let common = git_carry::common_repository(&path).ok();
            line(&format!(
                "{{\"schema\":\"{SCHEMA}\",\"kind\":\"scan\",\"path\":{},\"common\":{},\"class\":\"dest-only\"}}",
                json_string(&converge::diff::shown(&path)),
                common.map_or_else(
                    || "null".to_owned(),
                    |common| json_string(&converge::diff::shown(&common))
                ),
            ))?;
        }
    }
    let counts = |map: &BTreeMap<String, u64>| {
        map.iter()
            .map(|(key, count)| format!("{}:{count}", json_string(key)))
            .collect::<Vec<_>>()
            .join(",")
    };
    line(&format!(
        "{{\"schema\":\"{SCHEMA}\",\"kind\":\"summary\",\"classes\":{{{}}},\"adopt\":{{{}}}}}",
        counts(&summary.classes),
        counts(&summary.adopt),
    ))?;
    Ok(summary)
}

type Omitted = Option<Vec<git_carry::RebuildableOmission>>;

// A plan base, as the corpus names it.
type BaseKey = (String, [u8; 32]);

// The run's staged plan bases (M2r2 item 3), each kept from its first use
// to the last item whose capture names it, then dropped (its stage, and the
// scratch import in it, removed).
struct Bases {
    shared: bool,
    staged: BTreeMap<BaseKey, git_carry::StagedBundle>,
    last: BTreeMap<BaseKey, usize>,
}

impl Bases {
    // The last item index naming each base, read from the capture records
    // and their `.base` sidecars; an item whose records do not read names
    // none (its own read then refuses as it did).
    fn new(items: &[super::Item], corpus: &Path) -> Result<Self> {
        // Exactly `0` turns the share off; unset or any other value keeps it.
        let shared =
            std::env::var_os("BULKLOAD_DIFF_SHARED_IMPORT").is_none_or(|value| value != "0");
        let mut last = BTreeMap::new();
        if shared {
            for (index, item) in items.iter().enumerate() {
                let Ok(captured) = read::<Capture>(&corpus.join(format!("{}.capture", id(item)?)))
                else {
                    continue;
                };
                if !filename(&captured.bundle) {
                    continue;
                }
                if let Ok(Some(base)) = bound(corpus, &captured.bundle) {
                    last.insert((base.bundle, base.digest), index);
                }
            }
        }
        Ok(Self {
            shared,
            staged: BTreeMap::new(),
            last,
        })
    }

    // `base` staged: once per run when shared, else afresh. A refusal is
    // never kept: the next item naming the base stages it again, as every
    // item did before the share (a transient failure refuses one row).
    fn stage(&mut self, corpus: &Path, base: &Base) -> Result<Staged<'_>> {
        let key = (base.bundle.clone(), base.digest);
        if !self.shared || !self.last.contains_key(&key) {
            return stage_base(corpus, base).map(Staged::Own);
        }
        if !self.staged.contains_key(&key) {
            let mut staged = stage_base(corpus, base)?;
            staged.share_imports();
            self.staged.insert(key.clone(), staged);
        }
        self.staged
            .get(&key)
            .map(Staged::Shared)
            .ok_or(BulkloadRefusal::ContractSelfInconsistent)
    }

    // Item `index` is answered: drop every base no later item names.
    fn done(&mut self, index: usize) {
        let last = &self.last;
        self.staged
            .retain(|key, _| last.get(key).is_some_and(|at| *at > index));
    }
}

// A staged base: the run's shared one, or one staged for this item only.
enum Staged<'a> {
    Shared(&'a git_carry::StagedBundle),
    Own(git_carry::StagedBundle),
}

impl Staged<'_> {
    const fn get(&self) -> &git_carry::StagedBundle {
        match self {
            Staged::Shared(staged) => staged,
            Staged::Own(staged) => staged,
        }
    }
}

// `item` with each path spelled through a seatable link (`seats`: live,
// target) spelled at the target instead, and whether any was.
fn seated_item(item: &super::Item, seats: &[(PathBuf, PathBuf)]) -> (bool, super::Item) {
    let mut seated = false;
    let mut moved = |path: &Path| {
        for (live, target) in seats {
            if let Ok(rest) = path.strip_prefix(live) {
                seated = true;
                return if rest.as_os_str().is_empty() {
                    target.clone()
                } else {
                    target.join(rest)
                };
            }
        }
        path.to_path_buf()
    };
    let answered = super::Item {
        source: item.source.clone(),
        repository: moved(&item.repository),
        workspace: item.workspace.as_deref().map(&mut moved),
    };
    (seated, answered)
}

// One item: its capture staged (a chain flattened, a plan base staged),
// then the converge's report function. Nothing is imported at the
// destination: a base goes into the scratch repository only.
fn item_row(
    item: &super::Item,
    (identity, items): (&str, &[String]),
    (corpus, bases): (&Path, &mut Bases),
    source: &str,
    states: &[PathBuf],
    options: &DiffOptions,
) -> Result<(converge::diff::ItemRow, bool, Omitted)> {
    let record = corpus.join(format!("{identity}.capture"));
    if !record.try_exists().refuse_at("estate::diff")? {
        return Err(BulkloadRefusal::CaptureAbsent);
    }
    let captured: Capture = read(&record)?;
    if !filename(&captured.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let published = corpus.join(&captured.bundle);
    if !published.try_exists().refuse_at("estate::diff")? {
        return Err(BulkloadRefusal::SealedObjectMissing);
    }
    let staged = git_carry::stage_bundle(&published).map_err(sealed)?;
    if staged.digest() != captured.digest {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    let (staged, base) = if prior_sidecar(corpus, &captured.bundle)
        .try_exists()
        .refuse_at("estate::diff")?
    {
        let chain = chain_links(corpus, &captured.bundle, LinkBinding::Digest)?;
        (
            git_carry::chain::flatten(staged, &chain.bases, &chain.links)?,
            None,
        )
    } else if git_carry::shared::requires_base(staged.path())? {
        let base = bound(corpus, &captured.bundle)?.ok_or(BulkloadRefusal::SealedObjectMissing)?;
        (staged, Some(bases.stage(corpus, &base)?))
    } else {
        (staged, None)
    };
    let landing = converge::Landing {
        item: identity,
        observed_ns: retained_parts(corpus, &captured.bundle)?.map_or(0, |parts| parts.started_ns),
        capture: captured.digest,
        parents: None,
        exclude_union: None,
        beside: false,
    };
    let target = converge::Target {
        repository: &item.repository,
        workspace: item.workspace.as_deref(),
        source,
        base: base.as_ref().map(Staged::get),
    };
    let mut row = if item.workspace.is_some() {
        converge::diff::workspace(
            &staged,
            &target,
            &landing,
            options.since,
            (
                &options.activate_origin,
                items,
                options.beside.as_deref(),
                options.advance_ancestor,
                options.refs_in,
            ),
        )?
    } else {
        converge::diff::refs_only(
            &staged,
            &target,
            &landing,
            options.since,
            &options.activate_origin,
        )?
    };
    if let (Some(suffix), Some(workspace)) = (options.beside.as_deref(), item.workspace.as_deref())
    {
        retired_beside(&mut row, states, (identity, source), workspace, suffix)?;
    }
    let journal = journal_path(Path::new(""), identity, source, &captured.digest);
    let journaled = states.iter().any(|state| state.join(&journal).is_file());
    Ok((row, journaled, retained_omissions(corpus, &captured.bundle)))
}

// Gap C12: an absent beside path this item's last `.beside` record says it
// landed was folded or retired by the operator's agent, and `estate-apply`
// never lays it down again (`GIT_DESTINATION_MODIFIED`): the row says
// `beside=retired:<path>`, never `landing`.
fn retired_beside(
    row: &mut converge::diff::ItemRow,
    states: &[PathBuf],
    (identity, source): (&str, &str),
    workspace: &Path,
    suffix: &str,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let beside = git_carry::beside::path(workspace, suffix);
    let Some(path) = states
        .iter()
        .rev()
        .map(|state| super::beside_record_path(state, identity, source))
        .find(|path| path.is_file())
    else {
        return Ok(());
    };
    let landed = git_carry::beside::read_record(&path)?.is_some_and(|record| {
        record.item == identity
            && record.path == beside.as_os_str().as_bytes()
            && record.capture != [0; 32]
    });
    if !landed {
        return Ok(());
    }
    for component in &mut row.components {
        for verdict in ["landing", "ignore-policy-conflict"] {
            if let Some(rest) = component.strip_prefix(&format!("beside={verdict}:")) {
                *component = format!("beside=retired:{rest}");
                break;
            }
        }
    }
    Ok(())
}

fn write_row(
    text: &mut String,
    row: &converge::diff::ItemRow,
    journaled: bool,
    omitted: Option<&[git_carry::RebuildableOmission]>,
) {
    use std::fmt::Write as _;
    let _ = write!(
        text,
        ",\"class\":\"{}\",\"hint\":{},\"baseline\":\"{}\",\"adopt\":\"{}\",\"journaled\":{journaled},\"origin\":\"{}\",\"components\":[{}],\"evidence\":[{}]",
        row.class,
        row.hint
            .map_or_else(|| "null".to_owned(), |hint| format!("\"{hint}\"")),
        row.baseline,
        row.adopt,
        row.origin,
        row.components
            .iter()
            .map(|component| json_string(component))
            .collect::<Vec<_>>()
            .join(","),
        row.evidence
            .iter()
            .map(|evidence| json_string(evidence))
            .collect::<Vec<_>>()
            .join(","),
    );
    match omitted {
        None => text.push_str(",\"rebuildable\":\"unknown\""),
        Some(roots) => {
            let entries = roots
                .iter()
                .fold(0u64, |total, root| total.saturating_add(root.entries));
            let bytes = roots
                .iter()
                .fold(0u64, |total, root| total.saturating_add(root.bytes));
            let _ = write!(
                text,
                ",\"rebuildable\":{{\"roots\":{},\"entries\":{entries},\"bytes\":{bytes}}}",
                roots.len()
            );
        }
    }
    text.push_str(",\"refs\":[");
    for (index, reference) in row.refs.iter().enumerate() {
        let _ = write!(
            text,
            "{}{{\"name\":{},\"class\":\"{}\",\"row\":\"{}\"}}",
            if index == 0 { "" } else { "," },
            json_string(&reference.name),
            reference.class,
            reference.row,
        );
    }
    text.push(']');
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// A checkout (`.git` file or directory) or a bare repository.
fn git_directory(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    meta.is_dir()
        && (fs::symlink_metadata(path.join(".git")).is_ok()
            || (path.join("HEAD").is_file() && path.join("objects").is_dir()))
}
