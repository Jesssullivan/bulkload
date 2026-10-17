# 2026-10-16: partial clones and malformed inventories (M2 item 6)

Lane: M2 "bulkload is one command", item 6, worktree
`bulkload.worktrees/m2-partial-20261016`. Both halves are refusals the
in-place run carried as files by hand (`inplace-20261016/plan/
i01-refused.tsv`: ten `GIT_SOURCE_PARTIAL_CLONE`, one
`GIT_INVENTORY_MALFORMED`, then `scripts/sting-carry.sh`). Rulings cited:
Q185/Q192 (nothing excluded), Q194 C14 (carry non-recreatable leftovers as
files), S2 / WP1 (never interrupt or lazily fetch on the source), R-N13.

Status: the malformed half is a regression row (not a reproduction) plus the evidence still
needed; the partial-clone half is a design with experiments, not built.
Both say why below.

## 1. `GIT_INVENTORY_MALFORMED` on telefreeze

What is known: on neo, the capture of `/Users/jess/git/telefreeze` (a main
with 28 linked worktrees under `telefreeze.worktrees/`, planned with the
main and every worktree as items) refused `GIT_INVENTORY_MALFORMED`. The
capture ran on neo with the lane's binary `bulkload-agent-5beb0314900f`;
its stderr and the repository are on neo, which this lane may not read.
On sting the apply then reported the main `CAPTURE_ABSENT` and the 28
worktrees `GIT_REPOSITORY_NOT_AT_PATH` (`logs/i01-*.apply.out`).

Where the refusal can come from: the code returns it in about 350 places
(`git_carry.rs` 163, `ref_table.rs` 30, `converge.rs` 29, `estimate.rs` 27,
`native_refs.rs` 17, `shared.rs` 13, `shallow.rs` 11, `batch_objects.rs`
10, `chain.rs` 8, ...). In the two files named for it:

- `batch_objects.rs`: `cat-file --batch` framing (an answer header that is
  not `<oid> blob <size>` for the asked oid, over 127 bytes, or not UTF-8; a
  short body; a missing terminator; bytes left when the reader closes). A
  restore-side reader: it would refuse at apply, not at capture.
- `chain.rs`: a bundle header that is not `# v2`/`# v3`, a line without
  `<oid> <name>`, a non-oid value (`advertised`); a `cat-file
  --batch-check` answer with a bad oid (`source_held_tips`); a flattened
  chain whose heads differ from the head link's (`flatten`, restore side).

`advertised` runs at capture, on a first capture too (a review of this
note corrected it): items that share one common Git directory form a
plan-base group (`capture_groups`), telefreeze's main and its 28
worktrees among them, so the group's base is exported once
(`prepare_base`, `shared::export_base`) and each item's capture declares
its prerequisites from it (`shared::write_capture`,
`prerequisite_commits`, which itself refuses `GIT_INVENTORY_MALFORMED` for
a base ref outside `refs/carry-export/`, an answer with extra lines, or no
commits). So the suspects are the shared-base path (the base export runs
from whichever group member arrives first, which may be a linked
worktree) beside the census and export in `git_carry.rs` and
`ref_table.rs`. That only the main refused points to a per-item cause.

Reproduction attempted on sting with the current main (git 2.52.0): a main
with 28 linked worktrees, each on its own branch with its own commit, plus
a detached worktree, a locked one, a prunable one whose directory was
removed, per-worktree refs (`refs/bisect/*`, `refs/worktree/*`), a stash
and a staged file, `extensions.worktreeConfig` with a per-worktree key,
worktrees nested in the checkout (`.claude/worktrees/*`, `.worktrees/*`),
two worktrees whose administration names collide, an orphan branch in a
worktree, packed annotated tags. Every item captures and lands. The same
row run against the night's binary 5beb0314900f also captures every item
(it then refuses at apply only for the absent parents item 1 fixed), so
it does **not** reproduce telefreeze: it is a regression row for these
shapes, not a pin of that defect (`tests/estate_many_worktrees.rs`).
Known gaps of the row: the per-worktree refs land as provenance only (as
designed), and a lock reason and `config.worktree` are not carried; the
test asserts HEADs, the stash and the staged entry.

Evidence still needed (read-only, from neo):

1. The capture's own lines for the item: `capture-i01-*.err` and `.out`
   under the neo capture root (`logs/`), around `telefreeze`.
2. `git --version`; `git -C ~/git/telefreeze worktree list --porcelain`;
   `ls ~/git/telefreeze/.git/worktrees`; `git -C ~/git/telefreeze
   for-each-ref --format='%(refname)' | wc -l` and any ref name that is not
   plain ASCII; `git -C ~/git/telefreeze config --show-origin --list`
   (extensions, `core.*`, `worktree.*`); `ls .git` for `shallow`,
   `packed-refs`, `info/grafts`, `objects/info/alternates`.
