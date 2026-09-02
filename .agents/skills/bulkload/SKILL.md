---
name: bulkload
description: Capture, plan, stage, apply, verify, roll back, and recover a typed one-way Git and agent-state cutover with AgentCaptureV4 and GitWorkspaceV2.
---

# Bulkload

Use Bulkload for a reviewed machine cutover that must retain a `~/git` fleet,
linked worktrees, exact index/dirt state, non-Git fleet content, Codex, Claude,
Pi, or declared mutable-seat state. The supported runtime is Python 3.11 or
newer, Git, SQLite, and a filesystem with verified reflinks for live rollback.

Read [the migration contract](references/migration-contract.md) and
[agent-state guide](references/agent-context.md) before creating a plan.

## Keep the boundary explicit

1. Keep Codex, Claude, Pi, Emacs, and terminal sessions alive. Capture A into
   immutable custody, then capture B with A's explicit `snapshot-seal.json` as
   its base. For final, begin a no-interaction interval before source B and keep
   it through Sting verification and Neo's cutover-release verification;
   Bulkload proves two matching live epochs before and after transport, then
   two more after destination verification, and never signals a session.
2. Declare the source and destination home/Git maps. The longest source prefix
   wins, so map `/Users/jess/git` explicitly to
   `/srv/fast-local/jess/git` in addition to any broader home map.
3. Declare every directory seat and regular-file singleton. A whole-home seat
   is invalid; an absent optional file is captured as such.
4. Keep evidence, stage, journal, and rollback roots outside every live root.
5. Never put credential values, SQLite rows, raw symlink targets, or
   unsanitized remote URLs in output or chat.
6. Never invoke a terminal multiplexer, TCFS runtime, Home Manager, deploy,
   activation, browser migration, or credential rotation from this workflow.

`AgentCaptureV4` knows Codex, Claude, Pi, and mutable-seat state. Ordinary
descendants of a declared provider root default to byte-exact portable-private
state. Exact managed exclusions and regenerate trees are pruned first; unsafe
special entries, typed-state ambiguity, and structural collisions block. Codex
and Pi auth are typed source-authority operations. Claude
auth is nonportable: preserve it as a hold and authenticate on the destination
attended. Offline receipts do not prove provider acceptance.

## Watch it run

Every verb narrates itself on stderr. There is nothing to turn on.

- `bulkload-run event=start verb=... version=... pid=... host=...` opens each
  invocation and `bulkload-run event=end verb=... seconds=... status=... exit=...`
  closes it, so no invocation can leave a zero-byte log behind.
- `bulkload-progress phase=... root=... seconds=... unit=... done=... total=...
  bytes=... rate=... eta=...` is a heartbeat emitted *during* a long phase,
  every 30 s by default. `rate` is `unit` per second and `eta` is seconds
  remaining; both read `-` until the phase can measure them. The payload push
  is opaque from the source side, so its heartbeat carries elapsed time against
  the sealed charge rather than an invented byte position.
- `bulkload-phase phase=... root=... seconds=... files=... bytes=...` closes
  each phase. `files` is an entry count and `bytes` is a byte total; the named
  phases are `base-custody` (which encloses `custody-base`), `custody*`,
  `census`, `charge`, `generation-pre`, `copy`, `recensus`,
  `generation-post`, `digest`, `catalog`, `seal`, `witness`, `validate`,
  `stage`, `stage-objects`, `push`, `apply`, `verify`. Phases nest, so two
  lines can cover the same seconds.
- `bulkload-phase phase=UNACCOUNTED seconds=...` closes a run whose timed
  phases covered under 90% of its wall clock, and names the remainder. It is
  the instrument admitting where it is blind; treat it as a profiling lead,
  not a fault.

In every line `-` means "not measured". A field that was measured prints its
value, `0` included: `bytes=0` is a measured zero and `bytes=-` is no
measurement at all.

Flags, on every verb:

- `--progress-log PATH` appends the same lines to a file an unattended agent
  can tail. It is refused if it would land under any live, stage, snapshot or
  rollback root of the verb that is running — including the destination roots
  that only a plan or a stage receipt names, which is why the file is not
  created until that evidence has been read and has cleared it. A verb that
  refuses before it can read that evidence writes the refusal to stderr and
  creates no file at all.
