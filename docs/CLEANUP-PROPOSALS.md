# Cleanup proposals

Findings that are real but were deliberately left unapplied. Each one either
changes engine semantics, changes an operator-visible CLI contract, or adds
surface rather than removing it — so each needs a decision before it lands.

Everything here was verified against the code at the time of writing. Line
numbers are from that revision; re-check them before acting.

## 1. `RUNTIME_SOURCE_NAMES` has no guard against drifting from the package

Status: propose adding a test. Not applied — additive, and `tests/` was held by
another change in the same session.

`model.py:31` enumerates the six runtime modules as a literal tuple.
`BUILD.bazel:7-14` enumerates the same six paths as `py_library` srcs. The two
lists are maintained by hand and nothing compares them to each other or to the
directory they claim to describe.

`RUNTIME_SOURCE_NAMES` has exactly two references: its definition and its single
use in `runtime_source_digest()` at `model.py:530`.

Why it matters: `runtime_source_digest()` is the runtime custody seal that
receipts bind to. Add a seventh module and it is imported and executed but
silently excluded from the digest. The seal would then claim to cover a runtime
it does not cover — a false custody claim, not a missing feature.

Proposed guard:

```python
assert set(RUNTIME_SOURCE_NAMES) == {p.name for p in package_dir.glob("*.py")}
```

Cheap, and it fails at the moment a module is added rather than at the moment
someone audits a receipt.

## 2. Quiesced capture mode costs two custody modes for one that works

Status: propose retiring. Not applied — this is engine semantics plus an
operator-visible CLI flag.

`scanner.stable_capture_pair` (scanner.py:5007-5015) branches on
`writers_quiesced`. The quiesced branch demands both
`first["catalog_sha256"] == second["catalog_sha256"]` and full dict equality
`first["catalog"] == second["catalog"]` between the A and B captures, raising
`A/B captures are not byte-stable` otherwise.

On a living host that is unsatisfiable. A single mtime or size change anywhere
under home, `git_root`, the three provider roots, or any seat between the two
captures fails it. That unsatisfiability is exactly why the immutable-live
snapshot path exists, and the non-quiesced branch below it does the real work:
`contract_sha256` stability, seal lineage via base, and a no-lost-custody check
via `_catalog_path_identities`.

The quiesced mode nonetheless survives as a second full custody mode, costing 18
`writers_quiesced` touchpoints — cli.py 216, 219, 235, 272, 291 and scanner.py
4130, 4210, 4265, 4278, 4297, 4301, 4646, 4666, 4700, 4834, 5007, 5009 — each an
independent branch to reason about and test.

Retiring it would delete the flag, both branches here, and the
`--acknowledge-writers-quiesced` CLI surface, leaving immutable-live snapshot as
the only custody mode. That matches what every operator-facing document already
describes as the contract.

Decide before acting: whether any real workflow still captures with writers
stopped.

## 3. Declaring an absent seat aborts the whole live-snapshot capture

Status: propose fixing. Not applied — it is a behavior change, and the right
fix depends on which of the two existing behaviors is intended.

Two code paths disagree about whether a missing seat is fatal.

The non-snapshot path deliberately tolerates it. `_capture_seat` calls
`_declared_root(root, allow_absent=True)` and returns a record with
`"exists": False` for a missing directory seat (scanner.py:2196-2208), and
catches `FileNotFoundError` to do the same for a missing file seat
(scanner.py:3783-3784).

The live-snapshot path does not. Its `descriptors` list binds seat roots only
when present — `for name, binding in seat_bindings.items() if binding[3]` at
scanner.py:3835-3839 — so `roots` and `work_roots` contain no binding for an
absent seat. But the `snapshot_seats` loop at scanner.py:4103-4108 iterates every
declared seat regardless of existence and calls `_snapshot_path` on it.
`_snapshot_path` (scanner.py:4227-4235) raises
`live snapshot has no root binding for {path}` when no binding matches.

`descriptors` is seeded only with `git` (scanner.py:3827-3829) and extended only
with existing providers and existing seats. Home is not a root binding, so an
absent seat under home has no binding to fall through to. The capture aborts.

Why it matters: live snapshot is the default path, and an optional-absent seat is
a documented, supported declaration. SKILL.md states that an absent optional file
is captured as such, and references/agent-context.md names `.zsh_history` as
optional-absent. Declaring that documented seat on a host that lacks it aborts
the default capture path.

Decide before acting: whether the live-snapshot path should skip absent seats
(matching `_capture_seat`) or whether the declaration should be rejected earlier
with a message that names the seat.
