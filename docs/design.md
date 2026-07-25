# Bulkload v1 design

Status: repository v1 complete; Codex session-union dry-run extension in review,
2026-07-24

## 1. Decision

Build `bulkload` as a manifest-first controller and portable Agent Skill for
one-way migration of one Git repository or a fleet rooted at `~/git`. It must
make the safe path boring: two stable source observations, one destination
observation, an immutable reviewed plan, explicit digest acceptance, additive
application, and an independent post-apply verification receipt.

`bulkload` is not a second filesystem and not a generic home-directory clone.
It classifies state by authority and chooses the right movement mechanism for
each class. In particular, it does not pretend that a Git worktree registry,
uncommitted bytes, agent transcripts, caches, and rotating credentials are one
homogeneous tree.

## 2. Evidence from the Neo to Sting migration

The design is grounded in the July 2026 TCFS/Sting migration rather than a
greenfield copy model.

1. The protected `tummycrypt` owner checkout was dirty and intentionally kept
   intact on both hosts. A clean sibling worktree carried TIN-2864 development.
   This made ownership and mutation authority explicit.
2. Git branch state moved correctly through refs: the Sting TIN-2864 worktree
   was clean but its remote-tracking ref was stale. A targeted fetch plus
   `merge --ff-only` moved it from `e5ea0810` to signed head `36d8c0f6` without
   touching any dirty tree.
3. Worktree registries did not and should not match. Neo had seven worktrees;
   Sting had twenty-five historical, validation, rescue, and draft entries.
   Several identical basenames referred to different branches or commits.
   Basename equality was therefore not identity or parity.
   The protected checkout's 122 untracked paths and regular-file manifest were
   nevertheless exact; ignored build/cache output remained host-local.
4. The original archive mixed portable state with host metadata. It contained
   4,923 AppleDouble `._*` sidecars. One 163-byte `._default.rules` file was
   parsed as a Codex rule and blocked startup because it was not UTF-8.
   Lossless quarantine of that sidecar restored startup while preserving
   evidence.
5. Codex sessions were portable as append-only JSONL, but session indexes and
   rotating authentication were not copy authorities. The historical session
   was found and executed, yet its newest compaction no longer contained the
   requested exact checklist. A fresh persisted Sting session with a nonce was
   then resumed successfully, proving current auth, persistence, lookup, and
   dialog continuity.
6. `--ephemeral resume` on Codex 0.144.5 still appended to the historical
   rollout. The correct invariant is therefore observed immutability, not a
   flag name.
7. Authentication succeeded only after attended device login. Credential
   files were never treated as ordinary sync payloads.
8. Byte hashes and status hashes were useful, but a single hash was not a
   completeness proof. The active lane required branch, HEAD, cleanliness,
   ancestry, signature, remote backing, and exact source/destination evidence.

These facts establish the central rule: parity is a set of typed invariants,
not byte equality over an arbitrary directory.

## 3. State taxonomy

| Plane | Authority | v1 action | Typical examples |
|---|---|---|---|
| Git object/ref | remote or reviewed bundle | fetch, verify, recreate | commits, branches, tags, stash, notes, custom refs |
| Worktree topology | host-local Git administration | inventory, classify, recreate | linked worktree path, branch, HEAD, lock |
| Working bytes | source worktree plus manifest | hash, additive copy, verify | modified tracked files, safe untracked files |
| Agent continuity | append-only transcript authority | allowlisted copy, validate, resume proof | Codex JSONL, Claude project transcripts |
| Generated state | destination runtime | regenerate | indexes, caches, `.direnv`, platform binaries |
| Authentication | provider/operator | attended re-authentication | refresh tokens, browser cookies, auth DBs |

The first four planes can contribute to a migration dossier. Generated state
and authentication are never silently promoted into copy operations.

## 4. Safety properties

### 4.1 Read-only first

`capture`, `plan`, `verify`, and `files` are read-only. `apply` is separate and
requires the exact `plan_sha256`. The CLI never invokes a terminal multiplexer,
Home Manager, a deploy, or a product runtime.

