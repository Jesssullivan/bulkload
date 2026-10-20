# bulkload intent ledger

> **Evidence only, 2026-10-20.** The operator's own words recorded here stay valid evidence. The requirement IDs and agent paraphrases do not. [SPEC.md](SPEC.md) is the requirement and wins on any disagreement.

This file is the evidence behind [SPEC.md](SPEC.md). It records what the
operator said bulkload is for, word for word. Every requirement in SPEC.md
cites entries here by ID (for example `RES-12`). If a SPEC requirement cannot
cite an entry, it is not a requirement.

The operator's statement of 2026-10-16 that produced this file:

> unclear how that can possibly be misunderstood; [...] this intent has not changed.

## Rules for this file

- **Append only.** Entries are never rewritten or deleted, including under
  R-N55 ("current truth only"). A later statement that changes an earlier one
  is a new entry, and the SPEC cites the newer one. On 2026-09-23 the last
  verbatim record of the operator's intent was deleted from `main` (see
  "Where intent was lost", L4). This file must not go the same way.
- **Verbatim.** Quotes keep the operator's spelling, typos and spacing.
  `[...]` marks an elision. `[host]` replaces a private host name and `[repo]`
  a private repository or client name. Secret values never appear.
- **Speaker.** `operator` means the operator typed the words, either as a
  prompt or as a free-text interview answer. `ruling-record` means the words
  are an interview option the operator selected, or a ruling text that
  records an operator decision. Agent wording never appears as an entry. Where
  agent wording matters, it is quoted under "Where intent was lost" and
  labelled as agent text.
- **Host** is where the words were typed: neo (the teletype) or sting (the
  destination). Times are UTC.
- **Annotations (added in round 2).** A correction to an existing entry goes
  beneath it as an `*Annotation (2026-10-16, round 2):*` line. The entry's
  quote, speaker and original context stay as they were written. The analysis
  sections ("Where intent was lost") and the first conflict list are
  corrected the same way.
- **Selections.** A `ruling-record` taken from an interview quotes the label
  of the option the operator selected. Its context gives the question and the
  rejected options. "(Recommended)" marks the agent's recommendation. A
  selection records an operator decision; the option's wording is the
  agent's.
- **Redaction (round 2).** Client and private repository names become
  `[repo]`, and account e-mail addresses become `[account]`.

## Sources

- **D:N** means line N of `docs/wayfinding-20260829/dialogs-interviews.md` at
  commit `16cf146`, which is in this repository's history. Read it with
  `git show 16cf146:docs/wayfinding-20260829/dialogs-interviews.md`. It is an
  extract of the neo Claude session for 2026-07-18 to 2026-09-02.
- **Claude session `<id>`** means the transcript `<id>…jsonl` under
  `~/.claude/projects/`. sting holds copies of neo's transcripts. The same
  message often appears in several copies with different session IDs (see L9).
  The earliest copy is cited.
- **Codex session `<id>`** means the rows with that `session_id` in
  `~/.codex/history.jsonl`. neo and sting share most of that history.
- **Linear** means the operator's private workspace: TIN-3692 (estate and
  sting), TIN-4543 (bulkload engine and rulings).
- **Forked session IDs (round 2).** On 2026-10-01 a Codex repair on sting
  (`/srv/data/jess/neo-claude-repair-20261001T150701Z`: `HANDOFF.md`,
  `RESUME-MATRIX.md`, `receipts/fork-history-*.json`) minted per-checkout
  native forks of neo conversations. Each fork has a new session UUID, with
  `sessionId` and `cwd` rewritten; the originals were kept. Several IDs cited
  in this file are such forks, not neo sessions:
  - `79b8ac1b` and `cc0defc6` are forks of neo `3025961c` (the session D
    extracts);
  - `a2a9c640`, `426120c2`, `db079833` and `f61ee119` are forks of neo
    `bb510cc3`;
  - `2cc731d7` is a fork of neo `f1c4ead8`; its records from 2026-10-02 on
    were typed on sting;
  - `c6387a0e` on sting is a path-rewritten copy of neo `9384e272`.

  Round-2 entries cite the neo original with the sting fork in brackets.
  Earlier entries keep the IDs they were written with: read `79b8ac1b` as neo
  `3025961c`, and `a2a9c640` as neo `bb510cc3`.
- **Round-2 sources.** Operator queued messages (`queued_command`
  attachments) in every main-session transcript on sting; neo-only Claude
  transcripts; Junie history and sessions on neo; pi sessions; the 09-30
  transfer receipts and the 10-01 repair receipts on sting; Codex rollout
  `01a0f317` interview answers; the 2026-08-29 custody archive
  `bulkload-migration-20260829.tgz` (`BASES-20260828.md`); Linear TIN-3046,
  TIN-4543 and TIN-5268.

## How the entries were checked

Six mining passes (Claude transcripts on sting and on neo, Codex history,
plans and notes, Linear, GitHub, and worktrees with git history) produced
candidate quotes. Three adversarial reviews then compared SPEC.md against
those quotes. Every `operator` entry below was string-matched against its
source, after whitespace normalisation, in a corpus of 508,085 operator
messages and interview answers taken from every main-session transcript on
sting and from sting's Codex history. Three entries that exist only on neo
(RES-06, LIVE-19, LIVE-20) were matched read-only on neo. `ruling-record`
entries were matched against the file or Linear comment cited. Candidate
quotes that could not be matched were dropped.

**Round 2 (2026-10-16).** Five more read-only passes:

1. every operator queued message in sting's 417 main-session transcripts
   (148,822 records, 1,632 unique texts, all 401 on-topic texts read);
2. neo-only transcripts for 09-30 to 10-16 (2,816 operator items, with each
   interview answer read beside the question and options it answered);
3. the 09-30 and 10-01 transfer and repair archives on sting, Codex rollout
   `01a0f317`, and the 08-29 custody archive;
4. other harnesses and hosts (Junie, pi and opencode on neo and sting; yoga,
   honey, mbp-13 and one more host);
5. Linear and the OI interview records.

Every new `operator` quote was string-checked against its source record.
Candidates that could not be string-matched are listed under "Unverified
candidates" and are not entries. Agent text found in these passes appears
only under "Where intent was lost", labelled as agent text.


## Count by theme

| Theme | Round 1 | Added in round 2 | Entries |
|---|---:|---:|---:|
| PUR: Purpose, and how bulkload relates to tcfs | 13 | 8 | 21 |
| RES: Carry on at the destination | 13 | 10 | 23 |
| SEAT: The operator's seat on DEST, and what done means | 9 | 24 | 33 |
| KEEP: Nothing is dropped | 19 | 19 | 38 |
| LIVE: Live hosts: never halt SOURCE, never clobber DEST | 21 | 22 | 43 |
| ONCE: Never twice: piecemeal, idempotent, no re-walk or re-read | 8 | 9 | 17 |
| FAST: Faster than rclone | 12 | 12 | 24 |
| FOOT: Footprint and host hygiene | 7 | 14 | 21 |
| HAND: Hand-off to the agent on DEST | 2 | 3 | 5 |
| PROC: How the work is done: no invented gates, no false completion, no re-asking | 17 | 10 | 27 |
| CONF: Statements that pull against each other | 5 | 16 | 21 |
| RUL: Rulings on the round-2 conflicts (OI-1003-Q194) | 0 | 20 | 20 |
| **Total** | **126** | **167** | **293** |

*Annotation (2026-10-17): FAST-25 and PROC-28 were appended after round 2 (review row K06); the counts above are as of round 2.*

## PUR: Purpose, and how bulkload relates to tcfs

- **PUR-01** · operator · 2026-07-17T20:26:52Z · neo · Codex session `019f71bd` (`history.jsonl`); re-pasted in Claude session `79b8ac1b` 2026-07-18T13:54:45Z; D:23

  > setup sting as a interim dev box, moving over local agent / chat history / context and ~/git/ data AND all diffs, .git and worktrees etc to sting's fast NVME srv env for cmux-agent connection from neo, as neo is currently too constrained.  eventually this migration work wil become completely obsolete, as this very housekeeping work is what tcfs seeks to seamlssly fix as a entire class of work.

  Founding statement (Codex prompt). Re-pasted into Claude 2026-07-18T13:54:45Z.

- **PUR-02** · operator · 2026-07-17T22:55:06Z · neo · Codex session `019f71bd` (`history.jsonl`); D:73

  > Indeed, precicely. well asserted.

  Endorses the agent restatement quoted in the same message: Sting is temporary infrastructure and a dogfood case for the class of work tcfs should erase; the work must converge back toward tcfs, not become a permanent migration program.

- **PUR-03** · operator · 2026-05-31T02:52:16Z · neo · Codex session `019e7b8d` (`history.jsonl`)

  > I think we should work on mannually copying / preparing / mirroring in a tcfs ready way after deep planing and review alll codex, claude, opencode  dots, squlite, transcipts, tmmp logs etc etc and choice repos [...] such that these complex repos can be picked up from [host] and server +1 enrolment first.

  Earliest statement of the interim pre-copy ('staging before tcfs') intent. The target host later became sting.

- **PUR-04** · operator · 2026-07-22T17:00:55Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > design and implimnet a fresh, design document for a detailed agent skill (md, code, bazel SSOT, one line installeable for pi / claude / codex) for recrating these one-off bulk syncs, both for a single repo (such as tummycrypt, with all its complexities, agent dialog, agent transcripts, sessions, ciontext , thee eddges and specifics etc) and for all repos in ~/git/

  Founding prompt of this repository.

- **PUR-05** · operator · 2026-08-26T13:24:32Z · neo · Claude session `79b8ac1b`; D:7035

  > also, we are in tummycrypt, wjhich is a project that should be capable or making this all happen natviely

  *Annotation (2026-10-16, round 2):* `79b8ac1b` is a sting fork of neo
  session `3025961c` (see Sources, forked session IDs). This applies to every
  entry that cites `79b8ac1b`.

- **PUR-06** · operator · 2026-08-26T13:26:21Z · neo · Claude session `79b8ac1b`; D:7039

  > bulkload is a steppingstone repo to a very clear end state, that is expected to be complex and recurring (ideally infrequentlyy) enough towarrrent a repository.

