# Bulkload capture performance

Status: measured 2026-08-26 against real corpora, read-only.

Capture is slow for one reason, and the reason is a single keyword. This
document states the measured cost of every kernel in the capture path, names
the defect, gives the target architecture that keeps every typed guarantee at
roughly ten to twenty times the current speed, and states plainly what that
architecture cannot prove.

Line anchors below are the 2026-08-26 working tree. They drift a few lines
against `b635bd1`. The symbol names are the durable reference.

## Method

Host: 6 cores, 8 GiB RAM.

The JSONL sample is drawn uniformly over files with seed `20260826` from all
737 `.jsonl` files under
`/Volumes/TinylandState/tinyland-state/codex-boundary/sessions` (17.13 GB,
15.95 GiB total). The sample is 73 files, 1.73 GiB. Median file is 3.5 MB, max
is 267 MB, mean line is 2.68 KB. Every read is read-only. No capture was run
against a live root.

CPU-isolated rows run on a 150 MB fully cached subset. Raw read on that subset
measures 11,979 MB/s, so storage contributes nothing to those rows.

The tree-walk rows use `/Users/jess/git/lab.worktrees` and `/Users/jess/git`,
walked read-only with no writes.

## 1. Kernel throughput

| # | Kernel | MB/s | klines/s | vs (c) |
|---|---|---|---|---|
| a | raw sequential read, cold from external USB SSD | **236–287** | — | 265x |
| a | raw sequential read, page cache | **7,498–11,979** | — | 8,400x |
| b | `hashlib.sha256` whole file, 1 MiB chunks, cached, 1 core | **1,391–1,463** | — | 1,470x |
| b | same, 6 threads (`hashlib` drops the GIL) | **3,847–4,052** | — | 4,100x |
| **c** | **engine `_jsonl_records`, empty replacements, as shipped** | **0.89–0.99** | **0.35** | **1.0x** |
| d | micro-optimized per-line loop (json plus per-line digest) | **110–135** | **37.7** | **142x** |
| d2 | micro-optimized, per-line digest only, no json | **373** | **104.4** | 392x |
| d3 | micro-optimized, json validation only, no per-line digest | **139** | **38.8** | 146x |

Row (c) took **609.43 s for 542.2 MB and 211,970 lines**. The engine is
**1,470 times slower than a plain SHA-256 of the same bytes**, and **142 times
slower than a per-line loop doing the same work**.

The gap is not the guarantees. Rows (d), (d2) and (d3) compute the same
per-line digests and the same json validation as the engine.

## 2. The 142x is one keyword

`_jsonl_records` opens its stream unbuffered:

```python
with path.open("rb", buffering=0) as stream:
    for line in stream:
```

`scanner.py:1617`, in `_jsonl_records`. An unbuffered `FileIO` has no
`readline`. CPython therefore falls back to `RawIOBase.readline`, which issues
**one `read(1)` syscall per byte**.

Line delivery alone, no json and no hashing, over 27.03 MB of the real corpus:

| Line delivery | Time | MB/s | klines/s |
|---|---|---|---|
| `for line in open(p, "rb")`, buffered | 0.02 s | **1,122** | 135.9 |
| `for line in open(p, "rb", buffering=0)`, as shipped | **26.73 s** | **1.0** | 0.1 |

**1,122 times.** `rusage` on the shipped engine confirms the signature: wall
21.84 s = user 6.64 s + **sys 14.44 s, 68 percent kernel**, which is
**1.30 M syscalls per second** against exactly 28,338,725 bytes. That is
byte-at-a-time reading. It is not json and it is not SHA.

The fix is one argument. Measured on the engine's own code, with `Path.open`
patched so `buffering=0` becomes 1 MiB and nothing else changed:

| Variant | MB/s | Speedup |
|---|---|---|
| `_jsonl_records` as shipped | 0.99 | 1.0x |
| `_jsonl_records`, `buffering=0` removed | **70.5** | **71x** |
| plus micro-optimizations: single `json.loads`, block-fed hasher, `bytes.splitlines` | **134.7** | **136x** |
| plus 4-process pool | **305** | **308x** |

**Evidence-preserving.** `sha256`, `records_sha256`, `translated_sha256` and
the record counts were verified byte-identical across four real session files,
shipped against buffered. No recorded digest moves.

The residual algorithmic tax after the buffer fix is small and known.
`_jsonl_records` calls `json.loads` **twice per line** — once on `line` and
once on `transformed` — even when `replacements` is empty and `transformed is
line`. Rows (d3) at 139 MB/s against (d2) at 373 MB/s show that json
validation, not hashing, is the remaining CPU floor.

## 3. Stat-only walk against content generation

Subtree: `/Users/jess/git/lab.worktrees` — 126,174 files, 1.37 GiB,
11.4 KB per file.