### 4.2 Repeated-catalog barrier

Pass A and pass B capture the source independently. Planning is refused unless
their canonical `catalog_sha256` values match exactly. Timestamps and host
envelope data are excluded from the catalog digest; every repository fact,
status classification, ref, worktree record, and inventoried file is included.
Each envelope also has a random 128-bit `capture_id`, and planning rejects reuse
of one envelope as both passes.

This closes the most common omission race: creating a digest over a catalog
that was still changing or incomplete. It does not make a live filesystem
snapshot atomic. Any capture error marks the catalog incomplete and blocks
planning.

### 4.3 Typed operations

The planner emits operations, findings, and blockers. Safe v1 operations are
limited to additive or replacing copies of regular files. Clean tracked
symlinks are attested against the Git index and recreated through Git; dirty or
untracked symlink mutations block the plan. A clean tracked sensitive path is
privately attested against the Git index but remains redacted and ineligible.
Deletions, conflicts, sensitive working-byte changes, missing repositories, and
divergent Git heads are blockers or operator instructions, never implicit file
operations.

### 4.4 Immutable acceptance

The plan digest covers the stable source catalog, destination catalog, expected
file facts, operations, and blockers. `apply` requires the operator to repeat
that digest. It binds the live destination to the captured hostname and
resolved root, then re-hashes each source file immediately before copying.
Destination replacements are backed up under an explicit external backup root
and installed by same-directory temporary file plus rename. Every newly created
state, backup, and destination directory entry is fsynced, and the journal name
is persisted before any destination mutation.

The source root is bound semantically by Git and file preconditions rather than
by hostname/path so a reviewed cross-host transfer can use a quarantined,
destination-side reconstruction. The destination identity is not portable.

Repository preconditions carry the canonical status body and non-remote ref
records, not only caller-supplied digests or path lists. Ready-plan validation
recomputes those digests and reasserts branch/HEAD equality, source-ref
composition, the absence of destination-only replacement refs, and that
destination dirt is a subset of source dirt. Apply repeats those relationships
from fresh live captures and derives its allowed destination dirt from the live
source status body before any copy.

The same preconditions bind recovery-only Git roots found in local reflogs and
known pseudo-refs. A source recovery root is retained only when the destination
has an explicit anchor at that exact object: HEAD, a non-remote ref tip, or its
own recovery-root catalog. Merely being an ancestor of a destination ref is
deliberately insufficient proof in v1. Serializing every object reachable from
every destination ref would make repo and fleet manifests grow outside the file
and byte budgets and can add millions of OIDs. The bounded remediation is to
create a temporary reviewed destination retention ref at each missing OID,
recapture, and keep that ref through verification. Capture caps recovery
acquisition at 4,096 local reflogs, 16 MiB per authority file, 64 MiB total
authority bytes, and 4,096 candidate roots; exceeding any cap is incomplete,
never an omitted ready plan.

### 4.5 Independent verification

Verification consumes a fresh destination capture, not the apply process's
claims. It checks capture mode, expected HEAD/status, the destination
non-remote-ref digest, and every copied path's type, mode, size, and content
digest. It rejects reuse of the exact pre-plan destination snapshot even when a
no-op plan leaves the catalog unchanged, and it fails if the destination has
acquired alternates, grafts, submodules, content filters, LFS attributes, or
external attribute authority. The receipt records what was observed; it does
not expand the authority of the plan.

## 5. Catalog contract

The snapshot schema is `dev.tinyland.bulkload.snapshot.v1`.

Repository discovery recognizes a worktree only when `.git` is a real directory
or a regular Git pointer file. A symlink or special-file `.git` authority makes
capture incomplete. Bare repositories are detected explicitly and also make v1
capture incomplete: they require a separate object/ref transport rather than
being silently omitted from a fleet manifest.

Each envelope records:

- capture time, hostname, logical root, and mode (`repo` or `fleet`);
- whether discovery completed without errors;
- a sorted repository catalog;
- `catalog_sha256`, computed from canonical JSON over the catalog only; and
- `snapshot_sha256`, computed over the full envelope except its own digest.

