# The native mover

Status: **design + prototype, default off.** `--mover rsync` remains the
shipped transport and the default. Nothing in this document has been proven
against a live destination.

Provenance: this exists because of the 2026-08-28 nine-lane adversarial review
(`bulkload-boundary-20260824/product-review/VERDICTS.md`). Lane A's load-bearing
finding is that **bulkload does not move bytes — rsync does**, and Lane F's is
that **there is no transport-side dedup anywhere in the architecture**. Both
are correct against the code, and both are addressed here.

---

## 0. What this document is *not* allowed to claim

The brief that produced this work asked to "earn the *supersedes rclone*
thesis". Before any number appears, the record's own fence:

* `rclone` is not a comparator anywhere in the estate. `git log --all -S rclone`
  in `Jesssullivan/bulkload` returns zero hits; the single estate-wide
  occurrence names rclone as a macFUSE *user*
  (`BASES-20260828.md:718-729`). The written "beat the incumbent" bar is
  **odrive parity, dated 2026-05-06, and it belongs to TCFS, not bulkload**
  (`tummycrypt origin/main:docs/ops/odrive-parity-product-horizon.md:1-20`,
  quoted at `BASES-20260828.md:664-693`).
* Supersession is fenced twice as a *shape of claim*:
  `prompts-enqueue/prompts/60-remote-dev-fs-research.md:76-77` — "SSHFS-/
  Syncthing-class systems are **comparison baselines, not quiet replacements**"
  — and `prompts/47:50-51` on APFS-only evidence
  (`BASES-20260828.md:735-750`).
* Bulkload's own charter (Linear TIN-3268, 2026-07-29T21:06:07Z, quoted at
  `BASES-20260828.md:751-774`) says "first-class product module, not a one-off
  script". **It names no comparator and does not say "public."**

So the sanctioned form is a parity table plus a named differentiation, and that
is what §5 is. The honest positioning the judge reached, verbatim
(`VERDICTS.md`, judge §1), is *"a git-and-agent-state correctness layer you run
alongside your mover."* This mover does not change that sentence. It removes
one specific, measured refutation from underneath it: that the transport is a
single stream with no dedup and no resume.

---

## 1. What the shipped transport does, in code

| Fact | Citation |
|---|---|
| The payload push is one `rsync` process | `executor.py:642-661` |
| Flags: `-a --from0 --files-from=- --delay-updates --ignore-missing-args --no-devices --no-specials` | `executor.py:644-656` |
| Mode string recorded in the receipt | `executor.py:668-670` |
| A GNU rsync ≥ protocol 30 is pinned by SHA-256 | `scanner.py:158-200` (`inspect_rsync`) |
| `--rsync-path` is `required=True` | `cli.py:452` |
| The allowlist is a **path** list, built with no digest filter | `executor.py:213-268` (`_plan_source_paths`) |

There is no `-P`, no `--partial`, no compression, no parallelism, no
`--track-renames`. The measured consequences, from the review:

* **9.2 MB/s effective** on the ceremony's own push
  (84,112,374,824 B / 152 min, `L1-full-delta-runbook.md:437`).
* **195 files/s** for ~1.78M paths against a 6 ms RTT (`VERDICTS.md`, LANE F).
* **27.93 GB (24.9%)** of that push was redundant against the source's own
  content: the plan's capacity block charges 112.04 GB and finds 84.11 GB
  unique, and rsync has no cross-file dedup to catch the difference
  (`VERDICTS.md`, LANE F; `planner.py:747` already computes the unique set).
* `logs/preseed-push.log` is **0 bytes** for a 2h32m / 84 GiB push
  (`VERDICTS.md`, judge §1).

---

## 2. The mover

`bulkload_lib/mover.py`. Its unit of work is the **object** — one sha256
identity — not the path.

