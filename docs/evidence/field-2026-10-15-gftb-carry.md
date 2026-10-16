# Field evidence: the GFTB carry, neo to sting, 2026-10-15

**Label: field evidence, one run, outside any gate, deciding no SLO.** It
is the first carry of a real working estate (the GFTB repositories and
their worktrees) by the current engine. Nothing here is an S1 sample, an
S2 budget measurement or a gated S3 ratio.

Rulings: OI-1003-Q163 (resume GFTB on sting with bulkload), OI-1003-Q167
(the reconcile's `swap` infra mode), R-N13. **Authority note, as the
receipt records it:** the lane quoted OI-1003-Q163 from its workflow's
computed task text, and found no independent record of Q163 outside the
lane. The reconcile receipt records the operator's authority for Q163 and
Q167 on 2026-10-15.

## Provenance

Every number below is copied from the lane's receipts on sting. They are
not in this tree; they are identified by digest. Paths are symbolic:

| name | what it is |
|---|---|
| `<lane>` | the lane's private receipt directory on sting (`gftb-resume-20261015`) |
| `<sting-git>` | sting's live Git root, holding the live GFTB repositories |
| `<landing>` | the fresh estate root the carry was applied into, on sting |
| `<carry>` | the pulled corpus, under `<lane>` |
| `<neo-capture>` | the capture directory on neo's state volume |

| receipt | sha256 | written |
|---|---|---|
| `<lane>/carry.receipt` | `d2d4852f71c51148e47d6e73373de40408286fe7b99831468bf6c628ff6a5713` | 2026-10-15T16:19:07Z |
| `<lane>/reconcile.receipt` | `637867b736b02c37774982aa0bd58e43f2941e6b7b455cadcbc5a42ac73a03c2` | 2026-10-15T17:06:14Z |

The digests were re-checked on 2026-10-16. Where a figure was also in the
lane's raw outputs beside the receipt (`neo-capture-*.log`,
`sting-apply-*.log`, `closure-*.out`, `verify-final.tsv`), it was compared
and agrees.

## The agent

