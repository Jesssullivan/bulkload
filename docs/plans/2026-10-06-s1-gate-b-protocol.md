# S1 gate (b) protocol: neo→sting pull, native vs rclone over sftp

Status: harness built, never run against neo. Operator ruling OI-1003-Q66 lifts
the hold on building the harness only; no neo run happens until gate (a)
passes. Rulings: OI-1002-Q30, OI-1003-Q3, OI-1003-Q19, OI-1003-Q66, R-N81,
R-N91, R-N13. The harness is `crates/bulkload-bench/scripts/gate_b.py`
(`just bench-gate-b`) and its tests are `test_gate_b.py`.

## What gate (b) claims

S1 gate (b) in [docs/slo.md](../slo.md) says a neo→sting pull beats rclone over sftp.
OI-1003-Q3 adds that there is no wall-clock SLA for the migration run itself.
The gate is the comparison alone. The harness reports how long each arm took,
but no number of seconds passes or fails it.

## Hosts and transport

- **Where it runs.** The harness runs on the destination, sting. The source,
  neo, is reached only over ssh, with one fixed command line:
  `<ssh> [-F CONFIG] -T -oBatchMode=yes -oConnectTimeout=15 -- HOST <command>`.
  The native pull and the rclone arm (in its default `external` mode) use this
  same OpenSSH client, config and cipher.
- **What runs on the source.** Only these four things:
  - **The helper.** A standard-library Python program, sent on stdin to
    `<source-python> -I -`. Its arguments are embedded in the program text, so
    no argv carries them. It reads load and power, hashes trees, makes the
    harness's own working copy, and applies and reverts the delta in that copy.
  - **Link calibration.** `dd if=/dev/zero`; the bytes are discarded.
  - **Corpus check.** The repository's `estate_corpus.py verify`, run on the
    sealed corpus.
  - **The arms' own servers.** `bulkload-agent serve`, started by `pull`, and
    the sftp subsystem, started by rclone.
- **What it never does.** It never writes the sealed corpus, never deletes on
  the source, and never signals a process on either host.

## Workload