```
  SENDER (source host)                        RECEIVER (destination, N of them)
  ────────────────────                        ─────────────────────────────────
  allowlist (NUL, streamed)
        │
        ▼
  plan_objects()                              python3 -I -S mover.py --receive
   ├ lstat each path                                    --root <quarantine>
   ├ digest (sealed, or hashed here)                    │
   ├ spill-sort by (digest,size,mode)          hello {protocol, source_sha256}
   └ group ⇒ MoveObject(paths[], digest)  ◄───────────── │
        │                                                │
        │ 1 header frame per object ────────────────────► │
        │                                       have?  ── size+mode+sha256 of
        │                                                  the landed primary
        │ ◄──────────────── {"status":"have"}  (skip, link aliases, next)
        │ ◄──────────────── {"status":"send"}
        │ raw payload, `size` bytes ──────────────────►  hash while writing
        │                                                 ├ digest mismatch ⇒ abort
        │ ◄──────────────── {"status":"ok"}               ├ fsync, chmod, rename
        │                                                 └ os.link() each alias
        ▼
  MoveSummary{objects, paths, bytes_sent, bytes_deduplicated, ...}
```

### 2.1 The five properties

**Content-addressed and deduplicated.** Paths that share a digest become one
`MoveObject` with N paths. The first is the representative and receives the
bytes; the rest are `os.link`ed at the destination. `bytes_deduplicated` in the
summary is exactly the wire saving. This is the direct answer to LANE F's
"there is no transport-side dedup" — the transport unit *is* the digest.

**Parallel.** N channels, each an independent ssh process, pulling from one
shared work iterator under a lock (`_Work`), so a slow object cannot idle the
other streams. Default N = 8.

**Resumable, with no run-scoped state.** The receiver proves each offered
object against the destination by kind, size, mode **and sha256** before
answering `have`. The destination stage is therefore the checkpoint: there is
no journal to reconcile, no partial-run file to trust, and a resume after any
failure — including SIGKILL — transfers only what is missing. The receiver's
temp file is named from the path rather than randomised, so a resume truncates
the previous partial rather than stranding it; this matters because
`validate_snapshot_custody` fails closed on an entry the sealed index does not
name.

**Resumable is not the same as cheap to resume, and today it is not cheap.**
That digest proof costs a full content read on *both* sides of a no-op resume.
Measured in §4: the native repeat pass is slower than rsync's and 14x slower
than rclone's despite moving zero bytes. §6.1 and §6.2 are the two halves of
the fix. Until they land this property is correct and expensive, and the
design says so rather than selling the word "resumable" on its own.

**Integrity is the existing sha256 identity.** Nothing new is invented. The
receiver re-hashes every byte it writes before publishing, and the sender
re-hashes what it read and refuses if the source moved under it. Both ends
catch a mid-stream change independently.

**Backpressure and heartbeat.** Backpressure is the protocol: a stream sends
exactly one object at a time and waits for `ok`, so a slow destination blocks
its own stream and nothing else. Liveness is a per-stream `Heartbeat` at most
every 15 s plus one at exit; `executor.py` prints them to stderr. A watchdog
aborts the run when no *live* stream has progressed for `stall_seconds`
(default 900) and kills the channels so a stream blocked in `read()` is
released.

### 2.2 What it deliberately does not do

Stated plainly, because the transport contract depends on it. The native mover
does **not** preserve mtimes, hardlink topology, xattrs, ACLs, or device and
special files, and it creates implicit parent directories at 0o700.

This is safe *for the quarantine* and only there. The destination re-derives
the quarantine from the sealed index under `required_paths`
(`executor.py:1533-1535` → `validate_snapshot_custody`), which compares exactly
nine fields per record — `destination_device, kind, method, mode,
relative_path, root_index, sha256, size, source_device`
(`scanner.py:3118-3145`, record built at `scanner.py:2414-2446`) — and, under
`required_paths`, disables the namespace/0o700 perimeter
(`scanner.py:2978-2986`). No dropped attribute is load-bearing there. A caller
outside the quarantine path must not assume otherwise.

One refusal is new: a source **directory** without owner write+execute is
rejected at plan time, before a byte moves, because the mover sets the source
mode on creation and would otherwise strand that subtree. `rsync -a` defers
directory permissions to the end of a run and has no such constraint. Measured
on two real corpora — `~/.claude/projects` (1,969 directories) and a bulkload
checkout (373) — the offending count is **0**, which is why this is a
documented refusal and not a second protocol phase.

### 2.3 The receiver is pinned, not trusted

`AGENTS.md` requires every command to bind the pinned application-source
closure, and `runtime_source_digest()` (`model.py:88-101`) does that for the
engine. The receiver is a *remote* process and there is no remote hashing tool
to lean on, so:

1. `bootstrap_receiver()` ships the exact bytes the sender is running, via one
   `ssh host 'umask 077 && cat > path.part && mv -f path.part path'`.
