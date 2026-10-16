# 2026-10-16 estate hand-off block in AGENTS.md and CLAUDE.md (TIN-6569), design

Lane: estate-handoff-20261016. Worktree
`bulkload.worktrees/estate-handoff-20261016`, branch
`feat/estate-handoff-20261016`, off `3fe1f07` (the signed #226 adopt
commit, on main `5beb031`).

**Status: built, then reviewed and fixed** (section 17, "As built",
records what differs from the design below and why; section 18 is the
review of 2026-10-16 against `9ec8d36` and what changed for each
finding; section 19 is the two re-reviews against `7c5407f` and the
operator's rulings OI-1003-Q188 to Q191; section 20 is re-review 3
against `2b18a03`). Sections 0 to 16 are the
design as written before the build; code citations there are to
`3fe1f07`. Where a later section contradicts an earlier one, the later
one is current.

Rulings this serves:

- **OI-1003-Q183 (2026-10-16), Linear TIN-6569.** Quoted from the
  workflow's task text. The relayed operator request in this lane was
  truncated ("(in linear0"), so the ruling's text is the task's quote,
  not checked against Linear here. The operator's words, as quoted: the
  GloriousFlywheel beside-landing "should include (perhaps as a bulkload
  builtin) an addendum with abspaths to AGENTS.md and CLAUDE.md pointing
  to the known, dated, specific details of all paths, high level diffs
  etc for agents (from top of both files), maybe even README as well —
  prioritize work on converging and consolidating completely before they
  proceed with any other work with the user. This way we can ensure
  bulkload handoff is fully heads up."