- **The corpus.** The sealed estate corpus (#159, OI-1003-Q19). Gated mode
  needs:
  - scale `estate`;
  - the recorded identity, `586de100…`;
  - `readonly` sealed, with no mutation rounds;
  - `estate_corpus.py verify` exiting 0 on the source.
- **The working copy.** The helper copies the corpus once into a new
  `--source-work` and makes it writable. Both arms read this copy.
- **The comparable set.** The working copy minus the seats that `pull` refuses
  by design with `SQLITE_STATE_CHANGED`:
  - SQLite databases, found by their magic bytes;
  - WAL images, found by their magic bytes;
  - `-wal`, `-shm` and `-journal` companions, found by name.

  rclone gets the same seats as an anchored, escaped `--exclude-from` list. The
  native arm must refuse exactly that set, with that code. A refusal outside
  the set, or an excluded seat the agent carried, aborts the sample. SQLite
  goes through `snapshot`, not a byte mover, so it is not part of gate (b).

## One rep

Gated mode runs 3 B reps. Gate (b) is a single-revision claim, so there is no
A arm.

1. **Host gate.**
   - The source must be on AC power with load1 < 2.5 (R-N81).
   - The destination must have load1 < `--dest-load-limit` (default 2.5) and
     must not be on battery.
   - Both hosts are checked again before every arm. A host that has not settled
     within `--arm-settle-seconds` aborts the sample.
2. **Link calibration (#47).** `dd` of 64 MiB, once over one stream and once
   over 4 streams. Every arm reports its throughput as a share of the better
   ceiling: `link_fraction`.
3. **Initial copy.** The arms run N/R/N/R/N, each into a new destination. Each
   native arm gets new source and destination states. After every arm, the
   destination is hashed. It must hold every comparable regular file (size and
   SHA-256) and every directory. Symlink, mode and extra-entry differences are
   recorded as fidelity notes.
4. **Warm resume.** The first native arm is pulled again with nothing changed.
   R25 expects 0 bytes received and 0 content bytes read. This is recorded but
   does not gate.

   Content bytes read are `source_bytes_read` minus the SQLite magic probes.
   The agent reads up to 16 bytes of every SQLite seat it refuses by magic,
   on every pass, so that probe is netted out.
5. **The 1 % delta.**
   - The helper XORs 0xa5 over 1 % of the comparable regular-file bytes. It
     takes whole files in a seeded path-hash order, and the last file only up
     to the remaining bytes.
   - The harness waits out the 2 s racy window (R-N76).
   - Outside the timing, it removes those files from every arm's destination.
     The agent never clobbers an output, and the bench does the same.
   - The arms run N/R/N/R/N again into the same destinations.
   - The helper then XORs again, which restores the bytes, and the working copy
     must hash back to its initial manifest.
6. **After the rep.**
   - The source must still be on AC power.
   - Load1 on both hosts must fall under the limits within
     `--post-settle-seconds`.

## Verdict

A rep **passes** when all of these hold:

- the native median beats the rclone median for the initial copy;
- the native median beats the rclone median for the delta;
- every native arm stays under 2 GiB of RSS, counting both the `pull` (wait4)
  and the source `serve`, which the wrapper reports;
- every arm verified;
- every sample was gated.

Gate (b) **passes** only when all 3 B reps pass (the OI-1002-Q30 shape).

In under-load and dry-run modes, every rep is `informational`, so neither mode
can produce a gate verdict.

## Native arm

The native arm is `bulkload-agent pull HOST SOURCE DEST SOURCE_STATE DEST_STATE
REMOTE_EXECUTABLE [SSH_CONFIG]`. It runs one `ssh -T` stream, which is the
pre-W5 engine.

REMOTE_EXECUTABLE is a wrapper that the helper writes into the working copy's
`bin/`. The wrapper:

- runs the source agent;
- waits for it;
- prints `gate_b_serve max_rss_bytes=N`.

The source half takes the WP0(f) background class by default (OI-1003-Q25). The
explicit `--source-priority normal` passes `--priority=normal` instead.

The harness refuses the native arm with `NATIVE_REMOTE_ARM_MISSING`, "native
remote arm missing (#47)", and exits 2 in two cases:

- the local agent has no `pull`/`serve` pair;
- `--native-streams` asks for W5's N-stream pull.

## Against W5

W5 is the acceptance owner of gate (b). As of this harness, the agent lacks
these W5 pieces:

- **N parallel streams.** `pull` opens one `ssh -T` session to one `serve`.
  There is no stream-count flag and no SCM_RIGHTS rendezvous.
- **Wire compression.** There is no adaptive zstd-1: wire v5 has no compressed
  data frame.
- **`Ref` dedup frames.** A chunk already sent in the session is sent again,
  unless the destination fills it through WantManifest/NeedChunks.
- **`NeedRanges`.** NeedChunks names whole chunks by index, not byte ranges.
- **A source-side RSS line.** `serve` prints no max RSS, so the harness wraps
  it.
- **A `--priority` hand-off from `pull` to `serve`.** Only REMOTE_EXECUTABLE
  can add the flag.
- **An exclude or SQLite hand-off.** The estate corpus's SQLite seats are
  refused, so they stay out of the comparison.

The harness measures the pre-W5 engine today. It will measure W5 unchanged once
`pull` grows streams, except for the `--native-streams` refusal, which W5
replaces with the real flag.

## rclone arm

The rclone arm is `rclone copy` with the gate (a) flags:

- `--create-empty-src-dirs`, `--links` and `--metadata`;
- `--transfers 4` and `--checkers 4`;
- `--stats 0` and `--log-level ERROR`.

It reads from an sftp remote defined in a private `rclone.conf` (mode 0600).
Both transports set `skip_links = true`. rclone over sftp follows symlinks:
the loopback smoke turned a symlink into a copy of its target. The estate
corpus has dangling, looping and directory symlinks, so following them would
fail or duplicate trees. With `skip_links`, rclone carries no symlinks. Those
show up as fidelity notes, never as a mandatory miss. That remote is built in
one of two ways:

- **`external` (the default).** The remote's `ssh` is the native arm's OpenSSH
  command line. `shell_type = none` and `disable_hashcheck = true` stop rclone
  from opening a new ssh connection for every file hash. With hash checks off,
  rclone does less verification work than bulkload, so the comparison is
  conservative for the native arm.
- **`internal`.** rclone uses its own ssh, configured from `ssh -G`:
  hostname, user, port, `known_hosts_file`, and either ssh-agent or
  `key_file`. A host with no known_hosts file is refused with
  `HOST_KEY_UNVERIFIED`.

No secret reaches the config, argv, logs or JSON:

- the config is refused if any of its keys is a password, PEM or token key;
- every inherited `RCLONE_*` variable is dropped;
- logs and the JSON pass through `redact`;
- `validate_report` flags any secret-shaped string left behind.

## Modes and outputs

- **Gated (default).**
  - Needs `--coordinator-quiet` (R-N91).
  - Runs 3 reps of N/R/N/R/N at scale `estate`.
  - Writes its evidence draft to `docs/evidence/s1-gate-b-<stamp>.md`.
- **`--under-load`.**
  - Informational. It follows OI-1003-Q39 and Q50 by analogy, pending an
    operator ruling for gate (b).
  - The load gates are lifted, and load is still recorded.
  - Source AC power is still required.
  - Its evidence must be named `*underload*`.
- **`--dry-run`.** A loopback smoke on one host:
  - a shim plays ssh;
  - the local `sftp-server` serves rclone;
  - a synthetic corpus with three SQLite seats stands in for the estate corpus;
  - nothing is gated;
  - the output says NOT A GATE SAMPLE, and its evidence never goes under
    `docs/evidence`.
- **Outputs.** `WORK/gate-b.json` (format `bulkload-s1-gate-b-v1`) and redacted
  logs in `WORK/logs/`.
- **Exit codes.** 0 complete, 2 refused, 3 aborted.

## Not done here

- **No neo run.** OI-1003-Q66: none until gate (a) passes.
- **No arm timeout.** A hung arm is left for the operator. The harness never
  signals a process.
- **Calibration measures ssh, not the disk.** It sends `/dev/zero` with
  compression off. It is the ssh link ceiling, not sftp's.
