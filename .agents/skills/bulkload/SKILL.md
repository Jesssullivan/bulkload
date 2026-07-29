---
name: bulkload
description: Inventory, plan, transfer, and independently verify a one-way migration of one Git repository, its dirty or untracked working state, a collision-gated append-only Codex session union, selected agent transcripts, typed private Codex capture, or a narrow attended Codex auth install, or a fleet under ~/git. Use for dev-box moves, crash recovery, worktree parity audits, Neo-to-Sting-style migrations, rsync replacement planning, or any request to preserve Git and agent context across machines. Keep generic secret copying, raw live database copying, SQLite composition/installation, generated caches, terminal multiplexers, deployment, activation, and bidirectional synchronization out of scope.
---

# Bulkload

Runtime requirement: Python 3.11 or newer, Git, and a Unix-like host with file
locking and dirfd support. Codex evidence file publication requires macOS or
Linux for an OS-backed atomic no-replace rename.

Turn an ad hoc copy into a typed, digest-bound migration with a fresh proof at
every boundary.

## Establish the boundary

1. Read the source and destination repository `AGENTS.md` files.
2. Identify the protected owner checkout and declare it read-only.
3. Separate these state classes before running anything:
   - Git refs and objects;
   - linked-worktree topology;
   - modified, staged, conflicted, deleted, and untracked working bytes;
   - agent transcripts and durable memory;
   - generated indexes and caches;
   - provider-owned SQLite families; and
   - authentication or credentials.
4. Keep credentials and databases outside the generic repository adapter.
   Codex `auth.json` and provider-owned SQLite families are copy-eligible only
   through an explicit typed opt-in plan. Policy v3 implements only attended
   atomic auth replacement while preserving destination SQLite exact with zero
   mutations. SQLite union/composition/installation and combined apply remain
   false. Never log credential values. Any live SQLite WAL, SHM, or rollback
   journal hard-stops immutable capture.
5. Never invoke `cmux` or another terminal multiplexer. Never clean, prune,
   rebase, delete, switch Home Manager, deploy, reconcile, or activate as part
   of this workflow.

Read [references/migration-contract.md](references/migration-contract.md) for
the state matrix and stop conditions. Read
[references/agent-context.md](references/agent-context.md) before handling
Codex, Claude, or Pi state. Read
[references/codex-private-state-policy.v3.json](references/codex-private-state-policy.v3.json)
before classifying Codex auth or SQLite, and use
[references/codex-private-auth-install.md](references/codex-private-auth-install.md)
for the attended private workflow.

## Capture two stable source passes

Run from the installed skill directory or use the Bazel-built `bulkload`
binary:

```bash
python3 scripts/bulkload.py capture \
  --mode repo \
  --root /absolute/source/repo \
  --output /secure/evidence/source-a.json

python3 scripts/bulkload.py capture \
  --mode repo \
  --root /absolute/source/repo \
  --output /secure/evidence/source-b.json
```

Use `--mode fleet --root /absolute/path/to/git` for all discovered repositories
under one root. Do not use `--include-ignored` unless the operator explicitly
needs an inventory of generated state; ignored paths never become copy actions.

Pause writers before capture when possible. A complete scan can still race a
writer, so pass A and pass B must have the same `catalog_sha256`. Treat any
scan error or budget exceedance as incomplete.

Keep capture, plan, and verification evidence outside every local source and
destination root. The repository adapter rejects resolved overlaps and input
aliases before mutation; that pathname guard does not replace private,
quiescent evidence custody. The Codex adapter additionally pins bounded input
descriptors through a create-only atomic publish in its private output
directory. It verifies the exact temporary payload and single-link custody,
uses an OS no-replace rename, revalidates every input and the requested
directory/target immediately after publication, and never overwrites an
existing target. When emitting `-` over SSH, redirect only to a controller-side
path outside the controller's captured roots.

## Capture the destination

Install the same reviewed skill revision on the destination and capture there.
For SSH, redirect canonical JSON back to the controller rather than reading
credential files or copying the whole agent home:

```bash
umask 077
destination_tmp="$(mktemp /secure/evidence/.destination.XXXXXX)"
trap 'rm -f "$destination_tmp"' EXIT
ssh destination-host \
  'python3 ~/.agents/skills/bulkload/scripts/bulkload.py capture --mode repo --root /absolute/destination/repo --output -' \
  > "$destination_tmp"
mv -f "$destination_tmp" /secure/evidence/destination.json
trap - EXIT
```

