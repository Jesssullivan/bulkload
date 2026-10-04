# 2026-10-03 — wp1-s2-source-safety (WP1, S2 source-safety quick wins)

Rulings: OI-1003-Q15..Q21 (WP0 (a)–(g), wave 3), OI-1002-Q33 (#124), R-N13.
Plan: `docs/plans/2026-10-03-architecture-review.md` WP1 (PRs 1–4; PR 5's
ledger part only, per WP0(g)). Branch `feat/wp1-s2-source-safety-20261003`
from `origin/main` @ `c65dee5`. carry_v2 untouched (frozen, OI-1003-Q15).

## Done

1. **One `GitEnv` table** (`git_carry::git_env`: `CLEARED`, `SET`, `CONFIG`)
   feeds `git_carry::git()`. Added `GIT_NO_LAZY_FETCH`, `GIT_OPTIONAL_LOCKS`,
   `maintenance.auto=false`, `LC_ALL=C`/`LANGUAGE=`, and
   `GIT_CEILING_DIRECTORIES` at the `-C` path's parent. `estimate::hardened()`
   now adds only `--git-dir` and the probe's ceiling. `PROBE_SCRIPT`'s
   preamble is tested entry-for-entry against the table. `carry_v2::pinned()`
   is unchanged; it inherits the table through `hardened()`.
   New typed refusal `GIT_SOURCE_PARTIAL_CLONE` (`git_carry::partial_clone`,
   mirroring the probe's config + `.promisor`-through-alternates checks),
   raised in v1 export and in estate-capture's new `preflight` before the key
   reads anything. Fixture: a real `--filter=blob:none` clone.
2. **Background priority** (WP0(f)): `io::sys::enter_background()` /
   `in_background()`. Linux: `setpriority(19)` + `ioprio_set` IDLE. Darwin:
   `IOPOL_THROTTLE` (process scope) + `QOS_CLASS_BACKGROUND` + nice 19.
   `main` enters it before any thread for `serve`, `estate-capture`,
   `snapshot`, `git-carry-estimate`, `git-export` and `copy`.
   `--priority=normal|background` is the explicit flag. Every counters line
   records `priority=` and `priority_from=default|flag`. `bulkload-bench`
   gains `--priority` (default `background`; `normal` is gate (a)'s
   recorded opt-out), entered before the first sample, with `priority=` in
   the header. The dead `io::Qos`, `set_thread_qos` and `thread_qos` are
   deleted.
3. **serve/copy overlap first**: `canonical_state()` resolves a state root
   that does not exist yet. `serve` refuses an overlap before `Store::open`;
   `copy` refuses either state inside the source, and each state inside its
   own root, before either store exists. Proptest P-S2
   (`a_run_leaves_the_source_lstat_census_unchanged`): over generated trees,
   a copy, its warm rerun, and a refused copy with a state inside the source
   leave every source node's lstat identity unchanged. A deterministic
   regression covers serve's ordering. Added `test_support::prop_config`
   (fixed CI seed, `BULKLOAD_PROPTEST_DEEP=1` locally, no persistence).
4. **ObjectStoreRewritten**: the export reads the source's pack-listing
   identity with its authority. When a Git child fails
   (`GIT_INVENTORY_MALFORMED`) and the listing has changed, the export returns
   `Exported::ObjectStoreRewritten`. Estate-capture records it as
   `deferred-with-drift` with one `ObjectStoreRewritten "objects/pack"` row and
   no capture record; the next pass captures. The same failure with an
   unchanged listing still refuses. Both cases are tested.

## Deferred

- **WP0(g) (source ledger `synchronous=NORMAL`/`fullfsync=OFF`)**: deferred.
  The ruling holds "only if proven in the formal model", and that model does
  not exist yet. A comment and test cannot discharge it. A lost source
  `captures` row also turns an unchanged seat into a re-read on the next run,
  which is the S3 "0 content bytes" clause, so the ruling's "costs at most a
  re-read" needs that model.
- The `FADV_DONTNEED`/`F_NOCACHE` part of plan PR 5 is out of scope per the
  brief.

## Open / caveats

- Darwin code (`setiopolicy_np` FFI, the QoS check, `ps -o nice=` in the io
  test) is compile-unverified on sting: the devshell has no Darwin std. It
  needs a neo `just check-fast` (#104).
- `copy` is in the background-default set because it reads the source
  in-process (gate (a)'s verb). If the operator wants the ruling's list
  literally (without `copy`/`git-export`), that is a one-line change.
- `test_support::prop_config` is the property-test plan's PR0 helper in
  minimal form. A later PR0 lane should extend it rather than add a second
  one.
