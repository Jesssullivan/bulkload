# 2026-10-07: s1-rig-baseline-pin (the rig's A control and its rclone)

Rulings: OI-1003-Q103, OI-1003-Q105, OI-1003-Q96, OI-1003-Q97, OI-1002-Q30,
R-N81, R-N13.

Branch `feat/s1-rig-baseline-pin-20261007`, cut from
`origin/feat/s1-hermetic-rig-20261007` (PR #204, `d534aa4`, still open) and
merged with `origin/main` twice (`5875610`, then `8006085`). Pushed; no PR
is open for this branch.

Two sessions wrote this lane. The first ran the diagnostics, wrote the pin
and the rclone rule, and ended before committing. The second re-read the
host logs, re-counted them, committed, fixed the registry break below,
merged main and took the sample.

## Shas

- `7cf7338` feat(bench): pin the rig's A control and the flake rclone.
- `5689afd` fix(bench): keep the pmset child in `Preflight::read`.
- `adfdf57` Merge origin/main (`8006085`: #202). This is B of the sample.
- The commit that adds this note and the evidence follows `adfdf57`.

## What was asked

- Q103: on the hermetic rig, pin a newer baseline A that runs, and keep
  B/A/B/A/B. The verdict rule is unchanged (OI-1002-Q30).
- Q105: the rig uses the flake-pinned rclone on every host and for every
  sample; a version bump is a deliberate, recorded change.

## 1. The old A's `IO (errno 32)`, diagnosed

All runs are native-only (`--only native --reps 1`), one full native bench
each, on mbp-13 (Linux 6.19.5, XFS, 2 cores and 4 threads). Scripts and logs
are on the host: `~/git-bulkload/pin-diag.sh`, `logs/pin-diag-info1.log`
(12 rounds, `--informational`, the form of the earlier `logs/diag.log`) and
`logs/pin-diag-gated1.log` (12 rounds, R-N81 gated behind the `pmset`
translator, 15 s between runs).

| Bench built at | Corpus v1: refused / runs | Dry-run corpus: refused / runs |
|---|---:|---:|
| `7c3ecc7`, the old A | 4 / 15 | 15 / 15 |
| `41bf9a4`, v4 | 0 / 15 | 0 / 15 |
| `3931471`, the new A | 0 / 24 | 0 / 24 |
| `5875610`, main today | 0 / 12 | 0 / 12 |

Each row includes the three runs of the earlier `logs/diag.log` where that
build was in it (`7c3ecc7` and v4). Every refusal was `benchmark refused: IO
(errno 32)`. On the dry-run corpus the old A refuses before its first
sample; on corpus v1 it refused after two or three samples.

The host was not quiet throughout, and the note says so rather than calling
it quiet. The informational rounds ran back to back (load1 0.3 to 2.3). The
wp0g-ledger-sync lane built and measured on mbp-13 in the same hour: its
build overlapped the last two informational rounds (load1 up to 4.6), and
its build and `ab-final` series overlapped the gated rounds from about
12:46Z. These are completion counts, not timings, and the pressure makes
the zeroes stronger. No timing from these runs is reported.

Is it #201? Not on this evidence. #201 is a rare race on current main under
heavy load. Current main did not refuse once here. The old A refuses every
time on the dry-run corpus, and it also refuses on sting when the bench is
held to two CPUs (`taskset -c 0,1`: 2 of 4 runs), where the new A does not
(0 of 40). So it was fixed between `7c3ecc7` and `3931471`. It has #201's
symptom and may share its cause.

Where the fix is: a local bisect on sting (dry-run corpus, two CPUs, 40
native-only runs per commit) found 0 of 40 refusals at `3931471`, `cb681d3`
(#159), `ddce809` (#118), `f299b79` (#107), `ecf7684` (#96) and `bc5e13c`
(#75). `bc5e13c` is the first main merge after `7c3ecc7`, so the fix came in
with #75. The first session recorded 2 of 4 refusals for `7c3ecc7` under the
same two-CPU hold; that count was not re-run by the second session.

No comment was added to #201: it did not reproduce on main on mbp-13.

## 2. The new A

`3931471738cc3995a0e564e73af3134d7a7f1ff2`: main at the merge of #195, the
first-parent main commit that the rig lane's first commit (`9d01b68`) was
cut from.

- (a) It completes: 24 of 24 on corpus v1, 24 of 24 on the dry-run corpus.
- (b) It has no Linux power preflight (nothing on main has one until #204
  merges), so it runs behind the ratified `pmset` translator. Twelve of the
  24 runs on each corpus ran that way, R-N81 gated, with every sample row
  `gated=true`.
- (c) It is the control the task recommended, and the data do not argue
  against it. A is for drift of the rig: B moves with main, A does not.

Recorded in `RIG_BASELINE_A` in `r23_ab.py` (sha, date, reason), in
`test_r23_ab.py`, in the runbook (section 5) and in the third 2026-10-07
amendment of `docs/slo.md`. The pin is per rig: other hosts keep
`7c3ecc7`. A gated sample on a rig of record with another `--rev-a` is
refused.

## 3. The rclone rule

`RCLONE_PIN` in `r23_ab.py`: `rclone v1.74.4`, nixpkgs
`241313f4e8e508cb9b13278c2b0fa25b9ca27163`,
`/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4` on
`x86_64-linux` (built on sting and on mbp-13, the same path) and
`/nix/store/5aw5dn7z12pnwjp1yghg9xar9bcl21fk-rclone-1.74.4` on
`aarch64-darwin` (evaluated on sting, not built).

- Gated and under-load: any other `--rclone` is refused unless
  `--rclone-override-reason` is given. An override is recorded, the verdict
  carries `RCLONE-OVERRIDE`, and `of_record` is false.
- Gated: refused when the flake resolves to a build that is not the pin.
- Every report has `rclone_record`; the evidence and the status line show
  it. A rep whose bench header names another version aborts the sample.
- `flake_rclone` now picks the output that has `bin/rclone`. The old code
  took the last output printed, which was right only by the order Nix
  printed `-man` and the main output.

## 4. A break between #204 and main (#197), fixed here

`just check-fast` failed on the first merge of main:
`source_command_registry::every_child_goes_through_the_source_safe_builder`.
#204 moved the macOS `pmset` child from `Preflight::read` into
`pmset_power`; main's P76 registry (#197) pins it as
`bench::main::read("pmset")` in a list that never grows. `5689afd` puts the
child back in `Preflight::read`. Behaviour is the same, and the registry
and the agent crate are untouched. PR #204 has the same break as soon as it
merges main at or after `5875610`; it needs this commit or its own fix.

## 5. check-fast

- On `adfdf57`: first run failed with one test,
  `disposition::tests::a_ledger_is_bound_to_the_plan_bytes_not_only_its_path`
  (`Io(Some(11))`), which is the known flake #200. Rerun once: exit 0.
- On the commit that adds the evidence and this note: see the lane's
  result; the branch is pushed only after exit 0.

## 6. The gated sample (one, gate (a), B/A/B/A/B)

- Host mbp-13 (MacBookPro12,1, Linux 6.19.5-11.xr.el10 x86_64, i7-5557U, 2
  cores and 4 threads, 15.3 GiB, XFS, power probe `sysfs`).
- Prebuilt first (`logs/pin-build-gate-a2.log`, B compiled in 2m 48s, A and
  v4 already built), then waited for load1 below 0.10. No other build or
  bench was running before the start or after the end; load1 was 0.07 when
  rep 0 was admitted and 1.33 after the v4 rep, from the sample itself.
- Start 2026-10-07T14:10:33Z, end 14:11:43Z. Script `~/git-bulkload/gate-a2.sh`,
  log `logs/r23-ab-20261007-gate-a2.log`, work root
  `runs/r23-ab-20261007-gate-a2`.
- B is `adfdf57f0a70914b34fbdf6b4a4c89555c5d08a8`. `git diff origin/main
  adfdf57 -- crates/bulkload-agent crates/bulkload-proto` is empty, with
  `origin/main` at `8006085` (#202) before and after the sample. Main had
  gained no WP0(g) or other engine change: #202 changed agent tests only.
- A is `3931471738cc` behind the `pmset` translator; v4 is `41bf9a4`.
- rclone: `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4`,
  `rclone v1.74.4`, the flake-pinned build and the committed pin.
- Corpus v1 identity `f4a7619f...b07c7`, verified before and after.

The status line, as printed:

```
r23-ab status=complete-draft rig=mbp-13 rig_role=record of_record=true rclone_pinned=true json=/home/jess/git-bulkload/runs/r23-ab-20261007-gate-a2/r23-ab.json evidence=/home/jess/git-bulkload/bulkload/docs/evidence/r23-2026-10-07-1410Z-mbp-13-record.md gate=FAIL
```

**FAIL**: 0 of 3 B reps pass. Every B rep and both A reps: `r23_initial_win`
false, `r23_delta_win` false, both R25 zero checks true, RSS below 2 GiB.

| rep | label | native initial median ms | rclone initial median ms |
|---:|---|---:|---:|
| 0 | B | 863.384 | 359.103 |
| 1 | A | 866.452 | 352.495 |
| 2 | B | 856.695 | 451.957 |
| 3 | A | 907.211 | 349.279 |
| 4 | B | 802.696 | 374.447 |
| 5 | v4 | 1,282.749 | n/a |

B's bench medians (the report's gate block): initial 856.695 ms native against 374.447 ms
rclone; delta 140.475 ms against 107.739 ms. Nothing was tuned and nothing
was rerun.

Copied unchanged into `docs/evidence/` (sha256 equal on both hosts):
`r23-2026-10-07-1410Z-mbp-13-record.json` (`56fc48da...`) and
`r23-2026-10-07-1410Z-mbp-13-record.md` (`70ada220...`).

On mbp-13 everything this lane wrote is under `~/git-bulkload/`; the
scripts set `TMPDIR` to `~/git-bulkload/tmp`.

## Open

- The sample is a FAIL of record on the rig. It is the same shape as the
  06:52Z B/B/B sample: native is slower than rclone on this host, where the
  native arm fsyncs every file and rclone syncs nothing.
- PR #204 needs `5689afd` or an equivalent before it can merge main.
- The verdict and medians are not on TIN-4543 yet, and the Linear ledger
  was not reconciled by this lane.

- Whether the per-rig pin is what the operator meant, or whether
  `DEFAULT_A` should move for every host. The ruling says "on the hermetic
  rig", so only the rig moved.
- `test_r23_ab.py` now fails when `flake.lock`'s nixpkgs revision differs
  from `RCLONE_PIN`. That couples any lock update to this file. It is the
  only check of Q105 that runs without Nix; drop it if the coupling is not
  wanted.
- An rclone override on under-load samples is refused too. Q105 says "every
  sample"; say so if under-load on neo should stay free.
- The `aarch64-darwin` store path is evaluated, not built. A gated sample on
  PZM or neo will be refused if the real path differs.
- Questions 1 and 2 of the second 2026-10-07 slo.md amendment (a load bound
  scaled to the rig; flushed native against unflushed rclone) are still
  open.
- mbp-13 is shared with the wp0g-ledger-sync lane. Nothing on the host
  stops two lanes timing at once; this lane looked before each run and
  still overlapped with a build twice.