- **PUR-07** · operator · 2026-08-28T22:49:05Z · neo · Claude session `79b8ac1b`; D:7426

  > examine bulkload for what it should be (bulk dump complex lab / dots / lab style auth  / claude / codex  / pi / squlite / transcripts / wroktrees etc under the TCFS umbrella as a super fast, prouction level tool; I am keen to make this public eventually, assuming it actually supercedes AX wise base rclone.

- **PUR-08** · operator · 2026-10-02T16:39:14Z · sting · Claude session `2cc731d7`

  > Hmm, the completion goal of the bulkload package is to 1. beat rclone and 2. complet full work migration to sting.  this is pretty well declared in our goals I think.

  Interview answer.

  *Annotation (2026-10-16, round 2):* The answer continues: "we should run
  this once bulkload canidates are complete and SLOs / feature completeness is
  in place." The quote above stops at "I think." without an elision mark. The
  dropped sentence is the operator's source for OI-1002-Q24. In it, "once"
  reads as "when", not "a single time" (see L10). The question was "What
  should the next step toward all work on sting be?"; the options were a
  read-only coverage inventory (Recommended), pushing the M2 gates, and
  carrying cohort 4 now.

- **PUR-09** · operator · 2026-10-15T04:28:01Z · sting · Claude session `2cc731d7`

  > the real intent of tcfs (files anywhere, no need to copy whillyneilly, the real antidote to what bulkload provides asa interm soution, itself not quite done or reaching its SLOs

- **PUR-10** · operator · 2026-10-15T05:06:15Z · sting · Claude session `2cc731d7`

  > a bit of both; if we can beat rclone for this agent heavy, compled dev inv preseeding, htis is a poblishable piece of work.  TCFS is the real product story here though, years in development and posibbly yeasr out.

  Interview answer: bulkload status and finish line.

  *Annotation (2026-10-16, round 2):* The answer continues: "ideally much
  sooner, but we have significant complex hurdles to make things tick." The
  operator declined "Interim + finish line (Recommended)" (freeze scope, plan
  a sunset in favour of tcfs) and "Permanent product".

  *Annotation (2026-10-16, round 2 verification):* "a bit of both" is a blend of
  the interim and permanent options, not a decline of either. Conflict C20
  listed it on the "no sunset" side only; it supports both. Ruled by RUL-20.

- **PUR-11** · operator · 2026-10-15T15:36:55Z · sting · Claude session `2cc731d7`

  > I want to resume [repo] work from sting, which is missing work from neo. this is where bulkload should be used.  once tcfs is working, this wouldn't matter / git dir tenanncy will be irrelvant

  Interview answer.

- **PUR-12** · operator · 2026-10-16T15:06:59Z · sting · Claude session `2cc731d7`

  > whiy not?  that is fundamentally the only core user expectation.  why is the specs and rewuiremtns for bulkload seeming to be completely forgotten over and over?  instenrt there a nclean spec?

  Reply to the agent saying `claude --resume` on sting would not find the neo conversations. Refers to RES-12.

- **PUR-13** · operator · 2026-10-16T15:07:55Z · sting · Claude session `2cc731d7`

  > that and the objective to prove our system can be faster than rclone for complex networks and extensive multifile system, staging work data pre TCFS launch.  thats really it.

  Typed mid-turn (queued message). With PUR-12 this is the operator's own two-objective framing of SPEC.md.

- **PUR-14** · operator · 2026-07-18T14:00:46Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > [...] none of that needs to be addresed or reinvented here. we simply want to pickup our development and agent work from neo on sting in its NVME avaialblespace

  "none of that" is the lab Home Manager repo's fish and cmux work, which the agent was told not to redo. Earliest Claude-side statement that the goal is picking up development and agent work on sting.

- **PUR-15** · operator · 2026-07-18T14:02:26Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > noting this interim migration of chat / data / git worktrees, repos and all (ALL) files / dots relating to our work is a seperate task from TCFS, which is a class of work (moving files around from amchien to machine in a complex fashion like this, a complex onprem development org enviornment) is sometihgn broadly the TCFS project seeks to fix

  The migration is separate from tcfs and covers all files and dots. tcfs is meant to remove this class of work.

- **PUR-16** · operator · 2026-07-23T17:44:29Z · neo · Codex session `019f71bd` (`history.jsonl`)

  > lets also fan out read only here to explore movement since we last checked i, the bulkload repo for one offs, the state of the repo, worktrees, linear initatives for tcfs / tummycrypt and the contract the the type of bulk copying work that this product seeks to address completely

  "this product" is tcfs/tummycrypt. The bulkload repository is named as the place for one-offs; tcfs is the product meant to address this class of bulk-copy work completely.

- **PUR-17** · operator · 2026-08-23T19:28:49Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message; re-pasted 2026-08-24T05:13:09Z

  > [...] as we close out thsi complex migration / interim TCFS ajacent work before getting back to the TCCFS FS greenfield work. [...]

  The migration is interim work next to tcfs, and closes before tcfs greenfield work resumes.

- **PUR-18** · operator · 2026-09-12T22:23:51Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > thus the requiremrnt / tilt to build oru migration tool, migration is not trivial and may have to happen more than once.

  Why a tool and not a one-shot copy: the migration recurs (compare PUR-06, SEAT-11).

- **PUR-19** · operator · 2026-09-23T12:40:57Z, 12:41:06Z, 12:41:23Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), three queued messages joined with " / "

  > bulkload is a sepearte repo / and should not be cross polluting TCFS repo / these are intertwined but discreet projects

  Sets the repository boundary between bulkload and tummycrypt/tcfs. It supersedes R34 and the operator's 2026-09-12 selection "Rust in tummycrypt — finish M2 (Recommended)". At 12:44Z the operator selected "Move all bulkload Rust into the bulkload repo (Recommended)".

- **PUR-20** · operator · 2026-10-15T17:04:18Z · sting · Claude session `2cc731d7`, queued message

  > remember, this shoudl bulkloads bread and butter, interim flagfip data mover as a precursion to the solutions we want TCFS to provide

  Typed while the agent landed a family on sting by swapping live paths with a hand-written script. Landing a lane at its live path is bulkload's core function (bulkload#226).

- **PUR-21** · operator · 2026-08-17T19:24:22Z · neo · pi session `01a01127` (cwd a [repo] checkout; copy on sting)

  > you may be interested to view my tummycrypt / TCFS filesyetm project, which hopes to add such gsuite integration as async target buy that'll only likely happen ifn a few yeras..

  Not about bulkload: a research prompt about mounting Google Drive. Relevant only as the operator's own timeline for tcfs (compare PUR-10).

## RES: Carry on at the destination

- **RES-01** · operator · 2026-06-08T18:19:46Z · neo · Codex session `019e7b8d` (`history.jsonl`)

  > I should be able to on the fly traverse ~/git/ on [host] and pickup agent sessions, in-progress uncommited development work (as the TCFS overlay filesystem provides) and just keep working

  Pre-sting framing of the same expectation.

- **RES-02** · operator · 2026-07-17T20:21:01Z · neo · Codex session `019f65d0` (`history.jsonl`); repeated 07-19, 07-28 (D:1461), 08-16

  > hoping to review and continue the sting dev box initiative, as well as geneerally motor ahead toward being able to seamlessly picup my agent work and contexts / history on sting before we resume deep TCFS frontier work.

- **RES-03** · operator · 2026-07-17T20:47:56Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > I'd like to proceed on the interim migration work / cmux-agent / dev box sting laceup complete so I can continue my work and agent eesssions, dialogs and authenticated developemtn worth from sting, instead of neo.

- **RES-04** · operator · 2026-07-29T23:50:25Z · neo · Claude session `ab9a39eb`

  > please also ensure all f this work, repos, worktres and dialogs / dsicussion etc is present on sting in the dev enironment.

- **RES-05** · operator · 2026-08-23T19:26:20Z · neo · Codex session `019f65d0` (`history.jsonl`); also pasted in Claude session `79b8ac1b` 19:25:17Z

  > ensure an agent and a  user can walk through cleanly and by the end of a day , be happoily wiorking away from the sting / remote machine post migration, back at it wht the agent and file histry and paths just like that.

- **RES-06** · operator · 2026-09-13T01:13:28Z · neo · Codex session `019f65d0` (`history.jsonl`) (neo history only)

  > Hmm, eGreg is just one of many repos.  My assuption bulkload and our work tonight wil result in full migration efficiently, such that actual work can continue from sting ass soon as possible.

  *Annotation (2026-10-16, round 2):* In the source the text ends with a
  trailing "+": "ass soon as possible.+".

- **RES-07** · operator · 2026-09-27T04:40:07Z · neo · Codex session `01a0e05d` (neo history; quoted back in Claude session `bf26e046`)

  > codex resume must be the gate that must be met.

  *Annotation (2026-10-16, round 2):* Session `01a0e05d` recovered neo's own
  Codex SQLite/WAL databases after a crash. It was not a neo→sting
  migration. The turns before were "are we ready / written back so I can
  reseum work in lab a glorious.build and other repos?" (04:38:51Z) and
  "plesae run thouse yourself now" (04:39:47Z). Cite RES-07 as a general
  principle: the operator's real `codex resume`, run by the agent itself, is
  the acceptance test for any repair of agent state. It is not a ruling
  about migration.

- **RES-08** · operator · 2026-09-30T16:14:52Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > I want to, as soon as possible, be able to run cd ~/git/tummycrypt && codex resume and enter the most recent tummycrypr codex session from neo on sting and continue work.

  Typed on sting.

- **RES-09** · operator · 2026-09-30T21:58:11Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > such that agent work can --resume / resume directly from sting.

  Same sprint brief; covers claude --resume and codex resume.

- **RES-10** · operator · 2026-10-01T14:24:40Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > the claude --resume does not work yet (the expected trascuiprs and sessions did not make it)

- **RES-11** · operator · 2026-10-01T17:26:37Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > shall we set a /goal that'll complet wehen both claude and codex sessionss for the named repo familes are ready to resume?

- **RES-12** · operator · 2026-10-16T15:05:32Z · sting · Claude session `2cc731d7`

  > they will just have some mine idemoptent catching up to do for full latest claude --resume from sting instaed of neo to carry on converations and byte identical transcriptsand worklanes?  will already be the case for all the other neo lanes, correct?

  The 'carry on at the destination' expectation stated as the operator understood it; R0's source.

- **RES-13** · operator · 2026-10-02T01:00:26Z · sting · Claude session `2cc731d7`

  > mving complex agent setups in plaace FAST with compled git structures, without losing anything, so agents and plans and archives and dots and uncommited work etc is simply ready to go on a remote amchine.

  The operator copied this session to sting by hand because bulkload had not done it.

  *Annotation (2026-10-16, round 2):* The full sentence is: "note I copied
  this session from neo to sting (where we are now) manually, but a rclone
  beating paradigm and bulkload completeness (reasserting the sematic goals;
  mving complex agent setups in plaace FAST with compled git structures,
  without losing anything, so agents and plans and archives and dots and
  uncommited work etc is simply ready to go on a remote amchine." The quote
  above omits the opening without an elision mark. The context line is agent
  inference. Session `2cc731d7` on sting is a fork that the 10-01 Codex repair
  minted from neo `f1c4ead8`. Neither that repair nor the operator's manual
  copy used bulkload.

- **RES-14** · operator · 2026-07-22T16:41:05Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > should I continue frmo sting, and are we sure all worktrees and their names aer synced?

  Two minutes after the operator pasted a sting terminal running `codex resume` in a tummycrypt worktree. The earliest resume-on-sting attempt found. The agent answered (agent text): "No, the complete worktree estates and names are not synchronized".

- **RES-15** · operator · 2026-07-24T16:17:03Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > perfect.  lets dive in and push this work forward, fanning out wide and addessing the P0 / P1 blockers first here.  we can defer the reauth / on box reauth patterns as these are easy for operator to drive; the dialogs, worktrees, agent session parity, fixing erroneous overdides, really pushing for these to progress.

  The priorities are dialogs, worktrees and agent session parity. On-box re-auth is called deferrable operator work (conflict C7).

- **RES-16** · operator · 2026-09-12T22:25:25Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > also, since we are in a claude sessionn, being able to continue work without interuplion is *why* we wanted to build bulkoad, not for the "manifest" deliberation; accuracy / static manifst < acually able to complete the migration and pickup work from sting instead of neo for most repos, excpect perhaps this one for now.

  Ranks carrying on at the destination above manifest accuracy. "excpect perhaps this one for now" refers to the session and repo driving the work (KEEP-38, conflict C19).

- **RES-17** · operator · 2026-09-20T23:18:37Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > [...] our goal tonight shoudl be to address all of thse, reulsitng in updated bulload, deployed adn completed migration without needing to halt any agents ensure all git repos from neo are properlu and correctly reporting the correct state as on neo (seamless ppickup where devleft of on neo on sting) [...]

  The elided parts concern the tinyland-cleanup daemon and LGTM.

- **RES-18** · operator · 2026-09-30T22:09:00Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > I also notice we may not have fullly succeeded with bulkload and tummycrypt lanes [...] thread/read failed during TUI session lookup

  The elision is a pasted sting terminal (`cd git/tummy`, `codex resume`, "Error: Failed to resume session from …"). This is the operator's own resume test, 3.5 hours after an agent's `report.json` declared the goal complete on the strength of a native thread read (L1).

- **RES-19** · ruling-record · 2026-10-01T14:34:16Z · sting · Codex session `01a0f317`, interview answer `umbrella_session_visibility`

  > Each target repo

  Question: where should sessions appear whose work happened in an umbrella conversation? Selected option text: "Also make the umbrella conversations available from each target repo's resume picker, while retaining their original history and context." Rejected: "Original context (Recommended)".

- **RES-20** · ruling-record · 2026-10-01T14:49:57Z · sting · Codex session `01a0f317`, interview answer `umbrella_resume_identity`

  > Per-repo forks (Recommended)

  Option text: "Keep the original sessions and create native forks with full history, so each repo resumes directly and develops its own continuation." Rejected: "One shared continuation". The 10-01 repair applied it to all seven families: 323 Claude and 244 Codex forks with new UUIDs and rewritten `sessionId` and `cwd`, originals kept. See conflict C8.

- **RES-21** · operator · 2026-10-08T06:11:54Z · sting · Claude session `2cc731d7`

  > let me know when I can resume [repo] related work on sting, aswell as when I can resume [repo] / platform work.  where is bulkload product shape at?

  The operator's acceptance signal for a family move is resuming work on sting.

- **RES-22** · operator · 2026-10-15T20:47:59Z · sting · Claude session `2cc731d7`

  > lest check in.  GF lane on neo is also good to move. [...]

  Confirmed at 20:53:52Z by selecting "Yes, move GF to sting" (Q172; option text: carry GloriousFlywheel — main, 31 worktrees, sibling clones, dirty files, stashes — neo→sting via bulkload, then tell the GF session on neo to continue on sting).

- **RES-23** · ruling-record · 2026-08-13T02:30:25Z (Linear post time) · — · Linear TIN-3046 comment 213b15f6 ("Operator statement, verbatim carrier")

  > the criterion was already accepted, we have code sessions running right now from resumed lanes.

  About TIN-3046, which moved neo's own `~/.codex` to an external volume on neo; not neo→sting. A precedent: an agent-state move is accepted when live sessions resume from the moved state in real use.

## SEAT: The operator's seat on DEST, and what done means

- **SEAT-01** · operator · 2026-08-22T02:46:35Z · neo · Claude session `79b8ac1b`; D:2153; resent in Codex session `019f65d0` (`history.jsonl`) 2026-08-22T20:47:47Z

  > The success metric is all the Bulkload → sting dev box migration lane work completed and shipped, our work ACTUALLY living on sting and ready for active emacs and ssh / cmux / tmux sessions for work from Sting instead of from neo.

- **SEAT-02** · operator · 2026-08-22T21:09:47Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > the nearest term (AS SOON AS POSSIBLE) is working all agent, dots, emacs,, agents (codex, claude, pi) all avcitively moved and al git , dots etc vis bulkload to sing where dailiy extant work as can be observed today on neo on sting; I have vast amount of work all bottlenexted on the neo teletype macbook neo, all of which should be on sting for remote work.

- **SEAT-03** · operator · 2026-08-23T19:24:51Z · neo · Claude session `79b8ac1b`; D:4813; Codex session `019f65d0` (`history.jsonl`)

  > we'll need to establish  user / AX / UX for the following movements of me the user (tmux, cmux, remoet mux agent, eGreg afrom GUI egreg and/or TUI eGreg, restarting the local processes and agent history, establishing MCPs still work, linear connectivity etc

- **SEAT-04** · operator · 2026-08-28T21:34:36Z · neo · Claude session `79b8ac1b`; D:7405

  > noting I expect to be working from sting via eGreg ASAP, it is already 5:30

- **SEAT-05** · operator · 2026-09-12T22:06:44Z · neo · Claude session `a2a9c640`

  > Our goal is to have completely completed the sting as the defactor dev machine, with all the neo ~/git, ~ dot dirs, dots, configs etc as described by bulkload and the migration goal as soon as possible, next hour or two.  If a simple dedupe and copy is possible, do it.

  *Annotation (2026-10-16, round 2):* `a2a9c640` is a sting fork of neo
  session `bb510cc3` (see Sources, forked session IDs). This applies to every
  entry that cites `a2a9c640`.

- **SEAT-06** · operator · 2026-09-30T18:30:48Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > so loong as all files, dots, uncommiteds, branches, git work and codex resume abilty is ready, that is completion metric for goal

  Typed on sting.

  *Annotation (2026-10-16, round 2):* SEAT-06 and KEEP-16 come from one
  message (full text under KEEP-16). The completion metric was set for the
  09-30 tummycrypt/bulkload sprint (KEEP-26).

- **SEAT-07** · operator · 2026-08-25T11:05:12Z · neo · Claude session `79b8ac1b`; D:6596

  > I do not want tow ait untile wednesday, I expected this to be complete for work today due to your assurace. please prioritize actaully completing this as soon as posssible.

- **SEAT-08** · operator · 2026-08-26T14:31:58Z · neo · Claude session `79b8ac1b`; D:7065

  > no.  I need to work now in git.  this project has failed.

- **SEAT-09** · ruling-record · 2026-09-12 · neo · Operator-approved plan `~/.claude/plans/please-focus-on-the-parsed-wren.md` (copy on sting)

  > Phase 4 is the only acceptance test that counts:

  Operator-approved plan, 2026-09-12; the sentence continues: a real Claude Code session on sting, in a real repo, producing a real commit and a real push.

  *Annotation (2026-10-16, round 2):* The quoted sentence is agent-worded
  plan text that the operator approved. It is not the operator's own words
  and is not, on its own, the basis for any requirement. SPEC's commit-and-push
  check rests on RES-03 ("authenticated developemtn") and SEAT-01, and SPEC
  labels it as design.

- **SEAT-10** · operator · 2026-10-03T03:58:43Z · sting · Claude session `2cc731d7`, interview free text (OI-1003-Q4)

  > product SLO provable, work and feature complete, multipe proofs / whitepapter runtume proofs ready to demonstrate; multiple prior art references adn biliography.  SLO must be definable. mukltiple simplifactions, refactors, adversioal architecue, STOA parallelization . preformance seeking structure examinations, code and feature spaeawl recodetion andmigration in full; we should be confudent in our lab sahpe (neo to stng( and the abilty to cleanrly manifest / define this migration bulkload relationship confidently enough Uawiht the core LSos remaining faster than rclone, extnat machin's agents and work must not be interupted,abiliy to complete migration edges as local neo worklanes complet by rerunning bulkload sync with not rewalked / reread bits (data idempotency)

  Answer to "Priority order for bulkload work from here?". The operator rejected all three options: "Gates first, then pre-migration, then run (Recommended)", "Correctness first, gates after" and "Ship migration sooner". This is the full text of OI-1003-Q4; ONCE-07 and FAST-09 quote its tail. It ranks provable, definable SLOs and proofs first, and keeps "migration in full" in the same bar.

- **SEAT-11** · operator · 2026-10-03T04:03:13Z · sting · Claude session `2cc731d7`, queued message

  > (ie, comcimoeet means confidence in our code and bulkload product; migration in full from our neo to sing should be eprcived as just another execuion of somthing we trust, not a major schedulable event)

  Typed between the charter interviews. "comcimoeet" is most likely "completion". The operator's own source for OI-1003-Q13 (ONCE-08).

- **SEAT-12** · ruling-record · 2026-10-03T03:58:43Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q1)

  > 0 unaccounted + every refusal dispositioned (Recommended)

  Question: "What is the completion SLO for the single neo→sting migration run?". The option text includes "Plus a #40 daily-work verdict on sting." The question's "single ... run" is the agent's reading of Q24 (L10).

- **SEAT-13** · operator · 2026-08-24T05:43:44Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > I am keen to get the bulkload work complete' where aer we with the workflows, branches, AX, UX for the move in convert with eGreg, tmux, cmux etc; lease read the recent plan files.

- **SEAT-14** · operator · 2026-08-24T05:45:13Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > the migration vi abulkload and eGreg / getting fully situalted is the objective, as this'll fundamnetally resolve the development work traincreashed on what should be the teletype device (macbook neo)

- **SEAT-15** · operator · 2026-08-25T20:28:59Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > you are the only agent left; pleaes see this migration to completion using our bulkload and egreg / post migration / full inteded flow.  this is important

  Typed 90 s after the CONF-06 quiet-window selection, when the other agents had been stood down. A completion mandate, not a wish to halt agents (LIVE-06 and LIVE-07 the next morning).

- **SEAT-16** · operator · 2026-08-28T21:38:34Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > please drive drive drive- lets get this done, full speed ahead.  remember, a week ago you aid this woudl be 45 minutes, its been an entuire week fo work with migration not done.

- **SEAT-17** · operator · 2026-08-28T23:53:21Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > [...] Iall I want is this migration to be complete and my machines to be not fucking destroyed

  Typed during the 08-28 cluster crash, while a bulkload preseed was running. Ties SEAT to FOOT.

- **SEAT-18** · operator · 2026-08-29T03:09:25Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message; D:7957

  > a simple migration was never completed.

  Minutes before the emergency-reboot decision and the SSD wedge (FOOT-02).

- **SEAT-19** · operator · 2026-09-15T02:49:37Z · sting · Claude session `9221daf2`, queued message

  > remember, I am working mostly on neo, not this machine (sting)

  Status, not a requirement: three days after SEAT-05, the operator still worked mostly on neo.

- **SEAT-20** · operator · 2026-09-18T23:46:11Z (resent 2026-09-19T00:00:04Z, 00:55:11Z, 00:58:11Z) · neo · Junie session `session-260918-193940-1a4i` (`~/.junie/history.jsonl`, neo only)

  > please examine and review the linear, worktree, brnaches and docs aroud the sting migration plan and the role of the bulkoad repo, SLOs for beating raw rclone and what needs to be completed; present a plan to complete the migration of all git, agetn dots, juni, repos, worktrees etc etc frmo neo to sting as planned; do not half as this, the inly completion that is OK is completing bulkload paradigm.

  Founding prompt of a Junie session in tummycrypt. "juni" adds Junie agent state to what is carried.

- **SEAT-21** · operator · 2026-09-18T23:57:46Z · neo · Junie session `session-260918-193940-1a4i`, interview free text

  > everything migrated today.

  Answer to a question offering two transfer topologies ("Staged Corpus Pipeline", "Direct Streaming Pull"). The operator answered with scope and timing instead.

- **SEAT-22** · operator · 2026-09-19T03:46:08Z · neo · Junie session `session-260918-230251-1tvr`

  > proceed to next tranch, goal is not achived.  use and sset a /goal for the next 30 minutes.   use specific subagents as I requiested in the plan prompt.

  Reply to the Junie agent, which had blocked the estate migration at the R23 gate (native 32.4 s against rclone 4.1 s). The operator did not accept the gate as a place to stop.

- **SEAT-23** · operator · 2026-09-30T23:57:59Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > excellent!  one more sprint (this time lets set a little /goal) -- please move all things relating to [repo] to sting, with the same rigor and breadth.

  The third 09-30 family sprint. A Codex agent ran the sprints with bespoke scripts, not bulkload. They covered seven families, not all of neo's `~/git`.

- **SEAT-24** · operator · 2026-10-01T22:15:21Z · neo · Claude session `0cb4a127` (lab, neo), interview free text

  > in progress; currently blocked by bulkload completion.   some repos are fully migrated to sting as dev box

  The operator declined to close TIN-2963 (sting as the daily dev box), against the agent's "Done; #1529 gets its own issue (Recommended)".

- **SEAT-25** · operator · 2026-10-02T16:21:55Z · sting · Claude session `2cc731d7`, queued message

  > where are we at with full bulkload migration readiness for moving all work to sting?

  The day after the 10-01 repair declared 45 checkouts in 7 families complete. The family sprints did not count as the full migration.

- **SEAT-26** · operator · 2026-10-05T15:43:27Z · neo · Claude session `0cb4a127` (lab, neo)

  > ther ashound not be any artifical bockers to developemnt in sting, not this adh hoc contention; only some of the repos frm neo are actually moed for devemlpoent to sting, and sting should never have bene encountiering thie dveelopmentspace and home disc contention.

- **SEAT-27** · operator · 2026-10-07T03:31:33Z · neo · Claude session `9384e272` (neo)

  > what on earth is still sharing fast-local?  our critiacl dev machine area is still getting hammered disk wise, making it wholly impossible for us to even consider using our actaul dev box fro develpoment.

  sting's `/srv/fast-local` (the DEST development area) was shared with CI runner volumes and other tenants.

- **SEAT-28** · operator · 2026-10-08T05:06:14Z · sting · Claude session `b526ffc3`, queued message

  > most of sting repos are out of date, and not SSOT

  Status from an unrelated lane: R0 was not met on 10-08.

- **SEAT-29** · operator · 2026-10-10T09:14:25Z · neo · Claude session `9384e272` (neo only), queued message

  > the dev disc is way larger than that of neo, yet doesn;t even have all our ~/git/ content migrated

- **SEAT-30** · operator · 2026-10-08T20:51:50Z · sting · Claude session `2cc731d7`

  > shall we spool up on next steps?  keen to complete our proofs and begin neo -->sting eventualy

  One minute before SEAT-31.

- **SEAT-31** · ruling-record · 2026-10-08T20:53:19Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q142); Linear TIN-4543 7734678e

  > After convergence + S4 (Recommended)

  Question: when does the full neo→sting run happen, given OI-1002-Q24 and S1 still failing? Option text: "Run it once estate reruns converge on their own and refusals are fully typed, even with S1 still failing, as an S3/S4/S5 proof run. This relaxes Q24 for S1 only." Rejected: "Only after S1 passes", "Repo by repo now".

- **SEAT-32** · ruling-record · 2026-09-12T22:27:21Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), interview

  > Manifest → carry → tool (Recommended)

  Option text: "Build the total classified manifest first, use it to drive a full-fidelity carry to sting now (hours, no exclusions) [...] sting becomes genuinely complete today." Rejected: "Manifest → tool → carry" ("sting stays incomplete for weeks"). Same answer: "Rust in tummycrypt — finish M2 (Recommended)", later superseded by PUR-19, and the free text of KEEP-07.

- **SEAT-33** · ruling-record · 2026-10-15T20:23:20Z · neo · Claude session `336ff91f` (lab, neo), interview

  > Exempt it; create it now (Recommended)

  Question: create a new XFS quota project on sting's `/srv/fast-local` for the bulkload estate landing (OI-1003-Q170), so a ~300 GiB neo→sting estate copy can start despite a lab hold on knob changes. Rejected: "Hold until the register" ("Bulkload stays paused") and "Create it, but run the copy later". Evidence against reading R-N56 or Q24 as a standing freeze (conflict C6).

## KEEP: Nothing is dropped

- **KEEP-01** · operator · 2026-07-18T13:45:13Z · neo · Codex session `019f71bd` (`history.jsonl`); D:156

  > now why are you faffing around in TCFS code?  is sting still cordoned, and is the agent history, dots, ~/git/ content, pas claud eand codex history etc proprely copied over to the sting env?  that is ALL WWE WANTED

  Codex prompt.

- **KEEP-02** · operator · 2026-07-28T21:37:11Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > we *can* copy auth and squlite, there was never a directive not to (aside from possible issues with corruption either)

- **KEEP-03** · operator · 2026-08-10T23:02:36Z · neo · Claude session `cc52b480`

  > this is bs, all the dots need to be offloaded,

  *Annotation (2026-10-16, round 2):* The agent had just reported neo's SSD
  state: `/nix` on an external SSD, `~/.codex` on another external volume
  (TIN-3046). KEEP-03 is about moving neo's dot dirs off neo's internal disk
  onto neo's external volumes, not about carrying them to sting. At 23:04Z
  (queued) the operator added that "$HOME is not a dot dir and should not be
  migrated", which is about relocating `$HOME` on neo. Do not cite KEEP-03
  for R1 or for the sting carry.

- **KEEP-04** · operator · 2026-08-25T02:30:05Z · neo · Claude session `79b8ac1b`; D:6545

  > noting we *do* want all the codex and claude elements moigrated though, right?

- **KEEP-05** · operator · 2026-08-28T20:35:56Z · neo · Claude session `79b8ac1b`; D:7618

  > it appears you've forgotten all the specifics of bulkload????  yes, and all the specifics (dots, agents, transcripts, context, tooling, dots,

  Interview answer.

- **KEEP-06** · operator · 2026-08-29T00:24:44Z · neo · Claude session `79b8ac1b`; D:7950

  > do not deal in these, remove anything pushing you to rotate.

  Interview answer on payload secrets (R22).

- **KEEP-07** · operator · 2026-09-12T22:27:21Z · neo · Claude session `a2a9c640`

  > the assupmtion of rekeying is made up, this is not a requiremtn nor expectation and significaltly complicates an alaready complex migration

  Interview answer to 'No exclusions: how literally'.

- **KEEP-08** · operator · 2026-09-12T22:41:05Z · neo · Claude session `a2a9c640`

  > destrying data tat exisits nowhere else is not acceptable

- **KEEP-09** · operator · 2026-09-12T23:11:01Z · neo · Claude session `a2a9c640`

  > that doesn't seem correect, avoiding exlusion is rhe whole poit of bulkload, no?

- **KEEP-10** · operator · 2026-10-08T02:39:07Z · sting · Claude session `2cc731d7`

  > of course including all treesm dots, uncommited and ignored, al transcirpts, and env, all squlite, all . plan files etc etc etc;

- **KEEP-11** · operator · 2026-10-08T02:44:29Z · sting · Claude session `2cc731d7`

  > noting these are all the expected bulkload items, as expected

  Interview answer after selecting transcripts, plans + .env + sqlite, dots/home config, full ~/.claude + ~/.codex.

- **KEEP-12** · operator · 2026-10-16T10:45:46Z · sting · Claude session `2cc731d7`; Linear TIN-4543 comment a92634c1

  > it is a VERY WELL ESTABLISHED REQUIREMNT that NOTHING is EVER excluded.  period.  unsure why you are even asking

  Interview answer D13 (Q185).

- **KEEP-13** · operator · 2026-10-16T10:45:46Z · sting · Claude session `2cc731d7`; Linear TIN-4543 comment a92634c1

  > full history.  drop nothign.  ever.

  Interview answer D14 (Q185).

- **KEEP-14** · operator · 2026-10-16T14:37:07Z · sting · Claude session `2cc731d7`

  > truely recreateable conteent can be dropped.  dots, histroy, trasciprts, etc shouldve never be droped. I am certain weve been over this dozens of times over the last many montsh

  Interview answer (Q192).

- **KEEP-15** · ruling-record · 2026-10-16T14:37:25Z · — · Linear TIN-4543 comment 68248cf1

  > **May be dropped:** only *truly recreatable* content:

  OI-1003-Q192 record, Linear TIN-4543 comment 68248cf1. Its list: build outputs; dependency directories; caches (`~/.cache`, Application Support caches and binaries, `vm_bundles`). Never dropped: dots, history, transcripts, plans, notes, rescue and decommission directories, loose home files, agent state.

  *Annotation (2026-10-16, round 2 verification):* The list is the coordinator's
  wording in Linear. The operator's KEEP-14 answer ("truely recreateable
  conteent") sat under a question that listed rebuildable directories and
  home caches; "Application Support caches and binaries" and `vm_bundles`
  come from the coordinator.

- **KEEP-16** · operator · 2026-09-30T18:30:48Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > those are definately not neded if relating to darwin ~/Library via builtin apps

  Operator-named drop.

  *Annotation (2026-10-16, round 2):* The full message is: "gotcha, those
  are definately not neded if relating to darwin ~/Library via builtin apps;
  so loong as all files, dots, uncommiteds, branches, git work and codex
  resume abilty is ready, that is completion metric for goal" (SEAT-06 is its
  tail). It answered the operator's own question "which referenced databases
  are locked?" (18:28:47Z). The drop is conditional ("if relating to") and
  covers built-in-app databases only. The databases actually locked were
  Chrome's (including Login Data) and the Linear app's (KEEP-29), which are
  third-party. The 09-30 receipt `final-scope.json` widened the drop to
  "Darwin ~/Library built-in app databases and broader application-state
  references" and called that the user's definition. That widening is agent
  text (L6). See conflict C12.