Each repository records logical path, real path, Git common directory, branch,
HEAD, upstream, sanitized remotes, every name under `refs/` with its resolved
object and symbolic target, a canonical full `refs_sha256`, a
`local_refs_sha256` that excludes host-local `refs/remotes/`, recovery-only
roots and their digest, worktree records, status facts, and file facts.
Recovery acquisition reads both old and new OIDs from real reflog records plus
`AUTO_MERGE`, `BISECT_HEAD`, `CHERRY_PICK_HEAD`, `FETCH_HEAD`, `MERGE_HEAD`,
`ORIG_HEAD`, `REBASE_HEAD`, and `REVERT_HEAD`; remote-tracking reflogs remain
host-local and are excluded. Supported network and scp-style remote locators
have user-info and query or fragment data removed; local-path remotes are
represented by a typed digest, and remote-helper, unknown-scheme, or malformed
locators make capture incomplete without serializing the raw value. File facts
contain paths and digests, never file contents; symlink payloads are hashed
rather than recorded. Apply and verification fence the non-remote digest;
remote-tracking movement remains
recorded by the repeated catalog barrier but does not invalidate an already
reviewed working-byte plan.

Object reachability is not resumable-operation parity. Capture therefore
inspects worktree-local Git administration markers without following them and
records a sorted typed `git_operation_state`. Any active rebase, apply-mailbox,
sequencer, bisect, merge, cherry-pick, or revert makes the repository and
envelope incomplete; apply repeats the same live capture fence.

Every tracked regular file and symlink is also compared with its stage-zero Git
index blob and canonical full regular-file permission mode (`0644` or `0755`).
A `0600` file backed by a `100644` index entry is therefore typed as mode dirt,
copied with `0600`, and verified with that exact mode rather than falsely
passing on content alone. Status is derived from the HEAD tree, index stages,
direct no-follow byte hashes, and untracked enumeration; capture never
runs `git status`, `git diff`, or another worktree conversion command. A path
whose raw working bytes differ from the index is therefore classified as dirty
even when assume-unchanged, skip-worktree, or stat-cache metadata would hide it
from normal Git status.

Capture also walks the object closure reachable from detached HEAD, every
non-remote ref, and every recovery-only root with replacement semantics and
lazy fetching disabled. A missing commit, tag, tree, or blob therefore makes
the capture incomplete before a plan can become ready; a ref name and OID alone
are not a completeness proof. The closure is checked but not serialized as an
unbounded object inventory. Shallow
repositories are incomplete in v1 because a shallow boundary deliberately
hides ancestry that this protocol promises to retain. Effective
partial/promisor configuration from repository, worktree, include, global, or
system scope is rejected before object reads, preventing a capture from lazily
fetching missing objects or mutating the repository it observes. A full
read-only `git fsck` additionally verifies stored object integrity; corrupt
historical content therefore fails capture even when its filename still looks
like the expected object ID.

Snapshot validation recomputes `status_sha256` from the canonical status body
and requires each file record's status to equal that repository status entry.
An externally supplied, self-digested envelope therefore cannot detach the
plan-visible dirt from the apply-time Git precondition.

Capture strips ambient `GIT_*` repository, index, namespace, and object-store
overrides before invoking Git and sets `GIT_NO_REPLACE_OBJECTS=1`. Replacement
refs are still cataloged as state, but cannot change the HEAD tree used for
identity. A nonempty legacy `.git/info/grafts` authority blocks planning.
Effective `filter` attributes are queried with NUL framing in both working-tree
and cached views for every catalog path. Any effective LFS or other
content-filter path blocks planning; nonempty `.git/info/attributes` or an
external/global attributes authority is recorded as unportable and also blocks
v1 planning. Attribute inspection never invokes the configured filter program.

Tracked and non-ignored untracked files are inventoried. Ignored/generated
files are excluded by default; `--include-ignored` is an explicit, bounded
inventory mode and does not make them eligible for copying. Sensitive-looking
paths are recorded as blocked without a content digest, even if they are
already tracked. Remote URL user-info, query strings, and fragments are never
serialized.

