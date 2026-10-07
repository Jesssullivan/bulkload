# 2026-10-07: s1-hermetic-rig lane

Rulings: OI-1003-Q96 (hermetic rig), OI-1003-Q97 (gate of record),
OI-1003-Q99 (gated gate (a) on mbp-13), OI-1002-Q30, R-N81, R-N13. The
rulings are recorded on TIN-4543 (comment of 2026-10-07 about 02:10 EDT).
Branch `feat/s1-hermetic-rig-20261007`, from main `3931471`, merged with
main `95f43dc`. Commits `9d01b68`, `ef3fd75`, merge `3e7b5bf`, then the
evidence commit. No PR opened.

## What changed

- `crates/bulkload-bench/src/main.rs`: the R-N81 preflight reads
  `/sys/class/power_supply` on Linux (`linux_power`), `pmset` on macOS, and
  `unknown` elsewhere. `bulkload-bench preflight` prints the reading and its
  probe. The header gains `power_probe`, `os` and `arch`.
- `crates/bulkload-bench/scripts/r23_ab.py`: gated and under-load modes run
  on Linux. `--corpus` is required there (no `/Volumes` default). The report
  carries `host_identity` (platform, kernel, machine, product, CPU model,
  cores, RAM, work-root filesystem type, power probe and supplies).
- The Linux power rule, the same in both: `ac` when a `Mains` supply is
  online; else `battery` when a `Battery` or `UPS` supply exists; else
  `unknown` when a supply's type is unreadable or the class cannot be
  listed; else `ac` (no battery exists). Peripheral supplies (`scope` =
  `Device`) are ignored. `USB` and other types never prove AC. Both sides
  are tested over the same 13 fake sysfs trees.
- Baseline arm on Linux: A (`7c3ecc7`) and v4 (`41bf9a4`) run `pmset` from
  PATH. They are built at their exact sha, unpatched, and run with
  `<work>/pmset-shim/pmset` first on PATH; it reports the `linux_power`
  state in pmset's words, or exits 1 when unknown. B never runs behind it:
  a B without the sysfs probe is refused. The verdict rule is unchanged.
  The bench is one process that links the agent as a library, so "A's agent
  with B's bench" was not available.
- A on Linux is broken beyond its preflight (found on mbp-13, 06:30Z, log
  `~/git-bulkload/logs/diag.log`): native-only, informational, no shim.
  A refused with `IO (errno 32)` in 3 of 3 runs on the dry-run synthetic
  corpus (before the first sample) and in 1 of 3 runs on a copy of corpus
  v1 (after two samples). v4 completed 6 of 6. A full A rep makes about
  twice as many native copies, and a gated sample aborts on any refused
  rep. So the harness gained `--b-only-no-a-control` (with
  `--no-a-control-reason`): gated B/B/B, A not built, status
  `complete-draft-no-a-control`, verdict `PASS|FAIL (NO A CONTROL)`,
  evidence name must contain `no-a-control`. The default gated path and its
  verdict rule are unchanged.
- `docs/slo.md`: amendment 2026-10-07 (Q96, Q97). Runbook:
  `docs/plans/2026-10-07-s1-hermetic-rig.md`.
- `gate_b.py` and the justfile are untouched. The justfile comment above
  `bench-r23-ab` still says "Gated runs only on neo" (the justfile belongs
  to the ci-slim-source-gate lane).

## Validation

- `just check-fast` in `nix develop`, on sting: exit 0 at `9d01b68`,
  `ef3fd75`, the merge `3e7b5bf` and the evidence commit.
- `python3 crates/bulkload-bench/scripts/test_r23_ab.py`: 55 tests pass
  (it is in `check-optional`, not `check-fast`).

## Rig probes (2026-10-07, about 06:00Z)

| Host | Platform | Load1 | Free | Power as read | nix | rclone |
|---|---|---|---|---|---|---|
| mbp-13 | Linux 6.19.5 x86_64, MacBookPro12,1, i7-5557U, 4 cores, 16 GB, XFS | 0.01 | 693G on `/home` | `ADP1` Mains online=1, `BAT0` Full: ac | Determinate 3.21.9 (2.34.8) | `/usr/bin/rclone` v1.71.2 (distribution); none in the Nix profile |
| yoga | Linux 6.19.5 x86_64, i7-8550U, 8 cores, 16 GB | 0.56 | 51G on `/` | `ADP0` Mains online=1, `BAT0` Not charging: ac | Determinate 3.22.3 (2.35.2) | `~/.nix-profile/bin/rclone` v1.75.1 |
| PZM | Darwin 25.6.0 arm64 | 1.59 | 13Gi on `/System/Volumes/Data` | pmset: AC Power | Determinate 3.23.0 (2.35.2) | `/usr/local/bin/rclone` v1.68.2 |

