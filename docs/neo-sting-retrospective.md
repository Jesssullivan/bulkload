# Neo to Sting one-off sync retrospective

Evidence cutoff: 2026-07-29. Classification: `DONE_WITH_CONCERNS`.

## Outcome

The one-off migration is functionally complete for resuming TCFS work on
Sting. The active clean TIN-2864 worktree is exact, clean, signed, and
remote-backed on both hosts. The protected dirty owner checkout is also exact
for the state that matters: branch, HEAD, status names, modified bytes, and all
regular untracked content.

It is false to say that all worktrees or all agent-session files are synced.
That is not required for resuming work and would erase important host-local
ownership distinctions.

## Timeline and evidence

### July 14–15: stable-root TCFS work and interrupted context

- Historical TCFS thread: `019f5fc3-2352-7650-a437-6a41b8ad0feb`.
- Work proceeded in a stable/isolated tree while the primary checkout stayed
  dirty.
- The saved checklist was: recover worker patches; close auth, namespace,
  hydration, Git no-loss, rotation, and GC seams; compile affected crates; run
  library tests; run daemon/CLI integration; run Clippy; perform exact-head
  review.
- Later resume proved the thread could be located, but 26 compactions meant
  raw JSONL presence did not guarantee exact semantic recall of the earlier
  checklist.

### July 16–18: migration design and broad archive

- The migration design correctly split portable files, path-rewritten state,
  regenerated state, and fresh authentication.
- The broad rsync archive provided rollback insurance but also copied platform
  metadata and stale state that should never become live authority.
- Initial repo loops failed because SSH consumed the loop's stdin; a subsequent
  global `ssh -n` broke commands that intentionally streamed stdin. The durable
  lesson is to distinguish metadata probes from stream transports explicitly.
- Live agent files moved during copying because there was no first-class
  quiescence barrier. Additive delta passes were necessary.
- One path map briefly treated `/Users/jess/git` as a normal home subpath rather
  than mapping it to Sting's NVMe-backed Git root. Exact-root path tests closed
  that regression.

### July 17–20: clean TIN-2864 lane and source-only validation

- The clean lane became
  `/Users/jess/git/tummycrypt.worktrees/tin2864-root-plan`, branch
  `codex/tin-2864-root-plan-20260719`.
- Checkpoint 13 landed as signed commit
  `b267cd8738cfe04e045660a81e13293d2f43b20e` after full Sting format, Clippy,
  test, build, diff, and secret-scan gates.
- The lane remained source-only and digestless. No live plan, execute RPC,
  deployment, enrollment, reconciliation, or crypto ceremony was authorized.
- Sting accumulated many detached validation/draft trees. These are evidence
  and owner residue, not inputs to an automatic mirror or cleanup.

### July 20–22: auth, rules hygiene, and native continuity

- The archive contained 4,923 AppleDouble `._*` files. Codex attempted to parse
  a 163-byte `rules/._default.rules` sidecar as UTF-8 and aborted before resume.
- The file was losslessly quarantined at a SHA-256-derived path outside the
  rules directory; the canonical rule remained unchanged.
- Cached login status was not sufficient proof. Device codes expired during
  attempts, and fresh attended ChatGPT device authentication was required.
- The legacy resume probe showed Codex 0.144.5 ignored the intended
  `--ephemeral` protection and appended 13,291 bytes. The historical file must
  no longer be used for immutability experiments.
- Fresh Sting-native thread `019f8a9a-218a-7c91-8498-e17a794528a4` was created
  under `umask 077`, then intentionally resumed. It reproduced the nonce, lane,
  TODO, and boundary payloads without tool calls. This proved live auth,
  persistence, lookup, and dialog continuity.

### July 22–24: exact active-lane convergence

- The Sting clean lane was initially at `e5ea08104a462d84df158c004554100735df0a7a`
  while live remote and Neo had advanced four commits.
- A targeted fetch and `merge --ff-only` moved only the clean Sting lane to
  signed head `36d8c0f67c12d4a1d1164a925f71c8b4885c68bd`.
- The live branch subsequently advanced by one signed source commit,
  `0d7654791cef4b6f533b661eb029738408d85369`
  (`fix(sync): type non-linux inventory iterator`). A second targeted fetch and
  `merge --ff-only` moved only the clean Sting lane to that exact head.