File and byte budgets are enforced before hashing a file. In fleet mode they
apply independently to each repository; an exceeded budget makes the complete
snapshot fail closed.

## 6. Planning and application

The intended operator sequence is:

```text
source capture A ─┐
                  ├─ exact catalog equality ─ destination capture ─ plan
source capture B ─┘                                      │
                                                        review
                                                          │
                                             accept exact plan digest
                                                          │
                                            additive apply or rsync list
                                                          │
                                           fresh destination capture
                                                          │
                                                 verify + receipt
```

For one repository, logical path is `.`. For fleet mode, logical paths are
relative to the declared fleet root. The same plan can therefore generate a
NUL-delimited allowlist for `rsync --from0 --files-from=-` without embedding
source-host absolute paths in file names. That allowlist contains every
expected dirty source file, not only live-destination mutations, so an isolated
clean Git staging tree receives bytes that already happen to match the live
destination.

Git reconciliation remains explicit:

- fetch/push or a verified Git bundle moves refs and objects;
- every source non-remote ref must exist at the same object and symbolic target
  on the destination, with its reachable object closure present;
- every source recovery-only reflog or pseudo-ref root must have an explicit
  destination anchor at the same OID; use a temporary retention ref and
  recapture rather than relying on an unbounded ancestry catalog;
- destination-only local refs are retained and reported, never deleted; an
  extra replacement ref blocks because it changes ordinary Git object semantics;
- remote-tracking refs remain host-local evidence and are not parity authority;
- `git worktree add` recreates clean linked worktrees from exact refs;
- `git worktree repair` repairs pointers only when both sides were deliberately
  moved together;
- dirty working bytes are handled by the content plan;
- index conflicts and staged topology are blockers in v1.

Git bundles contain only the refs selected at creation plus their reachable
objects and prerequisites. `--all` can select `refs/stash` and other local refs;
a narrower selection can omit them. Bundles still omit the working tree,
index, untracked bytes, per-repository configuration, and hooks. That
limitation is a feature here: it prevents a Git-object transport from being
mistaken for total working-state parity.

## 7. Agent context policy

Agent context is an opt-in extension, not part of `~/git` discovery.

- Copy transcript stores only from explicit allowlists.
- Preserve append-only source files and compare prefix/hash evidence before
  deciding whether destination content is a superset.
- Regenerate indexes and platform caches on the destination.
- Quarantine AppleDouble files and other unsupported metadata; never discard
  them silently.
- Prove continuity with a fresh non-secret nonce in a disposable or newly
  persisted session before trusting historical resume behavior.
- Re-authenticate on the destination. Never copy refresh-token stores,
  browser profiles, cookies, or provider credential databases.
- Treat compaction as a semantic retention boundary. A transcript's presence
  does not prove every old instruction remains in active model context.

The v1 CLI implements repository catalogs and content plans. Its first
provider-specific extension is a read-only Codex rollout catalog and
collision-gated absent-only UUID union plan. It validates JSONL identity and
current-user ownership, rejects hardlinks and files writable by group or other,
and requires explicit writer quiescence plus two byte-stable observations of
both the source and destination. Destination custody is stricter than legacy
source evidence: every destination rollout must be exactly `0600` beneath
exactly `0700` directories.

The Codex snapshot and plan use v2 schemas. Each capture binds a non-secret,
operator-assigned host-authority UUID to the resolved root, its device/inode
lineage, and typed directory records. One authority ID names one physical
filesystem namespace even when a hostname changes or storage is shared; an
independent namespace receives a different ID.

The scanner holds descriptor authority while traversing, rejects duplicate
JSON keys, non-finite values, non-canonical UUIDs, missing or repeated
`session_meta`, and revalidates the resolved root, lineage, and every completed
directory after the scan. Candidate file count and stable size are reserved
before parsing, so malformed content cannot multiply the aggregate limits.
Entry, directory, record, path, catalog, error, and output budgets make
incomplete discovery explicit.