| Pass | Full subtree | **Per 100k files** | kfiles/s |
|---|---|---|---|
| `find . -type f`, C, dirent only | 1.12 s | **0.89 s** | 112 |
| `os.scandir` dirent only, Python | 1.04 s | **0.82 s** | 122 |
| `os.scandir` plus `lstat` fence (size, mtime_ns, ctime_ns) | 4.21 s | **3.34 s** | 30.0 |
| engine `_tree_census(content=False)`, Census A and B | 12.09–15.16 s | **9.6–12.0 s** | 8.3–10.4 |
| raw content SHA-256, 1 core | 21.16–22.33 s | **16.8 s** | 5.8–6.0 |
| raw content SHA-256, 8 threads | 10.27 s | **8.1 s** | 12.3 |
| engine `_tree_generation(content=True)`, fence read | 32.53–33.27 s | **25.8–26.4 s** | 3.8–3.9 |

**Content to stat is 2.7x at engine level and 5.0x at `scandir` level.**

The engine census is **three times slower than a bare `scandir` plus `lstat`**
for the same information. `_tree_census` calls `_stable_stat` **twice per
entry**, at `scanner.py:2295` and `scanner.py:2322`, and calls
`canonical_bytes` once per entry on top. The second stat is the change fence;
it is correct, and it is also the whole of the 3x.

Whole `/Users/jess/git`, measured tonight: **1,246,577 files, 63.1 GiB,
643 `.git` entries, 19.79 GiB of `.git` bytes across 613 repositories.**

| Whole-tree pass | Time |
|---|---|
| `find ~/git -type f` | **7.8–8.0 s**, 159 kfiles/s |
| `os.scandir` dirent only | 22.0 s |
| `os.scandir` plus lstat fence | **46.9–50.3 s**, 25–27 kfiles/s |
| engine `_tree_census` stat only, modeled at 9 kfiles/s | ~139 s |
| engine `_tree_generation` content, modeled | ~400 s |

A cost fit over three real repositories — nixpkgs at 5.2 KB per file,
GloriousFlywheel at 29.9 KB per file, lab.worktrees at 11.4 KB per file —
gives content hashing at **about 140 µs per file plus 1 second per 800 MB**.
The engine census wrapper adds **about 120 µs per file** on top.

On a 1.25 M file tree the per-file term dominates the per-byte term four to
one. That is exactly why a stat fence is cheap and a content fence is not.

## 4. Where the capture hours go

Measured inputs: git tree 63.1 GiB over 1,246,577 files; 19.79 GiB of `.git`
bytes over 613 repositories; hot providers about 40 GiB over about 345k files,
of which **append-only JSONL is 20.44 GiB in about 11,000 files and about
8.2 M lines** — codex-boundary 15.97 GiB plus `~/.claude` 4.47 GiB.

The four-times fence read is structurally visible: `scanner.py:3950`
(Census A), `:3975` and `:4046` (two `_tree_generation` passes over live git),
`:4012` (Census B), and `:4070` into `:3369` (generation over snapshots).

Costs below are **core-time**, derived by applying the section 1 and section 3
rates to those inputs. Only the `git fsck` rate was timed as a phase; the
remaining rows are modeled from the measured kernels, and are labelled as such.

| Phase | Work | Rate applied | Core-seconds | Share |
|---|---|---|---|---|
| `git fsck --full` x613 | 19.79 GiB `.git` | 27 MB/s/core, measured: 138 MB→5.49 s, 26 MB→0.95 s, 24 MB→0.67 s | 751 | 3.2% |
| **`_jsonl_records` over append JSONL** | **20.44 GiB, 8.2 M lines** | **0.89–0.99 MB/s (c)** | **21,141–23,517** | **90%** |
| git tree fences: 2 censuses plus 3 content generations | 1.25 M files, 63.1 GiB, four times over | 9 kfiles/s and 140 µs+1/800 s per MB | 1,492 | 6.4% |
| Other hot-provider content digest | ~19.6 GiB, ~334k files | 140 µs/file plus 1/800 s per MB | 72 | 0.3% |
| Redundant whole-file SHA on JSONL | 20.44 GiB, computed then discarded | 1,400 MB/s | 15 | 0.06% |
| **Total** | | | **23,470–25,846 s**, 6.5–7.2 h core | |

Cross-check on the dominant row: 8.2 M lines at 0.35 klines/s is 23,428
core-seconds, which agrees with the byte-rate derivation.

The pools are three wide — `MAX_CAPTURE_FILE_WORKERS`,
`MAX_CAPTURE_WORKSPACE_WORKERS` and `MAX_SNAPSHOT_ROOT_WORKERS` are all 3 at
`scanner.py:58`. `_ordered_parallel_map` uses a `ThreadPoolExecutor`. On the
JSONL row 68 percent of the work is kernel-side `read(1)`, which releases the
GIL, while `json.loads` holds it, so the effective speedup is between one and
three times. That places wall time in a two to three and a half hour band for
that phase alone, and the observed whole-capture wall of about five hours sits
inside the modeled band.

Three conclusions follow, and only these three are load-bearing:

1. **About 90 percent of capture core-time is one unbuffered read loop.**
   Everything else in the engine, added together, is under 10 percent.
