# bulkload

Bulkload is a typed, receipt-driven cutover tool for a `~/git` fleet and the
Codex, Claude, Pi, and mutable-seat state needed to continue work on another
machine. Source-authoritative conflicts are reversible through exact destination
rollback custody; structural ambiguity and unsafe state stop. Credentials and
SQLite are never treated as ordinary repository files.

Nine product schemas cover `AgentCaptureV4`, `GitWorkspaceV2`, plan, the five
phase receipts, and the durable journal. They drive one consolidated surface:

```text
agent-capture
agent-plan
agent-stage --phase preseed
agent-stage --phase final
agent-apply
agent-verify
agent-rollback
agent-recover
```

## Safety model

- Two stable captures are required for both source and destination.
- `/Users/jess/git` maps exactly to `/srv/fast-local/jess/git`; longest path-map
  prefix wins.
- Destination `prepare`, Neo-to-Sting `push`, and destination `materialize`
  write only to an external owner-private stage/quarantine.
- Final apply requires the exact plan digest and a sealed final-stage receipt.
- Before mutation, only exact overwritten entries are snapshotted with required
  verified reflinks. There is no silent full-copy fallback.
- The durable journal makes apply, rollback, and crash recovery idempotent.
- Evidence contains hashes and typed structure, never credential values,
  SQLite rows, symlink payloads, or unsanitized remote URLs.
- Offline verification never claims provider authentication; run a fresh
  attended provider action after cutover.

Git preservation includes refs, tags, notes, stash, custom and symbolic refs,
reflog/pseudo-ref recovery roots, linked and detached worktrees, the exact
index (all stages and intent-to-add), working-tree deletions, symlinks, modes,
and untracked dirt. Divergent destination ref tips receive durable
`refs/bulkload/recovery/destination/*` anchors before source refs move.

Codex includes SQLite/WAL, sessions, archives, history, goals, memory, queue,
rules/skills, and typed auth. Claude state is unioned with path rewriting only
for explicit path-bearing text; unknown-named binaries retain exact bytes and
Claude auth remains a nonportable hold. Pi state and auth are typed. Declared
provider descendants default to portable-private state after exact managed
exclusions. SQLite unions compatible rows and uses a reversible source snapshot
when schema, shared rows, or canonical primary keys conflict.

See [the design](docs/design.md), [migration contract](.agents/skills/bulkload/references/migration-contract.md),
[agent-state guide](.agents/skills/bulkload/references/agent-context.md), and
[the ceremony contract](.agents/skills/bulkload/references/ceremony.md).

## Ceremony

Applies to: bulkload 0.2.0 · ceremony revision 1 · reviewed 2026-08-23

A Bulkload switch is a ceremony, not a script. The operator owns every stop and
every start. The migration agent reads, plans, transports, applies, and
verifies; it sends no signal to any process.

> **HARD INVARIANT.** The migration agent must never send a signal to a process
> it does not own. The migration agent must never suspend, resume, or end an
> interactive agent session, a daemon, or a launchd job. The migration agent
> inventories writers. The operator stops writers. If a writer runs, the
> migration agent reports it and stops. The migration agent does not act.

The invariant is a correctness requirement before it is a courtesy. A suspended
writer is the worst state a capture can meet. It holds its open file locks. It
also holds an unflushed SQLite write-ahead log, mid transaction, for as long as
it stays suspended. Cooperative wind-down is the only writer state that
produces a capture Bulkload can prove. Quiesced is not the same as stopped.

The agent contract — the read-only probe allowlist, the six quiescence checks,
the refusal text, and the self-exclusion rule — is
[the ceremony contract](.agents/skills/bulkload/references/ceremony.md). That
file ships inside the installed Skill. This section owns the phases; that file
owns the agent's obligations. Lab owns every fact about shells, terminal
multiplexers, eGreg, MCP, and Linear, and this repository links to lab rather
than restating it.