Planning compares exact catalog bodies rather than trusting a digest alone.
Within one host authority, equal or ancestor/descendant roots are rejected by
resolved path and recorded lineage. Directory claims close the namespace before
classification: same-UUID byte divergence and portable file/file,
file/directory, or ancestor-prefix collisions are blockers, and any blocker
suppresses all copy candidates. Codex planning reads bounded exact-`0600`
single-link evidence through pinned descriptors, revalidates it before output,
and writes through a pinned private output directory. The extension
deliberately exposes no session apply command. Claude, Pi, provider indexes,
history, memory, and authentication remain procedural follow-ups because their
formats and refresh semantics change independently.

## 8. Relationship to TCFS

This tool is deliberately interim. TCFS is intended to replace the manual
working-byte plane with a product contract built from chunked content-addressed
storage, manifests, hydration, namespace composition, vector-clock/conflict
state, registered local/remote observations, Git topology, and monotonic remote
catalog control state.

The July TIN-2864 work is still source-only and digestless. It does not yet
authorize live planning, deployment, reconciliation, credential handling, or a
crypto ceremony. `bulkload` therefore does not call TCFS. Instead it records
the exact operational requirements TCFS must eventually absorb:

- complete, repeatable enumeration;
- typed namespace and Git claims;
- no-loss hydration and replay;
- monotonic publication/catalog state;
- explicit conflict and writer fencing;
- rotation and garbage-collection safety; and
- portable, independently verifiable receipts.

When TCFS supplies those properties end to end, `bulkload` should shrink to a
legacy importer and audit adapter rather than remain a second sync system.

## 9. Threat model

Primary hazards are source or destination mutation during capture, path
traversal, symlink or hardlink aliasing, payload disclosure, incomplete
reachable Git objects, omitted
reflog/pseudo-ref recovery roots, unsafe remote
helpers, case-folding collisions, stale remote refs, filename encoding,
AppleDouble sidecars, unbounded ignored trees, secret-bearing untracked files,
concurrent destination edits, partial transfer, and false confidence from a
green transport exit code.

V1 mitigations are canonical sorted catalogs, role-specific A/B barriers, no
content in manifests, path normalization, a v1 block on symlink mutations,
sensitive-path blocks re-enforced during plan validation, no deletion,
pre-copy source rehash, external backups, atomic replacement, durable
journal/directory entries, fresh verification, Codex evidence inputs and output
parents pinned across final rename, pathname overlap guards for the repository
adapter, and explicit incomplete/error states. Cross-filesystem
atomicity, ACL/xattr fidelity, sparse files, hardlink identity, special files,
case-insensitive collisions, submodule worktrees, and live concurrent writers
remain blockers or follow-up work.

## 10. Implementation layout

- `.agents/skills/bulkload/`: canonical portable skill and executable Python.
- `BUILD.bazel`: build/test SSOT for the exact skill code.
- `justfile`: human and agent entrypoint; delegates normal build/test to the
  GloriousFlywheel wrapper.
- `justfile.flywheel` and `.bazelrc.flywheel`: generated, endpoint-free
  GloriousFlywheel front-door kit pinned by CI to an immutable core revision.
- `.github/workflows/ci.yml`: direct `tinyland-nix` cache-first validation; no
  hosted or dynamic runner fallback.
- `scripts/install-skill.sh`: locked user-scope installation with fail-before-
  mutation destination preflight and no-follow private backup containment.
- `tests/`: deterministic fixture tests for catalogs, barriers, plans, apply,
  verification, traversal rejection, and installer behavior.
- `docs/design.md`: design and evidence authority.
- `docs/neo-sting-retrospective.md`: dated dialog and execution retrospective.
- `tinyland.repo.json`: machine-readable role and boundary declaration.

## 11. One-day sprint boundary

Included:

