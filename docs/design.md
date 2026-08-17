# Bulkload v1 design

Status: repository v1 complete; Codex session-union v3 prefix-proof extension,
private-state policy v4 narrow attended auth install, and the four-pass SQLite
composition opening plan in review; public-read CI repair naturally green on
its source carrier, 2026-08-16. SQLite composition, publication, installation,
and combined private apply remain fail-held.

The public-read CI boundary is separately closed by a repository-local
front door. It runs only on the literal `tinyland-nix` capability, rejects fork
pull requests before scheduling, and consumes the public immutable
`tinyland-inc/ci-templates` v2.13.0 actions at commit
`139bd4c7deabbe07c918dc764a3b9f054066431d`. It does not import private
GloriousFlywheel action or flake source. The workflow does not use a checkout
action. Its bootstrap trust root is the GitHub runtime and sanctioned ARC
runner image and kernel: the runner constructs the step environment and command
files and supplies system Bash, base64, env, Git/libcurl, DNS, TLS, and CA
authority. The contract does not claim resistance to a compromised member of
that root.

Before any leg starts, the job environment blanks `BASH_ENV`, imported shell
option and trace channels, loader injection, proxy override channels, CA
overrides, and TLS key logging. Lower-case proxy blanks, including `ftp_proxy`,
remain job-scoped for the local composite and pinned actions. The materializer
step separately blanks `FTP_PROXY`. After token and header destruction, exact
checkout verification, and private checkout-state cleanup, the trusted
materializer appends exactly five upper-case empty proxy records to an owned,
canonical regular `$GITHUB_ENV` file contained by
`$RUNNER_TEMP/_runner_file_commands`. Bash runs privileged without profile or
rc files, so imported functions and option state are ignored. The
materialization step removes both proxy cases and every other transport channel
again before its first child. It then requires
the pre-existing workspace and runner temporary root to be owned, canonical
nonsymlink directories and requires the workspace to be empty without deleting
stale contents. It uses fixed system binaries, a new private home and global
config, an empty template, a private temporary root, and no system Git config.

The read-only GitHub token is masked, copied to an explicitly non-exported shell
variable, and removed from the process environment before the fixed base64
encoder receives the raw value over stdin. The raw and encoded forms are
cleared before any Git fetch. A fetch-only subshell de-exports the inherited
environment and exports only the private Git configuration, fixed system path,
locale, temporary root, prompt fences, and masked Basic header; malformed
environment names such as imported function records are removed by fixed
`/usr/bin/env`. `--config-env` binds that header to GitHub, while
`--no-auto-maintenance` and `--no-write-commit-graph` prevent ancillary writers.
The fetch requests the exact event object and complete head and tag namespaces.
The header is cleared on success and by shell exit on failure before an exact
detached checkout. The step then proves full history, exact HEAD and worktree
root, a clean tree, a tokenless origin, no object alternates, and no credential,
HTTP, include, or SSH config.

The workflow is one literal, fail-fast-disabled matrix whose `source`, `build`,
and `test` legs are independently scheduled on `tinyland-nix`. Each leg has
exactly two workflow steps: the fixed-Git materializer and the local composite.
The composite fail-closes any gate outside that enum, establishes the common
public-read boundary, and selects one mutually exclusive tail. The source tail
executes the source suite; the build tail revalidates Bazel authority and ends
in `build //:bulkload`; the test tail revalidates independently and ends in
`test //:tests`. Thus each selected execution path has exactly one repository
consumer and no process or action after it. The materializer registers no
checkout post. The exact nested action pins contain no post, nested use, or
step after their Bazel invocation. Semantic contract validation rejects any
composite step that redeclares a job-fenced shell, imported-function, loader,
proxy, CA, or keylog channel, including `LD_PRELOAD` and `BASH_FUNC_*`.

The Nix client process is configured
for token-free reads at the `bulkload-ci` site and `main` cache, while endpoint
locations for Attic and Bazel remain injected by the runner. A no-value
repository preflight validates authority-only raw endpoints and clears
inherited shell, Nix, and Bazel credential channels before the pinned discovery
action; post-discovery
enforcement revalidates the boundary and requires both cache reachability
claims before any Nix or Bazel command. Pre-discovery command lookup is the
reviewed Nix profile plus the fixed system path; after discovery contributes
that profile through `GITHUB_PATH`, step mappings carry only the reviewed
system suffix. The guard rejects any other effective path.