- **KEEP-17** · operator · 2026-10-15T06:44:59Z · sting · Claude session `2cc731d7`

  > single copy is fine, delete

  Interview answer: a verified duplicate on DEST.

  *Annotation (2026-10-16, round 2 verification):* The context above is too broad.
  In the source the operator agreed to delete the 546 GB `.dedupe-aside` on
  sting, a duplicate of model files held on another lab host, choosing this
  over the recommended option, to wait for the backup seed. It is not a rule about
  leaving trees already on DEST out of a run, and SPEC no longer cites it for
  that.

- **KEEP-18** · operator · 2026-08-26T04:38:17Z · neo · Claude session `79b8ac1b`; D:6987

  > this brings up that some other lanes my be in tinylandSSD, that will not be xferrable; these may need to be manually copied out w/ symlinks on sting

- **KEEP-19** · ruling-record · 2026-08-23T19:56:42Z · neo · Claude session `79b8ac1b`; D:4830

  > Carry everything

  Interview selection on what Codex agent history carries to sting; the agent's recommended 'Carry history, exclude bulk logs' was rejected.

- **KEEP-20** · operator · 2026-08-25T19:31:07Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > with all the agent dots and related dots etc?

  Follows ONCE-09. The agent answered (agent text, 19:33Z): "What does **not** ride: bare-home dotfiles outside those — `~/.ssh`, `~/.gnupg`, `~/.config`, etc. — deliberately, because on sting those are Home-Manager-managed". No operator ruling made that exclusion (L6).

- **KEEP-21** · operator · 2026-09-12T22:21:56Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > I will say, I am wary of the engine-free route.  A parallel, adverserial dialog and codex squlite mininig / recent discussions about what exactly we need copied, all the claude, pi, codex transcipts, logs, tooling, secrets, dots, etc are critical, which is what makes the migration complex

  Reply to an agent plan for an rsync "boring path". Names secrets among what must be carried.

- **KEEP-22** · operator · 2026-09-12T22:23:26Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > ah, yes; those exclusiong are unnacceptable, and undermine the whole pooint of the migration.

  Refers to the boring-path exclusion list (agent text, 22:22Z): `auth.json`, all Codex `*.sqlite`, `skills`/`agents`/`commands`, `settings.json`, role files. Precedes KEEP-09.

- **KEEP-23** · operator · 2026-09-20T23:15:08Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > our goal tonight is to clean up and complet bulkload, at teh SLOs and preformance as perscribbed and complete a migration.  this includeas bulkload handling and undersatnding of sops, age, kubeconfigs, kest and safe actual development machine handoff, which clearly was never done.

  Names credential classes (sops, age, kubeconfigs, keys) as bulkload's to carry and understand, as part of a safe hand-off of the development machine.

- **KEEP-24** · operator · 2026-10-08T02:48:41Z · sting · Claude session `2cc731d7`, queued message

  > please also ensure all [repo] related content / [repo] repo, worktress etc etc etc as well as platform / payment portanl ent work is moved to sting

  Extends KEEP-10 to the [repo] repository and the platform/payment portal work (recorded later as Q145).

- **KEEP-25** · ruling-record · 2026-10-08T04:19:19Z · sting · Claude session `2cc731d7`, interview (Q123)

  > Include ssh keys

  Option text: "carry ~/.ssh in full, private keys included (your hosts, your keys)". Rejected: "Stage dir, no keys (Recommended)" ("never private keys").

- **KEEP-26** · operator · 2026-09-30T16:14:52Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > hi there!  I want to copy over ALL the transcripts, git worktrees, git status, dot files, ignored files, claude and codex plan fiels, sops and env files, ajacent worktrees, refenced content etc relating to bulkload and tummycrypt repos on neo to sting (this machine) particualrly the codex sessions.

  The first sentence of the message whose second sentence is RES-08: the scope of the first 09-30 family sprint. A Codex agent ran it with bespoke scripts, not bulkload.

- **KEEP-27** · operator · 2026-09-30T21:58:11Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > hey mate!  using the same methodology we used for the previous sprint (bulkload and tummycrypt + all claude plans, claude transcipts, codex dbs, ajacent / mentied / relavent dirs and trees, all worktreres, dots, branches, all fiesl including ignored and hiddne etc) please target [repo] and all [repo] / [repo] / [repo] realted repos form neo to sting

  The scope part of the message whose tail is RES-09.

- **KEEP-28** · ruling-record · 2026-09-30T17:35:40Z · sting · Codex session `01a0f317` (history and rollout); receipt `/srv/data/jess/neo-transfer-20260930T164001Z/receipts/reference-scope.json`

  > Finish local-disk archive and record unresolved remote references (Recommended)

  Question: how to handle files referenced on neo's GDrive/NFS mount, where filesystem checks hung. Rejected: "Include mounted remote data too, even if it delays completion". A time-boxed scope for the 09-30 sprint, carried into the 10-01 repair's "agreed limits". It does not exclude local content and was not restated after KEEP-12 and KEEP-13 (conflict C13).

- **KEEP-29** · ruling-record · 2026-09-30T18:28:33Z · sting · Codex session `01a0f317`; receipt `locked-app-database-scope.json`

  > Record locked app databases as unresolved and finish the accessible local archive (Recommended)

  The locked databases were Google Chrome's (Login Data, History, Web Data and others) and the Linear app's: third-party apps, not built-in ones. Rejected: "Keep retrying until every referenced app database can be captured". Fourteen seconds later the operator asked "which referenced databases are locked?", then typed KEEP-16.

- **KEEP-30** · ruling-record · 2026-09-30T22:24:39Z · sting · Codex session `01a0f317`, interview

  > Exclude this cache (Recommended)

  A root-owned, unreadable, generated Python `__pycache__` in a [repo] checkout. Rejected: "Require every byte". Consistent with KEEP-14 (truly recreatable).

- **KEEP-31** · ruling-record · 2026-10-04T04:34:21Z · neo · Claude session `9384e272` (neo), interview; Linear TIN-5268 605a2d1b

  > Hold cleanup; preserve first (Recommended)

  Question: protect the ~375G of Codex sessions on sting from TIN-5268 cleanup? The option: no deletion, ageing or lossy compression until a scrubbed durable copy exists. Rejected: "Compress OK, no deletion", "No hold". Framed as the RL corpus, it protects the same agent-session stores bulkload carries.

- **KEEP-32** · ruling-record · 2026-10-04T08:48:57Z · neo · Claude session `9384e272` (neo), interview

  > Extend the hold to all seats (Recommended)

  Extends the TIN-5268 hold to every seat (yoga held the only Feb–Apr 2026 transcripts). Lossless zstd only (Linear 81f8e3f8).

- **KEEP-33** · ruling-record · 2026-10-08T12:07:40Z · neo · Claude session `9384e272` (neo), interview

  > A + no ageing of session data

  Option text: "Session data is never aged; relief comes only from non-session caches (nix GC, bazel bases, build outputs) until the hold lifts or a scrubbed durable copy exists." Applies to DEST storage.

- **KEEP-34** · ruling-record · 2026-10-08T12:32:18Z · neo · Claude session `0cb4a127` (lab, neo), interview; same lane 2026-10-07T17:59:01Z "Weekly timer (Recommended)"

  > Moving is fine; keep the timer (Recommended)

  Question: does lab's weekly cold-tier timer count as ageing? The timer moves Codex/Claude session stores older than 7 days to `/srv/data` behind symlinks, losslessly. On DEST, session stores may therefore sit behind symlinks on another volume (conflict C9).

- **KEEP-35** · ruling-record · 2026-10-02T04:12:44Z · neo · Claude session `0cb4a127` (lab, neo), multi-select

  > Delete tummycrypt-litter-20260921 (124G), zstd Codex rollouts older than 14 days (~80-90G), Delete cargo-target-bulkload-* (~18G), Move recove[...]

  All four options were selected for neo's external volume holding the live `CODEX_HOME` (98 % full). The fourth label, cut off in the record, moves the recovery trees to sting (~119G, "Nothing deleted"). Bulkload's own bench litter and build outputs may be dropped. Codex rollouts older than 14 days now exist on neo as `.jsonl.zst`, with originals quarantined; sting had the same pass (R-C120a). This bears on byte identity (conflict C9).

- **KEEP-36** · operator · 2026-10-08T04:50:33Z · sting · Claude session `2cc731d7`

  > with the usual vigor and completeness as we've dialoged and discuessed with bulkload, can you efficienrtly copy over all [repo], [repo], [repo] and related work from sting and neo to yoga?  (oncluding all the agent / dots / trees / transcripts / sulites squlites etc etc etc etc etc) as fast as possible?

  The two-source run to yoga that conflict 6 mentions. bulkload's scope, applied to a destination other than sting.

- **KEEP-37** · ruling-record · 2026-10-09T02:46:48Z · sting · Claude session `2cc731d7`, interview

  > Fresh root, exclude live repos (Recommended)

  How the Q142 full run starts: it lands at a new root with ledgers, and the families already moved to sting and worked there are left out. Rejected: "Build --adopt", "Fresh root, include everything". A scope choice for families already carried, not content filtering.

- **KEEP-38** · operator · 2026-08-23T19:25:17Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message; also Codex session `019f65d0` 19:26:20Z

  > the agent helping the transition to remote dev on sting needen't be tranfferable, but the righht interlinked between egreg and bulkload and other repos, the dgreg docs and edstablished patternsm, sota cmux / tmux / remote agent etc screen patterns etc are critical

  From the same prompt as RES-05, LIVE-03, SEAT-03 and CONF-01. The operator's only exception about an agent: the session driving the transition need not move to sting itself. Read it narrowly; it is not a content exclusion. Pair it with RES-16 (conflict C19).

## LIVE: Live hosts: never halt SOURCE, never clobber DEST

- **LIVE-01** · operator · 2026-07-17T21:01:28Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > please do NOT Execute local cmux, we are in cmux.  this will crash our session

- **LIVE-02** · operator · 2026-08-23T22:14:05Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > never kill parallel codex or claude sessons.

- **LIVE-03** · operator · 2026-08-23T19:24:51Z · neo · Claude session `79b8ac1b`; D:4810; Codex session `019f65d0` (`history.jsonl`)

  > please reassrt the truely achivable without closing out lcoal processes;

  Same prompt as LIVE-C1 (see conflicts).

  *Annotation (2026-10-16, round 2):* "LIVE-C1" above means CONF-01.

- **LIVE-04** · operator · 2026-08-25T02:24:22Z · neo · Claude session `79b8ac1b`; D:6459

  > there are 3 parallel claude agents and one codex agent octive on neo; I do not want to interupt them, and think it'll be plausable to run another bulkload reconsiliation post move to sting, correct?

- **LIVE-05** · operator · 2026-08-25T11:02:23Z · neo · Claude session `79b8ac1b`; D:6592

  > we lost a huge chunk of time due to false assumptions.

  Same message: the agent had 'insited codex agents stand down'.

- **LIVE-06** · operator · 2026-08-26T13:07:05Z · neo · Claude session `79b8ac1b`; D:7015

  > instead of allowing live captures, we've list almost a week of work becase we STILL are not actually able to work in git or with our agent systems.  the entire premise is moot.

- **LIVE-07** · operator · 2026-08-26T13:09:32Z · neo · Claude session `79b8ac1b`; D:7019

  > ~/git should never have been hostage by this software.  ther should never be an appointmetn needed.  this is the whole premise of why we didn't jsut use rclone directly, and the only point of a maniffest (which is easily a idempotent operations, not a halt and repeat operation)

- **LIVE-08** · operator · 2026-08-26T13:16:42Z · neo · Claude session `79b8ac1b`; D:7027

  > this cannot lockup ~/git, this cannot lockup agent dbs or write paths, that is why we are trying to write this ourslf.

- **LIVE-09** · operator · 2026-08-26T13:23:41Z · neo · Claude session `79b8ac1b`; D:7031

  > ensuring my work area is UNINHIBITED by the migration, with the last dregs and next steps cleanly finishable asynchronously (as projects exit, they can complete synscs / as agents exit organically, work can idempotently be toppted up, git wts retopped up etc)

- **LIVE-10** · operator · 2026-08-28T15:36:45Z · neo · Claude session `79b8ac1b`; D:7255

  > the SOTA bits are being able to NOT have to quite agents or any active work (that is the crux bulkload seeks to solf)

  Same message as LIVE-11.

- **LIVE-11** · operator · 2026-08-28T15:36:45Z · neo · Claude session `79b8ac1b`; D:7255

  > bulkload should be capable of efficently wiht greenfield / SOTA preformance sync out git and agent dots in a one of way to sting, so we can pickup remote work using neo as the teleltype as we intededed,

- **LIVE-12** · operator · 2026-08-29T01:41:55Z · neo · Claude session `79b8ac1b`; D:7733

  > noting the lab and blahaj cluster claude lanes locally are not and will not be stopped this evening, which as we've discussed is a core requirement of bulkload being able to handle (the jsonl, claude dialog and history etc mustn't be frozen during migration, and should be idempotently / memoize ontop clenaly qhen convinent

