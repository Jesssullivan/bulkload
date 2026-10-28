# 2026-10-27: BL-2 source lane (fold verbs, gc-safe fetch, serve priority)

Authority: operator interview 2026-10-27, Linear TIN-3692 comments
`36925e2d` (BL2-Q2, Q3, Q5, BL2-small) and `ae17f1bd` (BL-2 selections).
SPEC rules 1, 2, 5 and 6. R-N13.

| stream | owner | repo/branch | state/evidence | next |
| --- | --- | --- | --- | --- |
| SPEC rule 1 exception | sting lab seat | #331 | merged | none |
| delete `docs/formal` | sting lab seat | #332 | merged | none |
| main green (registry rows) | sting lab seat | #335 | merged; fixes 3 of 4 | lsof bypass: argue on bulkload#188 |
| serve normal, pull forwards floor and priority | sting lab seat | #333 | merged | none |
| escalation test asks for background | sting lab seat | #337 | merged; #333 broke it | none |
| gc-safe fetch | sting lab seat | #334 | merged | none |
| worktree fold and reattach verbs | sting lab seat | #336 | merged (`03c7c46c`) | first ruled carry uses them |
| agent binary | sting lab seat | honey `~/.local/bin/tcfs-bulkload-agent-03c7c46c` | selftest ok, GC roots 2 of 2 | no running carry changed |

What each PR does is in its body. Facts a later lane needs:

- (Superseded 2026-10-28 by #339, which removed the lsof child; see
  `2026-10-28-bl2-f1-retire-libproc.md`.) Main still fails one test:
  `source_command_registry::every_child_goes_through_the_source_safe_builder`,
  on the Darwin `lsof` child of `retire::held_open` (#327). A bypass for
  it takes `FROZEN` past its cap of 17, and the registry says a bypass is
  argued on bulkload#188, never added in the test. It needs that argument
  and a ruling, or a `held_open` change that `darwin-check` on pzm covers.
- The fold verbs keep KC-1's records in SHA-256 (index and `status -z`),
  so a fold plan compares with the KC-1 evidence. The agent has no SHA-256
  crate, so `git_carry::fold` carries a FIPS 180-4 implementation pinned
  to the standard vectors.
- `attach` makes `canonical/.git/worktrees` when it is missing. Without
  it every move into a canonical repository with no linked worktree
  failed after the branch was created.
- `copy_tree` reads and writes each admin file. `fs::copy` clones on a
  reflink file system, and the registry's P84 scan refuses it here.
- The fold floor is 10% unless `--min-free-percent` is given (BL2-Q3).
  `main` tracks whether the flag was given (`FLOOR_GIVEN`); every other
  verb keeps the 25% default.
- `pack-refs` is a MAINTENANCE name. The registry allows it in
  `fold::fold_refs` alone (`REF_PACKERS`), which packs only the fold's
  namespace.
- On Darwin `procs::inside` cannot tell, so every fold worktree holds as
  `live-unknown` there. The fold moves worktrees on Linux only.
- `serve` left `SOURCE_SIDE_VERBS`. Other source-side verbs keep
  background. `pull` always forwards its floor, and its `--priority=`
  only when one was given.
- No fetch opt-out flag exists. TIN-7492 allowed one; no caller needs it.

Builds and tests ran on honey at `nice -n 19`, never on sting. Lessons:

- A `CARGO_HOME` under `/var/tmp` on honey lost crate sources mid-run.
  Crate files keep their tarball mtimes, so an age-based cleaner takes
  them. Keep `CARGO_HOME` under `~/git-build`; target and `TMPDIR` can
  stay under `/var/tmp`.
- A targeted gate can miss a test that reads the changed default.
  #333's did; the full run on the stack head found it.

Gate records are the PR comments.
