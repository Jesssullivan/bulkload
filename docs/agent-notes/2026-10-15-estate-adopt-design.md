# 2026-10-15 estate adopt: land into live paths natively (#226), design

Lane: estate-adopt-20261015. Worktree
`bulkload.worktrees/estate-adopt-20261015`, branch
`feat/estate-adopt-20261015`, off origin/main `a8d837a`, now at
origin/main `5beb031` (#228, Q168/Q169, merged). The branch has no
commits of its own; the staged work sits on `5beb031` and is adapted to
stash-order-v2's `StashEntry`.

**Status: built (PR 1 and PR 2 of section 10), staged and uncommitted
for the coordinator, who commits signed.** Sections 1 to 10 are the
design as written before any code; section 11 records the decisions this
lane took on the open questions, and section 12 what was built, how it
deviates from sections 1 to 10, and how it was validated. `--protect`
(section 4.3, PR 3) and `EstateProtect.tla` are not built.

Rulings this serves:

- OI-1003-Q171 and Q172 (Linear TIN-4543): bulkload is the interim
  flagship data mover, ahead of tcfs. This ruling is quoted from the
  workflow's task text; the relayed operator request in this lane was a
  go-ahead with no ruling text.
- OI-1003-Q163 and Q167: the GFTB resume.
- OI-1003-Q144 and Q149: convergence and its limits.
- OI-1003-Q145: `--adopt` is not built.
- OI-1003-Q129 and Q135 (#216): inert origins.
- OI-1003-Q1 (S4), OI-1003-Q6 and Q10 (S3), OI-1003-Q100 (no exchange
  means no replacement).
- R-N13.

**Spec of record:** the GFTB lane's
`/srv/fast-local/jess/bulkload/gftb-resume-20261015/{reconcile.sh,reconcile-plan.md,probe.sh,compare.py,inventory.tsv}`
and the carry and reconcile receipts in the same directory. Code
citations are to `a8d837a`.

## 1. What the hand script did, and what bulkload lacks

`reconcile.sh` (executed on 2026-10-15 at 17:06Z with infra=swap, run
`reconcile-run-20261015T170506Z`, verify_rc=0) did seven things:

- (a) classified 137 plan items plus sting's live rows into seven classes;
- (b) renamed 26 occupied live names aside to `<name>.sting-1015`;
- (c) ran `git worktree repair` on the aside families;
- (d) symlinked every live name into the ledgered landing;
- (e) imported 165 destination refs create-only under
  `sting-1015/*`, `sting-0930/*` and `neo-1015/*`;
- (f) re-added LA-Mesh's `yoga:git/LA-Mesh` origin;
- (g) verified V1 to V6, with closure as V6.

The carry needed one more hand step: the native refs of the preservation
bare.

The issue names six gaps (#226 items 1 to 6). This note answers each one.

## 2. The key question: adopt in place, or land and protect

The reconcile plan said the landing must not move, for two reasons. Read
against the code, only one of them binds:

- **Item identity binds.** `estate::id` is `blake3(postcard{source,
  repository, workspace})` (`estate.rs:686`). `estate-add` canonicalizes
  only the source; destination paths are kept as spelled. The workspace
  ledger records `item` and refuses `GIT_DESTINATION_OCCUPIED` when
  `ledger.item != landing.item` (`converge.rs:2032`, finding 15b). Journals
  are keyed by the same id (`journal_path`, `estate.rs:2612`). So a plan
  that names another path for the same tree, whether the tree was moved
  there or is reached through a symlink, is a different item and is
  refused for good. No verb re-keys a ledger.
- **Seat prints do not bind.** `prove` (`converge.rs:1099`) falls back to
  content whenever a print differs or is racy. It accepts a seat whose
  bytes hash to the landed blob and adopts its new print. A `cp -a` copy
  would prove, at the cost of one destination read. A same-filesystem
  `mv` keeps every file's inode and ctime; only the top directory's ctime
  moves, and directories compare by kind and mode. The reconcile plan's
  row "copy breaks every seat print" overstates this.

So the obstacle is the key, not the prints. That favours adopt:

| Criterion | (A) `--adopt` in place | (B) land into live paths and protect occupied ones by rename-aside |
|---|---|---|
| Trees moved | None. The live tree stays where every editor, shell, tmux pane and agent already has it open. | Every occupied family is renamed aside and freshly landed. GFTB: 26 names, about 2.1 GB of checkout writes, plus worktree repair on both sides. |
| Sting-only worktrees (51 rows in GFTB) | Stay attached to the same live main. Nothing to repair. | They follow the aside main, are re-pointed, and leave the live namespace. |
| Destination-only refs (165 imports in GFTB) | Stay native where they are: `native_refs::decide` R10 (untouched) and R3 (kept, source value at provenance). No import and no new names. | A create-only import `<tag>/*` into the new landing, one transaction per repository. |
| Work that differs from the capture | Refused, typed, with nothing written (§5). | Kept whole in the aside, and the source state lands live. |
| Byte cost | One content read of each adopted seat (`read_dest_verify_existing_bytes`), once. After that, prints. | A full first landing per family (GFTB: 864 s, 329 MB staged). |
| Crash surface | A read-only proof, then additive imports, then atomic ledger writes (§7). | Renames across a family, worktree admin rewrites on both sides, the landing, the ref import: four multi-step phases (§7). |
| What it needs that is new | The adopt proof, repository adoption, re-key, one refusal code. | All of (A)'s ref machinery, plus a family planner, a protect intent, worktree repair and the import. |

**Decision: (A), `estate-apply --adopt`, as the primary path.** Plan
destinations are the live paths, and an occupied destination becomes
bulkload's by proof, not by moving anything.

(B) is kept as an explicit per-run escalation, `--protect TAG`, only for
items that adopt refuses and that the operator wants to land as the
source has them. It is specified in §4.3 and proposed as a second PR.

Why (A) is enough for GFTB, measured against `inventory.tsv` (137 neo
rows and 51 sting-only rows):

| Inventory class | Count | Under `--adopt` |
|---|---|---|
| identical | 102 | Adopted by content proof (A11a). Ledgers are written in place. |
| neo-ahead (LA-Mesh) | 1 | Its checkout equals the capture. Only refs are ahead: three branches absent on sting are created (R1). Sting's origin stays in sting's config. |
| neo-only | 33 | Path absent, so a first landing as today (A5). The 25 infra nested worktrees land into the adopted infra main. |
| diverged (infra main) | 1 | Same HEAD (`feat/tin-2611-gf-v4-installation-inputs@8f2a6e1f3a7f`). Status differs, 38 entries on neo and 28 on sting, but per `reconcile-plan.md` §4 the difference is only which nested `.worktrees/*` exist. Those are other worktrees (nested custody or planned nests), not seats, and both sides have the same `p5-body.md` and `p6-body.md`. So it is expected to be adopted. This is an inference from the plan's dirt analysis, not yet proved: the acceptance run in §9 confirms it. Refs are per row: sting's `work/tin-5770...` R10 (stays native); sting's newer remote-tracking refs R3 (kept, neo's value at provenance); neo's heads sting lacks R1 (created). |
| sting-ahead | 0 | Not applicable. |
| sting-only | 51 | Not planned, so never touched. They stay attached to the same live mains, including the 22 orphans of the `*.sting-0930` copies. |
| gone-both | 23 | Nothing to do. |

Adopt removes the need for (c), (d), (e) and (f) in the hand script. The
estate-diff verb (§3) does (a), closure does (g), and the bare landing
(§4.4) covers the preservation hand step.

**Production note: the 2026-10-15 GFTB state now.** The live names are
symlinks into `estate/gftb-20261015`, and that landing is fully ledgered
under the estate-path plan, which converges today. A plan that spells the
live symlink paths is a different item (see above) and must not be used
against it. Two ways to unwind it (open question 1):

- Keep the estate-path plan.
- Remove each symlink, `mv` the estate tree back to its live name (same
  filesystem), `git worktree repair`, and `--adopt` under a live-path
  plan. The re-key rule (A11b) then proves each item against its existing
  ledger by print and rewrites only the item key.

## 3. `estate-diff` (#226 item 1)

```
estate-diff [--since NS] [--scan DIR]... PLAN CORPUS SOURCE STATE
```

It runs on the destination host. It is read-only at the destination and
in STATE; it writes only a private scratch stage under STATE, which it
removes. Output is one JSON line per row, schema
`bulkload.estate-diff.v1`. It is the report mode of the same functions
`--adopt` runs (`adopt::prove`, `native_refs::plan`), so the diff and the
apply cannot disagree.

**Reads:**

- At the destination: a metadata census, plus a content hash of each seat
  whose print does not already match a ledger. It runs with
  `GIT_OPTIONAL_LOCKS=0` and never refreshes the index.
- The capture side: each item's current capture, from CORPUS, staged into
  the scratch repository. That repository borrows the destination's
  objects through `alternates`, read-only, so ancestry questions can be
  answered.
- It never reads the source. S2 is not involved.

**Rows:**

- One row per plan item.
- One row per destination-only worktree: a worktree registered in a
  planned repository's admin directory that no item plans.
- With `--scan DIR`: one row per top-level git directory under DIR that is
  in no planned family. This covers orphans such as the `*.sting-0930`
  attachments.
- Per-ref rows inside each repository row.

**Item classes.** The baseline is, in order:

1. the item's workspace ledger, which gives an exact three-way answer;
2. otherwise `--since NS`, a landing instant, which flags destination work
   since then;
3. otherwise no baseline, which gives a two-way answer.

| Class | Rule with a ledger (L) | Rule without one |
|---|---|---|
| `identical` | The destination proves equal to C: seats, HEAD, index entries, stash, exclude. Refs are reported per row. | Same. |
| `source-only` | The destination path is absent. | Same. |
| `dest-only` | A registered worktree, or a scanned repository, that is not planned. | Same. |
| `source-ahead` | The destination proves against L, and C ≠ L.capture. A converge (A10) will apply it. | Destination clean, and its HEAD an ancestor of C's HEAD. Reported as `differs, hint=source-ahead`, never claimed exact. |
| `dest-ahead` | The proof against L fails, and C = L.capture. | Source clean, and its HEAD an ancestor of the destination's HEAD. Reported as `differs, hint=dest-ahead`. |
| `diverged` | The proof against L fails, and C ≠ L.capture. | Anything else that is not identical: `differs, hint=diverged`. |

**Per-ref classes** are always exact, by ancestry in the scratch
repository: `same`, `source-only`, `dest-only`, `source-ahead` (the
destination tip is an ancestor of the carried one), `dest-ahead`,
`diverged`. Each row also prints the `native_refs::decide` row that
`--adopt` would take (R1, R2, R3 or R10).

**Destination work since the last landing** is printed as evidence, never
as a class:

- with L: the components whose proof fails (seats with paths and counts,
  index, HEAD, the HEAD branch, stash T6, refs R8 and R9);
- with `--since`: HEAD reflog entries, any ref's reflog entries, an index
  mtime and seat mtime or ctime after NS (compare.py's `SWAP` test).

**Each row also carries:**

- `adopt`: one of `adoptable`, `diverged` (with the failing components),
  `occupied-foreign` (an unrelated repository, §4.1), `ledgered` or
  `ledgered-other-item`;
- `rebuildable`: the capture's omitted roots (§4.5);
- `origin`: what the first landing would activate (§4.6).

`compare.py`'s fields map one to one: HEAD ref and sha, status count and
digest, stash count and digest, and heads digest excluding bulkload's
namespaces.

## 4. Changes to `estate-apply`

### 4.1 Decision table additions

These rows extend the converge table (design note of 2026-10-08, §6 as
amended). They are tried after A10. A12 stays the default.

| # | Condition | Action | Outcome |
|---|---|---|---|
| A11a | D present, no workspace ledger, `--adopt`, and the repository adoptable (below) | Adopt proof against C (§5). Pass: adopt the repository's refs, import C, write `workspace.landed = C`, then the journal. Fail: nothing is written. | `workspace-restored`, reason `adopted seats=n refs-adopted=n refs-created=n refs-kept-destination=n refs-untouched=n`. On failure, **`GIT_DESTINATION_DIVERGED`** (new) with the failing components in the reason. |
| A11b | D present, a workspace ledger for **another** item id with this SOURCE label, `--adopt` | Prove against that ledger (the normal `prove`), then rewrite `ledger.item` atomically and continue at A6 or A10. | As A6 or A10, custody `rekeyed=1`. `GIT_DESTINATION_MODIFIED` if the proof fails. |
| A11c | Refs-only item, D absent, capture bare (`bare-repository-v1`) | Bare first landing (§4.4). | `refs-imported` |
| A11d | Refs-only item, D an existing repository with no ledger and **zero refs**, `--adopt` | Adopt the empty repository (the marker), then the native step. | `refs-imported` |
| A11e | D present, no ledger, adopt refused or not requested, `--protect TAG` | Protect the family (§4.3), then A5. | `workspace-restored`, custody `protected-aside=<path>` |
| A12 | D present, no ledger | Nothing, as today. | `GIT_DESTINATION_OCCUPIED` |

**Repository adoption.** This runs once per repository, under the
destination lock, before its items' workspaces.

- **Precondition: the repository is related.** It holds the root
  commit(s) of C's HEAD, or it has no refs at all. An unrelated
  repository at the path stays `GIT_DESTINATION_OCCUPIED`, and the row
  prints `occupied-foreign`. This is what keeps R1 from writing refs into
  someone else's repository.
- **Ref ledger.** Write `refs.landed` with the marker set and one row per
  native ref whose value equals the carried one (R2). Then the normal
  native step runs:
  - R1 creates carried refs that are absent;
  - R3 keeps conflicting values;
  - R10 leaves destination-only refs alone;
  - the guard row skips any checked-out branch.
- **Stash** gets one new row, T0: a destination reflog equal to the
  carried order is adopted. T2 keeps any other.
- **Encoding.** The marker stays the existing `created` bool, because
  `RefLedger` is strict postcard and a new field would break decoding of
  existing ledgers. An `adopted` sidecar record (own magic: instant, run,
  counts) says how the marker was set, for receipts and estate-diff. The
  marker's meaning widens from "bulkload created this repository" to
  "bulkload owns native refs here, by creation or by adoption"
  (open question 3).

### 4.2 What `--adopt` never does

- It writes no seat, index, HEAD or configuration.
- It deletes no ref.
- It never adopts a ref whose value differs from the carried one, and
  never adopts a stash whose order differs.
- It never adopts implicitly: the flag is per run and recorded in each
  receipt. A run without it keeps A12, as OI-1003-Q145 and design §17 Q8
  ("explicit, never automatic") require.

### 4.3 `--protect TAG` (escalation, second PR)

This is for items that adopt refuses (`GIT_DESTINATION_DIVERGED`, or a
first-landing collision) and that the operator wants live in the
source's state.

**The family.** The unit is the destination family: the main, every
worktree registered in its admin directory, and every top-level name
those occupy. It is computed as `reconcile.sh`'s `compute_plan` computes
it, with its refusals:

- a family mismatch;
- a worktree that would move without its main;
- a backlink from another main into a moving name;
- `alternates` into a moving name;
- `<name>.TAG` already existing.

**Steps:**

1. Write a protect intent, durable, in STATE (`{family}.protect`). It
   holds the name map, the old-to-new worktree map, and each member's
   pre-fingerprint (HEAD, index entries digest, stash, heads digest).
2. Rename each top-level name to `<name>.TAG` with
   `renameat2(RENAME_NOREPLACE)`, then sync the directory.
3. Run `git worktree repair` from the aside main with the new paths, then
   check that every worktree's `.git` points into the aside main.
4. First landing (A5) at the now-absent live path.
5. Import destination-only refs from the aside: create-only under
   `refs/heads/TAG/*` and `refs/tags/TAG/*`, one `update-ref --stdin`
   transaction per repository. Remote-tracking and bulkload namespaces are
   skipped, as in `reconcile.sh`'s `import_refs`. A target that exists at
   another value is reported `ref-import-conflict` and never overwritten.
6. Add remotes the aside defines and the landing lacks, only under §4.6's
   flag.
7. Record the receipt, then remove the intent.

There is no symlink mode. Diverged rows are never overwritten: the aside
holds them whole, and closure prints the item with custody
`protected-aside`.

### 4.4 Bare repository into a fresh root (#226 item 4)

**A11c.** For a refs-only item whose path is absent and whose capture is
bare, bulkload makes the repository itself:

- it builds in a private `.bulkload-restore-*` stage:
  `git init --bare` with the capture's object format, the native refs
  (all R1), the symbolic HEAD from the capture, and the ref ledger with
  the marker;
- it publishes with the existing no-replace rename (`publish_restored`,
  with R-N119's fallback).

**Configuration.** The bare capture's configuration takes the standalone
rule (#216): an https origin is activated, any other is preserved-only.
That is open question 6.

**A11d** covers the GFTB shape, an empty repository someone pre-made.
Today such a refs-only import into a missing path refuses
`GIT_REPOSITORY_NOT_AT_PATH` (S4, #183). That stays for a non-bare
capture, whose repository belongs to its standalone item.

### 4.5 Counted rebuildable omissions (#226 item 5)

Capture already measures what it omits: `RebuildableOmission {rel_path,
bytes, entries}` in metadata `rebuildable-omissions-v1`
(`git_carry.rs:2937`). Closure does not read bundles, so:

- **Capture** publishes a `{bundle}.omissions` sidecar, postcard after a
  magic, before the capture record, like `.parts` and `.prior`. A reuse
  hit keeps the old sidecar.
- **Closure** adds one row per omitted root, `kind:
  rebuildable-omission`, disposition `omitted-rebuildable`, accounted and
  outside `planned`, with `item`, `path`, `entries` and `bytes`. Item rows
  get `omitted: {roots, entries, bytes}`. Totals get
  `omitted_rebuildable_roots`, `_entries` and `_bytes`.
- **A capture with no sidecar** (every capture before this change) prints
  `omitted: unknown` and no rows. It is never counted as zero.
- **Schema** goes to `bulkload.closure.v4`, and its v3-reader delta is
  listed in design.md as before.

So "equals the source" reads exactly: equal except N counted rebuildable
roots (GFTB: about 840k entries). The adopt proof and converge already
skip these roots on both sides. The destination's own `node_modules` is
never touched or claimed.

### 4.6 Origins (#226 item 6, #216)

The default is unchanged: an origin that is not https is preserved-only
and inert.

**New:** `estate-apply --activate-origin PREFIX`, repeatable. A
standalone first landing (A5, and A11c) activates the origin, and
`branch.*.remote` and `pushremote` naming it, only when every
`remote.origin.url` value:

- is scp-style (`host:path`, `user@host:path`) or `ssh://`; and
- starts with one of the given prefixes, for example `yoga:git/` or
  `git@github.com:Jesssullivan/`.

A local path, absolute or relative, is never activated. The prefix list is
recorded in `configuration-activation.postcard` and in the receipt reason
(`origin-activated-by-prefix=<prefix>`). It is never inferred from a host,
an ssh config or a username.

**Adopted repositories keep their own configuration** (LA-Mesh keeps
`yoga:git/LA-Mesh`), and the flag has no effect on them. Configuration is
report-only after the first landing (Q6 of the converge note).

## 5. The adopt proof

It is read-only, runs under the destination lock, and runs before any
destination write. Objects come from the scratch stage, as in §3.

1. **HEAD.** Destination HEAD equals C's HEAD: the same symbolic branch
   at the same commit, or detached at the same commit. A branch HEAD also
   needs that branch's ref row to be R2 (equal), so the HEAD branch is
   owned.
2. **Index.** The index entries digest equals the digest of C's staged
   tree plus the carried flags (`index_digest`). Stat fields and
   extensions are excluded, as in §3.4 of the converge note.
3. **Exclude.** A standalone item's `info/exclude` equals the carried one
   by effective rules (#41, OI-1003-Q8: normalised comments, order and
   duplicates). A linked item does not check it, as in finding 12.
4. **Seats.** Every carried seat of C's worktree commit W (tracked,
   untracked and ignored) plus its `filesystem-v1` mode must exist with
   equal kind and mode, and its bytes must hash to W's blob (a symlink's
   target). The print is taken before and after the read and must be
   unchanged, as in `prove`'s settled check. Extra destination seats are
   left alone and reported (finding 18). Excluded: rebuildable roots,
   planned nests (`planned_nests`), `.git`.
5. **Stash.** The stash is not part of the workspace proof. It is decided
   by T0, T1 or T2 at repository level.

**Result.**

- **Pass:** the ledger records the prints taken at proof time, with
  `recorded_ns` set when the prints were taken. Racy seats are then
  re-verified by content at the next converge, as for a landing.
- **Fail:** `GIT_DESTINATION_DIVERGED`, with the reason naming the
  components (`head`, `index`, `exclude`, `seats=n` and the first paths).
  Nothing is imported or written at the destination.

**Why a new code, and not `GIT_DESTINATION_MODIFIED`.** MODIFIED means
"bulkload landed this, and it was changed since". A destination bulkload
never landed was never modified. It differs.

DIVERGED is typed. Closure shows it as `refused-pending-review`, and the
dispositions read:

- accept: keep the destination and do not converge this item;
- re-carry: rerun with `--protect`;
- abandon.

A new capture is a new instance, so it is pending again unless a standing
policy disposes it.

## 6. Safety properties

- **No clobber.** Adopt writes no worktree byte, index, HEAD or config.
  R1 creates are CAS-create lines. R2 rows record values equal to the
  carried ones. Destination-only and conflicting refs get no ledger row,
  so no later converge ever moves them: R10 and R3 hold forever, and R8
  cannot arise for them.
- **Claims only what equals the source.** A ledger seat print is
  recorded only for a seat whose content was proved equal to C under a
  print that held still while it was read. A later edit moves the print.
  The next converge then falls back to content, finds a mismatch and
  refuses MODIFIED. Q144's content semantics are unchanged.
- **Source.** No source read (S2, S3). The adopt proof reads destination
  bytes once, counted in `read_dest_verify_existing_bytes`. An unchanged
  rerun is A4: a journal no-op that reads zero bytes.
- **Accounting (S4).** Every adopt and protect outcome is a journal plus a
  typed refusal. An item is never left half-adopted without a row:
  without its journal it is `journal-missing`.

## 7. Crash safety (every step old or new, a rerun finishes)

**Adopt (A11a and A11d)**, per item:

| Step | Writes | State if a crash lands after it | Rerun |
|---|---|---|---|
| 1. Proof | None (scratch stage only) | Destination unchanged | Proves again |
| 2. Import of C (`import_verified_instance`) | Objects and `refs/carry/v1/<SOURCE>/<instance>/` | Additive, idempotent | Imports again (no-op) |
| 3. Repository adoption: `adopted` sidecar, then `refs.landed` with the marker | Atomic writes | Marker absent, or present with R2 rows | Marker absent: adopts again. Present: the native step settles from the ledger. |
| 4. Native ref step (R1 creates, stash T1) | Existing `refs.intent`, then CAS transaction, then ledger (`native_refs::step`) | Old or new per ref; the intent names both | Existing settle (`settle_repository`) |
| 5. `workspace.landed = C` | One atomic write | No ledger, or ledger | No ledger: A11a proves again (the destination is unchanged by adopt). Ledger: A6 proves it and writes the journal. |
| 6. Journal and outcome | Existing `emit` order | Journal-missing until written | A6 |

**Fault points.** Adopt needs `estate.adopt.after_import`,
`after_marker`, `after_refs` and `after_ledger`. The existing ref-step
points cover step 4.

A crash in steps 2 to 4 followed by a rerun **without** `--adopt` meets A12
(`GIT_DESTINATION_OCCUPIED`, typed). The marker or ref rows may already
exist, but no workspace ledger does. The receipt says to rerun with
`--adopt`. That is safe because nothing in the checkout changed.

**Re-key (A11b)** is a single atomic ledger rewrite after a full proof.
Old key or new key; rerun repeats it.

**Bare landing (A11c)** is staged and published by one no-replace
rename. A crash before it leaves only the stage, which the next run
removes when it carries bulkload's marker. A crash after it leaves a
complete repository with its ledger, and the rerun journals it (A6
analogue for refs-only).

**Protect (A11e).** The intent in STATE is written before the first
rename and names, for each member, its old path, its aside path and its
pre-fingerprint. The settle runs per name:

| Destination state on rerun | Meaning | Action |
|---|---|---|
| old present, aside absent | Rename not done | Rename (NOREPLACE) |
| old absent, aside present | Rename done | Go on |
| old present (bulkload's new landing, with a ledger), aside present | Landing done | Go on |
| old present without a ledger, aside present | A third party recreated the path | Refuse `GIT_CONVERGE_INTERRUPTED` and keep the intent |
| both absent | Impossible without a third party | Refuse, and keep the intent |

Worktree repair is idempotent. A rerun recomputes from the intent's map
and checks every backlink before the landing.

The ref import is one transaction per repository of `create` lines. The
rerun counts refs already at the recorded value as done and refs at
another value as `ref-import-conflict`. The intent is removed last. Until
then closure counts the item unaccounted (`journal-missing`), so S4 never
passes over a half-protected family.

A family rename is not one atomic step across several top-level names.
The guarantee is per name, old or new, with the intent naming both. The
fingerprint check (V3 of `reconcile.sh`) runs at the end of every
protect.

## 8. TLA+ additions

### EstateConverge

`Adopt` is a new constant. The catalogue renders it FALSE in every
existing row, so their state spaces are unchanged.

**State.**

- `Init` with `Adopt` and `~Landed` starts from a **pre-existing
  destination**: `dhead`, `dseat`, `dindex`, `nat` and `dstash` are set
  from a new constant `PreDest`. Each component is either equal to the
  first capture's value or an operator value.
- Operator values are entered in `opLive`, `opHead` and `opStash` from
  the start, so `NoLocalWorkLost` covers them unchanged.

**Actions.**

- `AdoptRepo`: the marker, R2 rows, R1, T0, T1 and T2.
- `AdoptProve(i)`: by content against `caps[capt[i]]`. It ends either in
  `WriteLanded` (the adopt branch) or in `Refuse(i, "DIVERGED")`.
- A crash may land between any two of them.
- `Refusals` gains `"DIVERGED"`. `TypedRefusal`'s cause for it is "some
  operator value present in a proved component".

**Invariants.**

- `NoLocalWorkLost` and `CrashAtomicity` hold as they are.
- New `AdoptClaimsOnlyEqual`: a ghost `adoptedAt` records the seat, index
  and HEAD values at adoption. They equal C's, and no `opLive` component
  is in the ledger.
- New `ForeignRefsUnowned`: a ref whose value at adoption differed from
  the carried one, or that was absent from the capture, never gets a
  ledger row.
- Liveness: `ConvergesWhenQuiet` with `PreDest` equal to the source.

**Mutations,** each expected to fail its named property:

| Mutation | Fails |
|---|---|
| `adopt_without_proof` (ledger from the destination as found) | `NoLocalWorkLost`, at the next converge |
| `adopt_ref_unequal` (R2 for a differing value) | `NoLocalWorkLost` |
| `adopt_r1_over_existing` (create without CAS) | `NoLocalWorkLost` |
| `adopt_untyped` | `TypedRefusal` |
| `adopt_ledger_before_refs` (workspace ledger before the ref step settles) | `CrashAtomicity` |

**Rows.**

- `MC_ec_adopt`
- `MC_ec_adopt_crash`
- `MC_ec_adopt_op` (OpConcurrent)
- `MC_ec_reach_adopted_converged`
- `MC_ec_neg_adopt_*`

The `ec_` prefix keeps them apart from BulkloadTransfer's
`MC_neg_adopt_*`, which belongs to the unrowed adoption of WP0(g).

The re-key is a relabelling that runs after `Prove` and adds no
behaviour. It is covered by a code test and stated in the README's
abstraction list.

### EstateProtect.tla (new, small, second PR)

**Variables:**

- `names`: each name maps to {old, aside}, with a value and its version;
- `links`: worktree backlinks per member;
- `intent`;
- `landed`;
- `imported` refs;
- the environment: crash and operator writes.

**Invariants:**

- `ProtectedIntact`: every pre-existing member's value is at its old path
  or its aside path, never lost or duplicated, and never overwritten.
- `LinksConsistent`: when the intent is cleared, every backlink points
  into the aside main.
- `ImportCreateOnly`: an imported ref never replaces a value.

**Mutations:** `rename_replace` (a rename without NOREPLACE),
`land_before_repair`, `import_overwrite`, `intent_after_rename`.

**Not modelled:**

- `estate-diff`: it is read-only, and covered by a P34-style property
  (below).
- The bare first landing: one atomic publish, as `FirstLandMain` already
  abstracts it.
- Origin activation: configuration, which the model does not cover.

`just tla-check` must pass, with the catalogue's sets matching the
labels.

## 9. Test plan

The fixtures are the GFTB inventory's shapes, scaled down. One generator,
`tests/fixtures/gftb_shapes.rs`, builds a source estate and an occupied
destination from a table whose rows are `inventory.tsv`'s classes:

| # | Fixture (GFTB row it stands for) | Expectation |
|---|---|---|
| F1 | identical main with two linked worktrees, one nested under `.worktrees/`; the destination a ledgerless `cp -a` copy (the 10-08 swap) | estate-diff says `identical`, `adoptable`. `--adopt` writes 0 worktree bytes (P34-style: every `lstat` field of every seat fixed, `.git/bulkload/` aside), writes the ledgers and closes `applied`. The rerun is A4 with 0 bytes read. A later source commit converges (A10) and equals a fresh restore. |
| F2 | LA-Mesh: the destination has three fewer branches and origin `yoga:git/LA-Mesh`; the source's origin is the same scp URL | `adoptable`. Three R1 creates. The destination origin is unchanged. Without `--activate-origin`, a fresh landing of the same capture leaves no origin; with `--activate-origin yoga:git/` it is active. |
| F3 | Infra: the destination main has a destination-only branch checked out in a destination-only nested worktree, newer remote-tracking refs and two destination-only worktrees; the source has 41 heads absent at the destination and 25 neo-only nested worktrees | The main is adopted. `work/tin-5770` R10 and remote-tracking R3 are unchanged. R1 creates 41. Every neo-only worktree lands. One whose branch is checked out at the destination lands detached with `intended_branch`. Destination-only worktrees: `git worktree list`, their HEADs, indexes and status are byte-identical before and after. `fsck` is clean. |
| F4 | Content diverged: a destination dirty seat, a staged hunk, a switched HEAD (three rows) | `GIT_DESTINATION_DIVERGED` naming the component. Nothing at the destination changes (P34-style over the worktree **and** `.git`). Closure shows `refused-pending-review`. |
| F5 | F4 with `--protect sting-1015` (second PR) | The family is aside whole, with backlinks repaired both sides. Destination-only refs are under `sting-1015/*`. The live path is freshly landed. Pre-fingerprints are equal at the aside paths. Closure passes. |
| F6 | lanea2-299: a worktree the destination removed on purpose (merged and clean) while the source still has it | estate-diff with `--since` reports `source-only` plus `dest-removed-after-since` evidence. `--adopt` lands it again (stated behaviour, open question 4). |
| F7 | The preservation bare: a refs-only bare item to an absent path (A11c), and to a pre-made empty bare with `--adopt` (A11d) | Native refs and symbolic HEAD, the marker set, `refs-imported`, closure `referenced-only`, no hand step. Without `--adopt`, an empty pre-made bare keeps today's provenance-only import. |
| F8 | `node_modules`, `.direnv`, `__pycache__` and `.terraform` in the source; the destination has its own `node_modules` | Closure rows `rebuildable-omission` with entries and bytes. Totals are exact. The destination `node_modules` is untouched and unclaimed. A legacy capture prints `omitted: unknown`. |
| F9 | Origins: https, scp same-owner, scp other host, `ssh://`, local absolute, local relative | The #216 inert rule by default. With `--activate-origin`, only matching scp or ssh URLs. Local paths never. The receipt and `configuration-activation` name the prefix. |
| F10 | Orphans attached to a `*.sting-0930` copy | Never planned and never touched. `estate-diff --scan` lists them `dest-only` with their foreign common directory. |
| F11 | An unrelated repository at a planned path (a different root commit) | `occupied-foreign`. `--adopt` refuses `GIT_DESTINATION_OCCUPIED` and writes no ref. |
| F12 | Re-key: F1's ledgered landing moved by `mv` (same filesystem) plus `worktree repair`, then a live-path plan with `--adopt` | A11b, proved by print with no content read (counter 0). `rekeyed`. Closure `applied`. A rerun is a no-op. |
| F13 | Crash rows | `scenarios!` rows for `estate.adopt.*` and, later, `estate.protect.*`. Each checks old-or-new at the crash, `journal-missing` before the rerun, the rerun finishing, and a second rerun being a no-op. Operator-write rows during adopt: a seat edit between the proof and the ledger write refuses MODIFIED at the next converge, and the edit is kept. |
| F14 | estate-diff is read-only | P34-style `lstat` freeze over the destination (worktrees, `.git`, the index) and STATE across estate-diff. Its classes agree with the A11 row taken by `--adopt` on the same inputs (property over F1 to F11). |

**Acceptance on sting,** run by hand as the operator's check rather than
in CI, and read-only. The `<name>.sting-1015` asides are exactly the
pre-reconcile live tree, repaired in place. Run estate-diff with a plan
whose destinations are those aside paths against the 10-15 corpus. It
must reproduce `inventory.tsv`'s classes for the 137 rows, with the
51 sting-only rows as `dest-only` under `--scan`. Every planned row that
exists on both sides is expected to be `adoptable` (§2), so the expected
count of `diverged` rows is 0.

**Gates:** `just check-fast`, `just fault-harness`, `just tla-check`,
then `just props-deep` and `just crash-sweep` once. Property numbers are
assigned at build time against the other open lanes (#228).

## 10. Proposed PR split

1. `estate-diff`, `--adopt` (A11a, A11b, A11d), `GIT_DESTINATION_DIVERGED`,
   and the EstateConverge additions. This is what makes GFTB and GF
   bulkload-only.
2. The bare first landing (A11c), the omission rows and closure v4, and
   `--activate-origin`.
3. `--protect`, with EstateProtect.tla.

## Open questions (for the operator)

1. **The live GFTB tree now.** Keep the estate-path plan (it converges
   as it is), or unwind the symlinks by `mv` back plus re-key adopt
   (A11b)?
2. **Adopt instead of the literal acceptance.** #226's acceptance says
   "the aside copies, the imported refs". Adopt produces neither, because
   nothing is moved: destination refs stay native in place. Is adopt with
   no asides acceptable as meeting #226, with `--protect` only on demand?
3. **The marker's meaning.** May an adopted repository take native refs:
   R1 creates absent carried refs, and owned refs follow the source? This
   widens finding 9's "only a repository bulkload created". Adopt is
   explicit and per run.
4. **R1 against destination deletions.** Without a ledger, a branch or
   worktree the destination deleted on purpose (sting's 10-10 cleanup)
   looks like a source addition, and adopt recreates it, as the reconcile
   swap did. Options:
   - accept that;
   - make adopt's R1 opt-in (`--adopt-create-refs`);
   - require a `--since` baseline that suppresses creates for names whose
     reflog shows a destination deletion. Git keeps no reflog for a
     deleted ref, so this works only where `logs/refs` survives.
5. **A content baseline for source-ahead content.** Adopt proves against
   the current capture only. A destination equal to an older source state
   (dirty bytes the source has since committed) refuses DIVERGED. Should
   adopt also accept a proof against a baseline capture (`--baseline PLAN0
   CORPUS0 SOURCE0`, matched by `item.source`) and then converge it
   (A10)? GFTB did not need this (§2). It would be its own PR.
6. **The bare landing's configuration.** For a bare first landing,
   activate an https origin as the standalone rule does, or keep bare
   configuration report-only?
7. **Where `--activate-origin` lives.** As an apply flag recorded in the
   receipt (proposed), or in the plan, where it is reviewed and bound by
   the disposition ledger's plan digest?
8. **A standing policy for DIVERGED.** Should `accept` be offered as a
   standing policy, so a deliberately divergent destination item does
   not go pending again on every new capture?

## 11. Decisions on the open questions (this lane, 2026-10-15)

The relayed operator request was a go-ahead ("perfect! great lanes. we
got this."), with no answer to the eight questions. The workflow task
said to implement and to record the decisions made here in this note.
Each one below is the conservative reading. Each is reversible without
a data change, and each is open for the operator to overrule. None is
an operator ruling: R-N13 says a question is never a ruling.

1. **The live GFTB tree.** This lane changes nothing in production. The
   estate-path plan converges as it is. The unwind (remove the symlinks,
   `mv` each estate tree back to its live name on the same file system,
   `git worktree repair`, then `estate-apply --adopt` under a live-path
   plan) is built and tested (A11b, P93). Choosing it, and when, is the
   operator's call.
2. **Adopt instead of the literal acceptance.** #226 is met by adopt
   without asides. Nothing is moved, and the destination's own refs and
   worktrees stay native where they are. No `<tag>/*` import is made,
   because no ref leaves its name. `--protect` stays the escalation for
   items adopt refuses. It is not built in this lane (PR 3).
3. **The marker's meaning.** It widens: an adopted repository carries
   the marker and takes native refs (R1 creates, R2 adopts, owned refs
   follow the source afterwards). This happens only under an explicit
   per-run `--adopt`. An `adopted` record beside `refs.landed` says how
   the marker was set (item, capture, instant, native and equal ref
   counts).
4. **R1 against destination deletions.** Accepted. Adopt recreates a
   carried ref the destination lacks, as the reconcile swap did.
   `estate-diff` shows each one first (`"class":"source-only",
   "row":"ref-created"`), so the operator reviews the diff before the
   apply. Neither `--adopt-create-refs` nor a `--since` suppression is
   built.
5. **A content baseline for older captures.** Not built. A destination
   equal to an older source state refuses `GIT_DESTINATION_DIVERGED`. The
   model states this as `AdoptBehind`, the exception of the two liveness
   properties.
6. **A bare landing's configuration.** The standalone rule applies: an
   HTTPS origin is activated, and so is an origin an `--activate-origin`
   prefix names. Any other origin is preserved-only (#216).
7. **Where `--activate-origin` lives.** It is a run flag. A prefix must
   be scp-style or `ssh://`, so a local path is refused
   (`FIELD_DOMAIN_VIOLATION`). The flag is recorded in the receipt
   (`origin-activated-by-prefix=1`) and in a receipt file beside
   `configuration-activation.postcard`
   (`carry-config/origin-activated-by-prefix`, the prefix). That record's
   codec is unchanged.
8. **A standing policy for DIVERGED.** Nothing new is built. The
   existing standing policy covers it: `closure-dispose LEDGER PLAN
   CORPUS SOURCE --policy GIT_DESTINATION_DIVERGED accept ...` disposes
   every such refusal under the plan, on every capture. Without it, each
   new capture's refusal is pending review again.

## 12. What was built (2026-10-15)

Rulings cited: OI-1003-Q171 and Q172 (from the workflow's task text; see
the header), OI-1003-Q163 and Q167, OI-1003-Q144 and Q149, OI-1003-Q145
(superseded in part: `--adopt` is now built, explicit and per run),
OI-1003-Q129 and Q135, OI-1003-Q1, Q6 and Q10, OI-1003-Q100, R-N98 (fixture
repositories only disable hooks), R-N11 (crash children end themselves),
R-N13.

### Code

| File | What |
|---|---|
| `crates/bulkload-proto/src/refusal.rs` | `GIT_DESTINATION_DIVERGED`. |
| `crates/bulkload-agent/src/git_carry/converge/adopt.rs` (new) | A11a `workspace` (scratch, relatedness, the read-only proof before any lock or write, a metadata re-check under the lock, import, the `adopted` record and marker with the HEAD branch's R2 row, the ref step, the ledger), A11b `rekey`, A11d `repository`, `Scratch` (a non-bare scratch repository in the staged bundle's private directory, borrowing the destination's objects through `alternates`), `prove_adoptable`, `Found`. |
| `crates/bulkload-agent/src/git_carry/converge/diff.rs` (new) | `estate-diff`'s per-item report: `workspace`, `refs_only`, per-ref rows by ancestry and by the `decide` row, `--since` evidence from reflog files and the index's mtime, the first landing's origin. |
| `crates/bulkload-agent/src/git_carry/converge.rs` | `Policy` (`adopt`, `origins`); `apply_workspace` dispatches A11a and A11b; `apply_refs` handles A11c (bare and absent) and A11d; `Applied::first`, `Applied::adopted`. |
| `crates/bulkload-agent/src/git_carry.rs` | `land_bare_staged` (A11c), `carried_symbolic`, `settle_bare_head`, `related_repository`, `origin_prefix_allowed` and the prefix rule in `plan_standalone_configuration` (`ORIGIN_PREFIX_RECEIPT`), one `init_repository` and one `set_symbolic_head` site (P76: the writer ceilings stay), `StagedBundle::is_bare`, `private_dir`. |
| `crates/bulkload-agent/src/git_carry/landed.rs` | The `Adopted` record (`adopted`). |
| `crates/bulkload-agent/src/git_carry/native_refs.rs` | Stash row T0 (`stash-adopted`). |
| `crates/bulkload-agent/src/estate.rs`, `estate/diff.rs` (new) | `ApplyOptions`, `apply_with`; the `{bundle}.omissions` sidecar at capture and `LedgerEntry::omitted`; the plan base staged for a bare first landing; `estate::diff` with dest-only worktree rows and `--scan`. |
| `crates/bulkload-agent/src/closure.rs` | `bulkload.closure.v4`: the `omissions` block. |
| `crates/bulkload-agent/src/main.rs` | `estate-apply [--adopt] [--activate-origin PREFIX]...`, `estate-diff`. |
| `crates/bulkload-agent/src/fault.rs` | `estate.adopt.after_import`, `after_marker`, `after_refs`, `after_ledger`, `bare_staged`. |

### Deviations from sections 1 to 10

- **The proof runs before the lock.** The adopt proof reads the
  destination without taking the destination lock, so a refused item
  leaves no `bulkload/` directory and no lock file. Under the lock, the
  proved prints, HEAD, index and exclude are checked again, metadata
  only, before the first write. If anything moved in between, the item
  refuses `GIT_DESTINATION_DIVERGED`.
- **DIVERGED's reason is its code.** A refusal is a value without a
  payload, so the receipt and the outcome record carry
  `GIT_DESTINATION_DIVERGED` only. `estate-diff` names the components
  (`head`, `index`, `exclude`, `seats=<n>` and up to three `seat=<path>`).
- **A11d covers related repositories, not only empty ones.** With
  `--adopt`, a refs-only item's repository is adopted when it holds a
  carried tip, holds a root commit of one on a native ref, or has no
  native ref at all. An unrelated repository keeps the provenance import,
  as without the flag, and refuses `GIT_DESTINATION_OCCUPIED` (section
  13, finding 7). An unborn HEAD of a bare repository is put on the
  captured branch.
- **Omissions are a block, not rows.** Closure v4 adds one top-level
  `omissions` block (items known and unknown, roots, entries, bytes, one
  row per root, the unknown items). It does not add rows of a new
  disposition, so the rows, totals, dispositions and gate are exactly
  v3's. A capture taken before this change has no sidecar and is listed
  `unknown`, never counted as zero.
- **The origin prefix is a count in the reason.** The prefix itself is
  in the receipt file, not in the reason, because custody words are
  counts.
- **Re-key is a relabel of the same tree** (section 13, findings 1, 2
  and 8): A11b requires `--adopt`, a canonically spelled checkout, a
  ledger item that is not one of this plan's, a capture that is the
  ledger's or whose HEAD descends from the ledger's HEAD, and then the
  ledger's own proof.
- **A crashed bare landing leaves its stage.** As a standalone restore
  does, a killed run leaves a `.bulkload-restore-*` stage beside the
  destination. Nothing sweeps it, the destination does not exist, and
  the rerun lands.
- **Not built:** `--protect` (A11e) and `EstateProtect.tla` (PR 3);
  `estate-diff`'s per-seat `--since` evidence (only reflog entries and
  the index's mtime are reported); the source-clean hints of section 3
  (hints come from the HEADs' ancestry alone); an older-capture baseline
  (decision 5).
- **The model** (`EstateConverge.tla`, section 8):
  - The `PreDest` constant became a nondeterministic `AdoptInit`: every
    component is the first capture's value or the operator's, refs absent
    at the destination included. One row covers every pre-existing
    destination.
  - Adoption is folded into existing actions, so no row's never-enabled
    set changed: `Begin`, `Decide_` (the proof), `Import` (`a_import`),
    `RefStep` (`a_refs`) and `WriteLanded` (`a_landed`).
  - The linked worktree waits for the main's adoption and first-lands.
    Its own adoption, A11b, A11c and A11d are not modelled.
  - A destination seat the capture lacks becomes the destination's own
    (its ghost is marked) when the adoption's ledger is written.
  - Before the main's adoption, the values the apply may claim are the
    adopted capture's (`AdoptVals`, `AdoptHead`), and a ref's absence, so
    an operator write of one of them is no edit. A ref deleted while the
    adoption is in flight is recreated by R1, as decision 4 accepts; the
    model found this case.
  - The liveness properties take `AdoptBehind` as their stated exception
    (decision 5), and `RefConverged` takes `AdoptKept(r)`: a ref R3 or
    R10 left at adoption is the destination's for good, even when it is
    only behind the source. The model found this case; it is the design's
    rule, now stated.
  - The five mutation rows of section 8 became four:
    `adopt_r1_over_existing` is `txn_no_cas`'s row already.

### Tests

- `tests/estate_adopt.rs` (with the fixture `tests/estate_adopt/gftb.rs`,
  the GFTB inventory's shapes):
  - P90 is the acceptance row. A live tree and a source go to a converged
    live tree through bulkload verbs only. The shapes are main and
    worktrees, a nested `.worktrees/` worktree, a branch diverged at the
    destination, a destination-only worktree with its own commit and
    dirt, a source-only worktree, a bare repository whose objects live in
    a mirror through `alternates`, an ssh-origin repository, and
    `node_modules` at both ends. Then an unchanged rerun reads 0 source and
    0 destination bytes, and a source change converges in place.
  - P91 is DIVERGED for a seat, a staged hunk and a switched HEAD, with
    the whole destination, `.git` included, unchanged.
  - P92 is an unrelated repository: OCCUPIED, nothing written.
  - P93 is a moved landing re-keyed under a live-path plan.
  - P94 is `estate-diff` read-only (destination and STATE) and agreeing
    with the apply.
  - P95 is an identical copy without `--adopt`: OCCUPIED, nothing written.
- Unit rows (`converge/tests.rs`):
  - `the_adopt_scratch_builds_the_index_a_landing_installs`, with the
    intent-to-add entries and modes. It found a defect: a placeholder in
    a missing directory refused `IO`. Fixed.
  - `the_adopt_proof_names_each_differing_component`.
  - `an_origin_prefix_activates_only_a_matching_remote_url`.
  - `the_stash_table_adopts_an_equal_destination_reflog`.
- Closure row:
  `omitted_rebuildable_roots_are_counted_and_unknown_is_not_zero`.
- Fault harness (`tests/fault_harness/estate_adopt.rs`): five
  `scenarios!` rows, one per point. Each crashes an `--adopt` child at its
  point (`_exit`, never signalled), then checks:
  - no seat, HEAD or index moved, and every ref is old or the capture's;
  - closure reports `journal-missing`;
  - before the ledger, a rerun without `--adopt` refuses OCCUPIED and
    writes nothing;
  - the rerun with `--adopt` finishes;
  - a second rerun is a no-op.

  The bare row checks that the destination is absent after the crash and
  that the rerun lands.

### Validation (2026-10-15, sting, on `5beb031` plus the staged work)

Every command ran in `nix develop .#default`, `nice -n 10`, with
`CARGO_TARGET_DIR=/srv/fast-local/jess/cache/cargo-target/adopt` (the
fault build in its `fault/` subdirectory, never `target/` in the tree)
and `TMPDIR=/dev/shm/adopt-tmp`. All passed; nothing was skipped.

- `cargo fmt --all -- --check`: clean.
- clippy three times, `-D warnings`: `--workspace --all-targets`;
  `-p bulkload-agent --all-targets --features io-trace`; `--workspace
  --all-targets --features bulkload-agent/fault-injection,bulkload-agent/io-trace`.
- `cargo test -p bulkload-agent --locked`: 27 test executables, 865
  passed, 0 failed, 7 ignored (the repo's standing ignores). Among them
  `estate_adopt` 7 of 7 (P90 to P96), `estate_converge` 23 of 23, and the
  lib's 686.
- `cargo test -p bulkload-proto`: 18 passed (the new refusal code).
- `fault_harness` (`fault-injection,io-trace`): 97 passed, the six
  `estate_adopt` rows among them; `power_loss`: 12 passed.
- `just contract-test`: repo-manifest PASS, `test_ci_contract.py` 24 OK.
- `just tla-render --check`: all 229 committed files equal the
  catalogue's rendering.
- `just tla-check` of every `MC_ec_` row (every one is touched: the
  `Adopt` constant and the two invariants are in each config), one
  invocation on the final spec, load average about 60 on 32 CPUs: 103
  rows matched (the budget self-test, 39 PASS with their never-enabled
  sets exact, 1 SIMULATION, 7 REACHED, 55 FAIL each on exactly its named
  property), `rc=0`, 3963 s wall. The adopt rows' distinct states equal
  `docs/formal/README.md`'s table (`MC_ec_adopt` 30486,
  `MC_ec_adopt_linked` 23340, `MC_ec_adopt_op` 14702, `MC_ec_adopt_live`
  6174), and every pre-existing pass row explores its recorded count
  (`MC_ec_one_checkout_crash` 1061046, `MC_ec_live_linked` 223978).
- Failing first: before the code, `estate_adopt` failed 6 of 6 (the
  verbs and flags did not exist; log kept at
  `/dev/shm/adopt-tmp/base-failing-first.log`, transient).

Not run here: `just props-deep`, `just crash-sweep` (section 9's
once-only gates), and the read-only sting acceptance of section 9, which
is the operator's check against production paths.

## Workstreams (restated, R-N13; facts verified this session unless marked)

| Stream | Owner | Repo / branch | State and evidence | Next |
|---|---|---|---|---|
| #226 adopt (PR 1 + PR 2 of section 10) | this lane | bulkload `feat/estate-adopt-20261015` at `5beb031` + staged work | Built, validated (section 12, "Validation"), staged, uncommitted. | Coordinator review and signed commit; operator answers to section 11 |
| #226 `--protect` (PR 3) | unassigned | - | Not built (section 11 decision 2). | Only if the operator wants asides on demand |
| #228 Q168/Q169 | other lane | merged | MERGED to origin/main `5beb031` (verified: `git fetch`, HEAD equals it). Its `StashEntry` (stash-order-v2) is integrated: T0 compares commit and message. | None for this lane |
| GFTB live | coordinator | `/srv/fast-local/jess/git` | Reconciled 17:06Z, verify_rc=0 (reported by `reconcile.receipt`; not re-verified here). Nothing in production was touched by this lane. | Decision 1 (unwind or keep), then the read-only sting acceptance of section 9 (operator's check, not run here) |

## 13. Review of 2026-10-16 (twelve findings)

Rulings cited: OI-1003-Q171 and Q172 (from the workflow's task text, as
in the header), OI-1003-Q144, OI-1003-Q1 (S4), R-N98 (fixture
repositories only disable hooks), R-N13. The relayed operator request was
the same go-ahead; nothing here is an operator ruling. Each finding was
reproduced before its fix, by a failing test or a surviving mutation.
Every fix refuses (`GIT_DESTINATION_OCCUPIED`, typed, pending review) or
records less; none writes more.

| # | Finding | Verdict | Fix | Proof |
|---|---|---|---|---|
| 1 | Adopt and re-key accept a path spelled through a symlink | Real (blocker). Both repros reproduced: `adopted-seats` under the link, and `journal-recovered=1 rekeyed=1` taking P1's ledger. | `adopt::foreign_shape`: the workspace and the plan's repository must equal their `canonicalize`, before any proof or write, in A11a and A11b. `estate-diff` says `symlinked-path`. Section 2's "must not be used against it" is now enforced. | P97 (A11a), P98 (A11b: the estate-path ledger's bytes unchanged, P1's rerun clean) |
| 2 | A11b has no identity check | Real. A typo `neo/x -> sting/y` re-keyed and converged y to x's root commit. | `adopt::rekey_refusal`: the ledger's item must not be one of this plan's (`Policy::items`); the capture must be the ledger's, or its HEAD the ledger's HEAD or a descendant (`merge-base --is-ancestor` in a scratch repository borrowing the destination's objects), which implies relatedness. Two items aliasing one tree are refused by the canonical rule. `estate-add-batch` already refuses two items whose canonical workspaces meet when the alias exists at add time (verified with the built binary: `GIT_DESTINATION_OCCUPIED`, in one batch and across calls); the runtime rule covers an alias made later, so `estate-add` is unchanged. | P99 (unrelated source; and a clone whose HEAD is an orphan branch: related, not descended), `a_rekey_never_takes_a_tree_from_an_item_of_the_same_plan` |
| 3 | A linked worktree whose admin backlink names another path is adopted | Real (adopted-seats=8 in the repro). | `foreign_shape`'s `worktree-link`: a linked item's `canonicalize(<admin>/gitdir)` (relative paths resolved against the admin directory) must equal `canonicalize(<worktree>/.git)`; a main checkout's git directory must be its own `.git`. Covers the `cp -a` copy sharing an admin directory. Run `git worktree repair` first. | P100 (no `bulkload/` in the admin directory; the main beside it adopts) |
| 4 | `recorded_ns` taken after the proof | Real. | Taken before the first seat read, as Git's racy-index check takes it. | `the_adopt_ledger_takes_its_racy_instant_before_the_first_read` (a hook at the first seat read; mutation to the old placement killed) |
| 5 | A11a does not re-check the ledger under the lock | Real. | After `landed::lock`, a workspace ledger refuses OCCUPIED. | `adopt_rechecks_under_the_lock_that_no_ledger_appeared` (the other ledger byte-equal after) |
| 6 | `AdoptKept` hides any ref with no row | Real. Reviewer mutations m2 (R2 lost) and m4 (R1 with no row) pass `MC_ec_adopt_live` on the old spec (7202 and 6602 states, no error; re-run here). | Ghost `adoptG.kept`: the refs the adoption's ref step left (R3 `kept-destination`, R10). `AdoptKept(r) == r \in adoptG.kept /\ ~Live(refLed[r])`. Mutation rows `adopt_r1_no_row` and `adopt_r2_not_adopted` must fail `ConvergesWhenQuiet`. | `MC_ec_neg_adopt_r1_no_row`, `MC_ec_neg_adopt_r2_not_adopted` FAIL as expected |
| 7 | A refs-only item into an unrelated repository is counted applied | Real (`outcome=refs-imported reason=occupied-foreign=1`); mutation c3 survived every test. | `adopt::repository` returns `GIT_DESTINATION_OCCUPIED` after the provenance import: no marker, no `adopted` record, no native ref. | P101 (closure `refused_pending_review:1`); c3 now killed by P101 |
| 8 | A11b has no relatedness check | Real, same as 2. | As 2 (descent implies relatedness). The section 12 deviation "stated, not prevented" is withdrawn. | P99 |
| 9 | The TOCTOU guards are not bitten | Real. c1 (`still` removed), c4 (settled print) survived; m3 (no recheck in `Import`) passes `MC_ec_adopt_op` on the old spec (15446 states, re-run here). | Test-only `mid_pass` stages `AdoptBeforeLock` and `AdoptSeatRead`. Model: ghost `adoptG.unequal` (an adoption import that proceeded with `~AdoptEqual`), checked by `AdoptClaimsOnlyEqual`; mutation `adopt_no_recheck`. In the model the recheck is not needed for `NoLocalWorkLost` (the next converge refuses the edit MODIFIED), so the claim itself is the property. | `adopt_rechecks_the_proof_under_the_lock` (seat, HEAD, staged hunk), `the_adopt_proof_refuses_a_seat_that_moved_while_it_was_read`; c1 and c4 killed; `MC_ec_neg_adopt_no_recheck` FAIL as expected |
| 10 | T0 unreachable in the model | Real. m5 (`Assert(FALSE)` at T0) passes all four adopt rows at exactly 30486, 14702, 6174, 23340 states (re-run here). | Under `Adopt` with `"stash"` in SrcKinds the source holds a stash at the copy (`SI`), and the destination's reflog may equal it. New pass row `MC_ec_adopt_stash` (LiveSpec) and mutation `adopt_t0_no_row`. The model then found two stash cases the design states for refs: a reflog the adoption's T2 kept (the source pushed since the copy) and one a capture carrying no stash left (the source dropped it) stay the destination's, with no row. Stated as `AdoptStashKept` (ghost `adoptG.stash`), the stash's `AdoptKept`; it matches the code (`stash_decision` T2, and no row when nothing is carried). | `MC_ec_adopt_stash` PASS; `MC_ec_neg_adopt_t0_no_row` FAIL as expected |
| 11 | `estate-diff` says `adoptable` where apply refuses OCCUPIED | Real (P102's shape: a checkout of another repository than the plan's). | `foreign_shape` is shared by `adopt::workspace`, `adopt::rekey` and both `estate-diff` paths; a ledger naming another item reports what A11b would do (`rekey_refusal`). New `adopt` words `symlinked-path` and `occupied-shape`, the reason in `components`. | P102, P97, P98, P100 |
| 12 | `--activate-origin` matches with no boundary | Real. | Match only at a boundary (the prefix ends in `/` or `:`, or the value's next byte is one); an `ssh://` prefix must carry the `/` after its host; every matching prefix goes into `origin-activated-by-prefix`, one per line. | `an_origin_prefix_activates_only_a_matching_remote_url` (extended), `every_matching_origin_prefix_is_named_in_the_receipt` |

**Open for the operator (not decided here):** the stash rule the model
found (finding 10) keeps a destination reflog that is only behind the
source's, as R3 keeps a behind ref. Adopting a destination reflog that is
a suffix of the carried one would need a new stash row; not built.

### Tests added (2026-10-16)

- `tests/estate_adopt.rs`: P97 (adopt through a symlink), P98 (a
  symlinked live-path plan never re-keys the estate-path ledger), P99
  (a re-key refuses an unrelated source and a related one whose HEAD does
  not descend), P100 (a stale linked-worktree backlink), P101 (a refs-only
  item into an unrelated repository refuses under `--adopt`), P102
  (`estate-diff` reports the shape the apply refuses).
- `converge/tests.rs`: `adopt_rechecks_the_proof_under_the_lock`,
  `the_adopt_proof_refuses_a_seat_that_moved_while_it_was_read`,
  `the_adopt_ledger_takes_its_racy_instant_before_the_first_read`,
  `adopt_rechecks_under_the_lock_that_no_ledger_appeared`,
  `a_rekey_never_takes_a_tree_from_an_item_of_the_same_plan`,
  `every_matching_origin_prefix_is_named_in_the_receipt`, and two cases
  in `an_origin_prefix_activates_only_a_matching_remote_url`.
- Failing first: P97 to P102 failed before the fix (apply assertions:
  `adopted-seats` under a symlink, `rekeyed=1`, the typo converged,
  `adopted-seats=8` at a stale backlink, `occupied-foreign=1` counted
  applied; P102 `adoptable`). Each unit row was checked by reverting its
  fix: c1 (`still`), c4 (settled print), the racy instant, the ledger
  re-check, the plan-item rule, the prefix boundary, the `ssh://` host
  rule and the multi-prefix receipt were each killed; c3 is killed by
  P101.

### Validation (2026-10-16, sting, on `5beb031` plus the staged work)

`nix develop .#default`, `nice -n 10`,
`CARGO_TARGET_DIR=/srv/fast-local/jess/cache/cargo-target/adopt` (the
fault build in its `fault/` subdirectory), `TMPDIR=/dev/shm/adopt-tmp`.
origin/main was fetched again: still `5beb031`, nothing to merge.

- `just check-source` (repo manifest, python, shell and workflow lint,
  the secret scan, `cargo fmt --check`, both clippy passes, `rust-test`):
  `rc=0`; all 31 test executables ran, 920 passed, 0 failed, 8 ignored
  (the standing ignores). Among them the lib 692, `estate_adopt` 13 of
  13, `bulkload-proto` 18.
- Clippy with `fault-injection,io-trace`: clean.
- The fault-harness recipe's steps, out of the tree: `fault_harness` 97
  passed (the `estate_adopt` rows among them), `power_loss` 12, the
  traced `io::` lib tests 64, the P5 partial-write proof 1 (not
  skipped), the resume power-loss rows 3.
- `just contract-test`: repo-manifest PASS, `test_ci_contract.py` 24 OK.
- `just tla-render --check`: 234 files current.
- `just tla-check`: the 14 adopt rows (236 s), then every `MC_ec_` row:
  108 matched, 3961 s, `rc=0` (`docs/formal/README.md`, "Review of
  2026-10-16").

Not run: `just props-deep`, `just crash-sweep` (the deep tier, on
demand), and the read-only sting acceptance of section 9.

### Workstreams (restated, R-N13; verified this session unless marked)

| Stream | Owner | Repo / branch | State and evidence | Next |
|---|---|---|---|---|
| #226 adopt, review of 2026-10-16 | this lane | bulkload `feat/estate-adopt-20261015` at `5beb031` + staged work | Twelve findings resolved (section 13), validated above, staged, uncommitted. | Coordinator review and signed commit |
| #226 open questions | operator | - | Section 11's decisions stand; the stash rule of finding 10 added (section 13). | Operator answers |
| #226 `--protect` (PR 3) | unassigned | - | Not built. | Only if asked |
| GFTB live | coordinator | `/srv/fast-local/jess/git` | Untouched by this lane. The live symlink paths now refuse under `--adopt` (P98), as section 2 required. | Decision 1, then the sting acceptance |

### Dogfood of 2026-10-16: F1 and F2 (fixed 2026-10-16)

Rulings cited: OI-1003-Q171, Q172 and Q173 (from the coordinator's task
text), R-N13. Source: the GloriousFlywheel neo->sting dry run,
`/srv/fast-local/jess/bulkload/gf-move-20261015/gf-move-plan.md` section
6, and its synthetic repro `fixture/run-*`. F3
(`CONTRACT_SELF_INCONSISTENT` naming) and F4 (`pull` of a single file or
into a missing directory) are left alone, as directed.

| # | Finding | Fix | Proof |
|---|---|---|---|
| F2 | A first linked landing imported provenance before its refusals: `restore_linked_staged_landed` ran `import_verified_instance` before the exclude check, and `estate::import_base` imported the plan base into the repository before `apply_workspace` was called at all. A linked item refused `GIT_IGNORE_POLICY_CONFLICT` or `GIT_DESTINATION_PARENT_MISSING` left `refs/carry/v1/<source>/...` refs and their objects behind. | `converge::check_linked` decides every refusal that needs no write before any write to the destination repository: the path (`first_landing_target`: bare capture, occupied, parent missing), then, from an `adopt::Scratch` beside the staged bundle that borrows the destination's objects read-only (the base imported into the scratch only), the capture's metadata, the common exclude policy (`linked_exclude`, finding 12's newer-exclude rule kept) and intent-to-add custody. `plan_linked` then takes the lock, settles, checks the exclude again (a settled intent may have changed it), and only then imports the base and the capture. `estate::import_base` no longer imports a linked item's base: it hands it to the apply as `Target::base` (`Place::base` is no longer standalone-only), and the base is recorded in `ImportedBases` only after that item's apply succeeds. So a linked adopt or converge also imports its base after its proof, as the standalone path already did. Nothing is rolled back: no import precedes a refusal that can be decided without a write. | P103: the GF shape (main adopt `GIT_DESTINATION_DIVERGED` on an exclude the source lacks, a source-only linked worktree `GIT_IGNORE_POLICY_CONFLICT`, a linked worktree under a missing parent `GIT_DESTINATION_PARENT_MISSING`): `for-each-ref` of the repository, `refs/carry` included, is unchanged, and so is the `lstat` identity of every entry of its `.git`. |
| F1 | `estate-diff` said `landing` for a source-only item the apply refuses. | `diff::first_landing` calls the apply's own `first_landing_target` (standalone) and `check_linked` (linked, when the repository exists): `ignore-policy-conflict` and `parent-missing` are new `adopt` values; any other refusal is the row's `refused` code. A linked item whose repository does not exist yet (the plan's main lands it first) is checked for its path only and carries the component `repository-absent`. | P104: the same shape; the diff says `ignore-policy-conflict` and `parent-missing`, and the apply then refuses `GIT_IGNORE_POLICY_CONFLICT` and `GIT_DESTINATION_PARENT_MISSING`. |

**Failing first, on `3fe1f07` with only the tests added:** P103 failed
at its refs assertion: 27 refs added to the destination repository, 17
of the capture's instance (`4ce01491...`) and 10 of the plan base's
(`5ac2a71a...`, `shared-base-head`). P104 failed on the only-src row
`"adopt":"landing"`. A first fix that moved only the capture's import
still failed P103 with the 10 base refs; that is how the base import in
`estate::import_base` was found.

**Crash safety.** A first linked landing is now: checks (scratch only;
the destination unchanged), the lock and `settle_repository` (as
before), the exclude re-read, the base and capture import (additive,
idempotent), the ref step, the landing intent, `worktree add`, the
ledger. A crash anywhere before the import leaves the destination as it
was; a crash after it leaves provenance only, as before, and the rerun
checks again before writing more. Provenance a crashed run imported is
not removed by a later refusal (no rollback: nothing deletes what this
run did not create).

**Model.** `EstateConverge.tla` is unchanged and no row was rerun. Its
`Begin` already takes the lock and settles (`RefSettle`) before
`FirstStepWt`'s import and ref step; the code imported before the lock,
a deviation the model did not show (the import is additive). The new
order matches the model. The pre-landing refusals change no modelled
variable: they are refusals before any write, and the model does not
carry the exclude policy for the linked worktree.

**Open (not fixed here).** `estate-diff` reads the destination as it
is. When a main item of the same plan converges first in the apply and
sets a newer exclude (finding 12's rule), the diff can say
`ignore-policy-conflict` for a linked item the apply then lands; when
the main lands the repository first, the diff says `landing` with
`repository-absent` and the exclude is checked only by the apply. The
scratch import costs a linked first landing a second `index-pack` of
its (thin) bundle, and the base's connectivity walk in the scratch.

**Validation (2026-10-16, sting, on `3fe1f07` plus this fix).** `nix
develop .#default`, `nice -n 10`,
`CARGO_TARGET_DIR=/srv/fast-local/jess/cache/cargo-target/adopt`
(`CARGO_BUILD_BUILD_DIR` its `build-dir`; the fault build in `fault/`),
`TMPDIR=/dev/shm/adopt-fix/tmp`. `cargo fmt --all -- --check` clean;
clippy `-D warnings` clean in all three passes (workspace; `io-trace`;
`fault-injection,io-trace`); `estate_adopt` 15 of 15 (P90 to P104);
`estate_converge` 23 of 23; the lib's `converge` unit tests 24 of 24;
`fault_harness` 97 of 97. `just tla-check`: not run, no row touched
(above). `just check-fast` is the coordinator's.

| Stream | Owner | Repo / branch | State and evidence | Next |
|---|---|---|---|---|
| #226 dogfood F1, F2 | this lane | bulkload `feat/estate-adopt-20261015` | Fixed and validated above; committed signed on top of `3fe1f07`, not pushed. | Coordinator: `just check-fast`, review |
| #226 dogfood F3, F4 | unassigned | - | Not touched, as directed. | Coordinator decides |
