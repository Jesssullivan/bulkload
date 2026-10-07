# 2026-10-07: whitepaper refresh after the carry_v2 deletion (lane whitepaper-refresh)

**Lane:** `whitepaper-refresh`, workflow subagent on sting. Worktree
`bulkload.worktrees/whitepaper-20261003`, branch `docs/whitepaper-20261003`,
PR #164 (opened 2026-10-04, held for this refresh). This session was the
worktree's only writer. Docs only: it changed `docs/whitepaper/bulkload.md`,
`docs/whitepaper/bibliography.md` and this note, and nothing else.

**Rulings:** OI-1003-Q51 (refresh the whitepaper after the carry_v2
deletion), OI-1003-Q7 (proof package: whitepaper and bibliography),
OI-1003-Q32 (the hybrid formal model the paper describes), OI-1003-Q97 (with
Q96: the hermetic rig is the S1 gate of record; the paper describes it as
ruled and in progress), R-N13 (receipts and this note). Followed without
incident: R-N11, R-N12, R-N92, R-N98, R-N101, R-N104. No guard hook refused
anything. No process was signalled and no PID was checked.

## What was done

1. **Merged `origin/main`** (`a80c63b`, the merge of #189) into the branch
   with a signed merge commit, `316d304`. No conflict: the branch only adds
   `docs/whitepaper/` and its own notes. The branch was 220 commits behind.
2. **Rewrote `docs/whitepaper/bulkload.md`** against `main` at `a80c63b`.
   Sources read in full: `AGENTS.md`, `docs/slo.md` with every amendment,
   `docs/design.md`, `docs/formal/README.md`, the property-test plan, the
   S3 estate, blahaj and Q42 probe evidence, and the agent notes of
   2026-10-04 to 2026-10-06 that the paper cites. The paper now:
   - labels every result *gated*, *informational* or *pending*, and defines
     the three labels at the top;
   - cites a file on `main` for every statement about a result;
   - still quotes no SLO number;
   - ends with "What changed since 2026-10-04" (section 8).
3. **Re-verified the bibliography entry by entry** (log below), added three
   entries and one page range, and dropped none.
4. **Validation.** The docs-only gate set replaced `check-fast` (see
   "Validation").

## Facts checked against `main` at `a80c63b`, and how

| Claim in the paper | Checked by |
|---|---|
| `carry_v2` is gone from the product code | `grep -rl carry_v2 crates`: only tests' comments and two bench scripts name it; `src/git_carry/` has no `carry_v2`; the agent's features are `fault-injection` and `io-trace` |
| Grouped chaining (P68) is not on `main` | no `p68` test in `crates/`; PR #199 is OPEN |
| P76 registry guard is not on `main` | PR #197 is OPEN; issue #188 is open |
| 25 proptest sites in 16 files | `grep -rn 'proptest!\|TestRunner::new' crates`, the guard file excluded |
| 12 of 64 planned properties on `main` | the plan's catalogue (P1–P63, P38 folded, plus P66, P67, P72–P75, minus P48, P49, P50, P59), against `fn p<N>_` tests and the plan's own status lines |
| Three S3 properties ignored against #186 and #187 | `#[ignore = ...]` lines in `tests/s3_transfer_resume.rs` (a fourth ignored test has no issue) |
| 57 bare `IO` sites in 15 files, 28 in the SQLite provider | sum of `IO_NONE_ALLOWLIST` in `tests/refusal_taxonomy.rs` |
| Both stores still `synchronous=FULL` (WP0(g) not implemented) | `src/io/durable.rs` |
| Buffer pool and fused chunker still unwired | the `allow(dead_code)` reasons in `src/io/mod.rs` |
| CI: two gates, 25-minute cap | `.github/workflows/ci.yml`; `eef8909` is in #190's merge |
| 54 of 54 and 23 of 23 model rows; explorer parity | `docs/formal/README.md`, "Results", "The explorer", "Results (GitCarry)" |
| No `docs/slo.md` text for OI-1003-Q96 or Q97 | `grep` over the tree: no hit |
| No evidence file for the 2026-10-04 under-load sample | `ls docs/evidence/`; the numbers are in `2026-10-05-coordinator.md` only |
| P75's unprivileged leg is skipped in CI | the log of CI run 37582012173 (`main` at `95f43dc`): two `p75 root: ... unprivileged leg SKIPPED (dropping to uid 65534: Permission denied (os error 13)); this run proved the refusal only` lines |
| Issues #157 and #162 are still open; #161 is closed | `gh issue list`, `gh issue view 161` |

## Bibliography verification log (2026-10-07)

Tools: the `paper-search` MCP server's `get_crossref_paper_by_doi` for every
DOI, and an HTTP fetch of every URL, each searched for the phrase the entry
relies on. Every fetch returned HTTP 200. 37 entries: 34 kept, 3 added, 0
dropped.

**By Crossref DOI** (title, authors, venue, volume, issue and pages
compared with the entry; all matched):

| Key | DOI | Crossref returned |
|---|---|---|
| CrashMonkey19 | 10.1145/3320275 | ACM Trans. Storage 15(2), pp. 1–34, 2019; five authors |
| FastCDC20 | 10.1109/TPDS.2020.2984632 | IEEE TPDS 31(9), pp. 2017–2031, 2020; nine authors |
| FileSync98 | 10.1145/288235.288261 | MobiCom '98 proceedings, pp. 98–108 |
| LBFS01 | 10.1145/502034.502052 | SOSP '01 proceedings, pp. 174–187 |
| OptFS13 | 10.1145/2517349.2522726 | SOSP '13 proceedings, pp. 228–243 |
| QuickCheck00 | 10.1145/351240.351266 | ICFP '00 proceedings, pp. 268–279 |
| TLA94 | 10.1145/177492.177726 | ACM TOPLAS 16(3), pp. 872–923, 1994 |
| TLC99 | 10.1007/3-540-48153-2_6 | LNCS chapter, pp. 54–66, ISBN 9783540665595; volume 1703 from Lamport's publications page (fetched) |
| NVersion85 (added) | 10.1109/TSE.1985.231893 | IEEE TSE SE-11(12), pp. 1491–1501, 1985; A. Avizienis |
| KnightLeveson86 (added) | 10.1109/TSE.1986.6312924 | IEEE TSE SE-12(1), pp. 96–109, 1986 |

**By fetch** (what was found in the fetched page):

| Key | Found |
|---|---|
| ALICE14 | title; `433--448`; ISBN 978-1-931971-16-4 |
| B3-18 | title; `pages = {33--50}`; ISBN 978-1-939133-08-3 |
| FastCDC16 | title; ISBN 978-1-931971-30-0; `pages = {101--114}` (now in the entry) |
| Rsync96 | ANU handle 1885/40765: title, both authors, 1996, TR-CS-96-05; the rsync.samba.org HTML copy |
| Specifying02 | Lamport's book page: title, Addison-Wesley, 2002, ISBN 0321143068 |
| Unison04 | the Penn repository REST record (title, both authors, 2004-02-24, the handle); Pierce's bibliography page (MS-CIS-03-36) |
| AppleDiskWrites | the JSON endpoint: title, `F_BARRIERFSYNC`, the best-effort sentence |
| AppleFcntl | the XNU man source: `.Dd August 12, 2021`, `F_BARRIERFSYNC`, "Apple SSDs are guaranteed to provide" |
| BLAKE3 | the specification PDF, text extracted: title, four authors, `Version 20211102173700`; the repository README (`derive_key`) |
| GitBundle, GitPackObjects, GitPackProto, RacyGit | each page's NAME line or key phrase on git-scm.com |
| IoprioSet | man7.org: the NAME line, `IOPRIO_CLASS_IDLE` |
| SQLiteBackup, SQLiteWAL | sqlite.org: `sqlite3_backup_step`; "Write-Ahead Logging" |
| ZfsSend | openzfs docs: `receive_resume_token` |
| Borg | readthedocs (Borg 1.4.5): files cache; `ctime,size,inode`; `cache.py` on `1.4-maint`: the newest-cmtime guard at lines 629–642 |
| Bup | `DESIGN.md`: the title line |
| Casync17 | the 2017 post; the repository README (`SHA512/256`, `SHA256`); repository not archived |
| Desync | README; GitHub API: "Alternative casync implementation", BSD-3-Clause |
| Proptest | GitHub API: "Hypothesis-like property testing for Rust" (the raw README path is a symlink, so the API record was used) |
| Rclone | `rclone copy`, the local backend page (`local-no-clone`), the FAQ (binary diff) |
| Restic | readthedocs (restic 0.19.1): Rabin, 512 KiB, 8 MiB |
| RsyncMan | the `rsync.1` page: "quick check" |
| UnisonRepo | README; GitHub API: "Unison file synchronizer", GPL-3.0 |
| Dhall (added) | dhall-lang.org; the `dhall-lang/dhall-lang` README and API record (BSD-3-Clause) |

Two wording points the pass settled:

- The draft sentence that called Dhall "total" was removed: the fetched
  pages did not state it in those words. The paper now cites the formal
  README for what the catalogue's `merge` guarantees.
- [NVersion85] and [KnightLeveson86]: in the first pass only the Crossref
  metadata was checked, and the paper's sentences about them came from
  memory. The review pass (below) read both abstracts and held the paper to
  them.

## Review pass (2026-10-07, same lane, same rulings)

A review of `8b13fa8` raised one high and two medium findings. All three
were judged valid and fixed in one signed commit on top of `1a3037b`.
`origin/main` was still `a80c63b`, so there was nothing to merge.

1. **S1 statements were only true of `main` (high).** Branch
   `origin/feat/s1-hermetic-rig-20261007` (head `83d08dc`, no PR) holds, at
   commit `640093d`, `docs/evidence/r23-2026-10-07-0652Z-no-a-control.md`:
   a gated-mode gate (a) sample on mbp-13, B/B/B with no A control, printed
   verdict "FAIL (NO A CONTROL)", 0 of 3. The paper now:
   - scopes every "no gated sample" sentence to `main` (abstract, 4.6,
     5.1, 5.2, 5.3, 7, 8);
   - reports the draft as *pending, off `main`* in 4.4 and in a new
     paragraph of 5.3, with the numbers as that file prints them, and says
     its standing is the operator's decision;
   - says beside the under-load numbers that the only gated-mode sample
     points the other way;
   - drops "No quiet gated window has been obtained";
   - names this as the one exception to "every result cites a file on
     `main`".
   Read with `git show origin/feat/s1-hermetic-rig-20261007:<path>`; the
   branch was not checked out and nothing was written to it.
2. **S3's rerun bound (medium).** `docs/slo.md` words the unchanged-estate
   bound on wall-clock (S3 row; WP0(c)). The S3 evidence applies
   OI-1003-Q35 (CPU ratio admissible, wall informational), and
   `grep Q35 docs/slo.md` finds nothing. Section 5.2 now gives both ratios
   at both scales against both readings, 4.6 notes the disagreement, and
   section 7's documentation-drift list carries the item.
3. **[NVersion85] and [KnightLeveson86] (medium).** Both abstracts were
   read on 2026-10-07 from the OpenAlex records for their DOIs
   (`paper-search` `search_openalex` for W1971991620; `api.openalex.org`
   for W2129360963), and both DOIs were resolved again through Crossref
   (`get_crossref_paper_by_doi`; SE-12(1) pp. 96-109 and SE-11(12)
   pp. 1491-1501). The full texts were not read. Changes:
   - the definition of N-version programming and the result (27 versions,
     coincident failures "substantially more than expected") are now cited
     to the Knight and Leveson abstract, and the bibliography lists the
     statements relied on;
   - [NVersion85] is cited as a review of the approach, which is what its
     abstract says it is. "Origin of the term" is gone. The earlier Chen
     and Avizienis paper was not added: Crossref lists only a 1995 reprint
     (10.1109/ftcsh.1995.532621) and its text was not read.

No other bibliography entry was touched in this pass, so the 2026-10-07 log
above stands for the rest.

**Low findings, not fixed by instruction** (listed in the lane result):
the [m0] citation for XFS in section 6; "three parts" naming two in 4.2;
the bibliography header's "first checked on 2026-10-03" for the three new
entries; the [Dhall] entry's README path; the Borg line range and
Specifying02 ISBN form differing between this note and the bibliography;
"HTTP 200 in every case" against the [Unison04] handle. The rig-branch
pointer that one low finding asked for was added as part of fix 1.

## Recheck and ship (2026-10-07, same lane, same rulings)

An adversarial recheck of `d7c74e7` against the three medium/high findings.
Verdict: clean. No paper or bibliography text changed in this pass.

- **S1 (high): fixed.** The off-`main` evidence file was read again with
  `git show 640093d:docs/evidence/r23-2026-10-07-0652Z-no-a-control.md`.
  Every figure section 5.3 quotes matches it: host and filesystem, corpus
  size, rclone version, B `3e7b5bf`, the three load1 values, the three
  native and rclone medians, the delta, resume and memory columns, the
  status string and the printed verdict. The branch head is still
  `83d08dc`, `gh pr list --head` returns no PR, and the three operator
  choices match the branch's draft `docs/slo.md` amendment. No unscoped
  "no gated sample" sentence remains (grep over the paper).
- **S3 bound (medium): fixed.** `docs/slo.md` lines 41 and 110 say
  wall-clock and the file has no Q35; the four ratio ranges in the new
  table match the evidence file's verdict table.
- **References (medium): fixed.** Both DOIs resolved again through Crossref
  (`get_crossref_paper_by_doi`), and both OpenAlex abstracts were fetched
  again from `api.openalex.org` (W1971991620, W2129360963). Each statement
  the bibliography lists is in the abstract it names.
- **No new defect found.** Relative links in both whitepaper files resolve;
  every citation key in the paper has a bibliography entry and the reverse.
  The same five gates exited 0 on the tree of `d7c74e7` and on this
  commit's tree (24 contract tests, OK).
- **Shipped on PR #164**, which already existed: title and body updated
  with `gh pr edit`, no second PR, not merged.
- The low findings listed above stay open, as before.

## Validation

Docs-only diff, so `check-fast` was replaced, as dispatched, by
`nix develop .#default --command just repo-manifest-validate python-lint
shell-lint workflow-lint contract-test`, with
`CARGO_TARGET_DIR=/srv/cache/jess/cargo-target/whitepaper-refresh` and
`TMPDIR=/dev/shm/whitepaper-tmp`. It exited 0 on the tree of `8b13fa8`
(2026-10-07): `repo-manifest: PASS` twice, ruff clean, shellcheck and
actionlint silent, and `tests/test_ci_contract.py` ran 24 tests, OK. No Rust
was built or tested, because no Rust changed. Every relative link in both whitepaper files was
checked to resolve.

The same gate set was run again on the review-pass tree and exited 0 with
the same output (24 contract tests, OK); the relative links were checked
again.

## Shas

- `316d304`: merge of `origin/main` `a80c63b`.
- `8b13fa8`: the refreshed whitepaper, the bibliography and this note
  (signed).
- `1a3037b`: this note's shas and gate result.
- `d7c74e7`: the review-pass commit.
- The recheck commit (below) follows `d7c74e7`; the PR body carries the
  final head.
- The commit after `8b13fa8` records these shas in this note; the PR body
  carries the final head.

## Workstreams (restated per AGENTS.md; reported from `gh`, not verified)

| Stream | Branch / PR | State | Next |
|---|---|---|---|
| Whitepaper refresh (this lane) | `docs/whitepaper-20261003`, #164 | refreshed and pushed | operator review and merge |
| Q42 L6b grouped chaining, P68 | #199 | open | review |
| S2 proof closure, P76 | #197 | open | review; closes #188 |
| S1 hermetic rig | `feat/s1-hermetic-rig-20261007` (`83d08dc`), no PR, not on `main` | ruled (OI-1003-Q96, Q97), in progress; one draft gated-mode sample, no A control, B FAIL 0 of 3 | the operator's ruling on the A control; its `docs/slo.md` amendment |
| S2 gated budget run | #165 | never run | a quiet gated window |
| Estate operations | n/a | **held** (R-N56) | the M2 gates |

## Open

- **The draft rig sample needs an operator ruling**, and the paper's
  section 5.3 should be rewritten when it is ruled on, rerun or merged.
- **`docs/slo.md` has no OI-1003-Q35 text.** Its S3 bound is still worded
  on wall-clock. Outside this lane's scope; the paper flags it.
- **The paper will go stale again when the rig amendment lands.** Its S1
  text cites OI-1003-Q96 and Q97 from this lane's dispatch only. Once
  `docs/slo.md` carries them, section 4.4 should cite that text instead.
- **Main CI was red at `3931471`** (both gates, 2026-10-07) and the run for
  `a80c63b` had not finished when this was written. Issues #200 and #201
  record flaky tests. The paper says "gated" means enforced, not always
  green. Nobody owns the flakes yet, as far as this lane can see.
- **P75 in CI proves the refusal only** (the log above). Closing the gap
  needs a CI job that is not root, or a sandbox that permits the uid drop.
- **Documentation drift on `main`, outside this lane's scope:**
  `docs/design.md` still calls the salvage bounds "unruled engineering
  defaults" although `docs/slo.md` ratifies them (OI-1003-Q24);
  `docs/formal/README.md` still says "the state root is never sealed" in
  "Code and design disagreements" and "Not proven here", although #166
  sealed it.
- **`docs/slo.md`'s wording of R25's model obligation** is still the vacuous
  `R25_NoCommittedCaptureReread`; the formal README asks for a ruling.
- **Issues #157, #162 and #169 are open on GitHub** although their main
  fixes merged (#196, #172, #198). Closing them is the coordinator's call.
- **`docs/agent-notes/2026-10-03-whitepaper.md`** describes the paper as it
  was at `dfb9604`. It is a dated record, and this lane was scoped not to
  edit it. Under R-N55 the coordinator may want it deleted or marked
  superseded.
- **No Linear comment was posted.** The dispatch scoped this lane to the
  repository; the distilled facts are in this note and the PR body.
- **Not re-measured, and the paper says so:** S3 on the estate corpus after
  #172, #177 and #182; the under-load S1 sample with the fixed harness.
