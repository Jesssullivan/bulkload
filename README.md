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

## Progress

Every verb prints its own progress on stderr with no flags: a `bulkload-run`
banner at both ends of the invocation, a `bulkload-progress` heartbeat every
30 s *during* long phases with `done`/`total`/`bytes`/`rate`/`eta`, and one
`bulkload-phase` line per phase at exit. `--progress-log PATH` duplicates the
same lines into a file — refused if it would land under a root the running
verb owns — `--heartbeat-seconds N` retunes the interval, and `--quiet` turns
stderr telemetry off. `-` in any field means "not measured", so a measured
zero reads `0`. None of it reaches an artifact, changes a digest, or can fail
a run.

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

## Exit codes

Every Bulkload process returns one of these. They are claim-bearing: an
orchestrator can branch on them without parsing a message.

| Code | Meaning | What it tells the caller |
|---|---|---|
| `0` | the verb completed and its evidence was written | continue the sequence |
| `1` | usage error on the command line | fix the invocation; `--help` and `--version` still exit `0` |
| `2` | the pinned launcher could not build its source closure | the install is broken or a runtime source moved under it; reinstall the skill |
| `3` | quiescence/epoch refusal: the live source moved under the capture | retryable. No live path was mutated. Re-capture. Raised at the coarse epoch fences only -- see below |
| `4` | custody, plan, or typed-invariant refusal | the general fail-closed class. Read the message; a blind retry will refuse identically |
| `5` | storage refusal: a volume could not hold or reflink-clone what was charged against it | the message names the path. It may be the destination stage, the rollback custody root, or the **source** host's own live-snapshot custody. There is no full-copy fallback |
| `6` | unexpected internal error, including malformed input evidence | the input is not the document it claims to be. The traceback is always printed: this is the one class with no curated message |
| `130` | the operator interrupted the process | Bulkload never signals a process; only the operator does. Use the durable journal and `agent-recover` |

`3` and `5` are narrowings of `4`, raised where the engine can prove the more
specific cause. Any refusal that is not proven to be one of those exits `4`.

**`3` is raised at the coarse epoch fences, not per entry.** Those fences are a
declared root that changed during capture (`scanner.py:434`), a declared file
seat that changed (`:1946`), a live snapshot whose census, payload, or Git
authority moved under it (`:2095`, `:2193`-`:2278`, `:3824`-`:3832`), live
source movement after immutable snapshot B (`:3234`, `:3296`), and an A/B pair
that is not stable (`:4765`-`:4784`, and the streaming equivalent that
`agent-plan` runs). A *single filesystem entry* that changes or is unsupported
mid-walk is deliberately **not** exit `3`: the directory walk records it as a
capture blocker and the capture finishes with `complete: false` and exit `0`
(`scanner.py:547`, `:577`), because aborting a 1.8M-entry live capture on one
churned file would make a live host uncapturable. `agent-plan` then refuses
that capture with `4` ("captures contain blockers"). Per-entry churn on a busy
host is visible in the evidence, not in the process status.

**`5` is volume-neutral.** The same capacity gate guards the destination stage,
the rollback custody root, and the source host's own live-snapshot custody --
`agent-capture` reaches it (`scanner.py:3794`) and that verb has no destination
at all. `ENOSPC`/`EDQUOT` from any write, including the evidence document, maps
here too. The message therefore always names the path; read the host off the
path rather than off the code.

## Dry run and progress

`--dry-run` is available on `agent-capture`, `agent-plan`, `agent-stage`, and
`agent-apply`. It prints a JSON report on stdout naming the exact mutations the
verb would perform, the refusals it can already reach, and -- in
`not_evaluated` -- the gates it did not reach. It writes nothing at all: not
the stage, not the journal, not the `--output` evidence document. Its
`exit_code` field is the status the process returns, so a dry run is usable as
a gate.

**Read `complete` first.** It is `false` whenever the rehearsal refused before
it finished enumerating. On a refusal the `mutations` list is a prefix and not
an inventory, and `live_destination_mutations` is a floor rather than a count;
`not_evaluated` is seeded before the first fence runs, so it names the skipped
gates on that path too. A `complete: true` report with an empty `mutations`
list is the only one that means "this verb mutates nothing".

```bash
bulkload agent-apply --dry-run \
  --plan /home/jess/.bulkload-evidence/final-plan.json \
  --stage-receipt /home/jess/.bulkload-evidence/final-stage.json \
  --accept-plan-sha256 PLAN_SHA256 \
  --journal /home/jess/.bulkload-evidence/apply-journal.json \
  --rollback-root /srv/fast-local/jess/bulkload/rollback \
  --output /home/jess/.bulkload-evidence/apply-receipt.json
```

`--progress` is **on by default**, including when stderr is a pipe or a log
file -- a redirected multi-hour push writing zero bytes for two and a half
hours is the case that motivated it, and a terminal-only default would have
left exactly that case silent. It writes phase and heartbeat lines to stderr,
never to stdout, so evidence on `-` stays byte-exact. Pass `--no-progress` when
you need byte-exact stderr. It reports; it never signals a process.

`--progress` carries liveness -- command, phase, elapsed -- and no counters.
The channel that carries files and bytes is the separate `phase_timing` record,
still gated behind `BULKLOAD_PHASE_TIMING=1`; set it as well when you want per
phase counts. Making that channel default-on is a separate change.

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
