---
name: bulkload
description: Inventory, plan, transfer, and independently verify a one-way migration of one Git repository, its dirty or untracked working state, selected agent transcripts, or a fleet under ~/git. Use for dev-box moves, crash recovery, worktree parity audits, Neo-to-Sting-style migrations, rsync replacement planning, or any request to preserve Git and agent context across machines. Keep credentials, live databases, generated caches, terminal multiplexers, deployment, activation, and bidirectional synchronization out of scope.
---

# Bulkload

Runtime requirement: Python 3.11 or newer, Git, and a Unix-like host with file
locking and dirfd support.

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
   - generated indexes and caches; and
   - authentication or credentials.
4. Re-authenticate on the destination. Never copy credential stores, browser
   profiles, `.env`, private keys, kubeconfigs, SQLite/WAL/SHM, or live auth.
5. Never invoke `cmux` or another terminal multiplexer. Never clean, prune,
   rebase, delete, switch Home Manager, deploy, reconcile, or activate as part
   of this workflow.

Read [references/migration-contract.md](references/migration-contract.md) for
the state matrix and stop conditions. Read
[references/agent-context.md](references/agent-context.md) before handling
Codex, Claude, or Pi state.

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
destination root. The CLI rejects overlapping file outputs so writing evidence
cannot create new dirt after the claim it records. An output or apply receipt
must also be distinct from every input evidence artifact; the CLI rejects
aliases before mutation. When emitting `-` over SSH, redirect only to a
controller-side path outside the controller's captured roots.

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

## Compile and review the plan

```bash
python3 scripts/bulkload.py plan \
  --source-a /secure/evidence/source-a.json \
  --source-b /secure/evidence/source-b.json \
  --destination /secure/evidence/destination.json \
  --output /secure/evidence/plan.json
```

Planning refuses a moving source. It records blockers for divergent Git heads,
missing or different source local refs, missing repositories, staged/index
state, conflicts, deletions, sensitive paths, symlink mutations, submodules,
effective LFS or other content filters, legacy grafts, unportable Git attribute
authorities, shallow or partial/promisor history, alternates, and
destination-only dirt.

The plan carries canonical status and non-remote-ref bodies for both sides.
Validation recomputes their digests and reasserts ready-plan branch, HEAD, ref,
and dirt relationships; apply repeats those relationships from live captures.

Capture derives raw working status from HEAD, index stages, direct no-follow
hashing, and untracked enumeration. It never runs `git status` or a Git diff
command, so repository-configured clean/process filters are not executed. It
catalogs every name under `refs/`, disables replacement-object semantics for
tree identity, blocks legacy graft authority, and walks the object closure from
HEAD and every non-remote ref with lazy fetching disabled. A missing reachable
commit, tag, tree, or blob makes capture incomplete. Shallow repositories are
also incomplete in v1 because their declared boundary can legitimize missing
ancestry. Effective partial/promisor configuration from repository, worktree,
included, global, or system scope is rejected before object reads so capture
cannot lazily fetch or mutate a repository. A read-only full Git fsck also
rejects corrupt stored objects. Apply fences the canonical
non-remote-ref digest; `refs/remotes/` remains repeated-catalog evidence rather
than cross-host parity authority. Supported URL and scp-style remote locators
are credential-sanitized, local paths are digest-redacted, and helper, unknown,
or malformed locator forms make capture incomplete without recording the raw
value. Symlink payloads are hashed, never serialized verbatim.

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

A sensitive path intentionally blocks the whole v1 plan. Preserve that owner
checkout, use a clean reconstruction for safe work, and report
`EXCLUDE_PRESERVE`; do not invent an exclude flag or silently weaken the plan.