- The branch subsequently advanced through signed catalog-writer,
  finalization, and composed-main checkpoints to
  `854d52802881aa6ec72a9685ea8c9622745f4ed5`.
- Current dated proof: Sting is clean and exactly `0/0` from the branch
  upstream at `854d528`; GitHub verifies the signature. Live `main` is an
  ancestor and draft PR #565 is twenty-eight commits ahead.
- Exact-head durable CI is still absent. Five `tinyland-nix` jobs and one
  `tinyland-dind` job have remained queued without an assigned runner since
  2026-07-23T22:38:02Z. The only `ubuntu-24.04` matrix surface is skipped and
  did not execute. Two Darwin-held jobs deliberately fail when scheduled, so
  restoring listeners alone cannot make the PR green. The lane remains
  source-only/digestless.

## Current classification manifest

### `EXACT_ACTIVE_LANE`

- Neo:
  `/Users/jess/git/tummycrypt.worktrees/tin2864-root-plan`
- Sting:
  `/srv/fast-local/jess/git/tummycrypt.worktrees/tin2864-root-plan`
- Branch: `codex/tin-2864-root-plan-20260719`
- HEAD: `854d52802881aa6ec72a9685ea8c9622745f4ed5`
- State: clean, exact, and remote-backed. The signature is valid on Neo and
  GitHub; Sting's local verifier lacks the public key.
- Constraint: never copy either linked worktree's host-specific `.git` pointer.

### `PROTECTED_PARITY`

- Neo: `/Users/jess/git/tummycrypt`
- Sting: `/srv/fast-local/jess/git/tummycrypt`
- Branch: `facet6/dotgit-conflict-corruption-harness`
- HEAD: `f9fb683d5aeb0c5a4ce87d2d0e6a428e97c04eb4`
- Upstream: 29 behind, recorded separately from parity.
- Staged: zero.
- Modified: `crates/tcfs-cli/src/main.rs`, exact bytes; its blob also matches
  insurance commit `1756851`.
- Untracked: 122 exact names with matching regular-file content manifest.
- Ignored state intentionally differs: Neo has `.direnv` and Python bytecode;
  Sting has `target` build output.
- Action: do not develop, rebase, clean, or copy from either owner checkout.

### `REMOTE_RECONSTRUCT`

Six non-primary Neo branch tips were proven on live remote refs. Recreate clean
worktrees from exact refs when needed; do not mirror their directories or
worktree administration.

### `EXCLUDE_PRESERVE`

- Neo stale MCP-redaction lane: eight tracked modifications.
- Sting MCP/context-migration residue.
- Sting CP15: one tracked modification.
- Sting CP14: seven tracked plus one untracked path.
- Other Sting detached validation/draft trees.

Retain and classify these until their owner disposition is explicit. Never use
them as an automatic recovery source and never silently delete them.

### `SESSION_NATIVE`

- Fresh Sting thread/resume proof passed.
- Historical Sting rollout is a strict append/superset of the Neo version;
  retain Sting's longer file and never overwrite it.
- In this dated July 20–22 execution, auth was established freshly per host and
  no auth file was copied. That is an execution fact, not a policy prohibition.
- Rules sidecar is quarantined; no AppleDouble file remains in interpreted
  rules.

## Worktree and session truth

Neo has seven worktrees and Sting has twenty-five. Only a few basenames
intersect, and several same-name entries intentionally point at different
branches or checkpoints. No worktree was locked or prunable in the dated audit.

Session stores are also a divergent union, not replicas. A later read-only
sample found thousands of rollouts on each side, hundreds unique to each host,
and active Neo files still growing. No session bulk copy is needed for TCFS
resume. Optional archival parity must wait for quiescence and copy only
selected closed, destination-absent JSONL files with no-overwrite, stable
pre/post hashes, owner-only modes, and post-copy JSONL validation.

## What worked

1. Preserve the dirty checkout and create a clean sibling lane.
2. Treat remote refs as committed-state authority and use targeted
   fast-forward-only reconciliation.
3. Hash real file bytes and untracked manifests instead of comparing rendered
   diff text across macOS and Linux.
4. Keep host-local ignored output out of parity claims.
5. Split transcript portability from indexes, caches, and authentication.
6. Quarantine invalid metadata by hash instead of deleting evidence.
7. Prove continuity with a fresh nonce and intentional resume.
8. Restate the exact TODO and safety fences after every crash or compaction.

