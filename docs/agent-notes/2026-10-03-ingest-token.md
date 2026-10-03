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
  systems, or remounted with new inode numbers, any number of times): when
  its journal has acks, its own quarantine is absent and ANY other
  `incoming-bulkload-<pack_id>-<key>` quarantine exists, resume and open
  refuse `JOURNAL_OWNERSHIP_CONFLICT` / `state_dir_copied`. Nothing is
  abandoned, adopted or re-sent. Once no other quarantine for the pack id is
  left, the journal is abandoned as `quarantine_lost`, as before.
- Review round 1 (BLOCK) fix: the first version refused only when the
  immediate predecessor's quarantine existed. A first hop whose resume
  refused had already been re-keyed, so a second hop named a predecessor
  that never had a quarantine, fell through to `quarantine_lost`, and a
  fresh open restarted at segment 0 while the original quarantine still
  held every acked pack (a silent re-send, R25 violation). The first
  version's note claimed copy-of-copy "fails safe (no re-send, no
  discard)"; that was wrong. The predecessor is now kept in the record for
  provenance only; the ingest no longer reads it.
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
passes. Also `issue120_a_state_dir_copied_mid_session_refuses_rather_than_resends`
and `issue120_a_state_dir_moved_twice_refuses_rather_than_resends` (the
reviewer's two-hop probe: copy, delete, refused resume, copy, delete; the
second hop refuses `state_dir_copied` instead of `quarantine_lost`, and the
plan restarts only after the orphan is swept by hand).

Gates: `nix develop .#default --command just check-fast` and
`just resume-power-loss` green (rustc 1.96.1).

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

- #138 (pre-migration): a state dir moved across file systems with an
  in-flight journal refuses `state_dir_copied` until the quarantine it left
  behind is gone, and nothing can remove or adopt it. No re-send and no
  discard, but a dead end; main resumed this case. Needs an adopt/sweep
  verb or an operator ruling.
- #139 (later): no `st_dev` in the binding, so a cross-file-system copy
  that reuses both inode numbers keeps the shared token.
- #140 (later): legacy 32-byte tokens are bound in place, so a pre-upgrade
  copy keeps the shared token (R-N56: none deployed).
- #141 (later): the record's crash safety assumes in-sector atomicity and
  ordered data; the assumption is now written in `state_binding`'s doc.
- Orphan reclaim (#121, from the 2026-10-02 note) is still open.