`GRPC_PROXY_EXP` is different from the empty-valued transport fences:
grpc-java treats even a present-empty value as an active proxy request and
resolves it to localhost port 80. It must therefore be exactly absent. The
guard inspects the NUL-framed process environment and rejects both
present-empty and nonempty forms before Nix setup, after discovery, and at each
immediate Bazel boundary. The local composite never declares or persists the
name, so Nix and the Java/gRPC/Bazel consumers receive absence rather than an
empty record. Fixed-Git source fetching remains inside its separate minimal
transport fence.

The exact guard is snapshotted before the selected repository consumer. Each
Bazel leg performs a fresh captured-byte, digest, and authority check
immediately before its terminal call and receives a new private Bazelisk home,
empty user home, test temporary root, and Nix/XDG runtime home. The source leg
uses a distinct private runtime home and enters its development shell with the
inherited environment cleared, retaining only `HOME` and its eight Nix/XDG
home selectors. Workspace wrappers, persistent caches and Bazel servers,
inherited username path components, system/user rc drift, and netrc
credentials fail closed. This is same-UID authority hygiene, not a filesystem
sandbox; exact source review remains part of the trust boundary.

The Nix client binds the canonical direct `/nix/store` with `store = local` and
`allow-symlinked-store = false`. Its exact substituter/key inventory is the
runner-injected public `main` Attic cache plus `cache.nixos.org`, with
signatures required. Store, daemon-socket, mirror, HTTP-family proxy,
certificate, curl, JVM, Bazel shell, and Bazelisk command selectors are empty;
`GRPC_PROXY_EXP` is absent. Trusted
substituters, remote builders, build and diff hooks, access tokens, user Nix
configuration, netrc, flake-config acceptance, secret signing keys, and
plugins are cleared and verified from effective settings. Ambient `GIT_*`,
`JUST_*`, `NIX_MIRRORS_*`, exported Bash functions, and `SHELLCHECK_OPTS` are
rejected. Bazel cache publication is false for every pull request and tag and
true only for a trusted push to `main`; remote execution is never selected.

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
5. Codex sessions were portable as append-only JSONL. Authentication and
   provider-owned SQLite were initially, incorrectly excluded as copy
   authorities; the operator later clarified that both are copy-eligible typed
   state. Published Codex guidance independently documents copying
   `auth.json` to a trusted headless machine. SQLite requires provider-aware
   snapshots and composition rather than ordinary file copying. The historical
   session was found and executed, yet its newest compaction no longer
   contained the requested exact checklist. A fresh persisted Sting session
   with a nonce was then resumed successfully, proving current auth,
   persistence, lookup, and dialog continuity.
6. `--ephemeral resume` on Codex 0.144.5 still appended to the historical
   rollout. The correct invariant is therefore observed immutability, not a
   flag name.
7. Authentication succeeded after attended device login, but reauthentication
   was an operational choice rather than a portability requirement.
   Credential files must never be treated as ordinary repo dirt; a dedicated
   private adapter may move them without logging values.
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
| Generated state | destination runtime | regenerate | caches, `.direnv`, platform binaries |
| Provider SQLite | provider runtime plus consistent snapshot | opt-in immutable capture; preserve destination exact during auth install; no composer/installer | Codex state, log, goal, and memory databases |
| Authentication | provider/operator | opt-in private atomic replace with rollback, or attended re-authentication | Codex `auth.json` |

Every plane has a separate contract. Generated caches are regenerated.
Authentication and provider SQLite are never silently promoted through the
generic file adapter, but they may enter an explicit typed migration dossier.

## 4. Safety properties

### 4.1 Read-only first

Repository `capture`, `plan`, `verify`, and `files` are read-only. Repository
`apply` is separate and requires the exact `plan_sha256`. Private policy v4
also exposes a separately attended, digest-accepted auth-only apply and a
distinct non-actionable four-pass SQLite opening request; neither can compose,
publish, or install SQLite. The CLI never invokes a terminal multiplexer, Home
Manager, a deploy, or a product runtime.

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
- Prefer an explicit typed auth plan when a trusted destination should retain
  the same Codex login. Require private custody, no value logging, source and
  destination backups, atomic install, rollback, and a fresh authenticated
  provider turn. Attended reauthentication remains a fallback.