2. The receiver's first frame is `hello{protocol, source_sha256}` where the
   digest is of its own `__file__`.
3. The sender refuses any stream whose hello does not match, **before any
   object is offered**. Tested.

---

## 3. Fleet-synchronous landing hazard — read before deploying

Adding `mover.py` to the package changes `runtime_source_digest()`, because
`RUNTIME_SOURCE_NAMES` (`model.py:30-38`) must list it — the launcher will not
import a module it has not pinned (`bulkload.py:17-25`), and
`test_runtime_bootstrap.test_runtime_closure_is_exact` fails on any file in
`bulkload_lib/` that the closure does not name.

That makes this change **fleet-synchronous**: X1 (`runtime_source_digest`) plus
X2 (`require_exact_keys`) mean both hosts must be redeployed and re-pinned in
the same window. The review records that exact tripwire killing v4.5 and v4.6
(`VERDICTS.md`, LANE F, P1/S compression item). **Do not land this into a
ceremony in flight.** It belongs in a deliberate runtime bump alongside the
other fleet-synchronous transport changes (zstd compression, digest-keyed
allowlist, A/B-pair retirement) rather than as its own break.

This change also closes half of open decision F-5 (`BASES-20260828.md:2132`):
the runtime closure was restated in three places that nothing compared, so
`test_runtime_closure_lists_agree` now asserts `RUNTIME_SOURCE_NAMES`, the
launcher's `RUNTIME_FILES`, and the files on disk are the same set.

---

## 4. Measured, on this machine, local-to-local

**What is measured and what is not.** The benchmark
(`scripts/bench_mover.py`) is local-to-local by construction. It does not
measure the wire, and that is deliberate: LANE F measured neo→sting ssh at
**14.8–18.5 MiB/s, cipher-independent, with four parallel streams giving zero
gain**, and put **83% of the cold floor** in wire time. A number taken over
that link measures the link. Removing it leaves the part a mover can change:
per-file protocol cost, deduplication, and resume.

Every row below is one run on neo (6 cores / 8 GiB, APFS, warm page cache),
synthetic corpus: **100,000 files / 1,237.7 MiB / 24.3% duplicate as
measured**. `rsync` is GNU rsync 3.4.4 with the engine's shipped flags;
`rclone` is v1.75.0. Reproduce with:

```
scripts/bench_mover.py --generate --files 100000 --mean-bytes 13000 \
    --duplicate-fraction 0.25 --workdir <scratch> --rsync-path <gnu-rsync>
```