- `--heartbeat-seconds N` changes the heartbeat interval (default 30).
- `--quiet` suppresses stderr telemetry. `BULKLOAD_PHASE_TIMING=0` does the
  same for a caller that cannot reach the flags.

Telemetry cannot fail a run. Every sink write is guarded, and a sink that has
gone away — `2>&-` closes fd 2 and CPython then sets `sys.stderr` to `None` —
is skipped rather than allowed to replace the verb's own refusal.

Telemetry is diagnostic only: it never reaches an artifact and never changes a
digest.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | the verb completed and its evidence was written |
| `1` | usage error on the command line (`--help`/`--version` still exit `0`) |
| `2` | the pinned launcher could not build its source closure |
| `3` | quiescence/epoch refusal: the live source moved under the capture, at a coarse fence (root, seat, snapshot, A/B pair). Retryable; no live path was mutated |
| `4` | custody, plan, or typed-invariant refusal -- the general fail-closed class |
| `5` | storage refusal: a volume could not hold or reflink-clone what was charged; the message names the path (it may be the **source** host's snapshot custody) |
| `6` | unexpected internal error, including malformed input evidence; the traceback is always printed |
| `130` | the operator interrupted the process. Bulkload never signals one |

Branch on the code, not on the message. `3` and `5` are narrowings of `4`,
raised only where the engine proved the specific cause; every other refusal is
`4`.

A single filesystem entry that changes or is unsupported mid-walk is **not**
exit `3`. It is recorded as a capture blocker, the capture exits `0` with
`complete: false`, and `agent-plan` then refuses it with `4`. Exit `3` means a
coarse fence moved: a declared root or file seat, a live snapshot's census or
Git authority, or the A/B pair itself.

Exit `5` names a volume, not a host. `agent-capture` reaches the same capacity
gate for its own live-snapshot custody on the source, so a full source disk
exits `5` as well. Read the path out of the message before deciding which
machine to look at.

## Rehearse before a long verb

`--dry-run` on `agent-capture`, `agent-plan`, `agent-stage`, and `agent-apply`
prints a JSON report of the exact mutations that verb would perform, the
refusals it already reaches -- the overlap fence, the argument shape, the
accepted plan digest, the stage receipt -- and, in `not_evaluated`, the gates
it did not reach. It writes nothing: not the stage, not the journal, not the
`--output` document. Its `exit_code` field is the process status, so use it as
a gate before a multi-hour run.

Read `complete` before `mutations`. A report that refused early carries
`complete: false`, and its `mutations` list is a prefix rather than an
inventory. Only `complete: true` with an empty list means the verb mutates
nothing.

`--progress` is on by default on every stderr, a redirected log included --
that is the case a silent hour actually happens in. `--no-progress` suppresses
it. Lines go to stderr only, so evidence on stdout stays byte-exact, and a
silent hour stops being indistinguishable from a hang. Progress reports; it
never signals a session.

`--progress` names the verb's coarse phases from the CLI; the per-root phase
and heartbeat lines described in **Watch it run** come from the engine. Both
are on by default, both write only to stderr, and either `--no-progress` or
`--quiet` silences both -- byte-exact stderr is one promise, not two.
`--progress-log` is unaffected: a silenced stderr still fills the file an
unattended agent tails.

## Capture twice per role

Run the canonical launcher directly or the Bazel-built `bulkload` binary. Both
enter Python with `-I -S` and pin the complete application-source closure
before importing the CLI.

```bash
scripts/bulkload.py agent-capture \
  --role source \
  --home /Users/jess \
  --git-root /Users/jess/git \
  --rsync-path /absolute/pinned/gnu-rsync \
  --path-map /Users/jess=/home/jess \
  --path-map /Users/jess/git=/srv/fast-local/jess/git \
  --output /secure/evidence/source-a.json
```

Repeat into `source-b.json` with
`--snapshot-base-seal /secure/evidence/.source-a.json.snapshot/snapshot-seal.json`.
That base is admitted with `--base-custody full` by default, which re-hashes
every byte of A while capturing B. `--base-custody sealed` admits it on A's
already-verified seal digest, index digest, and namespace digest instead, and
still re-derives the git root and every SQLite payload byte for byte.

Capture also records `source_path_folding` in the snapshot seal: a read-only
measurement of whether the source filesystems hold two case spellings, or two
Unicode normal forms, of one name as one directory entry. A destination folds
custody paths on exactly those axes and only when the seal says the source
does; an unmeasured, false, or contradicted axis leaves the required/seen
comparison byte-exact. Seals written before this field exists carry no licence
and stay byte-exact. Because the measurement is part of the seal, a catalog
must never be paired with a runtime whose folding rules differ from the one
that sealed it -- see the break-glass note in `scripts/bulkload.py`.

On the destination,
use `--role destination`, the live destination `--home` and `--git-root`, and
the same source-to-destination path maps. Repeat into two destination files.
Use exact provider-specific maps for `.codex`, `.claude`, and `.gstack` backing
paths in addition to the home/Git maps. Root translation uses the logical path;
reads/writes use the nofollow-proven physical backing, so Home Manager links are
never replaced. Obtain `--rsync-path` from the pinned flake shell; capture binds
its absolute path, hash, protocol, and features. Add identical reviewed
`--managed-exclusion PROVIDER:RELATIVE` values on both roles. Add directory
state with `--seat NAME=/absolute/path` and singleton history with
`--file-seat NAME=/absolute/file`.

Before `agent-plan`, Neo pulls destination A/B into the owner-private evidence
paths above through its outbound strict-SSH connection. Use the exact source
rsync binding from source capture and the immutable Nix-store rsync path passed
to destination capture. Sting never opens a connection or holds a Neo
credential; the accepted four-capture plan subsequently binds both tools.

Capture records hashes and typed metadata only. SQLite capture uses the backup
API and includes committed WAL state. Readable invalid Git candidates and
alternates/fsck-only repositories retain exact non-Git bytes; malformed stable
append JSONL retains exact portable-private bytes. Scan failures, unsafe schema
or Git authority, non-runtime special entries, active Git operations,
unsupported typed state, and budget overruns remain incomplete. Unknown-named
files inside a declared provider root are known portable-private bytes; only
explicitly typed Claude path-bearing text rewrites.

## Compile and review

```bash
scripts/bulkload.py agent-plan \
  --source-a /secure/evidence/source-a.json \
  --source-b /secure/evidence/source-b.json \
  --destination-a /secure/evidence/destination-a.json \
  --destination-b /secure/evidence/destination-b.json \
  --output /secure/evidence/plan.json
```

Planning requires four unique capture IDs, identical static contracts, B's
exact A-seal binding, and no A-only identity lost from B. B is plan authority.
Review:

For a four-capture set larger than 512 MiB, push both source captures from Neo
to the private Sting evidence directory and run `agent-plan` on Sting beside
the destination captures. The command fails closed unless Linux reports the
required memory reserve; do not raise the bound, add swap, or reuse captures.
Every transfer still originates on Neo.

- `ready` and every blocker;
- known `holds`, especially Claude auth;
- every Git ref/recovery action and worktree target;
- every source-authority auth or mutable-seat operation;
- every append/SQLite union;
- the capacity formula; and
- the exact `plan_sha256`.

Source bytes, Git dirt/deletions, non-Git files, portable state, and divergent
append state overwrite with exact `destination_before` rollback custody.
Compatible SQLite rows union; schema/shared-row/key ambiguity uses a reversible
source snapshot. Destination ref divergence is retained at a deterministic
`refs/bulkload/recovery/destination/*` ref before update. Destination-only
union members remain.

## Preseed without touching live state

The stage protocol is destination `prepare`, source `push`, destination
`materialize`. Install the same closure on both hosts. Keep the accepted plan
on Sting, with the source and destination GNU rsync paths and hashes bound in
that plan; rehash the source executable before transport, require the destination's
immutable Nix-store path, and let destination `prepare` rehash it locally. Use
`--delay-updates`, strict SSH, and exact owner-private paths. The push relies on
rsync's size+mtime quick check because the destination independently re-derives
every transported digest before those bytes gain apply authority; pass
`--transport-checksum` only to make the transport itself fail earlier. Every
connection originates on Neo: Neo may pull destination A/B captures and the
prepare receipt over that connection, but Sting never authenticates to Neo.

`--mover` selects the payload mover for `--transport-mode push` and defaults to
`rsync`, the shipped single-stream path. `--mover native` uses the
content-addressed parallel mover (`--transport-streams`, default 8): one
transfer per sha256 rather than per path, resumable by re-offering every
object. It is a prototype, it has not been run against a live destination, and
it does not preserve mtimes, hardlink topology, or xattrs. Read
`docs/design/native-mover.md` before using it; §3 there records that it is a
fleet-synchronous runtime change.

On Sting:

```bash
scripts/bulkload.py agent-stage --phase preseed --transport-mode prepare \
  --plan /home/jess/.bulkload-evidence/preliminary-plan.json \
  --accept-plan-sha256 PLAN_SHA256 \
  --stage-root /srv/fast-local/jess/bulkload/stage \
  --output /home/jess/.bulkload-evidence/preseed-prepare.json
```

On Neo, after pulling that exact receipt and its sibling
`.transport-allowlist-preseed.nul` over the same Neo-originated connection:

```bash
scripts/bulkload.py agent-stage --phase preseed --transport-mode push \
  --accept-plan-sha256 PLAN_SHA256 \
  --stage-root /srv/fast-local/jess/bulkload/stage \
  --destination-ssh-host jess@sting \
  --prepare-receipt /secure/evidence/preseed-prepare.json \
  --transport-allowlist /secure/evidence/preseed-allowlist.nul \
  --output /secure/evidence/preseed-push.json
```

Back on Sting:

```bash
scripts/bulkload.py agent-stage --phase preseed --transport-mode materialize \
  --plan /home/jess/.bulkload-evidence/preliminary-plan.json \
  --accept-plan-sha256 PLAN_SHA256 \
  --stage-root /srv/fast-local/jess/bulkload/stage \
  --prepare-receipt /srv/fast-local/jess/bulkload/stage/.prepare-receipt-preseed.json \
  --transport-receipt /srv/fast-local/jess/bulkload/stage/.transport-receipt-preseed.json \
  --allow-accounted-copy \
  --output /home/jess/.bulkload-evidence/preseed-stage.json
```

`prepare` creates the absent owner-private stage/quarantine, writes and seals the
plan-derived NUL allowlist, and gates capacity. Push streams only that sealed
allowlist through compatible captured GNU rsync; it does not load the multi-GiB
plan on Neo. Materialize revalidates the plan, allowlist authority, chained
receipts, and every byte. Changed-late
preliminary entries defer rather than gain apply authority. A failed reflink is
a hard stop. `--allow-accounted-copy` covers only charged incoming/transformed
bytes, never a hidden full rollback copy.

The capacity contract is:

```text
available >= incoming_unique + sqlite_compose + exact_overwritten + reserve
```

Do not place the stage on a non-reflink filesystem. For Sting use the reviewed
XFS authority beneath `/srv/fast-local`.

## Seal the final stage

Keep sessions alive, begin the attended no-interaction interval, take fresh
source/destination A/base-B captures, compile and accept a fresh plan, then
repeat all three modes with `--phase final`. Final may reuse
verified preseed content objects across a different plan digest, but restages
every final-plan operation and binds only that fresh plan. It rechecks source
bytes and destination SQLite and writes no live path. Only the materialized
final receipt has `ready_for_apply=true`. Compatible SQLite rows union; unsafe
schema/shared-key cases stage the source snapshot. Every result passes integrity,
foreign-key, and fresh logical-catalog checks.

## Apply

```bash
scripts/bulkload.py agent-apply \
  --plan /home/jess/.bulkload-evidence/final-plan.json \
  --stage-receipt /home/jess/.bulkload-evidence/final-stage.json \
  --accept-plan-sha256 PLAN_SHA256 \
  --journal /home/jess/.bulkload-evidence/apply-journal.json \
  --rollback-root /srv/fast-local/jess/bulkload/rollback \
  --output /home/jess/.bulkload-evidence/apply-receipt.json
```

Apply first revalidates destination preconditions. Before its first live
mutation it snapshots every exact overwritten entry with a required reflink,
records absent targets and transaction-created parents, seals rollback, and
persists the journal.
File changes use a same-directory reflink temporary, rename, file fsync, and
directory fsync. Git objects are additive; refs and worktree topology use Git.
Every mutation advances the durable journal.

Re-running a completed apply returns the exact stored receipt. Do not edit a
plan, stage manifest, journal, or receipt by hand.

## Verify

```bash
scripts/bulkload.py agent-verify \
  --plan /home/jess/.bulkload-evidence/final-plan.json \
  --stage-receipt /home/jess/.bulkload-evidence/final-stage.json \
  --apply-receipt /home/jess/.bulkload-evidence/apply-receipt.json \
  --output /home/jess/.bulkload-evidence/verify-receipt.json
```

Verification freshly reads file bytes/modes, exact indexes, refs, worktrees,
Git object integrity, SQLite logical state, removed sidecars, and holds. Require
`verified=true` and `failures=[]`. Neo then pulls that exact receipt over its
existing outbound strict-SSH connection and, while the no-interaction interval
continues, emits the cutover-release receipt:

```bash
scripts/bulkload.py agent-verify \
  --plan /secure/evidence/final-plan.json \
  --stage-receipt /secure/evidence/final-stage.json \
  --apply-receipt /secure/evidence/apply-receipt.json \
  --destination-verify-receipt /secure/evidence/verify-receipt.json \
  --output /secure/evidence/cutover-release.json
```

Release requires the exact successful Sting verify/apply/plan identity and two
matching current Neo generations equal to immutable source B. Only then may
interaction resume on Sting; this is an observational cut at the release
receipt, not a claim that future Neo writes are impossible. Then perform fresh
attended provider picker, resume/dialog, and authentication checks; these remain
outside the offline receipt.

## Roll back or recover

Attended rollback requires the exact apply receipt digest:

```bash
scripts/bulkload.py agent-rollback \
  --apply-receipt /home/jess/.bulkload-evidence/apply-receipt.json \
  --accept-receipt-sha256 APPLY_RECEIPT_SHA256 \
  --output /home/jess/.bulkload-evidence/rollback-receipt.json
```

After interruption, inspect the durable journal and choose exactly one path:

```bash
scripts/bulkload.py agent-recover \
  --plan /home/jess/.bulkload-evidence/final-plan.json \
  --stage-receipt /home/jess/.bulkload-evidence/final-stage.json \
  --journal /home/jess/.bulkload-evidence/apply-journal.json \
  --strategy forward \
  --output /home/jess/.bulkload-evidence/recovery-receipt.json
```

Use `--strategy rollback` to restore the sealed snapshot instead. Both paths
are idempotent and remain bound to the same transaction. Never improvise file
copies or delete a journal after a crash.

## Read a refusal instead of guessing

Every verb accepts `--failure-output PATH`. When the verb refuses, the same
single line goes to stderr first, and then a structured record is written to
that path:

```bash
scripts/bulkload.py agent-stage \
  --phase final --accept-plan-sha256 PLAN_SHA256 \
  --stage-root /srv/fast-local/jess/bulkload/stage \
  --output /srv/fast-local/jess/bulkload/final-stage.json \
  --failure-output /srv/fast-local/jess/bulkload/final-stage.failure.json
```

The record is `{schema, code, phase, root, label, field, expected, observed,
count, sample, remedy, message, command, version}`. `sample` holds at most 20
offending paths or fields. `code` is stable: branch on it, not on the message.
Codes that name a whole condition family end without a suffix; a code with a
suffix names the exact condition, e.g. `CUSTODY_REQUIRED_NOT_SEEN` (the stage
asked for paths the sealed index does not carry — a path-identity or selection
fault, which no re-capture fixes) versus `CUSTODY_INDEX_DIGEST` (the index
bytes moved). A refusal site that has not been converted yet still writes a
record, with code `UNCLASSIFIED`.

`--failure-output` is a write channel, so it is fenced like `--output`: it must
be a real path, it must not resolve to `--output` or to any artifact the verb
reads (`--plan`, `--stage-receipt`, `--prepare-receipt`, `--transport-allowlist`,
`--apply-receipt`, `--journal`, `--snapshot-base-seal`, the four `agent-plan`
captures), and it must not land inside any live root — the record's write
creates its whole parent tree, and a new directory under `--home` mid-capture
refuses that capture with "live snapshot path set changed". Those comparisons
resolve `~`, `..` and symlinks and fold case, because the source volume is
case-insensitive APFS. The record is written 0600. A record write that cannot
happen prints one `WARN` line after the `FAIL` line and never replaces it.

**The limit, stated plainly.** The record covers refusals: `BulkloadError`,
`OSError`, `sqlite3.Error`. A defect in the engine itself — e.g. a plan whose
JSON parses but has the wrong schema, which reaches a bare subscript — still
exits 1 with a Python traceback, no `FAIL` line and no record. Read the exit
status; do not treat a missing record as success. Classifying that class is
the separate exit-code work, not part of this channel.