### The quiet window is measured in tens of minutes

Revision 1 carries every Codex SQLite family: state, goals, `logs_2.sqlite` at
about 5.17 GiB, and `thread_history_1.sqlite` at about 2.47 GiB. Each capture
takes a backup-API copy of roughly 7.7 GiB and hashes every row. Each role
needs two such captures with no writer between them, and the ceremony holds two
separate quiet windows. Plan for tens of minutes per window. The read-only
quiet-window probe adds about one minute to every capture call on a 236-repository
Git root, and the ceremony makes eight such calls across the two windows.
Announce that length before Phase 1 begins.

An operator may declare a reviewed managed exclusion for the Codex log database
to shorten a window. Revision 1 carries it. Carrying everything is the reviewed
default, and any exclusion is an explicit operator decision recorded in the
plan.

### Phases

| # | Phase | Owner | Writers | Fails closed on |
|---|---|---|---|---|
| 1 | Prepare | operator | winding down | — |
| 2 | Verify quiet | migration agent, read-only | none | any live or suspended writer in scope |
| 3 | Capture | operator, from a plain terminal | none | unequal A/B catalogs |
| 4 | Plan | migration agent, operator reviews | none | any blocker; an unaccepted digest |
| 5 | Preseed | migration agent | may resume | capacity gate; a failed reflink |
| 6 | Seal final | operator and migration agent | none | a stale plan; changed source bytes |
| 7 | Apply and verify | migration agent | none | `verified != true`; `failures != []` |
| 8 | Land | operator | new, on Sting | — |
| 9 | Aftercare | operator | restarting on Neo | — |
| R | Rollback | operator and migration agent | none | a wrong receipt digest |

### Phase 1 — Prepare (operator)

1. Announce the quiet window. State the start time and the expected length.
2. Inventory the writers with the lab recipe `codex-process-residue`. That
   probe is read-only.
3. List every interactive agent session. Record the session, its repository,
   and its terminal.
4. Ask each session to finish its current turn. Wait for the turn to end.
5. Save the work. Commit or stash every repository in the `~/git` fleet.
6. Close each `codex resume` session yourself.
   > **NOTE.** One Codex session owns one Codex state directory. A second
   > session fails with `database is locked` on the state database.
7. Close each Claude Code session yourself. The migration agent drives every
   phase except the capture pair, so close its session too before Phase 3, and
   run the capture commands from a plain terminal; the ceremony contract,
   section 5, states why.
8. Fence the non-interactive writers yourself. `launchctl bootout` is the
   operator's fence verb here: a launchd job declared with `KeepAlive = true`
   comes back after a softer request, and suspending such a job wedges it
   permanently. Fence these labels and the file-provider extension:
   - `dev.tinyland.tcfsd`
   - `dev.tinyland.tcfsd-reconcile-claude-projects`
   - `dev.tinyland.tcfsd-reconcile-git-roam-tool-daemon`
   - `dev.tinyland.tcfsd-health`
   - `dev.tinyland.tcfs-mcp-reaper`
   - the `TCFSFileProvider` file-provider extension
   > **CAUTION.** `dev.tinyland.tcfsd-reconcile-claude-projects` writes into the
   > Claude provider root, which is inside the capture scope. An unfenced
   > reconcile job makes the A/B capture pair unequal, and Phase 3 fails closed.
9. Record the wind-down. Write down what you stopped, and when. Phase 9 restarts
   in the reverse of that record.

The reviewed fence pair is lab's `just tcfs-fence` and `just tcfs-unfence`. The
operator runs both from a lab checkout on Neo. `tcfs-fence` writes a wind-down
record under `~/.local/state/tcfs-fence/`, and it exits nonzero when a label
fails to boot out. `launchctl bootout` stays the operator's raw fallback.