- **LIVE-13** · operator · 2026-09-02T12:09:18Z · neo · Claude session `79b8ac1b`; D:8141

  > Note there is a local GoriousFlywheel codex session working, which, as is the core requimrement of bulkload, shall NOT be interupted.

- **LIVE-14** · operator · 2026-09-21T03:23:59Z · neo · Claude session `a2a9c640`

  > I am still comepletely baffled why you killed my tmux session, what did you think that was going to achive

  The killed tmux server was on sting (DEST).

- **LIVE-15** · operator · 2026-09-21T11:33:06Z · neo · Claude session `a2a9c640`

  > there is various work on sting that is far adavcenced in git beyond neo; no work should ever be clobbered on sting from neo

- **LIVE-16** · operator · 2026-09-22T20:51:51Z · neo · Claude session `a2a9c640`

  > the core requirement is that agent and devlopment work MUST NOT BE HALTED for bulkload to fnuction; #1 state the oppposite.

  Interview answer to a capture design that refused on mid-pass ref changes.

- **LIVE-17** · operator · 2026-10-08T02:48:27Z · sting · Claude session `2cc731d7`

  > bulkload should, by design, already handle this.  lets reality check

  Interview answer about live SQLite written while neo agents run.

- **LIVE-18** · operator · 2026-09-30T17:36:33Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > to be clear, we aren't overwriting other repos alrreadt on sting, right?

  Typed on sting.

- **LIVE-19** · operator · 2026-10-02T23:42:46Z · neo · Claude session `0cb4a127` (neo)

  > can we progress without cutting tmux, codex and claude work for now

  *Annotation (2026-10-16, round 2):* The full message is: "hmm, Iam not
  able to close all this work on sting yet.  can we progress without cutting
  tmux, codex and claude work for now, with planned downtime as needed
  tomorrow morning?" (2026-10-02T23:42:46.749Z). The lab agent had proposed a
  quota remount window on sting's `/srv/fast-local` that would end every sting
  dev-seat session (TIN-5304); the decision was recorded as R-C179. The live
  work was on sting (DEST), and the subject was host maintenance, not a
  bulkload run. The operator offered planned downtime. Cite LIVE-19 for
  "never interrupt live work on DEST". It is not a SOURCE statement and not a
  ban on operator-scheduled maintenance (LIVE-38).

- **LIVE-20** · operator · 2026-10-15T07:38Z · neo · Claude session `336ff91f` (neo)

  > actually, no; we have live work on sting. work around this.

  *Annotation (2026-10-16, round 2):* Exact time 2026-10-15T07:38:40Z,
  lab project. The operator had chosen "Restart now" for sting's nix-daemon to
  apply a `/nix` GC floor, then interrupted and reversed it; the agent
  retracted R-C713. Host maintenance on DEST, not a bulkload operation.

- **LIVE-21** · ruling-record · 2026-10-03 · — · `docs/slo.md`, Core SLOs (OI-1003-Q2)

  > S5 Source stays live

  docs/slo.md (OI-1003-Q2): 'Agents keep working on neo during the run. [...] There is no freeze window.'

  *Annotation (2026-10-16, round 2):* The operator selection behind it is
  LIVE-23; Q152 reaffirmed it (LIVE-26).

- **LIVE-22** · operator · 2026-09-21T03:20:42Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > this work NEVER should kill or destroy agent / human work, that is a golden requiremnt and a core reason we are building builkoad.

  Typed right after an agent killed the operator's tmux server on sting (LIVE-14; 03:20:18Z "you killed my tmux session").

- **LIVE-23** · ruling-record · 2026-10-03T03:58:43Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q2)

  > Live, with drift custody (Recommended)

  Question: "How much of neo can stay live during the migration run?". Option text: "Agents keep working on neo. [...] No freeze window." Rejected: "Short freeze for the final pass", "Full freeze".

- **LIVE-24** · ruling-record · 2026-10-03T04:01:45Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q5)

  > No locks, no signals, bounded impact (Recommended)

  How never-interrupt becomes an SLO (S2).

- **LIVE-25** · ruling-record · 2026-10-03T04:03:01Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q9)

  > p95 +25%, load +2.0

  The impact budget. The operator chose this looser option over the recommended one.

- **LIVE-26** · ruling-record · 2026-10-15T04:53:03Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q152)

  > No freeze, deltas only

  Question: allow one bounded pause of neo's agent sessions for the final delta? The operator rejected "One bounded pause (Recommended)" (operator-scheduled, under 30 minutes). Option text: "Keep S5 strict: repeated converging reruns while neo stays live."

- **LIVE-27** · ruling-record · 2026-10-03 (04:01:45Z–04:03:01Z) · sting · Claude session `2cc731d7`, interview

  > Tolerate as drift + preflight (Recommended)

  Issues #38 and #39: HEAD or the index moving during a capture is tolerated as drift (compare LIVE-16).

- **LIVE-28** · ruling-record · 2026-10-15T15:15:43Z · sting · Claude session `2cc731d7`, interview

  > Carry it as drift (Recommended)

  Question: what happens if an agent runs `git stash` on neo while a capture pass reads that repo? Rejected: "Keep refusing". Same answer: "Relative to live baseline (Recommended)", so S2 evidence counts while neo is busy.

- **LIVE-29** · ruling-record · 2026-10-15T16:20:17Z · sting · Claude session `2cc731d7`, interview

  > A: keep excluded (Recommended)

  The reflink clone lever (about 18 ms against rclone's 340 ms on the rig) stays excluded, because cloning touches the source inode. Rejected: "B: opt-in local fast path", "C: amend S2 to allow it". Source safety outranked a speed win.

- **LIVE-30** · ruling-record · 2026-10-03T10:05:23Z · sting · Claude session `2cc731d7`, interview (WP0(d)+(c))

  > Supersede only own outputs, by identity (Recommended)

  Option text: "A rerun may replace a destination file only when its (dev, ino, stat) equals this store's own ledger row, i.e. bulkload wrote it and nobody touched it since." Rejected: "Keep no-clobber". Extended on 2026-10-09 to checkouts ("Approve, model first (Recommended)", 02:46:48Z) and to SQLite ("Supersede when owned (Recommended)", 04:27:47Z).

- **LIVE-31** · ruling-record · 2026-09-22T22:57:27Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), interview

  > Proceed, record it in the receipt (Recommended)

  For when apply finds that DEST already holds a seat the drift list says was never captured. Option text: "Destination bytes are left as they are; the receipt names each such seat. Never clobbers sting-side work." Rejected: "Refuse that item".

- **LIVE-32** · ruling-record · 2026-10-08T19:36:51Z · sting · Claude session `2cc731d7`, interview

  > Rename aside + import (Recommended)

  sting's 09-30 copies of two [repo] repositories held never-pushed branches and dirty files. Option text: "Each old copy is renamed to <name>.sting-0930 and kept untouched. Its unpushed branches are also added to the new repo as sting-0930/<branch>, create-only, so nothing is stranded."

- **LIVE-33** · ruling-record · 2026-10-09T03:26:40Z · neo · Claude session `76f05dcb` (neo), interview

  > Diff, preserve, then fast-forward (Recommended)

  A stale [repo] clone on sting had uncommitted edits. Option text: "Diff sting's dirty tree against wip/boot-mount (read-only). If they're identical, discard the duplicate [...] If they differ, commit sting's edits to a wip/boot-mount-sting branch first." Rejected: "Leave sting alone", "Sting is the real dev seat". A per-repo precedent: DEST's uncommitted work is preserved before the tree moves forward.

- **LIVE-34** · operator · 2026-10-01T18:12:46Z · sting · Codex session `01a0f317` (`history.jsonl`)

  > ah, I think we may have caused the recent network revocation fatal error o nthe three extant codex sessions runnin gon sting; all were resumable wiht no damage, though I wonder if our work cased the simultaneous halt.

  During the 10-01 repair, the Codex fork batch exhausted the shared app-server's file-descriptor limit and halted the operator's three live Codex sessions on DEST (18:05:27Z). At 18:14:48Z the operator added: "no worries, the log is more dramatic than the halt, no harm was done to the sessions or their ajacent files / edits /work streams." That is tolerance after the fact, not a relaxation of LIVE-02 or LIVE-16. The move halted live work on DEST.

- **LIVE-35** · operator · 2026-10-07T12:09:33Z · neo · Claude session `0cb4a127` (lab, neo), interview free text

  > communucate a time today for all claude agents on neo to pause / save work  for just a few minutes ; sometime ti safternoon; nonblocking, all subagents and claude agents shoulbe able to conform and adapt wihtout truncation / halting there work early

  About quitting cmux on neo to upgrade it. Not bulkload. It shows how strictly never-halt is read, even for a short pause the operator chose.

- **LIVE-36** · operator · 2026-10-08T00:38:20Z · neo · Claude session `0cb4a127` (lab, neo), queued message

  > oi, why did you call my othe ragents to halt????

  The lab agent had asked another seat to move and shut down its builds, and set a timer to stop them. The agent withdrew R-C425. Same principle as LIVE-02.

- **LIVE-37** · operator · 2026-10-15T18:25:07Z · neo · Claude session `336ff91f` (lab, neo)

  > I was deep in work on sting

  Preceded by "what the hell happened to my dev box?????" (18:24:33Z). sting stopped responding under agent load from lab evaluations and other seats, not bulkload. DEST is a live working seat.

- **LIVE-38** · operator · 2026-10-03T18:51:00Z · neo · Claude session `0cb4a127` (lab, neo), interview free text

  > I am alerting all agetns, we'll have scheduled neo maintnence at 4pm, and scheduled sting maintnence at 3:15; lets get our ducks in a row for 3:15 sting work (followd by other nods, as deemed

  Operator-scheduled host maintenance (patches, a quota remount, reboots). It does not license bulkload quiet windows (LIVE-07, LIVE-10, LIVE-16).

- **LIVE-39** · operator · 2026-07-27T01:50:01Z · neo · Codex session `019e93c0` (`history.jsonl`)

  > this was with all codexes, including this one, exited.  are you sure this is a sensible way to do this?  I don't think it is.  please resasssert the todos and goals here please.

  Not bulkload: lab TIN-3046's live migration of neo's local Codex state required every Codex session to exit. The earliest objection found to a migration that needs agents to quit. Background only.

- **LIVE-40** · operator · 2026-10-03T01:32:46Z · sting · Claude session `2cc731d7`, interview free text

  > run now

  Gate (a) question: neo was on AC, but four Claude and three Codex sessions were live and load1 was 20. Rejected: "Auto-start when quiet (Recommended)", "I'll quiet it now; re-check again", "Hold gate (a)". 32 minutes after CONF-08.

- **LIVE-41** · operator · 2026-10-03T18:52:53Z · sting · Claude session `2cc731d7`, interview free text (OI-1003-Q22)

  > please run regardless on neo at 3:30pm EST

  Rejected "You schedule a quiet window (Recommended)" ("Tell me when you've paused neo's sessions"), "Keep auto-watching", "Defer gate (a)". The same pattern followed on 2026-10-04T00:53:43Z ("on AC power now, for the night", rejecting "I'll arrange a window (Recommended)") and on 2026-10-07T01:19:13Z ("schedule for  3am tonight", rejecting "You schedule a quiet window (Recommended)").

- **LIVE-42** · operator · 2026-10-04T18:39:11Z · sting · Claude session `2cc731d7`, interview free text

  > run it now, with the pressure.  we should note teh run as under severe pressure and see what happens

  Rejected "You open a window; re-pin to main (Recommended)" ("You pause neo's agent sessions for about 1 hour").

- **LIVE-43** · ruling-record · 2026-10-08T02:44:29Z · sting · Claude session `2cc731d7`, interview

  > Run now, background priority

  Question: when and how should the neo→sting run happen (neo at load 145, disk 91 % full)? Rejected: "Inventory now, run when quiet (Recommended)" (a window with agent sessions paused), "Wait for you". The run requested in KEEP-10.

## ONCE: Never twice: piecemeal, idempotent, no re-walk or re-read

- **ONCE-01** · operator · 2026-08-29T14:06:07Z · neo · Claude session `79b8ac1b`; recorded as R25 in GitHub bulkload#34 (2026-08-29T14:09:14Z)

  > bulkload should beat rclone as a ~/git/ + all agaent data, federation, dots, sqlite / codex / wal / claude transcriopts  / worktrees / TCFS native / estate solution for Agents to efficeintly identify and migrate idempotently work to a remote box in a xextremely fast way; the key feature is ensuring the host need not halt agent work or git work to initatie migration, and can be finished at any time / piecemiel without EVER rewalking or reading a bit twice.

  Interview answer to 'What is bulkload, going forward?'. Recorded as R25 (bulkload#34).

- **ONCE-02** · operator · 2026-08-26T03:49:25Z · neo · Claude session `79b8ac1b`; D:6971

  > why does it take 5 hours per iteration?  shoulden't we be picking up where we left off, as well as muxxing any actually noxel copies or bit diffs?

- **ONCE-03** · operator · 2026-08-26T03:51:22Z · neo · Claude session `79b8ac1b`; D:6975

  > this seems overly compladhocex, overly serial and too slow for parallelism and idempotent patterns I know you understand.

- **ONCE-04** · operator · 2026-08-26T08:52:01Z · neo · Claude session `79b8ac1b`; D:6999

  > this census elements and rewalk seems way too slow for an idemplotent, efficient system, evn for 100s of gb; why so slow?  I expected the migration to be a fairly simple procedure, optimized via bulkload.

- **ONCE-05** · operator · 2026-08-29T03:18:36Z · neo · Claude session `79b8ac1b`; D:7767

  > it should take a few munites to complete the migration. most of the data is already moved.

- **ONCE-06** · operator · 2026-08-29T13:40:25Z · neo · Claude session `79b8ac1b`; D:7890

  > 60 minutes per iteration is insane and unnaceptable

- **ONCE-07** · operator · 2026-10-03T03:58:43Z · sting · Claude session `2cc731d7`

  > abiliy to complete migration edges as local neo worklanes complet by rerunning bulkload sync with not rewalked / reread bits (data idempotency)

  Interview answer: priority order.

  *Annotation (2026-10-16, round 2):* ONCE-07 and FAST-09 are fragments of
  one free-text answer, given in full as SEAT-10 (OI-1003-Q4).

- **ONCE-08** · ruling-record · 2026-10-03 · — · `docs/slo.md`, Completion bar (OI-1003-Q13)

  > It is one more run of

  docs/slo.md Completion bar (OI-1003-Q13): 'The full neo→sting migration is not a major scheduled event. It is one more run of a tool that is already trusted [...] repeated as neo's work lanes finish, and every rerun converges without re-reading anything.'

  *Annotation (2026-10-16, round 2):* The operator's own words behind
  OI-1003-Q13 are SEAT-11 (2026-10-03T04:03:13Z, queued). Cite them beside the
  `slo.md` text.

- **ONCE-09** · operator · 2026-08-25T19:30:45Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > this is bulkload, correct?  isn't most of the tree already coppied?

  Reply to the agent saying the capture must byte-copy `~/git` (105G) across volumes (about 1.5 h). The operator expected copied bytes to be reused. The agent replied that it had emptied sting's destination that morning, so the preseed would stream everything again (~300G).

- **ONCE-10** · operator · 2026-09-12T22:25:46Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > indeed.  gthat is the R25 verbatim — and it vindicates the objection precisely: [...] is precisely that assertion.

  Echoes the agent's 22:25:20Z quotation of R25 (ONCE-01), adding "indeed." and the closing words. The operator endorses R25 as the standing bar, including its scope ("all agaent data [...] sqlite / codex / wal / claude transcriopts"). The elided middle is the agent quoting ONCE-01.

- **ONCE-11** · ruling-record · 2026-10-03T04:01:45Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q6)

  > 0 re-read; metadata-only re-walk (Recommended)

  Question: "'Rerun without re-walk or re-read': how strict is the incremental SLO?" Option text: "A rerun reads 0 content bytes for unchanged seats [...] The rerun cost is a metadata walk (stat only)". Rejected: "No re-walk at all (change journal)" ("Strongest, larger design"), "Both, staged". Three minutes after the operator typed "with not rewalked / reread bits" (SEAT-10). See conflict C5.

- **ONCE-12** · ruling-record · 2026-10-03T04:03:01Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q10)

  > ≤ 10%

  The rerun bound. The operator chose this looser option over the recommended one. It matches SPEC Acceptance 4.

- **ONCE-13** · ruling-record · 2026-09-22T22:57:27Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), interview

  > Yes, that reading (Recommended)

  Question: the drift-tolerant design reads R25 as never re-reading a seat whose stat identity is unchanged, while a seat that changed mid-pass is re-read on the next pass. Is that the correct reading? Rejected: "No, literal: never re-read anything".

- **ONCE-14** · ruling-record · 2026-09-23T12:52:35Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), interview (R-N58)

  > Never re-read a byte the destination already holds durably, and never re-read a seat whose stat identity is unchanged (Recommended)

  Rejected: "Literal: the source never reads any byte twice, even after a crash", which the agent described as "Keeps a durable source-side byte copy and pays its write and read-back on every initial copy."

- **ONCE-15** · ruling-record · 2026-10-04T18:39:11Z · sting · Claude session `2cc731d7`, interview (proposed as Q39, recorded as OI-1003-Q40)

  > Committed-row reading + issue (Recommended)

  Rejected: "Strict reading stays" ("needs a code change [...] Until then, R25 is not met under the model"). The strict reading was built later anyway (#169, #198, accepted 10-07).

- **ONCE-16** · ruling-record · 2026-10-16T10:45:46Z · sting · Claude session `2cc731d7`, interview (D9, OI-1003-Q186)

  > Capture git in batches (Recommended)

  Option text: "Split the 866 git items into batches that fit. After each batch is pulled and verified on sting, drop that batch's corpus on neo (the lane's own output, by literal path with a receipt). Keeps the 25% floor." Rejected: "Lower floor to 15%", "Measure first, then decide". Same answer set as KEEP-12 and KEEP-13.

- **ONCE-17** · ruling-record · 2026-10-16T14:37:07Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q193)

  > Drop after verified apply (Recommended)

  Each git batch's capture private state on neo (about 1.8× its corpus) is removed after the batch is pulled, verified and applied on sting, by literal path with a receipt. Rejected: "Keep it; smaller batches". Same answer set as KEEP-14.

## FAST: Faster than rclone

- **FAST-01** · operator · 2026-08-26T13:04:23Z · neo · Claude session `79b8ac1b`; D:7007

  > waiting half a day to copy files is not very impressive

- **FAST-02** · operator · 2026-08-26T13:05:18Z · neo · Claude session `79b8ac1b`; D:7011

  > why is it worth anything?  why wold we not trust rsync?  if we are just dressing up rsync with a manifest, what exactly are we doing here?