- Treat every provider-owned SQLite family under the effective
  `sqlite_home`/`CODEX_SQLITE_HOME`/`CODEX_HOME` authority as one typed set.
  The current immutable reader hard-stops if any WAL, SHM, or rollback-journal
  sidecar exists. It never copies a live database family as ordinary files.
  Policy v4 does not compose or install SQLite; during auth install it
  independently captures and preserves every destination family exactly with
  zero mutations.
- Prefer an auth-only source capture and an auth-plus-SQLite destination
  capture for the narrow auth installer. A full source capture plus a full
  destination capture is also accepted, but source SQLite is never consumed.
- Browser profiles, cookies, private keys, and unrelated provider credential
  databases remain outside the Codex adapter.
- Treat compaction as a semantic retention boundary. A transcript's presence
  does not prove every old instruction remains in active model context.

The v1 CLI implements repository catalogs and content plans. Its first
provider-specific extension is a read-only Codex rollout catalog and
collision-gated append-only UUID union plan. It validates JSONL identity and
current-user ownership, rejects hardlinks and files writable by group or other,
and requires explicit writer quiescence plus two byte-stable observations of
both the source and destination. Destination custody is stricter than legacy
source evidence: every destination rollout must be exactly `0600` beneath
exactly `0700` directories.

The Codex snapshot remains v2. The prefix-proof request/proof,
close-request/close-capture wrapper, and union-plan schemas are v1/v1/v3
respectively. Each snapshot binds a non-secret, operator-assigned
host-authority UUID to the resolved root, its device/inode lineage, and typed
directory records. One authority ID names one physical filesystem namespace
even when a hostname changes or storage is shared; an independent namespace
receives a different ID. The serialized non-private-directory count covers
only those portable descendant directory records. The capture validates the
root separately as namespace authority; it is not itself a catalog member or
part of that descendant count.

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
classification. Same-UUID differing bytes require targeted cross-catalog
prefix evidence bound to both catalog digests, both root identities, the UUID,
path, full hashes and sizes, and a JSONL record boundary. A proved source
superset may be promoted; a proved destination superset is preserved. Equal
sizes with different hashes, mid-record cuts, missing or replayed evidence,
portable file/file, file/directory, or ancestor-prefix collisions are blockers,
and any blocker suppresses all candidates. When prefix evidence is required,
all required proof passes first feed an immutable close request that binds the
opening custody, prefix request, proof capture IDs, and proof body digests.
Fresh closing A/B captures for both roots are then created directly from the
live roots and wrapped against that close-request digest; a stale v2 snapshot
cannot be supplied for wrapping. Their catalogs and custody must exactly match
the opening bodies. The plan binds each opening, proof, close request, and
close wrapper by distinct capture ID or body digest as appropriate. Codex
planning reads bounded exact-`0600` single-link evidence through pinned
descriptors and publishes into a pinned owner-private directory with an OS
no-replace rename. It verifies exact staging bytes and single-link custody,
revalidates inputs plus the requested directory/target after publication, and
never permits stdout as an evidence destination or pathname-deletes on
failure. A nonzero result can therefore retain an owner-private staging or
fail-held final artifact; it is not authority and requires attended
quarantine.

The exact private-state policy lives in
`.agents/skills/bulkload/references/codex-private-state-policy.v4.json`.
It classifies Codex auth and provider-owned SQLite as explicit opt-in state,
but makes five separate readiness claims:

- `auth_install=true`: an attended, journaled atomic replacement of an
  existing destination `auth.json` is implemented, with a complete rollback
  copy, offline verification, manual rollback, and crash recovery;
- `sqlite_compose_plan=true`: a four-private-pass, recomputed-session-plan
  opening request can classify exact registry, schema, migration, path, and
  type-tagged row relations without emitting operations;
- `sqlite_compose=false`: no source SQLite family is merged or installed; and
- `sqlite_publish=false`: no composed family or versioned directory can be
  published; and
- `combined=false`: there is no combined auth-plus-SQLite apply.