## What failed or nearly failed

1. Broad tree semantics hid state classes and admitted AppleDouble metadata.
2. SSH stdin behavior broke both loop and stream forms when handled globally.
3. No quiescence fence meant live session tails changed during transfer.
4. Copied rollout modes were briefly too broad before correction to `0600`.
5. Login-status metadata was mistaken for a live model/auth proof.
6. Raw transcript presence was mistaken for retained semantic context after
   compaction.
7. An “ephemeral” resume option mutated the source evidence.
8. Worktree name/count parity was initially conflated with usable continuity.
9. A Home Manager switch encountered a Determinate Nix parallel-build
   coroutine UAF; serialized build settings recovered. This was not an OOM.
10. One broad test leaked an ambient API credential into a traceback. Rotation
    remains a separate attended custody lane, not migration work.

## How TCFS removes this work class

TCFS has crossed the mechanism threshold but not the daily-driver threshold.
Current evidence covers selected repository movement, Git conflict behavior,
and bounded agent subtrees. It does not yet prove arbitrary linked-worktree
reconstruction, broad Codex/session roaming, or home-directory takeover.

The intended replacement is a registered-root pipeline:

```text
registered roots
  -> strict repeated snapshots
  -> immutable complete catalogs and heads
  -> typed namespace and Git claims
  -> monotonic publication and writer fencing
  -> encrypted content-addressed chunks/manifests
  -> hydration, conflict handling, rotation, and safe GC
```

Git data must remain semantic: refs/objects through Git-safe mechanisms,
working bytes through the sync layer, and linked-worktree administrative paths
reconstructed locally. Agent state requires provider-aware allowlists and
scheduled reconciliation. Generated state is denied/regenerated. Auth remains
typed, private, and fail-closed; either an attended typed copy or
reauthentication still requires a fresh destination acceptance proof.

TIN-2864 establishes source-only held snapshots, held GitRaw topology,
catalog/head completeness barriers, composed claims, and monotonic control
state. It intentionally cannot mint live authority, emit a plan digest, move a
live HEAD, mutate a namespace, or authorize runtime action.

`bulkload` is therefore migration insurance and an executable requirements
record for TCFS. Retire it as a primary mover once TCFS proves registered-root
R0–R5 in both directions, agent-coupled resume, path translation, AppleDouble
denial, and a safe linked-worktree design.

## Definition of done

Done now:

- active Sting lane exact and ready for source-only work;
- protected owner state exact and frozen;
- native Sting Codex continuity proven;
- credential state was kept local in that execution window;
- AppleDouble blocker quarantined;
- dirty/rescue worktrees classified as preserve/exclude;
- no `cmux`, live TCFS, deploy, activation, deletion, or ceremony entered the
  proof.

Remaining concerns, explicitly deferred:

- supersede the retained clean-lane handoff text with `854d528` in the active
  Sting conversation before frontier work;
- durable default `umask 077` rather than per-launch discipline;
- nonfatal MCP connectivity/login warnings;
- Sting public GPG verification-key availability;
- GloriousFlywheel-backed durable TIN-2864 CI, TIN-2998 Darwin authority, and
  completion of the in-progress independent exact-head source review;
- attended Home Manager activation after its own gates;
- separate credential rotation/containment; and
- TCFS runtime and ceremony fences.

The narrow continuity proof did not require another copy before resuming TCFS
on Sting.

### 2026-07-28 portability ruling and parity follow-up

The earlier execution choice is not a policy ban. The operator explicitly
ratified Codex auth and SQLite as copy-eligible typed state, and published
Codex guidance documents trusted `auth.json` movement for headless machines.
Full Neo-to-Sting parity therefore remains valid follow-up work.

Read-only live evidence found append-only composition rather than conflict:
all 20 differing shared rollout UUIDs and `history.jsonl` are exact
Sting-prefix/Neo-superset cases. Both hosts now run Codex 0.145.0, but their
SQLite schemas and thread sets differ, and provider SQLite includes state,
logs, goals, and memories families. Auth and SQLite must use dedicated private
plans, consistent SQLite backups, path/schema reconciliation, rollback, and
historical-resume acceptance. They must never enter the generic repo-file
adapter or logs as values.

### 2026-07-29 private reader and typed-auth boundary