3. A rerun of `estate-capture` with the current main binary on a one-item
   plan of telefreeze (read-only on the source): if it now captures, the
   5beb031 binary's defect is fixed already; if it still refuses, the
   output of `git-carry-estimate` on the same repository narrows the stage.

A follow-up worth making regardless: a refusal carries no site, so this
class of report cannot be narrowed from a receipt. A `#[track_caller]`
constructor for `GIT_INVENTORY_MALFORMED` that records the caller's
location in the receipt's detail would.

## 2. Carrying a partial clone natively

Today `preflight` refuses a partial clone (`refuse_partial_clone`: a
promisor remote, a partial-clone filter, `extensions.partialClone`, or a
`.promisor` pack in any followed store) before any read, because every
capture is a `git bundle create --all`, which needs every reachable
object, and the hardened environment forbids the lazy fetch that would
supply the missing ones (S2: never fetch on the source). The lane carried
the ten such repositories as plain files.

Experiments (sting, git 2.52.0, a `--filter=blob:none` clone of a local
origin with `uploadpack.allowFilter`):

| Command | Result |
| --- | --- |
| `GIT_NO_LAZY_FETCH=1 git bundle create b --all` (blobs missing) | `fatal: could not fetch <oid> from promisor remote` |
| `git bundle create b --filter=blob:none --all` | a v3 bundle with `@filter=blob:none`; `git bundle unbundle` into a repository with `remote.origin.promisor=true` and the filter lands it as a `.promisor` pack |
| `git bundle create b --all --missing=allow-promisor` | `unrecognized argument` |
| `git rev-list --objects --all --missing=allow-promisor \| git pack-objects --stdout --missing=allow-promisor` | a pack of exactly the present objects, no fetch |

So `--filter` alone is not a carry: it drops every blob of the filtered
kind, including blobs the source holds locally and the promisor does not
(a new commit's files, the capture's own worktree and index trees). The
native carry needs the present-objects pack.

Two gaps a review of this design found (git 2.52.0):

- The capture packs in bulkload's private repository, which borrows the
  source's objects through `alternates`. There, `rev-list --objects --all
  --missing=allow-promisor` fails (`fatal: unexpected missing blob
  object`) unless the private repository itself has
  `remote.<r>.promisor=true` and a URL, which arms lazy fetch inside it.
  Promisor mode must configure that and set `GIT_NO_LAZY_FETCH=1` on every
  child there.
- A sparse checkout's index can name a blob the clone never fetched (an
  `S` entry outside the cone). The index needs it, not the worktree, so
  the apply's completeness check and the converge proofs (which read index
  blobs) must refuse that shape, or carry such entries by name only.

Proposed design ("promisor mode"; not built):

1. **Capture.** A partial-clone source is captured in promisor mode
   instead of refused: refs, stash, index and worktree trees as today; the
   pack is every object the source holds (`rev-list --objects
   --missing=allow-promisor`, then `pack-objects --missing=allow-promisor`
   through the hardened builder, lazy fetch off), written as a v3 bundle
   whose header carries the source's filter (`@filter=`) so no reader
   expects completeness. A sidecar `{bundle}.promisor` records each
   promisor remote (name, URL, `partialclonefilter`) and
   `extensions.partialClone`, as-is (R-C841: the URL is carried like any
   configuration). The capture key gains the promisor configuration, so a
   changed remote or filter is a new capture.
2. **Apply.** The staged repository is initialised with the recorded
   promisor configuration before the import; the pack is indexed with
   `index-pack --promisor` so it stays promised; `bundle verify` is replaced
   for this mode by a check that every ref tip and every tree and blob the
   landed worktree and index need are present (they were present at the
   source, or the capture would not have them). Checkout reads only
   present blobs; lazy fetch stays off during the apply. The landed
   repository is a partial clone with the source's remotes; a later `git
   fetch` fills it.
3. **Converge and adopt.** Proofs read worktree and index blobs, which are
   present; history blobs a proof never reads may be missing. Adopting a
   destination partial clone requires the same promisor configuration.
4. **Refusals kept.** A promisor remote whose filter the destination Git
   cannot parse, a sparse checkout whose worktree needs a missing blob, and
   a promisor pack in an `alternates` store keep a typed refusal.

Proofs to build with it: P-PARTIAL-1 (a blob:none clone with a local-only
commit lands as a partial clone with every present object and the local
commit's blobs, no fetch on either side: the source's `ls -l
.git/objects/pack` and the network unchanged); P-PARTIAL-2 (an unchanged
rerun is a reuse hit; a new local commit converges); P-PARTIAL-3 (the
apply never lazy-fetches: an unreachable promisor URL still lands).

Why not built tonight: it changes the capture key, the export, the
bundle verification and the staged-repository initialisation, each of
which carries crash, reuse and S2 proofs; it needs its own adversarial
review. The hand carry (files, `--sqlite=snapshot`) stays the path until
then, and `estate-move` (item 7) folds it in as "carried as files".
