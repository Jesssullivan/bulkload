# 2026-10-03 ingest-token lane (#120)

Rulings: OI-1002-Q24 (bulkload completes before the single migration run),
OI-1002-Q29 (ultracode, full steam), OI-1002-Q34 (wave 2), OI-1002-Q33
(#124: salvage only for byte-touching refusals, bounded, typed refusal at
the bound; R25 strict for anything the destination durably held), R25 /
R-N58 strict per OI-1001-Q15, R33, R-N56, R-N13.

Branch `feat/ingest-token-20261003` from origin/main `727493a` (includes
#116-#119). Area: `crates/bulkload-agent/src/git_carry/carry_v2/{journal,ingest}.rs`.

## Done

- **#120** `git-carry-v2/quarantine-key` now holds one 88-byte binding
  record: token, predecessor token (zeros for none), the state dir's inode
  number, the token file's inode number, and an 8-byte BLAKE3 check. A
  record bound to other inodes means the state dir is a copy: its first
  open mints a fresh token and records the copied one as its predecessor.
  The original keeps its token, so neither side's fresh open ever discards
  the other's quarantine.
- Identity scheme: inode numbers only, never `st_dev`. Both inodes survive
  a rename within a file system, a remount and a reboot on Linux and Darwin.
  `st_dev` does not: Darwin assigns it at mount, and on Linux it can follow
  device-mapper or probe order, so checking it would read a reboot as a
  copy. Any file-level copy (cp -a, rsync, tar, Finder, clonefile, a restore
  from a file backup, a move across file systems) makes new inodes and is
  caught. A block-level clone (disk image, file system snapshot) keeps
  inode numbers and is not caught. It also clones any destination repo on
  the same volume, so its quarantines live in another repository.
- R25 for a journal copied mid-session (or a state dir moved across file
  systems): when its own quarantine is absent and the predecessor's exists,
  resume and open refuse `JOURNAL_OWNERSHIP_CONFLICT` / `state_dir_copied`.
  Nothing is abandoned, adopted or re-sent. Once the original's quarantine
  is gone, the journal is abandoned as `quarantine_lost`, as before.
- Crash safety: the record is written whole with one `pwrite` at offset 0.
  The file is never truncated first and is opened without `O_APPEND`, so a
  crash leaves either the old record or the new one. Short files (under 32
  bytes, creator died) are re-minted. A 32-byte #82 token is bound in place
  and keeps its token. Any other length, or a failed check, refuses
  `PATH_ESCAPES_ROOT`.
- The 2026-10-02 ingest-journal note is corrected: it said the flock "still
  keeps two live sessions apart" and so understated the sequential case.

Tests: `journal::tests::issue120_a_copied_state_dir_mints_its_own_key`,
`the_quarantine_key_follows_the_state_dirs_token_not_its_path` (updated),
`git_carry_v2::issue120_a_copied_state_dir_never_discards_the_originals_quarantine`
(the issue's scenario: copy, original crashes after 2 segments, the copy
opens fresh, the original's quarantine survives and its resume re-sends
nothing). Against origin/main's code this test fails, and with the fix it
passes. Also `issue120_a_state_dir_copied_mid_session_refuses_rather_than_resends`.

## R-N56 estate evidence (path-keyed quarantines)

- Code path: no `bulkload-agent` verb reaches `git_carry::carry_v2::ingest`.
  `src/main.rs` calls only `export_repository_with_policy`, `restore_*`,
  `import_bundle` and `estimate`. No binary from any commit can have
  created a quarantine or a token, path-keyed or not, on any host.
- sting: there is no `bulkload-agent` on PATH. A `find` over `/home/jess`
  and `/srv/fast-local/jess` (skipping cargo targets, tmp, caches) for
  `incoming-bulkload-*`, `git-carry-v2` and `quarantine-key` found nothing
  (2026-10-03; find exited 1 on unreadable directories only, no matches
  printed).
- Other hosts were not scanned from this lane. The code-path evidence
  covers them.

## Still open

- A state dir moved across file systems with an in-flight journal is a
  copy. It refuses `state_dir_copied` until the quarantine it left behind is
  gone, and no operator verb exists to adopt that quarantine. This fails
  safe (no re-send, no discard), but it needs an operator sweep or adopt
  verb if it ever happens; it is not built here.
- A copy of a copy records only its immediate predecessor.
- Orphan reclaim (from the 2026-10-02 note) is still open.