The first July 29 source-only slice implemented the copy-eligible reader
without claiming a cutover. `codex-private-capture` copied opted-in
`auth.json` into an owner-private, no-replace evidence bundle and captured
every selected top-level SQLite family through the immutable SQLite backup
path. `codex-private-plan` emitted a non-actionable compatibility dossier.
At that historical checkpoint no installer existed, and no live Neo or Sting
Codex home was captured or modified.

The later TIN-3268 policy v3 slice supersedes only that “no installer” claim.
It implements an attended, typed `auth.json` replacement with external backup,
durable journal, offline verification, rollback, and interrupted-operation
recovery. The preferred evidence shape is source auth-only plus destination
auth-and-SQLite. Full source and destination captures are also accepted, but
the source SQLite bytes are never consumed. Every destination SQLite family is
captured and held exact before and after auth replacement with zero SQLite
mutations and `sqlite_union_ready=false`.

The SQLite boundary did not move: any live `-wal`, `-shm`, or `-journal`
companion hard-stops immutable capture. There is still no WAL-aware capture,
SQLite union/composer/installer, or combined private apply. The operator's
short-lived quiescence attestation records a procedural fence with
`provider_writer_proof=false`; the advisory `flock` coordinates cooperating
Bulkload processes only.

The supported direct and Bazel launchers enter Python with `-I -S` before
Python startup hooks. The private command entrypoint then pins the complete
Bulkload policy/application-source closure before importing the command
implementation and binds that authority through compatibility and install
plans. This does not claim interpreter or standard-library closure authority.
Apply, verify, rollback, and recovery revalidate the accepted application
closure. Their receipts prove offline byte and preservation invariants only:
`provider_runtime_acceptance_verified=false` remains deliberate, and a fresh
attended Codex turn on Sting is required before claiming working auth.

No live Neo or Sting private state was mutated while developing or validating
this source slice.

### 2026-07-29 live parity re-audit

A later read-only audit separated “ready to resume TCFS source work” from
“complete agent-state parity.”

The clean TIN-2864 lane is exact across Neo, Sting, and the live remote at
`0a3a9ceceeab497af614e07e80e91780c7f3be47`. Sting can therefore carry
source-only TCFS work without another repository copy. That claim does not
extend to the full worktree registry: Neo registered eleven worktrees, one
prunable/absent, while Sting registered thirty-three. Four matching paths
currently exist on both hosts; only the clean root-plan lane was
cleanliness-audited. The protected primary remains forbidden.

Codex continuity is an asymmetric union. Neo's newest session and history are
dated July 29; Sting's are dated July 22 and July 18. Sting also retains seeded
session `019f8a9a-218a-7c91-8498-e17a794528a4`, which is absent on Neo.
Replacing Sting's store with Neo's would therefore destroy target-only
continuity. TIN-3268 must preserve destination-only sessions, add source-only
sessions, prove shared-prefix relations, and finish with fresh picker, resume,
dialog, and new-thread evidence.

Authentication and SQLite differ on both hosts. That is not a prohibition on
copying them: both are typed, copy-eligible state. It does prohibit raw file
replacement while provider SQLite is live. Both state homes had WAL activity
during the audit, so the safe path remains a consistent provider-aware backup
or a proved-quiescent immutable capture, schema/path-aware composition,
rollback custody, and attended provider acceptance.

The global Codex skill projection is also incomplete. Neo has the current
`~/.agents/skills` tree; Sting has no `~/.agents` directory and its legacy
`~/.codex/skills` is a different projection. Claude's gstack revision matched.
Skills must move as a typed, pinned projection with discovery/trigger tests,
not as an undifferentiated plugin-cache copy.

Two workstation surfaces remain separate attended lanes:

- eGreg source is ahead of Sting generation 46. A managed daemon serves the
  runtime-directory socket while an older unmanaged daemon still owns the
  legacy `/tmp` socket selected by `et`. Do not kill either ad hoc; restack the
  reviewed Lab source, switch under the runbook, and prove exactly one
  PID/socket plus edit-detach-reconnect.
- JetBrains remains intentionally on Honey. Sting has no backend unit or
  `pycharm-sting` alias. TIN-3079 requires source, deployment, Gateway, and GUI
  acceptance before Honey can be retired.

Tailnet and the previously failing research MCP endpoints were healthy during
this audit. Sting still lacked GitHub CLI authentication, so authenticated
private development remained an operator-controlled follow-up rather than a
network fault.