- OI-1003-Q144 and Q149: convergence, and the rule that a local change
  is never overwritten (S5's `NoLocalWorkLost`).
- OI-1003-Q171 and Q172 (#226): adopt and `estate-diff`, whose machinery
  this reuses.
- OI-1003-Q1 (S4), Q6 and Q10 (S3), Q2 (S5).
- R-N98: only the product's own tests disable hooks, and only in fixture
  repositories.
- R-N13.
- **OI-1003-Q185 (2026-10-16)**, relayed in the review task: nothing is
  ever excluded from a carry. The hand-off never strips or drops user
  content; the privacy concern is only that the block (absolute paths,
  host names) is never committed or pushed upstream. It withdraws the
  capture strip of section 9.6 (see 18.3).

## 0. Key decisions

1. **A new verb, `estate-handoff`.** It renders, re-renders, checks
   (`--check`) and clears (`--clear`). `estate-apply` has no new flag.
   When a workspace already holds a hand-off record, `estate-apply`
   re-renders the block after it converges that workspace (§6.4). Having
   the record is the opt-in.
2. **The block is prepended at byte 0 of `AGENTS.md` and `CLAUDE.md`, as
   the operator asked.** `README.md` is opt-in with `--readme`, off by
   default, and is never created.
3. **The block is a bulkload-owned overlay, recorded in its own
   ledger.** The ledger is `handoff.landed`, beside `workspace.landed`.
   The workspace ledger is never rewritten for it. Converge accepts a
   seat whose bytes are either the landed blob, or the recorded block
   followed by the landed blob. The block is bulkload's only while it is
   byte-identical to its record (blake3 and length). Any edit inside it
   makes the file the user's, and the normal rules apply.
4. **Converge treats the block as disposable bulkload bytes, never as
   work.** When the source changes the file, the converge's exchange
   drops the old block with the displaced file. The re-render then puts
   a new block on top of the new body. No seat is refused because of a
   block that is intact.
5. **A missing target is created**, recorded as `created`. A symlink is
   never written through. When `CLAUDE.md` is a symlink to `AGENTS.md`
   in the same worktree, it counts as covered. Any other symlink, or any
   file that is not regular, refuses the whole workspace with a typed
   code and nothing written.
6. **Privacy is default-deny.** The verb writes only into a workspace
   whose repository has no remote, or whose every remote URL and push
   URL starts with an operator-given `--private-remote PREFIX`. The other
   way in is an explicit `--allow-workspace PATH`. Remote URLs are never
   printed. A commit of the block is detected, not prevented: `--check`
   reads the index blobs and the HEAD blobs of the target files. A hook
   is installed only with `--commit-guard`, which is opt-in and lands in
   PR 3. Bulkload's own capture strips an intact recorded block (PR 3).
7. **An unchanged rerun reads 0 seat bytes and writes 0 bytes.** Its
   inputs digest is built from metadata only. When that digest is
   unchanged, the diff machinery is not run at all.
8. **The TLA model changes.** The proof, the settle classification and a
   new hand-off write each change a modelled step. Every existing row
   renders a new constant `Handoff` FALSE, so its state space stays the
   same.

## 1. The problem in the code

Three facts about `3fe1f07` shape the design:

- **A tracked file the hand-off edits is a landed seat.** Every carried
  seat (tracked, untracked and ignored) is in `LandedWorkspace.seats`
  (`landed.rs:432`).
- **An unchanged rerun never looks at it.** `estate-apply`'s A4 is the
  journal no-op, with no proof and no read. So a naive prepend survives
  every rerun until the source changes something.
- **A changed capture proves every seat** (A10, and A6 for a ledger
  ahead of its journal; `apply_workspace`, `converge.rs:1992`). `prove`
  (`converge.rs:1131`) tries the print first. When the print has moved
  it reads the content (`holds`, `converge.rs:1043`). A prepended file
  matches neither. The item then refuses `GIT_DESTINATION_MODIFIED`
  until someone removes the block by hand. `estate-diff`'s ledgered rows
  call the same `prove`, so they report the item `diverged`.

Three more places compare a seat with its landed blob by content:

- the settle mode of the seat engine (`Engine::expected`, the
  `op.old` branch);
- `Engine::sweep`'s "ours" test on a displaced temporary;
- converge's add-clash check: a third party's file where the source adds
  one (`converge`, the `Action::Add` filter).

A `created` hand-off file would hit the add-clash check if the source
later adds the same path.

What the hand-off never touches:

- the index (the index entries digest is unchanged, because nothing is
  staged);
- HEAD;
- `info/exclude` (its digest is part of the proof for a standalone
  item);
- configuration;
- refs.

## 2. Alternatives compared

| | (A) Ledgered prepend (chosen) | (B) Untracked sidecars: `CLAUDE.local.md` plus `AGENTS.override.md` | (C) Prepend plus `skip-worktree` on the target | (D) A thin pointer line, with the content in STATE |
|---|---|---|---|---|
| Meets "top of AGENTS.md and CLAUDE.md" | Yes | No. Codex's `AGENTS.override.md` replaces `AGENTS.md` rather than adding to it, so it must copy the whole file, and that copy goes stale at every converge. Claude reads `CLAUDE.local.md`, but other harnesses do not. | Yes | Partly: one line at the top, with the details elsewhere |
| Destination edits | Two tracked or untracked files, plus a record under `.git/bulkload` | Two new untracked files. Keeping them out of `git add -A` needs `info/exclude`, which the proof includes. | The index (the flags are in the index digest), so the index proof fails and the item refuses MODIFIED | Same as (A), but smaller |
| Converge interaction | One overlay predicate (§6) | The add-clash rule only | Index proof fails on every converge; `checkout` and `pull` silently skip or overwrite the file | Same as (A) |
| Commit risk | `git commit -a` would include it; detected by `--check` | `git add -A` would include it | Hidden from `git status`, so the block is also hidden from the agents the operator wants to warn | Same as (A), less content |
| Verdict | Default | Rejected: not what was asked | Rejected: changes the index and hides the warning | Rejected as the default, because the operator asked for the details at the top. Kept as the overflow: the estate-wide index lives in STATE (§3.3). |

## 3. The verb

### 3.1 CLI

```
estate-handoff [--item ID]... [--workspace PATH]...
               [--sibling LANDED_PREFIX=LIVE_PREFIX]...
               [--private-remote PREFIX]... [--allow-workspace PATH]...
               [--readme] [--renew] [--commit-guard] [--dry-run]
               PLAN CORPUS PRIVATE_STATE SOURCE
estate-handoff --check [--item ID]... [--workspace PATH]... PLAN CORPUS PRIVATE_STATE SOURCE
estate-handoff --clear [--item ID]... [--workspace PATH]... PLAN CORPUS PRIVATE_STATE SOURCE
```

The positional arguments are in `estate-apply`'s order.

**Scope.** Every plan item that has a workspace and a `workspace.landed`
for SOURCE. `--item` and `--workspace` (both repeatable) narrow the set.
An item that is not landed is a row with refusal `HANDOFF_NOT_LANDED`.

**`--sibling`** (repeatable) declares the live copy that stands beside
the landing:

- `LANDED_PREFIX` and `LIVE_PREFIX` are absolute, canonical directory
  prefixes, matched at a path-component boundary;
- a workspace under `LANDED_PREFIX` gets a sibling at
  `LIVE_PREFIX/<rest>`.

The plan has no field for this. `Plan { items }` is strict postcard, and
the item id is `blake3(source, repository, workspace)`, so a new field
would break decoding and would move ids. The prefixes are kept in the
record (§5), so a re-render inside `estate-apply` does not need the flag
again.

A sibling is read and never written, locked or refreshed. Its HEAD is
read through `git_env` (no hooks, no optional locks), and the read is
metadata only.

**Locks.** `estate-handoff` takes `PRIVATE_STATE/estate.lock`
exclusively, as `apply_with` does, so it never runs alongside an apply.
Each workspace's writes then run under `landed::lock(common, SOURCE)`,
the lock converge takes.

**`--dry-run`** prints the rows and the blocks it would write, and
writes nothing.

**`--renew`** puts back a block that a person removed by hand (§6.5).
Without it, a block removed by hand stays removed.

**`--commit-guard`** comes in PR 3, opt-in (§9.5).

**`--check`** is read-only at the destination and in STATE. It prints
one row per workspace, with `state` set to one of these:

| `state` | Meaning |
|---|---|
| `absent` | No record and no marker |
| `present` | Intact and current |
| `stale` | Intact, but the inputs digest has moved |
| `modified` | A marker is present, but it does not match the record |
| `removed-by-hand` | The record exists and the marker is gone |
| `staged` | The marker is in the index blob |
| `committed` | The marker is in the HEAD blob, or in the blob at a destination-ahead or destination-only tip |
| `temp-left` | A crashed hand-off left a temporary file |

Each row also has an `open` count (§4.4), and `ready-to-clear` when
`open` is 0.

**Exit status.** Any `staged` or `committed` row exits non-zero with
`HANDOFF_MARKER_COMMITTED`. Any refusal in a write mode also exits
non-zero after every row has been printed, as `estate-apply` does.

**`--clear`** removes the block from each file and then the record (§7).
It never checks `ready-to-clear`, because clearing is the operator's
call. The receipt prints `open=n`.

**Output.** One `bulkload.estate-handoff.v1` JSON line per workspace,
with these fields:

- `item`, `workspace`, `outcome` and `refusal`;
- `files`, one entry per file: `path`, `kind` (`prefixed`, `created` or
  `via-symlink`) and `action` (`written`, `unchanged`, `created`,
  `cleared`, `removed-by-hand` or `lifted`);
- `inputs`, the inputs digest;
- `open`.

A summary line comes last.

### 3.2 What it reads

Every read below is at the destination or in the corpus. The source is
never read (S2).

| Input | Where it comes from | Cost |
|---|---|---|
| Plan items, the plan's path and its blake3 | PLAN | Plan bytes |
| The capture: bundle digest, `started_ns`, item id | `{item}.capture` in CORPUS (`estate::ledger`'s `LedgerEntry`) | Records |
| Capture facts: HEAD branch and commit, staged, modified and untracked counts, stash count, omitted rebuildable roots | The capture bundle's meta, diffed in a scratch repository (`diff-tree` of the HEAD, staged and worktree trees). Cached per capture digest in `PRIVATE_STATE/handoff/facts/<digest>` (postcard, magic). Captures are immutable, so each is computed once. | Corpus objects, once per capture |
| The landing: `recorded_ns`, the ledger's capture and HEAD, and whether any seat print has moved since the ledger | `workspace.landed`, plus an `lstat` walk of the ledger's seats | Metadata |
| Apply outcome, and any refusal pending review | The `{item}.outcome` records and journals (`estate::ledger`) | Records |
| Differences: item class, ref rows (`same`, `diverged`, `dest-ahead`, `source-ahead`, `source-only`, `dest-only`, with the `decide` row), and destination-only worktrees | `estate::diff` over the selected items, taken through its existing JSON line callback (`estate/diff.rs` is unchanged; §11) | Its own cost, and only when the inputs digest moved |
| The sibling's existence and HEAD | `lstat`, and HEAD read with no lock | Metadata |
| The destination host name | `uname().nodename` | None |
| The build | `CARGO_PKG_VERSION`, the build's revision (`option_env!("BULKLOAD_BUILD_REV")`, which the flake sets from `self.shortRev`, or `dirty`), and `current_exe()` | None |

**The inputs digest** is `blake3` over the canonical postcard of
`HandoffInputs`. It holds everything above except the diff rows, plus:

- the repository's ref values (`for-each-ref` output, which is metadata);
- `git worktree list`;
- the prints of the target files;
- the options.

When the digest equals the record's, nothing else runs.

### 3.3 The estate-wide index

Each block carries only its own workspace's facts and those of its
repository family: the main and every registered worktree. A block
covers at most 40 family rows. Past that it says how many more there
are and points to the index. So a 137-item estate never puts 137 rows
into each file, and one worktree's commit does not re-render unrelated
repositories.

The index has every row:

- `PRIVATE_STATE/handoff/<blake3(SOURCE)[..16]>/INDEX.md`;
- `PRIVATE_STATE/handoff/<blake3(SOURCE)[..16]>/index.json`.

Both are mode 0600, written atomically (temporary file, fsync, rename,
directory fsync: `landed::write_bytes`), and deterministic. STATE is
`private_directory` (0700) and outside every repository, so the index is
never committed.

## 4. The block

### 4.1 Format rules

- **Placement.** The block is at byte 0. It ends with the END line
  followed by one empty line, and both belong to the block. The rest of
  the file is the remainder, kept byte for byte.
- **Markers.** The block is delimited by HTML comments, which are
  invisible when Markdown is rendered and visible to agents reading the
  raw file:
  `<!-- bulkload-handoff:v1 BEGIN id=<16 hex> ... -->` and
  `<!-- bulkload-handoff:v1 END id=<16 hex> -->`. The id is the first 16
  hex digits of the block's own blake3, computed with the id field
  blanked.
- **The commit-detection token.** The BEGIN line carries the literal
  token `BULKLOAD-HANDOFF-DO-NOT-COMMIT`. `--check` greps the index blob
  and the HEAD blob for it, and so would an opt-in hook or a scanner
  rule.
- **Recognition.** A block is recognised only at byte 0. A marker
  anywhere else is just text. A BEGIN line without a matching END line
  is `modified`.
- **Encoding.** UTF-8 with LF line endings. Everything is sorted by path
  bytes, then by ref name. Instants are UTC RFC 3339 to the second, from
  recorded nanoseconds. Nothing depends on the locale, and the render
  never reads the clock (§4.3).
- **Size cap.** 64 KiB. Family rows past 40 are summarised, and ref rows
  past 30 per class are counted with a pointer to the index. Because the
  truncation is deterministic, `BUDGET_EXCEEDED` never arises.
- **Excluded content.** No remote URL appears, because one can carry
  user information or a token. A remote appears by name only. No
  credential, no environment variable and no file content from the
  repository appears.

### 4.2 Rendered example (placeholder paths)

```markdown
<!-- bulkload-handoff:v1 BEGIN id=3f9c0a1b2c3d4e5f BULKLOAD-HANDOFF-DO-NOT-COMMIT · bulkload owns this block: do not edit inside it, do not commit it -->
> **bulkload hand-off: converge and consolidate this workspace before any other work.**
> bulkload landed this checkout from `<src-host>`. Before you start anything else
> the user asks for, tell the user this hand-off is open and work through
> "Do this first" with them. (Linear TIN-6569, ruling OI-1003-Q183)

| Fact | Value |
|---|---|
| As of | 2026-10-16T18:04:11Z |
| Captured | 2026-10-16T17:40:02Z on `<src-host>` |
| Landed | 2026-10-16T17:52:45Z on `<dst-host>` |
| Source path | `/<src-home>/git/GloriousFlywheel` |
| This landing | `/<dst-root>/estate/gf-20261016/GloriousFlywheel` |
| Live sibling | `/<dst-root>/git/GloriousFlywheel`: exists, HEAD `main` at `1a2b3c4d5e6f` |
| Plan | `/<dst-root>/estate/gf-20261016.plan` (blake3 `9d1e04c2…`) |
| Corpus | `/<dst-root>/estate/corpus` |
| State | `/<dst-root>/estate/state` |
| Capture | bundle `5c7e0d91…`, item `a41f6b20…` |
| Built by | bulkload-agent 0.1.0, rev `3fe1f07`, `/nix/store/<hash>-bulkload/bin/bulkload-agent` |
| Estate index | `/<dst-root>/estate/state/handoff/<src-tag>/INDEX.md` |

**This repository's paths** (at capture on `<src-host>`; landing status on `<dst-host>`)

| Path on `<dst-host>` | Role | Branch @ HEAD | Staged · modified · untracked · stash | Landing |
|---|---|---|---|---|
| `/<dst-root>/estate/gf-20261016/GloriousFlywheel` | main (this) | `feat/tin-2611-gf-v4@8f2a6e1f3a7f` | 0 · 3 · 2 · 1 | applied, unchanged since landing |
| `/<dst-root>/estate/gf-20261016/GloriousFlywheel/.worktrees/p5` | linked | `work/p5@0b1c2d3e4f50` | 1 · 0 · 0 · 0 | applied, unchanged since landing |
| `/<dst-root>/estate/gf-20261016/GloriousFlywheel/.worktrees/tin-5770` | destination-only worktree | `work/tin-5770@77aa88bb99cc` | not captured | not planned, untouched |

**Differences** (estate-diff, as of above)

- Refs: 2 diverged (`feat/x`, `work/y`), 1 dest-ahead (`main`), 3 source-ahead (`feat/a`, `feat/b`, `feat/c`), 4 source-only (created at landing).
- Destination-only worktrees: 1 (above).
- Refusals pending review for this item: none.
- Rebuildable roots omitted at capture: `node_modules` (184211 entries).

**Do this first** (converge and consolidate before any other work)

1. Tell the user this hand-off is open. Do not start other work until it is cleared or the user explicitly defers it.
2. See what differs, read-only:
   `/nix/store/<hash>-bulkload/bin/bulkload-agent estate-diff --scan /<dst-root>/git /<dst-root>/estate/gf-20261016.plan /<dst-root>/estate/corpus <SOURCE> /<dst-root>/estate/state`
3. For each diverged, dest-ahead or source-ahead ref, and for each destination-only worktree, decide with the user which copy carries the work forward, and bring the other side's commits into it (merge or cherry-pick). Never reset, force-push, delete a branch, or delete either copy: removing a copy is the operator's call.
4. Commit or stash nothing that contains this block. Before any commit, check that `git diff --cached` shows no `BULKLOAD-HANDOFF-DO-NOT-COMMIT`.

**When it is done**

- `… estate-handoff --check --workspace /<dst-root>/estate/gf-20261016/GloriousFlywheel /<dst-root>/estate/gf-20261016.plan /<dst-root>/estate/corpus /<dst-root>/estate/state <SOURCE>` prints `ready-to-clear` (0 open rows).
- Then remove this block with `… estate-handoff --clear --workspace /<dst-root>/estate/gf-20261016/GloriousFlywheel /<dst-root>/estate/gf-20261016.plan /<dst-root>/estate/corpus /<dst-root>/estate/state <SOURCE>`.
  Do not delete the block by hand, and do not edit inside it.
<!-- bulkload-handoff:v1 END id=3f9c0a1b2c3d4e5f -->

```

`<SOURCE>` and every `<…>` placeholder are replaced by real values
when the block is rendered. The commands carry absolute paths, so an
agent can paste them as they are.

### 4.3 Determinism

`render(HandoffInputs, DiffRows, as_of_ns) -> Vec<u8>` is a pure
function. Calling it twice with the same arguments gives the same
bytes, and so does calling it with the input rows in another order.

`as_of_ns` is the only instant that is not recorded elsewhere:

- it is the record's `as_of_ns` while the inputs digest is unchanged;
- it is the run's start instant when the digest moved.

So an unchanged rerun produces the same bytes. A test passes `as_of_ns`
explicitly.

### 4.4 `open` and "done"

The `open` count is the number of rows, in this workspace's family, that
are still to be dealt with:

- diverged, dest-ahead and source-ahead refs;
- destination-only worktrees;
- refusals pending review for the family's items (`closure-dispose`
  disposes them);
- seats whose landing proof fails.

`ready-to-clear` means `open = 0`. Whether the live sibling has been
consolidated cannot be decided by a tool. The block asks the agent to
settle that with the user. The operator question in §13 asks what
"done" should mean.

## 5. Ledger representation

Two records sit in the worktree's own git directory, beside
`workspace.landed`: `$GIT_DIR/bulkload/v1/<SOURCE>/`. Linked worktrees
each have their own pair. Both are postcard after a 16-byte magic,
written with `landed::write` (temporary file, fsync, rename, directory
fsync) and capped by `LEDGER_CAP`.

```rust
/// magic b"bulkload-hoff-v1"
pub struct HandoffLanded {
    pub item: String,              // estate::id; a re-key (A11b) makes it stale
    pub capture: [u8; 32],         // workspace.landed's capture it was rendered against
    pub inputs: [u8; 32],          // the inputs digest (§3.2)
    pub as_of_ns: i128,
    pub recorded_ns: i128,         // when the prints below were taken (the racy window)
    pub fsid: u64,
    pub options: HandoffOptions,   // sibling prefixes, private-remote prefixes, allow, readme
    pub files: Vec<HandoffFile>,   // sorted by path
}
pub struct HandoffFile {
    pub path: Vec<u8>,             // b"AGENTS.md", b"CLAUDE.md", b"README.md"
    pub kind: HandoffKind,         // Prefixed | Created | ViaSymlink { target: Vec<u8> }
    pub block_len: u64,
    pub block: [u8; 32],           // blake3 of the block's bytes
    pub base: Option<String>,      // Prefixed: the landed blob oid the block sits on, if a landed seat
    pub print: SeatPrint,          // the file's print right after the hand-off wrote it
}
/// magic b"bulkload-hoffi-1"
pub struct HandoffIntent {
    pub op: HandoffOp,             // Render | Clear | Lift
    pub tag: String,               // the temporaries' name tag
    pub files: Vec<HandoffStep>,   // path, kind before and after, old (len, digest) or none, new (len, digest) or none
}
```

The record stores the block's digest and length, not its bytes. Every
check either compares a prefix with the digest or strips that many
bytes. The block's text is re-derived by the render.

`base` is `None` in two cases: a `created` file, and a prefixed file
that is not a landed seat (the destination made its own `AGENTS.md`
after landing). Converge never consults the latter, because it is not a
seat. It is still cleared exactly.

The workspace ledger is never written by the hand-off. `LandedWorkspace`
gains no field, because it is strict postcard.

## 6. Converge interaction

### 6.1 One predicate: `Side.prefix`

`Side` (`converge.rs:794`) gains
`prefix: Vec<(u64 /*len*/, [u8; 32] /*digest*/, Option<SeatPrint>)>`.
A new `handoff::landed_sides(worktree, git_dir, source, meta0)` wraps
`sides(worktree, &meta0)`. It fills `prefix` for every old-side path
that the record names, provided its `base` equals the side's blob. It
also fills it for a pending intent's old and new blocks. The new side
never carries a prefix.

`holds(objects, path, side)` then accepts either of two contents:

- exactly the blob, which is today's rule;
- for some prefix, the first `len` bytes hashing to the digest and the
  rest equal to the blob, read in one streaming pass through `Compare`.

Each check is counted in `DestVerifyRead`, as today.

`prove` gains one fast path. When the record's print for the seat
equals the print now, and is not racy against the record's
`recorded_ns`, the seat is proved without reading it. That is the same
fast path the ledger's own print gets.

`landed_sides` replaces `sides(workspace, &meta0)` at four old-side call
sites:

- `apply_workspace` (A6 and A10);
- `settle`;
- `converge/diff.rs::ledgered` (estate-diff);
- `adopt::rekey`.

`Engine::expected`, `Engine::sweep` and every `holds` caller stay
unchanged: they pick up the prefix through `op.old`.

### 6.2 Which converge step sees the block, and why no seat is refused

| Step | What it sees | Result |
|---|---|---|
| A4, the journal no-op (unchanged capture) | Nothing. No proof and no read. | The block stays. 0 bytes are read (S3). |
| A6, a ledger ahead of its journal | `prove` | Proved by the record's print, or by block plus landed content. The journal is written. |
| A10 proof, with the target not in the delta | `prove` | Proved. The seat is untouched. `next_ledger` records the proof's print, which is the print with the block, so the next converge proves it by the ledger's print. |
| A10 delta, a `Replace` of the target (the source edited it) | Fresh `Engine::replace` | The exchange puts the new body in place. The displaced file is checked by the proof's print and removed, and the old block goes with it. Then the re-render (§6.4) prepends the new block. |
| A10 delta, a `Remove` of the target (the source deleted it) | `Engine::remove` | The block plus the landed file are removed. The re-render creates the file as `created`. |
| A10 delta, an `Add` where a `created` hand-off file stands | The add-clash check | §6.3: the file is lifted first, then the add runs, then the re-render prepends. |
| A3, settling after a crash | `Engine::expected` and `sweep` (settle mode) | Block plus old is classified as old: the landed one, or "ours" for a temporary. A file holding the new body is new, as today. |
| A pending hand-off intent when an apply starts | `handoff::settle` runs under the lock, after `settle_repository` and before the workspace settle | Rolled forward (§8). Converge never sees a block that is half written. |
| A11b, a re-key | `prove` through `landed_sides` | Proved. The record's `item` is now stale, and the re-render rewrites it. |
| A11a, adopt | No record can exist, because a record needs a workspace ledger | A block carried in as source bytes is plain content, proved as the capture's. |

**Why no seat is refused.** A seat whose bytes are block plus landed
holds landed content. The prefix is bulkload's own bytes, under a digest
that only bulkload wrote, so dropping it loses nothing. Every other
content still falls through to `GIT_DESTINATION_MODIFIED`.

### 6.3 Lifting a `created` file before an add

A `created` file whose bytes are exactly the recorded block is
bulkload's. This step runs in `converge`, under the lock, before the
add-clash loop and before the workspace intent:

1. Write the hand-off intent with `op=Lift`.
2. `rename_exclusive_at` the file to the temporary name.
3. Check by print or content that the temporary holds exactly the block.
   If it does, unlink it and sync the directory. If not, rename it back
   and leave it to the clash check, which refuses MODIFIED: an untracked
   file the user changed is the user's.
4. Drop the entry from the record.
5. Remove the intent.

The add then runs as today.

### 6.4 Re-rendering after a converge (the H step)

In `apply_item`, after a workspace item's journal is written and only
when the item converged or settled in this run, the H step re-renders
that workspace when it holds a record:

- It uses the options stored in the record, so the apply needs no flag.
- It takes the same per-repository lock.
- A no-op item (A4) never reaches it, so S3 holds.
- A refusal of the H step is reported in the receipt as `handoff=<CODE>`,
  never as the item's outcome. The landing is applied, and the block is
  an annotation on it.
- The estate-wide index is rewritten once at the end of the apply when
  any H step wrote.

### 6.5 The cases the issue names

- **The source also edits `AGENTS.md` or `CLAUDE.md`.** This is a
  `Replace`, covered in §6.2: the new body lands and a new block goes on
  top. The hand-off never merges the source's text into the block. The
  block is regenerated from facts.
- **The destination user edits below the block.** The remainder is no
  longer the landed blob, so `prove` refuses MODIFIED, as for any edit
  to a landed seat. The block and the user's edit are both kept. A
  hand-off rerun still re-renders the block alone and keeps the user's
  remainder byte for byte.
- **The block is stale.** The inputs digest has moved: a destination
  commit, a changed ref, a converge, a sibling change, an option change,
  or a re-key. The re-render happens on the next `estate-handoff` run,
  or in the H step of the next converging apply. `--check` reports
  `stale` in between.
- **The user edits inside the block.** The marker is at byte 0, but the
  block no longer matches its digest. The file is the user's:
  - `prove` refuses MODIFIED at the next changed capture;
  - the hand-off refuses `HANDOFF_BLOCK_MODIFIED` with nothing written;
  - `--clear` refuses the same way.

  The remedy is in the refusal's reason: remove the whole block by hand
  (from the BEGIN line through the empty line after END). After that,
  the content is the landed content again and proves, and the next
  hand-off run retires the record as `removed-by-hand`.
- **The block is removed by hand.** No marker is left at byte 0. If the
  workspace ledger's capture still equals the record's capture, this
  was a person's choice: the entry is retired as `removed-by-hand` and
  not re-added, unless the next run is given `--renew`. If the capture
  moved, a converge replaced the file, so the block is re-added.
- **A foreign block.** A marker at byte 0 with no record, for example
  one carried in from a source that was itself a landing before PR 3,
  refuses `HANDOFF_BLOCK_MODIFIED` with the reason `foreign`.

## 7. Removal (`--clear`)

Clearing is a recorded, clean step. Per workspace, under the lock:

1. Write the hand-off intent with `op=Clear`.
2. For each file, by kind:
   - `prefixed`: write the remainder to a temporary file (exclusive
     create, `O_NOFOLLOW`, same mode, fsync), `RENAME_EXCHANGE` it with
     the target, check the displaced file's print against the print read
     in this step, unlink the displaced file, and sync the directory.
   - `created` whose bytes are exactly the block: rename it to a
     temporary name, check it, unlink it, and sync the directory.
   - `created` with bytes after the block (the user appended): strip the
     block as for `prefixed`. The rest is now the user's file.
   - `via-symlink`: nothing to do.
3. Remove `handoff.landed`, then the intent.
4. Rewrite the estate-wide index.

Afterwards `git status --porcelain` equals its output before the
hand-off, and the bytes of every target equal what they were before the
first render (test H8).

A cleared workspace converges on content: the landed bytes prove with no
prefix.

## 8. Crash safety, per step

The rule is the one converge already uses: an intent first, then each
file through a temporary file, exchange and a check of the displaced
file, then the record, then removal of the intent. The temporaries are
named `.bulkload-handoff-<tag>-<n>` beside the target, following
`Seat::at`'s scheme. A file system without `RENAME_EXCHANGE` refuses
`DESTINATION_EXCHANGE_UNSUPPORTED` before the intent.

| Step | Writes | State if a crash lands after it | Rerun (either `estate-handoff` or `estate-apply`'s `handoff::settle`) |
|---|---|---|---|
| 0. Settle, then sweep | Removes the hand-off's own leftover temporaries. A temporary is the hand-off's when its content is old block plus remainder, new block plus remainder, or the block alone. Anything else is put back, as in `Engine::sweep`. | | |
| 1. Classify and render | None | Unchanged | Again |
| 2. `handoff.intent` | One atomic write | The intent exists. Every file is still old. | Settle |
| 3. Temporary written and fsynced | The temporary only | The target is old. The temporary holds new block plus remainder. | The sweep removes it, then the step is redone |
| 4. Exchange, then check the displaced file | The target | The target is new, and the temporary holds the old file | The settle sees the target starting with the new block: done. The temporary is ours and is removed. |
| 4'. A `created` file: rename with no-replace | The target | New | Done. If the rename failed with `EEXIST` (someone created the file), the temporary is removed and the step refuses `HANDOFF_BLOCK_MODIFIED`. |
| 5. `handoff.landed` | One atomic write | The record is new and the intent is still there | The settle finds every file done and removes the intent |
| 6. Remove the intent | Unlink, then directory fsync | Done | No-op |

The settle classifies each target as follows:

- it starts with the intent's new block, or for a clear it equals the
  remainder: **done**;
- it starts with the old block, or for a first render it equals the
  remainder read then, by print or by content: **to do**;
- anything else: **third party**. It refuses `HANDOFF_BLOCK_MODIFIED`
  and keeps the intent, as `GIT_CONVERGE_INTERRUPTED` keeps a workspace
  intent.

**A writer racing the exchange.** An editor that saves `AGENTS.md`
between step 1's read and step 4's exchange changes the displaced
file's print. The exchange is undone, nothing is lost, and the step
refuses `HANDOFF_BLOCK_MODIFIED`. A save after the exchange lands on top
of the new block. If it changes the block it is a user edit inside the
block (§6.5). Otherwise it is an edit to the remainder.

**Fault points:**

- `estate.handoff.after_intent`, `after_temp`, `after_exchange` and
  `after_record`;
- `estate.handoff.clear_after_exchange`;
- `estate.handoff.lift_after_unlink`.

Converge's existing `EstateConvergeAfterSeat` with an overlaid seat is
covered by a row of its own (§10).

## 9. Privacy guard

1. **Default deny.** A workspace is written only when one of these
   holds:
   - its repository has no remote;
   - every `remote.*.url` and `remote.*.pushurl` value starts with a
     `--private-remote` prefix, using the same boundary rule as
     `--activate-origin`;
   - it is named by `--allow-workspace` (the operator vouches for it,
     and the receipt says so).

   Otherwise it refuses `HANDOFF_NOT_PRIVATE` with nothing written.
   Bulkload has no network access and no forge API (the dependency
   wall), so it cannot know whether a repository is public. The
   operator declares it. Each decision is recorded in the record and
   the receipt as a count, never as a URL.
2. **What is written where.** Absolute paths and host names go only
   into:
   - the target files of eligible workspaces;
   - `.git/bulkload/` (0700, `ensure_dir`);
   - `PRIVATE_STATE` (0700).

   Nothing is written into a sibling, a destination-only worktree or a
   repository that is not planned.
3. **No URLs, no secrets.** Remote URLs are never rendered or printed.
   A test puts a token in a remote URL's user information and checks
   that the token appears in no written byte, on stdout or in any
   record.
4. **Commit detection, always.** `--check` reads the index entry blob
   and the HEAD tree blob of each target. It also reads the blob of each
   target at every destination-ahead or destination-only tip the diff
   lists. That is a few object reads and no seat reads. It reports
   `staged` or `committed` and exits `HANDOFF_MARKER_COMMITTED`.
   Closure's schema is not changed. In PR 2, `estate-diff` rows gain a
   `handoff` field with the same states.
5. **Commit prevention, opt-in (PR 3).** `--commit-guard` writes a
   `pre-commit` hook only when `core.hooksPath` is unset and no
   `pre-commit` hook exists. The hook greps the staged target blobs for
   the token. It is recorded in the record, and `--clear` removes it only
   when its bytes are byte-identical. In every other case it reports
   `commit-guard=unavailable` and installs nothing. This agrees with the
   repository rules: installing a hook is not bypassing one, and nothing
   is installed by default.
6. **Bulkload's own paths (PR 3).** `estate-capture` of a tree that
   holds a record captures each target's remainder, not the block, when
   the block is intact, and counts it as custody `handoff-stripped`.
   S2 holds: the capture reads the record and the file and writes
   nothing at the source. A marker with no record is captured as it is
   and counted `handoff-marker-unrecorded`. Bulkload never pushes.

## 10. Typed refusals

These are new codes in `bulkload-proto`'s `refusal.rs`, each with a
`refusal_taxonomy` row:

| Code | When | Written |
|---|---|---|
| `HANDOFF_NOT_LANDED` | No `workspace.landed` for SOURCE: the item has not been applied, was refused, or is `journal-missing` | Nothing |
| `HANDOFF_NOT_PRIVATE` | The privacy guard (§9.1) | Nothing |
| `HANDOFF_TARGET_UNSUPPORTED` | A target is a symlink that does not resolve, inside the worktree, to the other target the hand-off writes; or it is a directory, FIFO or other non-regular file; or its parent is not a directory | Nothing, for the whole workspace. A partial hand-off would warn one harness and not the other. |
| `HANDOFF_BLOCK_MODIFIED` | A marker at byte 0 that does not match the record, a foreign block, a writer racing the exchange, or a third party found by the settle | Nothing. Any exchange is undone. A pending intent is kept. |
| `HANDOFF_MARKER_COMMITTED` | `--check` only: the token in an index blob, a HEAD blob or a tip blob | Nothing. The check is read-only. |

These existing codes are reused:

- `GIT_CONVERGE_INTERRUPTED`: a pending workspace intent. Run
  `estate-apply` to settle it first.
- `DESTINATION_EXCHANGE_UNSUPPORTED`.
- `FIELD_DOMAIN_VIOLATION`: a malformed `--sibling` or prefix.
- `PATH_NOT_ABSOLUTE`.
- `BUDGET_EXCEEDED`: a record over `LEDGER_CAP`.

**Why symlinks refuse.** A write through a symlink lands outside the
seat that converge proves, and possibly outside the worktree, in a file
the record cannot own. Every open uses `O_NOFOLLOW`. A symlink to the
other target in the same worktree (the common `CLAUDE.md ->
AGENTS.md`) is covered by that file's block. It is recorded as
`via-symlink` with its target, so it is never written twice.

**Why a missing file is created.** The operator wants every agent warned.
A harness reads only its own file: Codex reads `AGENTS.md`, Claude reads
`CLAUDE.md`. A created file is untracked. It is lifted before any add
of the same path (§6.3), removed by `--clear`, and found by commit
detection like a prefixed one. `README.md` is never created.

## 11. Code impact, and keeping the F1/F2 merge easy

| File | Change |
|---|---|
| `git_carry/handoff.rs` (new) | Records, `render`, classify, write, settle, clear, lift, `landed_sides`, privacy, and check |
| `git_carry/landed.rs` | Two `Record` impls (magics), `HANDOFF_LANDED` and `HANDOFF_INTENT` names |
| `git_carry/converge.rs` | `Side.prefix`; `holds` accepts a prefix; `prove`'s record-print fast path; `landed_sides` at the old-side sites; `handoff::settle` after `settle_repository`; `handoff::lift_created` before the add-clash loop. About 60 lines. |
| `git_carry/converge/diff.rs` | `ledgered` calls `landed_sides` (one line) |
| `git_carry/converge/adopt.rs` | `rekey` calls `landed_sides` (one line) |
| `estate.rs` | `handoff(...)` entry point; the H step in `apply_item` after the journal |
| `estate/diff.rs` | **No change.** The hand-off takes the diff through `estate::diff`'s line callback and parses its own `bulkload.estate-diff.v1` lines. The `handoff` field in diff rows waits for PR 2, after the F2 merge. |
| `git_carry.rs` `restore_linked_staged_landed` | **No change** |
| `main.rs` | The `estate-handoff` verb and usage text |
| `fault.rs` | Six points |
| `bulkload-proto/src/refusal.rs` | Five codes |

## 12. Tests (failing first)

The fixtures reuse `tests/estate_adopt/gftb.rs`'s shapes: a main, a
linked worktree, a nested `.worktrees/` worktree, a destination-only
worktree, and a sibling copy beside the landing. Property numbers are
assigned at build time against the other open lanes.

**Integration (`tests/estate_handoff.rs`):**

| # | Row | Expectation | Fails before the feature because |
|---|---|---|---|
| H0 | Control: a naive prepend without a record, then a source change elsewhere | `estate-apply` refuses `GIT_DESTINATION_MODIFIED` for the item. This documents the problem and passes today. | (control) |
| H1 | Render into a main, a linked worktree and a nested worktree | Both files start with the block at byte 0, and the remainders are byte-identical. Records and the index (0600) are written. P34-style freeze of every other seat, the index, HEAD, refs, config, `info/exclude` and the hooks directory. | The verb does not exist |
| H2 | An unchanged rerun | Every target's `lstat` is unchanged, `DestVerifyRead` = 0, 0 writes, and the diff does not run | Verb |
| H3 | The headline: render, a source change to another file, `estate-apply` | `applied`. The block is byte-identical. A second apply is a no-op that reads 0. | Converge refuses MODIFIED |
| H4 | The source edits `AGENTS.md` | The new body lands, the H step re-renders on top, and the record's capture moves. Closure `applied`. | MODIFIED |
| H5 | The source deletes `AGENTS.md`, then adds it again | `created`, then lifted, the add runs, and the file is `prefixed` again | MODIFIED, then an add clash |
| H6 | The user edits below the block, then the source changes another file | MODIFIED. The user's bytes and the block are intact. A hand-off rerun re-renders the block alone. | Verb |
| H7 | The user edits inside the block | The hand-off and `--clear` refuse `HANDOFF_BLOCK_MODIFIED` with nothing written. Converge refuses MODIFIED. After a removal by hand: converge proves, and the next run says `removed-by-hand` and does not re-add the block. | Verb |
| H8 | `--clear` | Bytes and `git status --porcelain` equal their state before the hand-off. The record is gone. A second clear is a no-op. | Verb |
| H9 | Symlinks: `CLAUDE.md -> AGENTS.md` (one write, `via-symlink`); a link outside the worktree; `AGENTS.md` as a directory | Covered; `HANDOFF_TARGET_UNSUPPORTED` with nothing written | Verb |
| H10 | Both files missing | Both are created untracked at mode 0644, and `--clear` removes them | Verb |
| H11 | Privacy: an https remote not under `--private-remote`, no remote, `--allow-workspace`, and a token in a URL's user information | `HANDOFF_NOT_PRIVATE` with nothing written; eligible; recorded; the token appears in no written byte or output | Verb |
| H12 | `git add AGENTS.md`, then a commit | `--check` reports `staged`, then `committed`, and exits `HANDOFF_MARKER_COMMITTED` | Verb |
| H13 | `--check` and `--dry-run` | Read-only: freeze of the destination and STATE | Verb |
| H14 | `estate-diff` over a handed-off item | `identical` and `ledgered`, with 0 destination bytes read | `diverged` |
| H15 | `--sibling` | The sibling's path and HEAD are in the block. The sibling is frozen (`lstat`, refs, index). A missing sibling says `absent`. | Verb |
| H16 | `--readme` | Off by default, so `README.md` is untouched. With the flag, an existing README is prefixed. A missing one is never created. | Verb |
| H17 | Re-key (A11b) of a handed-off landing | Proved, then re-rendered with the new item | MODIFIED |
| H18 | Determinism: two runs into two STATE directories | Identical blocks, except for the STATE paths. Permuted plan order gives identical bytes. | Verb |

**Unit (`git_carry/handoff.rs`, `converge/tests.rs`):**

- `render_is_pure_and_order_free`
- `render_truncates_family_and_ref_rows_deterministically`
- `a_marker_counts_only_at_byte_zero_and_needs_its_end`
- `holds_accepts_the_recorded_block_then_the_landed_blob_only`: a
  mismatched digest, block plus other content, block alone and landed
  alone, each checked
- `only_the_old_side_carries_a_prefix`
- `settle_classifies_a_displaced_block_and_old_as_ours`
- `the_exchange_is_undone_when_the_target_moved_after_the_read`, using
  the `mid_pass` hook as adopt's TOCTOU rows do

**Property:** `render_then_clear_is_identity`, over arbitrary bodies:
empty, no final newline, CRLF, a BOM, a leading `---` and binary bytes.
It goes through `test_support::prop_config`.

**Fault harness (`tests/fault_harness/estate_handoff.rs`):** one
`scenarios!` row per point in §8, plus `EstateConvergeAfterSeat` with an
overlaid target. Each row checks:

- every target is old or new;
- no temporary is left after the rerun;
- the rerun finishes;
- a second rerun is a no-op that reads 0.

## 13. TLA impact (`EstateConverge`)

This changes the modelled proof, settle and seat steps, so the model
changes with it.

**New constant `Handoff`.** The catalogue renders it FALSE for every
existing row, so those state spaces are unchanged.

**Values.** A block-carrying seat value is `Blk(v) == BLK + v`, one
block per row; `BLK` is a number above every seat value. A changed block
is `Blk2(v)`, which only the operator writes.

**Variables.**

- `hrec[p]`: `has`, `base` and `kind`.
- `hint`: the hand-off intent.

**Actions.**

- `HRender`: intent, write, record, with a crash between any two.
  Enabled when the main item is idle and the seat is `v` or `Blk(v)`.
- `HClear`.
- `HLift`.
- The operator gains a `"block"` kind in `OpKindSet`: an edit inside the
  block (`Blk2`).

**Changed steps.**

- `SeatProved(p, s)` also accepts `dseat[p].v = Blk(s.v)` when
  `hrec[p].has` and `hrec[p].base = s.v`.
- The engine's displaced check and the settle's classification accept
  `Blk(F)` as `F`.

**Invariants.**

- `NoLocalWorkLost` is amended in one place. An operator value that a
  render left under the block is preserved:
  `dseat[c].v \in {opLive[c].v, Blk(opLive[c].v)}`. That is stated, and
  checked by the mutations below.
- `CrashAtomicity` with no intent pending gains:
  `dseat[p].v \in {L.seats[p].v, Blk(L.seats[p].v)}`, or the
  operator's value.
- New `HandoffOwnsOnlyItsBlock`: a hand-off write changes only the
  prefix. The remainder after a render or a clear is the value before
  it.
- Liveness: `ConvergesWhenQuiet` holds with `Handoff` on.

**Mutations,** each expected to fail its named property:

| Mutation | Fails |
|---|---|
| `handoff_no_overlay` (proof ignores the record) | `ConvergesWhenQuiet`: MODIFIED forever on a quiet estate. This is the issue's bug. |
| `handoff_overlay_any_block` (accepts `Blk2`) | `NoLocalWorkLost`: an operator edit inside the block is dropped by a `Replace` |
| `handoff_clear_unchecked` | `NoLocalWorkLost` |
| `handoff_record_before_write` | `CrashAtomicity` |
| `handoff_untyped` | `TypedRefusal` |

**Rows:**

- `MC_ec_handoff`
- `MC_ec_handoff_crash`
- `MC_ec_handoff_op` (OpConcurrent, with `"block"`)
- `MC_ec_handoff_converge` (the source edits the target)
- `MC_ec_reach_handoff_cleared`
- `MC_ec_neg_handoff_*` (five)

These go in `catalogue/EstateConverge.dhall`, then `just tla-render`.

**Not modelled, and why:**

- The render's content and the inputs digest. Render is a pure function,
  covered by property tests.
- The privacy guard, which is a precondition.
- The estate-wide index, which is outside the destination.
- Commit detection, which is read-only.

## 14. Docs

**design.md, "Live union".** A new bullet after "Adopt", titled
"Hand-off block (TIN-6569, OI-1003-Q183)". It covers the verb, the
overlay rule, the H step, removal, privacy and the five refusals.

**slo.md amendment, "2026-10-16: the hand-off block (OI-1003-Q183,
TIN-6569)".** No SLO number changes. It states:

- **S3:** an unchanged rerun reads 0 seat bytes and writes 0.
- **S4:** the hand-off is an annotation outside closure's gate. Its
  refusals are typed per workspace and never change an item's outcome.
- **S5:** a block that is intact is never a local change, and any edit
  inside it is one.
- **S2:** the source is never read. The capture strip in PR 3 reads only
  at the source.

`docs/formal/README.md` gets the rows' state counts. This note is
updated with what was built.

## 15. PR split

1. **Overlay and verb.** The records, `render`, write, settle and clear;
   `estate-handoff` with `--check` and `--clear`; the converge overlay
   (§6.1 and §6.2, including the lift); the five codes; the TLA rows;
   H0 to H14, H17 and H18; the fault rows; the docs. Without the H step,
   a converge that replaces a target drops the block, and nothing is
   refused. The next `estate-handoff` run restores it.
2. **Automatic re-render and reporting.** The H step in `estate-apply`;
   the estate-wide index; `--sibling` facts; `--readme`; the `handoff`
   field in `estate-diff` rows (after the F1/F2 merge touches
   `estate/diff.rs`); H15 and H16.
3. **Bulkload's own paths.** The capture strip (§9.6) and the opt-in
   `--commit-guard`.

## 16. Open questions for the operator

1. **Which repositories count as private?** Bulkload cannot tell whether
   a repository is public. This design refuses unless every remote
   matches an operator-given `--private-remote` prefix, or the workspace
   is named with `--allow-workspace`. For the GloriousFlywheel landing:
   - Are its remotes private?
   - Or is a local landing private whatever its remote, with commit
     detection as the only guard?
2. **What does "converged and consolidated completely" mean for a
   beside-landing?** The checklist's final step, and `ready-to-clear`,
   need to know:
   - which copy carries the work forward (the landing, or the live
     sibling);
   - whether agents may retire the other copy, or whether that stays
     the operator's call. This design says it stays the operator's, and
     that agents never delete either copy.

## 17. As built (2026-10-16)

Built in this lane on `feat/estate-handoff-20261016` (off `3fe1f07`), in
one commit (PR 1 and most of PR 2 of section 15). Rulings: OI-1003-Q183
(TIN-6569), with OI-1003-Q144/Q149 (S5) and Q171/Q172 (adopt, re-key) as
above; R-N13 for this note.

### 17.1 What exists

- **Verb.** `estate-handoff [--check | --clear | --dry-run] [--item ID]...
  [--workspace PATH]... [--sibling LANDED=LIVE]... [--private-remote
  PREFIX]... [--allow-workspace PATH]... [--readme] [--renew] [--as-of NS]
  PLAN CORPUS PRIVATE_STATE SOURCE` (`main.rs`, `estate/handoff.rs`).
  PLAN, CORPUS and STATE are made absolute (canonical when they exist),
  so every path in a block is absolute whatever the operator typed.
- **Overlay** (`git_carry/converge/handoff.rs`, `handoff/render.rs`):
  records `handoff.landed` and `handoff.intent`; `landed_sides` at the
  four old-side sites (`apply_workspace`, `settle`, `converge/diff.rs`
  `ledgered`, `adopt::rekey`); `holds` accepts a recorded block followed
  by the landed blob; `prove` trusts the record's print only under the
  conditions in 17.2; `lift` before the add-clash check.
- **H step** (`estate::apply_reporting`, `estate/handoff.rs`
  `after_levels`/`after_apply`): after the last level, each workspace of
  a repository the apply changed (not a refused item's own) that holds a
  record or a pending intent is re-rendered under the record's options
  and the apply's PLAN/CORPUS/STATE. It prints `item=<id>
  handoff=<written|unchanged|CODE>` after the receipts; it is never an
  item's outcome. `apply_with` keeps its signature (it reports nothing);
  `main.rs` calls `apply_reporting`.
- **Five refusal codes** in `bulkload-proto` with taxonomy rows; six fault
  points (`fault.rs`).
- **Docs:** `docs/design.md` "Hand-off block"; `docs/slo.md` amendment
  2026-10-16; `docs/formal/README.md` "TIN-6569".

### 17.2 Deviations from sections 0 to 16, and why

1. **Module layout.** `git_carry/converge/handoff.rs` (with `render.rs`
   and `tests.rs`) and `estate/handoff.rs`, not `git_carry/handoff.rs`;
   the record types and magics live in the hand-off module, not
   `landed.rs`. `landed.rs` is unchanged.
2. **Record fields.** `HandoffFile` has `print: Option<SeatPrint>` and
   `base: Option<String>`; `HandoffLanded` has `open` (so an unchanged
   rerun reports `open`/`ready` without reading), `options` (PLAN, CORPUS,
   STATE, siblings, private prefixes, allowed, readme) and `retired`
   (targets removed by hand, keyed by the capture then). The intent has
   `Render` and `Clear` only.
3. **The lift writes no intent.** It renames the created file to a
   temporary, checks it by print, unlinks it, then updates the record. A
   crash after the unlink (`estate.handoff.lift_after_unlink`) leaves the
   record naming an absent created file; the add lands and the next render
   prefixes it.
4. **`base` guards the print fast path (found in the build, H20).** A
   block re-rendered over a user's edit below it must never vouch for that
   edit. Converge trusts a record print only when `base` (the landed blob
   the block was written over, checked with `git hash-object` of the
   remainder) equals the seat's landed blob. With the guard disabled, H20
   shows converge replacing the seat and losing the user's line.
   **Corrected by section 18 (C1, C2):** as built in `9ec8d36` the guard
   held only for a print the render itself took; the keep path and the
   settle re-took prints under the old `base` and did vouch for an edit
   below the block. Every re-take now goes through `retaken`.
5. **Racy prints (S3).** `held` trusts the record's print only before the
   racy window of the record's `recorded_ns`, as `prove` does for the
   ledger (unit row `a_racy_record_print_is_proved_by_reading`, failing
   first). A render's prints are always racy against its own record, so
   the first unchanged rerun reads the blocks (at most 64 KiB each) and
   re-dates the record; from the second on it reads 0 bytes and writes
   nothing (H2). The design said the first.
6. **Inputs digest.** Each item's part is the digest of what the block
   says (current capture, omissions, last refusal code), not the record
   bytes: a no-op `estate-apply` rewrites outcome records and must not
   force a re-render (H2, failing first).
7. **H step placement.** Built now (section 15 put it in PR 2), and after
   the last level rather than per item, because a per-item H step rendered
   a main's block before its linked worktree converged and left it stale
   (H4's first run). A refused item's own workspace is skipped (the
   converge left it as it was; H6). A pending hand-off intent is settled by
   the next apply even when every item is a journal no-op (fault rows
   `estate_handoff_h_step_*`).
8. **Ref classes.** The block computes `estate-diff`'s classes itself
   (`native_refs` against the landed instance's carried refs) through a
   `ref_class` helper extracted from `converge/diff.rs` `ref_rows`, not
   through `estate::diff`'s JSON callback. `estate/diff.rs` and
   `restore_linked_staged_landed` are untouched; `converge/diff.rs` has the
   one-line `landed_sides` switch and that extraction.
9. **`--check`'s `committed`** also reads diverged tips' blobs, not only
   dest-ahead and dest-only ones.
10. **`--as-of NS`** was added for reproducible renders (H6, H18). H18
    clears and re-renders into the same STATE; the design's two-STATE and
    permuted-plan variant is not built (the unit row
    `render_is_pure_and_order_free` covers order).
11. **WP10 usage guard.** `tests/no_credential_tools.rs`
    `the_agent_has_no_handoff_verb` counted every "handoff" in USAGE; it
    now admits mentions spelled `estate-handoff` only (the verb and its
    schema) and still requires that no other handoff verb is listed. The
    alternative is renaming the verb (for example `estate-addendum`);
    that is the coordinator's call.
12. **TLA.** Rows `MC_ec_handoff`, `MC_ec_handoff_op`,
    `MC_ec_handoff_live`, two reach rows and five negative rows (section
    13 named `_crash` and `_converge` rows; crashes and source edits of the
    target are inside `MC_ec_handoff`). Mutations `handoff_exchange_unchecked`
    and `handoff_no_intent` replace `handoff_record_before_write` and
    `handoff_untyped`; `handoff_no_overlay` fails `TypedRefusal`, not
    `ConvergesWhenQuiet`. `LiveSpec` gains `WF_vars(HandoffRun)`. The
    operator's removal of a block by hand is modelled between applies
    only. Details: `docs/formal/README.md`, "TIN-6569".

### 17.3 Not built

The estate-wide `INDEX.md`/`index.json` under PRIVATE_STATE (section
3.3), the `handoff` field in `estate-diff` rows, `--commit-guard` and the
capture strip (section 9.5 and 9.6, PR 3; the strip is withdrawn under
OI-1003-Q185, section 18.3). A block holds its own
repository family only, capped at 40 rows.

### 17.4 Proof (failing first)

- Before the verb existed: `tests/estate_handoff.rs` H1 to H18 failed and
  H0 (the control) passed (17 failed, 1 passed).
- After the first build, these failed first against it and pass now:
  H2 (re-rendered after a no-op apply), H3, H4, H5 and H17 (no H step),
  H19 (relative paths in the block), the unit row
  `a_racy_record_print_is_proved_by_reading` (a racy print trusted), and
  H20 (with the `base` guard disabled: `seats-replaced=1`, the user's line
  lost).
- Counts at the commit: `estate_handoff` 21 integration rows (H0 to H20),
  11 hand-off unit and property rows, 14 `estate.handoff.*` fault rows.
  `cargo test -p bulkload-agent`: 909 passed, 0 failed, 7 ignored (29
  binaries, `--no-fail-fast`); `estate_adopt` 13, `estate_converge` 23;
  `fault_harness` 111; `cargo fmt --check`, the three clippy passes and
  `just contract-test` clean. The model: `docs/formal/README.md`,
  "TIN-6569".
- Two repository guards caught the first build and shaped it: the S2
  source-command registry (no new `hash-object` caller: the `base` check
  reads the landed blob through the existing batch reader; no bare
  `"diff"` literal) and WP10's usage guard (17.2 item 11).

### 17.5 Open for the operator

The two questions of section 16 stand. Two more, each with a
conservative default built:

1. The WP10 guard exception versus a verb rename (17.2 item 11).
2. Whether the H step should also re-render a refused item's own
   workspace (it now leaves it alone).

## 18. Review of 2026-10-16 and fixes

Two refuters reproduced findings against `9ec8d36` (converge safety;
privacy and claims). This section is the lane's answer, built on top of
`9ec8d36` in this worktree. Rulings: OI-1003-Q183 (TIN-6569), OI-1003-Q185
(nothing is excluded from a carry), OI-1003-Q173, OI-1003-Q144/Q149 (S5),
R-N13. Each finding was reproduced first by a row that failed on
`9ec8d36`, then fixed; none was rejected.

### 18.1 Per-finding table

| # | Refuter, severity | Finding | Reproduced (failed on `9ec8d36`) | Fix | Outcome |
|---|---|---|---|---|---|
| C1 | converge safety, BLOCKER | A plain rerun over an intact block after a user edit below it re-took the record's print and kept the old `base`; the next converge trusted the print and landed over the edit; `estate-diff` said `identical` | H21: `estate-diff` `identical`, then the user's line overwritten | `render_workspace`'s keep path re-takes a moved print only through `retaken`, which re-reads the file and checks the bytes under the block against the landed blob at that print; a kept target keeps the print its bytes were checked at | Fixed. H21 passes; M04a (the original shape) killed |
| C2 | converge safety, MAJOR | `settle` re-took prints with the old `base`: a crashed re-render settled by a run that then refused (privacy) vouched for the edit | fault row `estate_handoff_rerender_over_edit`: `seats-replaced=2`, the edit lost | `settle` re-takes every print through `retaken` | Fixed; M05 killed |
| C3 | converge safety, minor | A crashed `--clear` was undone by the next `estate-apply` (even a no-op): the H step re-rendered | fault row `estate_handoff_clear_noop_apply`: the block back on `AGENTS.md` | `settle` finishes a pending clear (roll forward; a refusal keeps the intent); the H step (`Request.h_step`) never starts a render and reports `handoff=cleared`; `recorded_options` also opts in on a pending clear intent; `clear` keeps its intent on a refusal | Fixed; M16, M17 killed |
| C4 | converge safety, minor | A block carried in from the source refused render and `--clear` forever (`HANDOFF_BLOCK_MODIFIED`, "foreign") | H24: render refused | Owner identity (18.2): a block naming another landing is content (`Held::Foreign`), kept below this landing's block, named as carried, never cleared | Fixed; M13, M14 killed |
| P1 | privacy, BLOCKER | The guard read a landing's preserved inert ssh origin as `no-remote` and wrote into landed clones of public repositories by default | H26: `"private":"no-remote"`, block written | `privacy` reads `remote.*.url`/`pushurl` in the common directory's `carry-config/source-config` and `source-config.worktree` as well | Fixed; M07 killed |
| P2 | privacy, MAJOR | `ready` was true at birth beside a live sibling, and unreachable after consolidating (dest-ahead and changed seats counted open) | H29: `open 2`, `ready:false` after a consolidating commit | Open rows are now diverged and source-ahead refs, a sibling whose HEAD and the landing's HEAD are in neither one's history, destination-only worktrees whose HEAD no other branch holds, refusals and a newer capture; a destination commit, dest-ahead or a changed seat is consolidation | Fixed for committed work; H29 passes; M18, M19 killed. `ready` still reads commits only (operator question 2) |
| P3 | privacy, MAJOR | Commit detection read branch tips only; a block committed and removed later went unreported | H28: `--check` ok | `history_has_token`: `git log --branches --tags [HEAD] -S TOKEN -- <targets>` (carried `refs/carry/` refs are not read) | Fixed; M10 killed |
| P4 | privacy, MAJOR | Six mutations survived the H suite and unit rows | The refuter did not list them; this lane ran its own 28 (18.4) | Rows H20 to H35, two unit rows and two fault rows added until each mutant died | Fixed: 28 of 28 killed |
| P5 | privacy, MAJOR | Re-carrying a handed-off landing copied the block to the next host, naming wrong paths; bulkload could neither render nor clear it | H25: render refused on the third landing | Owner identity (18.2); Q185: the carried block is kept, byte for byte, below the new block, which names it as carried and tells agents not to act on its paths | Fixed; M13, M14 killed |
| P6 | privacy, minor | A plain rerun dropped `--sibling`; the block's printed `--check` said `stale` for any render made with flags | H30: sibling gone, `stale` | `effective`: options not given are the record's (siblings, private prefixes, allowed, readme), for render, check and the H step | Fixed; M20 killed |
| P7 | privacy, minor | Doc claims went further than the code | The refuter's list was not handed over; this lane audited every hand-off claim | Corrected: `docs/design.md` (privacy "no remote", "foreign block" refusal, "settles a crash forward", where paths go, commit detection), `docs/slo.md` S5 (the hand-off refuses only an edit inside the block), 17.2 item 4 here ("never vouches" was false on the keep and settle paths), `docs/formal/README.md` ("racy re-dating S3 only"), the usage text | Fixed as far as found; operator question 6 |
| P8 | privacy, minor | The prefix check used raw URLs and ignored `insteadOf`/`pushInsteadOf` | H27: refusal missing for both | Each URL is checked as written and as rewritten by the longest `insteadOf` and `pushInsteadOf` rule from the repository's, the preserved, the user's global and the system configuration (located as Git does; read by `--file`, so bulkload's own children stay off global config) | Fixed; M08 killed |
| P9 | privacy, minor | Codex never reads `AGENTS.md` when `AGENTS.override.md` exists | H31: no block in the override | An existing `AGENTS.override.md` is a target (prefixed, never created; a symlink to `AGENTS.md` or `CLAUDE.md` is covered) | Fixed; M26 killed (H34) |

### 18.2 Owner identity and carried blocks (C4, P5; OI-1003-Q185)

The BEGIN line now carries `for=<16 hex>`, the blake3 of the destination
host, the workspace path as planned and the SOURCE label
(`render::owner`). A block at byte 0 that no record vouches for is:

- this landing's when its `for=` is this landing's identity: an edit
  inside, a cut END line, or a block with no record: `Modified`, refused
  as before (H7);
- otherwise **carried**: it came with the source's bytes (the source was
  itself a handed-off checkout, or a re-carry to another host or path).
  It is content. A render prepends this landing's block above it and keeps
  it byte for byte; the block lists it under "Carried hand-off blocks";
  `--check` reports `carried` until this landing's block is on; `--clear`
  removes only this landing's block; converge proves it as landed
  content, and the record's `carried` flag keeps the note on later
  renders. Bulkload never strips it; removing it is the user's call.

Commit detection still flags the token wherever it is (a carried block
committed at the source is still a block in pushable history).

### 18.3 Other changes

- **Capture strip withdrawn.** Section 9.6's plan (a capture strips an
  intact block) contradicts OI-1003-Q185 and is withdrawn; 18.2 handles a
  carried block at the next landing instead.
- **`retaken` is the one way a record's print is written after a read**
  (keep, settle, re-dating). The re-dating path (an unchanged rerun past
  the racy window) also checks the bytes first: a user edit made between a
  render's exchange and its `lstat` is caught there (unit row
  `a_redated_print_never_vouches_for_an_edit_made_after_the_exchange`,
  through a new `cfg(test)` hook `mid_pass::Stage::HandoffWritten`).
- **`git_env::USER_CONFIG`** names the variables Git uses to find the
  global and system files, so the privacy guard reads their rewrite rules
  by path without a new `Command` site (the S2 source-command registry
  passes unchanged).
- **Model.** `EstateConverge` models the record's print, its racy flag and
  its base (`HTrusted`, `HRetake`, `HandoffKeep` as a disjunct of
  `HandoffStart`), a settle that finishes a clear, and the new mutation
  `handoff_retake_unchecked` (fails `NoLocalWorkLost`). Details and the
  row table: `docs/formal/README.md`, "TIN-6569".

### 18.4 Mutation table

Each mutant is one edit of `handoff.rs` or `render.rs`, run against
`cargo test -p bulkload-agent --lib handoff`, `--test estate_handoff`
and the `estate_handoff` fault-harness rows (stopping at the first
failing suite). Runner and sites: `/dev/shm/handoff-lane/mut/` (scratch).

| Mutant | Edit | First run | Killed by |
|---|---|---|---|
| M01_overlay_base_guard_off | converge trusts a record print whatever its `base` | KILLED | `h20_a_block_over_a_users_edit_never_vouches_for_it`, `h21_a_plain_rerun_never_vouches_for_an_edit_below_the_block` |
| M02_held_trusts_racy_print | the hand-off trusts a racy record print | KILLED | `unit a_racy_record_print_is_proved_by_reading` |
| M03_overlay_trusts_racy_print | converge trusts a racy record print | KILLED | `h14_estate_diff_reads_a_handed_off_item_as_identical` |
| M04_keep_retakes_print_unchecked | keep path skips `retaken` (keeps the old print) | SURVIVED | `h33_a_rerun_after_an_edit_below_settles_to_writing_nothing` |
| M05_settle_retakes_print_unchecked | settle re-takes prints with the old base (C2 as found) | KILLED | `fault row estate_handoff_rerender_over_edit` |
| M06_redate_unchecked | re-dating a racy print skips `retaken` | SURVIVED | `unit a_redated_print_never_vouches_for_an_edit_made_after_the_exchange` |
| M07_privacy_ignores_preserved_origin | guard ignores `carry-config/source-config*` | KILLED | `h26_a_preserved_inert_origin_is_still_a_remote` |
| M08_privacy_ignores_rewrites | guard ignores `insteadOf`/`pushInsteadOf` | KILLED | `h27_insteadof_rewrites_are_checked` |
| M09_privacy_always_private | guard accepts every URL | KILLED | `h26_a_preserved_inert_origin_is_still_a_remote`, `h11_privacy_is_deny_by_default_and_urls_are_never_printed`, `h27_insteadof_rewrites_are_checked` |
| M10_no_history_scan | no branch/tag history scan | KILLED | `h28_a_block_in_branch_history_is_reported` |
| M11_no_index_scan | no index blob scan | KILLED | `h12_check_detects_a_staged_or_committed_block` |
| M12_exchange_unchecked | exchange never checks the displaced file | KILLED | `unit the_exchange_is_undone_when_the_target_moved_after_the_read` |
| M13_own_marker_read_as_foreign | an unrecorded block of this landing is read as carried | KILLED | `unit a_racy_record_print_is_proved_by_reading` |
| M14_foreign_block_read_as_modified | a carried block is read as an edited one | KILLED | `h24_a_block_carried_from_the_source_is_kept_as_content`, `h25_a_recarried_landing_keeps_the_earlier_block_below_its_own` |
| M15_overlay_any_prefix | `skip_prefix` accepts any bytes of the recorded length | KILLED | `unit holds_accepts_the_recorded_block_then_the_landed_blob_only` |
| M16_crashed_clear_not_rolled_forward | settle re-records instead of finishing a clear | KILLED | `fault row estate_handoff_clear_noop_apply` |
| M17_h_step_starts_a_render | the H step renders after a settle that cleared | KILLED | `fault row estate_handoff_clear_noop_apply` |
| M18_dest_ahead_is_open | dest-ahead counts as open | KILLED | `h29_ready_follows_consolidation` |
| M19_sibling_always_consolidated | the sibling always counts as reconciled | KILLED | `h29_ready_follows_consolidation` |
| M20_rerun_drops_options | a rerun drops the recorded siblings | KILLED | `h30_a_plain_rerun_and_the_printed_check_keep_the_records_options`, `h29_ready_follows_consolidation` |
| M21_lift_unchecked | lift removes a created file without its digest check | KILLED | `h32_an_edited_created_file_is_never_lifted` |
| M22_removed_by_hand_re_added | a block removed by hand is re-added | KILLED | `h7_an_edit_inside_the_block_makes_the_file_the_users` |
| M23_clear_remove_unchecked | clear removes a created file without checking it | SURVIVED | `unit the_removal_is_undone_when_the_target_moved_after_the_read` |
| M24_sweep_deletes_any_temp | sweep deletes a third party's temporary | KILLED | `unit settle_puts_back_a_third_partys_file_a_crash_displaced` |
| M25_held_ignores_block_digest | a marker of the right length counts as intact | KILLED | `unit a_racy_record_print_is_proved_by_reading` |
| M26_override_created | `AGENTS.override.md` may be created | SURVIVED | `h34_an_override_the_source_deletes_is_never_created` |
| M27_inputs_ignore_refs | the inputs digest leaves refs out | SURVIVED | `h35_a_new_destination_ref_makes_the_block_stale` |
| M04a_keep_relstat_old_base | keep path re-takes the print by `lstat` with the old base (the BLOCKER as found) | added in round 2 | `h21_a_plain_rerun_never_vouches_for_an_edit_below_the_block`, `h33_a_rerun_after_an_edit_below_settles_to_writing_nothing` |

First run: 22 of 27 killed. The five survivors (M04, M06, M23, M26, M27) and the
BLOCKER's own shape (M04a) were run again after H33, H34, H35 and the unit rows
`a_redated_print_never_vouches_for_an_edit_made_after_the_exchange` and
`the_removal_is_undone_when_the_target_moved_after_the_read` were added:
all killed. 28 of 28 killed.

### 18.5 Proof and counts

- Failing first, on `9ec8d36` (this worktree before the fix): H21, H24
  to H31 (`estate_handoff`, 9 rows) and the fault rows
  `estate_handoff_rerender_over_edit` and `estate_handoff_clear_noop_apply`
  failed; H20 (the old guard row) passed. H32 to H35 and the two new unit
  rows were added for the mutation table and fail under their mutants.
- After the fix, in this worktree: `cargo test -p bulkload-agent
  --no-fail-fast` 924 passed, 0 failed, 7 ignored (29 binaries), with
  `estate_handoff` 34 (H0 to H21, H24 to H35), `estate_adopt` 13,
  `estate_converge` 23, 13 hand-off unit and property rows;
  `fault_harness` 113 (16 `estate_handoff` rows with its child entry);
  `cargo fmt --check`, the three clippy passes (workspace; `io-trace`;
  `fault-injection,io-trace`) and `just contract-test` (24) clean;
  `source_command_registry` 16 passed; `just tla-render --check` current
  (245 files); `just tla-check` of all 11 hand-off rows matched (233 s,
  `docs/formal/README.md`), plus `MC_ec_refs` 54141, `MC_ec_adopt` 30652,
  `MC_ec_seats` 64478 and `MC_ec_neg_overwrite_modified` matched on the
  reviewed spec (`refs` and `adopt` equal their recorded counts).
  `just check-fast` was not run here (coordinator).

### 18.6 Open for the operator

1. (Section 16.1, still open.) Which repositories count as private. Also:
   should a remote whose URL is a local path count as private without a
   `--private-remote` (an agent consolidating may add the sibling as a
   remote, which now refuses the next render)?
2. `ready` reads commits only. Should it also require a clean working tree
   in the sibling and in destination-only worktrees (a read-only
   `git status` of the sibling on every run, which an unchanged rerun now
   avoids)?
3. Carried blocks are kept as content (Q185). Should `--clear` offer an
   opt-in to remove a carried block, or stay hands-off?
4. Commit detection flags the token anywhere in branch or tag history,
   including commits made at the source; a repository whose published
   history already holds a block reports `committed` for good. Accept, or
   count only commits not reachable from the carried capture?
5. (17.5) The WP10 guard exception versus a verb rename; whether the H
   step should re-render a refused item's own workspace.
6. The refuters' own lists (their six surviving mutations and the doc
   claims) were not handed over; P4 and P7 were answered from this
   lane's audit. Confirm against their lists.

## 19. Re-reviews of 2026-10-16, and the operator's rulings Q188 to Q191

Rulings: OI-1003-Q183 and Q185 (TIN-6569), Q188 to Q191 (Linear TIN-6569
comment `9c7121b5`, 2026-10-16, relayed in this lane's task text and not
read from Linear here), Q173, R-N13. Two re-reviews read `7c5407f`: the
first (findings 1 to 5) against the commit, the second against this
lane's uncommitted round-2 tree, which closed findings 1, 2, 3 and 5 and
part of 4 and regressed H7. Section 19.1 to 19.3 is the first commit on
top of `7c5407f` (round 2 finished); 19.4 onwards is the second (the
rulings).

### 19.1 Re-review 1 (against `7c5407f`), as closed by round 2

| # | Finding | Row (failed first) | Fix | Re-review 2 |
|---|---|---|---|---|
| 1 | The keep path re-dated a racy record print equal to the target's without checking the bytes under the block (a rerun that retires another target, or `--renew`) | unit `a_kept_block_never_redates_a_racy_print_unchecked_on_a_retire`, `..._on_a_renew` | The keep path keeps a print unchecked only when `trusted`; otherwise `retaken` | Verified closed |
| 2 | A live sibling strictly ahead of the landing counted as reconciled | H38 | Reconciled only when the landing holds the sibling's HEAD (a branch, or its own HEAD's history) | Verified closed |
| 3 | A symlinked global configuration (home-manager) was skipped by the privacy guard | H36 | `user_config_is_file` follows links as Git does | Verified closed; the guard is removed by Q188 (19.4) |
| 4 | A round-tripped block of this landing's identity with no record refused render and clear | H37 | No record entry names the target: carried | Closed only with no record (19.2, finding 2) |
| 5 | Commit detection read only this run's targets' history | H39 | Every name a hand-off may write | Verified closed |

### 19.2 Re-review 2 (against the round-2 tree) and its fixes

1. **BLOCKER: H7 regressed.** Round 2 dropped `unrecorded`'s
   `owner_of == ours => Modified` arm, so an edit inside the recorded
   block was read as a carried block (H7 and the unit row
   `a_racy_record_print_is_proved_by_reading` failed; `recorded` unused,
   clippy failed). Restored, then narrowed by finding 2 below: a block of
   this landing's identity on a target the record names (or retired) is
   `Modified` unless the whole file is the target's landed blob. Mutant
   M13 (the arm reads `Foreign`) is the regression's shape.
2. **Minor: a round trip while the record stands lost content or
   wedged.** The source carried back this landing's current block (H40)
   or an older one (H41) above an edited body. With H40 the block matched
   the record, so the H step replaced it (`B1` dropped, Q185 violated)
   and the next converge refused `GIT_DESTINATION_MODIFIED`; with H41 the
   block was read as an edit (`HANDOFF_BLOCK_MODIFIED` in the H step,
   render and `--clear`). The rule now (`classify_head`): a block at byte
   0 that could be this landing's (the record's block, a retired one, or
   this identity on a recorded target) is the file's content when the
   whole file is the target's landed blob, because bulkload's own block is
   never part of what the source landed. The check reads the file and the
   landed blob only on the read path (never for a trusted print). Both
   rows failed first (H40: the carried block dropped; H41:
   `handoff=HANDOFF_BLOCK_MODIFIED`).
3. **Minor: a retired block put back survived `--clear` as if live.** A
   block a person removed by hand and then restored by an undo or a
   `git stash pop` was read as carried (round 2) where `7c5407f` refused.
   The record's `retired` entries now keep the block's kind, length and
   digest (`Retired`); a block that matches one is this landing's again
   (`entry_of`): converge accepts it (`landed_sides` attaches retired
   blocks as untrusted prefixes), the next render takes it back into the
   record (`restored`, or `written` when it re-renders) and `--clear`
   removes it. A different block of this identity on a retired target is
   `Modified`, as at `7c5407f`, unless the whole file is landed. H42
   failed first (`--check` said `carried`).

Found while porting: **a created file became `prefixed` when re-rendered**
(H43, failed first: `--check` said `prefixed`, so `--clear` would leave an
empty untracked file and converge would not lift it before an add).
`Plan::Replace` keeps the target's kind.

### 19.3 Proof for the round-2 commit

- Failing first, on the round-2 tree with the `Modified` arm restored:
  H40, H41, H42 and H43 (4 failed; H7 and the 15 hand-off unit rows
  passed). After the fix: `estate_handoff` 42 passed.
- Checks at this commit, in this worktree (`CARGO_TARGET_DIR`
  `cargo-target/handoff`, `nix develop .#default`, nice 10): `cargo fmt
  --check`; the three clippy passes (workspace; `io-trace`;
  `fault-injection,io-trace`); `cargo test -p bulkload-agent --locked
  --no-fail-fast` 934 passed, 0 failed, 7 ignored (29 binaries), with
  `estate_handoff` 42, `estate_adopt` 13, `estate_converge` 23,
  `source_command_registry` 16 and 15 hand-off unit rows;
  `fault_harness` 113; `just contract-test` (24); `just tla-render
  --check` (246 files); `just tla-check` of the 12 hand-off rows and
  `MC_ec_refs`, 14 rows matched (`docs/formal/README.md`, "TIN-6569",
  re-reviews). The mutation rows for this commit ran after it; their
  table is in 19.8.

### 19.4 The rulings (Linear TIN-6569 comment `9c7121b5`, as relayed)

| Ruling | Text (relayed) | Built |
|---|---|---|
| OI-1003-Q188 | Remove the privacy guard entirely ("an erroneous overreach; such a block is irrelevant to bulkload"). The block is always written; commit detection stays as a report. | `privacy`, `under`, `Remotes`, `remotes_in`, `user_config_files`, `user_config_is_file`, `rewritten`, `git_env::USER_CONFIG`, `HANDOFF_NOT_PRIVATE` (proto code, taxonomy row), `--private-remote`, `--allow-workspace`, `Stored.private_remotes`/`allowed` and the rows' `private` field are gone. H26, H27 and H36 (the guard's rows) are removed; H11 now pins the opposite: a landed clone with a public remote and the source's inert origin gets its block, no URL appears anywhere, and the old flags are unknown. The fault row `rerender_over_edit` refuses on a target it cannot write (a directory at `AGENTS.override.md`) instead. |
| OI-1003-Q189 | `ready` = the commit rules AND a clean working tree in the live sibling and in every destination-only worktree (a read-only `git status` each). | `dirty`: `git status --porcelain=v1 -z --untracked-files=normal --ignore-submodules=none` through `git_carry::git` (`--no-optional-locks`, hooks and fsmonitor off), read on every run for the sibling (`Sibling.dirty`) and each destination-only worktree (`DestOnly.dirty`, in the inputs digest); a status that lists an entry or cannot be read is an open row. The block says which tree is not clean. H44, H45; H29 and H38 now settle the source's untracked `notes.txt` the sibling copy carries. |
| OI-1003-Q190 | Carried blocks are kept as content (Q185); opt-in `--clear-carried` removes them as a recorded, ledgered bulkload edit (intent, temp + fsync + exchange with displaced check, record), default hands-off. | `--clear-carried`, valid only with `--clear` (`FIELD_DOMAIN_VIOLATION` otherwise). `clear` removes the consecutive well-formed blocks at the head of a target's bytes under this landing's block (or alone) only when those bytes are exactly the landed blob (`landed_match`), so the removed bytes stay in the landing's objects; above anything else it keeps them (`carried-kept`). The intent names the removals (`HandoffIntent.stripped`, `carried`); after the exchanges they go to `handoff.stripped` (`HandoffStripped`), which `--clear` never removes. Converge: `landed_sides` adds a strip variant of every accepted prefix and the removal alone (`Prefix.strip`), only over the blob it was made from; `holds` compares the file with the blob after `strip` bytes; `landed_base` is strip-aware, so a later render over the stripped file vouches for it. A settle finishing a crashed clear keeps its flag and records the removals of the steps it finds done. H46, H47, unit `holds_accepts_a_recorded_removal_of_carried_blocks_only`, fault rows `estate_handoff_clear_carried_after_intent` and `_after_exchange`. |
| OI-1003-Q191 | Commit detection flags the token only in commits not reachable from the carried capture; a token already in source history is reported once as info, never as `HANDOFF_MARKER_COMMITTED`. | `history_adds_token`: `git log --branches --tags [HEAD] --not --glob=refs/carry/ -G TOKEN --patch --cc --unified=0 -- <every target name>`, flagged only when a hunk line marked `+` against every parent holds the token (a removal, or a line a merge took from a parent, adds nothing). `staged`: a token line in the index blob that the HEAD blob lacks. `in_source_history`: `git log --glob=refs/carry/ -S TOKEN`, reported as `"source_history":true` in the check row. The tip-blob and HEAD-blob scans are gone (the history scan covers them). H48. |
| OI-1003-Q185 | Nothing is ever dropped from a carry. | Held: Q190's removal is opt-in, only of landed content, recorded, and the source's bytes land again at the next converge of the target. |

### 19.5 Found by the mutation run of the round-2 commit

M31 (a block of this landing's identity on a target no record entry
names read as `Modified`) survived: with the whole-file rule of 19.2,
the round trip with no record is already carried content, so the
record-entry condition decided only the case of a lost record over
bytes that are not landed. There it presented this landing's own stale
block as carried, which finding 3 rules out. The rule is now one: a
block of this landing's identity that its record does not name is
`Modified` unless the whole file is the landed blob. H49 (record lost:
render and `--clear` refuse with nothing written, `--check` says
`modified`; removed by hand, the next render writes a current block)
failed first. M38 (a restored target kept in `retired` as well as in
`files`) survived H42 as written: the H step re-adopted the block after
a converge, whose new capture drops every retired entry anyway. It is
observable when the capture stays: a put-back block taken back by a
plain rerun, then replaced by a render (a new ref), then removed and put
back again, is recognised only if the record retired the new block, not
the old one. H42 now runs that round twice with a ref between them, and
M38 is killed.

### 19.6 TLA (EstateConverge)

The model gains carried values and Q190's write; Q188, Q189 and Q191
change nothing modelled (the guard was a precondition; `ready` and
commit detection are read-only). `docs/formal/README.md`, "TIN-6569",
has the details and the row table.

- `Cb(v)` (`CB` = 500, below `BLK`) is the bytes `v` under a carried
  block. With `"carried"` in `SrcKinds` the source may write `Cb(nextV)`
  at `HP`, and the operator's `OpBlock` may put `Blk(Cb(nextV))` (their
  bytes below the block starting with a pasted carried block).
- `hand` gains `u` (what a clear leaves), `cc` (`--clear-carried`), `ps`
  (the landed value a pending clear removes a carried block from) and
  `sp` (the one `handoff.stripped` records). `HandoffWrite` may clear
  with `cc` (`CarriedCut`: only from the landed value); `HandoffRecord`
  records `sp`; `HandoffSettle` finishes a pending clear with its flag
  and records a removal it finds done.
- `Holds` accepts `UnCb(v)` and `Blk(UnCb(v))` for a landed `v` the
  record or a pending intent stripped (`SOv`); `CrashAtomicity` and
  `Converged` use `WithHand`.
- Rows: `MC_ec_handoff_carried`, `MC_ec_handoff_carried_op` (pass),
  `MC_ec_reach_handoff_carried_cleared` (reach), and mutations
  `handoff_strip_no_overlay` (fails `TypedRefusal`) and
  `handoff_strip_unchecked` (fails `NoLocalWorkLost`).
- **Found while building the rows (not a hand-off finding):** with the
  operator's seat edits concurrent and `MaxSrc = 2`, the unmutated model
  violates `NoLocalWorkLost` with no hand-off step at all: the source
  removes `d` and adds it back; the converge's `Add` writes it; the
  operator deletes it in flight; the host crashes; the rerun's settle
  reads the absent seat as the `Add`'s old side and writes it again.
  The trace is the converge engine's settle classification, outside this
  lane; `MC_ec_handoff_carried_op` uses `MaxSrc = 1`, as
  `MC_ec_handoff_op` does. Open for the converge owner (19.10).

### 19.7 Failing first, for the rulings

On the round-2 commit (`752c8ec`), before any code change: H11 (the
render refused `HANDOFF_NOT_PRIVATE`), H44 and H45 (`ready` with an
untracked file in the sibling or in a destination-only worktree), H46 and
H47 (`--clear-carried` an unknown flag: `FIELD_DOMAIN_VIOLATION`) and H48
(`--check` exited `HANDOFF_MARKER_COMMITTED` for the source's committed
block): 6 failed. H49 failed on the same code (the render prepended over
the lost-record block). After the code: `estate_handoff` 45 passed, the 16
hand-off unit rows and the 18 `estate_handoff` fault rows passed.

### 19.8 Mutation tables

Runner: one edit at a time in a scratch copy of the tree
(`/dev/shm/handoff-lane/mut4`, `mut5`, with its own target directory),
each run against `cargo test -p bulkload-agent --lib handoff`, `--test
estate_handoff` and the `estate_handoff` fault-harness rows, stopping at
the first failing suite.

Round-2 commit (`752c8ec`):

| Mutant | Edit | Result | Killed by |
|---|---|---|---|
| M13_own_marker_read_as_foreign | the owner arm reads `Foreign` (re-review 2's BLOCKER) | KILLED | unit `a_racy_record_print_is_proved_by_reading` |
| M28_keep_equal_racy_print_unchecked | the keep path keeps a print equal to the seat's unchecked (re-review 1, finding 1) | KILLED | unit `a_kept_block_never_redates_a_racy_print_unchecked_on_a_retire`, `..._on_a_renew` |
| M29_sibling_ahead_consolidated | the history check reversed (finding 2) | KILLED | H38 |
| M30_user_config_lstat | the global configuration by `lstat` (finding 3) | KILLED | H36 |
| M31_round_trip_block_modified | an own-identity block with no record entry is `Modified` (finding 4) | SURVIVED | became the rule (19.5); H49 |
| M32_history_scans_targets_only | history of this run's targets only (finding 5) | KILLED | H39 |
| M33_whole_landed_ignored_intact | the record's block never landed content | KILLED | H40 |
| M34_whole_landed_ignored_modified | an own-identity block never landed content | KILLED | H41 |
| M35_retired_not_recognised | a retired block put back is not this landing's | KILLED | H42 |
| M36_retired_not_attached | converge ignores retired blocks | KILLED | H42 |
| M37_replace_kind_prefixed | a re-rendered created file becomes `prefixed` | KILLED | H43 |
| M38_retired_kept_after_restore | a restored target stays in `retired` too | SURVIVED, then KILLED in the rulings commit | H42 extended (19.5) |

Rulings commit:

| Mutant | Edit | Result | Killed by |
|---|---|---|---|
| M40_sibling_dirty_ignored | the sibling's status never read | KILLED | H44 |
| M41_dest_only_dirty_ignored | a destination-only worktree's status never read | KILLED | H45 |
| M42_open_ignores_dest_only_dirty | `open` ignores a dirty destination-only worktree | KILLED | H45 |
| M43_status_ignores_untracked | `--untracked-files=no` | KILLED | H44, H45 |
| M44_strip_overlay_off | converge ignores `handoff.stripped` | KILLED | H46 |
| M45_strip_any_content | a carried block removed above any bytes | KILLED | H47 |
| M46_strip_skip_ignored | `holds` compares from the blob's start | KILLED | unit `holds_accepts_a_recorded_removal_of_carried_blocks_only` |
| M47_strip_not_recorded | `handoff.stripped` never written | KILLED | H46 |
| M48_strip_base_unguarded | a removal accepted over any blob | KILLED | H46 |
| M49_finished_clear_drops_its_strips | a settle finishing a crashed clear drops its removals | KILLED | fault row `estate_handoff_clear_carried_after_exchange` |
| M50_carried_cleared_by_default | `--clear` always removes carried blocks | KILLED | H24, H25, H37, H40, H41, H46 |
| M51_history_includes_carried | the history scan reads the carried capture too | KILLED | H48 |
| M52_staged_ignores_head | any token in the index blob is `staged` | KILLED | H48 |
| M53_removed_lines_count | a removed token line counts as added | KILLED | H48 |
| M54_source_history_never | `source_history` never reported | KILLED | H48 |
| M55_lost_record_block_carried | an own-identity block not landed is carried (round 2's rule) | KILLED | unit `a_racy_record_print_is_proved_by_reading` (and H49) |

Round-2 commit: 10 of 12 killed; M31 became the rule and M38 was killed
by H42's extension in the rulings commit. Rulings commit: 16 of 16
killed, plus M38.

### 19.9 Checks for the rulings commit

In this worktree (`CARGO_TARGET_DIR` `cargo-target/handoff`, `nix develop
.#default`, nice 10), on the tree committed as the rulings commit: `cargo
fmt --check`; the three clippy passes (workspace; `io-trace`;
`fault-injection,io-trace`); `cargo test -p bulkload-agent --locked
--no-fail-fast` 938 passed, 0 failed, 7 ignored (29 binaries), with
`estate_handoff` 45 (H0 to H25, H28 to H35, H37 to H49; H26, H27 and H36
removed with the guard), `estate_adopt` 13, `estate_converge` 23,
`source_command_registry` 16, `refusal_taxonomy` 5, `no_credential_tools`
2 and 16 hand-off unit rows; `fault_harness` 115 (18 `estate_handoff`
rows); `just contract-test` (24); `just tla-render --check` (251 files);
`just tla-check` of the 17 hand-off rows with `MC_ec_refs`,
`MC_ec_seats` and `MC_ec_adopt`, 21 rows matched (`docs/formal/README.md`).
H42's second round was added after that run (the M38 row): `estate_handoff`
45 passed again, `cargo fmt --check` and the workspace clippy pass clean.
`docs/slo.md`: a copy of the hand-off's "Review of 2026-10-16" bullet that
`7c5407f` put inside the #226 adopt amendment is removed (the hand-off
amendment keeps it). `just check-fast` was not run here (coordinator).

### 19.10 Open

1. (16.2 and 17.5) Which copy carries the work forward for a landing
   beside a live copy; the WP10 usage guard exception versus a verb
   rename; whether the H step should re-render a refused item's own
   workspace. Answered since: 16.1 and 18.6.1 (Q188: no privacy
   question), 18.6.2 (Q189), 18.6.3 (Q190), 18.6.4 (Q191).
2. Q191's "reported once": built as one `source_history` field per
   workspace row of every `--check`, never a refusal. If "once" means
   only the first check ever, that needs state the read-only check does
   not keep; say so.
3. Q190: a converge that lands a new blob of the target lands the
   source's bytes, carried block included (the record speaks only for
   the blob it was made over). Re-applying `--clear-carried`
   automatically in the H step was not built: it would be a bulkload
   edit the operator did not ask for at that capture.
4. Commit detection reads `--branches`, `--tags` and HEAD, not
   remote-tracking refs or the stash (re-review 2 probed a block pushed
   on a branch then deleted locally). Unchanged here.
5. The model finding of 19.6 (the converge settle's `Add` over an
   operator's deletion in flight, at `MaxSrc` 2): for the converge
   owner; the code's behaviour was not checked in this lane.
6. The refuters' own lists (18.6.6) and a re-review of this section.

## 20. Re-review 3 (against `2b18a03`) and its fixes

Rulings: OI-1003-Q183, Q185 (nothing is dropped from a carry), Q188, Q189
(ready = commits and clean trees, read-only status), Q190 (the opt-in
`--clear-carried`, recorded), Q191 (only new commits are flagged; the
source's history is information), Q173, TIN-6569, R-N13. The reviewer's
probes (`/dev/shm/handoff-rereview3`, scratch: `tests/estate_handoff.rs`
`rr3_*`, fault rows `rr3_*`, logs `rr3_*.log`) were ported into the suite
as rows that failed on `2b18a03` first (20.2), then fixed. None was
rejected.

### 20.1 Per-finding table

| # | Severity | Finding | Row (failed first on `2b18a03`) | Fix | Outcome |
|---|---|---|---|---|---|
| 1 | MAJOR | Q185 residual: a round trip of the recorded block (landed blob `B1 + v2`) that the H step did not re-render (it refused for the workspace: an edit inside a created `CLAUDE.md`'s block, or a directory at `AGENTS.override.md`) was read as this landing's `Intact` block once the user wrote below it, because the rule asked whether the whole file was the landed blob. `--clear` then removed `B1`, and a render replaced it (`Plan::Replace`) | H50 (`--check` said `present`, not `carried`; the natural trigger, probe `rr3_a4`), H51 (the render dropped `B1`; probes `rr3_a`, `rr3_a2`) | `classify_head`'s and `unrecorded`'s question is now `lead_is_landed`: does the target's landed blob itself start with the block at byte 0 (a well-formed block's bytes; a broken one's as far as `marker` looks), from its start or after a recorded `--clear-carried` removal. Bulkload's block is never part of what the source landed, so a block the blob starts with came back with the source's bytes whatever is below it now. `landed_compare` (with `SameBytes.whole`) is `landed_match` with a prefix mode. A lost record (H49: the blob does not start with the block) stays `Modified`; H55 and H56 pin the rule's edges (a long landed body; a broken block) | Fixed |
| 2 | minor | The same root cause with no record: a round-tripped block of this landing refused render and `--clear` (`HANDOFF_BLOCK_MODIFIED`) after any edit below it | H52 (probe `rr3_c`; `--check` said `modified`) | The same rule (`unrecorded`) | Fixed |
| 3 | minor | Q190: a crashed `--clear-carried` finished after a user's edit left the removal unrecorded (the prior step counted as done only when the whole file's digest was the step's `after`), so converge, after the user reverted the edit, blamed the user for bulkload's removal (`GIT_DESTINATION_MODIFIED`) | fault row `estate_handoff_clear_carried_edit_revert` (probe `rr3_clear_carried_edit_revert`: `handoff.stripped` missing) | A prior step is done when the target no longer starts with the carried blocks it removed, under the step's old block or alone (`still_carries`); a missing file holds none. The model's `HandoffSettle` records `sp` by the same rule (20.4) | Fixed |
| 4 | minor | Q191: cherry-picking or rebasing a source commit that carries a block flagged `HANDOFF_MARKER_COMMITTED` (the pick adds the lines again, outside the carried capture's reach) | H53 (probe `rr3_e`; also the staged pick, `cherry-pick --no-commit`) | Commit detection compares token lines: `token_history` collects the token lines a `git log -p --cc -G TOKEN` adds (a `+` against every parent); a line is flagged only when the carried capture's history (`carried_token_lines`) never added it. Every BEGIN line names its block's id and `for=`, so this landing's block always has a new line. `staged` uses the same exemption (a staged pick is not flagged either); `source_history` is now "the carried history added a token line" | Fixed |
| 5 | minor | Q189: `dirty` ran `git status` with the repository's configured filters in the live sibling (a clean filter, git-lfs writing `.git/lfs/objects`), and a required filter that fails made the status fail, so `ready` could never hold | H54 (probe `rr3_b`: the filter's mark file written; then a required failing filter) | `dirty` runs `git_carry::unfiltered_status`: every `filter.<name>` driver the checkout's configuration (and its populated submodules') names gets empty `clean`, `smudge`, `process` and `required=false` as command-scope configuration (`GIT_CONFIG_COUNT`), the nest status's existing mechanism, split out as `unfiltered` (the registered injector; `nest_status` still refuses a nest with a filter command). The status compares raw bytes, so a filtered file whose index stat is stale reads as modified (an open row, never a false clean) until the checkout's own `git status` refreshes the index | Fixed |
| 6 | minor | Stale docs said an unrecorded block of this landing is content | review | `main.rs` usage, `render::owner`, `carried_of`, `classify_head`/`unrecorded`/`held` docs, `docs/design.md` (identity, removal, commit detection, `ready`, refusals), `docs/slo.md` (a re-review 3 bullet) | Fixed |
| 19.10.4 | open item | Commit detection did not read remote-tracking refs or the stash | H53 (a block pushed on a branch deleted here; a block in a stash entry below the newest) | The history scan adds `--remotes` and every stash entry (`refs/stash` and its reflog, `stashes`); the block's own step 4 says to stash nothing that holds it | Fixed (cheap) |

Not ported: probe `rr3_a3` (the user rewrote `AGENTS.md` as `B1 + v2 +
note` while the landed blob was still `v1`: `B1` is this landing's block
there and clearing it is right) and the crash probes
`rr3_round_trip_crash_after_landed`/`_after_head` (after the rerun apply
the H step had prepended a new block over the carried one; `--clear`
removed only that block and kept `B1 + v2 + note`, which is the intended
outcome; the probes compared with the pre-render file).

A case the rule leaves: a render that put a block byte-identical to the
carried one above it (`B1 + B1 + v2`) would read its own top copy as
carried after an edit below. A new capture changes the block's text (the
capture and ref facts it lists), so the H step cannot produce it; not
built.

### 20.2 Failing first

On `2b18a03` with only the new rows copied in (`/dev/shm/handoff-lane/base`,
scratch, rebuilt from its own sources): H50 (`--check` `present`), H51
(the remainder lost `B1`), H52 (`--check` `modified`), H53
(`HANDOFF_MARKER_COMMITTED` for the staged pick), H54 (the sibling's
clean filter wrote its mark) and the fault row
`estate_handoff_clear_carried_edit_revert` (`handoff.stripped` missing):
6 failed. H55 and H56 (added for the mutation run, 20.3) pass there.
After the fix all 8 pass.

### 20.3 Mutation table

Runner: one edit at a time in a scratch copy (`/dev/shm/handoff-lane/mut6`,
target `cargo-target/handoff-mut`), each run against `cargo test -p
bulkload-agent --lib handoff`, `--test estate_handoff` and the
`estate_handoff` fault-harness rows, stopping at the first failing suite.

| Mutant | Edit | Result | Killed by |
|---|---|---|---|
| M56_lead_whole_file | `lead_is_landed` asks whether the whole file is the landed blob (`2b18a03`'s rule) | KILLED | H50, H51, H52 |
| M57_prefix_compare_length_only | the prefix comparison checks lengths, not bytes | SURVIVED, then KILLED | H55 (added), H56 |
| M58_malformed_lead_empty | a broken block's lead is empty (never landed content) | SURVIVED, then KILLED | H56 (added) |
| M59_prior_strip_whole_digest | a prior clear step is done only when the whole file is its `after` (finding 3 as found) | KILLED | fault row `estate_handoff_clear_carried_edit_revert` |
| M61_history_carried_exempt_off | any added token line counts as committed (finding 4 as found) | KILLED | H53 |
| M62_staged_carried_exempt_off | any index token line HEAD lacks counts as staged | KILLED | H53 |
| M63_history_no_remotes | the history scan leaves out `--remotes` | KILLED | H53 |
| M64_history_newest_stash_only | only `refs/stash`, not its reflog | KILLED | H53 |
| M65_history_no_stash | no stash entry read | KILLED | H53 |
| M66_status_runs_filters | `dirty` runs plain `git status` (finding 5 as found) | KILLED | H54 |
| M67_status_required_kept | `required` is not overridden to false | KILLED | H54 |
| M33r_record_block_never_landed | the record's block is never landed content (re-run of M33 under the new rule) | KILLED | H40, H50, H51 |
| M34r_own_block_never_landed | an unrecorded own-identity block is never landed content (M34 re-run) | KILLED | H37, H41, H52, H56 |
| M55r_own_block_always_carried | an unrecorded own-identity block is always carried (M55 re-run) | KILLED | unit `a_racy_record_print_is_proved_by_reading` |

First run: 12 of 14 killed. M57 and M58 survived (every landed body in the
suite was shorter than a block, and no round trip carried a broken block):
H55 and H56 were added, both pass on `2b18a03` (rows for the new rule's
edges, not for a finding), and both mutants were then killed. 14 of 14
killed. Not run: `still_carries` checking only the bare carried blocks
(equivalent here: a step not done still starts with its old block and
the carried blocks, and the rerun's own clear removes and records them).

A measurement hazard found on the way: two source trees sharing one
`CARGO_TARGET_DIR` (`handoff-mut`) share fingerprints (paths relative to
the package root, compared by mtime), so a tree whose files are older
than the last build's outputs is not rebuilt. One base run of H55/H56 had
silently used the last mutant's library. Every base run in 20.2 was
redone after touching the base tree's sources (`Compiling bulkload-proto`
and `bulkload-agent` from the base path); each mutant's edit rewrote both
mutated files, so its build was fresh.

### 20.4 TLA (EstateConverge)

`HandoffSettle` records a pending clear's removal (`sp`) when the seat no
longer starts with the carried block (`~IsCb(Strip(cur.v))`), mirroring
`still_carries`; it was "the seat is exactly `UnCb(ps)`". The model cannot
show finding 3 itself: `OpSeat` writes a fresh value, so an operator's
edit is never reverted to the stripped value; no mutation row is added.
Findings 1, 2, 4 and 5 are in parts the model does not represent (a round
trip of this landing's own block; commit detection and `ready` are
read-only). `just tla-check` of the 17 hand-off rows with `MC_ec_refs`,
`MC_ec_seats` and `MC_ec_adopt`: 21 rows matched (the budget self-test
first), 417 s wall, `rc=0`; only `MC_ec_handoff_carried` (97864, was
97851) and `MC_ec_handoff_carried_op` (50313, was 50226) changed count
(`docs/formal/README.md`, "TIN-6569", re-review 3).

### 20.5 Checks for this commit

In this worktree (`CARGO_TARGET_DIR` `cargo-target/handoff`, `nix develop
.#default`, nice 10, TMPDIR `/dev/shm/handoff-lane`), on the tree
committed: `cargo fmt --check`; the three clippy passes (workspace;
`io-trace`; `fault-injection,io-trace`) clean; `cargo test -p
bulkload-agent --locked --no-fail-fast` 945 passed, 0 failed, 7 ignored
(29 binaries), with `estate_handoff` 52 (H0 to H25, H28 to H35, H37 to
H56), `estate_adopt` 13, `estate_converge` 23, `source_command_registry`
16 (the injector entry is now `git_carry::unfiltered`; `unfiltered` and
`unfiltered_status` are registered builders), `refusal_taxonomy` 5,
`no_credential_tools` 2 and 16 hand-off unit rows; `fault_harness` 116
(19 `estate_handoff` rows); `just contract-test` (24); `just tla-render
--check` (251 files); `just tla-check` as in 20.4. `just check-fast` was
not run here (coordinator).

### 20.6 Open

1. 19.10 items 1, 2, 3, 5 and 6 stand; item 4 is closed (20.1).
2. A filtered file in the sibling whose index stat is stale reads as not
   clean until the sibling's own `git status` refreshes it (20.1, finding
   5): conservative; say if `ready` should instead apply the filters it
   would refuse to run.
3. A stash holding this landing's block now exits
   `HANDOFF_MARKER_COMMITTED` (the block says to stash nothing that holds
   it); say if a stash should be information only.

## Workstreams (restated, R-N13; verified this session unless marked)

| Stream | Owner | Repo / branch | State and evidence | Next |
|---|---|---|---|---|
| TIN-6569 hand-off | this lane | bulkload `feat/estate-handoff-20261016` off `3fe1f07` | Built (17), reviewed (18), two re-reviews closed and OI-1003-Q188 to Q191 built (19), committed signed: `7c5407f`, then `752c8ec` (round 2), `2b18a03` (the rulings) and the re-review 3 commit (20); not pushed, no PR, no Linear write (this lane's task) | Coordinator: `just check-fast`, a re-review of section 20, PR; open items 19.10 and 20.6 |
| #226 adopt F1/F2 fixes | other lane | `feat/estate-adopt-20261015` at `c6ccd52` (seen, read-only) | Committed there; touches `converge.rs`, `converge/diff.rs`, `estate.rs`, `git_carry.rs` | Merge; conflicts with this branch checked by `git merge-tree` (lane return) |
| GF beside-landing | coordinator | production trees | Untouched by this lane | Hand-off after the operator answers 19.10.1 |
