# 2026-10-19: ledgers are dated before the writes they vouch for

Lane: coordinator follow-up 1 to #290 (the timestamp found while
deflaking p90 and h14). Branch `fix/racy-stamp-before-write-20261019` from
`origin/main` `9854236`. Edited and committed on sting; every build and
test ran on honey only (OI-1003-Q196), in this lane's own directories:
worktree `~/git-build/racy-stamp-20261019`, one `CARGO_TARGET_DIR` and
`CARGO_HOME` under `~/git-build/racy-stamp-20261019-cargo`,
`TMPDIR=/var/tmp/jess-bulkload-tests/racy-stamp-20261019`, `nice -n 19`,
`CARGO_BUILD_JOBS=4`, `RUST_TEST_THREADS=4`, one cargo run at a time,
targeted filters and clippy only (a gate was running); removed after.
Nothing ran against the live estate. R-N13 (lab
R-HOOK-CONVERGENCE-20261004). Hook output was advisory and is recorded
here: the process-safety audit ran on every commit; the push printed the
fork-identity advisory.

Requirement served (docs/SPEC.md R2.2, verbatim): "a rerun replaces a DEST
file only when it is bulkload's own output, unchanged since it wrote it
(LIVE-30);" and "DEST is live too, and is never clobbered." Register row
C03 of the 2026-10-19 reset review (racy file capture) is the class.

## The defect

A workspace ledger (`LandedWorkspace`) and a hand-off record
(`HandoffLanded`) trust an unchanged print stamped more than
`RACY_GRANULARITY_NS` (2 s) before their `recorded_ns`. Four writers took
that instant after their writes:

- `converge::record_landing`, for every first landing (standalone, linked,
  and the crash recovery of a linked one), after the checkout was written;
- `converge::next_ledger`, for every converge and converge settle, after
  the converge's writes;
- the hand-off render (`render_workspace`), after it wrote and printed its
  targets, although its own comment called the instant "the pass's start";
- the hand-off settle, after re-reading the targets.

A pass slower than 2 s from a write to that instant (a loaded host)
trusted at once a print taken after the write. A file a writer changed in
that window (a live DEST, R2.2) was then vouched for as bulkload's own,
and a later converge could replace it (LIVE-30). The converge settle also
re-dated the old ledger's prints, which it carries unread, to the settle's
time. Adopt already took its instant before its first read (review of
2026-10-16, finding 4); that is the rule applied here everywhere.

## What changed

- First landings: `restore_staged_landed`, `restore_linked_staged_landed`
  and `recover_linked_landing` take the instant at their start and pass it
  through `finish_landing` to `record_landing`.
- Converge: `prove` takes the instant before its first read
  (`Proof::started_ns`); `next_ledger` is dated by it.
- Converge settle: dated as the ledger it settles from, never later.
- Hand-off: `render_workspace` and `settle` take the instant at their
  start.
- Test hooks: `mid_pass::Stage::LandingWritten` (keyed by the destination's
  parent) and `ConvergeWritten`.

The 2 s allowance and every comparison against it are unchanged. Every
instant only moves earlier, so a pass can read more, never trust more.
Cost: every seat a pass writes is read by content once by the next pass
that proves it, which is what already happened whenever the pass took
under 2 s (the tests were written for that case).

## Results

All on honey, load1 52 to 120.

- `88bd7b0` (the four tests and the hooks, old stamps): the four tests
  fail, each at its intended assertion: the linked landing's proof did not
  refuse the rewritten seat (`tests.rs:1128`), a standalone seat was
  outside its ledger's racy window (`:1161`), the converge's ledger was
  dated after its writes (`:1231`), and the slow hand-off's edit was
  landed over (`handoff/tests.rs:555`).
- `37a6709` (the fix): the four pass. `cargo clippy -p bulkload-agent
  --all-targets --locked -- -D warnings` clean; `--lib --
  git_carry::converge:: estate::` 155 passed; `--test estate_adopt` 40,
  `--test estate_handoff` 53, `--test estate_converge` 23, `--test
  estate_move` 1, all passed.
- `5714805` (rustfmt of one test line, the PR's code head): `cargo fmt
  --all -- --check` clean; main's own p90 (`estate_adopt`, no re-stamp)
  10 of 10 and main's own h14 (`estate_handoff`, no re-stamp) 10 of 10.

## #290

With this fix the p90 and h14 re-stamps in #290 are unnecessary: every
seat a landing or a hand-off writes is racy against its own record
however long the pass takes, which is what both tests assumed, and main's
versions of both passed 10 of 10 here. #290's estate_move change (a
source-side capture, untouched here) and its reseat change (free-space
drift) stay necessary. #290 is not touched until it merges; the revert
of the two re-stamps is a follow-up then.

2026-10-20: #290 merged (main `86353ba`), then #283 (`912907c`); both
were merged into this branch with signed merge commits (`bb2631b`,
`a6a78c3`). `e040678` drops #290's p90 and h14 re-stamps (both tests back
to main's form before #290, p90's comment saying why its landed seats are
racy; the #290 note carries a dated label). On honey at `e040678`: clippy
`-D warnings` and fmt clean; the four C03 tests 4 of 4; p90 10 of 10 (22.4
to 39.4 s, load1 29 to 80) and h14 10 of 10 (7.1 to 15.4 s, load1 36 to
48), both without the re-stamps; `--test estate_adopt` 40 and `--test
estate_handoff` 53 passed.

## Open

- Transfer outputs, reseat placements and the capture side were read, not
  changed: the capture already takes its instant before its census, the
  transfer's settled checks take theirs before the read, and reseat does
  not trust prints by time. A reviewer may want the transfer's own output
  rows audited the same way.
