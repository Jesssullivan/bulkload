# Wayfinding — the Bulkload → sting migration week (2026-08-21 → 2026-09-02)

This document is the shareable map of the migration week for the operator's other
agents: what happened, what was ruled, where every artifact lives, and the historical decisions. Verbatim source material lives in [`docs/wayfinding-20260829/`](wayfinding-20260829/).

## 1. The week in one page

The Bulkload v4 Python engine — 11,081 LOC, seven verbs, A/B stillness-pair custody,
per-mutation journal reseals — spent 106h31m across the week attempting the
neo → sting estate migration: 45 failures, 20 distinct defect classes, **zero completed
ceremonies**. On landing day (2026-08-29) alone it failed apply 13 times across 12 new
defect classes (shallow-graft collisions, unborn-repo zero-OID heads, kernel-tree
fsck tag artifacts, promisor partial-clone classification, ~60-minute lap prefixes).
At 13:49:30Z the operator killed it with the verdict, thrice stated:

> "this bulkload disaster project is foundationally flawed... fallacies into a
> simple migration, antipatterns into our complex greenfield TCFS estate"

The ledger corroborated (week-review: 5/6 operator assertions TRUE, 0 refuted).
The landing finished **the boring way** — a from-scratch rsync + git script with no
engine: frozen-clone install, gitfile-pointer rewrite, remotes/fetch, HEAD sync,
bundles + stashes + worktree adds, live working-tree topup, doc-truth PR, tmux
`main`. `BORING-LANDED` at 2026-08-30T21:17:56Z. Attended adoption evidence
(TIN-3692) recorded `overall_met=true` on 2026-08-31. Residue (gitdir pointers,
failed fetches, HEAD syncs, sops items) was cleared and ~515G of migration debris
reclaimed on 2026-09-02; receipts archived at
`tinyland-state/archives/bulkload-migration-20260829.tgz`.

What survives of the engine: the defensible core (worktree gitdir-pointer rewrite,
WAL-consistent SQLite capture/compose, exact git index fidelity, typed refusals,
~10 portable cross-kernel defect classes) and the product bar. What does not: the
ceremony/quiesce architecture, the A/B stillness pair, the O(n²) journal reseal.

**The rebuild (R25, issue #34):** Rust, as tummycrypt workspace crates
(`tcfs-bulkload-proto` / `tcfs-bulkload-agent` / `tcfs-bulkload` driver), per-file
freshness-key identity everywhere, append-only WAL journal, live-host-only, benched
against rclone before any speed claim (R23). First draft PR:
[tummycrypt#592](https://github.com/Jesssullivan/tummycrypt/pull/592).

## 2. Operator dialog chronology + interview trees

Verbatim, typos preserved, credentials redacted:
[`wayfinding-20260829/dialogs-interviews.md`](wayfinding-20260829/dialogs-interviews.md)
— 176 de-duplicated operator messages and 38 AskUserQuestion interview trees,
2026-07-18 → 2026-09-02, timestamped and session-attributed.

## 3. Rulings ledger R1–R43

[`wayfinding-20260829/rulings-ledger.md`](wayfinding-20260829/rulings-ledger.md) —
all 43 rulings located, with verbatim text (or marked summaries), source citation,
and standing/superseded/discharged status. Standing invariants of note:

- **R25 product bar (verbatim, issue #34):** beat rclone as a live-host,
  never-rewalk, piecemeal estate mover — "the host need not halt agent work or git
  work to initiate migration... without EVER rewalking or reading a bit twice."
- **R23 earn-it rule:** never claim "supersedes rclone" until a benchmark win on the
  real corpus.
- **Never-signal invariant:** a migration agent never signals/stops interactive
  sessions or daemons it does not own (born of the 2026-08-23 SIGSTOP incident).
- **Kill criterion:** any migration lap failing twice on NEW defect classes falls
  back to the boring tool that day.

## 4. Artifact map

- Durable receipts: `tinyland-state/archives/bulkload-migration-20260829.tgz`
  (STATUS ledger, week-review, interview packets, judge docs, 41 run logs).
- The append-only STATUS ledger (kept in place):
  `tinyland-state/bulkload-boundary-20260824/STATUS`.

## 5. Where work continues

| Lane | Where |
|---|---|
| R25 Rust rebuild | tummycrypt `crates/tcfs-bulkload*` (PR #592 draft; milestones M0–M10) |
| Product bar | this repo, issue #34 |
| Python engine (archived reference) | this repo Git history (runtime retired) |
| Sting seat ops | lab `docs/operations/STING_FIRST_HOUR.md`, PR #1595 |
| TCFS follow-on | TIN-1556 (D4), TIN-4193 (fabric F1–F3), TIN-4194 (v0.12.19) |
