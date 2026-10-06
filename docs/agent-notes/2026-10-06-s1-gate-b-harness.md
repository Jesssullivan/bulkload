# 2026-10-06 S1 gate (b) harness (lane s1-gate-b-harness)

Rulings: OI-1002-Q30, OI-1003-Q3, OI-1003-Q66 (build the harness only; no neo
run until gate (a) passes), R-N81, R-N13.

- **Branch:** `feat/s1-gate-b-harness-20261006` from origin/main `b6ecd50`.
- **Worktree:** `bulkload.worktrees/gate-b-harness-20261006`. This session
  was its only writer.
- **PR:** none opened (by dispatch). Pushed to `origin`.
- **Protocol:** [2026-10-06-s1-gate-b-protocol.md](../plans/2026-10-06-s1-gate-b-protocol.md).

## Done

1. **Harness.** Added `crates/bulkload-bench/scripts/gate_b.py`. It imports
   the `r23_ab` helpers and leaves `r23_ab.py` unchanged. What it does:
   - It runs on sting and reaches neo only through one ssh command line.
   - Both hosts are gated: R-N81 on the source, and a load bound on the
     destination.
   - The workload is the sealed estate corpus (#159), with its SQLite seats
     left out of the comparable set.
   - Each of the 3 B reps runs N/R/N/R/N for the initial copy, a warm
     resume, and N/R/N/R/N for the 1 % XOR delta, which it then reverts.
   - Each rep calibrates the link first.
   - Native RSS is capped at 2 GiB, counting the pull and the wrapped
     source serve.
   - It writes a JSON verdict, a schema check and an evidence draft.
   - It has `--under-load` and `--dry-run` (loopback) modes.
2. **Native arm.** The audit found no remote arm, but the agent has one:
   `bulkload-agent pull HOST ... [REMOTE_EXECUTABLE [SSH_CONFIG]]`, a single
   `ssh -T` stream to `serve`. The harness drives that pre-W5 engine. It
   refuses with `NATIVE_REMOTE_ARM_MISSING`, "native remote arm missing
   (#47)", exit 2, in two cases:
   - the agent has no pull/serve pair;
   - `--native-streams` > 1 asks for W5 streams.
3. **rclone arm.** The gate (a) flags, over an sftp remote built from the
   ssh config:
   - `external` (default): the same OpenSSH command line as the native
     arm;
   - `internal`: built from `ssh -G`, refusing a host with no known_hosts.

   The config is 0600 and secret-free, `RCLONE_*` is dropped, and every
   log and the JSON are redacted.
4. **Tests.** Added `test_gate_b.py`, which uses fakes: gating, the native
   arm refusal (including a clean `main` refusal), verdicts and rollup, the
   JSON schema, redaction, rclone remotes, refusal parsing, tree
   verification, the delta plan, and the source helper run locally. One
   loopback smoke runs only when `GATE_B_AGENT` and `GATE_B_RCLONE` are set.
5. **Recipe.** Appended `just bench-gate-b` to the end of the justfile. It is
   not wired into `check-optional`, because L5 owns those lines.
6. **SLO.** Added a dated 2026-10-06 pointer amendment to `docs/slo.md`. It
   changes no SLO and lists the protocol choices that need ratification.

## Smoke (sting only, loopback; no neo)

`--dry-run --reps 1` ran with a release agent built from `b6ecd50` and
nixpkgs rclone 1.74.4. It used the loopback ssh shim and the local
sftp-server, on a synthetic corpus with 3 SQLite seats, a dangling link and a
directory link. Results:

- Exit 0 and `dry-run-complete-not-a-gate-sample`, with no schema problems
  (rerun 2026-10-06 18:03Z, after a session reattach).
- All 11 arms verified, and the native arm refused exactly the 3 excluded
  seats.
- Workload: 44 comparable files, 9 944 087 bytes; delta 4 files, 99 441 bytes
  planned, 118 807 bytes received (the last delta file is re-sent whole).
- Warm resume received 0 bytes and read 0 content bytes. The raw
  `source_bytes_read` was 32: two 16-byte SQLite magic probes.
- Native RSS: pull 46 MiB, serve 13 MiB. The delta reverted, and the restored
  manifest matched.
- `rclone.conf` was mode 0600 and held no secret key.

sting's load1 was about 64 during the run, so the timings mean nothing. The
same smoke is `LoopbackSmoke` in `test_gate_b.py`; it runs when GATE_B_AGENT
and GATE_B_RCLONE are set (47 tests pass with it, 46 and 1 skip without).

A fake agent with no `pull` verb is refused cleanly: exit 2,
`NATIVE_REMOTE_ARM_MISSING`, and `gate-b.json` has status `refused`.

## Open

- **Ratification.** The operator has not ratified these protocol choices:
  - SQLite seats out of the comparable set;
  - the 1 % XOR delta;
  - the N/R/N/R/N order;
  - the 2 GiB cap;
  - the default rclone transport (`external`, hash checks off);
  - `--under-load` by analogy with OI-1003-Q39 and Q50.
- **W5 gaps (#47).** The agent lacks N streams, zstd, `Ref`, `NeedRanges`, a
  serve RSS line, priority hand-off and a SQLite or exclude hand-off. The
  list is posted on #47
  ([comment](https://github.com/Jesssullivan/bulkload/issues/47#issuecomment-6022383837)).
- **Wiring.** `test_gate_b.py` is not in `check-optional` yet; that waits on
  L5's justfile lines.
- **SQLite probe reads.** The agent re-reads up to 16 bytes of every
  magic-refused SQLite seat on every pass. A rerun therefore never has
  `source_bytes_read` = 0 when the source holds SQLite. That is an R25
  reading question; the harness nets the probes out.
- **No neo run.** None until gate (a) passes (OI-1003-Q66).
