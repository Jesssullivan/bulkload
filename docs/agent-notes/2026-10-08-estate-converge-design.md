# 2026-10-08 estate converge: native ref restore (#215) and re-apply convergence (design)

Lane: estate-converge-20261008 (worktree
`bulkload.worktrees/estate-converge-20261008`, branch
`feat/estate-converge-20261008`, based on #221's head `d6a8b86`).

**Status (2026-10-08, later).** The design below was reviewed (twenty
findings) and amended: section 16 records each finding accepted or rejected
with its reason, section 17 the open questions decided, and section 18 what
was built on this branch, how it was tested and what is deferred. The built
code had a second review (eight findings); section 19 records each and its
fix. Sections 20 and 21 record the fixes after review 3 and review 4 (2026-10-09). Where sections 1 to 15 and sections 16 to 21 disagree, 16 to 21 win; the text of
1 to 15 is kept as the reviewed design. The code is staged, uncommitted: the
coordinator commits and opens the PR, signed. **Merge is held** for the
operator ruling and the TLA+ model of Q1 (section 17). Evidence: the 2026-10-08 neo->sting run (Linear TIN-3692;
receipts `/srv/fast-local/jess/bulkload/neo-20261008/`). Rulings this design
serves: OI-1003-Q13 (a full migration is one more routine run), OI-1003-Q2 and
Q11 (S5, drift custody), OI-1003-Q6, Q10 and Q18(c) (S3), OI-1003-Q1, Q74 and
Q75 (S4), OI-1003-Q18(d) and Q100 to Q102 (WP0(d), the model for "replace only
what bulkload landed and nobody touched"), OI-1003-Q130 (#215 "fix next"),
OI-1003-Q137 and Q138 (the Plan C delta and the swap this design makes
unnecessary). The parts that change the product contract need a new ruling
before code (section 15). Citations are to `d6a8b86` unless stated.

## 1. What went wrong, and what the run had to do by hand

- **#215.** `restore_linked_staged` attaches a linked worktree to its source
  branch only when `refs/heads/<branch>` already exists at the destination with
  the captured tip (`git_carry.rs:6247-6268`). The destination never has it:
  `import_verified` (`git_carry.rs:5050`) writes every carried ref under
  `refs/carry/v1/<source>/<instance>/...` only, with one full copy per item
  instance (13,405 provenance refs for 59 instances in the landed
  `great-falls-tool-bus-infra`, against 276 native refs). So every linked
  worktree landed on `carry/<source>/<blake3(dest path)>`.
  `restore-refs.sh` made it native per main repository:
  - created each original ref create-only from one instance (0 conflicts on
    1,116 refs);
  - rebuilt `refs/stash` with `git stash store`, oldest first, using the
    source's reflog order and messages, which the capture does not carry in
    order (`neo-stashes.tsv`);
  - pointed each worktree's HEAD at its source branch after checking the tip,
    or left it detached where the source was.
  An operator-ruled script then deleted the `carry/*` branches.
- **No convergence.** A re-apply of an item whose capture changed finds no
  journal for the new capture digest (`journal_path`, `estate.rs:2507`), goes
  to restore, and `restore_staged` or `restore_linked_staged` refuses
  `GIT_DESTINATION_OCCUPIED` on the landed workspace. The delta needed a
  hand-built plan (Plan C, `neo-delta-C.sh`, `sting-apply-C.sh`) and a ref
  script that fast-forwarded the remote-tracking refs
  (`restore-refs-C.receipt`: `upstream/HEAD` and `upstream/main`,
  `32a918bf..0bee0f1f`).
- **Apply order by hand.** Plan A (the standalone mains) ran before the
  original plan (the linked worktrees). In one plan, a main and its linked
  worktrees outside the main's directory share a level (`apply`,
  `estate.rs:2620`), so nothing orders them. After a landing, a standalone
  item keys its lock by its path (`estate.rs:2643`) while its linked items
  key theirs by `common_repository` (`<repo>/.git`). Two mutexes guard one
  repository.

The target: re-running `estate-add`/`estate-capture`/`estate-apply` with the
same plan and state against the same landing converges, natively, with S3 and
S4 holding. **Scope as built (section 19, finding 5):** this holds for a
landing this branch made, which has ledgers. The 2026-10-08 landing under
`/srv/data` was made before it and has none, so a re-apply of it still
refuses `GIT_DESTINATION_OCCUPIED` until `--adopt` (Q8) or a fresh landing
is ruled (Q4). This branch does not converge the existing estate.

## 2. Design in one paragraph

Bulkload records what it landed **next to what it landed**: a ref ledger in
the destination repository's common git directory, and a workspace ledger in
each landed worktree's own git directory. Both are written in the same
durable order as the journal. A re-apply whose capture changed proves that the
destination still matches its ledger, by stat with a content fallback, as
WP0(d)'s `(dev, ino, size, mtime, ctime)` proof does for a file. It then
writes an intent, and applies only the delta between the landed capture and
the new one: refs by compare-and-swap, the stash by `git stash store`, each
changed seat by `RENAME_EXCHANGE` with a check of the displaced file, the
index under git's own `index.lock`, and HEAD by compare-and-swap. Last it
writes the new ledger and the journal. A destination that fails the proof is
refused `GIT_DESTINATION_MODIFIED` before anything is written, including
objects. A crash leaves an intent. The next apply settles it forward, one
component at a time, from "old or new" to new, before it does anything else.
First landing uses the same native ref rules (#215), so `carry/*` branches
never exist.

## 3. Data model changes

### 3.1 Capture side (two small additions; coordinate with the S4 lane)

1. **`stash-order-v1` metadata** (`capture_administration`,
   `git_carry.rs:1692`). The capture already reads the stash reflog newest
   first, as `authority.stash` (`read_authority`, `git_carry.rs:953`), and
   hashes it into the key. It names each stash commit only as
   `stashes/<sha>`, so the order is lost. Carry those exact bytes as one more
   metadata ref. There is no new source read and no key change, so S2 and
   S3 are unchanged.
   - Messages come from each stash commit's subject. For `git stash push`
     that is the reflog message byte for byte (the `neo-stashes.tsv` rows
     show it). Only a `git stash store -m <other>` message differs. That
     limit is accepted and listed.
   - A capture without the metadata (every retained capture today, and a
     reuse hit whose key did not move) rebuilds the stash in committer-date
     order and says `stash-order-inferred` in its receipt.
2. **A vanished source (`GIT_SOURCE_VANISHED`).** When `lstat(item.source)`
   is `ENOENT` before any git child runs (`preflight`, `estate.rs:1407`),
   capture refuses `GIT_SOURCE_VANISHED`. It keeps the item's
   `{item}.capture` record, as every refusal does, and publishes
   `{item}.vanished` in CORPUS: a postcard of the pass's `started_ns` and the
   capture digest the record still names. A later clean capture of the item
   retires the marker after its record is durable, as `retire_drift` does.
   This is the only capture-side refusal this lane adds. The S4 lane
   (#219, #220, #162) owns capture refusal typing, so the coordinator
   decides whether this lands here or there. The apply side (section 6)
   needs only the marker.

Observation time already exists. The `{bundle}.parts` sidecar holds the
pass's `started_ns` (`Parts`, `estate.rs:319`). Converge uses it to order
captures of one source. A capture without `.parts` is legacy, is never
reused, and counts as `started_ns = 0`.

### 3.2 The destination repository: a ref ledger and its intent

Under `$GIT_COMMON_DIR/bulkload/v1/<SOURCE>/` (0700, created by bulkload):

```rust
// refs.landed: atomic write (temp, fsync, rename, dir fsync), as estate::write.
struct RefLedger {
    refs: BTreeMap<String, LandedRef>,  // native ref name -> what bulkload landed
    stash: Option<LandedStash>,
}
struct LandedRef { oid: String, observed_ns: i128 }   // observed = capture's started_ns
struct LandedStash { order: Vec<String> /* newest first */, observed_ns: i128 }

// refs.intent: written durable before any native ref or stash write; removed after refs.landed.
struct RefIntent { ops: Vec<RefOp>, stash: Option<StashOp>, observed_ns: i128 }
struct RefOp { name: String, old: Option<String>, new: Option<String> }
struct StashOp { old: Vec<String>, new: Vec<String> }
```

Plus `lock`: a destination-side `flock`. S2 forbids locks on the source
only. It closes the one gap the in-process per-repository mutex leaves: two
applies with different STATE directories landing into one repository.

**Why in the repository and not in STATE.** Refs belong to a repository, not
to an item or a plan. The 2026-10-08 landing put one repository's refs down
through three plans and three STATE directories (A, B, C). A ledger beside
the refs is the one every plan and STATE shares. It also travels with the
repository: the swap's `cp -a` copied it, and stays consistent. It is keyed
by SOURCE label, so a bidirectional union keeps one ledger per direction. It
is not a ref namespace, so a later capture of this repository (sting as a
source) does not carry it: the census already excludes `.git`. The file
names bulkload's values only. Ownership is proved against the refs
themselves.

### 3.3 Each landed worktree: a workspace ledger and its intent

Under `$GIT_DIR/bulkload/v1/<SOURCE>/`. That is `.git/` for a standalone
item and `.git/worktrees/<name>/` for a linked one, so `git worktree remove`
removes the ledger with the worktree.

```rust
// workspace.landed
struct LandedWorkspace {
    item: [u8; 32],            // estate::id(item)
    capture: [u8; 32],         // bundle digest the destination now holds
    observed_ns: i128,         // that capture's started_ns
    instance: [u8; 32],        // its refs/carry/v1/<SOURCE>/<instance> provenance
    worktree: String,          // carried worktree commit (W)
    staged: String,            // carried staged commit (S)
    head: (Option<String>, String),   // symbolic ref (None = detached), oid
    index: [u8; 32],           // digest of index *entries* (3.4)
    exclude: [u8; 32],         // info/exclude bytes this item owns (standalone) or compared (linked)
    recorded_ns: i128,         // when the prints were taken (racy window, 4.2)
    seats: Vec<SeatPrint>,     // sorted by path
}
struct SeatPrint { path: Vec<u8>, kind: u8, mode: u32, size: u64, dev: u64, ino: u64, mtime_ns: i128, ctime_ns: i128 }

// workspace.intent
struct WorkspaceIntent {
    from: Option<[u8; 32]>,    // landed capture; None = a first landing
    to: [u8; 32], to_instance: [u8; 32], tag: [u8; 16],
    seats: Vec<SeatOp>,        // section 7, with the temporary name of each
    index: ([u8; 32], [u8; 32]), head: ((Option<String>, String), (Option<String>, String)),
    exclude: Option<([u8; 32], [u8; 32])>,
}
```

A print is about 80 bytes. A 200k-seat worktree's ledger is about 16 MiB,
the same order as its `filesystem-v1`. It is capped like the reuse manifest
(`REUSE_MANIFEST_CAP`). Over the cap, the item refuses `BUDGET_EXCEEDED` at
its first landing, before publish, and is never half-recorded.

### 3.4 Index entries, not the index file

`git status` rewrites `.git/index` (stat cache, untracked cache) without
changing what it says. The proof digests each entry's path, mode, oid and
stage, plus the extended flags the capture carries: intent-to-add,
skip-worktree and assume-valid, read by the raw reader `intent_to_add_entries`
already uses. Stat fields and extensions are excluded.

### 3.5 STATE (the apply state directory)

Unchanged: `{item}-{blake3(SOURCE)}-{capture digest}.done` journals and
`{item}.outcome` records stay the completion and S4 surface.

New: `{item}.custody`, a postcard of the item's ref and stash custody lines
from its last apply (`ref-rewound`, `ref-deleted-at-source`,
`ref-kept-destination`, ...). It is informational. Closure prints the counts
per row and the verdict ignores them.

### 3.6 Outcomes

`Outcome` gains `WorkspaceConverged` (`workspace-converged`) and
`RefsConverged` (`refs-converged`), **appended after `Refused`**, because
postcard encodes variants by index (`outcome.rs` module docs,
`golden_encodings_are_stable`). The journal body stays `workspace-restored` or
`refs-imported`: the journal says what the destination holds, not how it got
there. Closure adds them to the arms of `WorkspaceRestored` and `RefsImported`
(`closure.rs:177`, `:188`).

## 4. The ownership proof

### 4.1 Refs

A native ref is **owned** when its current value equals its `RefLedger` row.
An absent row means not bulkload's. A ref and its row are not one atomic
write. The intent settles a crash between them (section 8).

### 4.2 A workspace

A workspace is **owned and unchanged** when all of the following hold. They
are read-only checks, and run under the repository lock before any object is
imported.

1. `workspace.landed` exists for this SOURCE, its `item` is this item, and
   no `workspace.intent` is pending (one pending is settled first, section
   8).
2. HEAD's symbolic name and oid equal the ledger's. A worktree on a branch
   also needs that branch owned (4.1).
3. The index entries digest equals the ledger's.
4. The `info/exclude` bytes equal the ledger's.
5. A metadata-only census of the worktree matches the seats one for one:
   - **No seat is missing and none was added.**
   - **Excluded from the census:**
     - seats under the capture's omitted rebuildable roots
       (`REBUILDABLE_DIRECTORIES`), so a destination build is not a change;
     - the workspaces of other planned items (`planned_nests`), which carry
       their own ledgers;
     - the `.git` administration.
   - **Directories** compare by existence, kind and mode only; their mtimes
     move with every entry.
   - **Regular files and symlinks:**
     - Equal `(kind, mode, size, dev, ino, mtime_ns, ctime_ns)` means
       unchanged (the WP0(d) fast path).
     - A seat whose print differs, or whose mtime lies within
       `RACY_GRANULARITY_NS` of `recorded_ns`, is unchanged only if its
       kind and mode are equal and its bytes hash to the blob the landed `W`
       names (symlink: the target).
     - The fallback reads destination bytes only, counted as
       `read_dest_verify_existing_bytes`, and never a source byte (S3 is
       about source reads). Once adopted, the seat's print is refreshed in
       the next ledger.

Any failure is `GIT_DESTINATION_MODIFIED`, and nothing has been written.

**How this compares to WP0(d).** WP0(d) proves a file by identity against a
committed row, and so does this proof. It then exchanges and checks the
displaced file, and so does every seat write here (section 7). The
deliberate difference is the content fallback.
- An editor's identical save, a `touch`, or `git checkout` of the same
  bytes is not a change in git's sense. Refusing it would block routine
  convergence.
- WP0(d) refuses such a file by default and only adopts one whose bytes
  already match.
- Whether "changed" must mean identity rather than content is an operator
  question (section 15, Q2).

## 5. Native ref restore (#215)

Natively restored: every carried ref outside bulkload's namespaces. That is
`refs/heads`, `refs/tags`, `refs/remotes`, `refs/notes`, `refs/replace` and
any other namespace.
- **Never restored natively:** `refs/carry/**`, `refs/carry-*/**`, the
  capture's metadata names, and per-worktree refs (`refs/bisect/**`,
  `refs/worktree/**`, `refs/rewritten/**`). Those stay at provenance and are
  reported `ref-per-worktree-not-restored`.
- **`refs/stash`** goes through the stash table (7.3), never `update-ref`.
- **Provenance** under `refs/carry/v1/<SOURCE>/<instance>/` is written
  exactly as today. `import_verified` is unchanged.

Per item, under the repository lock and after `import_verified`:

1. **Plan the ref operations** with the pure function
   `native_refs::decide(native, ledger, carried, observed_ns, class)` (table
   7.2). For a first landing, every row is R1 to R3.
2. **Apply them in one `git update-ref --stdin -z` transaction** of `create`,
   `update <new> <old>` and `delete <old>` lines. Every line is a
   compare-and-swap, so a concurrent writer fails the transaction. No ref is
   overwritten blind.
3. **Rebuild the stash** (7.3).
4. **Write `refs.landed`.**

Per item kind:

- **Standalone** (`restore_staged`). The ref operations, stash, symbolic HEAD
  and both ledgers are written **inside the private stage**. The existing
  no-replace publish makes them atomic with the checkout. `set_ref(&work,
  &symbolic, &head)` (`git_carry.rs` in `restore_staged`) becomes a verify
  when the native transaction already created the branch.
- **Linked** (`restore_linked_staged`). The native ref step runs before
  `worktree add`.
  - Source on a branch whose native tip equals the captured head, and no
    other worktree has it checked out: `git worktree add --no-checkout
    <dest> <branch>`.
  - Source detached: `git worktree add --no-checkout --detach <dest>
    <head>`.
  - The branch is owned but now names a newer value (another item's newer
    capture moved it, the normal S5 skew): attach `--detach` at the captured
    head and report `head-detached-ref-newer`. The next pass, whose capture
    sees the new tip, re-attaches by row W5.
  - The branch is not owned and names another value, or another worktree has
    it checked out: refuse `GIT_REF_CONFLICT` before `worktree add`. Nothing
    is laid down.
  - The `carry/<source>/<digest>` path (`git_carry.rs:6267`) is deleted.
    `carry/*` branches are never created.
- **Refs-only items** (`import_staged`). Same ref and stash step. HEAD is
  touched only in a bare repository whose HEAD is unborn, where it is set
  to the carried symbolic name, as `restore-refs-C` did for the preservation
  mirror. A non-bare repository's HEAD belongs to its standalone item.

**Apply order.** In `apply`, an item whose `repository` is another planned
item's standalone workspace runs one level after it, so mains land before
their linked worktrees and Plan A/B is unnecessary. Lock keys use
`common_repository` whenever the repository exists, standalone or not
(`estate.rs:2643`).

## 6. Per-item decision table (estate-apply)

`C` is the current `{item}.capture`, `J(C)` its journal, `L` the workspace
ledger (or the ref ledger for a refs-only item), `I` a pending intent and
`D` the destination path. Rows are tried in order. The first match decides.

| # | Condition | Action | Outcome / refusal | Writes |
|---|---|---|---|---|
| A1 | CORPUS holds `{item}.vanished` newer than `C` | nothing | `GIT_SOURCE_VANISHED` (pending review: accept keeps the destination as landed; abandon) | none; `L` kept |
| A2 | no `C` | as today, and as the S4 lane's #219 makes it | `SEALED_OBJECT_MISSING` or the capture's own refusal | none |
| A3 | `I` pending for this SOURCE | settle `I` forward (section 8), then go on with `L := I.to` | settled, or `GIT_DESTINATION_MODIFIED` / `SEALED_OBJECT_MISSING` if `I.to`'s bundle is gone | the settle only |
| A4 | `J(C)` present | no-op. Stages nothing and walks nothing (round-4 P2, S3) | `previous-workspace-restoration-not-revalidated` / `previous-ref-custody-not-workspace-parity`, as today | none |
| A5 | `D` absent | first landing, native (section 5). New items land here as usual | `workspace-restored` / `refs-imported` | as today plus the ledgers |
| A6 | `D` present, `L.capture == C` | a crash after the ledger, before the journal: write the journal | the outcome `L` records | journal, outcome |
| A7 | `D` present, `C.observed_ns < L.observed_ns` | nothing (an older corpus applied over a newer landing) | `GIT_CONVERGE_STALE_CAPTURE` | none |
| A8 | `D` present, `L` present, the source's shape changed between `L` and `C` | nothing | `GIT_CONVERGE_SHAPE_CHANGED` | none |
| A9 | `D` present, `L` present, proof (section 4) fails | nothing | `GIT_DESTINATION_MODIFIED` | none |
| A10 | `D` present, `L` present, proof holds | converge `L -> C` (section 8) | `workspace-converged` / `refs-converged` | intent, refs, seats, index, HEAD, ledgers, journal |
| A11 | `D` present, no `L`, `--adopt`, and STATE holds this item's journal for an earlier capture `C0` | adoption proof against `C0` (section 9), then A10 | as A10 | `L` from the census, then A10 |
| A12 | `D` present, no `L` | nothing (not bulkload's) | `GIT_DESTINATION_OCCUPIED`, as today | none |

Notes on the rows:
- **A8.** Shape covers the object format, bare against non-bare, the
  shallow frontier, and the nested-repository custody (`.nested`), none of
  which converge carries. A change of captured configuration is not shape.
  Configuration is applied once at first landing. A changed
  `configuration-v1` is reported `configuration-not-converged` and preserved
  under `.git/carry-config` (open question Q6).
- **Items dropped from the plan** are not in `apply`'s loop. Their
  destination and ledgers stay, and closure lists their records as
  `foreign_outcome_records`, as today.
- **A refs-only item** runs A1 to A12 with the ref ledger as `L`. Its proof
  is 4.1 alone, and only the item's own repository HEAD is not involved.

## 7. Delta tables

### 7.1 Inputs

- `W0`, `S0`, `F0` and `H0` come from the landed instance's provenance refs,
  which the destination already holds.
- `W1`, `S1`, `F1` and `H1` come from `C`'s import.
- Seat operations come from `git diff-tree -r -z --no-renames W0 W1` plus
  the `filesystem-v1` rows whose mode or kind differ (`F0` against `F1`).
  Both trees are in the destination's object store after the import. The
  import is the head link alone when the destination already holds every
  prerequisite (`git bundle verify` against the destination); otherwise it
  is a `chain::flatten`, as today.

### 7.2 Per ref (`native_refs::decide`; `t` = `C.observed_ns`)

| Row | Native | Ledger | Carried | Action | Custody line |
|---|---|---|---|---|---|
| R1 | absent | none | `v` | create `v`; ledger `(v, t)` | `created` (counted) |
| R2 | `x` | none | `x` | adopt; ledger `(x, t)` | `adopted` (counted) |
| R3 | `x` | none | `v != x` | keep `x` | `ref-kept-destination n x carried=v` (v at provenance). Item's own HEAD branch: `GIT_REF_CONFLICT` |
| R4 | `x` | `(x, tl)` | `x` | verify; ledger `t := max(t, tl)` | none |
| R5 | `x` | `(x, tl)`, `t > tl` | `v != x` | CAS `x -> v`; ledger `(v, t)` | local branch or tag not fast-forward: `ref-rewound n x->v`. Remote-tracking: counted only (they follow the source) |
| R6 | `x` | `(x, tl)`, `t <= tl` | `!= x` or absent | skip (an older capture of another item) | `ref-stale` (counted) |
| R7 | `x` | `(x, tl)`, `t > tl` | absent | remote-tracking: CAS delete, ledger row removed. Branch, tag, other: keep, ledger kept | `ref-deleted-at-source n x` (x is also at provenance) |
| R8 | `y` | `(x, _)`, `y != x` | any | keep `y` | `ref-modified-at-destination`. Item's HEAD branch: fails the proof (A9) |
| R9 | absent | `(x, _)` | any | keep absent | `ref-deleted-at-destination`. Item's HEAD branch: A9 |
| R10 | `x` | none | absent | untouched | none |

"Never lost":
- every value R5 or R7 moves off is the value of the same ref under the
  provenance instance that landed it, which converge never deletes;
- R3, R8 and R9 never write.

A HEAD branch rewound at the source and owned at the destination follows the
source (R5), and its worktree converges to the new head with it.

### 7.3 The stash (refs/stash and its reflog)

`Dst` is the destination's `reflog show --format=%H refs/stash`. `S0` is the
ledger's order. `Sn` is the carried `stash-order-v1`, all newest first.

| Row | Condition | Action | Custody line |
|---|---|---|---|
| T1 | no ledger, `Dst` empty | `stash store -m <subject> <sha>` for `Sn`, oldest first | `stash-restored` |
| T2 | no ledger, `Dst` not empty | keep. Carried stashes stay at provenance | `stash-kept-destination` |
| T3 | `Dst == S0 == Sn` | none | none |
| T4 | `Dst == S0`, and `S0` is a suffix of `Sn` | store `Sn`'s new prefix, oldest first | `stash-appended` |
| T5 | `Dst == S0`, any other `Sn` | `stash clear`, then store `Sn` oldest first. Dropped entries stay at provenance | `stash-rewritten` |
| T6 | `Dst != S0` | keep | `stash-modified-at-destination` |

Recovery: while T4 or T5 is in flight, `Dst` is always `S0` or a suffix of
`Sn`, so the intent's `StashOp` settles forward deterministically.

### 7.4 Per seat (path `p`; `P` = its ledger print; the target from `W1`/`F1`)

| Change | Operation | Check |
|---|---|---|
| in `W0` and `W1`, blob or kind differs | write `.bulkload-converge-<tag>-<n>` beside `p` (bytes, mode, fsync); `exchange(temp, p)` | the displaced file at the temp name has print `P` (or its content when `P` was adopted by content): unlink it. Otherwise exchange back and refuse `GIT_DESTINATION_MODIFIED`; the intent stays |
| mode only | open `p` `O_NOFOLLOW`, `fstat`, `fchmod` | the `fstat` equals `P`, else refuse as above |
| in `W0`, not in `W1` | `rename_exclusive(p, temp)` | the moved file has print `P`: unlink. Otherwise rename back and refuse |
| not in `W0`, in `W1` | write the temp; `rename_exclusive(temp, p)` | `EEXIST`: a third party created `p` since the proof; refuse, temp removed |
| directories | removed deepest first after their entries; created shallowest first before them | a removed directory must be empty (only bulkload's removals ran) |
| kind change | a remove, then an add | as both |

Order: removals, replacements, additions, then `filesystem-v1` modes for the
changed seats only (`restore_filesystem_rows` restricted to them).

- **No atomic exchange** (OI-1003-Q100). A replacement on a file system
  without one refuses `DESTINATION_EXCHANGE_UNSUPPORTED` up front, before
  any write and with no fallback, as the transfer does. Adds and removes need
  no exchange.
- **Never touched:** seats under rebuildable roots, other items' workspaces
  and gitlink directories (`restore_gitlink_directories` runs for added
  gitlinks only).

**The index.**
- Create `$GIT_DIR/index.lock` exclusively (git's own lock; a live `git`
  holding it is a refusal, `GIT_DESTINATION_MODIFIED`).
- Copy the current index into the lock and apply `S0 -> S1` with
  `update-index --index-info` from `git diff-tree -r -z S0 S1`.
- Re-apply intent-to-add for the changed entries (`restore_intent_to_add`).
- Check that the entries digest equals `C`'s, then rename the lock over the
  index.
- The stat cache of untouched entries survives, so git re-hashes only the
  changed paths.

**HEAD.** `update-ref --no-deref HEAD <H1> <H0>` (CAS), or `symbolic-ref`
after a check when the source switched branch or detached (row W5 below).

**Exclude.**
- A standalone item replaces its own `info/exclude` when owned (proof 4).
- A linked item keeps today's equality check against the common exclude.
  Its main converges first (section 5, apply order).

| Row | Worktree HEAD at landing, then at `C` | Action |
|---|---|---|
| W1 | same branch, branch owned and moved by R5 | content delta; HEAD stays symbolic |
| W2 | detached `a`, then detached `b` | CAS HEAD `a -> b` |
| W3 | branch `x`, then branch `y` | `y` created or owned and not checked out elsewhere: `symbolic-ref HEAD refs/heads/y`. Otherwise `GIT_REF_CONFLICT` |
| W4 | branch, then detached | `update-ref --no-deref HEAD <H1>` |
| W5 | detached by `head-detached-ref-newer`, branch tip now equals `H1` | `symbolic-ref` back to the branch |

## 8. The converge pipeline, crash points and settling

Each item runs under its repository's mutex and the destination `lock`.
Steps 1 to 3 are read-only.

1. Stage `C`'s bundle and check its digest. The drift-marker and bare
   refusals run here, as today (`apply_item`, `estate.rs:2134`).
2. Settle any intent (A3). Proof (A7 to A9).
3. Plan: ref and stash operations, seat operations, index, HEAD.
4. Import objects and provenance (`import_verified`; head link or flatten).
   Additive and idempotent. Fault point `estate.converge.after_import`.
5. Write `refs.intent` and `workspace.intent` durably. `after_intent`.
6. Ref transaction, then stash operations, then `refs.landed`, then remove
   `refs.intent`. `after_refs`.
7. Seat operations, each settled before the next. `after_seat` (nth).
8. Publish the index. `after_index`.
9. HEAD. `after_head`.
10. Census the changed seats. Write `workspace.landed = C`, then remove
    `workspace.intent`. `after_landed`.
11. The journal and outcome record (the existing `emit` order).

**Settling an intent** (a rerun's A3; also the first thing any item of that
repository does for `refs.intent`):

- **Refs.** For each op: native equals `new` means done; equals `old` means
  redo by CAS; anything else is kept and reported
  `ref-modified-at-destination`. Then write the ledger and remove the
  intent.
- **Stash.** `Dst` equal to `old`, or a suffix of `new`, finishes forward.
  Anything else is T6.
- **Seats.**
  - A seat whose content and mode equal the target is done.
  - One that still has its print `P` is redone.
  - A temp left at the intent's name is checked like a displaced file: print
    `P` means unlink. A foreign file is exchanged back into `p` and the item
    refuses `GIT_DESTINATION_MODIFIED`. This is WP0(d)'s sweep rule:
    a displaced file of anyone else is restored, never swept as a temporary.
  - Anything else refuses `GIT_DESTINATION_MODIFIED`, with the intent kept.
- **Index and HEAD.** Equal to the target means done; equal to the old
  value means redo; anything else is `GIT_DESTINATION_MODIFIED`.
- **The bundle.** A corpus that no longer holds `I.to`'s bundle refuses
  `SEALED_OBJECT_MISSING`, typed.

**What crash safety means here.** A live checkout has no single atomic
switch: files, the index, HEAD and refs are separate names. So the
guarantee is per component and per item.
- After any crash, every component is in its old state or its new one, or a
  third party's, which is refused and never clobbered.
- The intent names which old and which new.
- The next apply completes the item before anything else touches it.
- Until then the item has no journal for `C`, so closure counts it
  unaccounted (`journal-missing`). S4 cannot pass over a half-converged
  item.

This is the property the formal model checks (section 11). "Either the old
or the new state per item" as a single atomic switch is not achievable
without copying the whole worktree and exchanging the directory. That costs
the full checkout on every small delta and still cannot cover the index and
HEAD in the same step, so this design does not do it.

**One mechanism for first landings too (optional).** A first linked landing
is the same pipeline with `from = None` and adds only. That retires the
residual partial-worktree gap #216 documents for `restore_linked_staged`: a
rerun finishes its own partial worktree instead of refusing
`GIT_DESTINATION_OCCUPIED`. It is cheap to include, but not required by this
lane's properties.

## 9. Adoption of the 2026-10-08 landing (`--adopt`, explicit)

The landing at `/srv/data/jess/estate/neo-20261008/git` has journals but no
ledgers. Its native refs and worktree HEADs were made by `restore-refs.sh`.
Adoption runs per item, only with `--adopt`, and only when STATE holds the
item's journal for an earlier capture `C0`. It finds `C0` by scanning the
item's `.done` names. `C0`'s instance comes from its bundle's native lines,
or from `C0`'s provenance in the destination.

1. Prove the workspace against `C0` by content: every seat equals `W0`, the
   index entries equal `S0`, and HEAD is either `H0` detached as at the
   source, or on the source branch with tip `H0` (the `restore-refs.sh`
   shape), or on `carry/<source>/<digest(dest)>` at `H0`.
   - This reads the destination once (`read_dest_verify_existing_bytes`).
2. Adopt refs. A native ref whose value is the same ref's value under any
   `refs/carry/v1/<SOURCE>/*/` instance in that repository gets a ledger row
   with `observed_ns = 0`.
   - That covers `restore-refs-C`'s fast-forwards to the Plan C instance.
   - Other refs are R3 or R10.
   - `observed_ns = 0` means any new capture supersedes the adopted value.
3. Delete each `carry/<source>/<digest(dest)>` branch whose value is in the
   item's provenance and that no worktree has checked out, by CAS. Re-point a
   HEAD still on one to the source branch first (W3).
4. Write the ledgers from a census, then continue at A10 if `C != C0`.

The swapped copies in `/srv/fast-local/jess/git` are a separate question
(section 15, Q4).

## 10. New refusal codes

All are typed and dispositionable under S4 (accept, re-carry, abandon). None
is `IO`.

| Code | Meaning | Written before anything? |
|---|---|---|
| `GIT_DESTINATION_MODIFIED` | The destination bulkload landed was changed since landing: a seat, the index, HEAD, the exclude, or the item's HEAD branch. Or a third party changed it during a converge, and its file was exchanged back. | Yes for a proof failure (A9). Mid-converge, only bulkload's own completed components are new, and the intent records them |
| `GIT_REF_CONFLICT` | The item's HEAD branch exists at the destination with another value bulkload does not own, or is checked out by another worktree, so the worktree cannot be attached as at the source. | Yes |
| `GIT_SOURCE_VANISHED` | The item's source root no longer exists (capture), recorded by the apply from `{item}.vanished`. The destination is kept. | Yes |
| `GIT_CONVERGE_SHAPE_CHANGED` | Between the landed capture and this one, the source changed something converge does not carry: object format, bare, shallow frontier, nested-repository custody. | Yes |
| `GIT_CONVERGE_STALE_CAPTURE` | The capture is older than the one the destination already holds (`observed_ns`). | Yes |

`DESTINATION_EXCHANGE_UNSUPPORTED` (existing, OI-1003-Q100) is reused for a
replacement without an atomic exchange. `GIT_DESTINATION_OCCUPIED` keeps its
meaning: occupied by something bulkload did not land. Adding the codes
touches `bulkload-proto/src/refusal.rs` (variants, `code`, `CODES`),
`tests/refusal_taxonomy.rs`, and the Dhall `Refusal` union only if GitCarry's
decision core names them (it does not).

## 11. What the formal model must add (for the later TLA lane)

A new module `docs/formal/EstateConverge.tla`. GitCarry models CORPUS
custody; this is destination custody.

**Constants:** a small set of refs, seats and items in one repository (a
main and one linked worktree), source capture instants, and the booleans
`ExchangeSupported` and `ThirdParty`.

**Variables:**

| Group | Variables |
|---|---|
| Source | `src` (refs, stash list, seats, index, HEAD per worktree), changed by `SourceStep` (commit, branch force-move or delete, remote-tracking move or prune, stash push or drop, seat edit, index edit, switch or detach) |
| Corpus | `corpus` (a sequence of captures, each a snapshot with an `observed` instant) |
| Destination | `dst` (native refs, provenance, stash, seats with an identity version, index, HEAD) |
| Ledgers | `refLedger`, `wsLedger`, `refIntent`, `wsIntent`, `journal` |
| Ghosts | `everLanded` (every ref value bulkload ever set), `thirdPartyVals` |

**Actions:**

| Action | Code it models |
|---|---|
| `Capture` | `capture_item` |
| `ApplyFirst` | the native restore (#215) |
| `Prove` | read-only |
| `Import` | `import_verified` |
| `BeginIntent` | the intent write |
| `RefTxn` | one CAS transaction, or a files-backend partial commit under `Crash` |
| `StashStep` | one `stash store` or the clear |
| `SeatExchange` / `VerifyDisplaced` / `ExchangeBack` | 7.4 |
| `IndexPublish`, `HeadSet`, `WriteLanded`, `WriteJournal`, `ClearIntent` | the later steps of section 8 |
| `Settle` | a rerun's A3 |
| `ThirdPartyWrite(component)` | enabled at every point when `ThirdParty` |
| `Crash` | at every point |

**Invariants:**
- `ConvergeNoClobber`: a seat, ref, index or HEAD value bulkload replaces
  or removes always equals its ledger row or an unsettled intent's `old`.
  A `thirdPartyVals` value is never destroyed; it is at its path, or was
  exchanged back.
- `ComponentOldOrNew`: under a pending intent, every component is `old`,
  `new` or a third party's, and in the last case the item is refused.
- `JournalImpliesLanded`: `journal[i] = c` implies `wsLedger[i] = c`, and,
  with no third-party write since, `dst` holds `c`'s components.
- `RefNeverLost`: every value in `everLanded` is native or at provenance.
- `NoStaleRegress`: an owned ref's `observed` never decreases.
  `GIT_CONVERGE_STALE_CAPTURE` holds on the item level.
- `ModifiedRefusedUntouched`: a proof failure leaves `dst` unchanged, with
  no objects or provenance either.
- `NoopReadsZero` (S3): apply with `journal[i] = current capture` stages and
  writes nothing.
- `Accounted` (S4): in a quiescent state after `Settle`, every item has a
  journal for its current capture or a typed refusal. An item with a
  pending intent has no journal for it.

**Liveness:** with weak fairness on `Settle` and apply, and no
`ThirdPartyWrite` and no `SourceStep` from some point on, every item reaches
`journal = latest capture` and `dst` equals a fresh restore of it
(`ConvergeRefinesRestore`). The fresh restore is a second, independent
`ApplyFirst` into an empty destination, compared at the end.

**Mutations,** each expected to fail its named property:

| Mutation | Fails |
|---|---|
| converge without `Prove` | `ConvergeNoClobber` |
| exchange without `VerifyDisplaced` | `ConvergeNoClobber` |
| a sweep that unlinks a displaced foreign file | `ConvergeNoClobber` |
| a ref update without CAS | `ConvergeNoClobber` |
| journal before `WriteLanded` | `JournalImpliesLanded` |
| intent after the first destination write | `ComponentOldOrNew` |
| apply a capture with `observed <= ledger` | `NoStaleRegress` |
| delete provenance on converge | `RefNeverLost` |
| a no-op that re-stages | `NoopReadsZero` |

**Rows:** `MC_conv_main`, `MC_conv_crash`, `MC_conv_third_party`,
`MC_conv_noexchange` (adds and removes only converge), and the mutation rows
`MC_conv_neg_*`. The Haskell explorer is not required for this lane. If the
ref decision is extracted as a pure core, its 10 rows can be pinned like
`decide_rows.tsv`.

**Abstractions to state in its README section:**
- identity is a version every write moves;
- the files-backend partial commit is modelled as any subset of a
  transaction's lines;
- content-equal adoption (4.2) is a third-party write that restores the
  landed version, which is not a change.

## 12. File and function plan

| File | Change |
|---|---|
| `crates/bulkload-proto/src/refusal.rs` | The five codes (variants, `code`, `CODES`), with docs |
| `crates/bulkload-agent/src/git_carry/landed.rs` (new) | `RefLedger`, `LandedWorkspace`, the intents, `SeatPrint`. Paths under `$GIT_COMMON_DIR`/`$GIT_DIR`, atomic read and write (`estate::write`'s discipline), the destination `lock`. `index_entries_digest` (via the raw index reader) |
| `crates/bulkload-agent/src/git_carry/native_refs.rs` (new) | The pure `decide` (7.2), `plan` (inventory to ops), `apply` (`update-ref --stdin -z` CAS), the stash table (7.3, `stash store`), per-worktree and carry namespace filters. Used by `restore_staged` (in the stage), `restore_linked_staged` and `import_staged` callers |
| `crates/bulkload-agent/src/git_carry/converge.rs` (new) | `prove` (4.2), `shape_changed` (A8), `plan_seats` (diff-tree, filesystem rows), `apply_seats` (exchange or rename plus displaced check, via `io::exchange` and `io::sys::rename_exclusive_at`), `publish_index` (index.lock), `set_head`, `settle` (section 8), `adopt` (section 9). Fault points |
| `crates/bulkload-agent/src/git_carry.rs` | `restore_staged`: native refs, stash and both ledgers inside the stage. `restore_linked_staged`: native refs first, attach on the source branch or `--detach`, no `carry/*`, the workspace ledger after the checkout. `capture_administration`: the `stash-order-v1` metadata |
| `crates/bulkload-agent/src/estate.rs` | `apply_item`: table A1 to A12. `apply`: lock key via `common_repository` for existing standalone items; standalone-before-linked levels; `--adopt` threading. `item_space`/`space_plan`: a converging item charges its head-link size, not the full bundle. Capture: the `{item}.vanished` publish and retire (subject to the S4 lane, 3.1) |
| `crates/bulkload-agent/src/outcome.rs` | Two variants appended after `Refused`; `NAMED`; the golden test |
| `crates/bulkload-agent/src/closure.rs` | Two arms; the per-row custody counts from `{item}.custody` |
| `crates/bulkload-agent/src/fault.rs` | Points `estate.converge.after_import`, `after_intent`, `after_refs`, `after_seat`, `after_index`, `after_head`, `after_landed`, with their crash-leaves table |
| `crates/bulkload-agent/src/counters.rs` | `converge_refs_{created,updated,kept,stale}`, `converge_seats_{added,replaced,removed}`, `converge_proof_seats`. Content fallback reuses `read_dest_verify_existing_bytes` |
| `crates/bulkload-agent/src/main.rs` | `estate-apply --adopt` |
| `docs/design.md` | Git carry: native refs, the ledgers, convergence. Amend "Import preserves divergence and leaves active HEADs, indexes and working bytes untouched" to name the owned-and-unchanged exception, after the ruling (Q1) |
| `docs/formal/README.md` | A pending-symbols row for the new module until the TLA lane lands |

Rough size: about 1,800 lines of Rust with tests, most of it in
`converge.rs` and its tests. The capture-side diff is about 40 lines.

## 13. Test plan

The properties run through `test_support::prop_config` (fixed seed; deep tier
x20). The numbers are proposed; the coordinator confirms them against other
open lanes.

| Id | Property | Generator / check |
|---|---|---|
| P80 NATIVE-REFS | First landing restores native refs, stash order and messages, and each worktree's HEAD as at the source. No `refs/heads/carry/*`. Destination refs with other values are untouched and reported; provenance is unchanged from today | Ref sets over heads, tags, remotes, notes and a custom namespace. Stash push sequences. A main plus 0 to 3 linked worktrees, each on a branch or detached. Optional pre-existing destination refs |
| P81 CONVERGE-EQUIV | `converge(restore(c0), c1)` equals `restore(c1)` into an empty destination. Compared: native refs (except refs deleted at source, kept by R7 and reported), the stash reflog, HEAD, index entries, worktree bytes, modes and kinds | Source ops between captures: commit, branch create, delete or force-move, tag, remote-tracking ff, non-ff or prune, stash push or drop, tracked, untracked or ignored edit, add, remove, chmod, symlink retarget, type change, staged-only change, switch, detach |
| P82 CONVERGE-NO-CLOBBER | With one generated destination mutation (seat edit, add, remove, chmod, `git add`, HEAD moved, HEAD branch moved, stash dropped), the item refuses `GIT_DESTINATION_MODIFIED` and the destination is byte-identical to before the apply: tree, `.git/refs`, `packed-refs`, index, objects. A mutation outside the item (another ref) converges and reports it | As P81 plus the mutation |
| P83 CONVERGE-IDEMPOTENT (S3) | After a converge, an unchanged source gives: capture 0 content bytes (`capture-reused-after-census`); apply `read_bundle_stage_bytes == 0`, no destination write (snapshot with mtimes equal), outcome `previous-*`; closure `unaccounted == 0` | P81's states |
| P84 REF-DECISION | `native_refs::decide` equals table 7.2, exhaustively over its finite domain. For every permutation of item order inside one apply, the final native ref is the newest capture's value (`NoStaleRegress`) | Exhaustive, plus permutations |
| P85 STASH-ORDER | Restore and converge reproduce the source's stash order and messages; every dropped stash commit stays at provenance | Push and drop sequences |

Integration test file `crates/bulkload-agent/tests/estate_converge.rs`, which
drives `estate::add_batch`, `capture`, `apply` and `closure` the way
`sting-apply-2.sh` drives the CLI:

1. **`capture_apply_mutate_recapture_apply_converges`.**
   - Setup: one plan holding a standalone main, two linked worktrees (one
     detached), a refs-only bare mirror and a stash.
   - After landing, mutate the source:
     - a new branch and a new linked worktree, added to the plan;
     - a remote-tracking ff, and a non-ff force update;
     - a commit on a worktree's branch;
     - tracked, untracked and ignored edits, and a staged-only change;
     - a stash push.
   - Recapture and re-apply. Expect `workspace-converged` /
     `refs-converged` on the existing items and `workspace-restored` on the
     new one. The result equals a fresh restore (P81's comparison).
     `closure` passes with `unaccounted == 0`.
2. **`a_locally_modified_destination_refuses_and_is_untouched`.**
   - Run once per mutation kind:
     - a seat edit, an add, a removal, a chmod;
     - `git add` and `git commit` in the landed worktree;
     - `git switch`;
     - a `branch -f` of the worktree's branch.
   - Each refuses `GIT_DESTINATION_MODIFIED`. The destination snapshot is
     unchanged (no new objects or provenance). The other items in the plan
     converge. `closure` shows the refusal pending review, and an `accept`
     disposition closes the gate.
3. **`content_equal_touch_and_git_status_are_not_modifications`.** `touch`,
   an identical rewrite, and `git status` (index stat refresh) all converge.
4. **`a_second_reapply_is_a_noop_reading_zero_bytes`.** Capture
   `source_bytes_read == 0`; apply `read_bundle_stage_bytes == 0`; the
   destination is unchanged, mtimes included.
5. **`source_vanished_is_typed_and_keeps_the_destination`.**
   `GIT_SOURCE_VANISHED` at capture and apply. The destination and its ledger
   are intact. A recreated source converges again.
6. **`rewound_and_deleted_branches_are_preserved_and_reported`.** R5
   (`ref-rewound`, the old value at provenance) and R7 (branch kept,
   remote-tracking deleted).
7. **`an_older_corpus_over_a_newer_landing_refuses_stale`.** A7.
8. **`adopt_a_pre_converge_landing`.** A landing made by today's code, then
   `restore-refs.sh`'s steps replayed in the test (native refs from one
   instance, `stash store`, HEAD to the source branch). `--adopt` converges,
   removes the `carry/*` branches, and a second apply is a no-op.
9. **`one_plan_orders_mains_before_their_linked_worktrees`.** No Plan A/B
   split. A standalone main and three linked worktrees in one level, `jobs =
   2`.

Crash tests use the existing fault harness. A new scenario kind,
`tests/fault_harness/estate_converge.rs` (`EstateConverge`, shaped like
`EstateSidecar`), builds item 1's fixture smaller, lands `c0`, mutates the
source and recaptures `c1`. It then arms the point and applies (the child
`_exit`s). For every row it checks four things:
- the crash state: every component is old or new (`ComponentOldOrNew`, by
  comparing against the snapshots of `restore(c0)` and `restore(c1)`);
- the rerun applies and the result equals `restore(c1)`;
- `closure` reports `unaccounted == 0` after the rerun and
  `journal-missing` before it;
- a second rerun is a no-op.

`scenarios!` rows:

```
estate_converge_after_import       => EstateConvergeAfterImport: 1, EstateConverge::Delta;
estate_converge_after_intent       => EstateConvergeAfterIntent: 1, EstateConverge::Delta;
estate_converge_after_refs         => EstateConvergeAfterRefs: 1, EstateConverge::Delta;
estate_converge_after_seat_first   => EstateConvergeAfterSeat: 1, EstateConverge::Delta;
estate_converge_after_seat_mid     => EstateConvergeAfterSeat: 5, EstateConverge::Delta;
estate_converge_after_index        => EstateConvergeAfterIndex: 1, EstateConverge::Delta;
estate_converge_after_head         => EstateConvergeAfterHead: 1, EstateConverge::Delta;
estate_converge_after_landed       => EstateConvergeAfterLanded: 1, EstateConverge::Delta;
estate_converge_after_seat_foreign => EstateConvergeAfterSeat: 3, EstateConverge::ForeignWrite;
estate_first_linked_after_seat_mid => EstateConvergeAfterSeat: 4, EstateConverge::FirstLinked;  // only if 8's unification lands
```

- **`ForeignWrite`** uses a test hook (`converge::mid_seat::fire`, shaped
  like `git_carry::mid_pass`) that rewrites the next seat between its proof
  and its exchange. Expect: the foreign bytes are at the path after the
  crash and after the rerun, the item refuses `GIT_DESTINATION_MODIFIED`,
  and the intent is kept.
- **`crash-sweep`** picks the new points up through `sweep_rows`, every
  `nth`.
- **Power loss** (`tests/power_loss.rs`, a syscall-log replay) is a
  follow-on. It is listed, not required by this lane, the same split #187
  had before its power-loss traces.

Validation before the coordinator's PR: `just check-fast` and `just
fault-harness` in `nix develop`, then `just props-deep` and `just crash-sweep`
once on demand.

## 14. Limits and non-goals (stated, not hidden)

- **Configuration** is applied once, at first landing, and never converged.
  A source config change is reported.
- **Per-worktree refs** (`refs/bisect`, `refs/worktree`, `refs/rewritten`)
  stay at provenance only.
- **Provenance grows** by one full ref copy per item instance per pass:
  13,405 refs from 59 instances in one repository today. That is unchanged
  by this design and is a scaling concern for routine reruns (Q5).
- **A stash message set with `stash store -m`** that differs from the commit
  subject is not reproduced.
- **R-N29** (bulkload#48: apply proceeds on an occupied destination,
  recording uncaptured seats) stays deferred. Converge never merges into
  a destination it did not land.
- **No three-way merge.** A destination modified locally is refused per
  item, never merged.

## 15. Open questions for the operator

1. **Ruling to amend the product contract.** `estate.rs`'s module doc and
   `docs/design.md` say "Import preserves divergence and leaves active HEADs,
   indexes and working bytes untouched" and "Completed restores are never
   replayed over subsequent operator edits". Converge replaces HEAD, index
   and working bytes that bulkload itself landed and nobody changed.
   Operator edits stay refused. Does this need a ruling (proposed
   "OI-1008-Qn: owned-and-unchanged convergence"), with the TLA model before
   merge, as OI-1003-Q102 did for #187?
2. **What "changed locally" means.** By identity (WP0(d)-strict: any stat
   move refuses), or by content (4.2: a `touch`, an identical save or
   `git status` is not a change)? The design takes content, because git's
   own semantics are content.
3. **Refs deleted at the source.** Local branches and tags are kept and
   reported (R7); remote-tracking refs are deleted when owned (they follow
   the source, value at provenance). Should owned local branches deleted at
   the source also be deleted, which mirrors neo exactly, or kept until an
   operator prune?
4. **Where routine reruns land.** The convergence target is the bulkload
   landing (`/srv/data/.../neo-20261008/git`); the operator works in the
   swapped `cp -a` copies under `/srv/fast-local/jess/git`. Choose:
   - (a) keep `/srv/data` as the converged mirror and move work into
     fast-local by `git fetch` or the swap;
   - (b) re-plan items to fast-local paths and `--adopt` the copies by
     content proof, so any sting-side work in a worktree refuses that item
     `GIT_DESTINATION_MODIFIED`.
5. **Provenance growth.** Keep one full `refs/carry/v1/<SOURCE>/<instance>/`
   copy per item per pass (today's custody), or carry provenance in the
   destination as a ref table (the OI-1003-Q54 shape: one table commit and a
   tip per distinct object)? The latter is a separate lane.
6. **Configuration convergence.** Report-only (proposed), or converge the
   safe declarative keys under the same ownership proof?
7. **`GIT_SOURCE_VANISHED`'s home.** It is a capture-side refusal. Does the
   coordinator land it with this lane or hand it to the S4 lane (#219)?
   This lane needs only the `{item}.vanished` marker on the apply side.
8. **`--adopt` explicit or automatic.** Adoption reads every landed byte
   once and accepts `restore-refs.sh`'s HEAD shape. The design makes it
   opt-in per run.

## 16. Review findings and decisions (2026-10-08)

Twenty findings were raised against sections 1 to 15. Each is accepted or
rejected here with its reason; "built" names what section 18 holds.

| # | Finding (short) | Decision | Reason, and what was built |
|---|---|---|---|
| 1 | Another item's R5 moves a branch a different worktree has checked out | **Accept** | `native_refs::decide` gains the guard row G: a ref that is any destination worktree's HEAD (`worktree list --porcelain`) is skipped by every item's ref step (`ref-checked-out-elsewhere`) and its row is not touched. Only the worktree's own item moves it, with its HEAD, in step 9. A worktree with no planned item, or one bulkload did not land, never has its branch moved. Tested: P84 row G; `estate_converge.rs` P81 (a commit on the linked worktree's branch, the main applied first: the main reports `ref-checked-out-elsewhere`, the linked item converges, and the result equals a fresh restore). |
| 2 | The item's own HEAD branch moves before seats and index | **Accept** | The HEAD branch leaves the step-6 transaction (finding 1's guard covers it). It moves in step 9 in one `update-ref --stdin -z` transaction with HEAD (`symref-update`), after the index is published. Order: seats, exclude, index, HEAD and its branch. The crash rows check every component old or new at every point; "`git status` coherent" in the strict sense is not achievable between the index publish and HEAD (Git's own checkout has the same window) and is stated, not claimed. |
| 3 | Index publish has no compare-and-swap on the old entries | **Accept** | `publish_index` creates `index.lock` exclusively (with this converge's marker), then checks the current entries digest equals the landed one under the lock, then renames the new index over. A `git add -p` between the proof and step 8 refuses `GIT_CONVERGE_INTERRUPTED` with the staged hunk kept. The post-check stays (the built index must equal a fresh build of the new staged tree plus intent-to-add). A P82 row mutating the index between proof and index step is **not built** (it needs a mid-converge hook); the CAS is exercised by the `index_locked` crash row. |
| 4 | Stash rows ignore capture order; `stash clear` has no CAS | **Accept** | `LandedStash` carries `observed_ns`; a capture with `t <= tl` skips (`stash-stale`). `stash clear` is gone: a rewrite deletes `refs/stash` by compare-and-swap on its tip (`update-ref --stdin` `delete refs/stash <tip>`), each store is a CAS on the tip, and the reflog is re-read under the destination lock just before. Anything else is T6 (`stash-modified-at-destination`). The stash is in the stash table tests and P81's comparison; P84's property covers refs only. |
| 5 | Settle misreads bulkload's own crash leftovers | **Accept, by content instead of per-seat phases** | The intent names both captures and a tag; the settle recomputes the delta from both instances' provenance and classifies every seat by content: a temporary holding the new or the old side is bulkload's own and is removed; anything else at a temporary name is a displaced third-party file and goes back. An absent path is done for a removal and a pending add for an add. `index.lock` holding this converge's marker, or an index whose entries are the target's, is bulkload's own and removed. A seat under an ancestor the converge already turned into a file is done. Fault points `seat_before_exchange`, `seat_after_exchange`, `index_locked` added. `kind_between` is not needed: a kind change is one exchange, not a removal then an add (a directory to file change is a removal, a `rmdir` and an add, each settled by content). |
| 6 | A mid-converge refusal shares A9's code; the intent has no way out | **Accept (code, closure); resolution by settle-forward** | New `GIT_CONVERGE_INTERRUPTED`. Closure classifies it `unaccounted` (`converge-interrupted`) whatever its disposition, and an attestation cannot close it (`native-converge-interrupted`). The way out is the rerun: once the operator removes their file, or puts the landed bytes back, the settle completes forward. `--release-intent` (record the mixed state and move on) is **not built**: it needs a ruling (section 17, Q10). The TLA `Accounted` invariant change is listed for the model lane (section 11). |
| 7 | Git's ref, stash and index writes are not fsynced | **Accept the fix; reject moving the power-loss row here** | Every destination writer runs with `-c core.fsync=reference,index,objects -c core.fsyncMethod=fsync` (`git_env::DESTINATION`, applied by `durable`), and every destination verb refuses `GIT_UNAVAILABLE` on a Git older than 2.46 (also needed for `symref-update`). Rejected part: the syscall-log power-loss harness records this process's syscalls, not a Git child's, so it cannot see the ordering this fix makes durable; the row stays with #49. |
| 8 | R7 deletes the row: the final ref depends on item order | **Accept** | Tombstones `LandedRef { oid: None, observed_ns }`; R1 against a tombstone with `t <= tl` is `ref-stale`; a newer capture that also lacks the ref moves the tombstone forward (P84's property found this case: without it, a capture between two that lack the ref resurrected it). Retention: 90 days behind the ledger's newest instant. |
| 9 | R2 adopts refs in a repository bulkload did not create; A12 blocks refs-only import | **Accept, without the per-item refs-only ledger** | `RefLedger.created` is the repository marker; it is set only by a standalone first landing in its private stage. A refs-only or linked item into a repository without it keeps provenance only, as before (refs-only: `refs-imported`, not A12). R2 never adopts a checked-out ref (finding 1's guard). Rejected part: a per-item ledger for refs-only items. Their flow is idempotent per ref (each row carries its own instant, R4/R6), so A6/A7 are not needed for them. |
| 10 | The displaced-file check compares ctime | **Accept** | The landed regular file is opened `O_NOFOLLOW` before the exchange and checked on that descriptor against the full print; after the exchange the displaced name must be that inode with the same size and mtime, ctime ignored (`materialize.rs`'s discipline). A seat adopted by content gets the print it had when its bytes were compared. |
| 11 | A first linked landing now refuses `GIT_REF_CONFLICT` | **Accept** | No `GIT_REF_CONFLICT`. Whenever the branch cannot be attached, the worktree lands detached at the captured head with `intended_branch` recorded (`head-detached-ref-conflict`, or `head-detached-ref-newer` when a newer capture owns it); the attach rule is re-run on every converge, so it re-attaches once the branch is free. In a foreign repository an absent branch is created by `worktree add -b` (a new name overwrites nothing, as the retired `carry/*` branch did not). |
| 12 | `info/exclude` is shared; the main's converge makes a linked item refuse | **Accept** | The common exclude is not in a linked item's proof. The ref ledger records the exclude a standalone item set, with its instant; a first linked landing whose capture's exclude differs is not refused when the destination's is that recorded one and newer than the capture (`exclude_newer`). A standalone item converges its exclude like a seat (old or new by digest). |
| 13 | A5 recreates a worktree the operator removed | **Accept** | When the workspace is absent and STATE holds a journal of this item for another capture, the item refuses `GIT_DESTINATION_MODIFIED` and nothing is laid down (`a_removed_landing_is_not_laid_down_again`). |
| 14 | Settling needs `I.to`'s bundle in the corpus | **Accept** | The settle reads both instances from destination provenance (`refs/carry/v1/<SOURCE>/<instance>/`) only; it refuses `SEALED_OBJECT_MISSING` only when those refs are missing. |
| 15 | A6 writes a journal without proving; an `item` mismatch reads as A9 | **Accept** | A6 runs the proof before the journal (`journal-recovered`). A ledger naming another item (a plan edit or re-plan) refuses `GIT_DESTINATION_OCCUPIED`, never `GIT_DESTINATION_MODIFIED`. |
| 16 | Symbolic refs are not handled | **Accept** | Native restore never writes a carried `refs/remotes/*/HEAD` (capture carries it resolved) nor any ref that is a symref at the destination (`for-each-ref %(symref)`); every transaction runs `--no-deref`. A failed compare-and-swap transaction refuses `GIT_CONVERGE_INTERRUPTED` with the ref intent kept; the next apply settles each op forward. Carrying symref targets is a capture-side change, left to the S4 lane. |
| 17 | The W table is incomplete; `symbolic-ref` is not a CAS | **Accept** | `intended_branch` in `LandedWorkspace`; the W rows collapse into one attach rule (`head_plan`) that covers every case (anything not attachable detaches with a report). HEAD moves only by `update-ref --stdin` with an old value (`symref-update ... ref <old>` or `oid <old>`), a compare-and-swap. |
| 18 | Any untracked destination file blocks convergence | **Accept** | The proof covers the ledger's seats and the delta's added paths only. Other destination seats are left alone; a removed directory that still holds them stays (`seat-kept-destination`). A change in the omitted rebuildable set is shape (A8). |
| 19 | Prints include `dev` | **Accept** | Prints are `(kind, mode, size, ino, mtime, ctime)`; the ledger records the file system's `statvfs` `f_fsid` once. The next ledger takes the print of every seat adopted by content. |
| 20 | Too much complexity for one-time or noisy cases | **Accept 1, 3, 4, 6, 7; accept 2 in part** | (1) `--adopt` is not built; deferred with Q4/Q8. (2) `GIT_SOURCE_VANISHED` is handed to the S4 lane (Q7); `GIT_CONVERGE_STALE_CAPTURE` stays a typed refusal: it arises only when an older corpus is applied over a newer landing, which is an operator error worth a review, not a routine event. (3) No new `Outcome` variants and no `{item}.custody`: the outcome stays `workspace-restored` / `refs-imported`, and the receipt's reason carries the custody counts (`converged ref-moved=1 seats-replaced=3 ...`; a first landing prints only the exceptional ones). (4) The unified first landing is dropped. (5) Rejected: a shallow frontier change stays A8 (a typed refusal for review); converging `.git/shallow` is not in this lane. (6) Over the ledger cap a landing lands without a workspace ledger and reports `not-convergeable`. (7) P82 and T6 agree: a stash dropped at the destination is kept and reported, not refused. |

## 17. Open questions decided (conservatively)

- **Q1 (ruling to amend the contract): yes.** Converging a checkout
  replaces HEAD, the index and working bytes bulkload landed. That is an
  exception to design.md's "leaves active HEADs, indexes and working bytes
  untouched", so it needs an operator ruling (proposed
  "OI-1008-Qn: owned-and-unchanged convergence") and, as OI-1003-Q102 did
  for #187, the TLA+ model (section 11) before merge. The code is built
  and staged on this branch; **the PR is held for both.** design.md states
  the exception as pending that ruling.
- **Q2 (what "changed" means): content, as designed.** A touch, an
  identical save or `git status` is not a change; kind and mode must also
  match. No work can be lost by replacing bytes equal to what bulkload
  landed, so this is the safe reading; a print change alone costs one
  destination read of that seat, counted.
- **Q3 (local branches and tags deleted at the source): keep and report**
  (`ref-deleted-at-source`), never delete; their values stay at
  provenance. Remote-tracking refs follow the source (`ref-pruned`).
- **Q4 (where reruns land): (a).** `/srv/data/jess/estate/neo-20261008/git`
  stays the converged mirror. No adoption of the `cp -a` copies, and no
  re-plan to `/srv/fast-local` paths, in this lane. That landing has no
  ledgers, so a re-apply of it refuses `GIT_DESTINATION_OCCUPIED` as today
  until an adoption verb is ruled and built.
- **Q5 (provenance growth): keep** today's per-instance provenance; a ref
  table in the destination is a separate lane.
- **Q6 (configuration): report-only.** A changed `configuration-v1` is
  reported `configuration-not-converged`; nothing is written.
- **Q7 (`GIT_SOURCE_VANISHED`): the S4 lane (#219).** This lane adds no
  capture-side refusal and no apply row A1.
- **Q8 (`--adopt`): not built.** Deferred until Q4's target is ruled; when
  built it is an explicit one-shot repair verb, never automatic.
- **Q9 (property numbers):** P80 to P85 do not occur anywhere else on this
  branch (`grep -rn 'P8[0-9]'`); the coordinator confirms against the other
  open lanes. P80, P81, P82, P83, P84 and P85 are used as section 13
  proposed.
- **Q10 (new): an interrupted converge's escape.** Whether an operator may
  release a pending intent and keep the mixed state (`--release-intent`),
  instead of resolving the third party's component and rerunning. Until
  ruled, the only way out is the rerun.

## 18. What was built (2026-10-08)

Rulings cited: OI-1003-Q130 (#215 "fix next"), OI-1003-Q13, Q2, Q6, Q10,
Q11, Q18(c), Q1, Q74, Q75 (the SLOs the lane serves), OI-1003-Q100
(no exchange, no replacement), R-N98 (fixture repositories only disable
hooks), R-N11 (crash children end themselves; nothing is signalled),
R-N13. No commit, push or PR: the coordinator stages, signs and opens it.

### Code

| File | What |
|---|---|
| `crates/bulkload-agent/src/git_carry/landed.rs` (new) | `RefLedger` (repository marker `created`, rows with tombstones, `LandedStash` and `LandedExclude` with their instants), `RefIntent`/`RefOp`/`StashOp`, `LandedWorkspace` (item, capture, instant, instance, HEAD, `intended_branch`, index digest, exclude digest, `recorded_ns`, `fsid`, seats), `SeatPrint` (no `dev`), `WorkspaceIntent`. Magic-prefixed postcard, strict decode, 256 MiB cap, atomic write (temporary, fsync, rename, directory fsync), the destination `flock`, `statvfs` `f_fsid`, gitfile and `commondir` resolution. |
| `crates/bulkload-agent/src/git_carry/native_refs.rs` (new) | `decide` (table 7.2 as amended: guard row, tombstones, `ref-pruned`), `plan`, `restorable`, the one ref writer `update_refs` (`update-ref --no-deref --stdin -z`, CAS lines), `transact`, `step`/`commit`/`settle` (intent, transaction, stash, ledger, intent removed), the stash table (`stash_decision`, `stash_apply`: CAS delete and CAS stores, re-read under the lock), `carried_stash` (`stash-order-v1`, or inferred and reported). |
| `crates/bulkload-agent/src/git_carry/converge.rs` (new) | `Meta` (an instance's provenance), `index_digest`, `head_of`, `head_plan` (the attach rule), `set_head` (HEAD and its branch, one transaction), `delta` (seat operations in execution order), the proof, `shape`, the seat `Engine` (fresh: descriptor check, `RENAME_EXCHANGE`, displaced check, exchange back; settling: by content), `probe_exchange`, `publish_index` (`index.lock` CAS, `read-tree -m -i` keeping the stat cache, fresh fallback), `apply_workspace` (rows A3 to A12), `converge`, `settle`, `apply_refs`, `pending`, and the first-landing helpers. |
| `crates/bulkload-agent/src/git_carry.rs` | `git_env::DESTINATION` and `durable` (finding 7), `destination_git_ready` (Git 2.46+), `read_tree`, `import_verified_instance`, `stash-order-v1` capture metadata (only when a stash exists; no key change), `restore_staged_landed` (native refs, stash, HEAD and both ledgers in the stage; a standalone plan base imported into the stage), `restore_linked_staged_landed` (native refs first, attach or detach, no `carry/*`, the exclude rule of finding 12, the workspace ledger). `restore_staged` and `restore_linked_staged` (the CLI verbs) go through them. |
| `crates/bulkload-agent/src/estate.rs` | `apply_item` through `converge` (A3's metadata-only pending check before the journal; finding 13's `landed_before`); receipts carry custody words; lock keys by common repository for every item; `after_mains` (mains a level before their linked and refs-only items); `import_base` hands a standalone item's base to its landing. |
| `crates/bulkload-agent/src/closure.rs` | `GIT_CONVERGE_INTERRUPTED` is `unaccounted` (`converge-interrupted`) and not attestable. |
| `crates/bulkload-proto/src/refusal.rs` | `GIT_DESTINATION_MODIFIED`, `GIT_CONVERGE_SHAPE_CHANGED`, `GIT_CONVERGE_STALE_CAPTURE`, `GIT_CONVERGE_INTERRUPTED` (each raised by product code). |
| `crates/bulkload-agent/src/fault.rs` | Eleven points `estate.converge.*`: `after_import`, `after_intent`, `after_ref_intent`, `after_refs`, `seat_before_exchange`, `seat_after_exchange`, `after_seat`, `index_locked`, `after_index`, `after_head`, `after_landed`. |

The P76 source-command registry is unchanged: destination writes reuse the
registered subcommand sites (one new `update-ref` site replaces the removed
restore `update-ref HEAD`; the new `read-tree` uses go through one helper
that also serves the standalone restore), so every pinned count holds.

### Tests

- `native_refs/tests.rs`: P84 (named rows; `decide` against the table over
  its whole 864-point domain; every item order of up to four captures ends
  at the newest capture's value, proptest through `prop_config(48)`), the
  stash table T1 to T6 and stale, the plan's namespace filters.
- `converge/tests.rs`: the delta's order and numbering, the attach rule,
  the index digest unmoved by a touch and `git status` and moved by a
  stage. `landed.rs`: record round trip and foreign bytes, tombstone
  retention, directory prints. `closure.rs`: an interrupted converge stays
  unaccounted.
- `tests/estate_converge.rs`: P80 (a main, a linked worktree on a branch
  and a detached one in one plan, `jobs = 2`: native refs, stash order and
  messages, HEADs, no `carry/*`, the ledgers, closure 0); P81 and P83 (a
  working day at the source, including a commit on the linked worktree's
  branch, a branch switch, a pruned remote-tracking ref, a stash push and
  drop, file-to-symlink and directory-to-file changes: the converge equals
  a fresh restore, refs equal but the kept local branch, closure 0, then a
  rerun is a no-op with the destination unchanged); P82 (seven destination
  mutations, each refused `GIT_DESTINATION_MODIFIED` with the checkout,
  native refs and stash as the operator left them); touch, identical
  rewrite, `git status` and a destination-only file converge; an older
  corpus refuses `GIT_CONVERGE_STALE_CAPTURE`; a removed landing is not laid
  down again; an operator's own clone stays `GIT_DESTINATION_OCCUPIED` with
  no `bulkload/` written into it.
- `tests/fault_harness/estate_converge.rs`: fifteen `scenarios!` rows over
  the eleven points (first and later hits of the seat points), each
  checking ComponentOldOrNew at the crash, `journal-missing` before the
  rerun, the rerun equal to a fresh restore with no temporary, lock or
  intent left, and a second rerun a no-op; and `ForeignWrite`: a third
  party's write to a seat not yet converged makes the rerun refuse
  `GIT_CONVERGE_INTERRUPTED`, keeps the bytes and the intent, stays
  unaccounted, and converges once the seat is put back.

### Deviations from sections 1 to 15 (besides section 16)

- The proof runs before the import, so a failed proof writes nothing. The
  shape check (A8) and the check for a destination seat where the source
  added one run after the import: those refusals leave only the additive
  objects and provenance.
- Every destination verb, the CLI restores included, needs Git 2.46.
- A grouped plan (a main with its linked worktrees) now lands in one apply:
  a standalone item's plan base is imported into its stage. Before, such a
  main refused `GIT_DESTINATION_OCCUPIED` and the 2026-10-08 run split
  Plans A and B.
- Custody is printed, not stored: no `{item}.custody` file, no new
  `Outcome`.
- Not built: `EstateConverge.tla` (Q1, before merge), `--adopt` (Q8),
  `GIT_SOURCE_VANISHED` (Q7, S4 lane), `--release-intent` (Q10), the
  power-loss replay of the Git writes (finding 7), the unified first
  landing (a crashed linked first landing still leaves a partial worktree
  that refuses `GIT_DESTINATION_OCCUPIED`, #216's residual), the
  `converge_*` counters (receipts carry the counts), a mid-converge hook
  for P82's "index changed between proof and index step" row, and a
  `space_plan` charge for a converge's delta (it still charges the whole
  bundle, an over-estimate). The crash sweep (`just crash-sweep`) does not
  sweep the converge rows: like P70's estate rows they have no copy fixture
  and run as pinned rows.
- Limits stated: a nested repository's commit changes `nested-repositories-v1`
  and so refuses A8 until re-landed; a branch created at the destination,
  absent at the source and in no capture, is untouched forever (R10); a ref
  created and deleted at the source between two captures of one apply can
  end present or absent depending on item order (P84 holds for refs
  bulkload landed before).

### Validation (2026-10-08, in `nix develop`, git 2.54, lane target dir)

- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- The same with `--features bulkload-agent/fault-injection,bulkload-agent/io-trace`: clean.
- `cargo test -p bulkload-agent --locked -- estate git_carry closure`: lib
  346 passed, 0 failed, 4 ignored; the filtered integration binaries all ok.
- `cargo test -p bulkload-agent --locked --test estate_converge`: 7 passed.
- `cargo test -p bulkload-agent --locked --features fault-injection --test
  fault_harness -- estate_converge every_fault_point estate_after`: 23
  passed (the fifteen converge rows, the converge child entry, the P70
  estate rows, `every_fault_point_has_a_scenario`).
- Guards: `source_command_registry` 16, `refusal_taxonomy` 2,
  `prop_seed_guard` 5, `dep_graph` 8, all passed, with no registry entry
  changed.
- `cargo test -p bulkload-agent --locked --lib` (whole lib): 573 passed, 5
  ignored. `cargo test -p bulkload-proto`: 14 passed.
- Not run in this lane: `just check-fast` as a whole (the Python contract
  tests, the other workspace crates' tests), `just fault-harness` in full,
  `just props-deep`, `just crash-sweep`, and TLC.

### Open (for the coordinator)

- Rule Q1 (proposed OI-1008-Qn) and land `EstateConverge.tla` before merge.
- Confirm P80 to P85 against the other open lanes (Q9).
- The S4 lane: `GIT_SOURCE_VANISHED`; carrying symref targets; the
  `stash-order-v1` metadata this lane added at capture
  (`capture_administration`) touches its area.
- Rule Q10 (`--release-intent`).
- Linear TIN-3692 / the SSOT ledger: record this lane's state when the PR
  opens.

## 19. Second review of the built code (2026-10-08, later)

Eight findings against the staged implementation. Each was verified
against the code; every code fix has a test that failed before it
(evidence in the validation list below). Rulings cited: OI-1003-Q130,
OI-1003-Q1, Q2, Q11 (S4, S5), R-N98 (fixture repositories only), R-N13.

| # | Finding (short) | Verdict | Fix and test |
|---|---|---|---|
| 1 | The HEAD plan predates the ref step, so `set_head`'s CAS fails for a branch the step just created or moved, on every rerun | **Confirmed (blocker)** | `Place::refs` adds the checkout's new HEAD branch (`meta1.symbolic`) to the step's skip set, in `converge` and `settle`: only `set_head` moves it, with HEAD (findings 1 and 2 of section 16, as intended). `set_head` drops the branch line when the branch already holds its new value (a settle after another item's step moved it): HEAD alone moves. Tests: `a_main_switching_to_a_new_or_moved_branch_converges` (`switch -c feature` plus a commit; then a switch to `other` after `other` moved: R1 and R5), `a_linked_worktree_switching_to_a_moved_branch_converges` (the main refused by an operator edit, so the linked item's own step is the one that would have moved `other`). Both refused `GIT_CONVERGE_INTERRUPTED` before. Limit kept: the checkout's old HEAD branch stays skipped by its own step too (it is checked out there until step 9), so a source commit on it before the switch reaches the destination at the next changed capture. |
| 2 | A directory the source turned into a file or symlink, holding a destination entry, refuses only after writes and never settles | **Confirmed (major)** | Before the intent, every `Add` whose path is a landed directory walks that directory; an entry at any depth that is not a landed seat refuses `GIT_DESTINATION_MODIFIED` with nothing written but the additive import (`holds_unlanded`). Test: P82 rows `OwnFileWhereSourceTurnsDirectoryIntoFile` and `...IntoSymlink`; before, the file row refused `GIT_CONVERGE_INTERRUPTED`. |
| 3 | A branch renamed into a sub-path (`foo` to `foo/bar`) blocks every item of the repository | **Confirmed (blocker)** | `native_refs::plan` computes the names that exist after the plan (native, minus deletes, plus creates) and keeps back a create above or below one of them (`Line::NameConflict`, `ref-name-conflict`; its row untouched, so it stays at provenance). `transact` runs deletes in their own transaction first (a pruned `release` frees `release/1.0`). `finish` drops a create that can never apply instead of retrying it, restoring its row. `head_plan` detaches with the branch recorded (`head-detached-ref-conflict`) when the HEAD branch's name conflicts. Tests: P84 `p84_a_create_above_or_below_a_ref_that_stays_is_kept_back` (local both ways, remote-tracking prune and create); `a_ref_renamed_into_a_sub_path_converges_without_blocking` (local, remote-tracking, then the HEAD branch `main` to `main/next`; then a no-op rerun). Before: `GIT_CONVERGE_INTERRUPTED`; with only the `head_plan` check removed, the HEAD phase still refused. Not done: a typed refusal for other deterministic `update-ref` failures (a new code is the S4 lane's area); with name conflicts planned out, the remaining failures are lost compare-and-swaps. |
| 4 | Same as 2 (an editor or build file inside the replaced directory) | **Confirmed (duplicate of 2)** | Same fix and P82 rows. |
| 5 | Nothing already landed in `/srv/data` can converge (no ledger, no `--adopt`) | **Confirmed; documentation only** | Already decided in section 17 (Q4, Q8), but section 1's target read as if the existing estate converged. Section 1 and design.md now state that a pre-branch landing refuses `GIT_DESTINATION_OCCUPIED` and a repository without the marker gets provenance only, unlike `restore-refs.sh`. The coordinator rules Q4 and Q8 together: build `--adopt` (rule A11) or re-land `/srv/data` fresh. |
| 6 | Removing the attestation guard for `GIT_CONVERGE_INTERRUPTED` broke no test | **Confirmed (test gap)** | `closure::tests::attestation_cannot_close_an_interrupted_converge`: a row bound to the same source and current capture is rejected `native-converge-interrupted` and the item stays unaccounted. It fails with the guard arm deleted. |
| 7 | A sibling's pending ref intent makes finished items re-prove; an edited one flips to refused | **Confirmed (minor)** | `converge::pending` returns `{ refs, workspace }`. With the current capture's journal present, only this item's own workspace intent bypasses the journal; a shared ref intent is settled by `converge::settle_refs` (lock, `native_refs::settle`) without proving or restaging, and the journal's outcome stands (custody words on the receipt). Test: `a_sibling_ref_intent_does_not_re_prove_a_finished_item` (a refs-only item of the same repository interrupted by a concurrent writer's `refs/tags/v9.lock`; the operator edited two finished checkouts). Before: the main refused `GIT_DESTINATION_MODIFIED` over its earlier `workspace-restored`. |
| 8 | #215 still differs from `restore-refs.sh`: remote HEAD symrefs, stash messages, inferred stash order | **Confirmed; known gaps, documented** | Recorded below as known gaps; no code. |

**Known gaps against `restore-refs.sh` (finding 8).**

- `refs/remotes/*/HEAD` is never restored (`restorable`): the capture
  carries it resolved, so a landing has no `origin/HEAD` or
  `upstream/HEAD`, which restore-refs-C fast-forwarded. Carrying symref
  targets is a capture-side change, handed to the S4 lane (finding 16);
  the coordinator confirms the handoff.
- Stash messages come from each stash commit's subject (`%s`), not the
  source reflog's message. They differ for `git stash store -m <other>`
  and any entry whose reflog message is not its subject. Optional later:
  capture the reflog messages next to `stash-order-v1` (the capture key
  already hashes the reflog, so S2/S3 are unchanged).
- A capture reused from before this branch has no `stash-order-v1`; its
  stash order is inferred from committer time (`stash-order-inferred`)
  and can swap two stashes made in the same second.

**Validation (second review, in `nix develop`, git 2.54, lane target dir).**

- Before the fixes (index versions of `converge.rs`, `native_refs.rs`,
  `estate.rs`, new tests in place): the five integration tests above
  failed (`GIT_CONVERGE_INTERRUPTED` for 1 and 3 and P82's new file row;
  `GIT_DESTINATION_MODIFIED` on the rerun for 7); the closure test failed
  with the guard deleted; the HEAD phase of finding 3's test failed with
  only the `head_plan` check removed.
- After: `cargo fmt --all --check` clean; `cargo clippy --workspace
  --all-targets --locked -- -D warnings` clean, and the same with
  `--features bulkload-agent/fault-injection,bulkload-agent/io-trace`;
  `cargo test -p bulkload-agent --locked -- estate git_carry closure`: lib
  348 passed, 4 ignored, the filtered integration binaries ok;
  `--test estate_converge`: 11 passed; `--features fault-injection --test
  fault_harness -- estate_converge every_fault_point estate_after`: 23
  passed; guards `source_command_registry` 16, `refusal_taxonomy` 5,
  `prop_seed_guard` 8, `dep_graph` 2, all passed; whole lib 575 passed, 5
  ignored; `bulkload-proto` 14 passed.


## 20. Fixes after review 3 and the EstateConverge model (2026-10-09)

Lane: the convergence lane, worktree
`bulkload.worktrees/estate-converge-20261008`, branch
`feat/estate-converge-20261008` at `5b8e550` (this code merged with
`origin/main` `91d4294`, #222's S4 sweep included, and with the
EstateConverge model branch `feat/formal-estate-converge-20261009`).
Rulings cited: OI-1003-Q144 (the convergence merges only when the model
passes against the code), OI-1003-Q32 and Q43 (the catalogue and
tla-check pipeline), OI-1003-Q7, R-N11 (only processes this lane started;
none signalled), R-N98 (fixture repositories only), R-N13 (this section and
the formal note). Nothing committed or pushed; everything is staged.

Every finding the model reported against the code
(`2026-10-09-formal-estate-converge.md`, "Model-vs-implementation
discrepancies") is fixed in the code, except items 6 and 18, kept as
documented limits. Each fix has a Rust row that fails on the code before
it: an integration row run against the pre-fix tree (`git archive 5b8e550`
with the new test file), or a unit row run against a mutation that puts
the old code back (the "Evidence" column). In the model each fixed
finding row became a pass row and the code before the fix a mutation whose
`MC_ec_neg_` row fails the finding's property; the review 3 mutations
(CODE BEFORE THE FIX in the catalogue) keep their rows.

| Item | Finding | Status | Code | Rust row (evidence) | Model |
|---|---|---|---|---|---|
| 16 | Settle moves a branch checked out since the crash (blocker) | Fixed | `native_refs::finish` reads `checked_out_by` (HEADs, rebases, bisects) and never redoes an op on such a branch: row restored, `ref-checked-out-elsewhere` | `a_settle_never_moves_a_branch_checked_out_since_the_crash` (mutation MU1: moved `topic` under the operator's HEAD) | `MC_ec_neg_settle_moves_checked_out` fails; every pass row holds the rule |
| 17 | No HEAD verification in the ref transaction | Fixed | `transact` holds every worktree's HEAD with Git's own `HEAD.lock` (`HeadLocks`; `update-ref` refuses `worktrees/<id>/HEAD`, so `symref-verify` can name only its own worktree) and reads the checked-out set under them; an op on a checked-out or held branch, or a HEAD locked by another writer, refuses `GIT_CONVERGE_INTERRUPTED` | `a_transaction_never_moves_a_checked_out_or_held_branch` (MU2: moved it) | `MC_ec_neg_txn_no_symref_verify` |
| 8 | `finish`'s names after the step | Fixed | only the deletes that will run leave the names after the step, the creates that will run join; a blocked create is dropped (`ref-name-conflict`) | `a_create_beside_a_delete_that_will_not_run_is_dropped` (MU3: `GIT_CONVERGE_INTERRUPTED`) | `MC_ec_neg_finish_after_all_deletes`, `MC_ec_refs_rename_op` |
| 9 | Rebase/bisect branch not in the skip set | Fixed | `checked_out_by` adds `rebase-merge/head-name`, `rebase-apply/head-name`, `BISECT_START` (`held_branches`) | `a_branch_under_a_rebase_or_bisect_counts_as_checked_out` (MU6), and the bisect half of the item 17 row | `MC_ec_neg_rebase_unguarded`, `MC_ec_rebase` |
| 10 | Ledger re-reads the index and exclude | Fixed | `publish_index` returns the digest it installed and `next_ledger` records it, with the exclude the converge wrote or proved; a first linked landing records `installed_index` (built from the capture in a scratch index file) | `the_ledger_records_the_installed_index_not_a_re_read` (MU9) | `MC_ec_neg_ledger_reads_index`, `MC_ec_index_concurrent` |
| 3 (finding 4) | Unborn HEAD untyped | Fixed | `head_of`: `rev-parse -q --verify HEAD`, exit 1 is an unborn HEAD (empty oid): the proof refuses `GIT_DESTINATION_MODIFIED`, a settle `GIT_CONVERGE_INTERRUPTED` | `an_unborn_head_refuses_modified` (pre-fix tree: `GIT_CHILD_FAILED`), `an_unborn_head_reads_as_no_commit` (MU7) | `MC_ec_neg_unborn_head_untyped`, `MC_ec_head_op` |
| 12 | Seat parent not a directory after the intent | Fixed | `Engine::run` maps `Seat::at`'s error to `GIT_CONVERGE_INTERRUPTED` (the settle skip for Remove, RmDir and DirMode kept) | `a_seat_parent_replaced_after_the_intent_refuses_interrupted` (pre-fix: `PATH_ESCAPES_ROOT`) | `MC_ec_neg_seat_parent_untyped`, `MC_ec_seat_parent` |
| 13 | Git ref renames not durable before the ledgers | Fixed | `update_refs` takes the ref names it writes and syncs the directories their renames changed (`ref_dirs`: each ref's and its reflog's directory, the git directory for HEAD, the common directory for `packed-refs`) before returning | `a_ref_write_syncs_the_directories_its_renames_changed` (MU11) | `MC_ec_neg_refs_unsynced` |
| 15 | Probe files left by a crash | Fixed | `apply_workspace` removes `.bulkload-converge-*-probe-{a,b}` under the lock (`sweep_probes`); probes are created with `O_EXCL` and `O_NOFOLLOW` | `probe_files_a_crashed_pass_left_are_removed` (pre-fix: left), `the_probe_sweep_removes_only_probe_files` (MU8) | `MC_ec_neg_probe_left` |
| 1 | File -> directory settle wedge | Fixed | `Op::to_dir`; a settling `Remove` whose seat already holds the new directory is done | `a_file_turned_directory_settles_after_an_interrupted_converge` (pre-fix: `GIT_CONVERGE_INTERRUPTED` forever), `a_file_turned_directory_marks_its_removal` (MU10), fault row `estate_converge_after_seat_mid` (the source's day now turns a file into a directory) | `MC_ec_file_to_dir_settle` (LiveSpec) passes; `MC_ec_neg_settle_remove_dir` fails `NoWedge` |
| 2 | Stale settle branch move | Fixed | every ref step skips the branches a pending workspace intent of any item names (`intent_branches`, in `Place::refs`, `plan_linked`, `apply_refs`); the settle re-plans (item 19) | `a_pending_intents_head_branch_is_left_to_its_item` (pre-fix: the linked item's step moved `other`, the main's settle refused forever) | `MC_ec_settle_stale_branch` (seeded simulation) passes; `MC_ec_neg_step_moves_intent_branch` fails `CrashAtomicity` (simulation) |
| 19 | Settle replays a HEAD-branch create a put-back ref blocks | Fixed | the workspace settle, HEAD still old, checks whether the intent's branch line can still apply (`branch_line_fails`: moved, deleted, checked out by another worktree, create beside an existing ref); if not it re-plans with `head_plan`, rewrites the intent, then `set_head` (`head-replanned`) | `a_settle_re_plans_a_head_branch_a_put_back_ref_blocks` (pre-fix: `GIT_CONVERGE_INTERRUPTED` forever) | `MC_ec_settle_branch_conflict` passes; `MC_ec_neg_settle_replays_branch` fails `TypedRefusal` |
| 3 | Git child lock leftovers; `set_head` one transaction | Fixed | `native_refs::settle` removes a lock file of its intent whose content proves it bulkload's (the op's new value; an empty delete lock older than a timestamp tick; `refs/stash.lock` holding a stash entry it stores; `packed-refs.lock` empty and old with a delete pending) and every HEAD lock marked by `HeadLocks`; the workspace settle removes `HEAD.lock` and the branch lock holding the intent's new values (`remove_head_locks`). Any other lock refuses typed. `set_head` commits the branch first in its own transaction under every worktree's HEAD lock with this HEAD verified, then HEAD (new fault point `estate.converge.head_branch` between) | `a_settle_removes_a_killed_childs_lock_and_keeps_anyone_elses` (MU4: refused forever; a killed stash store cleared the stash row), fault row `estate_converge_head_branch` | `MC_ec_child_crash_live` (LiveSpec), `MC_ec_set_head_partial` pass; `MC_ec_neg_stale_lock_kept` fails `NoWedge`, `MC_ec_neg_stale_stash_lock` `TypedRefusal`, `MC_ec_neg_set_head_one_txn` `CrashAtomicity` |
| 14 | Killed stash store | Fixed | item 3's lock removal; a store that meets `refs/stash.lock` refuses `GIT_CONVERGE_INTERRUPTED`, never clears the stash row | as item 3 | `MC_ec_stash_lock` |
| 4 | Stash settle undoes an operator's drop | Fixed | `RefIntent::stash_at`: each store's reflog is made durable in the intent; `stash_apply` continues only from it or one store past it (`stash_path`); anything else is the operator's, kept, `stash-modified-at-destination`, and the ledger's stash row names the reflog bulkload last landed (a cleared row read an emptied stash as never restored, and the next step restored it: found by the model, `MC_ec_stash_settle_revert`) | `a_stash_settle_never_undoes_an_operators_drop` (MU5b, the old any-suffix rule: re-stored the dropped entries; MU13, the row cleared: the next decision restored) | `MC_ec_stash_settle_revert` passes; `MC_ec_neg_stash_settle_any_suffix` fails `NoLocalWorkLost`; the stash ghost follows review 3 finding 14's rule (old is no longer bulkload's once a store is made) |
| 5 | Old HEAD branch stays stale | Fixed | after `set_head` (converge and settle) a ref step for the branch HEAD left alone (`Place::left_branch`; `native_refs::step`'s `only`); when its transaction fails (the operator checked it out again, another writer) the item still lands and the ref intent waits for the next apply (`ref-left-branch-deferred`): a refusal there left HEAD moved under a pending workspace intent, and its settle moved HEAD off a branch the operator switched back to (found by the model, `MC_ec_head_concurrent`) | `the_branch_head_left_converges_with_it` (pre-fix: `main` stayed at its landed value), `the_branch_head_left_waits_for_a_writer_without_refusing` (MU12: refused), fault rows (the source's day switches to `side`) | `MC_ec_old_head_branch` (LiveSpec) passes; `MC_ec_neg_old_head_branch_skipped` fails `ConvergesWhenQuiet` |
| 7 | #216 residual: crashed first linked landing | Fixed | `restore_linked_staged_landed` writes a `LandingIntent` (common ledger directory, keyed by the destination) before `git worktree add` and removes it after the ledger; `apply_workspace` finds a worktree without a ledger that a landing intent names and finishes it forward under the lock (`recover_linked_landing`: HEAD must be the planned one; each capture seat absent is written, its bytes or a prefix of them (bulkload's cut-short write) are kept or rewritten, anything else is `GIT_DESTINATION_MODIFIED`); new fault point `estate.converge.linked_added` | fault row `estate_converge_linked_added` (pre-fix tree: no landing intent, so the rerun refuses `GIT_DESTINATION_OCCUPIED`) | `MC_ec_first_linked_crash` (LiveSpec) passes; `MC_ec_neg_no_landing_intent` fails `NoWedge` |
| new | A settle moves HEAD off a branch the operator switched back to after `set_head` (found by the model while re-checking these fixes) | Fixed | `WorkspaceIntent::head_done`, written right after `set_head` (`head_done`); a settle with it set refuses any HEAD but the new one; a HEAD transaction that fails after the branch's while HEAD already reads as new is done | fault row `estate_converge_after_head_switch_back` (MU14: the settle moved HEAD back to `side`) | `MC_ec_head_concurrent_crash` passes; `MC_ec_neg_head_not_recorded` fails `NoLocalWorkLost` |
| 6 | Detached HEAD never re-attaches on a no-op | **Documented limit** | none | none | `MC_ec_finding_detached_reattach` (fails `ConvergesWhenQuiet`, as a limit) |
| 18 | Restored ref never re-planned on an A4 no-op | **Documented limit** | none | none | `MC_ec_finding_restored_ref` (fails `ConvergesWhenQuiet`, as a limit) |
| 20 | Transaction and HEAD compare-and-swap | Already right | none | existing | `MC_ec_neg_txn_no_cas`, `MC_ec_neg_head_no_cas` |

**Why 6 and 18 stay limits.** Both are liveness only: nothing is lost,
overwritten or refused; the HEAD stays detached with its branch recorded,
the ref stays at the operator's put-back value (equal to bulkload's
landed one), and the next changed capture converges both (its converge
re-runs the attach rule and plans every ref). Fixing them needs the
journal no-op (A4) to read the workspace ledger and run the attach rule or
a ref step on every rerun of an unchanged source, which spends S3's zero
read budget on every no-op apply and which no ruling allows. Operator
decision asked: allow a metadata-only A4 check (ref ledger and
`intended_branch` only), or rule both known limits.

**Further stated limits of the fixes.** A lock left empty by a Git child
killed between creating and writing it (a create or update; Git writes a
delete's lock empty, which an age rule covers), and a `packed-refs.lock`
with content, are not provably bulkload's: the settle refuses
`GIT_CONVERGE_INTERRUPTED` naming no lock until the operator removes it.
HEAD locks bulkload holds for a transaction refuse a concurrent `git
switch` for that moment; one a crash leaves is marked and removed by the
next settle. `git bisect start` with no commit, between the HEAD-locked
read and the transaction, is not excluded. Reftable repositories are not
covered by the lock recovery (bulkload creates files-backend repositories).

**Fault harness.** Two points (`estate.converge.head_branch`,
`estate.converge.linked_added`, `Point::ALL` 38) and their rows; the
source's day now switches HEAD to a moved branch and turns a file into a
directory, so the existing rows cover set_head's two transactions, the
left branch and the file-to-directory settle; a seat whose kind changes
may be absent between its removal and addition (as the model's
`CrashAtomicity`). A new scenario, `LinkedLanding`, crashes a first landing
of a main and its linked worktree after `worktree add` and checks the
rerun finishes it forward and equals a fresh restore.

**Found while re-checking the model against the fixed code.** Four
interactions the first runs of the new rows showed, each fixed in both the
code and the model before the final run: the settle's re-plan counted the
item's own checked-out branch as "checked out elsewhere" (a stutter loop
in `MC_ec_child_crash_live`; `branch_line_fails` now takes the other
worktrees' branches, and a re-plan that changes nothing is not written);
the stash row cleared after an operator's change made the next step
restore the stash (above, item 4); the left-branch step's refusal (above,
item 5); and a settle that moved HEAD again after `set_head` (above, the
`head_done` row, found by a probe of `MC_ec_head_concurrent` with a crash,
now the pass row `MC_ec_head_concurrent_crash`). The settle's re-plan also removed the wedge of the old
mutation `head_branch_in_step`; its row now names the unjustified
`GIT_CONVERGE_INTERRUPTED` (`TypedRefusal`) the mutation still causes, and
the mutation skips neither the HEAD branch nor its own intent's.

**Two repairs outside the findings.** `destination_git_ready` probes the
Git version with `run_git` directly, not through `output`: a test's
injected child fault (`scratch_fault::Site::Child`) taken by that probe
was cached for the process, so `cargo test --lib estate::s4_sweep_tests`
alone failed 7 rows with `GIT_UNAVAILABLE` (the same 7 on the pre-fix
tree; the whole-crate run passed only by test order). And the recovery's
`read-tree` goes through the registered `read_tree` writer (the S2 command
registry's ceiling of 6 `read-tree` uses).

**Validation (2026-10-09, in `nix develop`, git 2.54, lane target dir
`/srv/fast-local/jess/cache/cargo-target/converge`, host load 60 to 100).**

- Before the fixes: the 7 new integration rows against the pre-fix tree
  (`git archive 5b8e550` plus the new test file) all failed
  (`GIT_CHILD_FAILED`, probes left, `GIT_CONVERGE_INTERRUPTED` forever for
  file -> directory, `main` left at its landed value, `PATH_ESCAPES_ROOT`,
  `GIT_CONVERGE_INTERRUPTED` forever for the put-back `other`, `other` moved
  by the linked item); the extended fault harness on the pre-fix tree
  failed 17 of 18 converge rows (the left branch, the file -> directory
  wedge, no landing intent, the `head_branch` point never reached); each
  unit row failed against its mutation (MU1 to MU14, `logs/mutation-
  evidence.log`: every one "FAIL (as required)").
- After: `cargo fmt --all -- --check` clean; `cargo clippy --workspace
  --all-targets --locked -- -D warnings` clean, the `io-trace` pass clean,
  the `fault-injection,io-trace` pass clean; `cargo test -p bulkload-agent
  --locked`: lib 607 passed, 5 ignored, every integration binary ok
  (`estate_converge` 19, `source_command_registry` 16, `refusal_taxonomy`
  5, ...); `--lib estate::s4_sweep_tests` 14 passed; `--features
  fault-injection,io-trace --test fault_harness` 79 passed, `--test
  power_loss` 12 passed; `python3 tests/test_ci_contract.py` 24 OK; TLC:
  all 74 `MC_ec_` rows matched (formal note).

## 21. Review 4 of the fixed code and model (2026-10-09, later)

Lane: the convergence lane, worktree
`bulkload.worktrees/estate-converge-20261008`, branch
`feat/estate-converge-20261008` at `5b8e550` with section 20's fixes and
these staged. Rulings cited: OI-1003-Q144, OI-1003-Q32 and Q43, OI-1003-Q7,
R-N11 (only processes this lane started; none signalled), R-N98 (fixture
repositories only), R-N104 (no PID liveness check: the live-writer row
polls for its lock file and reaps its own child), R-N13 (this section and
the formal note's "Review 4"). Nothing committed or pushed.

Thirteen findings; twelve fixed in code (findings 1 and 8 are one defect),
finding 11 a stated limit. Model side: the formal note, "Review 4".
Evidence before the fix: the new integration and fault-harness rows run
against the tree as review 4 found it (`git checkout-index` of the staged
tree before any edit, plus the new test files; `logs/r4/prefix-*.log`), and
each unit row against a mutant that puts that code back
(`logs/r4/mutation-evidence-r4.log`, MU15 to MU21).

| # | Finding | Code | Rust row (evidence before the fix) |
|---|---|---|---|
| 1, 8 | Settle or `set_head` attaches HEAD to a branch another worktree has checked out (blocker) | `set_head`: when it attaches HEAD to a branch it is not on, every other worktree's `HEAD.lock` is held from before the branch transaction to after HEAD's, and the branch checked out or held by another worktree, read under them, refuses `GIT_CONVERGE_INTERRUPTED` (with or without a branch line; the branch transaction itself also holds this worktree's lock). `settle`: `branch_line_fails` checks the other worktrees first; a HEAD whose target another worktree took since the intent re-plans even with no branch line (`taken`), detached, `head-detached-ref-conflict`; a done branch line keeps its ledger row | `set_head_never_puts_a_second_worktree_on_a_branch` (MU18: attached, two worktrees on `side`); fault row `estate_converge_head_branch_double_checkout` (pre-fix: the rerun attached the checkout to `side` beside the operator's `git worktree add`) |
| 2 | `recover_linked_landing` discarded the operator's staged index and overwrote a prefix | the index must have no entries (`empty_index_digest`) or be `installed_index`'s, else `GIT_DESTINATION_MODIFIED`; every check runs before any write; the prefix rule is gone: a linked landing writes each entry under `.bulkload-landing-temporary` and renames it into place without replacing (`restore_entries_with`), so a file at an entry's name that is not its blob is the operator's; the recovery removes that temporary; its own killed `read-tree`'s `index.lock` (whose entries are the installed index) is removed, any other refuses INTERRUPTED; `read-tree` runs only if the index is not already the installed one | fault row `estate_converge_linked_added_operator` (pre-fix: `workspace-restored`, `mine.txt` unstaged, `f1` overwritten) |
| 3 | Stash reflog never fsynced | `update_refs` fsyncs `logs/refs/stash` (`ref_files`, `sync_ref_files`) before the directory syncs, so before `record` and the ledger | `a_stash_store_syncs_its_reflog` (MU17). Not power-loss traced (Git's syscalls are a child's) |
| 4 | Empty `HEAD.lock`/`index.lock` after a power loss | `native_refs::publish_lock`: the marker goes to `bulkload-lock-<pid>-<n>` in the git directory, is synced, and is renamed to the lock without replacing (the link fallback where there is no exclusive rename): the lock never exists without its marker. `HeadLocks` and `publish_index` use it; `sweep_lock_temps` removes a crash's temporaries (`settle_repository`) | `a_lock_bulkload_takes_holds_its_marker_from_the_moment_it_exists` (MU16: the lock existed empty) |
| 5 | A killed `read-tree` left `bulkload-index-<tag>-fresh.lock`: `GIT_CHILD_FAILED` on every rerun | `publish_index`'s cleanup removes `fresh.lock` and `merged.lock` too; `installed_index` uses `bulkload-index-landing` per worktree, removed with its lock before and after; `after_intent` maps every `GIT_CHILD_FAILED` after a workspace intent (converge and settle) to `GIT_CONVERGE_INTERRUPTED` | fault row `estate_converge_index_locked_fresh_lock` (pre-fix: `GIT_CHILD_FAILED stderr_class=other`) |
| 6 | `packed-refs.new` left by a killed delete wedged | `remove_orphan_packed_new` in `remove_stale_locks`: with deletes or a stash op pending and no `packed-refs.lock`, the orphan is removed (lock checked first; Git creates the file exclusively, so no writer can make one while it stays) | `an_orphan_packed_refs_new_is_removed_once_no_lock_holds_it` (MU20); fault row `estate_converge_after_ref_intent_packed_new` (pre-fix: the empty `packed-refs.lock` was removed by age, `.new` stayed) |
| 7 | A create beside an operator's symbolic ref never demoted | `plan_names` and `finish` count the destination's symbolic refs among the names after the step; `head_plan` and `branch_line_fails` among the names a branch conflicts with | `a_create_beside_a_symbolic_ref_is_dropped` (MU21); `a_create_beside_an_operators_symbolic_ref_is_dropped` (pre-fix: `GIT_CONVERGE_INTERRUPTED`) |
| 9 | A killed `set_head`'s `HEAD.lock` beside a pending ref intent wedged | `converge::settle_repository` replaces the bare `native_refs::settle` in `apply_workspace`, `apply_refs`, `settle_refs` and `plan_linked`: first, for every worktree with a pending workspace intent, the HEAD and branch locks holding that intent's new values (`remove_intent_head_locks`), the marked HEAD locks and lock temporaries; then the ref settle | `a_killed_set_heads_lock_beside_a_pending_ref_intent_settles` (pre-fix: `GIT_CONVERGE_INTERRUPTED` forever) |
| 10 | Settle removed live Git locks ("empty and older than 2 s") | `remove_owned_lock` removes only a lock whose content proves it bulkload's; an empty lock is never removed, `packed-refs.lock` never. The transaction that meets one refuses `GIT_CONVERGE_INTERRUPTED` until its owner finishes or the operator removes it, as Git instructs (stated limit; `NoWedge`'s exception in the model) | `an_empty_lock_is_never_removed_even_when_old` (MU19: a live `update-ref --stdin` session's `packed-refs.lock` removed) |
| 11 | `git switch` reads the branch before `HEAD.lock` | stated limit, `transact`'s doc comment and below | none (no lock closes it) |
| 12 | No Rust row pins `set_head`'s two transactions | a test hook over `update_refs` (`UPDATE_REFS`, test builds only) plays Git's killed child: given HEAD's line and the branch's in one transaction it commits HEAD's (Git renames `HEAD` first) and fails | `set_head_never_commits_head_without_its_branch` (MU15: HEAD on `side` with `side` at its old value) |
| 13 | Unseeded negative simulation | catalogue only (formal note) | n/a |

**Stated limits added (review 4).**

- An empty lock is never provably bulkload's: a delete's ref lock, a
  stash rewrite's `refs/stash.lock`, `packed-refs.lock`, and the empty
  `HEAD.lock` Git takes for the HEAD reflog when `set_head` moves the
  branch HEAD stays on (a log-only update; `strace`, git 2.52: created,
  never written, unlinked: `logs/r4/strace-head-log-only-lock.log`). One a killed Git child left
  makes the item refuse `GIT_CONVERGE_INTERRUPTED` until the operator
  removes it, as Git's own message instructs; the next rerun then settles
  (and removes an orphan `packed-refs.new`). A live writer's is never
  touched.
- `HEAD.lock` serialises only the final HEAD write of a `git switch`
  (finding 11): a ref transaction that moves a branch after a switch read
  it and before the switch writes HEAD leaves that worktree on the branch
  over its old commit's files and index. Not closed by any lock (the
  switch commits the index before it locks HEAD); not modelled.
- A power loss between Git's rename of `refs/stash` and bulkload's fsync
  of its reflog can still leave the tip ahead of the reflog; the settle
  then reports `stash-modified-at-destination` and keeps the stash (its
  entries stay at provenance).
- `recover_linked_landing` reads an absent capture seat as not yet
  written: a capture file the operator removed (or `mv`'d away without
  Git) before the recovery is written again. Any index change refuses.
- An operator's write into a linked worktree while its first landing
  runs (between `worktree add` and the index) is not checked.
- A `read-tree` killed while it wrote a linked worktree's `index.lock`
  leaves a lock whose entries do not read back as the installed index: the
  recovery refuses `GIT_CONVERGE_INTERRUPTED` until the operator removes
  it (one it finished writing is removed).
- Lock temporaries (`bulkload-lock-*`) are swept under the destination
  lock of one SOURCE label; two labels applying into one repository at
  once could remove each other's in-flight temporary (the publish then
  refuses typed).

**Validation (2026-10-09, later; in `nix develop`, lane target dir).**
Host load 70 to 110 on 32 cores; logs under the lane's cache,
`/srv/fast-local/jess/cache/cargo-target/converge/logs/r4/`.

- Before the fixes (the tree review 4 found, `prefix-int.log`,
  `prefix-fault.log`): both new integration rows and all four new
  fault-harness rows failed: `a_killed_set_heads_lock_beside_a_pending_ref_intent_settles`
  (refused), `a_create_beside_an_operators_symbolic_ref_is_dropped`
  (refused), `estate_converge_head_branch_double_checkout` (the rerun
  converged with no re-plan, attaching the checkout to `side` beside the
  operator's worktree), `estate_converge_linked_added_operator`
  (`workspace-restored ... landing-recovered=1` over the operator's work),
  `estate_converge_index_locked_fresh_lock` (`GIT_CHILD_FAILED
  stderr_class=other`), `estate_converge_after_ref_intent_packed_new` (the
  empty `packed-refs.lock` removed by age). Each unit row failed against
  its mutant (`mutation-evidence-r4.log`): MU15 `set_head_one_txn`, MU16
  `lock_written_after_create`, MU17 `stash_reflog_unsynced`, MU18
  `head_attach_unchecked`, MU19 `empty_lock_removed`, MU20
  `packed_new_kept`, MU21 `symref_names_unplanned`: every one "FAIL (as
  required)".
- After (`final/`): `cargo fmt --all -- --check` clean; `cargo clippy
  --workspace --all-targets --locked -- -D warnings` clean, the
  `io-trace` pass clean, the `fault-injection,io-trace` pass clean; `cargo
  test -p bulkload-agent --locked`: lib 614 passed, 5 ignored, every
  integration binary ok (`estate_converge` 21, `source_command_registry`
  16, `refusal_taxonomy` 5, ...); `--lib estate::s4_sweep_tests` 14
  passed; `--features fault-injection,io-trace --test fault_harness` 83
  passed, `--test power_loss` 12 passed; `python3
  tests/test_ci_contract.py` 24 OK. TLC: the formal note, "Review 4".