`TCFSFileProvider` is outside that fence. No launchctl verb reaches a File
Provider extension. The operator stops that extension per capture through an
Apple-supported route, and never through a signal. The lab recipe `tcfs-status`
reads state only, and it confirms the result.

### Phase 2 — Verify quiet (migration agent, read-only)

The migration agent runs the six checks in the ceremony contract, section 6:
writer census, suspended-process check, SQLite lock check, daemon check,
modification-time stability, and the read-only destination preflight. Every
check reads. None of them changes a process state.

The lock check refuses a `-shm` sidecar newer than one hour, because a clean
SQLite close unlinks that file. An older `-shm` sidecar is unclean-exit debris
with no writer left to end, so the probe records it as a warning and continues.

On any finding, the migration agent prints the finding, names the process or
the path, and stops. It asks the operator to act. Bulkload's whole exit surface
is four codes: `0` success, `1` handled error, `2` launcher `BootstrapError`,
and `3` quiescence refusal. Code `3` is the code this ceremony adds.

`--acknowledge-writers-quiesced` records a procedural fence. It proves no
process state; see "Capture barrier" in [the design](docs/design.md). Byte
equality across the capture pair in Phase 3 is the only proof of quiescence.

### Phase 3 — Capture (operator, from a plain terminal)

1. Run `agent-capture --role source` with the live home, Git root, path maps,
   seats, and `--acknowledge-writers-quiesced`, into `source-a.json`.
2. Repeat into `source-b.json`. Allow no writer between the two runs.
3. Run `--role destination` twice on Sting with the live destination home and
   Git root and the same path maps.
4. Neo pulls both destination captures over its own outbound strict SSH. Sting
   never opens a connection to Neo.
5. Use the exact longest-prefix maps listed under "Minimal operator sequence".
6. Declare the migration agent's own provider state as a reviewed managed
   exclusion on both roles. Never declare the whole home.

### Phase 4 — Plan (migration agent; operator reviews)

1. Run `agent-plan` over the four captures.
2. If the four captures exceed 512 MiB in total, push both source captures to
   Sting and compile the plan there.
3. Review `ready`, every blocker, every hold, every ref action, every auth or
   seat operation, every union, the capacity formula, and the exact
   `plan_sha256`.
4. Read the migration agent's self-exclusion in the plan before accepting the
   digest. An unnamed self-exclusion is a review failure.
5. Accept the digest by writing it down. Every later command binds it.

### Phase 5 — Preseed (writers may resume)

1. Run `agent-stage --phase preseed` in the three transport modes: destination
   `prepare`, Neo `push`, destination `materialize`.
2. The operator may reopen sessions during preseed. Preseed writes only inside
   the external stage root. No live destination path changes.
3. Close the sessions again before Phase 6.

### Phase 6 — Seal final (operator and migration agent)

1. Repeat Phase 1 and Phase 2. This is the second quiet window, and it costs the
   same tens of minutes.
2. Take fresh source and destination A/B captures.
3. Compile a fresh plan. Accept a fresh digest.
4. Repeat `prepare`, `push`, and `materialize` with `--phase final`.
5. Keep the writers stopped through Phase 7. Only the final materialized receipt
   carries `ready_for_apply=true`.

### Phase 7 — Apply and verify (migration agent)

1. Run `agent-apply` with the plan, the sealed final stage receipt, the accepted
   digest, a journal, and a rollback root.
2. Run `agent-verify` over the plan, the stage receipt, and the apply receipt.
3. Require `verified=true` and `failures=[]`.
4. The receipt keeps `provider_runtime_acceptance_verified=false`. An offline
   receipt never proves authentication. Phase 8 performs the attended turn.

### Phase 8 — Land (operator)

1. Connect to Sting. Open the seat session first with `tmux new -A -s main`.
2. Name every work lane by purpose, as `egreg-<repo>` or `codex-<repo>`.
3. Start eGreg with `EMACS_DAEMON=<repo> et <path>`. No `emacsclient` is on PATH.
4. Perform the attended provider turn: one `codex resume`, then an attended
   Claude login. Claude auth is a nonportable hold.