- **Revision**: `main` at `f4988060cf495d1c4a546452399214fc86d297ed` (the
  #224 merge; #225 was not merged at build time). Not the S1 stack.
- **sting** (x86_64-linux): built in a detached worktree, inside
  `nix develop .#default`, `nice 19`, `--locked`, its own
  `CARGO_TARGET_DIR`, 5 min 21 s. Binary
  `~/.local/bin/bulkload-agent-f4988060cf49`, 5,795,040 bytes, sha256
  `5b2fe9df448151aa210bb03dd79370869c54245e5787916b2836459852f77437`.
- **neo** (aarch64-darwin): built from a fresh clone of a bundle made on
  sting (neo's own checkout untouched), same recipe, about 2 min 16 s.
  Binary `~/.local/bin/bulkload-agent-f4988060cf49`, 4,764,480 bytes,
  sha256 `0d6c3d9ee18cd7be2e218bf2932d5d17a6e9722f9a58ade970aee3ce7e11743a`,
  plus a `--priority=normal` wrapper.
- The two binaries differ by platform. The agent has no `--version` verb.
  `selftest` passed on both; with dev, ino and times normalised the
  outputs are identical except frame bytes 83 against 84 (the varint width
  of dev, ino and mtime).

## The plan

137 items, equal to the inventory's neo rows, in one plan run two jobs
wide:

- 13 main checkouts, standalone (workspace equals repository);
- 123 linked worktrees, each whose repository is its main's landing (36
  nested in one repository's `.worktrees`, 1 in another's);
- 1 bare repository, carried refs-only. Neo's original has
  `objects/info/alternates`, so it was carried from a `--mirror
  --no-local` copy under `<neo-capture>` (fsck 0, refs identical, 3).

## First pass

| step | result |
|---|---|
| neo `estate-capture` | exit 0, wall 287 s (15:52:45Z to 15:57:32Z), 137 `captured`, 0 refusals. Summed per-item `source_bytes_read` 1,582,312,069; `read_hash_file_bytes` = `write_source_pack_bytes` = 285,638,037; 77,122 pack objects; max RSS 508,608,512 B. Corpus 289,848 KiB. |
| sting `pull` | exit 0, 32 s, 688 completed, 0 reused, `bytes_received` 294,639,856, 0 refusals, SQLite mode `refuse` (no SQLite stores in the plan). `<carry>` 290,004 KiB. |
| sting `estate-apply` into `<landing>` | exit 0, wall 864 s at sting load 23 to 48: 136 `workspace-restored`, 1 `refs-imported`, 0 refusals, `source_bytes_read` 0, `read_bundle_stage_bytes` 329,161,191. Landing 2,176,972 KiB. Linked items carry the reason `ref-checked-out-elsewhere=N`, a custody note, not a refusal. Pre-made before the apply: the landing root, the four `*.worktrees` parents outside main workspaces, and an empty bare for the refs-only item. |
| `closure-report` | `verdict=pass`, `gate=pass`: planned 137, applied 136, referenced-only 1, refused 0, pending review 0, **unaccounted 0**. |

## Rerun on the unchanged source (S3's unchanged clause)

| step | result |
|---|---|
| neo `estate-capture` | exit 0, 19 s, 137 `capture-reused-after-census`. Summed `source_bytes_read` **0**, and no nonzero `read_*` counter. Max RSS 10,616,832 B. |
| sting `pull` | exit 0, 0 s (553.8 ms elapsed), 0 completed, 688 reused, `bytes_received` **0**, `source_bytes_read` **0**. 122,762 wire bytes received, control frames only. |
| sting `estate-apply` | exit 0, 2 s, 136 `previous-workspace-restoration-not-revalidated` and 1 `previous-ref-custody-not-workspace-parity`, `source_bytes_read` **0**. The landing probe was identical before and after the rerun. |
| `closure-report` | pass, pass; the same totals. |

## Verification

A script compared neo's pre-capture probe with the landing probe after the
rerun: **137 of 137 OK** on HEAD branch and commit, status count and
digest, untracked count, stash list (order and entries; 2 entries), and
`refs/heads` and `refs/stash` of all 14 main and bare repositories. No
`refs/heads/carry/*` anywhere, 0 prunable worktrees. 36,613 provenance
refs under `refs/carry/v1`, as expected.

Configuration differences were reported, not applied: `remote.origin.url`
is absent in the landing for one repository and its two worktrees (neo's
origin there is an SSH remote, not HTTPS) and for the refs-only bare (neo's
is an HTTPS GitHub URL). This is #216's inert-origins rule.

## What held at the source and at sting

- neo's GFTB repositories were read only (`GIT_OPTIONAL_LOCKS=0`, no
  fetch, gc or config). Neo's probe was identical before the first capture
  and after the rerun capture; HEAD, status, stash and `refs/stash` were
  unchanged during each capture.
- The live repositories under `<sting-git>` were untouched by the carry: a
  probe after the lane equalled the inventory probe (empty diff).
- No GitHub push. No process signalled. The only deletions were the lane's
  own empty scratch clone on neo.
- Disk headroom on sting's project quota stayed above the 25 % floor
  (31.6 % free before, 31.3 % after the first landing, 33.7 % at the end).

## Findings

- **A bare repository has no native landing into a fresh root.** The
  refs-only item received provenance refs only:
  `converge::apply_refs` restores native refs only where bulkload created
  the repository's ledger. Its 3 native refs were created by hand from
  the item's own provenance, create-only, in one `update-ref --stdin`
  transaction (exit 0, 0 removed or changed, 3 added; 2026-10-15T16:15:38Z).
  The receipt names this an agent finding: a bare item needs a hand step
  or a new verb.
- **The tool could not make the landing live.** Pointing the live sting
  paths at the ledgered landing took a hand-written reconcile script
  (`reconcile.receipt`: infra `swap`, 26 occupied names swapped aside,
  3 linked, 0 held, 165 destination refs created create-only,
  `verify_rc=0`; executed by the bulkload coordinator under OI-1003-Q163
  and Q167). Issue #226 asks for this natively (`estate-apply --adopt`).
- **What it does not show.** The rerun ran on an unchanged source, so it
  shows S3's unchanged clause, not S5's convergence under a moving source.
  No S2 observation ran alongside it. No SQLite store was in the plan.
