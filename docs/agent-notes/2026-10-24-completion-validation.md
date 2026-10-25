# Native estate recovery, 2026-10-24

Authority: the operator's October 24 direction prioritizes lab, blahaj and
softconnect, including repeated neo reboots. SPEC goal 1 and rules 1, 2,
6, 7 and 8 apply. R-N13. Lexical: XL-ACC-004.

Source increment: signed `803dec539731ed0f0c115ebd693bc897f2cb40e6`.
Review: https://github.com/Jesssullivan/bulkload/pull/328, draft.
Base: `5b5efd718b2aebef82b9d853d6003cfa492538bc`.
The signed follow-up is `65f006db6ae6dcee1351ee3dde76cc0eab52bcdb`.
Both final artifacts were built from that follow-up.
This receipt supersedes pending-validation statements in the six source notes
dated October 24. Later sections record checks still running.

| Stream | Owner | Repo/branch | State/evidence | Next |
| --- | --- | --- | --- | --- |
| Recovery source | coordinator | bulkload/codex/bulkload-completion-20261024 | signed source, draft #328 | finish fault validation |
| Linux validation | coordinator | owned honey checkout | focused recovery tests pass | finish power-loss and mode recipes |
| Darwin validation | coordinator | owned PZM external checkout | focused recovery tests pass, existing failures retained | finish repaired fault suite |
| Native execution | coordinator | existing neo/sting move states | no estate pass in this session yet | same-build deployment and native run |
| Rollout diagnosis | homes lane | fix/rollout-mismatch-diagnostics-20261024 | separate unvalidated source | validate without changing acceptance |
| Opaque decision | decision lane | codex/opaque-decisions-20261024 | exact decision and scoped design preserved | native ledger, receiver and closure implementation |

## Focused results

Validation runs at nice 19 on honey and PZM. Each has its own target and
temporary directory. No build or test suite ran on neo or sting.

| Check | Honey | PZM |
| --- | --- | --- |
| workspace clippy, all targets, warnings denied | pass | pass |
| shallow and handoff-policy integration | 19 pass | 19 pass |
| first-fetch custody integration | 11 pass | 11 pass |
| native reconnect integration | 7 pass after follow-up | 7 pass after follow-up |
| estate-move integration | 39 pass | 34 pass, four existing failures, one ignored |
| exact-instance handoff dispositions | 4 pass | 4 pass within full suite |
| full handoff suite | earlier 61 pass | 61 pass |
| streamed metadata unit tests | 4 pass | 4 pass |
| source device alias unit tests | 6 pass | 6 pass |
| bounded retirement unit tests | 5 pass | 4 pass, one ignored |
| first-fetch admission unit test | 1 pass | 1 pass |
| reconnect budget unit tests | 2 pass | 2 pass |
| formatting | pass | pass |
| gitleaks directory scan | no leaks | no leaks |

The reconnect tests cut a data frame after another file has a durable
output record. A failed connection precedes recovery of the partial file.
The resumed source read remains exactly four MiB. Other cases bound repeated
lost SSH status for empty, warm, equal-adopted and cold roots.

The source-device test changes the source device three times while preserving
reuse and ownership. It does not reboot an actual host.

An unchanged shallow rerun transfers zero object bytes. Tests also retain
dirty, staged, stashed and linked-worktree state. A full destination retains
its deeper history. Differing or insufficient frontiers remain held.

## Existing failures remain visible

The full Darwin check previously failed. Separate unchanged-base executions
reproduced the failures below. No source guard was relaxed to hide them.

| Suite | Existing failure | Baseline evidence |
| --- | --- | --- |
| IO units | background priority inheritance | unchanged base reproduces |
| reseat key map | case-sensitive expected findings | unchanged base reproduces |
| reseat holder | held target cannot be proved on Darwin | unchanged base reproduces |
| estate-adopt | corpus sharing returns IO errno 92 | unchanged base reproduces |
| refusal taxonomy | existing bare Io(None) in estate_move | unchanged base reproduces |
| source command registry | unregistered lsof and stale SSH shape | unchanged base reproduces |
| source command registry | render string classified as a Git command | unchanged base reproduces |
| source command registry | dependent mutation oracle starts from that breach | unchanged base reproduces |
| estate-move | three archive/live-link cases and one key-map case | all four reproduce on unchanged base |

The source registry has 19 passing tests and three failures on both hosts.
Those failures keep the focused wrapper's exit nonzero. A passing test count
is never substituted for its exit or the full gate.

## Fault fixture repair

The first fault run reported 164 passing cases, 17 failures and two ignored.
Unchanged base reproduced all 17 handoff failures. Its legacy fixture expected
two tracked files while the default had changed to `CLAUDE.local.md`.

The fixture now selects the existing compatibility mode in owned test
subprocesses. Crash children select it explicitly. The parallel parent process
and production code keep their normal environment. No assertion or fault point
was removed. Honey's repaired suite reports 181 pass, zero failures and two
ignored. Power-loss and the remaining mode recipes are still running.

## Boundaries and next execution