Keep the shell path literal and operator-reviewed. Do not interpolate an
untrusted hostname or path.

## Catalog a Codex session union

Read [references/agent-context.md](references/agent-context.md), quiesce both
Codex writers, and capture two distinct observations of each role:

```bash
python3 scripts/bulkload.py codex-capture \
  --root /absolute/source/.codex/sessions \
  --role source \
  --host-authority-id 11111111-1111-4111-8111-111111111111 \
  --acknowledge-writers-quiesced \
  --output /secure/evidence/codex-source-a.json
python3 scripts/bulkload.py codex-capture \
  --root /absolute/source/.codex/sessions \
  --role source \
  --host-authority-id 11111111-1111-4111-8111-111111111111 \
  --acknowledge-writers-quiesced \
  --output /secure/evidence/codex-source-b.json
python3 scripts/bulkload.py codex-capture \
  --root /absolute/destination/.codex/sessions \
  --role destination \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --acknowledge-writers-quiesced \
  --output /secure/evidence/codex-destination-a.json
python3 scripts/bulkload.py codex-capture \
  --root /absolute/destination/.codex/sessions \
  --role destination \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --acknowledge-writers-quiesced \
  --output /secure/evidence/codex-destination-b.json
python3 scripts/bulkload.py codex-plan \
  --source-a /secure/evidence/codex-source-a.json \
  --source-b /secure/evidence/codex-source-b.json \
  --destination-a /secure/evidence/codex-destination-a.json \
  --destination-b /secure/evidence/codex-destination-b.json \
  --output /secure/evidence/codex-union-plan.json
```

If that first plan reports `same-uuid-prefix-proof-required`, compile the
immutable request, then capture two more quiescent passes for every role named
as `longer_role` in the request:

```bash
python3 scripts/bulkload.py codex-prefix-request \
  --source-a /secure/evidence/codex-source-a.json \
  --source-b /secure/evidence/codex-source-b.json \
  --destination-a /secure/evidence/codex-destination-a.json \
  --destination-b /secure/evidence/codex-destination-b.json \
  --output /secure/evidence/codex-prefix-request.json

python3 scripts/bulkload.py codex-prefix-proof \
  --prefix-request /secure/evidence/codex-prefix-request.json \
  --source-a /secure/evidence/codex-source-a.json \
  --source-b /secure/evidence/codex-source-b.json \
  --destination-a /secure/evidence/codex-destination-a.json \
  --destination-b /secure/evidence/codex-destination-b.json \
  --root /absolute/source/.codex/sessions \
  --role source \
  --acknowledge-writers-quiesced \
  --output /secure/evidence/codex-source-prefix-a.json
```

Repeat the proof command into a distinct `source-prefix-b.json`. If the request
also names destination-longer observations, repeat both passes with the
destination root, `--role destination`, and distinct destination outputs.
Never manufacture a proof for a role absent from the request.

Keep both writers quiesced. After every required proof pass has completed,
compile one immutable close request that binds the exact request, opening
custody, proof capture IDs, and proof body digests:

```bash
python3 scripts/bulkload.py codex-close-request \
  --prefix-request /secure/evidence/codex-prefix-request.json \
  --source-a /secure/evidence/codex-source-a.json \
  --source-b /secure/evidence/codex-source-b.json \
  --destination-a /secure/evidence/codex-destination-a.json \
  --destination-b /secure/evidence/codex-destination-b.json \
  --source-prefix-a /secure/evidence/codex-source-prefix-a.json \
  --source-prefix-b /secure/evidence/codex-source-prefix-b.json \
  --output /secure/evidence/codex-close-request.json

python3 scripts/bulkload.py codex-close-capture \
  --close-request /secure/evidence/codex-close-request.json \
  --root /absolute/source/.codex/sessions \
  --role source \
  --acknowledge-writers-quiesced \
  --output /secure/evidence/codex-source-close-a.json
```

Add the two `--destination-prefix-*` arguments when the prefix request names
destination-longer observations. Repeat `codex-close-capture` for source B and
destination A/B with distinct outputs and the same close request. Closing A/B
is required for both roles even when only one role had longer files. Each
command captures the live root and emits a v1 wrapper around an unchanged v2
snapshot; it does not accept a pre-existing snapshot to wrap. Then compile the
final plan:

```bash
python3 scripts/bulkload.py codex-plan \
  --source-a /secure/evidence/codex-source-a.json \
  --source-b /secure/evidence/codex-source-b.json \
  --destination-a /secure/evidence/codex-destination-a.json \
  --destination-b /secure/evidence/codex-destination-b.json \
  --prefix-request /secure/evidence/codex-prefix-request.json \
  --source-prefix-a /secure/evidence/codex-source-prefix-a.json \
  --source-prefix-b /secure/evidence/codex-source-prefix-b.json \
  --close-request /secure/evidence/codex-close-request.json \
  --source-close-a /secure/evidence/codex-source-close-a.json \
  --source-close-b /secure/evidence/codex-source-close-b.json \
  --destination-close-a /secure/evidence/codex-destination-close-a.json \
  --destination-close-b /secure/evidence/codex-destination-close-b.json \
  --output /secure/evidence/codex-union-plan-v3.json
```

Add the two `--destination-prefix-*` arguments when the request names
destination-longer observations. The final plan rejects missing, reused,
pre-proof, wrong-request, or drifting closing evidence.

Assign one non-secret canonical host-authority UUID to each filesystem
namespace for this operation. Reuse it for every capture that can address the
same physical namespace, including shared storage or a renamed host; use a
different ID for an independent host-local namespace.

Source capture accepts only current-user, single-link regular rollout JSONL
files that are owner-readable and not writable by group/other. Destination
capture additionally requires every file to be exactly `0600` and every
directory to be exactly `0700`. Strict JSON parsing rejects duplicate keys and
non-finite values; the first and only `session_meta` must carry the canonical
UUID bound by the filename. Capture reserves every candidate file and its full
stable size before parsing, bounds entries, directories, records, paths,
errors, catalogs, and output, then revalidates the resolved root, root lineage,
and every scanned directory.

Planning requires four unique capture IDs, exact A/B catalog bodies for both
roles, explicit quiescence, and distinct source/destination custody. Within one
host authority it rejects equal, ancestor, descendant, symlinked, or bind-aliased
roots from recorded filesystem lineage. It proposes destination-absent UUIDs
and proof-bound source-superset promotions, preserves destination-only and
proved destination-superset sessions, and blocks unproved or true same-UUID
divergence plus portable file/file, file/directory, and ancestor-prefix
namespace collisions. Prefix planning also requires a post-proof close request
that binds every required proof digest, followed by fresh wrapped closing A/B
catalogs for both roots that bind that request and match every opening catalog
and custody claim. The plan binds each opening, prefix proof, close request,
and close artifact by distinct capture ID or body digest as appropriate. Any
blocker suppresses the complete copy candidate list.
Codex evidence inputs must be owner-only, single-link regular files; the CLI
pins them through a create-only atomic publish in an owner-private output
directory. It verifies exact temporary bytes and single-link custody, uses an
OS no-replace rename, revalidates every input and the requested
directory/target immediately after publication, and refuses an existing
target. It never pathname-deletes on a failure: a nonzero result may leave an
owner-private staging or fail-held final artifact for attended quarantine. It
is a dry-run report: the protocol intentionally has no Codex-session apply
command. Any future attended copier must recheck the exact source and
`destination_before` hashes and sizes immediately before replacement. Never
run it against active writers or treat path/size equality as content proof.

## Handle private Codex auth

Follow
[references/codex-private-auth-install.md](references/codex-private-auth-install.md)
exactly. Prefer source auth-only and destination auth-plus-SQLite captures.
Full/full inputs are accepted, but source SQLite is never consumed. Every
destination family is preserved exact with zero mutations, while
`sqlite_union_ready=false`.

Create and digest-accept a fresh, purpose-bound quiescence attestation for each
capture or operation. It is an operator procedural fence with
`provider_writer_proof=false`; the advisory `flock` coordinates only
cooperating Bulkload processes. Any SQLite WAL, SHM, or rollback journal blocks
immutable capture.

The private entrypoint pins policy v3 and the full Python runtime closure before
import, and the compatibility/install plans bind that authority. Review and
accept both exact plan digests before attended auth apply. Offline receipts do
not prove provider authentication. Require a fresh attended provider turn.
Never infer SQLite authority, compose/install SQLite, or use a combined apply.

## Compile and review the plan

```bash
python3 scripts/bulkload.py plan \
  --source-a /secure/evidence/source-a.json \
  --source-b /secure/evidence/source-b.json \
  --destination /secure/evidence/destination.json \
  --output /secure/evidence/plan.json
```

Planning refuses a moving source. It records blockers for divergent Git heads,
missing or different source local refs, missing explicit destination anchors for
reflog/pseudo-ref recovery roots, missing repositories, staged/index state,
conflicts, active Git operations, deletions, sensitive paths, symlink mutations, submodules,
effective LFS or other content filters, legacy grafts, unportable Git attribute
authorities, shallow or partial/promisor history, alternates, and
destination-only dirt.

