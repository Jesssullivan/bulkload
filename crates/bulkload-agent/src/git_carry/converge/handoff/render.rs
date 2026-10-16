//! The hand-off block's text (TIN-6569, OI-1003-Q183; design section 4).
//!
//! A pure function of [`Inputs`]: the same inputs give the same bytes, in
//! any order of their rows; nothing here reads the clock, the locale or the
//! file system.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// The literal the BEGIN line carries. `--check` looks for it in a target's
/// index, HEAD and destination-ahead blobs; an opt-in hook or a scanner rule
/// can look for it too.
pub const TOKEN: &str = "BULKLOAD-HANDOFF-DO-NOT-COMMIT";
/// The BEGIN line's prefix; a block is recognised only at byte 0.
pub const BEGIN: &[u8] = b"<!-- bulkload-handoff:v1 BEGIN id=";
/// The END line's prefix.
pub const END: &[u8] = b"<!-- bulkload-handoff:v1 END id=";
/// The largest block (design 4.1): deterministic truncation keeps every
/// block under it.
pub const MAX_BLOCK: usize = 64 * 1024;
/// Family rows a block lists before it counts the rest.
pub const FAMILY_ROWS: usize = 40;
/// Ref names a block lists per class before it counts the rest.
pub const REF_NAMES: usize = 30;

const BLANK_ID: &str = "0000000000000000";

/// How the build that rendered a block names itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    pub version: String,
    pub rev: String,
    pub exe: Vec<u8>,
}

/// The live copy that stands beside a landing (`--sibling`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SiblingState {
    /// Nothing at the path.
    Absent,
    /// Something there that Git does not read as a checkout.
    NotACheckout,
    /// A checkout: its HEAD's branch (`None` detached) and commit.
    Head { branch: Option<String>, oid: String },
}

/// The sibling's path and what is there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sibling {
    pub path: Vec<u8>,
    pub state: SiblingState,
    /// For a checkout: whether its HEAD commit and this landing's HEAD
    /// commit are reconciled (one is in the other's history: the sibling's
    /// HEAD reachable from a branch of this landing, or this landing's HEAD
    /// reachable from the sibling's). Read-only, from commits alone.
    pub consolidated: bool,
    /// For a checkout: its working tree has uncommitted changes (a
    /// read-only `git status` lists an entry, untracked files included,
    /// or cannot be read): an open row (OI-1003-Q189).
    pub dirty: bool,
}

/// What a capture held for one checkout (from its provenance instance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureFacts {
    pub branch: Option<String>,
    pub head: String,
    pub staged: u64,
    pub modified: u64,
    pub untracked: u64,
    pub stash: u64,
}

/// A landing's state as its workspace ledger and the seats' prints say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Landing {
    /// A workspace ledger; `changed` seats' prints moved since it.
    Applied { changed: u64 },
    /// Planned, with no workspace ledger for the SOURCE label.
    NotLanded,
    /// A worktree of the repository that no plan item names.
    NotPlanned,
}

/// One worktree of the landing's repository family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyRow {
    pub path: Vec<u8>,
    /// The repository's main worktree (else a linked one).
    pub main: bool,
    /// The workspace this block is written into.
    pub this: bool,
    pub capture: Option<CaptureFacts>,
    /// HEAD at the destination now: branch (`None` detached) and commit.
    pub now: Option<(Option<String>, String)>,
    pub landing: Landing,
}

/// One ref whose destination value is not the landed capture's.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RefDiff {
    /// `estate-diff`'s class: `diverged`, `dest-ahead`, `source-ahead`,
    /// `dest-only` or `source-only` (`same` is never listed).
    pub class: String,
    pub name: String,
}

/// A rebuildable root the capture omitted, and its counted entries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Omitted {
    pub root: Vec<u8>,
    pub entries: u64,
}

/// A worktree of the repository that no plan item names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DestOnly {
    pub path: Vec<u8>,
    /// Its HEAD commit is in another branch of the repository (merged or
    /// picked into the copy that carries the work forward).
    pub consolidated: bool,
    /// Its working tree has uncommitted changes, as for [`Sibling`]
    /// (OI-1003-Q189).
    pub dirty: bool,
}