yoga and PZM were only read. Link speed between the hosts was not measured.
The three hosts have three different rclone versions on PATH; the harness's
default, the flake-pinned `nixpkgs#rclone`, resolved to v1.74.4 on mbp-13.

## mbp-13 setup (all under `~/git-bulkload/`)

- Clone at `~/git-bulkload/bulkload`; `CARGO_HOME` and `XDG_CACHE_HOME` under
  `~/git-bulkload/`. Nix wrote to `/nix/store`, which cannot be avoided.
- Corpus generated, sealed and verified at `~/git-bulkload/r23-corpus-v1/corpus`:
  identity `f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`.
- Baselines prebuilt with the harness's own `build()`:
  A `7c3ecc714e38` sha256 `94ab3dd4304a0a6b...`, v4 `41bf9a4fa233` sha256
  `a5acdebb85ef9374...`.

## The gated sample (one sample, 2026-10-07 06:52Z to 06:56Z)

**Verdict as printed: `status=complete-draft-no-a-control gate=FAIL (NO A CONTROL)`.**
It is a B/B/B sample without its A control, so it is reported, not a gate
verdict of record (see Open). Nothing was tuned and nothing was rerun.

- B = `3e7b5bf` (this branch after merging main `95f43dc`). `git diff
  origin/main 3e7b5bf -- crates/bulkload-agent crates/bulkload-proto` is
  empty; the tree ids match (`45e81a4d` and `29be8d7b`).
- Host: mbp-13, Linux 6.19.5-11 x86_64, MacBookPro12,1, i7-5557U, 4 cores,
  15.3 GiB, XFS, power probe sysfs (`ADP1` online, `BAT0` Full). Every row
  `power=ac gated=true`; load1 before the reps 2.03, 1.87, 1.82, 1.78.
- rclone: flake-pinned `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4/bin/rclone`, v1.74.4.
- Bench priority `background`, durability `group`. The run was started
  under `nice -n 10`, which both arms inherit.
- Corpus v1 verified before and after, identity `f4a7619f...`.

| B rep | verdict | initial native / rclone ms | delta native / rclone ms | warm zero | interrupted zero | native peak RSS |
|---:|---|---:|---:|---|---|---:|
| 0 | fail | 872.965 / 367.993 | 132.117 / 115.670 | true | true | 48.9 MiB |
| 1 | fail | 914.173 / 361.784 | 135.596 / 105.634 | true | true | 50.0 MiB |
| 2 | fail | 854.826 / 358.996 | 141.215 / 105.065 | true | true | 49.3 MiB |

- Median of the rep medians: initial 872.965 ms native against 361.784 ms
  rclone (2.41 times slower); delta 135.596 ms against 105.634 ms (1.28
  times slower).
- Warm resume and interrupted resume: 0 bytes received and 0 source bytes
  read in every rep.
- Seal time (the sum of `flush_*_ns`) is 64 % of the native initial wall time
  (median share 0.639).
- v4 reference (behind the pmset translator): 1,174.875 ms initial,
  219,946,368 bytes received; B's duplicate share is 0.0829.
- Evidence: `docs/evidence/r23-2026-10-07-0652Z-no-a-control.json` (sha256
  `9f53da43...`, byte-identical to the host's `r23-ab.json`) and `.md`.
  Raw record on mbp-13: `~/git-bulkload/runs/r23-ab-20261007-gate-a/` and
  `~/git-bulkload/logs/r23-ab-20261007-gate-a.log`.
- Also on mbp-13, not samples: `runs/dryrun-9d01b68` (a dry run, aborted
  when A refused), `runs/diag-baselines` (the A and v4 check) and
  `prep.sh`, `smoke.sh`, `diag.sh`, `gate-a.sh`. The smoke script stopped
  at the dry run, so the A-behind-the-translator check never ran on a real
  A binary; the translator did run for v4 in the sample.

## Open

- Not posted to TIN-4543 by this lane; the coordinator owns that comment.

- Operator: the A control on the rig. Accept B/B/B, pin a newer A that
  runs on Linux, or record a refused A rep without aborting.
- Operator: ratify the pmset translator for baselines that only know pmset
  (v4 runs behind it today).
- Operator: mbp-13 has no Nix-profile rclone; the sample uses the
  flake-pinned one (v1.74.4). Say if a profile rclone is wanted instead.
- `gate_b.py` still reads `unknown-no-supply-class` for a host with no
  `Mains` supply; it should take the same rule before gate (b) runs on the
  rig.
- The justfile comment (other lane) and gate (b) on the rig (W5).
