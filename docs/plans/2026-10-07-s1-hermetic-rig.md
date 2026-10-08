# S1 hermetic rig: runbook (2026-10-07)

Rulings: OI-1003-Q96 (the rig), OI-1003-Q97 (gate of record), OI-1003-Q99
(gated gate (a) on mbp-13), OI-1003-Q103 (the rig's A control), OI-1003-Q105
(the rig's rclone), OI-1002-Q30, OI-1002-Q28, R-N81, R-N13.
The SLO text is the three 2026-10-07 amendments in [../slo.md](../slo.md).

## Bounds

- Hosts: mbp-13 (gate (a); gate (b) destination), yoga and PZM (gate (b)
  sources). Not neo, not sting, not honey.
- Everything goes under `~/git-bulkload/` on the host, apart from what Nix
  itself puts in `/nix/store`. That holds only with the three variables of
  section 1 exported first. Without `TMPDIR`, every `nix develop` call
  leaves a `nix-shell.*` and a `nix-develop-*` directory in `/tmp`, which is
  tmpfs (RAM) on mbp-13. Without `XDG_CACHE_HOME`, any `nix` command,
  `nix --version` included, writes `~/.cache/nix/sentry/` on a Determinate
  Nix host. The first lane did both (see its agent note).
- A read-only probe of another host runs no `nix` command at all.
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
  tmp/                      TMPDIR (nix develop's temporary directories)
  runs/r23-ab-<stamp>/      one work root per sample (never reused)
  logs/                     detached run logs
```

## 1. Set up the host

```
mkdir -p ~/git-bulkload/logs ~/git-bulkload/runs ~/git-bulkload/build ~/git-bulkload/tmp
export CARGO_HOME=~/git-bulkload/cargo-home XDG_CACHE_HOME=~/git-bulkload/xdg-cache TMPDIR=~/git-bulkload/tmp
git clone https://github.com/Jesssullivan/bulkload ~/git-bulkload/bulkload
cd ~/git-bulkload/bulkload && git fetch origin && git checkout --detach <sha>
```

Every command, the first probe included, runs with that `export` line in
effect. A new ssh session starts without it: export it again.

Check the host first, and record the answers in the agent note: the Nix
version, `cat /proc/loadavg`, free space under `~/git-bulkload/`, the CPU
topology and the power state the bench will read. Take the Nix version
without running `nix`; the store path names it:

```
readlink -f "$(command -v nix)"
lscpu | grep -E 'Model name|Socket|Core|Thread'
for d in /sys/class/power_supply/*; do echo "$d $(cat $d/type) $(cat $d/online 2>/dev/null) $(cat $d/status 2>/dev/null)"; done
```

Physical cores and logical CPUs are different numbers (mbp-13: 2 and 4).
Record both.

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

## 3. rclone (OI-1003-Q105)

Every sample, on every host, runs the rclone build that the repo flake pins:
`nix build --inputs-from <repo> nixpkgs#rclone`, which is the nixpkgs of the
clone's `flake.lock`. Do not pass `--rclone`. `RCLONE_PIN` in `r23_ab.py`
records that build:

| | |
|---|---|
| Version | `rclone v1.74.4` |
| nixpkgs revision (`flake.lock`) | `241313f4e8e508cb9b13278c2b0fa25b9ca27163` |
| Store path, `x86_64-linux` (mbp-13, yoga) | `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4` |
| Store path, `aarch64-darwin` (PZM, neo) | `/nix/store/5aw5dn7z12pnwjp1yghg9xar9bcl21fk-rclone-1.74.4` (evaluated, not yet built by a rig lane) |

What the harness does with it:

- A gated or under-load sample is refused (exit 2, before anything is
  written) when `--rclone` names any binary but the flake-pinned one. A
  distribution rclone (`/usr/bin/rclone`) is such a binary.
- A gated sample is also refused when the flake resolves to a build that is
  not `RCLONE_PIN` for the host's system. That is what a `flake.lock` update
  does when it moves rclone.
- A rep whose bench header names another `rclone_version` aborts the sample.
- Every report records `rclone_record`: the store path, the version, the
  binary's sha256, the flake's nixpkgs revision, the committed pin and
  whether they match. The evidence has the same in its `rclone:` line, and
  the status line says `rclone_pinned=true|false`.

A deliberate exception is `--rclone <path> --rclone-override-reason "<why>"`.
The reason is recorded, the verdict becomes `PASS-RCLONE-OVERRIDE` or
`FAIL-RCLONE-OVERRIDE`, and `of_record` is false. It is for measuring another
rclone, never for a verdict.

To bump rclone: update `flake.lock`, set `RCLONE_PIN` (version, nixpkgs
revision, each store path, the date) in the same change, and add a dated
line to docs/slo.md. `test_r23_ab.py` fails while `RCLONE_PIN`'s nixpkgs
revision differs from `flake.lock`. Samples on either side of a bump are not
compared as one series.

## 4. Prebuild, then let the host go quiet

No compile runs inside a gated sample. Build first, with the harness's own
`build()`, into the same `--build-root` the sample will use:

```
cd ~/git-bulkload/bulkload
nice -n 10 nix develop .#default --command python3 crates/bulkload-bench/scripts/r23_ab.py \
  --build-only --work-root ~/git-bulkload/runs/build-$(date -u +%Y%m%d-%H%MZ) \
  --build-root ~/git-bulkload/build --rev-b <sha> \
  > ~/git-bulkload/logs/build-<sha>.log 2>&1
```

It prints one `built label=... compiled=true|false` line per revision and
`status=built-not-a-sample`. On mbp-13 it builds the rig's pinned A without
being told (section 5). Then wait until `cat
/proc/loadavg` shows the host back at its idle load (mbp-13 idles at 0.00
to 0.10). R-N81's bound is load1 below 2.5 and the harness enforces only
that; on a 2-core host that bound alone would admit a rep right after a
build, which is what happened in the first sample (rep 0 at load1 2.03).
The evidence states what was compiled inside the sample and the load after
the builds, so a sample that skipped this step shows it.

## 5. Run gated gate (a)

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
- The harness finds B, A and v4 prebuilt (section 4), waits up to
  `--settle-seconds` for AC power and load1 below 2.5, then runs B/A/B/A/B
  and one v4 rep. It checks
  the host before and after every rep; the bench checks before every arm.
- Do not pass `--rev-a` or `--rclone`. A is the rig's pinned control (below)
  and rclone is the flake-pinned build (section 3); the harness refuses
  anything else in a gated sample on the rig.
- Before starting, check that nothing else is building or benchmarking on
  the host: `uptime`, `pgrep -a cargo`, `pgrep -a rustc`, and `pgrep -a
  bulkload` (another lane's bench binary may carry a suffix). Look only;
  never signal a process. If something runs, wait and look again.
- On Linux, B must answer `bulkload-bench preflight` with
  `power_probe=sysfs`. A and v4 run behind the `pmset` translator; the
  report marks them `pmset-shim`.
- Poll the log for the line `r23-ab status=... rig=... rig_role=...
  of_record=... json=... evidence=... gate=...`. Exit codes: 0 complete (the
  verdict is PASS or FAIL), 2 refused before the sample, 3 aborted during
  it, 4 build failure.
- `rig_role` comes from `RIG_OF_RECORD` in `r23_ab.py`: `record` on mbp-13,
  `field` on every other host. Only a sample with `of_record=true` is a
  verdict of record (gated, a `record` rig, with its A control). A gated
  sample is refused when the host's identity cannot be read in full.
- The native arm's seal on Linux is `fsync`, with a device cache flush; the
  rclone arm syncs nothing. The bench header prints `seal_primitive`. Read
  the result with that in mind (slo.md, the later 2026-10-07 amendment).
- OI-1003-Q107: after each rclone copy the bench runs one `syncfs` of the
  destination outside the timed window (`rclone_sync` lines), which also
  leaves the next arm none of rclone's dirty pages to flush. The bench's
  `rclone_synced` lines and the evidence's "Equal durability" table add it
  back: native against rclone made equally durable. That comparison is
  informational; the verdict stays against rclone as shipped.

### The A control (OI-1003-Q103)

A is a fixed commit run between the B reps. B changes with main; A does not.
So a change in A's numbers, inside one sample or from one sample to the
next, is a change in the rig, not in the code. That is all A is for here:
it shows drift of the rig. It never decides the gate (OI-1002-Q30).

On mbp-13, A is `3931471738cc3995a0e564e73af3134d7a7f1ff2`: main at the merge
of #195, the last main commit before the rig work of 2026-10-07. It is
`RIG_BASELINE_A["mbp-13"]` in `r23_ab.py`, with its date and reason. It
replaces `7c3ecc7` there, which refuses with a bare `IO (errno 32)` on this
host (the counts are in the slo.md amendment of OI-1003-Q103). It predates
the Linux power preflight, so it runs behind the `pmset` translator, like
v4. A gated sample on the rig with any other `--rev-a` is refused; the pin
changes with the SLO text. Other hosts keep `7c3ecc7`.

When the pin changes, A's numbers before and after are different series.
Say so in the evidence of the first sample after a change.

`--b-only-no-a-control --no-a-control-reason "<why>"` still exists for a rig
where no pinned A can run. The order becomes B/B/B, every other gated check
stays, and the verdict is the single token `PASS-NO-A-CONTROL` or
`FAIL-NO-A-CONTROL` with `of_record=false`. It is not how mbp-13 samples
any more.

One sample is one sample. A FAIL is a result: do not tune and do not rerun
for a better one. A refused or aborted sample has no verdict; record why.

## 6. Evidence

- `~/git-bulkload/runs/r23-ab-<stamp>/r23-ab.json`: everything, including
  `host_identity` and each build's `preflight`.
- `docs/evidence/r23-<stamp>-<rig>-<role>.md` in the host's clone: the
  Markdown draft (`...-<role>-no-a-control.md` for a B/B/B sample). A gated
  evidence name without the rig's name is refused.
- Copy both into the lane's `docs/evidence/` under that same name (`.md`
  and `.json`), commit them as drafts, and put the verdict and the
  medians on TIN-4543. The work root stays on the host as the raw record.

## Gate (b) on the rig

Not runnable yet: the native multi-stream arm is W5. When it is, gate (b)
runs twice into mbp-13: from yoga, and from PZM. PZM needs room first.
