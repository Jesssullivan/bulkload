# 2026-10-02 ingest-journal lane (#82, #92, #94)

Rulings: OI-1002-Q24 (bulkload completes before the single migration run),
OI-1002-Q25 (gate plan / pre-migration fixes), OI-1002-Q29 (operator
"ultracode, full steam ahead", 2026-10-02), R25 / R-N58 strict per
OI-1001-Q15, R33, R-N13.

Branch `feat/ingest-journal-20261002` from origin/main `84242b8`. Area:
`crates/bulkload-agent/src/git_carry/carry_v2/{journal,ingest}.rs` (W6 M1
ingest, #75).

## Done

- **#82** The quarantine key is BLAKE3 over a 32-byte random identity token,
  `<state>/git-carry-v2/quarantine-key` (0600, private-file checks), not the
  state dir's canonical path. Created and read only under an exclusive
  `flock`, and sealed (file and `git-carry-v2/`) before any key is returned,
  so no quarantine is named by a token a power loss could take back. A token
  file shorter than 32 bytes (creator died before its write) was never handed
  out and is rewritten; a longer one refuses `PATH_ESCAPES_ROOT`. Device and
  inode were rejected (Darwin `st_dev` is not stable across reboots, per the
  issue). A renamed or remounted state dir resumes its own quarantine.
  (#120 later binds the token to inode numbers, not `st_dev`.)
- **#92** `finish` and `abandon` call `still_owned()` at the top;
  `finish` again before migration, publication and dropping keeps;
  `abandon_journaled` before and after its `abandoned` append. A replaced
  journal name refuses `JOURNAL_OWNERSHIP_CONFLICT` / `journal_replaced`.
- **#94** `Refused` gains `cleanup: Option<BulkloadRefusal>` (printed as
  `cleanup_refused=`). The fresh-open paths in `Ingest::start`
  (`quarantine_held`, resume-with-nothing-sealed, `occupied`/`preflight`)
  return the original refusal and attach a failed journal removal as the
  cleanup cause instead of replacing it with `IO`.

Tests: `journal::tests::the_quarantine_key_follows_the_state_dirs_token_not_its_path`,
`issue82_a_renamed_state_dir_resumes_its_own_quarantine`,
`issue92_finish_and_abandon_refuse_a_replaced_journal`,
`issue94_a_failed_cleanup_never_masks_the_quarantine_held_refusal` (skips
itself when directory modes are not enforced, i.e. a privileged runner), and
`pr75_r3_m3_*` rewritten for the new semantics.

## Still open

- A state dir lost and recreated at the same path now gets a new key, so the
  lost one's quarantine is an orphan that no open reclaims (r3 M3 used to
  reclaim it because the path key matched). No open can prove such an orphan
  unowned, since a moved state dir keeps its key. Reclaiming orphans needs an
  explicit operator sweep (unlocked `incoming-bulkload-*` with no journal
  anywhere); not built here.
- (Corrected 2026-10-03, #120.) A state dir copied byte-for-byte (token
  included) shared its quarantine names with the original. The quarantine
  `flock` kept only two *live* sessions apart: a sequential fresh open from
  the copy discarded the original's unlocked quarantine, and the original's
  resume then re-sent every segment (R25). Fixed by #120; see
  `2026-10-03-ingest-token.md`.
- No legacy path-keyed fallback: estate operations are frozen (R-N56), so no
  deployed state dir holds a path-keyed quarantine.