The supported installer input matrices are source `["auth"]` to destination
`["auth","sqlite"]` (preferred), and source `["auth","sqlite"]` to the same
destination selection. In both cases the install plan selects the destination
state classes, consumes only source auth, emits
`sqlite_union_ready=false`, records zero SQLite mutations, and binds every
destination SQLite family as `preserve-destination-exact`. An auth-only
destination is deliberately unsupported because a changing auth file must not
bypass the destination SQLite preservation proof.

`codex-private-capture` accepts only a short-lived, digest-accepted quiescence
attestation bound to the exact role, roots, selected state classes, Codex
version, host authority, and output. The attestation states
`provider_writer_proof=false`: it records the operator's procedural fence.
The accompanying nonblocking directory `flock` coordinates only cooperating
Bulkload processes and cannot stop or detect a Codex writer.

For SQLite selection, the operator must resolve configured
`sqlite_home`/`CODEX_SQLITE_HOME`/`CODEX_HOME` authority and pass the effective
path explicitly. Any observed `-wal`, `-shm`, or `-journal` companion rejects
the immutable capture. With no sidecar present, the reader opens the source
immutably, snapshots every top-level `*.sqlite` family through SQLite's backup
API, normalizes the evidence copy to `DELETE` journal mode, runs
`quick_check`, and binds schema, migration, header, thread/path, namespace,
count, byte, and time evidence. It never copies source sidecars.

`codex-private-plan` remains a non-actionable compatibility dossier.
`codex-private-sqlite-compose-plan` consumes source A/B and destination A/B
private bundles plus the recomputed session-union closure, exact adapter
registry, and exact path map. It embeds and revalidates the complete registry
and path-map bodies while binding their digests to the accepted inputs. It is
an immutable opening request and always records that fresh post-plan close
captures remain required. It does not claim that the session union was
executed or verified. Its eight private capture and attestation IDs are
globally distinct and disjoint from session evidence. Classification binds
observed foreign-key topology and actions exactly, fail-holds every
trigger/view and unsafe or secondary UNIQUE claim, requires an exact
UUID-to-session-path binding, reapplies captured family/count/byte budgets, and
permits equal lexical roots only under distinct role-bound host authorities.
The registry-bound schema contract carries a canonical raw-schema catalog,
typed virtual/shadow omissions, and the exact producer classification-blocker
set; the family role blockers must equal that set plus reconstructed
foreign-key-shape blockers. Internal `sqlite_%` table state, including
AUTOINCREMENT high-water authority, failed or malformed migrations, and
explicit table-column collation clauses therefore remain structurally bound
blockers. Source/destination migration counts and latest versions are
cross-bound to the opening projections. Safely named unknown families are also
retained as fail-held evidence. A single aggregate ledger covers all
source/destination rows and typed bytes across every table and family, while
SQLite progress handlers enforce the shared deadline. The CLI recomputes the
complete plan against pinned inputs before and after create-only publication.
Its full contract is in the
[SQLite opening-plan reference](../.agents/skills/bulkload/references/codex-private-sqlite-compose-plan.md).
`codex-private-install-plan` consumes its exact accepted digest and compiles
only the narrow auth-install plan. Apply revalidates source auth and the entire
destination auth-plus-SQLite capture, backs up destination auth, journals each
durable transition, replaces auth in the destination directory, then captures
and compares destination auth and SQLite again. Verify uses a fresh
attestation and capture. Rollback requires an exact apply receipt plus an
operator assertion that no provider writes occurred after apply. Recovery
classifies the durable journal before choosing a bounded forward or rollback
path. Every receipt remains an offline byte-and-custody claim with
`provider_runtime_acceptance_verified=false`; only a fresh attended provider
turn can establish working authentication.

The command entrypoint opens and pins the canonical policy and complete Python
runtime source inventory before importing the command implementation. The
runtime authority record—policy digest, closure digest, and exact core source
digests—is carried by the compatibility and install plans and revalidated
through publication and mutation boundaries. A changed or replaced runtime
therefore fails closed rather than executing against a previously accepted
plan. The validator also requires the exact command/handler topology and
forbidden SQLite/combined command set.

The operator sequence, exact arguments, evidence custody, and recovery rules
are in the
[private auth install runbook](../.agents/skills/bulkload/references/codex-private-auth-install.md).