/// Everything one block says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inputs {
    pub as_of_ns: i128,
    /// The SOURCE label: the operator's name for the origin host.
    pub source: String,
    pub dest_host: String,
    pub plan: Vec<u8>,
    pub plan_digest: [u8; 32],
    pub corpus: Vec<u8>,
    pub state: Vec<u8>,
    pub build: Build,
    pub item: String,
    /// The capture the landing holds, and the corpus's current one.
    pub capture: [u8; 32],
    pub current: Option<[u8; 32]>,
    pub captured_ns: i128,
    pub landed_ns: i128,
    pub workspace: Vec<u8>,
    pub source_path: Vec<u8>,
    pub sibling: Option<Sibling>,
    pub family: Vec<FamilyRow>,
    pub refs: Vec<RefDiff>,
    pub dest_only: Vec<DestOnly>,
    /// Targets whose bytes under this block start with a hand-off block
    /// carried with the source's bytes (another landing's, or this
    /// landing's from a render whose record is gone; OI-1003-Q185): kept
    /// as content, never this landing's block.
    pub carried: Vec<Vec<u8>>,
    /// `(item, code)`: the family's items whose last outcome is a refusal.
    pub refused: Vec<(String, String)>,
    pub omitted: Option<Vec<Omitted>>,
}

/// The ref classes that are open rows.
///
/// The landing holds commits the destination's ref does not
/// (`source-ahead`), or each side holds commits the other lacks
/// (`diverged`). `dest-ahead` is not open: the destination already holds
/// every landed commit, which is what consolidating at the destination
/// leaves.
pub const OPEN_CLASSES: [&str; 2] = ["diverged", "source-ahead"];

impl Inputs {
    /// The rows still to be dealt with (design 4.4, as amended by the
    /// review of 2026-10-16 and OI-1003-Q189): diverged and source-ahead
    /// refs, destination-only worktrees whose HEAD commit no other branch
    /// holds or whose working tree is not clean, a live sibling not
    /// reconciled with this landing or with uncommitted changes, recorded
    /// refusals, and a newer capture not applied yet. Each closes by
    /// consolidating (a merge, a cherry-pick, a commit, an apply, a
    /// disposal), never only by deleting a copy; a destination commit or a
    /// seat edited since landing is consolidation, never an open row.
    #[must_use]
    pub fn open(&self) -> u64 {
        let refs = self
            .refs
            .iter()
            .filter(|row| OPEN_CLASSES.contains(&row.class.as_str()))
            .count();
        let dest_only = self
            .dest_only
            .iter()
            .filter(|row| !row.consolidated || row.dirty)
            .count();
        let sibling = usize::from(self.sibling.as_ref().is_some_and(|sibling| {
            matches!(sibling.state, SiblingState::Head { .. })
                && (!sibling.consolidated || sibling.dirty)
        }));
        let behind = usize::from(self.current.is_some_and(|current| current != self.capture));
        u64::try_from(refs + dest_only + sibling + self.refused.len() + behind).unwrap_or(u64::MAX)
    }

    /// The identity of the landing this block is rendered for: what its
    /// BEGIN line carries as `for=` ([`owner`]).
    #[must_use]
    pub fn owner(&self) -> String {
        owner(&self.dest_host, &self.workspace, &self.source)
    }
}

/// A landing's hand-off identity, the `for=` of its blocks.
///
/// The first 16 hex digits of the blake3 of the destination host, the
/// workspace's absolute path and the SOURCE label. A block whose BEGIN line
/// names another identity was rendered for another landing and carried
/// here with the source's bytes: it is content (OI-1003-Q185), never this
/// landing's block. So is a block of this identity on a target no record
/// entry names (a round trip).
#[must_use]
pub fn owner(host: &str, workspace: &[u8], source: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-handoff-owner-v1\0");
    for part in [host.as_bytes(), workspace, source.as_bytes()] {
        hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part);
    }
    hasher
        .finalize()
        .to_hex()
        .get(..16)
        .unwrap_or_default()
        .to_owned()
}

