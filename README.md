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
and [agent-state guide](.agents/skills/bulkload/references/agent-context.md).

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

## Minimal operator sequence

Install the same Bulkload closure on Neo and Sting. Resolve an explicit GNU
rsync from each pinned dev shell; capture binds its path, hash, protocol, and
features. Native Neo `/usr/bin/rsync` is not accepted.

Capture source and destination immutable-live A/base-B pairs with identical
managed exclusions and source maps. Do not stop or signal sessions. Pass A's
exact `snapshot-seal.json` to B with `--snapshot-base-seal`; B reuses sealed A
objects and contains only charged deltas. The longest-prefix live mappings are:

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

`agent-plan` validates all four self-digested captures, each B-to-A seal
binding, static contract equality, and absence of A-only loss. Accept its exact
digest only after review; preliminary preseed permits continued live work.

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

Neo pulls that exact receipt *and* the sealed allowlist `prepare` wrote beside
the stage root, then pushes the digest-bound allowlist and bytes. `push` takes
the prepare receipt, the allowlist, and the destination host; passing `--plan`
or `--transport-receipt` to `push` is rejected. Preserve the allowlist's 0600
mode on the pull. No destination live path changes:

```bash
RSYNC_RSH="$SSH_RSH" "$RSYNC" -a --checksum --delay-updates \
  --no-devices --no-specials --rsync-path="$DEST_RSYNC" \
  jess@sting:/home/jess/.bulkload-evidence/preseed-prepare.json \
  /secure/evidence/preseed-prepare.json
RSYNC_RSH="$SSH_RSH" "$RSYNC" -a --checksum --delay-updates \
  --no-devices --no-specials --rsync-path="$DEST_RSYNC" \
  "jess@sting:$STAGE/.transport-allowlist-preseed.nul" \
  /secure/evidence/.transport-allowlist-preseed.nul

bulkload agent-stage --phase preseed --transport-mode push \
  --accept-plan-sha256 "$PLAN_SHA256" --stage-root "$STAGE" \
  --destination-ssh-host jess@sting \
  --prepare-receipt /secure/evidence/preseed-prepare.json \
  --transport-allowlist /secure/evidence/.transport-allowlist-preseed.nul \
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

Changed-late preliminary entries are deferred. For final, take fresh
source/destination A/base-B captures, compile and accept a fresh plan digest,
then begin an attended no-interaction interval before Neo source B. Processes
remain alive. Repeat `prepare`, Neo `push`, and Sting `materialize` with
`--phase final`; Neo proves two matching complete live epochs before and after
transport and returns no final push receipt on drift. Keep the interval through
apply and Sting verification. Neo pulls that exact verification receipt and
runs `agent-verify --destination-verify-receipt`; only its sealed
cutover-release receipt ends the interval.
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

After Neo pulls that exact Sting receipt over its outbound connection, release
the cutover on Neo without stopping any session:

```bash
bulkload agent-verify \
  --plan /secure/evidence/final-plan.json \
  --stage-receipt /secure/evidence/final-stage.json \
  --apply-receipt /secure/evidence/apply-receipt.json \
  --destination-verify-receipt /secure/evidence/verify-receipt.json \
  --output /secure/evidence/cutover-release.json
```

This receipt is an observational release cut. It proves Neo still equals sealed
source B after Sting verification; it does not claim future Neo writes cannot
occur after the interaction interval ends.

Use `agent-recover --strategy forward|rollback` after an interrupted journal,
or `agent-rollback` with the exact apply receipt digest for an attended revert.
The full command and custody contract is in the installed Skill.
