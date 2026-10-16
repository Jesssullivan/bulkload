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

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{
    bound, chain_links, filename, id, journal_path, prior_sidecar, read, retained_omissions,
    retained_parts, sealed, stage_base, Capture, LinkBinding, Plan,
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
    let mut seen = BTreeSet::new();
    for item in &contents.items {
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
        match item_row(item, (&identity, &items), corpus, source, states, options) {
            Ok((row, journaled, omitted)) => {
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

// One item: its capture staged (a chain flattened, a plan base staged),
// then the converge's report function. Nothing is imported at the
// destination: a base goes into the scratch repository only.
fn item_row(
    item: &super::Item,
    (identity, items): (&str, &[String]),
    corpus: &Path,
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
        let base = stage_base(corpus, &base)?;
        (staged, Some(base))
    } else {
        (staged, None)
    };
    let landing = converge::Landing {
        item: identity,
        observed_ns: retained_parts(corpus, &captured.bundle)?.map_or(0, |parts| parts.started_ns),
        capture: captured.digest,
    };
    let target = converge::Target {
        repository: &item.repository,
        workspace: item.workspace.as_deref(),
        source,
        base: base.as_ref(),
    };
    let row = if item.workspace.is_some() {
        converge::diff::workspace(
            &staged,
            &target,
            &landing,
            options.since,
            (&options.activate_origin, items),
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
    let journal = journal_path(Path::new(""), identity, source, &captured.digest);
    let journaled = states.iter().any(|state| state.join(&journal).is_file());
    Ok((row, journaled, retained_omissions(corpus, &captured.bundle)))
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