## 8. Relationship to TCFS

This tool is deliberately interim. TCFS is intended to replace the manual
working-byte plane with a product contract built from chunked content-addressed
storage, manifests, hydration, namespace composition, vector-clock/conflict
state, registered local/remote observations, Git topology, and monotonic remote
catalog control state.

The July TIN-2864 work is still source-only and digestless. It does not yet
authorize live planning, deployment, reconciliation, unreviewed credential
handling, or a crypto ceremony. `bulkload` therefore does not call TCFS.
Instead it records the exact operational requirements TCFS must eventually
absorb:

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
green transport exit code. Provider-state hazards additionally include token
disclosure, refresh races, a false quiescence claim, partial WAL capture,
stale absolute rollout paths, schema skew, an incomplete SQLite-family
inventory, changed runtime source after plan review, unintended SQLite
mutation, and replacing healthy destination auth without rollback.

V1 mitigations are canonical sorted catalogs, role-specific A/B barriers, no
content in manifests, path normalization, a v1 block on symlink mutations,
sensitive-path blocks re-enforced during plan validation, no deletion,
pre-copy source rehash, external backups, atomic replacement, durable
journal/directory entries, fresh verification, Codex evidence inputs and output
parents pinned across final rename, pathname overlap guards for the repository
adapter, and explicit incomplete/error states. Private-state policy v4 adds
pre-import runtime pinning, digest-bound procedural quiescence, a cooperating-
Bulkload lock, hard rejection of SQLite sidecars, complete destination SQLite
preservation, an external auth backup, a durable state-machine journal, and
fresh offline capture. Its separate SQLite opening request adds four-pass
stability, exact session-plan recomputation, schema/migration registry keys,
allowlisted path normalization, globally disjoint evidence identity, exact
foreign-key closure, trigger/view and UNIQUE fail-holds, bounded semantic row
preflight, complete embedded registry/path-map bodies, a canonical raw-schema
record catalog, typed schema omissions, registry-bound producer blockers,
exact stable-projection budget/count/latest-migration bindings, bidirectional
rowset/digest claims, plan-wide row/byte charging with structural lower bounds,
enforceable SQLite deadlines, exact edge/collation/table-set blocker
derivation, explicit column-collation and unknown-family blockers, and
type-tagged row digests while keeping compose, publish, install, and apply
false. It does not convert either the operator attestation or the lock into
provider-writer proof.
Cross-filesystem atomicity, ACL/xattr
fidelity, sparse files, hardlink identity, special files, case-insensitive
collisions, submodule worktrees, live concurrent writers, and provider runtime
acceptance remain blockers or separate proof steps.

## 10. Implementation layout

- `.agents/skills/bulkload/`: canonical portable skill and executable Python.
- `BUILD.bazel`: build/test SSOT for the exact skill code.
- `justfile`: human and agent entrypoint; delegates normal build/test to the
  GloriousFlywheel wrapper.
- `justfile.flywheel` and `.bazelrc.flywheel`: generated, endpoint-free
  GloriousFlywheel front-door kit pinned by CI to an immutable core revision.
- `.github/actions/bulkload-public-read-ci/action.yml`: immutable public action
  closure with audited, mutually exclusive source/build/test terminal paths.
- `.github/workflows/ci.yml`: direct `tinyland-nix` cache-first validation with
  a literal three-gate matrix and exact minimal-environment, no-post Git
  materialization; no hosted or dynamic runner fallback.
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
10. Narrow attended Codex `auth.json` install with exact destination SQLite
    preservation, offline verification, rollback, and recovery.

Deferred:

- SQLite union, composition, or installation, including WAL-aware live-family
  capture;
- combined auth-plus-SQLite apply;
- browser-state movement;
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
- Codex authentication documentation: trusted headless-device `auth.json`
  copy fallback and password-equivalent handling,
  <https://learn.chatgpt.com/docs/auth>.
- Codex configuration and app-server documentation: `sqlite_home`,
  `CODEX_SQLITE_HOME`, JSONL rollout logs, SQLite-backed resumable state, and
  default JSONL scan-and-repair behavior,
  <https://learn.chatgpt.com/docs/config-file/config-reference> and
  <https://learn.chatgpt.com/docs/app-server>.
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
