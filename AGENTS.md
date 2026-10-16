# bulkload Development Context

> **Read [docs/SPEC.md](docs/SPEC.md) first.** It states what bulkload is for,
> traced to the operator's own words in
> [docs/intent-ledger.md](docs/intent-ledger.md). R0: every work lane carries
> on at the destination (conversations resume and can be carried on,
> transcripts intact, repos and worktrees intact, nothing dropped but truly
> recreatable content; transcripts byte for byte under their original
> session IDs, SPEC C8/C9). R3: proven faster than rclone on complex networks and many-file estates,
> staging work data before tcfs launches. Where this file, `docs/slo.md` or a
> Linear document disagrees with SPEC on what bulkload must achieve, SPEC
> wins.
>
> - **Quote the requirement you serve.** Every lane plan and every audit
>   quotes, verbatim, the SPEC requirement it serves (R0–R4), with the ledger
>   IDs SPEC cites for it. A plan or audit that cannot quote one is out of
>   scope. "Done", "ready" or "verified" is reported only with SPEC's
>   acceptance receipts.
> - **Never re-ask a standing ruling.** The rulings in SPEC's "Standing
>   operator rulings" table are never re-asked, and no interview offers an
>   option that contradicts them. SPEC's 2026-10-16 rulings on C1–C20
>   (OI-1003-Q194) are standing too. A new conflict may be asked once, as a
>   dated question. When the operator restates a standing ruling, even
>   phrased as a question, that is confirmation, not an open question.
> - **The ledger is append-only.** `docs/intent-ledger.md` is never trimmed or
>   paraphrased, R-N55 included. New operator statements are added as new
>   entries.

## Ratified rulings (read first)

