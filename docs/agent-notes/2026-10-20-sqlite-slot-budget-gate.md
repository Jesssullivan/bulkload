# 2026-10-20: #278 gate failure in the finding-B test (sqlite slot budget lane)

Lane: `feat/sqlite-slot-budget-20261019` (continues
`2026-10-19-sqlite-slot-budget.md`). Rulings: R-N13; operator ruling
2026-10-19 "Hold one read snapshot". Builds and tests on honey only.

The #278 merge gate on honey at `26e645c5` (main `912907c`) failed group 1
on two tests. One is this lane's:
`provider_sqlite::tests::an_extended_busy_step_is_waited_out_on_a_new_backup_object`
panicked at "a new one goes on" (a fresh backup on the same source
connection did not finish after the wal-index locks were dropped). The
other (`io::durable::tests::a_group_waits_out_its_idle_and_closes_at_its_age`)
is not this lane's; the deflake lane has it.

Cause (test-only, not a product gap). The test held the wal-index's WRITE
and RECOVER locks with this process's own OFD locks on the `-shm`. OFD
locks belong to the open file description, and a fork copies the
descriptor table: a child that any concurrent test spawns (the gate's
group 1 runs ~950 tests at default parallelism, many of them spawning
writer and git children) holds a copy of the locked descriptor until it
execs, and the locks with it. Closing the test's own descriptor then did
not release them, and the fresh step met `SQLITE_BUSY_RECOVERY` again.

Evidence (honey, throwaway test, not committed): with a second descriptor
of the locked description open (as a fork leaves it), a fresh backup on
the same connection returned `Err(261)`; with both closed it returned
`Ok(Done)` on the same connection. Alone, the test passed 20/20 (load ~80,
`TMPDIR` under `/tmp` and `/var/tmp`), and 8/8 in a 172-test concurrent
subset; the race needs the gate's full group. So a fresh backup object on
the same connection does go on once the lock is really gone: the product
retry (a new backup object, bounded by the wall) needs no new connection.

Fix: the locks are held by a dedicated child process
(`wal_index_lock_holder_child`, this test binary re-run alone) with POSIX
locks, which a fork never copies, and are released by waiting for that
child to exit (`WalIndexHolder::release`). The fresh step's result is now
printed if it ever fails again. Proof runs are in the PR body.