The plan carries canonical status and non-remote-ref bodies for both sides.
Validation recomputes their digests and reasserts ready-plan branch, HEAD, ref,
and dirt relationships; apply repeats those relationships from live captures.

Capture derives raw working status from HEAD, index stages, direct no-follow
hashing, and untracked enumeration. It never runs `git status` or a Git diff
command, so repository-configured clean/process filters are not executed. It
classifies a tracked regular file whose full permission mode differs from the
canonical index projection (`0644` or `0755`) as mode dirt; apply and verify bind
the exact four-octal source mode. It catalogs every name under `refs/`, disables
replacement-object semantics for tree identity, blocks legacy graft authority,
and walks the object closure from
HEAD, every non-remote ref, and every recovery-only local reflog or known
pseudo-ref root with lazy fetching disabled. A missing reachable commit, tag,
tree, or blob makes capture incomplete. Recovery acquisition is bounded; a
reflog, authority-byte, or candidate-root budget excess makes capture
incomplete. Active rebase, apply-mailbox, sequencer, bisect, merge,
cherry-pick, and revert markers are inspected without following them, recorded
as typed state, and make v1 capture incomplete. Shallow repositories are
also incomplete in v1 because their declared boundary can legitimize missing
ancestry. Effective partial/promisor configuration from repository, worktree,
included, global, or system scope is rejected before object reads so capture
cannot lazily fetch or mutate a repository. A read-only full Git fsck also
rejects corrupt stored objects. Apply fences the canonical
non-remote-ref digest; `refs/remotes/` remains repeated-catalog evidence rather
than cross-host parity authority. Supported URL and scp-style remote locators
are credential-sanitized, local paths are digest-redacted, and helper, unknown,
or malformed locator forms make capture incomplete without recording the raw
value. Symlink payloads are hashed, never serialized verbatim. Fleet discovery
also fails closed on bare repositories and symlink or special-file `.git`
authority instead of silently omitting or following it.

Review the exact plan body and repeat its `plan_sha256` only after every
operation and blocker is understood. Never edit the plan by hand; recapture
and regenerate it.

The built-in sensitive-path classifier is defense in depth, not a proof that
an innocuously named file contains no credential. Inspect every operation at
the source and run the operator-approved redacted secret scanner when
available. Any suspected credential or decrypted secret blocks acceptance.

## Reconcile Git semantically

Use fetch/push or a verified Git bundle for commits and refs. Reconcile every
source non-remote ref, including stash, notes, replace, and custom namespaces;
each must retain the same object, symbolic target, and reachable object closure.
For each source recovery-only reflog or pseudo-ref OID, create a reviewed
temporary destination retention ref at that exact OID before recapture. V1 does
not serialize every reachable destination object, so ancestry without an exact
anchor remains a conservative blocker.
Destination-only local refs are reported and preserved, except extra replacement
refs, which block. Recreate a clean worktree from the exact reviewed ref. Never
copy `.git`, `.git/worktrees`, or a linked-worktree `.git` pointer between
hosts. Worktree basename equality is not identity, and sibling-worktree dirt
requires a separate capture of each worktree.

After Git reconciliation, repeat both source captures and the destination
capture. Only then compile the working-byte plan.

For an attended live-remote proof, prefer the provider API with a literal,
operator-reviewed repository and ref. For a non-provider Git remote, run
`git ls-remote` outside any repository with system/global Git configuration
disabled, a literal reviewed URL, and a literal refspec:

```bash
scratch="$(mktemp -d)"
env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
  GIT_TERMINAL_PROMPT=0 \
  git -C "$scratch" ls-remote --exit-code LITERAL_URL LITERAL_REFSPEC
rmdir "$scratch"
```

Do not evaluate a repository-derived URL or SSH command, put credentials in the
URL, or record credential-helper output. If private authentication needs local
Git configuration, use an attended provider API instead of weakening this
isolation. A
worktree-topology finding is expected across hosts because Git records absolute
host-local paths; compare typed branch, HEAD, lock, and reconstructability facts
instead of raw list equality.

## Apply safe working bytes

Prefer local application when source and destination roots are both visible:

```bash
python3 scripts/bulkload.py apply \
  --plan /secure/evidence/plan.json \
  --accept-plan FULL_PLAN_SHA256 \
  --source-root /absolute/source/root \
  --destination-root /absolute/destination/root \
  --state-root /separate/private/bulkload-state \
  --receipt /secure/evidence/apply-receipt.json
```

