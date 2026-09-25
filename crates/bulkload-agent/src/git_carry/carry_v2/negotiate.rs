//! M1's first round: exactly the held tips as haves, ancestors first (R-N113,
//! R-N116), under the R-N75 refusals.

use std::collections::{BTreeMap, BTreeSet};

use super::super::estimate::{present, Refused};
use super::{is_oid, lines, pinned, run_child, Offer, Outcome, Source};
use crate::BulkloadRefusal;

/// The request M1's first round packs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstRound {
    /// What the source sends: the wants given, minus every held tip (a fetch
    /// never wants what the destination holds). Sorted.
    pub wants: Vec<String>,
    /// Exactly the held tips: every offered tip the source holds as an
    /// object, ancestors first; tips that peel to no commit last, by oid.
    pub haves: Vec<String>,
    /// The destination's shallow frontier, sorted; non-empty only when it
    /// equals the source's (R-N75).
    pub shallow: Vec<String>,
    /// Offered tips.
    pub destination_tips: usize,
    /// Held tips that peel to no commit (placed last).
    pub non_commit_haves: usize,
}

impl FirstRound {
    /// Whether the destination is shallow, which selects
    /// `--objects-edge-aggressive` as upload-pack's `--shallow` does.
    #[must_use]
    pub const fn shallow_destination(&self) -> bool {
        !self.shallow.is_empty()
    }
}

/// Build the first round of carrying `wants` from `source` to the destination
/// that made `offer`.
///
/// # Errors
/// Refuses `GIT_HAVES_UNPROVABLE` for a partial-clone destination or a
/// shallow one whose frontier differs from the source's (R-N75),
/// `GIT_INVENTORY_MALFORMED` for an offer or want that is not an object name,
/// and a child whose output is not the shape promised.
pub fn first_round(
    source: &Source,
    offer: &Offer,
    wants: &BTreeSet<String>,
    store: Option<&super::StderrStore>,
) -> Outcome<FirstRound> {
    let named = |set: &BTreeSet<String>| set.iter().all(|value| is_oid(value.as_bytes()));
    if !named(&offer.tips) || !named(&offer.shallow) || !named(wants) {
        return Err(BulkloadRefusal::GitInventoryMalformed.into());
    }
    if offer.partial {
        return Err(Refused::because(
            BulkloadRefusal::GitHavesUnprovable,
            "destination_partial_clone",
        ));
    }
    if !offer.shallow.is_empty() && offer.shallow != source.shallow {
        return Err(Refused::because(
            BulkloadRefusal::GitHavesUnprovable,
            "destination_shallow_frontier_differs",
        ));
    }
    let held = present(&source.repository, &offer.tips)?;
    let haves = ancestors_first(source, &held, store)?;
    let non_commit_haves = haves.1;
    let held_oids: BTreeSet<&str> = held.iter().map(|(value, _)| value.as_str()).collect();
    Ok(FirstRound {
        wants: wants
            .iter()
            .filter(|value| !held_oids.contains(value.as_str()))
            .cloned()
            .collect(),
        haves: haves.0,
        shallow: offer.shallow.iter().cloned().collect(),
        destination_tips: offer.tips.len(),
        non_commit_haves,
    })
}

/// `held` (oid, type) ordered so every commit comes after its held
/// ancestors: tips are sorted by the position of their peeled commit in
/// `rev-list --topo-order --reverse` over all of them (ties by oid), and tips
/// that peel to no commit follow, by oid (spike D1). Upload-pack drops a
/// commit have only when a child have marked it first, so in this order it
/// keeps every have (R-N116). Returns the order and the non-commit count.
fn ancestors_first(
    source: &Source,
    held: &[(String, String)],
    store: Option<&super::StderrStore>,
) -> Outcome<(Vec<String>, usize)> {
    let peeled = peel(source, held, store)?;
    let commits: BTreeSet<&str> = peeled.values().map(String::as_str).collect();
    let mut position: BTreeMap<String, usize> = BTreeMap::new();
    if !commits.is_empty() {
        let mut input = Vec::new();
        for commit in &commits {
            input.extend_from_slice(commit.as_bytes());
            input.push(b'\n');
        }
        let mut next = 0_usize;
        run_child(
            pinned(source).args(["rev-list", "--topo-order", "--reverse", "--stdin"]),
            &input,
            store,
            "topological_order_failed",
            |stdout| {
                lines(stdout, |line| {
                    if !is_oid(line) {
                        return Err(BulkloadRefusal::GitInventoryMalformed);
                    }
                    let value = std::str::from_utf8(line)
                        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
                    if commits.contains(value) {
                        position.insert(value.to_owned(), next);
                    }
                    next += 1;
                    Ok(())
                })
            },
        )?;
        if position.len() != commits.len() {
            return Err(BulkloadRefusal::ContractSelfInconsistent.into());
        }
    }
    let mut ordered: Vec<(usize, &str)> = Vec::with_capacity(held.len());
    let mut rest: Vec<&str> = Vec::new();
    for (value, _) in held {
        match peeled.get(value).and_then(|commit| position.get(commit)) {
            Some(at) => ordered.push((*at, value)),
            None => rest.push(value),
        }
    }
    ordered.sort_unstable();
    rest.sort_unstable();
    let non_commit = rest.len();
    Ok((
        ordered
            .into_iter()
            .map(|(_, value)| value)
            .chain(rest)
            .map(str::to_owned)
            .collect(),
        non_commit,
    ))
}

/// The commit each held tip peels to (`<oid>^{commit}`), for the tips that
/// peel to one: commits themselves and tags (of tags) of commits.
fn peel(
    source: &Source,
    held: &[(String, String)],
    store: Option<&super::StderrStore>,
) -> Outcome<BTreeMap<String, String>> {
    let tags: Vec<&String> = held
        .iter()
        .filter(|(_, kind)| kind == "tag")
        .map(|(value, _)| value)
        .collect();
    let mut peeled: BTreeMap<String, String> = held
        .iter()
        .filter(|(_, kind)| kind == "commit")
        .map(|(value, _)| (value.clone(), value.clone()))
        .collect();
    if tags.is_empty() {
        return Ok(peeled);
    }
    let mut input = Vec::new();
    for tag in &tags {
        input.extend_from_slice(tag.as_bytes());
        input.extend_from_slice(b"^{commit}\n");
    }
    let mut answers = Vec::with_capacity(tags.len());
    run_child(
        pinned(source).args(["cat-file", "--batch-check=%(objectname)"]),
        &input,
        store,
        "peel_failed",
        |stdout| {
            lines(stdout, |line| {
                answers.push(line.to_vec());
                Ok(())
            })
        },
    )?;
    if answers.len() != tags.len() {
        return Err(BulkloadRefusal::GitInventoryMalformed.into());
    }
    for (tag, answer) in tags.into_iter().zip(answers) {
        if is_oid(&answer) {
            let commit =
                String::from_utf8(answer).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
            peeled.insert(tag.clone(), commit);
        } else if !answer.ends_with(b" missing") {
            return Err(BulkloadRefusal::GitInventoryMalformed.into());
        }
    }
    Ok(peeled)
}