5. Verify the MCP plane and the Linear connector with the lab recipes named in
   lab's `docs/operations/REMOTE_DEV_WORKFLOW.md`.
6. Lab's `docs/operations/STING_FIRST_HOUR.md` is the landing guide for the
   first hour. This repository links to it and does not restate it.

### Phase 9 — Aftercare (operator)

1. Keep Neo intact. Bulkload never deletes source data. Neo is the rollback
   source for seven days.
2. Retain the plan, the stage, the journal, the rollback root, and every receipt
   until disposition.
3. Restart the Neo writers in the exact reverse of the Phase 1 wind-down record.
4. Start one `codex resume` per state directory. One session owns one directory.
5. Do not enable TCFS on Sting. Sting stays a seat-only host, and the Linux end
   of the TCFS pilot is honey.
6. Lab's `docs/operations/REMOTE_DEV_WORKFLOW.md` owns the restart detail and
   the remote-development surface. Read it beside the record from Phase 1.

### Phase R — Rollback

1. Run `agent-rollback` with the exact apply-receipt digest. Rollback is
   attended.
2. After an interrupted journal, choose exactly one path: `agent-recover
   --strategy forward` or `--strategy rollback`.
3. Never hand-edit a plan, a stage manifest, a journal, or a receipt.
4. Never delete a journal after a crash. Never improvise a file copy.

## Development

The repository uses Bazel as the build/test source of truth and the
GloriousFlywheel wrapper for the normal attached path:

```bash
nix develop
just check
```

For an explicitly source-only local fallback:

```bash
just check-local
```

Useful direct gates:

```bash
just skill-validate
just repo-manifest-validate
just python-lint
just shell-lint
just workflow-lint
```

### Consuming the module

```text
bazel_dep(name = "bulkload", version = "0.2.0")
```

`MODULE.bazel` declares `module(name = "bulkload", version = "0.2.0",
compatibility_level = 1)`. The public targets are `//:bulkload`, the isolated
`-I -S` launcher binary; `//:bulkload_lib`; `//:tests`; and `//:skill_bundle`.

The module supports the seven consolidated commands, the nine product schemas,
the receipt and journal contracts, and the installed Skill bundle. The module
does not install a machine seat, does not own Home Manager, TCFS, or a terminal
multiplexer, and does not perform the ceremony. The ceremony is operator work.

`scripts/install-skill.sh --all` creates a real directory at
`~/.agents/skills/bulkload` and a relative symlink at `~/.claude/skills/bulkload`.
Check an existing install with `scripts/install-skill.sh --doctor`. The
installer refuses a foreign symlink, refuses a non-identical existing path
without `--force`, and validates the bundle before it touches anything.

Every ceremony surface carries an `Applies to:` header line naming the module
version, the ceremony revision, and the review date. The
`ceremony_contract_test` target
binds that declared version to `MODULE.bazel`, holds the ceremony to the public
command surface, and rejects a process-control verb written as an agent
instruction. The invariant is a test, not a paragraph.

`v0.1.0` is an annotated, GPG-signed, verified tag, and CI triggers on a `v*`
tag push. `main` carries no required status checks today, so a tag is not proof
that CI passed. Cite the tag; do not cite a release document.

## Minimal operator sequence

Install the same Bulkload closure on Neo and Sting. Resolve an explicit GNU
rsync from each pinned dev shell; capture binds its path, hash, protocol, and
features. Native Neo `/usr/bin/rsync` is not accepted.

Capture source and destination A/B under brief quiescence with identical
managed exclusions and source maps. The longest-prefix live mappings are:

```text
/Users/jess/git     -> /srv/fast-local/jess/git
/Users/jess/.codex  -> /srv/fast-local/jess/state/codex
/Users/jess/.claude -> /srv/fast-local/jess/state/claude
/Users/jess/.gstack -> /srv/fast-local/jess/state/gstack
/Users/jess          -> /home/jess
```