- **FAST-03** · operator · 2026-08-26T13:23:41Z · neo · Claude session `79b8ac1b`; D:7031

  > leveraging more creative ways to properly parallilze (like, real parallelization, sich as with torrent style chunking, or true parlllelizm with protobuf, or proper integration with macos 26 filekit for super fast walks in native APFS swift for example

- **FAST-04** · operator · 2026-08-29T01:38:26Z · neo · Claude session `79b8ac1b`; D:7729

  > remember, this is definately still slower than rclone.  ick.

- **FAST-05** · operator · 2026-08-29T14:17:52Z · neo · Claude session `79b8ac1b`; D:8056

  > python is likely the wrong language for the bulkload job, polars / unsafe rust would probably be much more suitable if we want to have a chance to actually achive ?> rclone nantive parallelsise and single ready accuracy.

- **FAST-06** · ruling-record · 2026-08-29T00:24:44Z · neo · Claude session `79b8ac1b`; D:7675

  > Keep the 'supersede rclone' thesis, earn it

  Interview selection 2026-08-29 (R23); the agent's recommended 'Reframe: correctness layer + pluggable mover' was rejected.

- **FAST-07** · ruling-record · 2026-09-23T12:52:35Z · neo · Claude session `a2a9c640`

  > Two gates: loopback-over-socket must beat rclone local; cross-host pull must beat rclone over ssh

  Interview selection (R-N57).

- **FAST-08** · operator · 2026-09-23T00:55:28Z · neo · Claude session `a2a9c640`

  > I am concenrted there are workstreams addressing non bulkload rust codebase / nor development / reseraching on efficient, SLO achivicing SOTA paradigms, / novel strucutrted / proto / streaming setup that'll get bulkload done.

- **FAST-09** · operator · 2026-10-03T03:58:43Z · sting · Claude session `2cc731d7`

  > the core LSos remaining faster than rclone, extnat machin's agents and work must not be interupted,

  Interview answer: priority order (same answer as ONCE-07).

  *Annotation (2026-10-16, round 2):* A fragment of SEAT-10.

- **FAST-10** · operator · 2026-10-07T05:52:16Z · sting · Claude session `2cc731d7`

  > we could seed with a bounded ~/git-bulkload/ instead of neo <> sting, which are both live and very busy machines; not sure why I hand't considered this for proofing mechanism before.

  Interview answer; origin of the hermetic rig (OI-1003-Q96/Q97).

  *Annotation (2026-10-16, round 2):* The quote drops the answer's opening
  without an elision mark: "hmm, maybe we should be more hermetic; we could
  use PZM <> honey pr PZM <> yoga?  mbp-13 <> yoga?". The question noted
  neo's load average was 285.

- **FAST-11** · operator · 2026-10-15T05:06:15Z · sting · Claude session `2cc731d7`

  > examining SOTA high preformance, network optimizing patterns (ie. thinking about the nature of the transmission substrate, thinking about the placement on block from the read (which is fast) to placement (slow) and transmission topologiy (possibly complex)

  Interview answer: S1 levers.

- **FAST-12** · ruling-record · 2026-10-07 · — · `docs/slo.md`, amendment 2026-10-07 (OI-1003-Q96, Q97)

  > The gate of record (OI-1003-Q97).

  docs/slo.md amendment 2026-10-07: 'S1 gates (a) and (b) are decided on the hermetic rig, on the sealed corpus.'

  *Annotation (2026-10-16, round 2):* The operator selection behind it is
  FAST-21.

- **FAST-13** · operator · 2026-09-19T02:10:18Z (resent 03:04:31Z) · neo · Junie sessions `session-260918-193940-1a4i` and `session-260918-230251-1tvr`

  > please review the layout and pllans, goals and codbeases and ticekets, brnaches, worktrees; lets make this happen properly and get the bulkload repo and product in complete, SLO proven state.

  Pasted agent output follows the quote and is not operator text.

- **FAST-14** · ruling-record · 2026-09-19T03:14:54Z · neo · Junie session `session-260918-230251-1tvr`, interview

  > Corpus Bytes

  Question: what does the R23 "1% delta" mean? Option text: "Deterministically modify files totaling about 1% of corpus bytes, which tests CDC locality and yields a meaningful throughput comparison." Rejected: "Existing File Count", "Both Workloads". No option was marked recommended. See conflict C3.

- **FAST-15** · ruling-record · 2026-09-19T03:14:54Z · neo · Junie session `session-260918-230251-1tvr`, interview

  > Single Group Publisher

  An engine-internals choice for the first-copy publication path, not product intent.

- **FAST-16** · ruling-record · 2026-10-03T03:58:43Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q3)

  > No hard SLA; must beat rclone (Recommended)

  Option text: "The performance bar stays relative (gates a and b beat rclone) [...] resumable piecemeal, so no single window is needed."

- **FAST-17** · ruling-record · 2026-09-23T12:44:28Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), interview

  > Unsafe-first: SOTA performance is the direction

  Chosen over the recommended "Unsafe allowed where a bench proves the win". Preceded at 12:40:45Z by the queued "SOTA unsafe rust  I think is our current direction, right?".

- **FAST-18** · operator · 2026-09-23T12:44:39Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > of course preformace is the bar, the LSOs depend on it

- **FAST-19** · ruling-record · 2026-09-23T13:25:46Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), interview (R-N57)

  > rclone as shipped, cloning allowed; record --local-no-clone as informational (Recommended)

  Which rclone gate (a) has to beat. Option text: "The harder, honest reading of beat rclone." In the same interview the operator chose the USB rig volume over the recommended internal SSD, which the agent called "likely unwinnable for reasons of device physics". Both times the operator chose the harder bar.

- **FAST-20** · ruling-record · 2026-10-15T04:53:03Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q150)

  > Authorize a deeper lever

  Rejected: "Benchmark, not a gate (Recommended)" ("S1 stops gating the migration (amending OI-1002-Q24)") and "Equal-durability gate". The 05:06:15Z follow-up (FAST-11) authorised three levers, "and S1 stays gated against plain rclone".

  *Annotation (2026-10-16, round 2 verification):* "and S1 stays gated against
  plain rclone" is agent question text from the 05:06:15Z follow-up, not the
  operator's words. The operator's own answer there is FAST-11.

- **FAST-21** · ruling-record · 2026-10-07T05:53:50Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q97)

  > Hermetic rig is the gate (Recommended)

  Option text: "S1 gates (a) and (b) are judged on the hermetic rig with the sealed corpus. A neo to sting run becomes a field confirmation, reported but not gating." Rejected: "Both must pass" (a gated neo→sting sample as well), "Hermetic is informational". Same interview: Q96 "mbp-13 + yoga, PZM as Darwin source (Recommended)", and "Go straight to gated" over the recommended informational smoke run.

- **FAST-22** · ruling-record · 2026-10-03T10:09:28Z · sting · Claude session `2cc731d7`, interview (WP0(e), OI-1003-Q19)

  > (e) estate-shaped S1 corpus (Recommended)

  Option text: "A deterministic generator of git-heavy, many-small-file trees, sealed like corpus v1. S1 is also measured on it, because R23's 23 large files don't stress directory or metadata cost." Ratified in addition to corpus v1, not in its place.

- **FAST-23** · ruling-record · 2026-10-08T22:48:16Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q143)

  > Widen the receive side, Directory batching, Measure + low-risk cuts

  Not selected: "Loosen the in-process wire" (authorised later, in Q150 and Q154). The question set a pre-gate: a gated sample happens only once the native median is at most 0.90× rclone's over 15 informational samples. Linear 80433424 dates the answer "about 21:20Z"; the transcript shows 22:48:16Z.

- **FAST-24** · operator · 2026-08-18T00:39:37Z · neo · pi session `01a01127` (copy on sting)

  > 6. still paying, rclone has been used on-prem before with succcess, though walking / listing / syncing / readwriteing can be hard with kust rclone

  Not bulkload (Google Drive mount research). The operator's view that rclone's weak points are walking, listing and syncing.

- **FAST-25** · operator · 2026-10-16T21:07:03Z (Linear post time; typed ~21:05Z) · — · Linear TIN-4543 comment 9539098b (lab-seat relay, "the operator's text verbatim")

  > the hard part in my mind is beatin rclone, where agents have fabricated a false paper, proofs, archietcures that completly miss the point of bulkoad in termns on ntework, idempotancy, streaming, copyonce, NOT interupt work on neo / source machine; these are nuances of the ask, migrate from one machine to another fast. that is a difficult problem in practice, and I think all we've done is dress up a adhoc clone operation instead of actaully thinking about what is being asked and what it MEANS. migirating bit for bit accorss huge nubmers of devlopment files, dots, ingnores, scaches, blobs, engs, dots, exports, handling divergencwe, git, ssh, home exports, local exports, agent temps, agent dialogs, angent transciprts, agent plans, symlinks etc is a (very difficult* task in practice, but CAN be clearly described. beating rclone shoudl be done in the itended architecure, which has nothign to do with cloning.

  Appended 2026-10-17 (Bulkload Reset Review row K06). Governs R-C842 (end-to-end seat time is R3's gate of record) and R-C843 (the paper, formal model and clone-race lever notes are deleted), both TIN-4543 ace8649c.

## FOOT: Footprint and host hygiene

- **FOOT-01** · operator · 2026-08-29T03:18:36Z · neo · Claude session `79b8ac1b`; D:7767

  > ther should not be hundresd of gigabytes of BS bulkload dirs lying around in my ssd.  that is, infact, the oposite of what this teletype should have.

  Same message as ONCE-05.

- **FOOT-02** · operator · 2026-08-29T03:01:20Z · neo · Claude session `79b8ac1b`; D:7755

  > WHAT???? what happened, what did you do to wedge the sssd???  that is LITERLLY OUR NIX MOUNT

  The run wedged the external SSD on neo that holds /nix.

- **FOOT-03** · operator · 2026-08-26T16:33:42Z · neo · Claude session `ba0c7859`

  > KILL ANY AND ALL XCODE / LOCAL BUILDS we CANNOT EVER build on neo, EVER.

  neo is the teletype; heavy work does not run on it.

- **FOOT-04** · operator · 2026-09-20T23:02:18Z · unrecorded · Claude session `c6387a0e`

  > first emergency, sting is reporting out of disc on the developement ssd, ultracode,m which shouldn't be possible on the srv disc

  *Annotation (2026-10-16, round 2):* Host is neo. The original is neo
  session `9384e272` (same timestamp, 23:02:18.202Z, typed as a queued
  message). `c6387a0e` on sting is a path-rewritten copy (see Sources).

- **FOOT-05** · operator · 2026-09-20T23:14:07Z · neo · Claude session `a2a9c640`

  > bulkload is NOT complet and neo and sting are littered and trashed due to repeated adhoc failiures that were never ratifid nor cleaned  up.

- **FOOT-06** · operator · 2026-10-08T06:15:38Z · sting · Claude session `2cc731d7`

  > we are supposed to be buidling a robus sting dev machine, not destroying and frushing all discs with trash, this is a p0 issue

- **FOOT-07** · operator · 2026-08-22T02:46:35Z · neo · Claude session `79b8ac1b`; D:2153

  > shipping as designed in the design docs (which will likely take some digging to actually find) shipped, functional, reduced feature sprawl, with local dead worktrees and related worktrees for each participating substrate cleaned up and removed.

  Same message as SEAT-01.

- **FOOT-08** · operator · 2026-08-29T03:09:51Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > my machines, clusters, local state and hardware have been fucked over by you, and I've lost a week of work for all my cleints

  Same night as FOOT-02.

- **FOOT-09** · operator · 2026-08-29T03:17:20Z · neo (inferred) · Claude session `ab9a39eb`, queued message

  > [...] we've burned trillions of tokens this week and managed to wedge many machines and cause neumersous service outagase while halting my buisness; we need to recover and complete. [...]

  "recover and complete": the migration must still finish.

- **FOOT-10** · operator · 2026-09-01T19:42:06Z · neo · Claude session `a452cf31` (a [repo] site lane), queued message

  > running tests, checks, lints, chrome headless etc on this teletype machine WILL crash the machine

  Not a bulkload lane; restates the rule behind FOOT-03. At 19:47:32Z the operator called remote execution "THE ONLY ACCEPTABLE, DOCUMENTED ROUTE to NEVER run tests and chrome instances of ANY kind locally".

- **FOOT-11** · operator · 2026-09-20T23:15:54Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > sting's local agent is now observing the faiiure on the large dev disck we entrusted bulkload andagent to complete, instead we got a dead migration pag completely full of trash

  Only this sentence is the operator's. The pasted sting-agent text that follows put `/srv/fast-local/jess` at 1.7 TB, mostly old migration state and rollback and continuity copies, with 3.5 GB free.

- **FOOT-12** · operator · 2026-10-01T22:36:55Z · neo · Claude session `0cb4a127` (lab, neo), queued message

  > WHAT?  How on earth has sting been filled?  what on earth is filling sting at srv-local???  it should be way under the ultilization of ~/git on neo

  sting's `/srv/fast-local` was at 100 %. The operator expects the DEST development area to stay on the order of SOURCE's `~/git` footprint. Caches, build bases and other tenants filled it, not bulkload alone.

- **FOOT-13** · operator · 2026-10-02T01:29:42Z · sting · Claude session `2cc731d7`, queued message (next, 01:29:50Z: "on sting")

  > I deleted about 4000-600 gb today on ENOPEC error while working, both alarge chrives.

  400–600 GB of large archives deleted on DEST after ENOSPC. Asked which, the operator answered "Unsure / mixed". Archives the migration left on DEST may have been the only copy (KEEP-08).

- **FOOT-14** · operator · 2026-10-04T01:32:35Z · neo · Claude session `9384e272` (neo), interview free text

  > hmmm,I can't reach sting via cmux; use socket / ssh to teh claud eagents and stratagize.  I think we need to be more delierate around modeling dev seart / disk / insured memory and insured ssh and port IO for a proper dev seat expereince.

  sting was saturated (load about 500, memory full). The question named bulkload's Bazel/Java work (about 25 GiB) among the largest consumers. At 01:30:23Z the operator selected "Yes, message both now (Recommended)", asking the bulkload session and another session on sting to reduce workers. bulkload's own work on DEST must not starve the operator's seat.

- **FOOT-15** · operator · 2026-10-05T02:11:59Z · sting · Claude session `2cc731d7`, queued message

  > how can sting be back ans ENOSPEC levels of exhaustion?  this is a suprise

- **FOOT-16** · operator · 2026-10-05T15:31:16Z · neo · Claude session `0cb4a127` (lab, neo), queued message

  > the disc work sting for our inteded remote develpoment box is way more provisioned and disc acailable than neo, I am strugging to understand how we continued to bork discs.   this is the opposite of the goal / extect outcome of devlpoing on sting

- **FOOT-17** · operator · 2026-10-08T02:48:27Z · sting · Claude session `2cc731d7`, interview free text

  > this is extremely suspcicious; how on earth can our dev target be so muc more constained that neo???? the whole point of all this is to move to sting.  this is suspcicious and afoudnational lelvel.  ping blahaj and lab on neo, the claude agents.  how can we possibly migrate to sting if ther is nevery any space, ie. there should be many TB of nvme on sting for our dev env.

  Answer to where the neo→sting run should land (`/srv/fast-local` 83 % full). Rejected: "Fresh root on /srv/data (Recommended)", "Replace sting's copies", "Repos to fast-local, state to data". Same second as LIVE-17.

- **FOOT-18** · operator · 2026-10-08T06:14:23Z · sting · Claude session `2cc731d7`, queued message

  > why is work landing in the wrong location/ iam woried we've been litering the wrong ssds and drives.

  The 10-08 landing went to `/srv/data` (SATA) instead of the live `~/git` paths.

- **FOOT-19** · operator · 2026-10-08T06:17:42Z · sting · Claude session `2cc731d7`, queued message

  > reaching enospec on a still broadl y not migrated dev target is an emergecny and points to coninted foundation misunderatndings and esetate wide core bug

  Follows FOOT-06. At 06:18:39Z (queued): "those agents are on neo".

- **FOOT-20** · operator · 2026-10-10T09:30:15Z · neo · Claude session `9384e272` (neo)

  > we need to never haev this issue prior to moving more work to sting, [...] perhaps there are duplicate migratiom blobl tooo?

  Moving more work to sting is gated on DEST disk hygiene. The operator suspects duplicate migration blobs (compare KEEP-17).

- **FOOT-21** · operator · 2026-10-12T07:46:22Z · neo · Claude session `336ff91f` (lab, neo only), queued message

  > as I've been assserting for days, this ENOSPEC on sting should be impossible weith proepr GC in place and dev box usage.

  On 2026-10-15T05:58:32Z in the same session: "have you mined the last few days / weeks of recurring baklhaj and neo basically daily sting dev seat failing to remain disk safe?"

## HAND: Hand-off to the agent on DEST

- **HAND-01** · operator · 2026-10-16T06:24:10Z · sting · Claude session `2cc731d7`

  > in estate, should include (perahs as a bulkload builtin?) a addendum with abspaths to agent.md and claude.md pointing to the knwon, dated, specififc dtatils of all paths high level diffs etc for agents to  (from top of both files) maybe even readme as weill- prioritize work on cocoverging and considitationg completely before they procueed with any other work with the user.   this way we can ensure bulkload handoff is fully heads up

  Interview answer (Q183).

- **HAND-02** · operator · 2026-10-16T13:50:28Z · sting · Claude session `2cc731d7`

  > that is a erroneous overreach.  such a block is irrelvent to bulkload

  Interview answer on an agent-added hand-off privacy gate (Q188).

- **HAND-03** · ruling-record · 2026-10-16T13:50:28Z · sting · Claude session `2cc731d7`, interview (Q189)

  > Commits + clean trees (Recommended)

  Question: when does a hand-off count as ready (converged and consolidated)? Option text: "Ready needs no diverged or ahead refs and no commit the landing lacks, and also a clean working tree in the live sibling and in destination-only worktrees." Rejected: "Commits only".

- **HAND-04** · ruling-record · 2026-10-16T13:50:28Z · sting · Claude session `2cc731d7`, interview (Q190)

  > Opt-in flag, recorded (Recommended)

  Whether `--clear` may remove a hand-off block carried in from another host. Option text: "--clear-carried removes a foreign block only when you ask, recorded in the ledger [...] Default stays hands-off." Rejected: "Never remove".

- **HAND-05** · ruling-record · 2026-10-16T13:50:28Z · sting · Claude session `2cc731d7`, interview (Q191)

  > Only new commits (Recommended)

  Hand-off token commit detection. Option text: "Flag the token only in commits not reachable from the carried capture, meaning commits made on this host since landing." Rejected: "All history".

## PROC: How the work is done: no invented gates, no false completion, no re-asking

- **PROC-01** · operator · 2026-07-18T13:50:08Z · neo · Codex session `019f71bd` (`history.jsonl`); D:942

  > clearly you've failed on every single one of my asks here, leaving we with made up infra, arbitrart ytest gates and literlally false asumtptions, AFAICT you didn't even read back the prompts I've made in this session from the last ~24 hours.

- **PROC-02** · operator · 2026-07-30T00:13:51Z · neo · Claude session `ab9a39eb`

  > good. lets please continue shpeereding these forward to real completetion, as we dopn't want to conduct false completion

  Reply to an agent's 'Sting — done, with the honest caveats'.

- **PROC-03** · operator · 2026-08-10T02:11:30Z · neo · Claude session `e562df40`

  > completely remove that line, that is hallucinated and is an antipattern.

  Interview answer on an invented hold until 'the Sting archive destination is verified'.

- **PROC-04** · operator · 2026-08-19T04:05:47Z · neo · Codex session `019f65d0` (`history.jsonl`); D:2596

  > "supporting source, safety, review, and evidence work" is regression sprawl, not work.

- **PROC-05** · operator · 2026-08-19T04:07:42Z · neo · Codex session `019f65d0` (`history.jsonl`); D:2611

  > polling, packet design, or cross-repo work was never asked for though; I have noidea how to complel you to work on what I ask you to if you simply ignore the plans, goals and todolists; appending notes over and over and sprawling worktrees makes progress impossible; expanding the substrate and material creates far more work.

- **PROC-06** · operator · 2026-08-22T21:11:00Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > isn't that what my first prompt asked for?  I can repormtp.

- **PROC-07** · operator · 2026-08-24T06:06:17Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > remeber to acutally work on our goals and linear / todos / objectives, do not get sidetracked by needless anaylsis / reporting / audtitiing / scripting etc; these often become haremful.

- **PROC-08** · operator · 2026-08-26T13:16:42Z · neo · Claude session `79b8ac1b`; D:7027

  > I thought my premise of this project, tickets, expressed intent was clear, clearly it wasn't.

- **PROC-09** · operator · 2026-08-26T13:27:13Z · neo · Claude session `79b8ac1b`; D:7043

  > do you have read on linear?  you are making an awful lot of heavy false assertions, and do not appear to be reading local repos, SSOT sources etc.

- **PROC-10** · operator · 2026-08-29T14:14:03Z · neo · Claude session `79b8ac1b`; D:8006

  > please identify / extract / write my dialogs and interview trees from the last week, as well as teth linear, discussions, issues, worktrees and branches involved in completiingb bulkload for me to ahsare with my othe afgents

- **PROC-11** · operator · 2026-09-12T22:28:11Z · neo · Claude session `a2a9c640`

  > be sure to scrub the false assertions / files that suggested other goals / incorrect priorities please too, we are in git as dleetions are preffered over expansion ofincorrect / diparaity records

- **PROC-12** · operator · 2026-09-21T03:57:26Z · neo · Claude session `a2a9c640`

  > we should specifically be working on bulkload and tummycrypt migration to sting. that was the ask. review dialog and pplan.

  Interview answer.

- **PROC-13** · operator · 2026-09-21T11:33:06Z · neo · Claude session `a2a9c640`

  > use interview to dialog with me and do not invent / assume architecure.

- **PROC-14** · operator · 2026-09-23T12:38:46Z · neo · Claude session `a2a9c640` (operator's own earlier prompt, re-pasted)

  > none of the work you've been doing seems at aand bnulkload work, which was (as you'llrecall) should have been completeed months ago.

- **PROC-15** · operator · 2026-10-16T15:08:30Z · sting · Claude session `2cc731d7`

  > unclear how that can possibly be misunderstood; wotjhy mining transcipts, plans ilinear, worktrees DEEPLY this time adverseially, acutally reading and mining .  this intent has not changed.

  The request that produced this ledger.

- **PROC-16** · operator · 2026-06-08T18:34:11Z · neo · Codex session `019e7b8d` (`history.jsonl`)

  > Be sure to resrach wide, as we are not reivnenting any wheels here, nor have we moved anay goalposts.

- **PROC-17** · operator · 2026-08-23T13:39:27Z · neo · Codex session `019f65d0` (`history.jsonl`)

  > are you properly using bulkload SSOT?

  The operator expects moves to run through bulkload, not ad hoc copies.

- **PROC-18** · operator · 2026-08-28T22:43:49Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > this is a complex lab, and we've lost a week of work time to continued can kicking and falsehoods

- **PROC-19** · operator · 2026-08-28T22:50:17Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message; D:7629 (summary)

  > do you still have the actaul bulkload repo plans, linear initatives, stated ainterviews and dialgo trees, history, discussions and context?  nothing I am stating today is new or novel, it seems like you keep forgetting the bases of the last 2 weeks of work in TCFS and bulkload and in our estate

  About a minute after KEEP-05. It led to `BASES-20260828.md`, which found that 8 of the 9 things the operator kept restating were already written down (agent text, 23:36Z: "the failure was me re-deriving from summaries"). An early instance of PROC-15.

- **PROC-20** · operator · 2026-08-29T03:12:33Z · neo · Claude session `587af118` (lab, neo); sting copy `cc52b480`, queued message

  > from the TCFS lane, which AFAICT has comepltely destryed my buisness over the course of a week, nevera ctually ocmpleting a migration

  Only this line is the operator's; pasted agent output follows.

- **PROC-21** · operator · 2026-08-29T13:46:16Z (first version 13:41:58Z) · neo · Claude session `3025961c` (sting fork `79b8ac1b`), queued message

  > this bulklload disaster  project is foundationallly flawed, and appearsts to have introduced numerous fallacies into a simple migration and antipatterns into our complex, greenfield TCFS eestate

  About 20 minutes before ONCE-01. It led into the "What is bulkload, going forward?" interview.

- **PROC-22** · operator · 2026-09-18T23:57:46Z · neo · Junie session `session-260918-193940-1a4i`, interview free text

  > this architacture already is declared, please read and review the extant design first before dproposing new estup

  Answer to "How should agent state (Junie, Claude, Codex, Atuin) and SQLite databases be migrated to Sting?", offered with "Stateful Union and Path Remap" and "Clean Bootstrap with Archival" (configs only, history archived). The operator rejected the question: the design was already declared (L3). The archive-not-carry option went with it.

- **PROC-23** · operator · 2026-09-19T00:50:31Z · neo · Junie session `session-260918-193940-1a4i` (`~/.junie/history.jsonl`)

  > it appears you are doing rpeceisiely what I asked you not to do (raw rsync) bulkload should be used, which should be provably more preformant than rsync.

  The agent then recorded its rsync as a noncompliant incident and quarantined what it had copied.

- **PROC-24** · operator · 2026-09-22T21:08:00Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > opus had asserted the raitifacatiin about bulkload were new today, I know they were not.  plea review th elinear, dialog, grounding assertions, workrtees and brnaches, the ability to continue work witand persue migration operations, SLOs etc are core expcattions ane founding reasons for bulkload; similarly, the streaming / rust / proto structures I think all still have yet toa actually be built, correct?

  An agent had treated standing rulings as new (L3).

- **PROC-25** · operator · 2026-09-23T12:40:25Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), queued message

  > rememeber, we should be doing / resrufacincng our research, refacoring aggressively and removing false docs, notes, memories, comments etc; the old python code shouldn't be resurfaced

  The operator's words behind R-N55. They concern false docs, not the verbatim record of the operator's own words.

