# 2026-09-23: PR #55 git-carry-estimate review fixes (R-N74, R-N75, TIN-4545)

- **Scope:** fix the adversarial-review findings F1–F13 on the read-only
  `git-carry-estimate` verb. Each fix has a test that failed on the PR head
  and passes after it. The PR was not merged.
- **Rulings applied:**
  - R-N74: `missing_thin_pack_bytes` is the gate metric.
  - R-N75: refuse a partial or differently-shallow destination.
  - R-N69: local build exception.
  - R-N11, R-N12, R-N80: no process was signalled, and no guard hook refused
    anything.
- **Found during the re-run:** Git's default `pack.useSparse=true` packed one
  object the haves already reach (bulkload: 659 packed vs 658 walked). The
  estimate now pins `pack.useSparse=false` and refuses on any count mismatch.
  The M1 sender must build its pack the same way.
- **Shared target dir hazard:** another M2 lane's worktree builds into the
  same `CARGO_TARGET_DIR`, and workspace members hash identically across
  worktrees. Cargo then reported that lane's newer artifacts as fresh and
  ran its test binary. Touch this worktree's sources before each gate, and
  check the test list or the binary's strings.
- **Evidence:** `docs/evidence/git-carry-estimate-2026-09-23.md`, re-measured
  at `f65313b`. Nothing was written on sting or in any cohort repository.

## r2 (same day): re-review BLOCK at `5313c32`

- **Rulings applied:**
  - R-N74 and R-N75, fail closed.
  - R-N97: the gate is sent bytes ≤ 1.1× `missing_thin_pack_bytes` and sent
    objects ≤ `missing_objects`. The estimate is an upper bound, and exactness
    is checked against `git fetch`.
  - R-N80: no guard refusals occurred.
- **Findings fixed:** N1–N6 and N9, with the reviewer's artifacts ported as
  regression tests (see PR #55). N7 and N8 are deferred to a follow-up PR
  comment; the "lower bound" wording is corrected to "upper bound" in the
  module doc.
- **Found during the fix (N3):** upload-pack drops a have that is a parent of
  an earlier-processed have. For a shallow client (`--shallow`,
  edge-aggressive over the command-line haves), that changes the pack. The
  estimate drops every have that is a parent of another have, which keeps it an
  upper bound whatever order the client offers them in. It equals a real fetch
  whenever commit dates order the haves child-first.
- **Build:** own `CARGO_TARGET_DIR`
  (`cargo-target-bulkload-m2-w6-estimate`), so there is no shared-target
  staleness.

## r3 (same day): BLOCK at `7680da6` on the upper-bound claim

- **R-N113:** M1's first round sends exactly the held tips as haves. The
  exactness oracle is upload-pack over exactly that set; the tests use a Rust
  port of the reviewer's `oracle_upload_pack.py`.
- **Correction to r2's claim:** "ancestor probing can only shrink the pack" was
  false for shallow destinations. An extra have can enlarge the pack (fixture
  A3: 232 B → 3,919 B). The module doc and the evidence doc are corrected.
- **Fixtures A3 and B** are ported as regression tests. Per the coordinator,
  the parent-drop rule stays; R3-3 (`merge-base --independent`) was dropped.
- **R3-2 redaction:** all 11 leaks are fixed, and a PEM rule is added. The
  U+0085 path refusal code is deferred as cosmetic.
- **R3-4:** M1 must pin `pack.threads` and `pack.windowMemory`.
- **Spike #64:** a bitmapped sender packs fewer objects than the walk.

## r4 (same day): BLOCK at `6f3d9dc`

- **R-N116:** M1 sends every held tip, ancestors first. The estimate dropped
  the parent-drop rule and equals upload-pack over that list exactly. It was
  checked on fixtures P1 (four seeds), M, F, A3 and B, and on the reviewer's
  `n4_oracle.sh`, where every fixture except the deferred S1 matches.
- **blahaj:** re-run alone at 22:03Z, read-only: 30 objects, 14,644 B. A
  scan found no writes on neo or sting.
- **R4-3 and R4-4 redaction:** fixed and covered by regression cases.
- **Deferred:**
  - R4-2: a shallow source with a non-shallow destination over-estimates.
  - base64 with no key word.
  - the U+0085 path refusal code.

## r5 (2026-09-24): BLOCK at `bb6efa7` on two redaction regressions and one doc paragraph

- **R5-A:** a whole token prefix followed by a separator (`ghp_`, `AKIA`,
  `github_pat_`, `glpat-`, `xoxb-`) now redacts the next word too.
- **R5-B:** a short key whose separator is its own word (`pass : x`,
  `pwd = x`, `{"pwd" : "x"}`) redacts the separator and the value. `auth` and
  `key` also accept `:`.
- **R5-C:** the docs say a have is dropped only out of ancestors-first order
  (fixture A3X: 232 B ancestors first).
- **R5-F:** the 22:03Z blahaj raw output was lost in the neo crash, before it
  was persisted. blahaj was re-run alone, read-only, at 01:34Z. The raw
  output is now in the evidence doc: 31 objects / 15,584 B, no writes on
  either side.
- **Deferred:**
  - R5-D: `exit code=128` is over-redacted.
  - R5-E: `jess:1234/5678@host`.
  - the three-word NBSP split.
  - `pass`+NEL.
  - R4-2.

## r6 (2026-09-24): BLOCK at `2eabcd5`; R-N121 "classify, don't echo"

- **Heuristic redaction is deleted:** `detail()`, `redact*`,
  `prefix_reaches_boundary`, `spaced_short_key` and their tests.
- **What a refusal prints now:** `refused=`, `refused_reason=` (own
  vocabulary), `stderr_class=` (closed set: `not_a_repository`, `auth_failed`,
  `host_unreachable`, `timeout`, `bad_object`, `other`) and `stderr_blake3=`.
  No field of `Refused` holds printable stderr, and its `Debug` omits the raw
  bytes.
- **Raw bytes:** `--state-dir DIR` writes them to `DIR/stderr/<blake3>.log`,
  created with `O_CREAT|O_EXCL` at mode 0600 (directory 0700), and prints
  `stderr_file=`. A planted file or symlink there is refused. Without
  `--state-dir`, nothing is written.
- **Tests:**
  - classification on real git and OpenSSH messages;
  - the private file's mode, contents, reuse and symlink refusal;
  - the full r3–r6 corpus in-process (`tests/stderr_corpus`, about 2,600
    probes, each with a canary);
  - an end-to-end sample (every e2e and keep probe, plus 1 in 80 of the
    rest) through the binary with a fake `git` (local) and a fake `ssh`
    (remote). Output keys, class, digest, file mode and file bytes are
    checked, and no canary or secret appears in stdout or stderr.
- **Why a sample end to end:** neo spawned processes about 10× slower than
  normal after the crash (about 1 s per verb run), so the full corpus goes
  through the CLI only as a sample; the in-process test covers all of it.
- **R6-C:** one sentence in both docs.
- **Cohort raw outputs (item 5):** all 26 repositories re-run read-only from
  neo at 03:45:28Z–03:48:15Z with the `332c7be` release binary. The raw
  output is committed as
  `docs/evidence/git-carry-estimate-cohort1-raw-2026-09-24.txt`, and the
  evidence table was regenerated from it: 5,415 objects, 9,758,779 B.
- **Write scan:** nothing on sting. On neo, only glorious.build's files from
  another session's commit at 03:45:30Z (reflog `commit:`), about 90 s
  before its pair ran.
