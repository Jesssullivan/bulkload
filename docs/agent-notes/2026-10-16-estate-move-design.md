# 2026-10-16: `estate-move` design (M2 item 7)

Design only: nothing here is built. Lane: M2 "bulkload is one command"
(plan of record `2026-10-16-plan-of-record.md`, fix 7), worktree
`bulkload.worktrees/m2-move-design-20261016`. Sources read (read-only): the
in-place lane's scripts `inplace-20261016/scripts/{presence.py, probe.py,
neo-inplace-capture.sh, sting-apply.sh, sting-apply2.sh, refs-in.py,
sting-carry.sh, sting-pull-resume.sh, watch-and-land.sh}` and its `plan/`
tables (`presence.tsv`, `groups.tsv`, `group-verdict.tsv`, `items-i0*.tsv`,
`refs-in.tsv`, `carried-i01.tsv`, `i01-premake-dirs.txt`). Rulings cited:
the plan of record's axioms (same paths; nothing excluded; never
overwrite, differences beside as `<path>.neo-YYYYMMDD` and refs as
`neo-YYYYMMDD/*`; never halt the source; done is the resume test; every
item accounted), Q185/Q192, Q194 C10/C11/C14, OI-1003-Q171/Q172 (adopt),
Q183 (hand-off), Q198 (re-seat), R-N11, R-N13.

## 1. What the lane did by hand

| Step | Script | What it did | Shape of the decision |
| --- | --- | --- | --- |
| P0 presence | `presence.py` | For each source item, where its live path stands on the target: `absent`, `git:ok`/`git:broken`, `link:ok` (a symlink, often into `/srv/data/.../archive`), `nongit` | 611 absent, 240 git, 61 link, 22 nongit |
| P1 probe | `probe.py` (both hosts) | Read-only Git state per path: HEAD, symbolic HEAD, stash reflog digest, heads and tags, index digest, dirty digest | JSONL per host |
| P2 groups | (inline) | Group = a main and its worktrees; group shape from presence | `ALL-ABSENT` 60, `MAIN-PRESENT-ONLY` 191, `MAIN-PRESENT-SOME-WTS-PRESENT` 30, `MAIN-PRESENT-WTS-ABSENT` 33, `MAIN-nongit` 10 |
| P3 verdict | (inline) | Per present main: `ADOPT` when the probes agree (`EQUAL`, or `refs+`: only extra source refs), else `BESIDE` with the differing components (`head,sym,index,dirty,stash,refs+`) | 176 adopt, ~70 beside |
| P4 plan | `items-i02.tsv` | One row per item with a tag: `adopt` 218, `refs-only-adopt` 70, `adopt-bare` 2, `beside-main` 70, `beside-wt` 20, `new-wt` 498, `beside-group(nongit)` | live repo/workspace columns already resolved (symlinks to their real path, beside paths suffixed `.neo-20261016`) |
| P5 capture | `neo-inplace-capture.sh` | `estate-add-batch` then `estate-capture` on the source, `--priority=normal`, a manifest of the corpus | per batch `iNN` |
| P6 carry + apply | `sting-apply.sh`, `watch-and-land.sh` | `pull` the corpus, verify the manifest, `estate-diff` (preview), `estate-apply --adopt --activate-origin ...`, `closure-report` | polled the source for `capture.rc` |
| P7 refs-in | `refs-in.py` | For every adopted live repo: each source head or tag whose tip is absent or different there, and whose object is present, gets a create-only ref `refs/heads/neo-20261016/<name>` (`refs/tags/neo-20261016/<name>`); never moves or deletes a ref | rows `created`, `exists`, `MISSING-OBJECT` |
| P8 carried-as-files | `sting-carry.sh` | Items Git carry refused (`GIT_SOURCE_PARTIAL_CLONE` 10, `GIT_INVENTORY_MALFORMED` 1): `pull` the tree as files to the live path, skipped when the path is non-empty | Q185/D6 |
| P9 parents | `i01-premake-dirs.txt` | `mkdir -p` of 7 absent parents (42 of 104 items refused `GIT_DESTINATION_PARENT_MISSING`) | M2 item 1 makes this native |
| P10 sessions | `sting-pull-resume.sh` | `pull --sqlite=snapshot` of the agent state trees into staging | then `estate-reseat` and `estate-handoff` |

Every step's decision is a pure function of facts the earlier steps
recorded, plus the operator's date suffix. That is what makes it one verb.

## 2. The verb

```text
bulkload-agent estate-move [--date YYYYMMDD] [--plan-only] [--jobs N]
    [--map SRC_PREFIX=LIVE_PREFIX]... [--activate-origin PREFIX]...
    [--sessions TREE=DEST]... [--no-handoff] [--no-reseat]
    SOURCE_HOST SOURCE_ROOT... MOVE_STATE
```

Run on the destination. `MOVE_STATE` (0700, the operator's) holds
everything the move decides and does: the plan, the corpus, the apply
STATE, the pull states, and `move.ledger` (below). The source side runs
only verbs that exist (`git-carry-estimate`'s probe, `estate-capture`,
`serve`) through the native SSH transport, at `--priority=auto` with the
bounded-starvation watcher (M2 item 3). It never halts the source.

### 2.1 Phases, each a resumable step recorded in `move.ledger`

1. **Inventory (source, read-only).** The remote estimate probe lists every
   repository and linked worktree under each `SOURCE_ROOT` (mains, linked
   worktrees, bare repositories, nested repositories as their own items,
   R-N114). `--map` turns each source path into its live path (default:
   the same path under the destination home). Output: inventory rows.
2. **Presence probe (destination, read-only).** For each live path:
   `absent`, `git(main|linked|bare, ok|broken)`, `link(real path, in
   policy?)`, `nongit`, and the parent shape (M2 item 1: creatable, or
   refused under a symlink or file). A symlinked live path is resolved to
   its real path, and that real path is what the plan names (the adopt
   rule refuses a symlinked spelling, OI-1003-Q171/Q172).
3. **Capture (source).** `estate-add-batch` and `estate-capture` of the
   inventory, then a native `pull` of the corpus into `MOVE_STATE/corpus`
   with its manifest checked. Refusals that Git carry cannot take natively
   are recorded per item (`GIT_SOURCE_PARTIAL_CLONE`,
   `GIT_INVENTORY_MALFORMED`: M2 item 6 shrinks this set).
4. **Classification (destination, read-only).** `estate-diff` of the
   capture against the live tree gives each checkout one class:
   - `absent`: nothing at the live path (`source-only`, `landing`);
   - `match`: `identical`, `adoptable`;
   - `diverged`: `differs` (any component: head, index, exclude, seats,
     stash), or `refs+` only (extra source refs: still `match` for the
     checkout, the refs go through refs-in);
   - `nongit`: a directory that holds no repository, or an occupied shape
     (`occupied-foreign`, `symlinked-path` after resolution failed).
   Group verdict: a main and its worktrees take one row each, and the
   main's class decides where the group's new worktrees go.
5. **Plan (pure).** One action per item, recorded with its reason:

   | Main | Item | Action | Lands at |
   | --- | --- | --- | --- |
   | absent | main | land | live path (parents made) |
   | absent | linked | new worktree | live path, after the main |
   | match | main | adopt | live path, in place (`--adopt`) |
   | match | linked absent | new worktree | live path |
   | match | linked match | adopt | live path |
   | match | linked diverged | beside worktree | `<path>.neo-YYYYMMDD`, a linked worktree of the live repo |
   | diverged | main | refs-only adopt + beside main | live repo keeps its checkout; the source checkout lands standalone at `<path>.neo-YYYYMMDD` |
   | diverged | linked absent | new worktree | live path, in the live repo |
   | diverged | linked present | beside worktree | `<path>.neo-YYYYMMDD` |
   | nongit | any | beside group | the whole group at `<path>.neo-YYYYMMDD` |
   | bare absent | bare | bare landing | live path |
   | bare present | bare | refs-only adopt | live path |
   | (refused) | any | carried as files | live path when absent, else `<path>.neo-YYYYMMDD` |

   `--plan-only` stops here and prints the plan and the counts per action
   (the lane's `items-i02.tsv` and its tag histogram).
6. **Apply.** `estate-apply --adopt --activate-origin ...` of the plan, in
   its levels (mains, then linked worktrees, refs-only items by their
   repository path, M2 item 1 review). The apply's own refusals stay typed
   and are recorded; nothing is overwritten.
7. **Refs-in.** For every live repository the plan adopted or refs-only
   adopted: each source head and tag whose tip the live repository lacks or
   holds at another value, and whose object it now holds, is created as
   `refs/heads/neo-YYYYMMDD/<name>` (`refs/tags/neo-YYYYMMDD/<name>`). One
   `update-ref` transaction per repository with an all-zero old value for
   each ref (create-only), under the destination lock; an existing ref of
   that name is kept and reported `exists`; a missing object is reported
   `missing-object` (the capture should have carried it: a defect, not a
   choice). Recorded in the repository's ref ledger as a new row kind, so a
   converge never moves them and a rerun is a no-op. This replaces
   `refs-in.py`.
8. **Carried as files.** Each item the capture refused and Git carry cannot
   take is `pull`ed as plain files (`--sqlite=snapshot`) to its live path
   when that path is absent, else beside, with its own pull state under
   `MOVE_STATE/files/<item>`. Never into a non-empty path. Recorded as
   `carried-as-files` with the refusal it stands in for, so closure counts
   it accounted, not applied.
9. **Sessions, re-seat and hand-off.** `--sessions` pulls the agent state
   trees (`pull --sqlite=snapshot`), then `estate-reseat --lanes-from-plan`
   keys them to the live paths (Q198), then `estate-handoff` writes the
   block in each landed workspace (Q183) unless `--no-handoff`.
10. **Converge loop.** The whole move is rerunnable: a rerun repeats phases
    1 to 9 and each is a delta (captures reuse, pulls read nothing settled,
    applies converge landed checkouts, refs-in creates only new refs). The
    verb ends when a pass changes nothing (every phase reports 0 new work)
    or after `--passes N`; `closure-report` over the move must say 0
    unaccounted. The resume test (axiom 5) stays the operator's.

### 2.2 `move.ledger`

A per-move record (postcard, temporary + fsync + rename, like the other
records): the date suffix fixed at the first plan (a rerun never re-dates a
beside path), the inventory digest, and per item its class, action, live
path, reason and phase reached. A phase's record is written only after what
it describes is durable. A changed classification on a rerun (a live
checkout the operator edited between passes) never changes a landed item's
action: an adopted item that now differs is converged or refused by the
apply as today, never moved beside after the fact.

### 2.3 What stays out

- No deletion anywhere (Reclaim is its own verb).
- No checkout of the live repository is switched, reset or rewritten;
  refs-in only creates refs in a dated namespace.
- No guessing of intent: a class the plan table does not cover (a
  `git:broken` main, a symlink out of policy) is `refused` with its reason,
  and `--plan-only` shows it before anything is written.

## 3. Proofs to build with it

- P-MOVE-1: the lane's shapes as a fixture (absent, match, refs+, diverged
  main with absent and present worktrees, nongit, bare absent and present,
  a partial clone): one `estate-move` lands every item, the tag histogram
  equals the table above, closure 0 unaccounted.
- P-MOVE-2: an unchanged rerun reads 0 source bytes and writes nothing; a
  source change converges in place; the date suffix never changes.
- P-MOVE-3: no live ref moves or disappears, no live seat changes `lstat`
  identity (the adopt proofs' freeze), across every phase.
- Fault rows: a crash after each phase's work and before its ledger row is
  settled forward by the rerun.

## 4. Questions for the operator

1. The beside suffix: `.neo-YYYYMMDD` (the run's) or `.<source-host>-YYYYMMDD`
   (a second source machine, M2's exit criterion)?
2. A diverged linked worktree: beside as a linked worktree of the live
   repository (shares its objects and refs; today's `beside-wt`) or as a
   standalone repository?
3. Refs-in for tags: `refs/tags/neo-YYYYMMDD/<name>` as the lane did, or no
   tags (they rarely differ)?
4. Should `estate-move` drive the source over SSH itself (one command), or
   split into a source half and a destination half for hosts where the
   destination cannot reach the source?