1. Repo and fleet discovery.
2. Canonical catalogs with Git/worktree/status/file facts.
3. Two-pass stability barrier.
4. Deterministic plan and digest.
5. Safe local additive apply plus external backup and receipt.
6. NUL allowlist export for reviewed rsync transport.
7. Independent verification.
8. Portable Agent Skill and authenticated one-line private install.
9. Bazel tests on the sanctioned GloriousFlywheel cache-first ARC path.

Deferred:

- credential or browser-state movement;
- automatic remote command execution;
- deletion/mirror mode;
- conflict/index reconstruction;
- ACL, xattr, hardlink, sparse-file, and special-file fidelity;
- provider-specific Claude/Codex database writers;
- TCFS runtime integration.

## 12. Research basis

- Tridgell and Mackerras, *The rsync algorithm*: remote delta discovery and
  pipelining, <https://rsync.samba.org/tech_report/>.
- Samba rsync manual: archive semantics, dry-run, checksum mode, itemized
  changes, file allowlists, delayed updates, and cross-filesystem hazards,
  <https://download.samba.org/pub/rsync/rsync.1>.
- Git worktree manual: linked-worktree administration, locks, repair, and
  prune behavior, <https://git-scm.com/docs/git-worktree>.
- Git bundle manual: explicitly selected refs/objects and prerequisites, while
  worktree, index, untracked bytes, config, and hooks remain outside the bundle,
  <https://git-scm.com/docs/git-bundle>.
- Git replace manual: replacement refs and `GIT_NO_REPLACE_OBJECTS`,
  <https://git-scm.com/docs/git-replace>.
- Git attributes manual: check-in conversion and external clean/process filter
  execution, which is why capture derives raw status without `git status` or
  diff commands, <https://git-scm.com/docs/gitattributes>.
- Linux `rename(2)` and `fsync(2)` contracts: same-filesystem atomic name
  replacement plus the separate directory sync needed to persist a directory
  entry, <https://man7.org/linux/man-pages/man2/rename.2.html> and
  <https://man7.org/linux/man-pages/man2/fsync.2.html>.
- Chandy and Lamport, *Distributed Snapshots: Determining Global States of
  Distributed Systems*: coordinated consistent global-state capture in an
  asynchronous system,
  <https://lamport.azurewebsites.net/pubs/chandy.pdf>.
- Agent Skills specification: portable `SKILL.md`, scripts, references, and
  progressive disclosure, <https://agentskills.io/specification>.
- Codex skill documentation: repository and user discovery under
  `.agents/skills`, symlink support, and optional `agents/openai.yaml`,
  <https://developers.openai.com/codex/skills/create-skill>.
- Pi skill documentation: discovery under `~/.agents/skills` and
  `~/.pi/agent/skills`, <https://pi.dev/docs/latest/skills>.
- Claude Agent SDK skill documentation: user and project discovery under
  `.claude/skills`, <https://code.claude.com/docs/en/agent-sdk/skills>.
- Bazel Central Registry, `rules_python` 2.2.0; this repository pins and tests
  Bazel 8.2.1, <https://registry.bazel.build/modules/rules_python>.

These sources constrain, rather than broaden, the protocol. Bulkload treats
worktree administration as path-bound; bundles remain object/ref transports;
and rsync is restricted to reviewed `--from0 --files-from` transfers into
quarantine. On Linux filesystems honoring the cited contracts, same-directory
rename plus file/directory sync remains the durability primitive. Equal A/B
catalogs are only an observational stability barrier, not a Chandy-Lamport-style
coordinated consistent cut and not an atomic point-in-time snapshot. Concurrent
writers still require quiescence or a stronger snapshot authority, and any scan
error, changed catalog, or live precondition mismatch fails closed.

## 13. Acceptance criteria

The sprint is complete when a fixture with a clean repo, a modified tracked
file, a safe untracked file, and a sensitive untracked file can:

1. produce two exact stable source catalogs;
2. reject a changed second pass;
3. block the sensitive path;
4. create a deterministic plan for safe files;
5. require the exact plan digest to apply;
6. preserve replaced destination bytes in an external backup;
7. verify fresh destination facts independently; and
8. install one canonical skill usable by Codex, Pi, and Claude.