Process rulings are lab's, and lab's text is canonical: lab `AGENTS.md`
"Ratified rulings", **R-HOOK-CONVERGENCE-20261004** (Linear TIN-3692 comment
`98cf680c-7299-4949-bfb2-60079053ad43`). It supersedes the former R-N11
blanket process ban and the R-N12 hook-stop mechanics that this file used to
summarise (the 2026-09-20/24 text, last synced from lab#1913 by #78). Read
R-N11, R-N12 and R-N13 there; they are not copied here
([docs/SOURCES.md](docs/SOURCES.md), "Lab process rulings").

Repo-specific delta: this file's former R-N13 text (2026-09-20) also said
"Ticket descriptions are never rewritten, only superseded by dated
comments." Lab's current R-N13 does not restate that clause and no ruling
has withdrawn it, so it still applies here.

## Repository

This repository is bulkload: a live-host estate mover in Rust. It is its own
project and shares no code with TCFS (tummycrypt); it may carry TCFS data
(R-N53, 2026-09-23). The Python engine, the stillness ceremony and the
installer are retired and are not recreated.

- `crates/bulkload-proto`: wire frames, row schema, refusal taxonomy.
- `crates/bulkload-agent`: walk, chunk transport, Git estate carry, SQLite
  provider-state composition.
- `crates/bulkload-bench`: the R23 benchmark against rclone.
- `crates/bulkload-handoff`: the credential-class handoff probes, kept out of
  the agent binary (WP10).

What bulkload must achieve: [docs/SPEC.md](docs/SPEC.md) (#236). How it
works: [docs/design.md](docs/design.md). `docs/evidence/` holds measured
results and the historical rulings ledger (evidence, not current
instructions).

## Sources of truth

[docs/SOURCES.md](docs/SOURCES.md) names the one canonical place for each
kind of fact. Point to it; do not copy its content into agent files or notes.

## Live workstreams

The entry point is the Linear document named in
[docs/SOURCES.md](docs/SOURCES.md) (rewritten 2026-10-16T17:34Z): priority
order, live board and pointers. When a stream changes state, update its
P-TIN-177 milestone and live issue; edit the document's table only when a
live issue is added or retired (the document's own rule). In chat, follow lab
R-HOOK-CONVERGENCE-20261004 (brief, changes only, no whole-board recital).

## Engine rules

- Performance: S1 in [docs/slo.md](docs/slo.md) defines the bar, the gates,
  their order and verdict rule.
- Unsafe-first (R-N54): raw syscalls, zero-copy and reused buffers are the
  default design choice. Every `unsafe` block carries a `// SAFETY:` comment.
- R25 (R-N58): no re-read of what the destination holds durably. The reading
  that is the SLO's obligation, and the strict reading's status and limits,
  are in docs/slo.md (OI-1003-Q40 and its 2026-10-06 amendment). Resume must
  be measurable with `source_bytes_read` counters.
- Durability: a record is never committed before the bytes it describes are
  durable. Refusals are values, never panics: the deny-panics lints in each
  crate's `[lints.clippy]`. Code calls that wall "R33"; the R1–R43 ledger's
  R33 is a different ruling (see that file's header).
- Dependency wall: the agent must not depend on anything in the `FORBIDDEN`
  list of `crates/bulkload-agent/tests/dep_graph.rs`. Fix a violation by
  removing the dependency, not by widening the list.

## Estate rules

- Keep all operator and agent sessions running. Process control follows lab
  R-N11 (above).
- Include Git, agents, credentials, dots, SQLite and worktrees. Preserve both
  hosts' unique state; an unresolved state class is not an implicit exclusion.
  Only truly recreatable content may be dropped (OI-1003-Q185, Q192).
- Git-aware union preserves refs, objects, real stashes, indexes, working bytes
  and worktree administration without changing an active checkout.
- Carry account credentials privately; preserve destination machine identity
  and Home Manager authority. Never print credentials.
- Capture SQLite through its backup API; never copy live WAL/SHM bytes or
  replace a database with unique rows. Preserve conflicts for explicit
  resolution.
- Reclaim only proven redundant data after preserving unique content.
- Estate operations are not frozen. R-N56's freeze is superseded on order
  (OI-1003-Q194 C6, RUL-06), after OI-1003-Q142 relaxed OI-1002-Q24
  (docs/slo.md, amendment 2026-10-16).

## Agent lanes

- Keep process-control words out of command text. The repo's pre-commit
  process-safety audit is the authoritative wording check (R-N92).
- Never check whether a PID is alive. Wait only on your own background
  tasks' notifications, or on files (R-N104).
- Never bypass git hooks from the shell (`core.hooksPath`, `--no-verify`).
  Only the product's own cargo tests may disable hooks, and only in fixture
  repos they create and destroy (R-N98).
- Each lane builds with its own `CARGO_TARGET_DIR`. A shared target
  directory lets one lane test another lane's binaries.
- Builds and tests on neo: **open for the operator**. FOOT-03 (2026-08-26,
  "we CANNOT EVER build on neo, EVER") and FOOT-10 (2026-09-01: tests,
  checks and lints on the teletype "WILL crash the machine") say no; SPEC
  R4 cites both. R-N69 (about 2026-09-23, newer) allowed local cargo builds
  on neo behind an inline `TINYLAND_ALLOW_LOCAL_BUILD="<lane> (R-N69)"`
  prefix as the first token. Neither is recorded as superseding the other
  ([docs/SOURCES.md](docs/SOURCES.md), "Open for the operator").
- Benchmark and S2 sampling conditions (R-N81, OI-1003-Q39, Q152, Q160,
  Q168) are in docs/slo.md S1 and S2. R-N91 (holding other lanes quiet for a
  sample) is read as superseded by OI-1003-Q194 C18 (never-halt) together
  with SPEC R3 ("Samples run on live hosts"); RUL-18 itself names CONF-01,
  06, 07 and 08, not R-N91. How R-N81's load gate fits SPEC R3 is open
  (docs/slo.md, amendment 2026-10-16).

## Durable notes

Every session writes one entry in `docs/agent-notes/`, named
`YYYY-MM-DD-<lane>.md` (R-N13, R-N84). An entry records what was done,
the rulings it cites, the PRs and shas it produced, and what is still open.
Distilled facts and rulings also go on the owning Linear issue. Nothing
durable is left in `/tmp` or a harness scratchpad. Notes are dated records:
one that is no longer true gets a dated label at its top naming what
superseded it, and is not rewritten or deleted (lab R-N13; this replaces the
earlier R-N55 deletion rule).

## Validation

Local first (OI-1001-Q2, 2026-10-01): run `just check-fast` inside
`nix develop` before pushing; GloriousFlywheel CI is a contended backstop,
not the first signal. The `justfile`'s recipe comments are the reference for
every tier (`just --list`): `check-fast` (mandatory), `check-optional`,
`check-full`, and the deep tier (`props-deep`, `crash-sweep`; OI-1003-Q78,
Q81), which is never a PR gate. PR CI runs two gates, `ci-source` and
`ci-fault-harness` (R-N122, OI-1003-Q65); `ci-source` also runs the history
secret scan, which locally is in the optional tier.

Rules the recipes do not enforce on their own:

- The fault harness, the R-N88/R-N119 power-loss proofs and the R25 counter
  tests are mandatory and never move to the optional tier. A test moves to
  the deep tier only by a ruling, and its PR names it. No property draws a
  random seed.
- CI remains on GloriousFlywheel tinyland-nix; keep `.bazelrc.flywheel`
  endpoint-free. Changing `flake.nix`, the public-read guard or its
  composite action requires updating the pinned digests in
  `scripts/ci-public-read-guard.sh`, the action and
  `tests/test_ci_contract.py`. Changing the workflow updates
  `WORKFLOW_SHA256` in `tests/test_ci_contract.py`.
- No PR loads the Bazel graph. `just test` and `just test-local` run
  `//:tests`; `just build` and `just ci` build `//:bulkload`.

Stage explicit paths.
