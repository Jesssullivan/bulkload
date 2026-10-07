# S1 hermetic rig: runbook (2026-10-07)

Rulings: OI-1003-Q96 (the rig), OI-1003-Q97 (gate of record), OI-1003-Q99
(gated gate (a) on mbp-13), OI-1002-Q30, OI-1002-Q28, R-N81, R-N13.
The SLO text is the 2026-10-07 amendment in [../slo.md](../slo.md).

## Bounds

- Hosts: mbp-13 (gate (a); gate (b) destination), yoga and PZM (gate (b)
  sources). Not neo, not sting, not honey.
- Everything goes under `~/git-bulkload/` on the host. Nothing else is
  written, apart from what Nix itself puts in `/nix/store`.
- No sudo. No service, dotfile or printer change (mbp-13 runs the print
  service). Builds run at `nice -n 10`.
- A gated sample is started detached with its log under `~/git-bulkload/`
  and polled through its log file. Nothing else outlives the session.

## Layout

```
~/git-bulkload/
  bulkload/                 clone of Jesssullivan/bulkload
  r23-corpus-v1/corpus/     sealed R23 corpus v1 (0444/0555)
  build/                    --build-root: per-revision target dirs and binaries
  cargo-home/  xdg-cache/   CARGO_HOME and XDG_CACHE_HOME for the builds
  runs/r23-ab-<stamp>/      one work root per sample (never reused)
  logs/                     detached run logs
```

## 1. Set up the host

```
mkdir -p ~/git-bulkload/logs ~/git-bulkload/runs ~/git-bulkload/build
git clone https://github.com/Jesssullivan/bulkload ~/git-bulkload/bulkload
cd ~/git-bulkload/bulkload && git fetch origin && git checkout --detach <sha>
```

Every later command runs with:

```
export CARGO_HOME=~/git-bulkload/cargo-home XDG_CACHE_HOME=~/git-bulkload/xdg-cache
```

Check the host first, and record the answers in the agent note: `nix
--version`, `cat /proc/loadavg`, free space under `~/git-bulkload/`, and the
power state the bench will read:

```
for d in /sys/class/power_supply/*; do echo "$d $(cat $d/type) $(cat $d/online 2>/dev/null) $(cat $d/status 2>/dev/null)"; done
```

## 2. Generate, verify and seal the corpus

```
cd ~/git-bulkload/bulkload
nix develop .#default --command python3 crates/bulkload-bench/scripts/r23_corpus.py generate ~/git-bulkload/r23-corpus-v1
nix develop .#default --command python3 crates/bulkload-bench/scripts/r23_corpus.py seal ~/git-bulkload/r23-corpus-v1
nix develop .#default --command python3 crates/bulkload-bench/scripts/r23_corpus.py verify ~/git-bulkload/r23-corpus-v1/corpus
```

`verify` must print `identity=f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`
and exit 0. The harness verifies it again before and after the sample and
refuses any other corpus.

## 3. rclone

The harness's default is the rclone that the repo flake pins
(`nix build --inputs-from <repo> nixpkgs#rclone`), which is the same build on
every rig host. Pass `--rclone` only to name another Nix-built rclone by its
absolute path. Do not use a distribution rclone (`/usr/bin/rclone`). The
bench records `rclone_version` in every header.

## 4. Run gated gate (a)

`--rev-b` is the commit under test; state how its `crates/bulkload-agent`
and `crates/bulkload-proto` relate to main (`git diff --stat origin/main
<sha> -- crates/bulkload-agent crates/bulkload-proto`).

```
cd ~/git-bulkload/bulkload
stamp=$(date -u +%Y%m%d-%H%MZ)
nohup nice -n 10 nix develop .#default --command python3 crates/bulkload-bench/scripts/r23_ab.py \
  --corpus ~/git-bulkload/r23-corpus-v1/corpus \
  --work-root ~/git-bulkload/runs/r23-ab-$stamp \
  --build-root ~/git-bulkload/build \
  --rev-b <sha> --coordinator-quiet \
  > ~/git-bulkload/logs/r23-ab-$stamp.log 2>&1 &
```

- `--coordinator-quiet` states that nothing else is running on the host
  (R-N91). On the rig that is the operator's or coordinator's statement
  about mbp-13, not about sting's lanes.
- The harness builds B, A and v4, waits up to `--settle-seconds` for AC
  power and load1 below 2.5, then runs B/A/B/A/B and one v4 rep. It checks
  the host before and after every rep; the bench checks before every arm.
- On Linux, B must answer `bulkload-bench preflight` with
  `power_probe=sysfs`. A and v4 run behind the `pmset` translator; the
  report marks them `pmset-shim`.
- Poll the log for the line `r23-ab status=... gate=...`. Exit codes: 0
  complete (the verdict is PASS or FAIL), 2 refused before the sample, 3
  aborted during it, 4 build failure.

If the pinned A cannot run on the host (on mbp-13 it refuses with `IO
(errno 32)` on its own; see the slo.md amendment), add
`--b-only-no-a-control --no-a-control-reason "<why>"`. The order becomes
B/B/B, every other gated check stays, and the verdict is marked
`(NO A CONTROL)`. It is reported, not a gate verdict of record, until the
operator rules.

One sample is one sample. A FAIL is a result: do not tune and do not rerun
for a better one. A refused or aborted sample has no verdict; record why.

## 5. Evidence

- `~/git-bulkload/runs/r23-ab-<stamp>/r23-ab.json`: everything, including
  `host_identity` and each build's `preflight`.
- `docs/evidence/r23-<stamp>.md` in the host's clone: the Markdown draft
  (`r23-<stamp>-no-a-control.md` for a B/B/B sample).
- Copy both into the lane's `docs/evidence/` (`r23-<stamp>.md` and
  `r23-<stamp>.json`), commit them as drafts, and put the verdict and the
  medians on TIN-4543. The work root stays on the host as the raw record.

## Gate (b) on the rig

Not runnable yet: the native multi-stream arm is W5. When it is, gate (b)
runs twice into mbp-13: from yoga, and from PZM. PZM needs room first.