| Mover | Pass | Wall | files/s | MiB/s | Bytes on the wire |
|---|---|---:|---:|---:|---:|
| `engine-sha256-walk` (1 thread, the review's primitive) | — | 14.16 s | 7,060 | 87.4 | — |
| **native, 8 streams** | cold | **59.75 s** | 1,674 | 20.7 | **982,026,165 (= unique)** |
| **native, 8 streams** | repeat | **46.41 s** | 2,155 | 26.7 | **0** |
| `rsync` shipped flags | cold | 79.53 s | 1,257 | 15.6 | 1,297,871,578 (all of it) |
| `rsync` shipped flags | repeat | 20.49 s | 4,881 | 60.4 | 0 |
| `rclone sync --transfers 16` | cold | 31.04 s | 3,222 | 39.9 | 1,297,871,578 (all of it) |
| `rclone sync --transfers 16` | repeat | 3.33 s | 30,060 | 372.1 | 0 |
| `rclone check --checksum --checkers 16` | verify | 13.69 s | 7,307 | 90.4 | n/a |

Corpus totals: 1,297,871,578 B charged, 982,026,165 B unique, 75,533 objects
for 100,000 paths. The native mover moved **315,845,413 B (24.3%) fewer bytes
than either other mover on the cold pass**, and the landed tree was
correspondingly smaller on disk (1.1 GiB vs rsync's 1.4 GiB) because the
aliases are hardlinks.

Sanity cross-check on the machine: `rclone check --checksum` here does 7,307
files/s; Lane A measured 268,406 files in 33.8 s = 7,941 files/s on the real
corpus. Same order, so this host reproduces the review's rclone numbers. The
`engine-sha256-walk` row is *faster* here (7,060 files/s vs the review's 4,523)
because this corpus is warm and synthetic — do not read that row as a
contradiction of Lane A.

**Honest reading of that table** — including the two rows that refute this
design's own first instinct:

1. **Native beats the shipped rsync cold, by 1.33x** (59.75 s vs 79.53 s), and
   moves 24.3% fewer bytes doing it. That is the Lane F finding closed: the
   transport unit is now the digest, not the path.
2. **Native loses to `rclone sync --transfers 16` cold, by 1.9x** (59.75 s vs
   31.04 s). It is a Python prototype paying a JSON frame and a round trip per
   object against a Go worker pool. This design does not claim it will win that
   race, and §0 says why the race is not the bar anyway.
3. **The repeat row is a refutation, not a win. Read it before quoting
   anything else here.** "Resumable" is *not* "cheap to resume": the native
   repeat is 46.41 s — **2.3x slower than rsync's repeat and 14x slower than
   rclone's** — even though it moved zero bytes. The reason is exactly the
   integrity property §2.1 sells: the sender re-hashes every source file to
   build the plan, and the receiver re-hashes every landed file to answer
   `have`. Two full content passes to prove a no-op. rsync and rclone answer
   the same question from `stat` and are correct-enough for their contract.
   **This is the single most important open item in the design**, and it has a
   named fix in two halves: §6.1 (sealed digests remove the sender's pass) and
   §6.2's freshness key (removes the receiver's). Until both land,
   `--mover native` is a cold-pass improvement and a resume *regression*.
4. **The dedup number is a property of the corpus, not of the clock.** 24.3%
   here was built in. On the real corpus the plan already measured it: 112.04
   GB charged, 84.11 GB unique — **27.93 GB that rsync moved and this mover
   would not**. That is a projection from the plan's own capacity block, not a
   measurement of this code.
5. **A local run is an upper bound on the mover's contribution** and says
   nothing about a ceremony's wall clock. `prompts/47:50-51` fences exactly
   this: a substrate-only benchmark is not a product result.

---

## 5. Parity table — the sanctioned form of the comparison

| Capability | rsync (shipped) | rclone | native mover |
|---|---|---|---|
| Parallel streams | no | yes (`--transfers`) | yes (N, default 8) |
| Cross-file dedup on the wire | no | no | **yes (sha256 identity)** |
| Resume after SIGKILL | whole run | per file | **per object, no journal** (but see §4: costs 2 full content passes) |
| Integrity proof at the destination | `--checksum` (2nd full read) | `check --checksum` | **every byte, always, before publish** |
| Progress / heartbeat | none as invoked | `--stats` by default | per-stream, ≤15 s |
| mtimes, xattrs, hardlink topology | yes | yes | **no** (§2.2) |
| Filter grammar / globs | `--exclude` | rich | none (consumes the allowlist) |
| Receiver source pinned by digest | n/a | n/a | **yes (hello frame)** |
| Proven against a live destination | **yes, 84 GiB** | yes, widely | **no** |

The last row is the one that matters today. The three "yes" cells in the native
column are real and tested; the tool is still unproven where it counts.

---

## 6. Designed, not built

### 6.1 The digest seam — making the plan pass free

`plan_objects()` takes a `digests` callable. When it returns `(sha256, size)`
the file is never opened; when it returns `None` the file is hashed there, at
the single-threaded 4,523 files/s the review measured.

Every payload path in the allowlist **already has a sealed digest**: the
capture's `snapshot-index.jsonl` carries `sha256`, `size`, `mode` and `kind`
per record (`scanner.py:2414-2446`). Wiring the seam means joining the
allowlist to that index. It is not wired here, for one measured reason: a
`dict[path] → digest` over 1,781,044 entries is roughly 500 MB of RSS on a
host with 8 GiB that has already had its corpus curated down twice for memory
(`VERDICTS.md`, LANE F: `DEFAULT_MAX_SQLITE_ROWS`, the 4 GiB output cap, the
183 G codex root that "can NEVER capture"). The correct implementation is a
**merge-join**: both sides are sorted, the index by `(root_index,
relative_path)` and the allowlist by absolute path, so the join is streaming
and O(1) in RSS. That is the next change, and it is what makes the native
mover's cold pass cheaper than rsync's rather than merely equal to it.

### 6.2 Resumable materialize

**The freshness key is already computed.** Two places, both cited:

* `_stable_stat()` (`scanner.py:379-390`) returns
  `(st_dev, st_ino, IFMT, IMODE, st_nlink, st_size, st_mtime_ns, st_ctime_ns)`
  — a strict superset of the `(dev,ino,size,mtime_ns,ctime_ns)` tuple the
  review names. It has 14 call sites, every one of them a *stability fence*
  (did this move under me?), never a *freshness* check (do I still need to read
  this?).
* The copy loop compares exactly `(st_dev, st_ino, st_size, st_mtime_ns,
  st_ctime_ns)` before and after each accounted copy
  (`scanner.py:2256-2268`).

**And it is thrown away.** The sealed index record has exactly nine keys and
none of them is a timestamp or an inode (`scanner.py:2437-2446`, enforced by
`require_exact_keys` at `scanner.py:3070-3084`). So the next run has no
recorded stat tuple to compare against, and re-derives everything from content.

**Why materialize is expensive to resume, precisely.** Materialize is *already
idempotent per object* — the content-addressed store short-circuits on a
digest that is already published (`executor.py:913-923`) and `_publish_object`
is an O_EXCL hard-link that never replaces (`executor.py:820-841`). The cost is
ordering: `_materialize_file` calls `_verify_record()` **first**
(`executor.py:873`), and `_verify_record` unconditionally does
`sha256_file(path)` (`executor.py:778`). So a resume re-reads the entire
quarantine mirror to prove records it is about to skip. On the 173 G stage
that is the whole corpus, twice over the run.

**The change, in three parts:**

1. **Reorder.** Hoist the `_object_path(stage_root, digest).exists()` check
   above `_verify_record`. When the object is present and its digest verifies,
   the source read is redundant — the object store already holds the proof.
2. **Fence the reorder with the freshness key, not with nothing.** Add
   `(dev, ino, size, mtime_ns, ctime_ns)` to the sealed index record (a tenth
   key, therefore an X2 `require_exact_keys` contract break — land it in the
   same runtime bump as §3) and require a stat match before the short-circuit.
   The stat is what proves the source did not move; the object store is what
   proves the bytes are right.
3. **Journal the manifest incrementally.** The manifest is assembled in memory
   and written once (`executor.py:1749-1751`), so a killed materialize loses the
   record of objects that did land. Append each entry to a durable per-phase
   journal as it is published — `agent-recover` already owns exactly this shape
   for apply (`cli.py:554`, "resume or roll back an interrupted apply").

**The darwin caveat, stated because it is load-bearing.** `setattrlist` and
`utimes` can move `mtime_ns` backwards without changing `ctime_ns`, and APFS
`clonefile` preserves both across a *different* inode. So `ino` must be in the
key, and a `--paranoid` flag must re-hash everything for the ceremony of
record. This is not a theoretical caveat on this fleet: `reflink_clone` is on
the hot path (`scanner.py:2206-2232`) and 1,557,224 of 1,574,829 files in the
source-B seal were `base-reflink` (`VERDICTS.md`, LANE F).

### 6.3 Not designed here, deliberately

On-wire compression (`--compress-choice=zstd`, ~1.75x corpus-weighted),
dropping `--delay-updates`, parallelising the destination custody walk, and
scoping FastCDC to sqlite and git packs are all LANE F items with their own
measurements. They belong in the same fleet-synchronous runtime bump but not in
this module.

---

## 7. What must be true before `--mover native` may become the default

1. A live source→destination run against a real quarantine, whose output passes
   `validate_snapshot_custody` unmodified. **Not done.** The local tests prove
   byte, mode and namespace equality against the same fields custody checks
   (`tests/test_mover.py::TransportMoverSelectionTests`), which is necessary
   and not sufficient.
2. A killed-mid-run resume against that same live stage.
3. The §6.1 merge-join **and** the §6.2 freshness key, together — the
   measured resume regression in §4 (46.41 s vs rsync's 20.49 s to move
   nothing) is disqualifying on its own, and neither half fixes it alone.
4. A measured comparison against the rsync push **on the real link**, which is
   the only place the 24.9% dedup saving turns into wall clock.
5. `mover.py` in `RUNTIME_SOURCE_NAMES` deployed to both hosts in one window
   (§3), with the receiver bootstrap proven against the destination's `python3`.
6. An answer to F-2 (`BASES-20260828.md:2072`) — whether bulkload is a public
   product and what its comparator is — because until then no benchmark table
   in this repo has a sanctioned audience.