Reconnect is opt-in with `--reconnect-seconds=N`, default zero. It retries
native file pulls only. It does not replay Git upload-pack, SQLite union or
retirement. Its budget limits admission and backoff, not an active attempt's
wall time. Only new durable payload resets the budget.

First-fetch recovery covers bound object-only roots for ordinary standalone
repositories and linked families. Overlapping independent repositories,
submodules, bare repositories and separate administration retain existing
behavior. Their interrupted first-fetch recovery is not established.

The original command records retain 61 archive overrides. A fresh pass must
retain their labels, state paths, source identities and complete mappings.
Source and destination executables must match for the snapshot format.
Historical wrappers overwrite old receipts and must not run unchanged.
The homes wrapper also chains retirement, outside the intended fresh pass.

The recorded opaque exception is exactly 65 paths under 15 destination roots,
with suffix `.neo-20261023`. Forty-two was the broader held-workspace count.
The exact answer is already recorded. It requires no repeated decision.
Safe native implementation needs a scoped ledger, remembered-hold handling
and closure proof together. An accepted item refusal cannot prove those
bytes reached either the original or the authorized sibling path.

No operator continuation, full closure, retirement or live reboot acceptance
has occurred in this session. Only the operator can establish completion.

Mutation receipt: actor=coordinator | target=owned branch and review #328 |
reason=retain recovery custody and correct closure identity | ruling=operator
2026-10-24, SPEC goal 1 and rules 1, 2, 6, 7 and 8, R-N13 |
prior_state=typed blockers and lost resume custody | result=signed source
increment with focused evidence, full estate completion still open.

## Completed checks, 2026-10-24

All scheduled validation commands have finished. Honey's complete
`just fault-harness` exits zero, including both mode arms. The active crash
suite reports 181 passing cases with two existing ignored cases. All 19
power-loss cases pass in each exercised arm.

PZM's repaired default crash suite reports 181 pass and two ignored.
Its 17 Darwin power-loss cases pass. The canonical recipe then exits 101:
79 IO tests pass, one existing priority-inheritance test fails and two are
ignored. The remaining partial-write, resume, handoff-off and delta-lever
recipes were executed separately and pass. Their step exits are retained.
The original full Darwin check's other failures remain recorded above.

The final reconnect follow-up adds a seventh regression. Omitted and zero
budgets retain configured SSH sharing. A positive budget sets both
`ControlMaster=no` and `ControlPath=none`, so existing masters cannot defeat
the new connection's keepalive settings. It neither signals nor modifies
those masters. Both hosts pass the seven integration and two budget tests.
Both regular and feature clippy passes succeed. Gitleaks reports no leaks.
Registry wrappers still exit 101 for the same three baseline failures.

Authoritative exit records come from the scripts beside their logs. A zero
outer SSH launcher status is not substituted for an inner recipe failure.
This is a component validation receipt, never a clean full-gate receipt.

## Artifacts and execution hold, 2026-10-24

| Artifact | SHA-256 | State |
| --- | --- | --- |
| Linux release | `c2744853511c7618a7c95ccadad8463a6de32937b6361fea2f00630aa1eb535c` | build-host selftest passes |
| Darwin arm64 release | `b967bab78f074e8ff1cd5576783305dee55f7e6fbe65f3e694e93f1316daad7a` | build-host selftest passes, retained for Neo |

Sting's uniquely named candidate is
`/home/jess/.local/bin/tcfs-bulkload-agent-65f006db`.
The native installer removes the build-directory RUNPATH and registers two
runtime dependency roots. Its small installation selftest passes, reporting
both roots registered. No compiler or test suite ran on Sting.
The installation digest is recorded in `artifact-manifest.json` because
RUNPATH sanitization changes the release bytes. Earlier candidates created
by this task were removed with their own metadata and roots.

The Darwin artifact remains under the durable receipt directory. Neo has
not received it. Tailscale reports the exact Neo peer offline, last seen
`2026-10-25T00:40:00.1Z`. Configured and fresh SSH attempts both timed out.
No reboot cause is inferred and no alternate host is substituted.

Durable receipts, compressed logs, exit records, artifact hashes and prepared
native commands are under
`/srv/data/jess/fast-local-relocated/bulkload/receipts/codex-completion-20261024`.
The prepared commands retain all mappings and existing state identities.
Their manifest says unexecuted. A fresh same-build source capture and move
remain blocked on source reachability. No estate bytes moved this session.

Next code work remains native support for the exact opaque-file exception,
homes composition and rollout diagnosis. Source retirement stays separate
from this convergence attempt. The operator's continued original sessions
still establish the final outcome.

## Scratch cleanup, 2026-10-24

All 30 changed source and lane-note files in each build checkout matched
the signed follow-up byte for byte. Those development-source checks are
recorded in `honey-source-match.json` and `pzm-source-match.json`.
They are not cross-host estate comparisons.

Final logs and inner exit files were compressed and hashed before cleanup.
Both owned build roots were then removed, including their targets and
temporary files. Readback confirms both roots absent. The Git common
directories were inside those roots. Shared Cargo caches and older owner
builds were preserved. `scratch-cleanup.json` records the exact targets.
The Darwin artifact and Sting's installed candidate remain available.