/// The `for=` identity a BEGIN line at byte 0 of `bytes` names, if any.
#[must_use]
pub fn owner_of(bytes: &[u8]) -> Option<&[u8]> {
    if !bytes.starts_with(BEGIN) {
        return None;
    }
    let at = BEGIN.len() + 16;
    let tag = b" for=";
    (bytes.get(at..at + tag.len()) == Some(tag.as_slice()))
        .then(|| bytes.get(at + tag.len()..at + tag.len() + 16))
        .flatten()
}

/// Bytes as a block prints them: UTF-8 as it is, anything else escaped.
#[must_use]
pub fn shown(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes).map_or_else(|_| bytes.escape_ascii().to_string(), str::to_owned)
}

/// One shell word: as it is when it needs no quoting, else single-quoted.
#[must_use]
pub fn word(bytes: &[u8]) -> String {
    let text = shown(bytes);
    if !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./=:@%+,-".contains(&b))
    {
        text
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn short(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

fn branch(name: Option<&str>) -> String {
    name.map_or_else(
        || "detached".to_owned(),
        |name| name.strip_prefix("refs/heads/").unwrap_or(name).to_owned(),
    )
}

/// `ns` since the epoch as UTC RFC 3339 to the second.
#[must_use]
pub fn rfc3339(ns: i128) -> String {
    let seconds = ns.div_euclid(1_000_000_000);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Howard Hinnant's days_from_civil, inverted.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i128::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// A command a block gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `estate-diff`, read-only.
    Differences,
    /// `estate-handoff --check`.
    Check,
    /// `estate-handoff --clear`.
    Clear,
}

/// The commands a block gives, with every path absolute.
fn command(inputs: &Inputs, verb: Verb) -> String {
    let exe = word(&inputs.build.exe);
    let (plan, corpus, state) = (
        word(&inputs.plan),
        word(&inputs.corpus),
        word(&inputs.state),
    );
    let source = word(inputs.source.as_bytes());
    let workspace = word(&inputs.workspace);
    match verb {
        Verb::Differences => format!("{exe} estate-diff {plan} {corpus} {source} {state}"),
        Verb::Check => format!(
            "{exe} estate-handoff --check --workspace {workspace} {plan} {corpus} {state} {source}"
        ),
        Verb::Clear => format!(
            "{exe} estate-handoff --clear --workspace {workspace} {plan} {corpus} {state} {source}"
        ),
    }
}

/// The block for `inputs`: deterministic, its id the first 16 hex digits
/// of its own blake3 computed with the id blanked. `compact` leaves the
/// family table and the ref names out (the size cap's fallback).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn render(inputs: &Inputs, compact: bool) -> Vec<u8> {
    let mut family = inputs.family.clone();
    family.sort_by(|a, b| a.path.cmp(&b.path));
    let mut refs = inputs.refs.clone();
    refs.sort();
    refs.dedup();
    let mut dest_only = inputs.dest_only.clone();
    dest_only.sort();
    dest_only.dedup();
    let mut carried = inputs.carried.clone();
    carried.sort();
    carried.dedup();
    let mut refused = inputs.refused.clone();
    refused.sort();
    refused.dedup();
    let source = &inputs.source;
    let host = &inputs.dest_host;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<!-- bulkload-handoff:v1 BEGIN id={BLANK_ID} for={} {TOKEN} · bulkload owns this block: do not edit inside it, do not commit it -->",
        inputs.owner()
    );
    out.push_str(
        "> **bulkload hand-off: converge and consolidate this workspace before any other work.**\n",
    );
    let _ = writeln!(
        out,
        "> bulkload landed this checkout from source `{source}`. Before you start anything else"
    );
    out.push_str("> the user asks for, tell the user this hand-off is open and work through\n");
    out.push_str("> \"Do this first\" with them. (Linear TIN-6569, ruling OI-1003-Q183)\n\n");
    out.push_str("| Fact | Value |\n|---|---|\n");
    let _ = writeln!(out, "| As of | {} |", rfc3339(inputs.as_of_ns));
    let _ = writeln!(
        out,
        "| Captured | {} on source `{source}` |",
        rfc3339(inputs.captured_ns)
    );
    let _ = writeln!(
        out,
        "| Landed | {} on `{host}` |",
        rfc3339(inputs.landed_ns)
    );
    let _ = writeln!(
        out,
        "| Source path | `{}` (on source `{source}`) |",
        shown(&inputs.source_path)
    );
    let _ = writeln!(out, "| This landing | `{}` |", shown(&inputs.workspace));
    let sibling = inputs.sibling.as_ref().map_or_else(
        || "none given (`--sibling`)".to_owned(),
        |sibling| {
            let path = shown(&sibling.path);
            match &sibling.state {
                SiblingState::Absent => format!("`{path}`: absent"),
                SiblingState::NotACheckout => format!("`{path}`: exists, not a Git checkout"),
                SiblingState::Head { branch: name, oid } => format!(
                    "`{path}`: exists, HEAD `{}` at `{}`; {}; {}",
                    branch(name.as_deref()),
                    short(oid),
                    if sibling.consolidated {
                        "this landing holds its HEAD (reconciled)"
                    } else {
                        "not reconciled: this landing does not hold its HEAD (an open row)"
                    },
                    if sibling.dirty {
                        "its working tree has uncommitted changes (an open row)"
                    } else {
                        "its working tree is clean"
                    }
                ),
            }
        },
    );
    let _ = writeln!(out, "| Live sibling | {sibling} |");
    let _ = writeln!(
        out,
        "| Plan | `{}` (blake3 `{}…`) |",
        shown(&inputs.plan),
        hex(inputs.plan_digest.get(..6).unwrap_or_default())
    );
    let _ = writeln!(out, "| Corpus | `{}` |", shown(&inputs.corpus));
    let _ = writeln!(out, "| State | `{}` |", shown(&inputs.state));
    let current = match inputs.current {
        Some(current) if current == inputs.capture => "the corpus's current capture".to_owned(),
        Some(current) => format!(
            "the corpus holds a newer capture `{}…`, not applied",
            hex(current.get(..6).unwrap_or_default())
        ),
        None => "the corpus holds no capture record for the item".to_owned(),
    };
    let _ = writeln!(
        out,
        "| Capture | bundle `{}…`, item `{}…`; {current} |",
        hex(inputs.capture.get(..6).unwrap_or_default()),
        inputs.item.get(..12).unwrap_or(&inputs.item)
    );
    let _ = writeln!(
        out,
        "| Built by | bulkload-agent {}, rev `{}`, `{}` |",
        inputs.build.version,
        inputs.build.rev,
        shown(&inputs.build.exe)
    );
    if !compact {
        let _ = write!(
            out,
            "\n**This repository's paths** (at capture on source `{source}`; now on `{host}`)\n\n"
        );
        let _ = writeln!(
            out,
            "| Path on `{host}` | Role | At capture: branch @ HEAD | Staged · modified · untracked · stash | Now: branch @ HEAD | Landing |"
        );
        out.push_str("|---|---|---|---|---|---|\n");
        for row in family.iter().take(FAMILY_ROWS) {
            let role = match (row.main, row.this) {
                (true, true) => "main (this)",
                (true, false) => "main",
                (false, true) => "linked (this)",
                (false, false) => "linked",
            };
            let (captured, counts) = row.capture.as_ref().map_or_else(
                || ("not captured".to_owned(), "-".to_owned()),
                |facts| {
                    (
                        format!(
                            "`{}@{}`",
                            branch(facts.branch.as_deref()),
                            short(&facts.head)
                        ),
                        format!(
                            "{} · {} · {} · {}",
                            facts.staged, facts.modified, facts.untracked, facts.stash
                        ),
                    )
                },
            );
            let now = row.now.as_ref().map_or_else(
                || "unborn".to_owned(),
                |(name, oid)| format!("`{}@{}`", branch(name.as_deref()), short(oid)),
            );
            let landing = match row.landing {
                Landing::Applied { changed: 0 } => "applied, unchanged since landing".to_owned(),
                Landing::Applied { changed } => {
                    format!("applied, {changed} seat(s) changed since landing")
                }
                Landing::NotLanded => "planned, not landed".to_owned(),
                Landing::NotPlanned => "destination-only worktree, not planned".to_owned(),
            };
            let _ = writeln!(
                out,
                "| `{}` | {role} | {captured} | {counts} | {now} | {landing} |",
                shown(&row.path)
            );
        }
        if family.len() > FAMILY_ROWS {
            let _ = writeln!(
                out,
                "\n{} more worktree(s) of this repository are not listed.",
                family.len() - FAMILY_ROWS
            );
        }
    }
    out.push_str("\n**Differences** (estate-diff's ref classes, against the landed capture; open rows: diverged and source-ahead)\n\n");
    let classes = [
        "diverged",
        "dest-ahead",
        "source-ahead",
        "dest-only",
        "source-only",
    ];
    let mut parts = Vec::new();
    for class in classes {
        let names: Vec<&str> = refs
            .iter()
            .filter(|row| row.class == class)
            .map(|row| row.name.as_str())
            .collect();
        if names.is_empty() {
            continue;
        }
        if compact {
            parts.push(format!("{} {class}", names.len()));
            continue;
        }
        let mut listed: Vec<String> = names
            .iter()
            .take(REF_NAMES)
            .map(|name| format!("`{name}`"))
            .collect();
        if names.len() > REF_NAMES {
            listed.push(format!("and {} more", names.len() - REF_NAMES));
        }
        parts.push(format!("{} {class} ({})", names.len(), listed.join(", ")));
    }
    let _ = writeln!(
        out,
        "- Refs: {}.",
        if parts.is_empty() {
            "every ref is the landed capture's".to_owned()
        } else {
            parts.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "- Destination-only worktrees: {}.",
        if dest_only.is_empty() {
            "none".to_owned()
        } else if compact {
            dest_only.len().to_string()
        } else {
            let mut listed: Vec<String> = dest_only
                .iter()
                .take(REF_NAMES)
                .map(|row| {
                    format!(
                        "`{}` ({}{})",
                        shown(&row.path),
                        if row.consolidated {
                            "its HEAD is in another branch"
                        } else {
                            "open: its HEAD is in no other branch"
                        },
                        if row.dirty {
                            "; open: uncommitted changes"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            if dest_only.len() > REF_NAMES {
                listed.push(format!("and {} more", dest_only.len() - REF_NAMES));
            }
            format!("{} ({})", dest_only.len(), listed.join(", "))
        }
    );
    let _ = writeln!(
        out,
        "- Refusals recorded for this repository's items: {}.",
        if refused.is_empty() {
            "none".to_owned()
        } else {
            refused
                .iter()
                .take(REF_NAMES)
                .map(|(item, code)| format!("`{}…` `{code}`", item.get(..12).unwrap_or(item)))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    let omitted = match &inputs.omitted {
        None => "unknown (a capture from before the record)".to_owned(),
        Some(roots) if roots.is_empty() => "none".to_owned(),
        Some(roots) => {
            let mut roots = roots.clone();
            roots.sort();
            roots
                .iter()
                .take(REF_NAMES)
                .map(|root| format!("`{}` ({} entries)", shown(&root.root), root.entries))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let _ = writeln!(out, "- Rebuildable roots omitted at capture: {omitted}.");
    if !carried.is_empty() {
        let names: Vec<String> = carried
            .iter()
            .map(|name| format!("`{}`", shown(name)))
            .collect();
        let _ = writeln!(
            out,
            "- Carried hand-off blocks: {} also hold, below this block, a hand-off block that came with the source's bytes (rendered for another landing, or for this one by a run whose record is gone). It is not this block and may be out of date: do not act on its paths or commands. bulkload keeps it as the file's content (OI-1003-Q185); removing it is the user's call (`--clear --clear-carried` does it as a recorded edit, OI-1003-Q190).",
            names.join(", ")
        );
    }
    let _ = writeln!(
        out,
        "- Open rows: {} (the hand-off is ready to clear at 0). A commit at this landing or a file edited since landing is consolidation, not an open row. `ready` also needs a clean working tree in the live sibling and in each destination-only worktree (OI-1003-Q189); ask the user about work not yet committed at this landing.",
        inputs.open()
    );
    out.push_str("\n**Do this first** (converge and consolidate before any other work)\n\n");
    out.push_str("1. Tell the user this hand-off is open. Do not start other work until it is cleared or the user explicitly defers it.\n");
    let _ = writeln!(
        out,
        "2. See what differs, read-only:\n   `{}`",
        command(inputs, Verb::Differences)
    );
    out.push_str("3. For each diverged or source-ahead ref, each destination-only worktree and the live sibling, decide with the user which copy carries the work forward, and bring the other side's commits into it (merge or cherry-pick). Settle uncommitted changes in the sibling and in destination-only worktrees with the user (commit them where the work goes on). Never reset, force-push, delete a branch, or delete either copy: removing a copy is the operator's call.\n");
    let _ = writeln!(
        out,
        "4. Commit or stash nothing that contains this block. Before any commit, check that `git diff --cached` shows no `{TOKEN}`."
    );
    out.push_str("\n**When it is done**\n\n");
    let _ = writeln!(
        out,
        "- `{}` prints `\"ready\":true` (0 open rows).",
        command(inputs, Verb::Check)
    );
    let _ = writeln!(
        out,
        "- Then remove this block with `{}`.\n  Do not delete the block by hand, and do not edit inside it.",
        command(inputs, Verb::Clear)
    );
    let _ = write!(out, "<!-- bulkload-handoff:v1 END id={BLANK_ID} -->\n\n");
    let id = blake3::hash(out.as_bytes()).to_hex();
    let id = id.get(..16).unwrap_or_default();
    out.replace(&format!("id={BLANK_ID}"), &format!("id={id}"))
        .into_bytes()
}

/// What starts a file (design 4.1): a block is recognised only at byte 0,
/// and only with its END line and the empty line after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// No marker at byte 0.
    None,
    /// A well-formed block of this many bytes (both lines included).
    Block(usize),
    /// A BEGIN line with no matching END line and empty line: an edit
    /// inside the block, which makes the file the user's.
    Malformed,
}

/// The marker at byte 0 of `bytes` (at most [`MAX_BLOCK`] of them are
/// looked at).
#[must_use]
pub fn marker(bytes: &[u8]) -> Marker {
    if !bytes.starts_with(BEGIN) {
        return Marker::None;
    }
    let id = bytes.get(BEGIN.len()..BEGIN.len() + 16);
    let window = bytes.get(..MAX_BLOCK.min(bytes.len())).unwrap_or_default();
    let mut at = 0;
    while let Some(found) = window
        .get(at..)
        .and_then(|rest| rest.windows(END.len()).position(|w| w == END))
    {
        let start = at + found;
        // The END line starts a line, names the BEGIN line's id, and ends
        // the block with one empty line after it.
        let line_start = start == 0 || window.get(start - 1) == Some(&b'\n');
        let names = window.get(start + END.len()..start + END.len() + 16) == id;
        let tail = b" -->\n\n";
        let closes = window.get(start + END.len() + 16..start + END.len() + 16 + tail.len())
            == Some(tail.as_slice());
        if line_start && names && closes {
            return Marker::Block(start + END.len() + 16 + tail.len());
        }
        at = start + 1;
    }
    Marker::Malformed
}

/// A fixed block's inputs, for the unit rows here and in `handoff::tests`.
#[cfg(test)]
pub(super) fn sample() -> Inputs {
    Inputs {
        as_of_ns: 1_790_000_000_000_000_000,
        source: "neo".to_owned(),
        dest_host: "sting".to_owned(),
        plan: b"/estate/plan".to_vec(),
        plan_digest: [7; 32],
        corpus: b"/estate/corpus".to_vec(),
        state: b"/estate/state".to_vec(),
        build: Build {
            version: "0.1.0".to_owned(),
            rev: "unknown".to_owned(),
            exe: b"/nix/store/x-bulkload/bin/bulkload-agent".to_vec(),
        },
        item: "a".repeat(64),
        capture: [1; 32],
        current: Some([1; 32]),
        captured_ns: 1_789_000_000_000_000_000,
        landed_ns: 1_789_500_000_000_000_000,
        workspace: b"/estate/gf/main".to_vec(),
        source_path: b"/home/u/git/gf".to_vec(),
        sibling: None,
        family: vec![
            FamilyRow {
                path: b"/estate/gf/main".to_vec(),
                main: true,
                this: true,
                capture: None,
                now: Some((Some("refs/heads/main".to_owned()), "b".repeat(40))),
                landing: Landing::Applied { changed: 0 },
            },
            FamilyRow {
                path: b"/estate/gf/wt".to_vec(),
                main: false,
                this: false,
                capture: None,
                now: None,
                landing: Landing::NotPlanned,
            },
        ],
        refs: vec![
            RefDiff {
                class: "diverged".to_owned(),
                name: "refs/heads/x".to_owned(),
            },
            RefDiff {
                class: "dest-only".to_owned(),
                name: "refs/heads/y".to_owned(),
            },
        ],
        dest_only: vec![DestOnly {
            dirty: false,
            path: b"/estate/gf/wt".to_vec(),
            consolidated: false,
        }],
        carried: Vec::new(),
        refused: Vec::new(),
        omitted: Some(Vec::new()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn render_is_pure_and_order_free() {
        let inputs = sample();
        let once = render(&inputs, false);
        assert_eq!(render(&inputs, false), once);
        let mut permuted = inputs;
        permuted.family.reverse();
        permuted.refs.reverse();
        assert_eq!(render(&permuted, false), once);
        assert_eq!(marker(&once), Marker::Block(once.len()));
        let text = String::from_utf8(once).unwrap();
        assert!(text.contains(TOKEN));
        assert!(text.contains("2026-09-21T"), "{text}");
        assert!(!text.contains(BLANK_ID), "{text}");
    }

    #[test]
    fn render_truncates_family_and_ref_rows_deterministically() {
        let mut inputs = sample();
        let row = inputs.family[1].clone();
        inputs.family = (0..100)
            .map(|n| FamilyRow {
                path: format!("/estate/gf/wt-{n:03}").into_bytes(),
                ..row.clone()
            })
            .collect();
        inputs.refs = (0..100)
            .map(|n| RefDiff {
                class: "diverged".to_owned(),
                name: format!("refs/heads/r{n:03}"),
            })
            .collect();
        let text = String::from_utf8(render(&inputs, false)).unwrap();
        assert!(text.contains("60 more worktree(s)"), "{text}");
        assert!(text.contains("and 70 more"), "{text}");
        assert!(!text.contains("wt-040`"), "{text}");
        assert!(text.len() < MAX_BLOCK);
        let compact = String::from_utf8(render(&inputs, true)).unwrap();
        assert!(compact.contains("100 diverged"), "{compact}");
        assert!(!compact.contains("This repository's paths"));
    }

    #[test]
    fn a_marker_counts_only_at_byte_zero_and_needs_its_end() {
        let block = render(&sample(), false);
        let mut file = block.clone();
        file.extend_from_slice(b"# Agents\n");
        assert_eq!(marker(&file), Marker::Block(block.len()));
        let mut later = b"x\n".to_vec();
        later.extend_from_slice(&file);
        assert_eq!(marker(&later), Marker::None);
        // No END line, an END with another id, no empty line after it.
        let cut = &block[..block.len() - 30];
        assert_eq!(marker(cut), Marker::Malformed);
        let text = String::from_utf8(block.clone()).unwrap();
        let id = &text[BEGIN.len()..BEGIN.len() + 16];
        let other = text.replacen(&format!("END id={id}"), "END id=ffffffffffffffff", 1);
        assert_eq!(marker(other.as_bytes()), Marker::Malformed);
        let tight = &block[..block.len() - 1];
        assert_eq!(marker(tight), Marker::Malformed);
    }

    #[test]
    fn words_are_quoted_for_a_shell() {
        assert_eq!(word(b"/a/b-c_d.e"), "/a/b-c_d.e");
        assert_eq!(word(b"/a b"), "'/a b'");
        assert_eq!(word(b"/it's"), "'/it'\\''s'");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400_000_000_000), "2000-02-29T00:00:00Z");
    }
}
