# 2026-10-28: BL2-F1 and BL2-F4 (retire holder check without a child; darwin-check)

Authority: operator interview 2026-10-28, Linear TIN-3692 comment
`c10e9bec`. BL2-F1, verbatim: "Code fix + pzm check". BL2-F4, verbatim:
"Run it now". SPEC rule 6 and engine rule S2 (the source command
registry). R-N13.

| stream | owner | repo/branch | state/evidence | next |
| --- | --- | --- | --- | --- |
| BL2-F1 retire holder check | sting lab seat | #339 | merged `d88f4e84` | none |
| BL2-F4 darwin-check for #337 | sting lab seat | #339 comment, #337 correction | no new Darwin failure | none |

Facts a later lane needs:

- `git_carry::git` builds Git children only, so the ruling's "route the
  lsof call through the source-safe builder" could not be literal. #339
  removed the child instead. `procs::open_vnodes` reads every process's
  vnode descriptors in this process through libproc. `retire::held_open`
  matches by device and inode. `FROZEN` and its cap of 17 are unchanged.
- A process that exited, or another user's, is skipped, as on Linux. Any
  other libproc failure answers unknown, and retire skips the file.
- `darwin-check` exits 1 on main for reasons outside this lane. PZM's
  `/nix` has about 20 GB free of 170 GB, below the 25% free-space floor,
  so about 160 estate, adopt, handoff and fold tests refuse there. Three
  lib tests fail as recorded on 2026-10-24.
- `just darwin-check` stops each integration group at its first failing
  binary. To see every binary on Darwin, run
  `cargo test -p bulkload-agent --tests --no-fail-fast`.
- On honey, a `TMPDIR` under `/home` (84% used) trips the same floor.
  Keep `CARGO_HOME` under `~/git-build` and `TMPDIR` on `/var/tmp`.
- `reconnect::a_mid_frame_disconnect_preserves_committed_files_and_resumes_the_partial_file`
  flakes under honey load: 1 failure in 4 runs at #339's head.
- One rust-test run on PZM saw 354 transient `EPERM` errors from mkdir in
  `TMPDIR`. Two later runs at the same head saw none. The cause is unknown.

Gate records are the #339 comments. Logs: honey
`~/git-build/bl2-f1-20261028/logs/`, PZM
`/nix/var/tinyland/user/client-tmp/bulkload-build/darwin-check-f1-*`.
