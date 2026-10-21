# bulkload specification

Reset 2026-10-20 at the operator's request ("rewute to the actual spec? review
and cosnidate halluciated garbage?"). This page replaces the earlier
`docs/SPEC.md`, `docs/slo.md`, `docs/design.md` and `docs/intent-ledger.md`
as the statement of what bulkload must do. Those files remain in git history
and carry a superseded banner. Where anything disagrees with this page, this
page wins. It changes only when the operator says so.

## The operator's words

These are verbatim, typos kept, and they are the requirement:

- "setup sting as a interim dev box, moving over local agent / chat history /
  context and ~/git/ data AND all diffs, .git and worktrees etc" (07-18)
- "~/git should never have been hostage by this software. ther should never be
  an appointmetn needed" (08-26)
- "the jsonl, claude dialog and history etc mustn't be frozen during
  migration, and should be idempotently / memoize ontop clenaly" (08-29)
- "bulkload should beat rclone as a ~/git/ + all agaent data, federation,
  dots, sqlite / codex / wal / claude transcriopts / worktrees / TCFS native /
  estate solution" (08-29)
- "agent and devlopment work MUST NOT BE HALTED for bulkload to fnuction"
  (09-22)
- "mving complex agent setups in plaace FAST with compled git structures,
  without losing anything, so agents and plans and archives and dots and
  uncommited work etc is simply ready to go on a remote amchine" (10-02)
- "NOTHING is EVER excluded. period." / "full history. drop nothign. ever."
  (10-16)
- "we are coping in place, right? ... no more space than neo has should be
  needed on sting" (10-16)
- "network, idempotancy, streaming, copyonce, NOT interupt work on neo /
  source machine" (10-16)
- "no intermidiary stage patterns ... large, complex agent heavy states, with
  secrets, compelx tree and worktree patterns, compelx removables /
  rebuildable, compelx cacheables, complex transcripts, rdiffes, known
  rediverngeces, agetn recosnilations" (10-19)
- "no dupes, no duplciated besides, no stage dupes" / "a dupllicate is
  inherinatly not path for patyh / byte for byte" (10-20)
- "neither "WIN". they must be consoldiated. this is a non trivial task. this
  is what builload should be doing. NOT a fucking glorified rclone wrapper."
  (10-20)

## What bulkload is

bulkload consolidates a live developer estate from one machine into another
machine that is also in use. Both machines keep working throughout. Where only
one side has something, bulkload brings it across once. Where both sides
changed the same thing, bulkload merges both into one result at the one path.
It never copies a second version next to the first.

## Goals

1. **Consolidate neo's estate into sting, in place.** The estate is:
   - git repos, all refs and worktrees, uncommitted and ignored files;
   - Claude and Codex homes: sessions, transcripts, history, settings,
     plugins, Codex SQLite;
   - dotfiles and shell history, secrets.

   sting then becomes the development machine and neo a terminal.
2. **Beat rclone at this.** On this class of estate, bulkload must be faster,
   more parallel, use more bandwidth and do better peer to peer, end to end to
   a working seat. rclone appears only as the baseline in that measurement and
   never moves estate bytes.

## Done

1. The operator runs `claude --resume` and `codex resume` on sting in each
   lane and carries on the neo work: the same sessions, branches and
   uncommitted work.
2. bulkload's closure for the estate shows nothing unaccounted, every hold
   listed for the operator, and no duplicate bytes on sting.
3. sting's estate footprint is not larger than neo's plus sting's own work.
4. For goal 2, the measured end-to-end race shows bulkload ahead of rclone.

Only the operator says a lane is done. Byte counts, gate results and agent
reports are evidence, never status.

## Rules

Each rule is testable, and a change that breaks one is a defect.

1. **One mover.** Estate bytes move only through bulkload. No rsync, rclone,
   cp, scp, tar, hand git or lane scripts. When bulkload cannot do something,
   the gap is fixed in bulkload or the operator is asked.
   An agent never checks a lane by hand (comparing checkouts, transcripts,
   worktrees or sessions between hosts) and never offers the operator a
   checkout, switch, copy or other manual step to make a lane usable; the
   lane's state is whatever bulkload's plan and closure report. Operator,
   2026-10-21: "the manually checking and offering to hceckout is a
   dangerous antipatterhn that has been recurring for months and must never
   happen again, this is the whole asdkfjasdjlk point of bulkload".
2. **In place, copy once, streaming, on both machines.** Bytes go from
   neo's live paths straight over the wire into sting's live paths, through
   the path maps. Neither machine ever gets a staging copy: no capture
   bundle, repository copy, corpus, side root, landing area or overflow
   directory, on any drive. bulkload's private state holds ledgers and
   resume marks only, never payload. Every pass ends with its payload bytes
   in state on both machines printed on the closure line, and a test fails
   any pass that leaves more than its resume marks. Operator, 2026-10-20,
   after finding a full bundle and a full repository copy of every captured
   repo on neo's drives: "this pattern can never happen again."
3. **No duplicates.** There is one version per path. There are no beside
   copies, `.LABEL` names or numbered second copies. Duplicates already on
   sting are consolidated into the live path and removed.
4. **Consolidate, never pick a winner.** When both sides differ, the result
   contains both, merged by the rules in the next section. A case bulkload
   cannot merge yet is a typed hold: nothing is written, and the item is
   listed for the operator.
5. **Nothing excluded.** Truly rebuildable build output (`build/`, `dist/`,
   `target/`, `.svelte-kit/`, bazel links) is recorded by path, mode, size and
   hash in bulkload's ledger and not carried (operator, 2026-10-20). Nothing
   else is left out.
6. **Never halt or hinder.** There are no freeze windows, appointments or
   paused agents. bulkload never signals another process and never holds a
   lock a live writer waits on. It runs at normal priority (operator,
   10-18), respects free-space floors on both machines, and never fills a
   disk.
7. **Never clobber.** sting's own work is kept by consolidating it, never by
   overwriting it or setting it aside.
8. **Idempotent.** A rerun converges. After both sides change again, a new
   pass consolidates again and reads only what changed.

## How each kind of data is consolidated

| Data | Equal | One side ahead | Both changed |
| --- | --- | --- | --- |
| Git repo | adopt | refs come in; a clean checkout advances or switches to the other side's branch | one repo holds both sides' refs; uncommitted changes are merged file by file against the common commit; real conflicts are held, typed |
| Git worktrees | adopt | registered once at the mapped path | union; each worktree registered once |
| Claude transcripts, Codex rollouts, `history.jsonl`, todos (append logs) | adopt | the missing tail is appended in place | records from both sides joined by identity (message `uuid`, or the line); both branches of a session are kept in the one file, and the result is byte-identical to neo's where they share history |
| JSON and TOML config (`settings.json`, `.claude.json`, MCP config, Codex `config.toml`) | adopt | written | structural merge: objects key by key, lists unioned, host paths translated through the maps; a key with two values takes the later one, and the other is kept in bulkload's ledger |
| Codex SQLite | adopt | rows come in | rows merged into sting's live database by primary key, streamed from neo's held read snapshot; no database copy is written; a key with two versions takes the later one, and the other is kept in the ledger |
| Other files | adopt | written | typed hold until a rule exists |
| Rebuildable build output | ledgered | ledgered | ledgered |

## Out of scope

Anything that does not serve the two goals is not part of bulkload. That
includes new ruling IDs, boards, checkpoint cadences, papers, formal models
and rig programs. Tests, the local merge gate on honey (builds never on neo or
sting) and the Darwin check on pzm stay. They serve the rules above.