2. The four-times fence read is the second term at 6.4 percent. It is worth
   fixing, and it is worth fixing second.
3. The redundant whole-file SHA is free to remove and buys nothing measurable.
   `classify_file` calls `_file_record`, which computes `sha256_file(path)`
   at `scanner.py:500`; `_jsonl_records` then recomputes the identical
   whole-file digest as its `sha256` key, and `record.update(append_records)`
   overwrites the first with the second. Two full digests of 20.44 GiB, one
   discarded. Remove it because it is dead work, not because it is slow.

## 5. Target architecture

The goal is every typed guarantee preserved, at ten to twenty times the
current speed. Ordered by measured payoff.

**Step 0 — remove `buffering=0` from `_jsonl_records`.** One argument.
Verified digest-identical. It removes about 90 percent of capture core-time.
Nothing else on this list matters until this lands.

**Step 1 — one fused walk.** Produce census and digests together in a single
traversal, instead of Census A, two live generations, Census B, and a snapshot
generation. One `_stable_stat` per entry, not two. One whole-file digest per
file, not two. This collapses the 6.4 percent fence term and the double-stat
3x penalty in section 3 at the same time.

**Step 2 — a persistent content-addressed digest index**, keyed on
`(dev, ino, size, mtime_ns, ctime_ns)`. `_stable_stat` at `scanner.py:404`
already returns all five as fields 0, 1, 5, 6 and 7, so the key costs no new
syscall. The S3 refuter established that `(dev, ino, mode, nlink)` alone is
unsafe; that tuple is not the key, and `ctime_ns` is the load-bearing field.
A hit skips the content read. A miss reads and records.

**Step 3 — fences by stat comparison, with targeted content re-proof of only
the changed entries.** The fence keeps its meaning: an entry that moved during
capture still fails the capture. It stops costing a whole-tree content pass to
say so.

**Step 4 — a bounded parallel digest pool.** `_ordered_parallel_map` at
`scanner.py:381` already provides the ordered, bounded, determinism-testable
shape, and S4 already proved it observationally invisible. Extend it to a
process pool for the GIL-bound json path; the 4-process row in section 2
measures 305 MB/s against 134.7 MB/s threaded.

**Step 5 — a trust-but-verify sampling knob.** Re-prove a random fraction of
index-hit entries on every capture. See section 6.

**Step 6 — the streaming seal, as already landed in S5.** `HexRecords` spill
past `spill_record_threshold` keeps the per-line record lists off the heap and
serializes to identical JSON. Keep it unchanged.

Projected core-time with steps 0 through 4, using the same inputs and the
measured post-fix rates:

| | Core-seconds | Speedup |
|---|---|---|
| As shipped | 23,470–25,846 | 1.0x |
| Step 0 only, JSONL at 134.7 MB/s | 2,469 | **9.5x** |
| Steps 0 and 1, fences at ~280 s | 1,257 | **18.7x** |

Ten to twenty times is not an aspiration. It is two changes, and the first one
is a keyword.

## 6. Limits

**What stat-keying cannot catch.** An index keyed on
`(dev, ino, size, mtime_ns, ctime_ns)` reports a hit for a file whose contents
changed in place, at identical size, with all three timestamps restored. Such
a forgery is not a normal write. `mtime_ns` is settable through `utimensat`,
but `ctime_ns` is not settable by any ordinary interface; forging it requires
raw device access, a filesystem-level write, or clock manipulation. The
practical residual risks are therefore three, and they should be stated rather
than argued away:

- a privileged or offline in-place rewrite that restores `ctime_ns`;
- a clock rollback across the interval between two captures;
- device and inode reuse after the index entry was recorded, where a new file
  lands on a recycled `(dev, ino)` with a matching size and matching times.

A content fence catches all three. A stat fence catches none of them. That is
the trade being made, and it is the reason the sampling knob is part of the
architecture and not an option on it.

**The sampling answer.** Re-prove a uniform random fraction `p` of index-hit
entries on every capture. A persistent alteration survives `n` captures with
probability `(1 - p)^n`. At `p = 0.02` it is detected with 95 percent
probability within 149 captures; at `p = 0.10`, within 29. Set `p = 1.0` for an
attended cutover capture, which pays the full content price by design and by
declaration. The chosen `p` belongs in the capture evidence, so a receipt
states how much of itself was proved and how much was inherited.

**The index is trusted state.** It must live inside the evidence boundary and
bind the pinned Bulkload application-source closure, exactly as capture and
plan already do. A missing, unreadable, or closure-mismatched index must
degrade to a full content pass. It must never degrade to a pass.

**What does not change.** The buffer fix changes what is read, not what is
recorded, and that was verified byte-identical on four real session files. The
fused walk and the index also change only what is read. The canonical bytes
must remain identical under all of it, and the existing serial-against-pooled
determinism test is the instrument that says so. Any change on this list that
moves a recorded digest is a defect in the change, not a new baseline.