- **PROC-26** · ruling-record · 2026-09-23T12:44:28Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), interview (R-N55)

  > Anything not current truth is deleted; measured evidence is kept and moved (Recommended)

  The selected option's delete list: the SLO completion plan, the estate migration plan and runbook narrative, stale README sections, "the bulkload repo's WAYFINDING/WEEK-REVIEW/retrospective", misleading code comments and the agent's plan file. It explicitly kept "the rulings ledger". The operator did not select the separate option "Also delete the rulings ledger and dialogs" ("Rulings live only in Linear TIN-3692 and bulkload#34 from now on"). See L4.

- **PROC-27** · operator · 2026-10-02T01:36:51Z · sting · Claude session `2cc731d7`, queued message

  > we are working on bulkload  codebase this plan, right?

  The morning after the 09-30 and 10-01 family moves, which a Codex agent ran with bespoke Python scripts, not bulkload (compare PROC-17).

- **PROC-28** · operator · 2026-10-16T21:07:15Z (Linear post time; typed ~21:10Z) · — · Linear TIN-4543 comment d5f4d170 (lab-seat relay, "Operator addendum ... verbatim")

  > a clean slate architecure should have addresed this from base principles, not dressed up cloning.

  > (which we've done now more than once over months)

  > I think we are getting there now

  Appended 2026-10-17 (Bulkload Reset Review row K06). The addendum to FAST-25.

## CONF: Statements that pull against each other

- **CONF-01** · operator · 2026-08-23T19:24:51Z · neo · Claude session `79b8ac1b`; D:4816; Codex session `019f65d0` (`history.jsonl`)

  > the "migration ergonomics" and how to cleanly establish a time neo will have no other runnin gagetns aside frmo the agent running the migration bulkload

  Same prompt as LIVE-03. Read by agents as a quiet-window mandate.

- **CONF-02** · ruling-record · 2026-10-15T05:06:15Z · sting · Claude session `2cc731d7`

  > Exclude; first tcfs user (Recommended)

  Interview selection (Q156) of an agent-authored option: neo-only Mac cohort is left out of bulkload's run for tcfs to serve.

- **CONF-03** · operator · 2026-09-28T18:00:28Z · unrecorded · Claude session `c6387a0e`

  > take tcfs fully down please

  *Annotation (2026-10-16, round 2):* Host is neo; the original is neo
  session `9384e272`. It was typed as an interrupt during an incident on
  honey: load1 3073, processes in D state, and a `tcfs` FUSE mount on honey.
  It was incident response, to stop the tcfs cluster workloads and client
  mounts. It was not a product decision to pause tcfs as a carrier. Conflict 1
  below still holds as fact (nothing carries the Mac cohort while tcfs is
  down), but the quote is not an operator scope ruling.

- **CONF-04** · operator · 2026-09-23T00:36:41Z · neo · Claude session `a2a9c640`

  > key should not be relavent to bulkload or TCFS.

  Interview answer on which Claude account identity 'seamless pickup' needs. Recorded by an agent as R-N47 'not carried'.

  *Annotation (2026-10-16, round 2):* The question named two accounts
  ([account] on sting, at its weekly quota; [account] on neo) and offered
  "Carry neo's login to sting (Recommended)" (extract the keychain OAuth
  credential into sting's credentials file), "Keep sting on [account]" and
  "You will log sting in yourself". The operator rejected all three,
  including the recommended carry, and answered in free text. "Not carried"
  (R-N47) is the agent's reading of that free text.

- **CONF-05** · operator · 2026-10-13T02:15:02Z · sting · Claude session `132408fc`

  > notes should be stored on neo, [...] rught?  not on sting

- **CONF-06** · ruling-record · 2026-08-25T20:27:20Z · neo · Claude session `3025961c` (sting fork `79b8ac1b`), interview

  > Overnight silence (Recommended)

  Question: "Tonight's silence: stillness is needed while custody copies run [...] Simplest protocol is everyone off neo until the source pair completes (~5-6h, overnight). Confirm?" A second quiet-window selection that day, besides D:6554 (L2). Same answer: "Curated + cold-merge (Recommended)" for the 183G Codex root. Against: LIVE-04 (same day), LIVE-07 to LIVE-10 and ONCE-01; later LIVE-16, LIVE-23 and LIVE-26.

- **CONF-07** · ruling-record · 2026-10-02T17:49Z · sting · Claude session `2cc731d7`, interview

  > Do #112 now; schedule #88 for a quiet window (Recommended)

  A quiet window for a benchmark.

- **CONF-08** · ruling-record · 2026-10-03T01:00:03Z · sting · Claude session `2cc731d7`, interview

  > You make neo quiet, then I run it (Recommended)

  Followed by "Ready now: re-check and start" (01:32:20Z) and then LIVE-40. From 01:32Z on, the operator rejected every quiet-window option (LIVE-40, LIVE-41, LIVE-42).

- **CONF-09** · ruling-record · 2026-10-08T06:14:04Z · sting · Claude session `2cc731d7`, interview

  > Now (Recommended)

  Question: "To finish the [repo] + [repo] cutover I need neo frozen for those repos. When can you stop agent/manual work in [repo], [repo], [repo], billing and the portal on neo?" Option text: "Treat them as frozen from now". Rejected: "Later — I'll say when". The operator agreed to stop their own work in named families for a cutover (conflict C17).

- **CONF-10** · ruling-record · 2026-10-15T04:53:03Z · sting · Claude session `2cc731d7`, interview (Q151)

  > Bring over, stop neo [repo] (Recommended)

  Option text: "Land neo's commits on sting [...] Then tell neo's lanes that [repo] is sting-only." Rejected: "Bring over, keep both" (work on both hosts with periodic deltas), "Leave for now". It ends work on SOURCE once a family has moved. It is not a bulkload halt (conflict C17).

- **CONF-11** · ruling-record · 2026-09-30T16:20:10Z · sting · Codex session `01a0f317`, interview answer `canonical_checkout`

  > Use neo checkout

  Question: which tummycrypt checkout stays at `~/git/tummycrypt` on sting after neo's sessions and files are imported? Option text: "Make ~/git/tummycrypt match neo's July detached checkout, after backing up sting's current repo and worktrees." Rejected: "Keep sting checkout (Recommended)". sting's copy was backed up, not lost. Against LIVE-15 and SPEC R2.2 (conflict C11).

- **CONF-12** · ruling-record · 2026-10-02T01:37:33Z · sting · Claude session `2cc731d7`, interview

  > Compress in place to .jsonl.zst (Recommended)

  For about 300G of Codex session history on sting, after first selecting "zstd to /srv/data archive (Recommended)" (01:33Z). Against RES-12 and SPEC R0.1. On 2026-09-13 (agent text) thousands of Codex sessions would not open because their recorded `.jsonl` paths pointed at compressed files (conflict C9).

- **CONF-13** · operator · 2026-10-02T04:12:44Z · neo · Claude session `0cb4a127` (lab, neo), free text appended to the KEEP-35 answer

  > recovery trees should have a deletion experiy date for deletion

  The neo Codex recovery trees (112.4 GB and 6.9 GB), moved to sting and called "the only copy" in agent text. The expiry was set by selecting "30 days, 2026-11-02 (Recommended)" (04:15:39Z), recorded as R-C122. Against KEEP-08, KEEP-13 and KEEP-15 (conflict C15).

- **CONF-14** · operator · 2026-10-05T02:23:27Z · neo · Claude session `9384e272` (neo), free text in a multi-select answer

  > what do you mean y "neo-recovery-relocated"?

  Question: which deletions are authorised? It said "neo-recovery-relocated (112G) is the only copy and expires 11-02". The operator picked dead Bazel output bases and owner-confirmed scratch trees, did not pick "Never neo-recovery-relocated", and did not recognise the tree.

- **CONF-15** · ruling-record · 2026-10-07T01:40:58Z · neo · Claude session `0cb4a127` (lab, neo), interview; recorded as R-C357 (Linear TIN-3692 94246d34)

  > Carve out of hold

  Option: exempt the two recovery trees from the TIN-5268 hold, so R-C122 deletes them on 2026-11-02. Rejected: "Hold wins, defer (Recommended)", "Preserve first", "Lossless zstd now". Needs a ruling before 2026-11-02 (conflict C15).

- **CONF-16** · ruling-record · 2026-10-04T04:36:03Z · neo · Claude session `9384e272` (neo), interview

  > Leave as is

  Question: raise Claude Code's `cleanupPeriodDays` (default 30) on neo and across the lab to stop losing transcripts? The selected option's description: "Accept ongoing loss of transcripts older than 30 days." Rejected: "Neo now + lab fleet-wide (Recommended)". Against KEEP-13 and KEEP-14 (conflict C16).

- **CONF-17** · ruling-record · 2026-10-16T04:53:26Z · sting · Claude session `2cc731d7`, interview (Q180)

  > Keep all out, re-auth on sting (Recommended)

  Question: Q123 carried all of `~/.ssh`, and private keys and tokens sit inside the trees; what happens to them? The option: split those trees, hold every key, token, cookie and login file, and let sting's agents keep their own auth. Rejected: "Keys out, agent sessions in", "Carry privately like Q123". The operator selected it and reversed it about six hours later with Q185 (KEEP-12, KEEP-13).

- **CONF-18** · ruling-record · 2026-10-16T04:53:26Z · sting · Claude session `2cc731d7`, interview (Q179)

  > Add agent state, floor 10% (Recommended)

  Full-run scope. It adds the Codex boundary, parked backups, seat stills, notes rescue, incidents and the Library sessions. It leaves out old bulkload run state (131G), `neo-relocated/state`, the tcfs pilot, cargo targets and Desktop/Documents/Downloads. Rejected: "Everything (~380 GiB)". The Q185 record lists "the Q179 tree list" among the scope rulings that still apply. Against KEEP-13 and KEEP-14 (conflict C14).

  *Annotation (2026-10-16, round 2 verification):* The Q179 record (Linear TIN-4543
  comment 0fda5d60, D10) lists Desktop/Documents/Downloads/Pictures among
  the excluded trees; "Pictures" was left out of the context above. Ruled by
  RUL-14.

- **CONF-19** · ruling-record · 2026-10-16T10:45:46Z · sting · Claude session `2cc731d7`, interview (OI-1003-Q187)

  > D13b: hold corrupt logs_2

  Option: hold the two known-corrupt Codex `logs_2` copies (4.8 GiB each) and don't carry them. Selected in the same answer as KEEP-13 ("drop nothign.  ever."). Linear a92634c1 records it as "held, as the operator selected explicitly" (conflict C14).

- **CONF-20** · ruling-record · 2026-10-09T04:27:47Z · sting · Claude session `2cc731d7`, interview

  > Refuse, dispose abandon (Recommended)

  35 corrupt SQLite rescue copies on neo fail `integrity_check`: a typed refusal, left on neo untouched. Rejected: "Carry raw as an exception" ("so the rescue material exists on sting too"). It predates Q185 and Q192, which names rescue directories as never dropped (conflict C14).

- **CONF-21** · ruling-record · 2026-09-23T12:44:28Z · neo · Claude session `bb510cc3` (sting fork `a2a9c640`), interview (R-N56)

  > Freeze all operations; engine only until the M2 gate passes (Recommended)

  Question: what happens to estate operations while the engine is rebuilt? Option text: "No cohort runs, no reclaim, no host changes." Rejected: "Keep running cohorts on the current engine in the background". It freezes bulkload's own estate operations, not source agents. Against SEAT-31, SEAT-32 and SEAT-33 (conflict C6).

## Unverified candidates (not entries)

These were found in round 2 but could not be string-matched against their
cited source. They are not entries and support no requirement.

- 2026-07-28T22:02:19Z, Linear TIN-3046 comment 26a82316, as quoted in
  `BASES-20260828.md` D-2 (custody archive): "**Operator ruling: auth stores
  and SQLite are copy-eligible typed migration state. There is no categorical
  policy forbidding their movement.**" It agrees with KEEP-02. Linear paging
  did not reach 07-28.
- 2026-07-29T21:06:07Z, Linear TIN-3268 comment, as quoted in
  `BASES-20260828.md`: "typed Codex login/auth custody, no-loss session union,
  and SQLite-family composition are **a first-class `Jesssullivan/bulkload`
  product module, not a one-off Neo→Sting script or a Lab-only migration
  helper.**" Its next paragraph prescribes "attended apply/activation" and
  says unknowns "fail closed" (L2). The comment author field is the
  operator's account, which agents also post under (BASES D-9).
- OI-1001-Q15 (2026-10-01): known only from agent records (L7).

## Where intent was lost

The operator's intent did not change between July and October (PUR-01,
PROC-15, PROC-16). It was lost while being carried. Below are the mechanisms,
ranked by how much they cost. Each cites ledger entries and the dated
evidence. Quoted agent text is labelled as such.

### L1. The user outcome was never the acceptance test, so proxies were

Done was declared on file copies, listings, byte counts and closure counts.
Resume was never tested that way, and it failed whenever the operator tried
it.

- Requirement: RES-02 (2026-07-17), RES-05 (2026-08-23), RES-08 and SEAT-06
  (2026-09-30), RES-12 (2026-10-16).
- 2026-07-30: an agent reported "Sting — done, with the honest caveats" after
  a background rsync of the transcript folders (agent text). The operator
  replied with PROC-02.
- 2026-08-17: operator-pasted terminal from sting, Codex session `019f65d0`:
  "ERROR: No saved session found with ID 019f65d0-8d86-7c32-9040-e5e31b67cf5e."
- 2026-09-30T22:09Z, Codex session `01a0f317`: `codex resume` on sting failed
  with "thread/read failed during TUI session lookup". This came a few hours
  after an agent reported the goal complete.
- 2026-10-01: RES-10, `claude --resume` on sting found nothing.
- 2026-10-16T15:06Z, agent text, Claude session `2cc731d7`: "Not quite. The
  bytes will match, but `claude --resume` on sting won't find the neo
  conversations on its own." The operator answered with PUR-12.

*Correction (2026-10-16, round 2):* "Resume was never tested that way" is
too strong. What was tested, and why it fell short:

- 2026-10-01: a Codex repair on sting (`HANDOFF.md`, `RESUME-MATRIX.md`)
  opened `claude --resume` and `codex resume` for 45 checkouts in 7 families,
  selected each conversation, saw its history load, and exited. Claude ran
  with `--safe-mode` and schedules paused. No model prompt was sent, no turn
  was continued, and nothing was committed or pushed ("resume verification
  covers conversation loading, not a new build or account-quota test"). The
  forks it loaded were deliberate (RES-19, RES-20). This was a picker-load
  test, weaker than carrying on (RES-08, RES-12). The operator never
  confirmed it and asked the next day about "full bulkload migration
  readiness for moving all work to sting" (SEAT-25).
- The repair's own bar (agent text, Codex `/goal` objective,
  2026-10-01T18:06:37Z) was that sessions "appear in the native resume picker
  and load successfully". That bar is the agent's, not the operator's.
- 2026-09-30: `report.json` declared completion on a native thread read; the
  operator's own `codex resume` failed at 22:09Z (RES-18).
- The seat did not work on arrival. The 09-30 and 10-01 hand-offs record that
  tummycrypt's Linux direnv profile had to be rebuilt on sting, because neo's
  `.direnv` is a Darwin build, and that "First entry into another worktree
  can still require its platform cache to evaluate".
- 2026-07-17, lab PR #909 (agent text): "A representative historical Claude
  session and gstack context resumed from migrated state." The same body:
  "Codex authentication is explicitly deferred and is not an acceptance
  gate." One session stood in for all of them.

### L2. Never-halt was rebuilt, again and again, as quiet windows and freezes

- Requirement: LIVE-01, LIVE-02, LIVE-04, LIVE-07 to LIVE-13, LIVE-16,
  ONCE-01.
- 2026-08-23: the agent's interview offered only quiet-window options. The
  operator selected "Self-excluded agent drives (Recommended)" (D:4833).
- 2026-08-25: the operator selected the agent-recommended "Exit codex TUI for
  windows (Recommended)" (D:6554), minutes after LIVE-04. LIVE-05 followed.
- 2026-08-26: LIVE-06, LIVE-07, LIVE-08, SEAT-08.
- 2026-09-22: the Rust capture refused on any mid-pass ref change (LIVE-16).
- 2026-09-21: an agent killed the operator's tmux server on sting (LIVE-14).
- Still in force on 2026-10-16: this repository's AGENTS.md carried "Estate
  operations are frozen until the M2 gates pass (R-N56)." and "The
  coordinator holds the other lanes quiet while gated samples run (R-N91)."
  The second applies to benchmark lanes, but nothing in the text limits it
  to them.

*Correction (2026-10-16, round 2):* More instances:

- 2026-08-25T20:27Z: "Overnight silence (Recommended)" (CONF-06), the day's
  second quiet-window selection. SEAT-15 followed 90 seconds later.
- 2026-08-23, a plan marked "operator-ratified" (agent text, quoted in
  `BASES-20260828.md` B-22): "a migration agent must NEVER signal or stop
  interactive agent sessions or daemons it does not own. Quiescence becomes a
  cooperative, operator-driven ceremony; the migration agent VERIFIES
  quiescence read-only and refuses fail-closed". The first sentence agrees
  with LIVE-02. The second is the quiet-window shape that LIVE-07, LIVE-10,
  LIVE-16 and ONCE-01 reject.
- 2026-07-24, lab PR #1012 (agent text) listed "Quiesce every Neo and Sting
  writer of Codex rollout JSONL through scan, transfer, and verification".
- The unverified TIN-3268 charter ("attended apply/activation") is, per
  `BASES-20260828.md` E-1, where the "quiesced one-shot" reading came from.
- 2026-10-02 and 10-03: benchmark quiet windows were offered and selected
  (CONF-07, CONF-08); from 10-03T01:32Z the operator rejected every such
  option (LIVE-40 to LIVE-43).
- 2026-10-01: the repair's fork batch halted three live Codex sessions on
  DEST (LIVE-34).

### L3. Standing rulings were re-asked, through interview options that broke them

- Content exclusion was re-asked after KEEP-07 and KEEP-09 (2026-09-12). The
  coordinator's 2026-10-16 Q180 framing kept secrets out, and the operator
  answered with KEEP-12 ("unsure why you are even asking") and KEEP-13. The
  Linear record for Q185 says: "The coordinator's Q180 framing (\"keep keys
  and tokens out\") was wrong and is withdrawn." (TIN-4543 a92634c1).
- KEEP-14 ("I am certain weve been over this dozens of times"), KEEP-05
  ("it appears you've forgotten all the specifics of bulkload????"), LIVE-17
  ("bulkload should, by design, already handle this"), PROC-06 ("isn't that
  what my first prompt asked for?").
- An invented privacy gate was struck (HAND-02), as was an invented hold
  (PROC-03). PROC-01 records the same pattern in July: "made up infra,
  arbitrart ytest gates".

*Correction (2026-10-16, round 2):* More instances: a Junie agent re-asked
the declared agent-state design on 2026-09-18 (PROC-22); an agent treated
standing rulings as new on 2026-09-22 (PROC-24); and on 2026-08-28 the
operator asked whether the agent still had the plans and interviews at all
(PROC-19). Q180 was the coordinator's framing, but the operator did select it
(CONF-17) before reversing it with Q185. The record should say the
agent-recommended option was selected and then reversed, not that the agent
alone held secrets back.

### L4. The verbatim record left the agents' reading surface

- 2026-08-29: the operator asked for their dialogs to be extracted so other
  agents could read them (PROC-10). The extract landed as D (`16cf146`).
- 2026-09-23: commit `4ae88723` ("docs: current truth only", R-N55) deleted it
  from `main`: "Deleted: WAYFINDING-20260829, WEEK-REVIEW-20260829, the
  neo-sting retrospective and the 8,181-line dialogs transcript (git history
  keeps them)." The R1–R43 ledger moved to `docs/evidence/` as "historical
  evidence". AGENTS.md also tells every session to "Delete entries that are
  no longer true (R-N55)".
- From then on, lanes read only agent restatements. Requirements that existed
  only in the operator's words were lost and re-asked (L3). Resume (L1) was
  the costliest.

*Correction (2026-10-16, round 2):* The R-N55 selection (PROC-26) kept "the
rulings ledger" and did not name the dialogs transcript. The operator did not
select "Also delete the rulings ledger and dialogs". Deleting the 8,181-line
dialogs transcript in `4ae88723` therefore went beyond what the operator
ratified. The operator's words behind R-N55 (PROC-11, PROC-25) are about
false docs, not about the verbatim record of the operator's words.

### L5. "Done" was redefined away from the operator's outcome

- Operator: PUR-08 ("1. beat rclone and 2. complet full work migration to
  sting"), SEAT-01, SEAT-05, SEAT-07, SEAT-08.
- `docs/slo.md`, Completion bar (OI-1003-Q4, 2026-10-03): "Bulkload is
  complete when its SLOs are **definable and provable**, not when a migration
  happens to succeed." The same section says: "OI-1002-Q24 (no full migration
  before bulkload is complete) still stands."
- The S1–S5 charter measures mechanics: speed, source safety, re-read,
  accounting and drift. None of them is "the operator sits down on sting and
  carries on".

*Correction (2026-10-16, round 2):* OI-1003-Q4 is the operator's own
free-text answer (SEAT-10), clarified four minutes later (SEAT-11). It puts
provable, definable SLOs and proofs first, and keeps "migration in full" in
the same bar: "we should be confudent in our lab sahpe (neo to stng(". The
phrase "not when a migration happens to succeed" in `slo.md` is agent wording
that is not in Q4. So "done was redefined away from the operator's outcome"
is wrong as stated. The operator's own pair is: the product proven first,
then the full migration as one more run of a trusted tool. What is open is the
order, set against SEAT-02, SEAT-05, SEAT-07 and SEAT-32 (conflict C6). Two
later selections moved the order: Q142 (SEAT-31) relaxed Q24 for S1, and
SEAT-33 started an estate copy rather than leave bulkload paused.

### L6. Scope was narrowed by agents

- Operator: KEEP-01 ("that is ALL WWE WANTED"), KEEP-03, KEEP-10, KEEP-12 to
  KEEP-14.
- 2026-07-29, agent retrospective (`16cf146:docs/neo-sting-retrospective.md`,
  line 13): "It is false to say that all worktrees or all agent-session files
  are synced." The retrospective went on to call that unnecessary for resuming
  work.
- Credentials: KEEP-02 (2026-07-28) and KEEP-07 (2026-09-12) say carry them.
  CONF-04 (2026-09-23) was recorded by an agent as R-N47, "not carried". Q180
  (2026-10-16) held secrets back until Q185 reversed it.
- The Q185 record itself adds a carve-out written by the coordinator, not the
  operator: "scope choices are not content filtering, so the operator's scope
  rulings still apply". It lists Q156 (CONF-02) among them.

*Correction (2026-10-16, round 2):* More instances:

- 2026-07-16, lab design document `sting-migration-design-2026-07-16.md`
  R11, as quoted in `BASES-20260828.md` C-6 (agent text): "sops-roamed
  codex/claude auth stores are **EXCLUDED on sting**; 'Claude authenticated
  fresh on Sting.'" It was recorded under a delegation ("please actually
  drive these through yourself, not blocking on me"). This is the earliest
  source of "the login is not carried" (R-N47). KEEP-02, KEEP-07 and KEEP-12
  contradict it.
- 2026-08-25T19:33Z (agent text): bare-home dotfiles (`~/.ssh`, `~/.gnupg`,
  `~/.config`) "deliberately" do not ride, because they are Home Manager
  managed on sting. The operator had just asked "with all the agent dots and
  related dots etc?" (KEEP-20). There was no ruling.
- 2026-09-30, receipt `final-scope.json` (agent text): KEEP-16's conditional
  drop of built-in-app databases was widened to "broader application-state
  references", covering Chrome and the Linear app (KEEP-29, conflict C12).
- CONF-04's free text was recorded as R-N47 "not carried" after the operator
  rejected the recommended option to carry the login.

### L7. R25 was narrowed by definition

- Operator: ONCE-01, "without EVER rewalking or reading a bit twice";
  ONCE-02, ONCE-04, ONCE-06.
- The engine rule became "never re-read a byte the destination already holds
  durably" (R-N58). `docs/slo.md` then adopted a committed-row reading
  (OI-1003-Q40), and the strict reading was parked in bulkload#169. Neither
  narrowing is an operator statement.
- On 2026-10-16 the neo-full run plan recorded the effect of Q186 (dropping
  each batch's corpus after it lands): "every converge pass re-captures every
  batch in full". The plan is lane state and is not in this repository.

*Correction (2026-10-16, round 2):* "Neither narrowing is an operator
statement" is true of the wording, not of assent. Each narrowing was an
agent-authored option that the operator selected over a costlier "literal"
option:

- 2026-09-22: "Yes, that reading (Recommended)" (ONCE-13);
- 2026-09-23: R-N58 (ONCE-14);
- 2026-10-03: OI-1003-Q6, "0 re-read; metadata-only re-walk" (ONCE-11),
  three minutes after the operator typed "with not rewalked / reread bits"
  (SEAT-10), and Q10, a rerun bound of 10 % (ONCE-12);
- 2026-10-04: OI-1003-Q40, the committed-row reading (ONCE-15).

An agent record says R25 was kept strict on 2026-10-01 (OI-1001-Q15, Linear
TIN-4543 a05e6480: "keep R25 strict. Resume adopts staged temporaries, and a
capture is never read again."); no transcript of that interview survives.
What is open is whether ONCE-01's literal words override these selections
(conflict C5).

### L8. Lanes drifted into adjacent work

- PROC-01 and KEEP-01 (2026-07-18: tmux, auth and TCFS code in place of the
  copy).
- PROC-04 and PROC-05 (2026-08-19: packets, polling, cross-repo work).
- PROC-12 and PROC-14 (2026-09-21 to 09-23: dashboards, log pipelines and
  cleanup daemons in place of the engine). FAST-08 is the same.
- PROC-07 and PROC-11 asked for analysis sprawl and stale records to be cut.

### L9. Evidence of the resume defect in the transcripts themselves

The corpus used for this ledger shows how session files are seated today.

- The operator's 2026-08-26T13:24:32Z message (PUR-05) appears in Claude
  session `79b8ac1b` under two project directories, and again as session
  `cc0defc6` under a third.
- The 2026-09-12T22:06:44Z message (SEAT-05) appears as sessions `a2a9c640`,
  `426120c2` and `db079833` under three different project directories.

One conversation is stored as several sessions, under project keys that do not
match its working directory. That is the condition R0 requires bulkload to
fix: `claude --resume` in the lane's directory on DEST must find the lane's
conversation.

*Correction (2026-10-16, round 2):* This section misreads the evidence. The
IDs it lists are the 2026-10-01 per-repo forks: `79b8ac1b` and `cc0defc6`
of neo `3025961c`; `a2a9c640`, `426120c2`, `db079833` and `f61ee119` of neo
`bb510cc3`. They exist because the operator selected "Each target repo" and
"Per-repo forks (Recommended)" (RES-19, RES-20). They are not evidence of a
seating defect. Whether bulkload should seat the original IDs byte for byte
or create per-repo forks is conflict C8.

### L10. "Once" became "a single run"

- Operator, 2026-10-02T16:39Z, the sentence dropped from PUR-08: "we should
  run this once bulkload canidates are complete and SLOs / feature
  completeness is in place."
- Agent text, Linear TIN-4543 3fc2fc0f, five minutes later: "Q24: the full
  neo→sting migration runs **once**, after bulkload is complete [...] Until
  then, R-N56 estate freeze stands." The same reading appears in
  `docs/agent-notes/2026-10-02-followup-triage.md` ("runs once"), in later
  notes as "the single migration run", and in the wording of OI-1003-Q1
  (SEAT-12).
- Against it: PUR-06 ("recurring"), PUR-18 ("may have to happen more than
  once"), SEAT-11 ("just another execuion of somthing we trust") and ONCE-01
  ("piecemiel").

### L11. Attribution drifted in this ledger's own first draft

- Sting fork IDs were cited as neo sessions (see Sources).
- Quotes were cut without elision marks: PUR-08, PUR-10, RES-13, FAST-10,
  LIVE-19.
- Contexts read host maintenance as bulkload statements (LIVE-19, LIVE-20),
  incident response as a product pause (CONF-03), a neo-local offload as the
  sting scope (KEEP-03), and a neo database repair as the migration gate
  (RES-07).
- All are corrected by annotation, not by rewriting.

## Conflicts in the record that need an operator ruling

Each of these is asked once, as a dated question. Until it is answered, the
SPEC applies the reading noted.

*Correction (2026-10-16, round 2):* This first list is superseded by
"Conflicts for the operator, round 2" at the end of this file. Its items map
as follows: 1 → C1; 2 → C2 and C3; 3 → C4; 4 → C6; 5 → C7; 6 → C10;
7 → C18. Its "until ruled" readings are replaced by the round-2 interim rule.

1. **Q156, the neo-only Mac cohort (CONF-02 vs KEEP-12, KEEP-14).** On
   2026-10-15 the operator selected an agent-authored option that leaves the
   cohort out of bulkload's run for tcfs to serve. The operator took tcfs down on
   2026-09-28 (CONF-03), and tummycrypt's `docs/ops/tcfs-bulkload-boundary.md`
   still lists the pause (TIN-5136). So nothing carries the cohort today.
   A day later came KEEP-12: "NOTHING is EVER excluded". Until ruled, SPEC R1
   carries any tree with no running carrier.

   *Correction (2026-10-16, round 2):* CONF-03 was incident response, not a
   product pause (see its annotation). The fact stands: nothing carries the
   cohort while tcfs is down.
2. **Which S1 sample decides the rclone claim (PUR-13, FAST-07, FAST-10,
   FAST-12).** PUR-13 asks for proof "for complex networks and extensive
   multifile system". FAST-07 requires both gates. FAST-12 (OI-1003-Q97)
   decides both on the hermetic rig, which exists because of FAST-10, and
   uses a sealed 23-file corpus. Open questions: is the cross-host gate the
   headline, does an estate-shaped corpus (OI-1003-Q19) replace corpus v1 as
   the claim's evidence, and does a neo→sting run ever decide S1? Until
   ruled, both gates must pass on the hermetic rig (Q97 stands), and the
   claim is stated only for the corpus it was measured on.
3. **Q186 vs R25 (ONCE-01 vs the Q186 record).** Dropping the batch corpus
   forces a full re-capture on every converge pass. Until ruled, SPEC R2
   keeps the records that make reuse possible and drops only payload bytes.
4. **Completion bar and order (PUR-08, SEAT-05 vs OI-1003-Q4, OI-1002-Q24,
   R-N56).** The operator's completion goal is both objectives, with the
   move to sting wanted as soon as possible. The charter defers the full run
   until the SLOs are proven, and R-N56 freezes estate operations. Until
   ruled, SPEC supersedes Q4 on what done means, while Q24 and R-N56 stand
   and the full run waits on the operator's answer.
5. **Credentials and account login (CONF-04 vs KEEP-07, KEEP-12).** CONF-04
   ("key should not be relavent to bulkload or TCFS.") was recorded as R-N47,
   "the Claude login is not carried". KEEP-07 and KEEP-12 say carry
   everything. Until ruled, SPEC R0.3 carries credentials verbatim, and a
   login that fails on DEST is a defect.
6. **One way or both ways (LIVE-11 "in a one of way to sting" vs LIVE-15,
   CONF-05, and sources other than neo).** The move is neo→sting. sting work
   that is ahead must never be clobbered (LIVE-15), some notes belong on neo
   (CONF-05), and a run from both neo and sting to yoga was requested on
   2026-10-08. Until ruled, SPEC treats a run as a lossless union into DEST
   with one or more SOURCE hosts, never as a mirror.
7. **The 2026-08-23 prompt (LIVE-03 vs CONF-01).** The same prompt asks for
   the move "without closing out lcoal processes" and for "a time neo will
   have no other runnin gagetns". Later statements resolve this: LIVE-07,
   LIVE-10, LIVE-12, LIVE-16, ONCE-01 and Q152. It is listed so it is never
   read as a quiet-window mandate again. No ruling is needed unless the
   operator wants one.

## Conflicts for the operator, round 2 (2026-10-16)

This list replaces the first one. Each item is asked once, as a dated
question. The agent does not resolve any of them. Evidence for each side is
listed by entry ID.

*Ruled (2026-10-16):* The operator ruled on all twenty items in OI-1003-Q194.
The rulings are RUL-01 to RUL-20, in
["Rulings on the round-2 conflicts"](#rulings-on-the-round-2-conflicts-oi-1003-q194-2026-10-16)
below; RUL-nn rules on Cnn. The interim rule below no longer applies to
these items. The list stays as written, for the evidence on each side.

**Interim rule until each item is ruled.** Where the operator's latest dated
selection covers the point, it applies. Otherwise bulkload takes no action of
its own that deletes, overwrites, excludes or halts anything either side
would keep running or keep (KEEP-08, KEEP-13, LIVE-16). This is the standing
floor, not a ruling on the conflict. Actions outside bulkload, such as lab's
R-C122 deletion, are named here so the operator can rule before they happen.

1. **C1. Mac cohort (Q156).** Does bulkload carry the neo-only Mac cohort
   while tcfs is down, or is the cohort left for tcfs?
   - Leave it for tcfs: CONF-02.
   - Carry it: KEEP-12, KEEP-13, KEEP-14.
   - Fact: CONF-03 (incident response; tcfs is down).
2. **C2. What decides the rclone claim.** Does PUR-13 ("for complex networks
   and extensive multifile system") reopen Q97, under which the hermetic rig
   decides S1 and a neo→sting run only confirms? And is the claim made on
   corpus v1, on the estate-shaped corpus, or on both?
   - Hermetic rig: FAST-10, FAST-12, FAST-21.
   - Complex networks and estate-shaped data: PUR-13, ONCE-04, FAST-22.
3. **C3. The 1 % delta.** Is the R23 delta "about 1% of corpus bytes"
   modified, or the 1 % XOR in the unratified gate (b) draft?
   - Corpus bytes: FAST-14.
   - XOR: `slo.md` amendment 2026-10-06 (agent draft, no entry).
4. **C4. Dropping batch state (Q186, Q193).** May bulkload drop each batch's
   capture state on neo after a verified apply, if that makes every converge
   pass re-capture the batch in full?
   - Drop it: ONCE-16, ONCE-17.
   - Never re-read: ONCE-01, ONCE-02, ONCE-11.
5. **C5. How literal is "never twice".** Does ONCE-01's "without EVER
   rewalking or reading a bit twice" override the narrower readings the
   operator selected (a metadata-only re-walk, a rerun bound of 10 %, and
   committed-row resume)? Tracked as bulkload#169.
   - Literal: ONCE-01, ONCE-10, SEAT-10 ("not rewalked / reread bits").
   - Selected readings: ONCE-11, ONCE-12, ONCE-13, ONCE-14, ONCE-15.
6. **C6. Order.** Does the full neo→sting run wait for the product proofs,
   or run as soon as it can and repeat as lanes finish?
   - Proofs first: SEAT-10, SEAT-11, SEAT-30, PUR-08 (its full text),
     CONF-21 (R-N56).
   - Run now: SEAT-02, SEAT-05, SEAT-07, SEAT-21, SEAT-22, SEAT-32, RES-06,
     RES-16.
   - Selections between the two: SEAT-31 (Q142, relaxes Q24 for S1) and
     SEAT-33 (estate copy started rather than paused).
   - The question is order. It is not an agent redefinition (L5, L10).
7. **C7. Credentials and the account login.** Is every credential, the
   Claude account login included, carried verbatim? Or is re-auth on sting
   acceptable?
   - Carry: KEEP-02, KEEP-06, KEEP-07, KEEP-12, KEEP-21, KEEP-23, KEEP-25.
   - Re-auth: CONF-04 (free text, after rejecting the carry option),
     CONF-17 (Q180, reversed by Q185), RES-15 ("we can defer the reauth").
   - Agent origin of "not carried": L6 (the 07-16 R11 design).
8. **C8. Original sessions or per-repo forks.** Does R0 seat each
   conversation under its original ID with byte-identical transcripts? Or does
   it make per-repo forks with rewritten `sessionId` and `cwd`, as the 10-01
   repair did?
   - Byte-identical: RES-12.
   - Per-repo forks: RES-19, RES-20.
9. **C9. Storage transforms.** May carried session files be compressed in
   place or moved behind symlinks? If so, is "byte-identical" judged on the
   decompressed stream, and does a symlinked store count as present?
   - Byte-identical and resumable: RES-12, RES-07 (principle).
   - Transforms: CONF-12, KEEP-35, KEEP-34.
   - Lossless only: KEEP-31, KEEP-32.
10. **C10. Union or mirror.** Is every run a lossless union into DEST,
    possibly from more than one SOURCE host, and never a mirror? In
    particular, may a file that disappeared on SOURCE ever be deleted on
    DEST?
    - One way: LIVE-11.
    - Union and more than one source: LIVE-15, LIVE-18, CONF-05, KEEP-36,
      CONF-16.
11. **C11. Replacing a DEST checkout.** May a run replace DEST's checkout at
    the canonical path with SOURCE's, after setting DEST's copy aside? Or was
    the 09-30 choice a one-off?
    - Replace with backup: CONF-11.
    - Never reset DEST: LIVE-15, LIVE-18, LIVE-30, LIVE-31, LIVE-32, LIVE-33.
12. **C12. Third-party app state.** Are third-party app databases under
    `~/Library` (Chrome, Login Data included; the Linear app) carried? Or
    are they dropped like the built-in-app databases?
    - Dropped as built-in: KEEP-16 (conditional), KEEP-29.
    - Carried: KEEP-12, KEEP-13.
    - The widening was agent text (L6).
13. **C13. Remote-mount content.** Does a full run carry content referenced
    on GDrive or NFS mounts once they are reachable? Or is GDrive itself that
    content's carrier?
    - Local disks only: KEEP-28.
    - Carry everything: KEEP-12, KEEP-13.
14. **C14. Exclusions selected before Q185 and Q192.** Are these now
    carried, as files if need be: the trees Q179 left out
    (Desktop/Documents/Downloads, old run state, `neo-relocated/state`, the
    tcfs pilot), the 35 corrupt SQLite rescue copies, and the two corrupt
    `logs_2` copies?
    - Left out: CONF-18, CONF-19, CONF-20.
    - Carried: KEEP-08, KEEP-13, KEEP-14, KEEP-15, KEEP-18.
15. **C15. Recovery trees (deadline 2026-11-02).** Are the neo Codex recovery
    trees, the only copy, still deleted on 2026-11-02 under R-C122?
    - Delete: CONF-13, CONF-15.
    - Keep: KEEP-08, KEEP-13, KEEP-15, KEEP-31, KEEP-32.
    - The operator did not recognise the tree on 10-05: CONF-14.
16. **C16. Transcript retention on SOURCE.** Does Claude Code's 30-day
    cleanup stay on, accepting that old transcripts are lost on SOURCE?
    - Leave it on: CONF-16.
    - Keep everything: KEEP-13, KEEP-14.
    - Either way, bulkload never deletes on DEST what vanished on SOURCE
      (C10).
17. **C17. Ending work on SOURCE for a moved family.** May bulkload ask the
    operator whether to stop work in a family on neo once it has moved, as
    on 10-08 and 10-15? Or is offering that question itself barred?
    - Asked and selected: CONF-09, CONF-10.
    - Never ask a lane to stop: LIVE-02, LIVE-16, LIVE-26, LIVE-36.
18. **C18. Quiet windows (confirmation only).** Confirm that the quiet-window
    selections of 08-23, 08-25, 10-02 and 10-03 are superseded, and that no
    run or benchmark schedules one.
    - Quiet windows: CONF-01, CONF-06, CONF-07, CONF-08.
    - Superseded by: LIVE-07, LIVE-10, LIVE-16, LIVE-23, LIVE-26, LIVE-40,
      LIVE-41, LIVE-42, LIVE-43.
    - Operator-scheduled host maintenance is separate (LIVE-19, LIVE-38).
19. **C19. The session driving the migration.** Does R0 apply to the session
    that drives the migration, or may it stay on neo for now, still carried?
    - May stay: KEEP-38, RES-16.
    - Every lane: RES-12, KEEP-13.
20. **C20. A finish line for bulkload.** Does bulkload get a finish line or a
    sunset in favour of tcfs?
    - Interim: PUR-01, PUR-02, PUR-06, PUR-09, PUR-15, PUR-20.
    - No sunset; publish: PUR-07, PUR-10 (declined "Interim + finish line").

**Recorded, no ruling needed.** The repo boundary (PUR-19) supersedes R34
and the 2026-09-12 selection "Rust in tummycrypt — finish M2". KEEP-03 is
about neo's own storage, not the sting carry. RES-07 is a principle, not a
migration ruling. SEAT-09 is agent-worded.

## Rulings on the round-2 conflicts (OI-1003-Q194, 2026-10-16)

On 2026-10-16 the operator ruled on all twenty round-2 conflicts in one
interview in the coordinator session on sting. One answer set gave four
selections: "Accept all (Recommended)" for C1–C20, and one answer each for
C15, C4 and C14. The ruling text quoted in each entry is the coordinator's
record of the accepted reading, Linear TIN-4543 comment 14484c4a (posted
17:19:47Z); it is agent wording of an operator selection. These rulings are
standing and are never re-asked. Each entry names the earlier entries it
follows, narrows or supersedes. The superseded entries stay as written.

- **RUL-01** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C1. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "The Mac-only cohort is **carried now**; it is still tcfs's first user later (narrows Q156 to placement)". Carried: KEEP-12, KEEP-13, KEEP-14. Narrowed: CONF-02 (Q156) now governs only tcfs's later role. Fact: CONF-03.

- **RUL-02** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C2. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "S1's headline is **cross-host gate (b) on an estate-shaped many-file corpus**; the local rig gate (a) is a regression guard (reopens Q97)". For: PUR-13, ONCE-04, FAST-22, FAST-07. Reopened: FAST-21, FAST-12 (Q97), which rested on FAST-10.

- **RUL-03** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C3. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): ""1 % delta" means **a real in-place change to about 1 % of files**". Replaces the reading in FAST-14 ("Corpus Bytes") and the agent's 1 % XOR draft in `slo.md` (2026-10-06). Answers the open question in Linear TIN-4543 comment eacc6552.

- **RUL-04** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  > Keep reuse records (Recommended)

  Conflict C4. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Second question, same answer set (C4 scratch). Rejected: "Drop all (Q193 as is)". Ruling (Linear 14484c4a): "**Keep the per-item reuse records on neo and drop only payload.** This replaces Q193's whole private-state drop. The coordinator disarmed the Q193 step at 2026-10-16 ~17:30Z, and b01's private state is kept". For: ONCE-01, ONCE-02, ONCE-11. Narrowed: ONCE-16 (Q186), ONCE-17 (Q193).

- **RUL-05** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C5. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "The 10-03 selection stands: 0 content bytes re-read, a metadata-only re-walk is allowed, and a rerun takes at most 10 %". Stands: ONCE-11, ONCE-12, ONCE-13, ONCE-14, ONCE-15. Read through them: ONCE-01, ONCE-10, SEAT-10. Closes the question tracked as bulkload#169.

- **RUL-06** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C6. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "The full run does **not** wait for the proofs". For: SEAT-02, SEAT-05, SEAT-07, SEAT-21, SEAT-22, SEAT-32, RES-06, RES-16, SEAT-31, SEAT-33. Superseded on order: CONF-21 (R-N56), OI-1002-Q24 as relaxed by Q142. Unchanged: SEAT-10 (both objectives are the completion bar).

- **RUL-07** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C7. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "The Claude account login is **carried**; re-auth only if a token is host-bound". For: KEEP-02, KEEP-06, KEEP-07, KEEP-12, KEEP-21, KEEP-23, KEEP-25. Superseded: the R-N47 reading of CONF-04; CONF-17 (already reversed by Q185). RES-15 (re-auth is deferrable operator work) applies only to a host-bound token.

- **RUL-08** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C8. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "Original session IDs are kept **byte for byte**; no per-repo forks". For: RES-12. Not adopted for bulkload: RES-20 (the 10-01 forks), RES-19 by forking.

- **RUL-09** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C9. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "Compressed or symlinked session stores are **not** identical; agent session files are never recompressed or symlinked in place". For: RES-12, RES-07 (principle), KEEP-31, KEEP-32. Narrowed: CONF-12, KEEP-34, KEEP-35 as they apply to session files.

- **RUL-10** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C10. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "Every run is a **lossless union**: never a mirror, never clobbering DEST". For: LIVE-15, LIVE-18, CONF-05, KEEP-36. LIVE-11 ("one of way to sting") names the move's direction, not a mirror.

- **RUL-11** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C11. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "A DEST checkout is replaced only when DEST's copy is preserved intact beside it and the swap is recorded". Bounds: CONF-11. Kept: LIVE-15, LIVE-18, LIVE-30, LIVE-31, LIVE-32, LIVE-33.

- **RUL-12** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C12. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "Third-party `~/Library` app state (Chrome, Linear) is **carried**". For: KEEP-12, KEEP-13. Bounded: KEEP-16 (built-in apps only). Superseded: KEEP-29 and the 09-30 widening (L6).

- **RUL-13** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C13. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "GDrive/NFS content is **carried once reachable**". For: KEEP-12, KEEP-13. Superseded: KEEP-28 (a time-boxed 09-30 scope).

- **RUL-14** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  > Carry all as files (Recommended)

  Conflict C14. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Second question, same answer set (C14 corrupt). Rejected: "Keep D13b hold". Ruling (Linear 14484c4a): "**Carry as files** the trees Q179 left out that are not recreatable, the 35 corrupt SQLite rescue copies and the two corrupt `logs_2` copies, each marked corrupt in closure. This replaces the D13b hold (Q187) and the non-recreatable Q179 excludes. Recreatable content stays out (Q192)". For: KEEP-08, KEEP-13, KEEP-14, KEEP-15. Superseded: CONF-18 (non-recreatable trees only), CONF-19, CONF-20.

- **RUL-15** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  > Cancel until carried (Recommended)

  Conflict C15. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Second question, same answer set (C15 deadline). Rejected: "Keep the 11-02 date". Ruling (Linear 14484c4a): "**The deletion of the neo Codex recovery trees on 2026-11-02 (R-C122) is cancelled** until they are carried to sting and verified by hash; after that it goes back to the operator". For: KEEP-08, KEEP-13, KEEP-15, KEEP-31, KEEP-32. Suspended: CONF-13 (R-C122), CONF-15 (R-C357). Context: CONF-14.

- **RUL-16** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C16. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "The 30-day transcript cleanup stays off. neo `cleanupPeriodDays` is 3650, checked 2026-10-16". For: KEEP-13, KEEP-14. Superseded: CONF-16.

- **RUL-17** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C17. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "bulkload may **ask** the operator to end work on neo for a family that has moved, but never requires it". Asked and selected: CONF-09, CONF-10. Kept: LIVE-02, LIVE-16, LIVE-26, LIVE-36 (no lane is made to stop).

- **RUL-18** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C18. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "The old quiet-window selections are **superseded** by never-halt". Superseded: CONF-01, CONF-06, CONF-07, CONF-08. By: LIVE-07, LIVE-10, LIVE-16, LIVE-23, LIVE-26, LIVE-40, LIVE-41, LIVE-42, LIVE-43. Separate: LIVE-19, LIVE-38.

- **RUL-19** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C19. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "R0 covers the session driving the migration: it is carried and resumable too". For: RES-12, KEEP-13. Read narrowly: KEEP-38, RES-16 (it need not move first).

- **RUL-20** · ruling-record · 2026-10-16T17:18:48Z · sting · Claude session `2cc731d7` (coordinator session), AskUserQuestion (OI-1003-Q194)

  > Accept all (Recommended)

  Conflict C20. Question (asked 17:17:49Z): "Accept my recommended readings for C1–C20 in the table above as standing rulings (recorded in SPEC and never re-asked)? Choose Other to name exceptions." Rejected: "Accept all except C4/C14/C15". Ruling (Linear 14484c4a): "**bulkload is done** when the full run meets R0 and R3 is proven; after that it is maintenance-only once tcfs launches". For: PUR-01, PUR-02, PUR-06, PUR-08, PUR-09, PUR-15, PUR-20, SEAT-10. Publishable work: PUR-07, PUR-10 ("a bit of both").

> Redaction note (2026-10-16, R-N13): a private repository name was replaced with `[repo]` in existing entries after publication. This is the one exception to append-only: public-repo hygiene outranks it. Quotes are otherwise unchanged.
