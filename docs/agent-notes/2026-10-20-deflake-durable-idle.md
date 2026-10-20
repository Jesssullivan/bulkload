# 2026-10-20: the durable group idle test waits for the committer, not the clock

Lane: coordinator brief 2026-10-20 (the durable group test that failed in
#278's gate). Branch `test/deflake-durable-idle-20261020` from
`origin/main` `912907c`. Edited and committed on sting; every build and
test ran on honey only (OI-1003-Q196), in this lane's own directories:
worktree `~/git-build/durable-idle-20261020`, one `CARGO_TARGET_DIR` and
`CARGO_HOME` under `~/git-build/durable-idle-20261020-cargo`,
`TMPDIR=/var/tmp/jess-bulkload-tests/durable-idle-20261020`, `nice -n 19`,
`CARGO_BUILD_JOBS=4`, `RUST_TEST_THREADS=4`, one cargo run at a time;
removed after. Test-only. Nothing ran against the live estate. R-N13 (lab
R-HOOK-CONVERGENCE-20261004). Hook output was advisory: the process-safety
audit ran on the commit; the push printed the fork-identity advisory.

Requirement served (docs/SPEC.md R3, verbatim): "**Speed comes from the
engine:** parallelism, a smarter wire, native walks and unsafe Rust where
it wins (FAST-03, FAST-05, FAST-08, FAST-11, FAST-17, FAST-23), on the R2
contract (LIVE-07, LIVE-29)." The test pins OI-1003-Q154 lever 3 (a longer
group idle, so fewer, larger durable groups).

## The failure

`io::durable::tests::a_group_waits_out_its_idle_and_closes_at_its_age`
(`io/durable.rs:1452`) failed in #278's gate at load 55 to 60: `left:
[2, 1, 1, 1]`, `right: [1, 1, 1, 1, 1]`. Its second case submits five
items 200 ms apart to a committer with a 20 ms idle and expects five
groups; two items shared one. The gap had already been widened from 60 ms
to 200 ms for the same failure ("a 60 ms gap grouped two items at load 45
to 60 on 32 CPUs").

## Cause: scheduling, not the grouping rule

The committer (`run`) holds an open group in `recv_timeout(group.wait())`
and closes it only on `Timeout`. A committer thread that is not scheduled
between the idle's expiry and the next item wakes to find that item
already queued; `recv_timeout` returns it, and it joins the open group,
whatever the wall-clock gap. No gap makes the case deterministic. The
gates run the tests at `nice -n 19` (the honey lane rule), and honey's
kernel (6.12) schedules with EEVDF: a nice-19 thread (weight 15) that
wakes on a CPU where nice-0 work (weight 1024) is runnable gets a virtual
deadline about 68 base slices out, roughly 200 ms of waiting, which is
the gap the case had been widened to.

It is not a durability defect. A group that closes late commits its items
later; within every group the data is sealed before its records commit
(the module's load-bearing order), and `Committer::sync` still closes a
group at once. Grouping by what the committer observes is the rule the
module documents (`GROUP_IDLE`: "after this long without a new item").

## What changed (test only)

- The 20 ms case: before each next item it waits, bounded at 10 s, until
  the committer has closed every group so far by itself, on its idle. The
  case is back to the same items 60 ms apart as the 2 s case it contrasts
  with, and it still fails if a lone group never closes without a sync.
- The age case had the same race the other way: twenty items, at least
  three groups, but a starved committer takes several queued items at
  once and a group's age runs from when it took the first. It now submits
  items 60 ms apart until the age has closed three groups (bounded at
  30 s) and checks nothing was lost.
- The 2 s case (one group, closed by sync) is as it was: it fails only if
  the producer itself stalls for 2 s between items.

## Results

All on honey.

- Unmodified `origin/main` `912907c`, 10 runs at load1 25 to 51: 10
  passed. The failure did not reproduce at that load.
- `4e93b40` (the 20 ms case only): clippy `-D warnings` and fmt clean, 10
  of 10 at load1 24 to 32, `io::durable::` 7 of 7.
- Contention on one CPU: each run of the test binary pinned to CPU 31
  with 80 self-terminating nice-19 spinners on that CPU (load1 86 to 109
  with them): `main` 10 of 10, `20be4eb` 10 of 10. Equal-nice spinners do
  not starve a waking committer under EEVDF, so this does not reproduce the
  nice-19-under-nice-0 case; no nice-0 load was generated on a shared host.
- `20be4eb` (the PR's test head): clippy `-D warnings` and fmt clean; the
  test 10 of 10 at load1 35 to 47; `io::durable::` 7 of 7.

The fix is justified by the mechanism above, not by a reproduction: the
changed cases now wait on what the committer did (a closed group), not on
the clock, so a descheduled committer only makes them slower.
