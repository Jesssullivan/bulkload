# 2026-10-20: the crash checker tells a reused inode number apart

Lane: coordinator brief 2026-10-20 (the resume-power-loss proof that failed
in #295's gate). Branch `test/trace-inode-reuse-20261020` from `origin/main`
`17e4790`. Edited and committed on sting; every build and test ran on
honey only (OI-1003-Q196), in this lane's own directories: worktree
`~/git-build/trace-inode-reuse-20261020`, one `CARGO_TARGET_DIR` and
`CARGO_HOME` under `~/git-build/trace-inode-reuse-20261020-cargo`,
`TMPDIR=/var/tmp/jess-bulkload-tests/trace-inode-reuse-20261020`,
`nice -n 19`, `CARGO_BUILD_JOBS=4`, `RUST_TEST_THREADS=4`, one cargo run at
a time; removed after. Test and checker code only: `io::crash_check` is
built only under `test` or the `io-trace` feature, and no shipped product
code changed. It touches the io layer, so `just darwin-touched` flags it
and `just darwin-check` on pzm applies. Nothing ran against the live
estate. R-N13 (lab R-HOOK-CONVERGENCE-20261004). Hook output was advisory:
the process-safety audit ran on the commit; the push printed the
fork-identity advisory.

Requirement served (docs/SPEC.md R2.3, verbatim): "**Piecemeal.** Any run
can stop at any point and resume later." The resume power-loss proofs are
the evidence for "any point", and AGENTS.md "Validation" holds them
mandatory ("The fault harness, the R-N88/R-N119 power-loss proofs and the
R25 counter tests are mandatory and never move to the optional tier"). A
proof whose model refuses a legitimate trace proves nothing.

## The failure

`just fault-harness` → `handoff-off` → `resume-power-loss`, #295's gate:
`materialize::adoption_power_loss::a_level_crashed_after_some_renames_adopts_the_renamed_and_makes_the_rest`
panicked at `adoption_power_loss.rs:906` (the `check_view(...).unwrap()`):
"trace event 25 creates node NodeId { dev: 64768, ino: 1275116270 }, which
the image or an earlier event already names (inode reuse); refusing to
conflate them". The proof crashes a level after two of its renames, and
the resume sweeps the temporaries and makes the level's directories
again; xfs gave one of them a swept temporary's inode number.

## Cause

The checker keyed every node by `NodeId { dev, ino }` for the whole
trace, so a number the file system freed and gave out again within one
trace was refused (`build_ops`). The proof was right; the model could not
represent the trace.

## What changed

- `crash_check::incarnations(initial, events)`: an inode is freed only
  once it has no name and no open descriptor, so a create (`Create`,
  `Mkdir`, `Symlink`) of a number the image or an earlier event names is a
  new inode exactly when, by then, the trace's own namespace has dropped
  every name of the old one (an unlink, or a rename or exchange over it).
  From that create on, every event and every commit record naming the
  number names the new inode, which gets an unused identity (its `dev`,
  an `ino` counted down from `u64::MAX`); everything before keeps the old
  one. Indices are unchanged.
- A create of a number whose inode still has a name is refused as
  conflated. `check_view` and `check` still refuse any reuse they are
  handed unresolved, and now say to resolve it with `incarnations`: an
  invariant reads node identities from the trace's commit records, so it
  must read the resolved trace too, and only the caller can pass it that.
- The recorded-trace sites resolve before checking: the five in
  `materialize/adoption_power_loss.rs`, and in `tests/power_loss.rs` the
  copy and rerun helpers, the timing proof and `check_store`.

## Results

All on honey, `TMPDIR` on xfs (`/var/tmp`, the root file system), load1
9 to 35 (honey was quiet; no load was generated).

- `6a2cd99` (the change): `io::crash_check::` under `fault-injection,io-trace`
  39 passed (the four hand-written reuse tests and the forced one among
  them), without features 36 passed. Then ten rounds in a row, each
  `materialize::adoption_power_loss::` (the `resume-power-loss` set, the
  failing proof among its 8) and the forced-reuse test: 10 of 10 rounds,
  8 of 8 and 1 of 1 each; the forced test never printed its "no inode
  number was reused" skip, so xfs reused a number in every round.
  `--test power_loss` 19 passed. clippy flagged `explicit_auto_deref`, and
  rustfmt three spots, fixed in `5c9bd42` and `31ca370`.
- `5c9bd42`: `cargo clippy -p bulkload-agent --all-targets --locked -- -D
  warnings` clean with `io-trace`, with `fault-injection,io-trace` and
  without features.
- `31ca370` (the PR's code head): `cargo fmt --all -- --check` clean;
  `io::crash_check::` and `materialize::adoption_power_loss::` under
  `fault-injection,io-trace` 47 passed; `io::crash_check::` without
  features 36 passed.

The original failure (a reuse inside the resume proof) is environmental
and was not seen again here; the forced test reproduces the same class
(a freed number given to the next create within one trace) on every run.