Sting captures the logical `/home/jess` install links and their physical
backings; apply never replaces `.codex`, `.claude`, `.gstack`, or `git` links.
Use repeatable `--seat` for Claude Desktop, Emacs, gregs-org, Atuin, and gstack;
use `--file-seat` for `.bash_history` and optional-absent `.zsh_history`. Never
declare the whole home. Supply the exact Lab authority exclusions on both roles,
including the managed Codex leaves and Pi `AGENTS.md`/`APPEND_SYSTEM.md`.

Before planning, Neo pulls both destination captures over its existing outbound
SSH authentication. `DEST_RSYNC` is the exact immutable Nix-store path passed
to destination capture; no connection or credential originates on Sting:

```bash
SSH_BIN=/usr/bin/ssh
SSH_RSH="$SSH_BIN -oBatchMode=yes -oStrictHostKeyChecking=yes -oClearAllForwardings=yes"
SOURCE_A=/secure/evidence/source-a.json
RSYNC="$(python3 -I -S -c 'import json,sys; print(json.load(open(sys.argv[1]))["catalog"]["transport"]["rsync"]["path"])' "$SOURCE_A")"
test "$(python3 -I -S -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$RSYNC")" = "$(python3 -I -S -c 'import json,sys; print(json.load(open(sys.argv[1]))["catalog"]["transport"]["rsync"]["sha256"])' "$SOURCE_A")"
DEST_RSYNC='/nix/store/REVIEWED_HASH-rsync-REVIEWED_VERSION/bin/rsync'
python3 -I -S -c 'import re,sys; assert re.fullmatch(r"/nix/store/[0-9a-z]{32}-rsync-[A-Za-z0-9._+-]+/bin/rsync", sys.argv[1])' "$DEST_RSYNC"

RSYNC_RSH="$SSH_RSH" "$RSYNC" -a --checksum --delay-updates \
  --no-devices --no-specials --rsync-path="$DEST_RSYNC" \
  jess@sting:/home/jess/.bulkload-evidence/destination-a.json \
  /secure/evidence/destination-a.json
RSYNC_RSH="$SSH_RSH" "$RSYNC" -a --checksum --delay-updates \
  --no-devices --no-specials --rsync-path="$DEST_RSYNC" \
  jess@sting:/home/jess/.bulkload-evidence/destination-b.json \
  /secure/evidence/destination-b.json
```

`agent-plan` validates all four self-digested captures. Accept its exact digest
only after review; then resume writers for preliminary preseed.

Relay the accepted preliminary plan and all receipts over connections initiated
by Neo. Sting never needs a Neo credential or a Sting-originated connection.
With owner-private evidence directories already established:

```bash
SSH_BIN=/usr/bin/ssh
SSH_RSH="$SSH_BIN -oBatchMode=yes -oStrictHostKeyChecking=yes -oClearAllForwardings=yes"
PLAN=/secure/evidence/preliminary-plan.json
PLAN_SHA256='REVIEWED_PRELIMINARY_PLAN_SHA256'
STAGE=/srv/fast-local/jess/bulkload/stage

python3 -I -S -c 'import hashlib,json,sys; value=json.load(open(sys.argv[1])); recorded=value.pop("plan_sha256"); expected=hashlib.sha256(json.dumps(value,allow_nan=False,ensure_ascii=False,separators=(",",":"),sort_keys=True).encode()).hexdigest(); assert recorded == expected == sys.argv[2]' "$PLAN" "$PLAN_SHA256"
plan_binding() {
  python3 -I -S -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]]["catalog"]["transport"]["rsync"][sys.argv[3]])' \
    "$PLAN" "$1" "$2"
}
RSYNC="$(plan_binding source path)"
DEST_RSYNC="$(plan_binding destination path)"
test "$(python3 -I -S -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$RSYNC")" = "$(plan_binding source sha256)"
python3 -I -S -c 'import re,sys; assert re.fullmatch(r"/nix/store/[0-9a-z]{32}-rsync-[A-Za-z0-9._+-]+/bin/rsync", sys.argv[1])' "$DEST_RSYNC"

RSYNC_RSH="$SSH_RSH" "$RSYNC" -a --checksum --delay-updates \
  --no-devices --no-specials --rsync-path="$DEST_RSYNC" \
  "$PLAN" \
  jess@sting:/home/jess/.bulkload-evidence/preliminary-plan.json
```

