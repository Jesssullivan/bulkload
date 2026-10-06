# 2026-10-06 S1 gate (b) harness (lane s1-gate-b-harness)

Rulings: OI-1002-Q30, OI-1003-Q3, OI-1003-Q66 (build the harness only; no neo
run until gate (a) passes), R-N81, R-N13.

- **Branch:** `feat/s1-gate-b-harness-20261006` from origin/main `b6ecd50`.
- **Worktree:** `bulkload.worktrees/gate-b-harness-20261006`. This session
  was its only writer.
- **PR:** none opened (by dispatch). Pushed to `origin`.
- **Review round 1:** `72d934d` (fixes) and `c1d0d02` (merge of origin/main);
  see the section at the end. Where it differs from the text above, the
  later section is current.
- **Commit:** `024ec1f` (signed) carries the harness, tests, recipe, protocol
  note and SLO pointer.
- **Validation:** `just check-fast` exited 0 on that tree (2026-10-06 15:50
  EDT, inside `nix develop`, after about 1 h 50 min queued on the lane lock).
  origin/main then stood 6 CI commits ahead (`2247ab8`, PR #190); the branch
  is not merged with it yet, and a trial merge is clean.
- **Protocol:** [2026-10-06-s1-gate-b-protocol.md](../plans/2026-10-06-s1-gate-b-protocol.md).

## Done

1. **Harness.** Added `crates/bulkload-bench/scripts/gate_b.py`. It imports
   the `r23_ab` helpers and leaves `r23_ab.py` unchanged. What it does:
   - It runs on sting and reaches neo only through one ssh command line.
   - Both hosts are gated under R-N81: AC power on the source, and load1
     under 2.5 on both.
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

## Review round 1 (2026-10-06, same lane)

Rulings: OI-1002-Q30, OI-1003-Q3, OI-1003-Q66, R-N81, R-N13.

Three medium review findings were valid and are fixed in `72d934d` (signed).
`c1d0d02` (signed) merges origin/main `2247ab8` into the branch.

1. **Rep rule weaker than gate (a).**
   - The warm resume now gates: 0 bytes received and 0 content bytes read, or
     the rep fails.
   - The harness has no interrupted-resume phase and does not add one. Gate
     (a) stops a transfer in-process; over ssh that would mean stopping a
     running `pull`, and the harness never signals a process. Each verdict
     records `r25_interrupted_resume: "not-run: …"`, and the rule string and
     `deviations_from_gate_a` carry it as an unratified deviation.
   - The harness now waits out the racy window after the restoring XOR. This
     was a low finding, but once the warm resume gates, a later rep could fail
     on the harness's own rewrite without it.
2. **R-N81 on the destination.**
   - Gated mode refuses `--dest-load-limit` over 2.5 with `LOAD_LIMIT`.
   - Each sample's `gated` flag is computed against the fixed 2.5 on both
     hosts.
3. **Destination disk budget.**
   - A read-only `measure` helper op sizes the sealed corpus before anything
     is copied on the source.
   - The harness refuses with `DEST_SPACE`, exit 2, unless the work root keeps
     the agent's 25 % free floor after every destination held at once.
   - A verified rep's destinations are released, so a run holds 5 copies at
     once, not 15. `--keep-destinations` keeps them.
   - The report records the free ratio and the budget in `destination.disk`.

The protocol note, the 2026-10-06 `docs/slo.md` amendment and the recipe
comment say the same.

### Checks

- `test_gate_b.py`: 59 tests pass with `GATE_B_AGENT` and `GATE_B_RCLONE` set,
  58 and 1 skip without.
- Two-rep loopback smoke (`--dry-run --reps 2`, sting only, release agent,
  rclone 1.74.4): exit 0, no schema problems, both warm resumes at 0 bytes
  received and 0 content bytes read, both reps' destinations released.
- The budget refused for real during this round: `/srv/scratch` and
  `/srv/cache/jess` each stood under 25 % free at some point, and the dry run
  exited 2 with `DEST_SPACE` before any copy.
- `just check-fast` exited 0 on `c1d0d02` (2026-10-06 16:48 EDT, inside
  `nix develop`, under the lane lock, run as a waited background task because
  it outlasts the 10-minute foreground limit).

### Open after this round

- **Ratification, added to the list above.**
  - No interrupted-resume phase in gate (b).
  - Warm-resume reads net of the SQLite magic probes.
  - Releasing a verified rep's destinations, and the budget estimate (4 KiB
    per entry, 64 MiB slack).
  - Running the pre-W5 single-stream pull as the native arm, where #47 defines
    gate (b) as W5's acceptance.
- **A volume for the gated run.** sting has no volume today that holds 5
  estate copies (about 21 GB) above a 25 % floor: `/srv/scratch` has about
  22 GiB available at 28 % free. The operator needs to name a work root, or
  rule on `--min-free-percent` for the native arm.
- **Low findings, not fixed (by dispatch).**
  - `op_xor` compares a realpath against a root that is not realpath'd, so a
    `--source-work` under a symlinked prefix (Darwin `/tmp`, `/var`) refuses
    every delta target.
  - Nothing enforces OI-1003-Q66 in code; gated mode needs no gate (a)
    evidence and does not check the source is neo or differs from the
    destination.
  - The last delta file is prefix-XORed but re-sent whole, so delta bytes
    moved exceed the recorded 1 %.
  - Arm logs are redacted only after the arm exits.
  - A failed link calibration (`complete: false`) is never acted on.
  - The serve wrapper needs Python 3.9 (`os.waitstatus_to_exitcode`); its
    comment says 3.8+.

## Recheck and PR (coordinator, 2026-10-06)

- **PR:** #193, opened by the recheck stage from `3dc5562`. Verdict CLEAN: the three medium findings are fixed in `72d934d` (warm resume gates a rep; gated mode refuses a destination load limit over 2.5; the destination disk budget and release of verified reps).
- **Merge of main:** `5ed5c44` merges origin/main `8e23b1d` (signed, clean). The recheck stage's check-fast on it was still queued when the stage ended. This lane changes no Rust, so the coordinator ran the non-Rust gates and `test_gate_b.py` on the merged tree instead; the PR's CI is the full gate.
- **Still for the operator:**
  - ratify the draft protocol choices (the pre-W5 single-stream native arm, SQLite seats outside the comparable set, the 1% XOR delta, N/R/N/R/N order, the 2 GiB cap, no interrupted-resume phase);
  - name a work root for a gated run. No sting volume holds 5 estate copies (about 21 GB) above the 25% floor today.
- **No neo run** until gate (a) passes (OI-1003-Q66).