The source, destination, and state roots must be pairwise disjoint, and the
receipt must be outside both repositories. Application binds those roots to
the plan's Git and file preconditions, backs up replacements, journals each
step, re-hashes the source, performs same-directory temporary-file replacement,
and durably creates each new ancestor before fsyncing files and directory
entries. The journal name is durable before destination mutation. Replaying the
same accepted plan verifies completed operations and resumes pending ones. Use
one private state root as the journal and backup authority for a destination.
Apply takes
advisory locks on the destination repository directories and refuses to run as
root; non-cooperating writers must still be paused.

For an attended cross-host transfer, first reconstruct the exact Git source at
an isolated destination-side staging root. Export only a ready plan's
NUL-delimited expected-source allowlist into that disposable staging root. It
contains every eligible dirty source file, including one that already matches
the live destination and therefore needs no operation there. Never point this
transport at the live destination and never add a delete option:

```bash
bash -c '
set -euo pipefail
umask 077
allowlist=$(mktemp /secure/evidence/.bulkload-files.XXXXXX)
cleanup() { rm -f -- "$allowlist"; }
trap cleanup EXIT
python3 scripts/bulkload.py files --null \
  --plan /secure/evidence/plan.json \
  --accept-plan FULL_PLAN_SHA256 >"$allowlist"
rsync -a --from0 --files-from="$allowlist" --checksum --delay-updates \
  --no-devices --no-specials \
  /absolute/source/root/ destination-host:/absolute/private/staging-source/
'
```

The transfer may race or contain unreviewed bytes, so staging is quarantine,
not proof. On the destination host, run the normal digest-accepted `apply` with
that reconstructed staging repository as `--source-root`; its Git and content
preconditions rebind every byte before the live destination changes. If either
side lacks the needed rsync features, stop and use a separately reviewed
staging transport.

## Verify independently

Capture the destination again after application, then verify:

```bash
python3 scripts/bulkload.py verify \
  --plan /secure/evidence/plan.json \
  --accept-plan FULL_PLAN_SHA256 \
  --destination /secure/evidence/destination-after.json \
  --output /secure/evidence/verification.json
```

Require `verified: true`, zero failures, exact branch/HEAD/status evidence,
unchanged destination non-remote ref catalog, exact capture mode, and the
expected file identities. Reuse of the exact pre-plan destination snapshot is
a verification failure, including for a no-op plan. Any newly active alternate,
graft, submodule, content filter, LFS attribute, or external attribute authority
also fails verification. Keep the source captures, plan, application receipt,
destination capture, verification receipt, exclusions, tool revision, and
operator decision together.

`verified: true` proves only the accepted content plan. It does not by itself
prove a live remote head, upstream agreement, commit signature, or historical
session continuity. Collect those separate proofs before claiming
`EXACT_ACTIVE_LANE` or `SESSION_NATIVE`.

## Report accurately

Use these claim classes:

- `EXACT_ACTIVE_LANE`: branch, HEAD, upstream, status, and required bytes match.
- `PROTECTED_PARITY`: protected dirt matches and neither side was mutated.
- `REMOTE_RECONSTRUCT`: committed state is remote-backed and the worktree can
  be recreated; its host-local directory was not mirrored.
- `EXCLUDE_PRESERVE`: dirty rescue/validation state is inventoried and retained
  but is not a migration input.
- `SESSION_NATIVE`: fresh destination auth and an intentional resume proved
  dialog continuity.
- `DONE_WITH_CONCERNS`: required work can resume, with named deferred gaps.

Never report “all worktrees synced” merely because the active lane is ready.

## Interpret exit status

- `0`: command completed and its positive claim is true.
- `2`: malformed input, failed invariant, or unsafe operation; an output plan
  is not promised.
- `3`: capture evidence was written but is incomplete.
- `4`: a digest-valid plan was written but contains blockers and is not ready.
- `5`: verification evidence was written and reports one or more failures.
- `6`: `doctor` could not find Git.

A sensitive path that requires a working-byte operation intentionally blocks
the whole v1 plan. A clean tracked sensitive path is privately attested against
the Git index, remains redacted and ineligible, and never becomes a copy
operation. Preserve an owner checkout with sensitive dirt, use a clean
reconstruction for safe work, and report `EXCLUDE_PRESERVE`; do not invent an
exclude flag or silently weaken the plan.