On Sting, `prepare` creates the absent stage/quarantine at mode 0700 and runs
the destination capacity gate:

```bash
bulkload agent-stage --phase preseed --transport-mode prepare \
  --plan /home/jess/.bulkload-evidence/preliminary-plan.json \
  --accept-plan-sha256 "$PLAN_SHA256" --stage-root "$STAGE" \
  --output /home/jess/.bulkload-evidence/preseed-prepare.json
```

Neo pulls that exact receipt over its existing outbound authentication, then
pushes the digest-bound allowlist and bytes. No destination live path changes:

```bash
RSYNC_RSH="$SSH_RSH" "$RSYNC" -a --checksum --delay-updates \
  --no-devices --no-specials --rsync-path="$DEST_RSYNC" \
  jess@sting:/home/jess/.bulkload-evidence/preseed-prepare.json \
  /secure/evidence/preseed-prepare.json

bulkload agent-stage --phase preseed --transport-mode push \
  --plan /secure/evidence/preliminary-plan.json \
  --accept-plan-sha256 "$PLAN_SHA256" --stage-root "$STAGE" \
  --destination-ssh-host jess@sting \
  --prepare-receipt /secure/evidence/preseed-prepare.json \
  --output /secure/evidence/preseed-push.json
```

On Sting, materialize only from the chained prepare/push receipts:

```bash
bulkload agent-stage --phase preseed --transport-mode materialize \
  --plan /home/jess/.bulkload-evidence/preliminary-plan.json \
  --accept-plan-sha256 "$PLAN_SHA256" --stage-root "$STAGE" \
  --prepare-receipt "$STAGE/.prepare-receipt-preseed.json" \
  --transport-receipt "$STAGE/.transport-receipt-preseed.json" \
  --allow-accounted-copy \
  --output /home/jess/.bulkload-evidence/preseed-stage.json
```

Changed-late preliminary entries are deferred. For final, quiesce again, take
fresh source/destination A/B captures, compile and accept a fresh plan digest,
and repeat `prepare`, Neo `push`, and Sting `materialize` with `--phase final`.
The same stage reuses verified content objects across plan digests, but every
final operation is restaged and only the final materialized receipt is apply
authority.

Apply and verify:

```bash
bulkload agent-apply \
  --plan /home/jess/.bulkload-evidence/final-plan.json \
  --stage-receipt /home/jess/.bulkload-evidence/final-stage.json \
  --accept-plan-sha256 PLAN_SHA256 \
  --journal /home/jess/.bulkload-evidence/apply-journal.json \
  --rollback-root /srv/fast-local/jess/bulkload/rollback \
  --output /home/jess/.bulkload-evidence/apply-receipt.json

bulkload agent-verify \
  --plan /home/jess/.bulkload-evidence/final-plan.json \
  --stage-receipt /home/jess/.bulkload-evidence/final-stage.json \
  --apply-receipt /home/jess/.bulkload-evidence/apply-receipt.json \
  --output /home/jess/.bulkload-evidence/verify-receipt.json
```

Use `agent-recover --strategy forward|rollback` after an interrupted journal,
or `agent-rollback` with the exact apply receipt digest for an attended revert.
The full command and custody contract is in the installed Skill.
