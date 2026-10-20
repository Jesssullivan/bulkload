# Bulkload design

How bulkload works: its contracts, records and refusal codes. What it must
achieve and when it is done is [SPEC.md](SPEC.md) (#236); the SLOs and their
proofs are [slo.md](slo.md); [SOURCES.md](SOURCES.md) maps the rest.

## Completion

Done is defined in SPEC.md (OI-1003-Q194 C20). On the design side: coverage
includes both hosts' unique Git and agent state, credentials, dots, SQLite
and worktrees. Unsupported items remain outstanding with preserved custody;
nothing is silently excluded (OI-1003-Q185, Q192).

Closure is bulkload's own gate, never a metric defined outside it
(OI-1001-Q2). `closure-report PLAN CORPUS SOURCE PRIVATE_STATE...` reads the
plan, the corpus's capture records and the apply ledger (outcome records and
`.done` journals). It ends every planned item as one of:

- `applied`: the item plans a workspace, and the exact journal for its
  current capture and SOURCE says `workspace-restored`.
- `refused-pending-review` (S4, 2026-10-06): a typed refusal code that no
  operator review disposes yet. The item is accounted for, but the run is
  not complete.
- `refused`: a typed refusal code that a review disposes (accept, re-carry
  or abandon; before 2026-10-06 this meant any typed refusal). A bare `IO`
  or `FRAME_CODEC` names no cause and does not close an item, and neither
  does a code that has since left the taxonomy (`refusal-code-retired`).

  Refusal codes stay typed at the source (WP3, 2026-10-03). Every taxonomy
  code is raised by product code, never kept as unused vocabulary. A Git
  child that exits non-zero refuses `GIT_CHILD_FAILED` with a
  `stderr_class=` from a closed set; its stderr is classified, never echoed
  or kept (R-N121). A child whose stderr says it ran out of space or quota
  (class `no_space`) refuses a space code instead (#220): the verb names
  where its children write (see Space). No blanket
  conversion turns an OS or codec error into a refusal: each site names
  itself with `.refuse_at(site)`. The sites that still raise a bare `IO`
  with no errno are an allowlist that only shrinks. A bare `IO` carries its
  errno; it carries no path, so a site that knows its path and a typed
  cause raises the typed code (`SPACE_EXHAUSTED` names its directory).
- `referenced-only`: the item plans no workspace, and the exact
  current-capture journal says `refs-imported`. `git-repair-missing-index`
  given `PLAN CORPUS PRIVATE_STATE` binds its bundle to the planned item
  whose current capture it is (by digest and repository) and writes the same
  journal, then the outcome `index-repaired` (or `refused` with its code), so
  a repaired item reads as referenced-only natively (#95). An item that plans
  a workspace never binds: the repair refuses `RECEIPT_BINDING_INVALID`
  before writing anything, and estate-apply restores the workspace (#132).

Anything else is `unaccounted`: a stale journal from an earlier capture, a
record naming another source, refs only for an item that plans a
workspace, or a `GIT_CONVERGE_INTERRUPTED` refusal (`converge-interrupted`:
a converge left its checkout part old, part new under a pending intent; no
review or attestation closes it, only a rerun that settles it, 2026-10-08). Stale and foreign journals are listed. The native `verdict`
passes only when `unaccounted` is 0. The report's `gate` is the exit
status, and a failing gate exits nonzero with `CLOSURE_UNACCOUNTED`. Since
S4 (WP3 PR 3, 2026-10-06) `gate` no longer equals `verdict` without an
attestation ledger: it also fails while any typed refusal is
`refused-pending-review`, with or without `--attest`. `CLOSURE_UNACCOUNTED`
therefore also covers refusals pending review, and can be raised while
`verdict` is `pass` and `unaccounted` is 0 (`totals.pending_review` says how
many). A pass is necessary for completion, not sufficient: the daily-work
bar above still applies.

`closure-report --attest LEDGER.json PLAN ...` (#95; the option is
recognised only before PLAN, #133) joins a `bulkload.closure-ledger.v1`
attestation ledger for items closed by audit rather than by a verb. The
ledger must name this plan (`plan`) and SOURCE label (`source_label`). A row
closes a planned item only when the native ledger leaves it unaccounted, and
only with a matching source, the item's current capture digest (`capture`,
as the native item row prints it; `null` only when the corpus holds no
capture record), a known disposition (`applied`, `refused` with
a typed code, `referenced-only`, `present`, `source-absent`), a basis other
than `native-closure-report`, and non-empty evidence. Rows are reported in a
separate `attested` block with their evidence; native totals are unchanged
and a native record is never overridden (rows for natively closed items are
counted as superseded, and disagreements listed). The top-level `verdict`
stays native; `gate` passes only when every item is native-accounted or
attested (#133). A ledger missing or naming another plan or SOURCE label,
or listing an item twice, refuses.

S4 (WP3 PR 3, 2026-10-06): outcome records are typed (`Outcome`,
`Refusal{code, site, errno}`, postcard with no bytes left over; legacy string
records are mapped), and a typed refusal is `refused-pending-review` until
`closure-dispose` records an accept, re-carry or abandon review for it;
`closure-report --dispositions LEDGER` joins those reviews, and `gate` then
also requires every typed refusal, native or attested, to be reviewed. A
bare `IO` can be neither reviewed nor attested away: whether an attestation
may close an item is decided from the item's decoded record, so a bare `IO`
recorded under another source (`record-source-mismatch`) is still refused
(`native-refusal-untyped`).

- **Item reviews are bound to the refusal instance.** `closure-dispose
  [--attest LEDGER.json] LEDGER PLAN CORPUS SOURCE ITEM CODE DECISION
  REVIEWER DATE PRIVATE_STATE...` reads the same inputs as `closure-report`
  and refuses (`RECEIPT_BINDING_INVALID`) unless ITEM holds a typed refusal
  with CODE now, so no review is written ahead of its refusal. The row
  records that refusal's `instance`: a digest of the item's current capture
  and its outcome record (source, code, site, errno, reason), printed on
  every item row. A later refusal of the same code for the same item against
  another capture or with another record is another instance; the old row
  does not dispose it and is listed under `reviewed.unmatched` as
  `refusal-instance-stale`. The record carries no time, so two refusals with
  the same capture and a byte-identical record are one instance.
- **Standing policies are open-ended.** `closure-dispose LEDGER PLAN CORPUS
  SOURCE --policy CODE ...` disposes every refusal with CODE under the bound
  plan and SOURCE label, now or later, for any item. It names no instance;
  the plan digest and the label are its only bounds.
- **The ledger is bound to the plan's bytes.** A disposition ledger records
  its plan's path and a blake3 digest of the plan file. Reading or appending
  refuses (`RECEIPT_BINDING_INVALID`) when the plan at that path now has
  other bytes, and `closure-dispose` reads PLAN for every row, so a ledger is
  never created for a path that holds no plan. Appending an item to the plan
  (`estate-add`) therefore starts a new ledger.
- **Retired codes fail closed per row.** Writers record only a code the
  taxonomy holds now. Readers accept any well-formed code token: an outcome
  record naming a code that has since left the taxonomy reads as
  `unaccounted` with `refusal-code-retired` (never as unreadable), no review
  or attestation closes it, and only a verb recording a current outcome
  does. A review row naming such a code disposes nothing and is listed under
  `reviewed.unmatched`; the rest of the ledger keeps working.

**Capture-refused items carried as files (M2r2 b, 2026-10-17).** An item
whose capture refused has no capture record; `estate-apply` records it
`CAPTURE_ABSENT`, and closure holds it `refused-pending-review`. The cases
are a partial clone, a malformed inventory, a missing index, a nest
conversion attribute and a failed Git child. The in-place run carried 14
such items with hand-run `pull` sessions, which closure could not bind to
the plan.

`estate-carry-files [--dry-run] [--beside SUFFIX] [--item ID]... [--host
HOST --source-state-root DIR [--remote-executable PATH] [--ssh-config FILE]]
PLAN CORPUS PRIVATE_STATE SOURCE CARRY_STATE` carries them with bulkload's
own transfer and records the result in the estate ledger.

- **Eligible.** An item is eligible when the corpus holds no capture
  record for it, and its outcome record is
  `CAPTURE_ABSENT`, or an earlier `carried-as-files`, which is re-proved.
  Every other item is printed `skipped=<why>` and left alone.
- **Destination.** The destination is the planned workspace, or with
  `--beside` the workspace path with SUFFIX appended (a suffix holding `/`
  or NUL refuses `FIELD_DOMAIN_VIOLATION`). An absent destination is made
  one level deep, 0755, under an existing parent; otherwise the item
  refuses `GIT_DESTINATION_PARENT_MISSING`.
- **One transfer session.** Each item gets one `copy` or `pull` (from
  HOST, with the remote source state under DIR) recorded in
  `CARRY_STATE/<item>/dest`, like any session. The transfer's rules hold:
  - bytes are carried exactly, `.git` included;
  - an equal destination file is adopted with nothing written;
  - a differing one is refused and never overwritten;
  - the transfer's own durability order applies.

  A session that ends refused, or completes with a refused path, does not
  carry the item. It prints `carried=incomplete` with the first refused
  path, keeps its outcome, and the verb exits nonzero. The transfer's own
  rows, joined with `--transfer`, name every refused path.
- **Binding an earlier session (`--bind ITEM=DEST_STATE`).** The item is
  bound to a session the same transport already recorded in that state:
  same source key, same destination, completed, with no refused path. No
  session runs and nothing is written under the destination; the `.git`
  resolution and the records below are the same. This is how bulkload proves a
  carry made earlier by its own `pull` (the in-place run's hand-run
  sessions). Without such a record the item refuses
  `CLOSURE_UNACCOUNTED` and keeps its outcome. A later session rewriting
  that record unproves the carry (`carried-transfer-changed`) until it is
  bound again.
- **`.git` resolution (#261).** Whenever anything landed (a completed,
  incomplete or refused session, or a bind), every directory below the
  destination that holds a `.git` entry (a directory, a pointer file, a
  symlink, or a symlink to a pointer file) is resolved as Git resolves it.
  Git's `rev-parse` gives its git dir, common dir and top level; it runs
  with no optional locks, no hooks, no fsmonitor and no discovery above
  the directory. Its object alternates and every symlink inside its
  administration are resolved too. Each result must be inside the carried
  tree, or Git must be unable to use the directory at all (an
  administration absent on this host). Anything outside refuses
  `GIT_CARRY_POINTER_ATTACHED path=<the .git entry>`, recorded as the
  item's outcome, because Git would act on another repository through the
  carried copy. That covers:
  - a pointer naming an outside repository;
  - a `.git` symlink to a pointer file;
  - an internal worktree administration whose `commondir` is outside;
  - a nested `.git` directory with an outside `core.worktree`;
  - outside alternates.

  An incomplete session reports an attachment too. The landed bytes are
  left for review; nothing is rewritten. Nothing is withheld before
  landing, because the source walk (local, or the remote `serve`) has no
  per-path exclusion. Admin-internal gitfiles that Git reads only for
  pruning (`worktrees/*/gitdir`) are not resolved.
- **Records, in order.** The carry record
  `{item}-{blake3(SOURCE)}.carried` is written first, durably (temporary,
  fsync, rename, directory fsync). It holds the item, source, destination,
  `beside`, whether the carry made the destination, the transfer state, the
  session's row identity and the blake3 of its record bytes, and the
  pointers. Then the outcome becomes `carried-as-files` (a new `Outcome`
  variant, appended after `Refused`).
  - A crash between the two leaves `CAPTURE_ABSENT`, and closure never
    reads such an item `carried`; the rerun's transfer adopts what landed
    and finishes it (fault point `estate.carry.recorded`, harness row
    `estate_carry_recorded`).
  - `estate-apply` keeps a carried item's outcome while its carry still
    proves, so a re-apply never returns it to `CAPTURE_ABSENT`.
- **Closure (v5).** Closure counts the item `carried` (accounted, like
  `applied`) only when all of these hold:
  - the outcome is `carried-as-files`;
  - the carry record decodes and names the item and its source;
  - its destination is the planned workspace with the recorded suffix, or,
    for a refs-only item, none of its own;
  - the transfer record it names still has the recorded bytes, and is a
    completed session from this item's source into that destination with
    no refused path;
  - every recorded `.git` directory still resolves inside the destination
    or not at all;
  - for a refs-only item, its `via` names a planned workspace item of the
    same source whose own outcome is `carried-as-files` and whose own carry
    proves.

  Otherwise it is unaccounted, with a reason such as
  `carried-record-missing`, `carried-destination-mismatch`,
  `carried-transfer-changed`, `carried-transfer-mismatch`,
  `carried-transfer-refused-paths`, `carried-pointer-attached`,
  `carried-shape-mismatch` or `carried-via-unproved`.
- **What `carried` means.** `carried` means the item was carried at the
  session's time.
  - Checked again on every proof (closure, and `estate-apply` keeping the
    outcome): the transfer record's bytes, and each recorded `.git`
    directory's resolution, so a target that appeared since (a sibling
    applied later beside it, say) makes the item unaccounted.
  - Not checked again: the destination's bytes, and `.git` entries made
    under it after the carry.
- **Byte-exact, with the transfer's two exceptions.** A live SQLite
  database is carried as a snapshot (`--sqlite=snapshot`), its live
  sidecars covered, not copied. The source's tagged engine temporaries are
  listed by the transfer, not carried.
- **Selections are checked before any write.** Every `--item` and
  `--bind` must name a planned, eligible item, and a bind must name a
  workspace item; otherwise the run refuses `FIELD_DOMAIN_VIOLATION`, one
  line per bad selection, before any lock or write. `--bind-only` also
  refuses a selected workspace item that has no bind, so no session can
  run by accident.
- **Reruns.** An item already carried whose carry still proves is kept
  (`kept=proved`) with no session, unless `--recarry` or a bind for it asks
  again. One whose carry no longer proves needs either.
  - A rerun that ends refused or incomplete for an item that was carried
    writes `CAPTURE_ABSENT` back, for it and for every refs-only item
    riding on it.
  - Carrying a workspace item again also re-records the refs-only items
    riding on it.
- **Binaries.** Only binaries with this change know `carried-as-files`
  (outcome variant 11; M2r2 c's `workspace-beside` and
  `previous-workspace-beside-not-revalidated` are 12 and 13). An older binary reads such a record as unreadable,
  and an older `estate-apply` records `CAPTURE_ABSENT` over it. Run the
  carry and every later apply with the merged binary.
- **What changed for a v4 reader.** `bulkload.closure.v5` (M2r2 b and c,
  one schema):
  - the disposition `carried`, counted in the new `totals.carried` after
    `covered` (accounted; it is in neither `applied` nor
    `pending_review`);
  - the disposition `applied-beside` (M2r2 c: an existing planned checkout
    the adopt proof refused `GIT_DESTINATION_DIVERGED`, landed beside by
    `estate-apply --adopt --beside SUFFIX` with every captured ref in; see
    the `--beside` paragraph), counted in the new `totals.applied_beside`
    right after `applied` (accounted; in neither `applied` nor
    `pending_review`); its outcome is `workspace-beside` (or a rerun's
    `previous-workspace-beside-not-revalidated`), and it is unaccounted
    `beside-record-missing` or `beside-record-mismatch` unless its
    `{item}-{blake3(SOURCE)}.beside` record names the item, the current
    capture and no pending or kept ref;
  - the totals, in order: `planned`, `transfer_rows`, `applied`,
    `applied_beside`, `refused`, `refused_pending_review`,
    `referenced_only`, `covered`, `carried`, `unaccounted`,
    `pending_review`, `ruled_default`;
  - a `carried` block on such a row: `via`, `destination`, `beside`,
    `transfer_state` and `gits`;
  - the new unaccounted reasons above;
  - (gap C12, 2026-10-19) on a row with a typed refusal, a `refs_in` block
    (`ns`, `created`, `kept`) when the item's `.refs-in` record names its
    current capture (a refused beside's refs went in all the same), and,
    for `GIT_BESIDE_FOREIGN`, `refusal_detail`, the refusal as recorded
    (it names the beside path). Both are added keys; no total, disposition
    or gate changes;
  - (2026-10-19) on an `applied-beside` row, `beside_reason`: the refusal
    code that sent the capture beside (`GIT_DESTINATION_DIVERGED`,
    `GIT_DESTINATION_NONREPO_DIFFERS`), from the item's `.beside-reason`
    record. An added key only.

  Nothing else changed.
- **No-op applies.** A re-apply keeps a carried item's outcome as a no-op,
  like a previous journal: its repository is not marked touched.
- **Refs-only items.** A refs-only item whose capture refused (the plan's
  `repository` only) has its custody, the source's `.git`, inside a carried
  workspace item of the same source. After the workspace items, it gets a
  carry record naming that item (`via`) and binding the same transfer
  record, and the outcome `carried-as-files`. Nothing is written at its
  repository. Without a carried sibling it keeps `CAPTURE_ABSENT`. Closure
  requires `via` exactly for a refs-only item
  (`carried-shape-mismatch` otherwise).
- **`--dry-run`.** It reads only and prints, per item, the destination,
  what is there now (`absent`, `empty`, `nonempty`), whether its parent
  exists, the prior transfer records in its state, and the session it would
  run.

**Transfers in the same report (WP3 PR 4, 2026-10-15, S4).** Every `copy`
and `pull` session leaves a durable record in its DEST_STATE, one per
transfer under `transfer.closure.d/<item>.closure` (`<item>` is the
session row's identity; `bulkload.transfer-closure.v1`, postcard after a
magic, decoded strictly). The session first makes the verb's S2 overlap
decision (`transfer::copy_roots_apart`, `receive_roots_apart`), so a
refused overlap writes nothing; then, when DEST_STATE already exists (a
record never creates a state root), takes the record's lock (a second
session of the same transfer refuses before it writes) and writes
`running`; and writes its end when it ends. A rerun of the same transfer
replaces its record; another transfer sharing the DEST_STATE has its own.
A session that ends refused, or never ends, keeps the refused and covered
rows of the record it replaced; only a completed session (which walked
the whole source again) replaces them. An unreadable earlier record is
never overwritten, and a DEST_STATE that exists after a session but took
no closing record refuses `CLOSURE_UNACCOUNTED`. `closure-report
--transfer DEST_STATE` (repeatable, only before PLAN; canonicalized, so one
state under two spellings refuses, as does any transfer row joined twice)
joins every record there as rows of the
same report, totals and gate:

- a **session** row (`kind: transfer-session`): `applied` when the session
  ran to its end; a typed refusal when the session itself refused; and
  `unaccounted` when the record still says `running`
  (`transfer-interrupted`: a crash or a kill cut it short), when there is no
  record (`no-transfer-record`) or none that decodes
  (`transfer-record-unreadable`), or when the session refused with a bare
  `IO` or `FRAME_CODEC`;
- one **entry** row per refused path (`kind: transfer-entry`), classified
  exactly as an estate item's recorded refusal (`closure::refusal_disposition`):
  pending review until a review disposes it, never closed when untyped or
  retired. `SQLITE_INTEGRITY_CHECK_FAILED`, and `SQLITE_BACKUP_FAILED` whose
  primary code is 11 or 26, carry their ruled default disposition,
  `abandon` (OI-1003-Q148): it disposes the row unless an item review or a
  standing policy says otherwise (the review wins), and the row's `review`
  prints `basis: ruled-default` with the ruling;
- one **covered** row per live `-wal`, `-shm` or `-journal` the source
  reported beside a database carried as a snapshot (`covered`, accounted);
- one **applied-beside** row per seat a `copy` or `pull` with `--beside
  SUFFIX` landed beside a differing live destination file (plan of record
  2026-10-19, C1/C2; `applied-beside`, accounted and counted in
  `applied_beside`, the row printing `beside`, the path it landed at).
  Under the flag a file bulkload did not write is still adopted when its
  bytes are the seat's and extended when it is a verified byte prefix
  (M2r2 A-103); one with other bytes, a differing symlink and a database
  with a live sidecar beside it land at `<path>SUFFIX` under the beside
  path's own row, so a rerun reuses them reading nothing. A beside path
  holding a foreign file with other bytes refuses `DESTINATION_OCCUPIED`;
  only the store's own beside output is superseded (WP0(d)).
  `estate-move --sessions` pulls both agent homes this way into the live
  home, joins both records into its closure, and holds its gate on any
  re-seat outcome that leaves a session not resumable (C9 to C11).

A transfer row's identity (`item`, 64 hex) is a digest of the transfer's
source (`SOURCE`, or `HOST:SOURCE` for `pull`), its destination, the path
and the code (or `covered`, `applied-beside`); its refusal `instance` digests the row's
record, so a rerun that refuses identically keeps its review and any other
record is a new instance. `closure-dispose --transfer DEST_STATE` reviews a
transfer row as it reviews a planned item, in the same plan-bound ledger.

A run's expected transfers are bound to its PLAN by a transfer manifest
(`bulkload.transfer-manifest.v1`, text): `transfer-expect MANIFEST PLAN
copy|pull ...` lists each transfer (named as the verb records it, its
DEST_STATE canonical), creating the manifest with the PLAN's digest and
`since` (now) when absent. `closure-report --transfer-manifest MANIFEST`
(and `closure-dispose`) refuses `RECEIPT_BINDING_INVALID` for another
PLAN's manifest and joins exactly the listed transfers: one with no record
is `no-transfer-record`, one whose session started before `since` is
`transfer-record-stale` (its entry rows still joined), and a record of
another verb is `transfer-record-mismatch`, all unaccounted. Creating the
manifest when the closing pass starts makes `since` that pass's marker.
The report prints a `transfer_manifest` block when one is joined; without
one it covers only the transfers named, which the absent block shows.
An attestation names planned items only: it never closes a transfer row.
Not joined: `snapshot` and the offline compose verbs, which belong to no
run; a source's tagged engine temporaries a walk skipped are listed by the
transfer, not rows.

The report schema is `bulkload.closure.v5` from M2r2 b and c (2026-10-17;
what changed for a v4 reader, both dispositions, is in the carried-as-files
section above). v4 came from #226 (OI-1003-Q171/Q172, 2026-10-15). What
changed for a v3 reader: one top-level block after
`unmatched_journals`, `omissions`: the rebuildable roots the planned items'
current captures omitted (`node_modules`, `.direnv`, `__pycache__`,
`.terraform` and the rest of `REBUILDABLE_DIRECTORIES`), from each
capture's `{bundle}.omissions` sidecar: `items_known`, `items_unknown`,
`roots`, `entries`, `bytes`, one row per root (`item`, `path`, `entries`,
`bytes`) and the `unknown` items, never counted as zero. Rows, totals,
dispositions and the gate are v3's: an omitted root is accounted custody
outside `planned` and every disposition. "Equals the source" reads
exactly: equal except these counted roots.

A capture without the sidecar (M2 round 2, d, 2026-10-17). Every capture
since `bf10700c` (authored 2026-09-22) names what it omitted in its own bundle, as
the metadata ref `refs/carry-export/rebuildable-omissions-v1`, present only
when it omitted something; the sidecar arrived with `3fe1f075`
(2026-10-16). The 2026-10-16 in-place batches were captured on neo by
`5beb0314`, between the two, so `closure-report` on i02 listed all 900
items `unknown`. Without a sidecar the report now reads the bundle's
header (never its pack): no omission ref means the capture omitted
nothing (known, zero roots); the header is read strictly, and a line, a
capability or an oid a capture header does not have is
`header-unreadable`, never zero. With `--omissions-from-destination`
(opt-in: it reads the live destination repositories), the ref's list is
read by the oid the header names from the item's destination repository,
which holds it once the item's import ran, and a shallow custody
envelope's inventory is read from the same repository first. Reads only
(`cat-file`, `rev-parse`, `ls-tree`, `--no-optional-locks`); nothing is
unbundled or written. A destination is read only with a `git` at or over
2.46 (so `GIT_NO_LAZY_FETCH` keeps a missing object from being fetched)
and never when it is a partial clone, and only the shape a capture writes
is taken: a commit whose tree is one `100644 blob` entry named `value`.
Objects are not hashed again: that needs a SHA implementation the agent
does not carry or a writer subcommand the S2 contract's pinned writer
table does not admit (bulkload#188), so an object planted in the
destination under the metadata oid reads as what it says. Like the
sidecar, nothing checks the bundle's own digest here: a pack corrupted
after its header still reads as its header says.
What stays unknown says why on stderr, outside the v4 JSON:
`omissions-unknown <reason>=<items> ...`, the reasons `no-capture`,
`capture-unreadable`, `sidecar-unreadable`, `header-unreadable`,
`value-not-read` and `envelope-not-read` (named in the bundle, destination
not read), `value-not-held` and `envelope-not-held` (read, but the
destination does not hold it, for example before the apply),
`value-unreadable` and `envelope-unreadable` (held, but not the shape a
capture writes, over the cap or not decodable),
`holder-partial-clone` and `holder-git-too-old`. `estate-diff` and `estate-handoff` still read the
sidecar only.

The report schema was `bulkload.closure.v3` from WP3 PR 4. What changed for
a v2 reader: a transfer row names its `kind` and adds `transfer` (the
DEST_STATE), `path`, `transfer_outcome` and, with its refusal, `sqlite_code`
(an estate row prints as in v2); `disposition` can be `covered`; the totals
add `transfer_rows`, `covered` and `ruled_default`, and `applied`,
`refused`, `refused_pending_review` and `unaccounted` count transfer rows
too (`planned` still counts planned estate items only).

The report schema was `bulkload.closure.v2` from WP3 PR 3 (it was
`bulkload.closure.v1`). What changed for a v1 reader:

- `disposition` can be `refused-pending-review`; `refused` means reviewed.
- `totals.refused` counts reviewed refusals only (0 without
  `--dispositions`); `totals.refused_pending_review` and
  `totals.pending_review` (native plus attested) are new. The `refusals`
  map still counts every typed refusal by code, reviewed or not.
- Each item row gains `instance`, and `review` when a review disposes it.
- `refusal` (with `site` and `errno`) is still printed for typed refusals
  only. A recorded refusal that is not typed for this item (a bare `IO` or
  `FRAME_CODEC`, a retired code, a record naming another source) is printed
  as `recorded_refusal` with the same `site` and `errno`.
- `outcome` is still `"refused"` for a legacy refusal record that names no
  code (`refusal-untyped`).
- The `reviewed` block (`schema`, `ledger`, `plan_digest`, `totals`,
  `decisions`, `policies`, `unmatched` with a `reason` per row) is new, and
  the `attested` block gains `totals.pending_review`, a `pending_review`
  list and `review` on reviewed rows.

## Live union

Both hosts stay usable. Bulkload never signals sessions, never requires a
stillness pair and never requires writers to pause.

Source safety (S2, WP1):

- Every Git child is built from one hardening table (`git_carry::git_env`):
  no hooks, fsmonitor, automatic gc or maintenance, optional locks, lazy
  fetch, system or global config, or replace objects; the C locale; and a
  discovery ceiling at the given path's parent. The estimate's remote probe
  is tested against the same table. Exceptions: the agent's four child builders
  the P76 registry tolerates and argues (`BYPASSES` in
  `crates/bulkload-agent/tests/source_command_registry.rs`, #188), among them the estimate's local
  probe `bash` (its scrubbed environment is a hand copy of 9 of the table's
  11 cleared keys) and its remote probe over ssh.
- A partial-clone source refuses `GIT_SOURCE_PARTIAL_CLONE` before any other
  read, in `git-export` and `estate-capture`.
  A native carry ("promisor mode": the present objects only, packed with
  `--missing=allow-promisor`, the promisor configuration carried as-is,
  the landed repository a partial clone) is designed, with experiments,
  in `docs/agent-notes/2026-10-16-partial-clone-carry-design.md` (M2 item
  6); not built. Until it is, such a repository is carried as files.
- The source's object store is read-only to a capture (#162).
  - Git freshens (re-stamps the mtime of) any existing copy of an object it
    writes, alternates included.
  - So the private repository's object writers (`hash-object -w`, `mktree`,
    `write-tree` and its archival commits) run against a write store that
    borrows nothing (`git_env::WRITE_STORE`).
  - `fast-import` never explodes its pack into loose objects
    (`fastimport.unpackLimit=0`).
  - Readers see the write store, then the source, through `alternates`.
  - The P34 property holds every `lstat` field of the source fixed across a
    capture and its chained reruns.
- Source-side verbs (`serve`, `snapshot`, `git-carry-estimate`,
  `git-export`, `copy`; `estate-capture` by `auto`, below) enter background
  CPU and IO priority before anything else (WP0(f)), inherited by every
  thread and
  child. The remote estimate probe's `bash` runs at default priority on the
  remote host (WP0(f) covers only the local verb; #188). `--priority=normal`
  is the explicit, recorded opt-out; every
  counters line and the bench header record the class.
- **Bounded starvation (M2 item 3, 2026-10-16; `priority.rs`).** On neo
  an `estate-capture` at background priority had a Git child in
  uninterruptible IO for 25 minutes while a normal-priority `serve` read
  the same disk: a throttled (Darwin) or idle-class (Linux) reader is
  served only when no other IO is pending, so a busy disk starves it
  without bound. By the operator's M2 instruction (`docs/slo.md`,
  amendment of 2026-10-16), `estate-capture` changes; every other
  source-side verb keeps the WP0(f) default above:
  - `estate-capture` defaults to `--priority=auto`: background when a
    terminal device was read or written within 300 s (access or
    modification time of `/dev/pts/*` and `/dev/tty*` on Linux,
    `/dev/ttys*` and `/dev/console` on Darwin; output counts, so the test
    errs toward background), normal otherwise; an unreadable `/dev` counts
    as a person present. The counters line says `priority_from=auto
    priority_auto=idle|interactive|unknown`. Activity in a graphical
    session alone is not seen (a residual; HID idle time is the
    follow-up). `--priority=auto` works for any verb.
  - A background `estate-capture` (or any background verb given
    `--escalate-after=SECONDS`) runs a watcher thread that samples, each
    second, its counters and, on Linux, its live descendants' read and
    write bytes (`/proc/<pid>/io`, so a long healthy Git child is progress).
    After the threshold (default 300 s for `estate-capture`; 0 turns it
    off) with no movement it escalates once: on Linux every thread of the
    process and of each live descendant (found through
    `/proc/<pid>/task/<tid>/children`, each checked to name its parent)
    leaves the idle class for the class its nice value implies (best
    effort, level 7 at nice 19); on Darwin the process disk policy goes
    back to `IOPOL_DEFAULT`, unverified there and likely helping only later
    children (a thread's `QoS` also bounds its IO tier; `auto` is Darwin's
    remedy). Nice stays 19. The watcher cannot tell idle from starved, so
    a capture blocked on nothing but a quiet source also escalates; the
    escalation is one-shot. Recorded as `priority_escalate_after=` on the
    counters line, and `priority_escalations`, `priority_escalated_ns`,
    `priority_escalated_threads`, `priority_escalation_failures`;
    `priority=` stays the class the process started at. No signal is sent
    (R-N11).
  - Not built: de-escalation when a person returns, and passing `--priority`
    or `--escalate-after` from `pull` to its remote `serve` (only
    `--durability` is forwarded).
- `serve` and `copy` refuse a private state root that overlaps the source
  (`SNAPSHOT_ROOTS_OVERLAP`) before any store is created.
- No clone of a source file (OI-1003-Q165).
  - A source file is read by `pread`, or by `read`/`splice` into a pipe.
    It is never the input of a remap: `FICLONE`, `FICLONERANGE`,
    `FIDEDUPERANGE`, `copy_file_range`, `clonefile` or `copyfile`.
  - `std::fs::copy`, and `std::io::copy` between two regular files, are
    remaps on Linux, because both use `copy_file_range`.
  - A clone writes the source inode, with its reflink flag and extent
    reference counts, and moves no `stat` field. Only `FIEMAP`'s
    `SHARED` flag shows it.
  - P84 SOURCE-NO-CLONE enforces the rule in two halves:
    - a registry scan refuses every remap call in non-test code, and pins
      each `fs::copy` and `io::copy` site with the reason its input is not a
      source file;
    - a kernel leg runs a real `copy`, `serve`, v1 Git capture and
      `SQLite` snapshot on a reflink file system and requires every source
      extent to stay unshared (`just p84-reflink` proves that it ran).
  - `FIEMAP` shows sharing that exists when it looks, not a clone later
    undone, and is trusted only on file systems that report it (xfs,
    btrfs, bcachefs, ocfs2); the static half rules the call out.
  - Three `std::fs::copy` calls are left, all on the destination side and
    none with a source file as input. Two copy bulkload's own staged
    capture bundle into its receipts. The third, estate converge's
    `publish_index` (OI-1003-Q144), copies the landed checkout's
    `.git/index`, which the operator's Git may have rewritten since
    landing, to converge's private merged index beside it.
  - `bulkload_agent::extents` is the read-only `FIEMAP` probe.
- SQLite provider snapshots are S2's stated exceptions: one lock and two
  writes. Every source is read through the backup API from a read-only,
  WAL-aware connection; none is read `immutable=1`. All three are bounded
  and counted:
  - **Lock (OI-1003-Q16).** The backup API's shared read lock on the source
    database, held only inside one step of the bounded read. Counted since
    #218 as `source_sqlite_lock_ns`: the source connection's whole life,
    from open to close, an upper bound on the lock (a WAL-mode connection
    holds `SHARED` on the main file until it closes, and a step's read mark
    in the `-shm`), with restarts in `source_sqlite_backup_restarts`.
  - **Write: the wal-index (OI-1003-Q36).** The source's `<db>-shm`:
    SQLite's own coordination file, which holds no user data. A WAL-aware
    read opens it read-write, maps it and takes `fcntl` locks on it. It
    creates or rebuilds the file when no live connection holds it, and
    beside a live writer it often leaves every byte as it was.
    - `source_wal_index_touched` counts all of these: 1 for each snapshot
      that leaves a `-shm` beside its source, whether or not the file
      changed. So 0 means that no snapshot opened a source wal-index.
  - **Write: the empty `-wal` (OI-1003-Q72, which extends Q36).** A
    WAL-mode database with no `-wal` is one that was checkpointed and
    closed, or one a writer has opened but not yet read. A WAL-aware open
    of it creates a `-wal` of zero bytes. The exception is that file only:
    allowed where no `-wal` existed, and empty.
    - `source_wal_created` counts it: 1 for each snapshot that leaves a
      `-wal` beside a source that had none. The file stays, so a second
      snapshot of the same source adds 0.
    - The read-only connection cannot append a frame, so the file it
      creates is empty. A writer that arrives during the read and creates
      the `-wal` itself is counted too: the counter is an upper bound and
      never an undercount.
  - Every counters line reports both counters, and S2 evidence
    (`s2_budget.py`) records both.
  - The main database file stays byte-identical with its timestamps
    unchanged. A `-wal` that existed before the read stays byte-identical.
    Nothing else beside the source is written (P75).
  - **Refused as root (OI-1003-Q76).** With an effective uid of 0, every
    provider verb that opens a database to read it refuses
    `SQLITE_SOURCE_AS_ROOT` before it opens anything: `snapshot`, `compose`,
    `compose-state`, `hydrate-state` and `apply-state-candidate`.
    - Why: run as root, SQLite re-applies the database's owner to the
      `-wal` and the `-shm` it opens (`fchown`; as any other user it skips
      the call). Measured on 2026-10-06: the same read-only, WAL-aware open
      plus backup leaves an existing `-wal`'s ctime unchanged as uid 1000
      and moves it as uid 0, with the `-wal`'s size, mtime and bytes
      unchanged. That is a source metadata write that Q16, Q36 and Q72 do
      not admit, and neither counter sees it.
    - The refusal is by uid, not by journal mode: a rollback-journal source
      is refused as well, and no extra read of the source is made.
    - A refused verb leaves the source directory as it was: no `-shm`, no
      `-wal`, no timestamp moved, both counters 0 (P75's root leg).
    - Run the verb as the database's owner.
    - The transfer's `--sqlite=snapshot` mode (#218, "SQLite snapshot
      seats" under Wire v6) reads through the same provider code under the
      same lock and the same two counted writes. As root its whole session
      is refused at `Open`; a database owned by another user than the
      reader is refused `SQLITE_SOURCE_NOT_OWNER` before `SQLite` opens it,
      since `SQLite` would create that database's `-shm` (and an empty
      `-wal`) owned by the reader, which the owner's own writer may then
      fail to open. The lock is now counted: `source_sqlite_lock_ns` is
      each source connection's life, for the verb and the transfer alike.

Git carry retains refs, objects, real stash commits including binaries and
untracked files, indexes and dirt, worktree administration and translated
paths. A bare repository (a mirror) is carried as ref custody: its refs, HEAD
and administration, with empty staged and worktree trees, since it has
neither an index nor a worktree (S4, #162).

- The capture marks itself bare in-band (`bare-repository-v1`, lifted into a
  shallow envelope's headers). Every verb that lays down a workspace, an
  index or a payload attachment refuses such a capture
  `GIT_BARE_CAPTURE_WORKSPACE` before writing anything; it applies only as
  `refs-imported`, planned without a workspace.
- A bare repository is a root only at its own git dir. Reached through a
  `.git` gitfile (the bare-plus-worktrees layout), it refuses
  `GIT_REPOSITORY_NOT_AT_PATH`, as the estimate probe does.
- A repository's own administration below its root, reached through a
  gitfile (`--separate-git-dir`), is never a seat.
- A non-bare repository with no index file (a `--no-checkout` clone or
  worktree) refuses `GIT_INVENTORY_INDEX_ABSENT`. Git reads the absent file
  as an unborn index that `git checkout` populates, and an empty index file
  does not, so no carried index restores it.
- A bare repository whose HEAD names a branch that does not exist (unborn:
  `init --bare`, or a mirror whose default branch is gone) is valid and
  captures as ref custody: its refs and its symbolic HEAD, with no
  `refs/carry-export/head` (S4, #162, #219). An empty bare repository, with
  no ref at all, captures the same way. An unborn HEAD in a non-bare
  repository still refuses `GIT_CHILD_FAILED`.
- Alternates (`objects/info/alternates`) are followed, decided at capture
  (S4, #219). The capture's private repository borrows the source's store,
  and the bundle packs every reachable object wherever it is stored, so a
  landing (a refs import or a checkout) is self-contained: it has no
  alternates file and `git fsck --full` is clean. Git reads alternates at
  most six levels down, and the private repository adds one, so a source
  can borrow through at most five stores. Git links each store once, by
  its real path, depth first in file order, and the capture's walk does the
  same: an entry naming the source's own store, a cycle, or a diamond
  reaching a store already linked costs no depth. An alternates file five
  stores down that names a store not yet linked, or a quoted alternates
  entry, refuses `GIT_SOURCE_ALTERNATES` with that file's path, before any
  export; the estimate probe walks and refuses the same way. An entry
  naming a store Git cannot open (a lender moved or deleted after
  `clone --shared`) is skipped as Git skips it; if the capture then misses
  an object (`GIT_CHILD_FAILED` of class `bad_object` or `other`), it
  refuses `GIT_SOURCE_ALTERNATES` naming that file. A `gc` or repack in a
  lender under the pass is drift custody, as one in the source is: the
  rewrite check reads every followed store's pack listing. A promisor pack
  in any followed store still refuses `GIT_SOURCE_PARTIAL_CLONE`.
- A capture's header does not list every carried ref (OI-1003-Q54, #178;
  `docs/plans/2026-10-05-v1-header.md`). The refs travel in a
  content-addressed ref table commit (`refs/carry-ref-table/v1`), with one
  tip ref per distinct object they name, so the header grows with the
  distinct objects, not the refs: 111 B each self-contained and 165 B each
  in a thin bundle (SHA-1; 159 B and 237 B on SHA-256). Import expands the
  table into exactly the refs the old format carried; a capture without a
  table (every capture written before) imports as it always did. Import is
  linear in the bundle: `bundle unbundle` and one connectivity walk, no
  ref-name matching.
- **No mixed-version apply.** A capture with a ref table is applied only by
  a build that reads one. The table ref sits outside `refs/carry-export/`
  on purpose: every earlier build refuses such a capture
  `GIT_INVENTORY_MALFORMED` before it writes a ref, never imports it as a
  wrong ref set.
- A bundle header, a ref table or a shallow envelope's manifest over its
  size bound refuses `GIT_INVENTORY_OVER_CAP`, never
  `GIT_INVENTORY_MALFORMED`. Writers measure the header before they write
  it, so no capture is recorded that a later reader refuses for size. A
  thin bundle (a chained link or a plan base's item) whose header would be
  over the cap is written self-contained instead of refused.
- Standalone restore configuration (#216, OI-1003-Q129, OI-1003-Q135).
  `git-restore` and estate-apply's standalone item (workspace equal to
  repository) keep the source's local `config` and `config.worktree`
  verbatim in `.git/carry-config/source-*`. They activate only safe
  declarative keys: identity, core booleans, pull and push modes, branch
  tracking, and the origin's default fetch refspec. Every other key is
  preserved-only. `configuration-activation.postcard` lists the activated
  and the preserved-only keys, never values.
  - The origin activates only when every `remote.origin.url` value is a
    plain `https://host/path` URL.
  - Any other origin is preserved-only, never refused: scp-style
    (`host:path`, `git@host:o/r`), `ssh://`, or a local path, absolute or
    relative (a clone of a local repository). Its URL and every other
    `remote.origin.*` key stay in the receipt.
  - The restored repository then has no `origin` remote: no URL and no
    fetch refspec, so `git remote` lists nothing. A fetch refspec alone
    would list a half-configured `origin`.
  - `branch.*.remote = origin` and `branch.*.pushremote = origin` are
    then preserved-only too. Git reads a named but unconfigured remote as
    a path relative to the working directory, and the restored worktree
    can carry an `origin` entry (a bare repository, a bundle) that fetch,
    pull and push would read or write. Without them, `git fetch` is a
    no-op, `pull` reports no tracking information and `push` no
    configured destination. `branch.*.merge` stays active; it names no
    remote by itself. Once an origin is added, the operator restores
    tracking with `git branch --set-upstream-to`.
  - So no restored repository points at a remote or local path the
    destination host did not choose.
  - `estate-apply --activate-origin PREFIX` (repeatable; #226 item 6,
    OI-1003-Q171/Q172) is the destination host's choice, made per run: a
    first standalone or bare landing activates an origin whose every
    value is scp-style (`host:path`, `user@host:path`) or `ssh://` and
    starts with a named prefix (`yoga:git/`,
    `git@github.com:Jesssullivan/`) at a path boundary (the prefix ends
    in `/` or `:`, or the value's next byte is one, so
    `git@github.com:Jesssullivan` never matches
    `git@github.com:Jesssullivan-other/x`), with `branch.*.remote` naming
    it. A local path never matches, and a prefix that is not scp-style or
    `ssh://`, or does not name its host whole (`ssh://yoga`), refuses
    `FIELD_DOMAIN_VIOLATION` before anything runs. Nothing is inferred
    from a host, an ssh config or a user name. The receipt counts
    `origin-activated-by-prefix`, and
    `carry-config/origin-activated-by-prefix` names every prefix that
    matched a value, one per line. An adopted repository keeps its own
    configuration: the flag never touches it.
  - An explicit mapping (`restore_bundle_configured`,
    `git-attach-standalone-payload`) is unchanged. `from` must be
    absolute; `to` must be an absolute path to an existing repository.
    Every captured origin value must equal `from` (else
    `GIT_AUTHORITY_CHANGED`), and a capture without an origin refuses
    `GIT_INVENTORY_MALFORMED`.
  - A standalone restore builds the whole checkout in a private
    `.bulkload-restore-*` stage beside the destination: the import, every
    configuration and authority decision, the worktree, index, captured
    modes and the configuration apply. It then publishes the checkout
    with one no-replace rename (`renameat2(RENAME_NOREPLACE)`,
    `renameatx_np(RENAME_EXCL)`); a file system without one gets a fresh
    0700 directory and a rename of each top-level entry (R-N119). Any
    refusal, a full disk or a killed run before that leaves no
    destination, so a rerun is not refused `GIT_DESTINATION_OCCUPIED`.
    A refused run removes its stage (best effort: a captured read-only
    directory can keep part of it); a killed run leaves the stage, which
    nothing reclaims automatically.
  - `git-restore-linked` activates no captured configuration, since a
    linked worktree shares its common repository's. Its exclude policy and
    intent-to-add custody refuse before `worktree add`. It is not staged,
    so a refusal while the worktree is materialized (a path collision, a
    malformed filesystem row, IO) or by the checks on a concurrent writer
    moving HEAD or the common exclude leaves the partial worktree.
  - The `git-attach-*` verbs refuse a mapping that is not absolute, or
    whose `to` is not a repository, before the receipt is written. A
    captured origin other than `from`, or none, is decided from the
    imported capture, after the receipt is written but before `.git` is
    published, leaving the payload untouched and the receipt as
    evidence; a rerun into the same receipt refuses
    `GIT_DESTINATION_OCCUPIED`. The one check after publication catches a
    concurrent writer.

Estate converge (2026-10-08; `docs/agent-notes/2026-10-08-estate-converge-design.md`).
Merged in #224 (2026-10-15) and ruled with three stated limits
(OI-1003-Q144, Q149; `docs/slo.md`, amendment 2026-10-15); its model is
`docs/formal/EstateConverge.tla`.

- **Native refs (#215).** A standalone first landing restores every
  carried ref natively inside its private stage (the repository bulkload
  creates): `refs/heads`, `refs/tags`, `refs/remotes`, `refs/notes` and any
  other namespace, and the stash exactly as `git stash list` shows it at
  the source: its order and each entry's reflog message, byte for byte
  (stash-order-v2, OI-1003-Q169: a capture with a stash carries
  `stash-order-v2`, each entry's commit and reflog message, and the
  message is part of each entry's identity in the capture key, in the
  ledger's stash row and in the stash table, so a message-only change at
  the source moves the key and converges as a rewrite). The format change
  moved the key of every checkout with a stash once, on purpose: its
  retained capture is re-captured at the next pass. A capture taken
  before Q169 (`stash-order-v1`) still applies, with each commit's
  subject as its message (`stash-messages-inferred`); a ref ledger or ref
  intent written before Q169 decodes through its own layout (a ledger's
  stash row then names its entries by commit, and is upgraded to the
  destination's entries, messages included, the first time it names them,
  `stash-ledger-upgraded`); a pending v1 intent is resolved against the
  destination's stash; anything else refuses `FRAME_CODEC`. Never
  natively: `refs/carry/**`, `refs/carry-*/**`, `refs/stash` (its own table),
  per-worktree refs, `refs/remotes/*/HEAD`, and any ref that is a symref at
  the destination. Provenance under `refs/carry/v1/<SOURCE>/<instance>/` is
  written as before. HEAD goes on the source's branch, or detached at the
  captured head with the branch recorded (`head-detached-ref-conflict`);
  no `carry/*` branch is created. A linked item attaches the same way; a
  repository bulkload did not create gets no native ref but the linked
  item's own HEAD branch (created by `worktree add -b` when absent).
- **Ledgers.** A ref ledger (`$GIT_COMMON_DIR/bulkload/v1/<SOURCE>/refs.landed`,
  with the repository marker, tombstones, the stash and the exclude, each
  with its capture instant) and a workspace ledger
  (`$GIT_DIR/bulkload/v1/<SOURCE>/workspace.landed`: capture, HEAD, index
  entries digest, the stat print of every seat on its file system) record
  what bulkload landed, next to it. Each is written atomically after the
  Git writes it names, which run with `core.fsync=reference,index,objects`;
  a Git older than 2.46 refuses `GIT_UNAVAILABLE`.
- **Refs converge** on every apply into a repository bulkload created: a
  ref follows the source only while it still holds the value bulkload set
  and the capture is newer (compare-and-swap); a ref changed or deleted at
  the destination, any worktree's checked-out branch, and a branch a rebase
  or bisect in progress holds (`rebase-merge/head-name`,
  `rebase-apply/head-name`, `BISECT_START`), is left alone and reported; a
  remote-tracking ref the source pruned is deleted, a local branch or tag
  is kept. Every worktree's HEAD is held by Git's own `HEAD.lock` while a
  ref transaction runs and the checked-out set is read under it, so no
  HEAD is written onto a branch the transaction then moves (2026-10-09;
  a `git switch` that read the branch before that lock is a stated limit
  below). A ref Git cannot create because a ref that stays
  sits above or below its name (`foo` kept, `foo/bar` new) stays at
  provenance (`ref-name-conflict`); a pruned name is deleted in its own
  transaction before its successor is created. A checkout's new HEAD
  branch moves only with HEAD, never in the ref step, and so does any
  branch a pending workspace intent of another item names; the branch a
  HEAD left is brought to the capture by a ref step after HEAD moved
  (left to the next apply's settle, never a refusal, when another writer
  holds it then).
  Every ref write syncs the directories its renames changed before a
  ledger names it.
- **Checkouts converge** when the capture changed: the landed checkout is
  proved unchanged against its ledger (HEAD, index entries, a standalone
  item's exclude, every landed seat by print or by content); a failed proof
  refuses `GIT_DESTINATION_MODIFIED` with nothing written. Otherwise only
  the delta is written, under a durable intent: refs, each seat by
  `RENAME_EXCHANGE` with the displaced file checked (put back if it is not
  the landed one), the index under `index.lock` by compare-and-swap (the
  ledger records the digest of the index installed, never a re-read), then
  the HEAD branch in its own transaction under every worktree's HEAD lock
  with this HEAD verified, then HEAD, and the intent records HEAD done (a
  settle then never moves HEAD again). HEAD is never attached to a branch
  another worktree has checked out or holds: read under every other
  worktree's HEAD lock, held until HEAD is written; a settle re-plans such
  a HEAD detached with the branch recorded (review 4). A crash leaves every component old
  or new; the next apply settles the intent forward before anything else:
  Git lock files a killed Git child left are removed when their content
  proves them bulkload's, and so are the HEAD locks of every pending
  intent before any ref transaction; an empty lock never is (any other
  refuses `GIT_CONVERGE_INTERRUPTED`), and an orphan `packed-refs.new`
  with no `packed-refs.lock` is removed; bulkload's own locks hold their
  marker from the moment they exist; a
  ref op on a branch checked out since is not redone, a HEAD branch line
  that can no longer apply is re-planned (detached with the branch
  recorded when it must), a file the source turned into a directory is
  found done, the stash continues only from the progress its intent
  records (an operator's drop is kept, and the ledger keeps the reflog
  bulkload last landed, so later applies keep it too), and probe files a
  crashed pass left are removed. Every refusal after the intent is
  `GIT_CONVERGE_INTERRUPTED`, and an unborn HEAD refuses
  `GIT_DESTINATION_MODIFIED`. An older capture refuses
  `GIT_CONVERGE_STALE_CAPTURE`, a changed
  shallow frontier, nested custody or rebuildable set
  `GIT_CONVERGE_SHAPE_CHANGED`, a checkout this item landed and an
  operator removed `GIT_DESTINATION_MODIFIED`, and so does a landed
  directory the source turned into a file or symlink while it holds an
  entry bulkload did not land (checked before the intent). Configuration
  is applied once at the first landing and only reported after.
- **Adopt: landing into the live tree (#226, OI-1003-Q171/Q172,
  2026-10-15; `docs/agent-notes/2026-10-15-estate-adopt-design.md`).**
  Without `--adopt` a destination bulkload never landed still refuses
  `GIT_DESTINATION_OCCUPIED`, and a repository without the marker gets
  provenance only. With `estate-apply --adopt`, explicit per run, an
  existing checkout becomes bulkload's by proof, with nothing moved:
  - The checkout must be the item's to adopt, before any proof: its path
    and the plan's repository spelled canonically (never through a
    symlink: a ledger is keyed by the item, the item by its spelled
    paths), its repository the plan's, a standalone item's checkout a
    main worktree, and its git directory naming it back (a linked
    worktree moved without `git worktree repair`, or a copy sharing
    another's admin directory, is not). Otherwise
    `GIT_DESTINATION_OCCUPIED` with nothing written; `estate-diff` says
    `symlinked-path` or `occupied-shape` with the reason.
  - Its repository must be the capture's: it holds a carried tip or a
    root commit of one, or it has no native ref. An unrelated repository
    stays `GIT_DESTINATION_OCCUPIED` with nothing written.
  - The proof is read-only and runs before any lock or write. The capture
    is read from a scratch repository beside its staged bundle, borrowing
    the destination's objects. HEAD (branch and commit), the index entries
    (with intent-to-add), a standalone item's exclude, and every carried
    seat's kind, mode and bytes (each print held still while read) must
    equal the capture's. Extra destination seats, the omitted rebuildable
    roots and planned nests are left alone.
  - Any difference refuses `GIT_DESTINATION_DIVERGED` with nothing
    written: never claimed, never overwritten. The refusal is pending
    review; accept, re-carry or abandon disposes it, and `--policy` makes
    a standing accept.
  - On equality, under the destination lock, the checkout must still have
    no workspace ledger (another apply may have written one), and the
    proved prints are checked again. Then come the import, an `adopted` record and the
    repository marker with the HEAD branch's R2 row, and the native ref
    step:
    - R1 creates carried refs the destination lacks;
    - R2 adopts equal ones;
    - R3 keeps differing ones;
    - R10 leaves destination-only ones;
    - checked-out branches are skipped;
    - the stash goes by T0 (an equal reflog adopted), T1 or T2.

    Then the workspace ledger from the proof's prints.
  - No seat, index, HEAD or configuration is written, and no ref is
    deleted. A crash leaves the checkout as it was and the item
    `journal-missing`. Before the workspace ledger, the rerun with
    `--adopt` finishes; after it, any rerun does.
  - A ledger that names another plan's item (a landing moved to its live
    name) is re-keyed under `--adopt` after its normal proof, and only
    when the checkout is spelled canonically, the ledger's item is not one
    of this plan's, and the capture is the landed tree's (the same
    capture, or its HEAD is the ledger's HEAD or descends from it).
    Otherwise `GIT_DESTINATION_OCCUPIED`, nothing rewritten: a plan typo
    or two items aliasing one tree never hand a landing to another
    source.
  - A refs-only item's related or empty repository is adopted the same
    way, and an unborn HEAD of a bare one is put on the captured branch.
    An unrelated one keeps the provenance import and refuses
    `GIT_DESTINATION_OCCUPIED` (pending review, never counted applied).
  - `estate-diff` reports all of this beforehand, read-only.
  - `estate-diff` imports a repository's plan base once (M2r2 item 3,
    the 10-16 in-place finding: it indexed the whole base again for every
    worktree item, O(items x repository), 11 rows an hour on a large
    repository under load). A base is staged once per run and imported
    once per destination object store into a scratch repository in its
    stage; every item's scratch borrows that import and imports only its
    own capture. A borrower takes the import's objects as one more
    alternate after the destination's (Git reads that repository's refs
    as negative tips, `--alternate-refs`, so a borrower's connectivity
    checks stop at the base) and the refs the import made outside its own
    provenance instance (the `union/v1` carry refs, which the borrower's
    import compares its own against; usually none). The instance's own
    refs are not copied: every scratch read is scoped to the item's
    instance. A custody envelope (whose import writes a shallow frontier;
    `estate-capture` does not make one as a plan base) and a stage whose
    path no alternates line can name (a newline or a leading quote) are
    imported per scratch, as before; a path that is not UTF-8 is lent, its
    line written as bytes. A refusal while staging or importing a base is
    not kept: the next item naming it tries again, so a transient failure
    refuses one row, as before. The stage, with every import in it, is
    removed after the last item naming its base, so its disk is one copy
    of the base plus one import per destination store from the first such
    item to the last. `estate-reseat --check` reads Git state through
    `estate-diff` and shares the same way. Counted on the counters line:
    `git_scratch_base_imports` (whole-base imports into a scratch
    repository, by `estate-diff` and by the apply's adopt and
    first-landing scratches) and `git_scratch_base_borrowed` (scratches
    that borrowed a shared import). `BULKLOAD_DIFF_SHARED_IMPORT=0`
    (exactly `0`; unset or any other value shares) is the A/B arm, an
    import per scratch as before; its rows are byte-identical (P130-P133).
    `estate-apply` is unchanged: each item still stages and imports its
    own base. Not built: a chained (re-captured) item is still flattened
    per item (`chain::flatten` fetches its bases and links into a private
    repository and writes the whole history as one bundle, which the
    scratch then indexes), a whole-repository cost per chained item in
    every verb, counted `git_chain_flattens`.
  - Not built: `--protect` (land the source's state beside a destination
    adopt refuses).
- **Hand-off block (TIN-6569, OI-1003-Q183, 2026-10-16;
  `docs/agent-notes/2026-10-16-estate-handoff-design.md`).**
  `estate-handoff PLAN CORPUS PRIVATE_STATE SOURCE` writes a dated,
  delimited block at byte 0 of `AGENTS.md` and `CLAUDE.md` in each landed
  workspace, of an existing `AGENTS.override.md` (Codex reads it instead of
  `AGENTS.md`), and of an existing `README.md` only with `--readme` (neither
  is ever created). The block names this landing, its source path on SOURCE and a
  live sibling (`--sibling LANDED_PREFIX=LIVE_PREFIX`) by absolute path;
  the capture, landing and build; each worktree of the repository at
  capture and now; `estate-diff`'s ref classes against the landed capture,
  destination-only worktrees and recorded refusals; and the instruction to
  converge and consolidate with the user before any other work, with the
  exact `estate-diff`, `--check` and `--clear` commands.
  - **Owned by a record, never by inference.** The block is recorded in
    `handoff.landed` beside `workspace.landed` (its length and blake3),
    with `handoff.intent` durable before any target is written. Each
    target goes through a temporary, fsync, `RENAME_EXCHANGE` and a
    check of the displaced file against the print read (a created file
    by a no-replace rename); a writer that raced it is put back and the
    run refuses. A rerun of either verb settles a crash forward: a
    pending render's record is made to name exactly the blocks in place,
    and a pending clear is finished, never undone (the H step included).
  - **Owned by identity.** The BEGIN line names the landing it was
    rendered for (`for=`, a digest of the destination host, the
    workspace path and SOURCE). A block that names another landing came
    with the source's bytes (a re-carried, handed-off tree), and so did a
    block of this landing's identity that the target's landed blob itself
    starts with (a round trip: carried back to the source and landed
    again at the same path, after the record is gone or while it stands,
    and whatever the user has written below it since; re-review 3 of
    2026-10-16): it is content (OI-1003-Q185, nothing is dropped from a
    carry), kept byte for byte below this landing's block, named in it as
    carried, and cleared only by the opt-in `--clear --clear-carried`
    (below). Any other block of this landing's identity that its record
    does not name (edited inside, cut, or its record lost) is never
    presented as carried: the hand-off refuses `HANDOFF_BLOCK_MODIFIED`. A block a
    person removed by hand (retired in the record with its digest) and
    then put back byte for byte, by an editor's undo or a `git stash
    pop`, is this landing's again: converge accepts it, the next render
    takes it back into the record (`restored`) and renders it current,
    and `--clear` removes it.
  - **Converge keeps an intact block.** A landed seat holds landed
    content when its bytes are the landed blob, or a recorded block
    followed by it (`holds`, through `landed_sides`). An edit inside the
    block or below it is the user's: converge refuses
    `GIT_DESTINATION_MODIFIED` as for any edit. The record's print of a
    target lets converge skip the read only when it is out of the racy
    window and its `base` is the landed blob; every write of that print
    (a rerun over a kept block, a settle, a re-dating) checks the bytes
    under the block against the landed blob again first, so it never
    vouches for an edit below the block. A file the hand-off created is
    lifted (removed, when it is exactly its block) before the source adds
    the same path; it stays `created` when a later render replaces its
    block, so the lift and `--clear` still remove it.
  - **The H step.** After the last level of an `estate-apply`, each
    workspace of a repository the run changed that holds a record (or a
    pending intent) has its block re-rendered from the estate as it now
    stands, under the record's options; a refused item's own workspace is
    left alone. Its result is a line of its own (`item=<id>
    handoff=written|unchanged|<CODE>`), never an item's outcome.
  - **Removal** is `estate-handoff --clear`, a recorded step that restores
    the bytes and `git status` exactly. Carried blocks stay hands-off by
    default; `--clear --clear-carried` (OI-1003-Q190, only with `--clear`)
    also removes the carried blocks at the head of a target's landed
    content, under this landing's block or alone, by the same intent,
    temporary, exchange with the displaced file checked, and a record,
    `handoff.stripped`, which outlives `handoff.landed`: converge and
    `estate-diff` accept the landed blob without those bytes (alone or
    under a later block) while that blob stands, and the landing's objects
    still hold them. Above content that is not the landed blob (a user's
    edit) a carried block is kept (`carried-kept`). A crashed clear that
    the next run finishes records each removal whose target no longer
    starts with the removed blocks, a user's later edit included. A
    converge that lands a new blob of the target lands what the source
    has, carried block included.
  - **Commit detection** (`--check`, read-only; OI-1003-Q191): a token
    line is one holding the `BULKLOAD-HANDOFF-DO-NOT-COMMIT` token that the
    source's history (the commits the carried capture, `refs/carry/`,
    reaches) never added; every BEGIN line names its block's id and
    landing, so a block of this landing's always has one. `staged` when a
    target's index blob holds a token line that its HEAD blob lacks;
    `committed` when a commit that the carried capture does not reach, in
    the history of any branch, tag, remote-tracking ref, stash entry or a
    detached HEAD, adds one (a block committed and removed again is still
    published by a push; the block says to stash nothing that holds it).
    Both exit `HANDOFF_MARKER_COMMITTED`. A block the source committed,
    also when cherry-picked or rebased here, is reported once per
    workspace as information (`"source_history":true`), never as
    committed. Every name
    a hand-off may write (`AGENTS.md`, `CLAUDE.md`, `AGENTS.override.md`,
    `README.md`) is read, whether or not it is a target now. No hook is
    installed.
  - **Open rows and `ready`.** Diverged and source-ahead refs, a live
    sibling whose HEAD the landing does not hold (on a branch or in its
    HEAD's history; a sibling strictly ahead is open), destination-only
    worktrees whose HEAD no other
    branch holds, recorded refusals, and a newer capture not applied.
    A destination commit, a dest-ahead ref or a seat edited since landing
    is consolidation, not an open row, so `ready` is reachable by
    consolidating. `ready` also needs a clean working tree in the live
    sibling and in every destination-only worktree (OI-1003-Q189): a
    read-only `git status` (`--no-optional-locks`, untracked files
    counted, ignored ones not) of each, on every run, with every filter
    driver the checkout configures switched off (no clean filter or
    long-running process such as git-lfs runs there, and a required one
    cannot fail the read; a filtered file whose index stat is stale reads
    as modified until the checkout's own `git status` refreshes it); one
    that lists an entry or cannot be read is an open row.
  - **Options persist.** A rerun, the H step and the `--check` a block
    prints keep the `--sibling` and `--readme` the record was rendered
    with unless they are given again.
  - **Always written (OI-1003-Q188).** The privacy guard is removed ("an
    erroneous overreach; such a block is irrelevant to bulkload"): the
    block is written into every landed workspace whatever its remotes,
    and no remote URL is read for it, rendered or printed. Absolute paths
    and host names are written only into the targets, `.git/bulkload/`
    and PRIVATE_STATE, and printed on stdout (the rows, and `--dry-run`'s
    blocks); commit detection reports a block committed here.
  - **Typed refusals,** each with nothing written: `HANDOFF_NOT_LANDED`,
    `HANDOFF_TARGET_UNSUPPORTED` (a symlink other
    than `CLAUDE.md -> AGENTS.md` in the same worktree, which is covered by
    `AGENTS.md`'s block, or any other non-regular target),
    `HANDOFF_BLOCK_MODIFIED` (a block of this landing's identity that is
    not the record's and that the landed blob does not start with: edited
    inside, cut, or its record lost), and `HANDOFF_MARKER_COMMITTED`.
  - **S3.** Rendering is a pure function of metadata and an `as_of`
    instant kept while its inputs digest holds. An unchanged rerun reads 0
    destination bytes and writes nothing once a rerun has re-dated a
    record whose prints were racy, as Git re-dates a racily clean index.
  - Not built: the estate-wide `INDEX.md` under PRIVATE_STATE, a
    `handoff` field in `estate-diff` rows and the opt-in `--commit-guard`
    hook. A capture never strips a block (OI-1003-Q185: nothing is
    excluded from a carry); the next landing's identity check handles a
    carried one.
- One plan lands a main and its linked worktrees: an item whose repository
  is a planned standalone workspace applies a level after it, and a
  standalone item's plan base is imported into its stage. A first linked
  landing writes a landing intent before `git worktree add`; a crash before
  its ledger leaves a worktree the rerun finishes forward (only absent
  seats are written: each entry is laid down under a temporary name and
  renamed into place, so an operator's file at an entry's name, or any
  index the landing did not write, refuses `GIT_DESTINATION_MODIFIED`),
  never `GIT_DESTINATION_OCCUPIED` for good. Every refusal a first linked
  landing can decide without a write (a bare capture, an occupied path, a
  missing parent, the capture's metadata, a common exclude that is neither
  the capture's nor a newer one the main item set
  (`GIT_IGNORE_POLICY_CONFLICT`), intent-to-add custody) is decided from a
  scratch repository before its first write to the destination
  repository, so it leaves that repository's refs and objects as they
  were; the import then runs under the destination lock. `estate-diff`
  runs the same checks and says `ignore-policy-conflict` or
  `parent-missing` (dogfood of 2026-10-16, F1 and F2).
- **Absent parents are made (M2 finding 1, 2026-10-16).** A first
  landing (standalone, linked or bare) whose parent does not exist no
  longer refuses: `estate-apply` makes each absent level, mode 0755
  exactly, just before the landing's first write (after the path checks
  of a standalone or bare landing, whose private stage then decides the
  capture's own refusals, and after every write-free check of a linked
  one), syncs it with its parent, and only then appends it to the item's
  `{item}-{blake3(SOURCE)}.parents` record in STATE (the levels made
  before a refusal deeper down included) and counts it in the receipt
  (`parent-created=<n>`). Levels are made only inside a real directory
  this user can write: the nearest existing ancestor is opened
  `O_NOFOLLOW`, and each level is made under a `.bulkload-parent-*`
  temporary, given its mode, synced and renamed into place without
  replacement, then opened `O_NOFOLLOW` again. A symlink there (or one
  swapped in meanwhile), a non-directory, an ancestor this user cannot
  write and search, or a `..` in the planned path keeps
  `GIT_DESTINATION_PARENT_MISSING`, and nothing is made where a link
  points; symlinks above that ancestor are the operator's layout and
  resolve as an existing parent does. A refs-only item is ordered by its
  repository's path, so a bare landing below a planned workspace lands
  after it and never makes the workspace's directory. A crash after the
  levels are made and before the record leaves empty 0755 directories no
  record names (`estate.apply.parents_made`), or an empty
  `.bulkload-parent-*` temporary; the rerun lands into them. The CLI
  restore verbs keep the refusal. `estate-diff` says `landing` (or
  `bare-landing`) with the component `parents=<n>` (the levels absent
  now; items sharing them each count them), and `parent-missing` only
  where the policy refuses.
- **`--exclude-union` is a recorded write (M2 round 2, item 2,
  2026-10-17).** The in-place run refused 250 new linked worktrees
  `GIT_IGNORE_POLICY_CONFLICT` in repositories whose captured common
  exclude was a pure superset of the destination's. Without the flag
  nothing changes. With `estate-apply --exclude-union`, a first linked
  landing whose common `info/exclude` is neither the capture's nor a newer
  one the main item set appends the captured lines it lacks after its last
  line, each once, in captured order, and lands. Lines compare as Git
  reads them (a leading UTF-8 byte order mark skipped, one trailing CR
  dropped, unescaped trailing spaces trimmed, a line empty after that
  skipped); an appended line is the captured line's bytes. No existing
  byte is removed, changed or reordered (the old file is a byte prefix of
  the new one); when nothing is missing nothing is written, so a rerun,
  and every later linked item of the same repository, writes nothing.
  Where a line would be appended, the refusal is kept for: a negation
  (`!pattern`; `\!` is a literal) on either side, since Git decides by
  the last matching pattern and an append would then not be a union; an
  exclude a bulkload ledger at the repository records (any label's main
  workspace ledger, or a ref ledger's exclude row: that main item's
  converge owns it, and an append would fail its proof); an `info` that is
  not a real directory, or an exclude that is not a regular file, has more
  than one link or another owner (nothing is written through a link); and
  an exclude that changed between the decision and the write. **The write
  is repository-wide:** every worktree of the repository reads
  `info/exclude`, so the append changes what the main checkout and every
  other linked worktree ignore (an untracked file a new pattern matches
  leaves `git status`, `git add -A` and `git stash -u` skip it, and a
  `core.excludesFile` negation no longer re-includes it). The decision is
  made by the write-free checks, then again under a repository-wide lock
  (`$GIT_COMMON_DIR/bulkload/exclude.lock`, shared by every SOURCE label;
  Git itself takes none) that also covers settling and the append. The
  append is the plan's last write before `git worktree add`; that and the
  materialization after it can still refuse. The new bytes go to a
  temporary in `info` (given the old file's mode with `fchmod`, 0644 for a
  new file, synced); the append is recorded write-ahead in STATE
  (`{item}-{blake3(SOURCE)}.exclude-union`: the path, the digests before
  and after, the lines, the temporary's name and inode) as an intent, the
  temporary is renamed over the exclude, `info` is synced, and the entry is
  marked `appended`. A crash between (`estate.apply.exclude_intent`,
  `estate.apply.exclude_appended`) leaves an intent the next run under the
  flag settles by identity, never by content: `appended` when the exclude
  is the recorded inode, `not-written` when the temporary is still there
  (it is removed), otherwise left an intent (the file was replaced since).
  A crash before the intent can leave a `.bulkload-exclude-*` temporary no
  record names; Git never reads it. The receipt of the landing that
  finishes counts `exclude-appended=<n>`: every appended line of the
  item's record no finished landing has reported, so an append a later
  refusal or a crash kept from its receipt is counted by the rerun that
  lands. Where the destination holds lines the capture lacks, the landed
  worktree's ignore set is the union of both: a captured untracked file
  can be ignored there (its bytes are carried all the same).
  `estate-diff` still says `ignore-policy-conflict` and, where the flag
  would land the item, the component `exclude-union=<n>` (items of one
  repository each count the lines; the apply appends them once), or
  `exclude-union-refused=<CODE>` where it would not.
- **`--beside` lands a diverged checkout beside it (M2 round 2 item c,
  2026-10-17).** The in-place i02 batch ended 10 workspace items
  `GIT_DESTINATION_DIVERGED`: the live checkout exists and differs from the
  capture, so `--adopt` cannot prove it. Each was carried by hand as a
  refs-only item plus a standalone `<path>.neo-20261016`. With
  `estate-apply --adopt --beside SUFFIX` (the flag requires `--adopt`), a
  workspace item whose existing checkout the adopt proof refuses
  `GIT_DESTINATION_DIVERGED` (nothing written) leaves that checkout's
  working tree, index, HEAD and native refs exactly as they are and lands
  the capture at the workspace path with `SUFFIX` appended, the meaning
  `estate-carry-files --beside SUFFIX` gives it. No other refusal leads
  there: a first landing that refused after writing (an authority change)
  leaves its partial checkout for review, never a second copy. The ref
  namespace `ns` is `SUFFIX` without its leading `.`, one plain ref
  component (letters, digits, `.`, `_`, `-`; no leading `.` or `-`, no
  `..`, no trailing `.` or `.lock`); labels using the same suffix share it.
  A ref named `refs/<ns>` or `refs/<ns>-stash` refuses
  `GIT_DESTINATION_OCCUPIED` before anything lands. An absent beside path
  gets a first linked landing into the planned repository (its objects and
  provenance refs imported, its `worktrees/` administration and ledgers
  written, as any linked landing writes them), detached at the captured
  head with the branch recorded (`head-detached-beside`): no branch of the
  live repository is created or moved, and its native refs are never
  stepped. A beside landing never appends to the shared `info/exclude`
  (that would change what the live checkout ignores): under
  `--exclude-union` it lands only when the live exclude already holds
  every captured line, and otherwise refuses `GIT_IGNORE_POLICY_CONFLICT`.
  A landing a crash cut short is finished forward, then converged when it
  held an older capture. An existing beside path is taken as the next
  bullet says (gap C12). Then every captured ref
  `refs/<name>` is wanted as `refs/<ns>/<name>` and stash entry `n`
  (newest first) as `refs/<ns>-stash/<n>`: a name already at the same value
  is present; one this item created and that still holds the value it
  created is moved to the new value; one this item created that is gone is
  never created again; one at any other value is kept, never moved or
  deleted; one
  that would collide with an existing ref as a directory or a file refuses
  `GIT_DESTINATION_OCCUPIED` before any ref is written. The rest are
  recorded write-ahead as pending in STATE
  (`{item}-{blake3(SOURCE)}.beside`: path, suffix, the capture from the
  beside checkout's own ledger, instance, refs pending, created and kept),
  created in one compare-and-swap transaction through the converge's ref
  writer (synced), then recorded as created; a rerun after a crash moves
  each pending ref it finds at its value to created. The receipt counts
  how the beside path was taken (`beside-landed`, `beside-present`,
  `beside-converged`, `beside-rekeyed` or `beside-adopted`),
  `beside-linked` or `beside-standalone`, `beside-ref-created`,
  `beside-ref-present`, `beside-ref-moved`, `beside-ref-retired` and
  `beside-ref-kept`. With every wanted ref in, the outcome and journal are
  `workspace-beside` (a rerun `previous-workspace-beside-not-revalidated`)
  and closure v5 classifies the item `applied-beside`; a kept ref refuses
  the item `GIT_DESTINATION_OCCUPIED` after the record, so it stays pending
  review. A crash after the worktree is added (`estate.converge.linked_added`)
  or after the refs (`estate.apply.beside_refs`) leaves the live checkout
  untouched and the item `journal-missing`; the rerun finishes it.
  `estate-diff --beside SUFFIX` adds `beside=<landing|ignore-policy-conflict|
  landed|converge|rekey|existing|foreign|occupied>:<path>` to a `diverged`
  row (what the apply tries; no proof is run for it). A beside checkout
  bulkload landed first is a worktree of the live repository: `git
  worktree list` there shows it.
- **An occupied non-repo directory (2026-10-19, the td01 batch).** Six
  linked worktrees of tinyland.dev exist on sting as directories with
  their files and no `.git` at all (the 2026-08-29 ceremony stripped them);
  `estate-apply` refused each `GIT_DESTINATION_OCCUPIED`. Under `--adopt`, a
  linked worktree item whose planned path is such a directory is proved
  read-only first (`converge::check_nonrepo`): spelled canonically, no
  registration of the repository naming it (unless a landing intent of
  this item does: its own registration a crash cut short), the first linked
  landing's checks, and every seat the capture carries, tracked, untracked
  and ignored, held there with its kind, mode and bytes (`adopt::prove_seats`);
  files the capture does not carry are left alone. Equal, it is adopted by
  registering it again, as `git worktree add` would and with nothing in it
  written (`git_carry::adopt_nonrepo_linked`): under the destination lock
  the capture is imported, the native ref step runs and HEAD is planned as
  for a first linked landing; the directory is checked again; the landing
  intent; a branch `worktree add -b` would create in a foreign repository;
  the administration directory `worktrees/<id>` (`commondir`, `HEAD`, then
  `gitdir`; `<id>` the directory's name, numbered when taken;
  `estate.apply.nonrepo_registered`); the `.git` gitfile, created new
  (`estate.converge.linked_added`); the index from the capture; and the
  workspace ledger from the proof's prints once they still hold. A crash
  is finished forward: before the gitfile by the same adoption reusing its
  administration directory, after it by `recover_linked_landing`, which
  sets no mode a seat already holds. The receipt counts `adopted-nonrepo`
  and closure says `applied`; later passes converge it as any landing.
  One that differs, or a standalone item's (no registration to re-create,
  no repository to land beside into), refuses
  `GIT_DESTINATION_NONREPO_DIFFERS` with nothing written; under `--beside`
  a linked item's capture lands beside it exactly as a diverged checkout's
  does (refs in, the directory untouched), and the beside landing records
  the code that sent it there (`beside-reason=<CODE>` in the receipt,
  `{item}-{blake3(SOURCE)}.beside-reason` in STATE, `beside_reason` on
  closure's `applied-beside` row; `GIT_DESTINATION_DIVERGED` for a diverged
  checkout). `estate-diff` reports `nonrepo-adoptable`, `nonrepo-duplicate`
  or `nonrepo-differs` with the differing seats and the beside component,
  and `estate-move`'s plan previews a linked item's non-repo directory as
  `adopt` (`nonrepo-adopt-or-beside`).
  Review of #286 (2026-10-19): an equal directory is never registered as a
  second copy of the same worktree: when the captured branch is checked
  out by another worktree of the repository (the td01 shape: the item's
  beside landing from an earlier pass holds it) or the item's beside
  landing exists at `<path>SUFFIX`, it refuses
  `GIT_DESTINATION_NONREPO_DUPLICATE`, nothing written, pending review
  (retiring a beside proven equal, as #289 retires one, is the follow-up).
  Its verification (2026-10-19): the same refusal, with or without
  `--beside` and whatever the copy's HEAD (the tin2835 shape is a detached
  `<path>.neo-YYYYMMDD`), when any registered worktree of the repository is
  bulkload's landing of this item (its workspace ledger names the item) or
  a `<path>.<suffix>` sibling bulkload landed under any source.
  The adoption's landing intent carries a marker: a crash past the gitfile
  is finished by proving every seat again (content and mode, read from the
  capture imported into the repository) and writing none, a missing or
  changed seat refusing `GIT_DESTINATION_MODIFIED`, and the ledger is that
  proof's. The administration directory is held `locked` "initializing"
  until the gitfile is written, as `git worktree add` holds it; a relative
  `gitdir` names the directory as an absolute one does; the re-check runs
  under the lock before the import, the native ref step and any exclude
  append; and a later pass that does not adopt it (a refusal, or a pass
  without `--adopt`) removes the registration a crash left before the
  gitfile, as does a failure of the adopting pass itself before its
  gitfile names the registration (a partial gitfile it created is removed
  first): nothing stays `locked` "initializing" for `git worktree prune`
  or `git branch -f` to trip over. The removal is ownership-checked: only
  an administration directory still locked "initializing", holding only
  what the adoption writes, no `.git` naming it, and this item's intent.
- **Beside landings converge, and a beside path is adopted by proof (gap
  C12, plan of record rev 2; operator ruling A-115, 2026-10-18,
  "Landings placed beside refuse every later pass once neo changes" →
  "Yes, converge them").** A diverged item lands beside on every pass,
  including when the beside directory exists. `git_carry::beside::land`
  reads the beside path before anything is written and takes it one way:
  absent, the first linked landing (one this item's STATE record says it
  landed there and that is gone was retired by the operator's agent: never
  laid down again, `GIT_DESTINATION_MODIFIED`, as a removed live checkout
  is); this item's own landing, present for this capture and converged for
  an older one by the converge that converges a live landing (proved
  against its ledger first, read-only, its recorded hand-off blocks its
  own: an edit, a commit or a switch there since refuses
  `GIT_DESTINATION_MODIFIED` with nothing written); a landing of another
  item (the in-place lane's hand plans landed `<path>.neo-20261016` as
  standalone items of their own, which is what refused the five
  GloriousFlywheel beside items on 2026-10-18) re-keyed by A11b after its
  proof and converged; a standalone checkout no ledger names adopted by
  the A11a proof in its own repository; anything else
  `GIT_BESIDE_FOREIGN`, which names the beside path, with nothing there
  written (a `.git` gitfile naming a gitdir that is gone, or another
  repository's, included: it is resolved read-only first, never a bare
  `IO`). Namespace stash entries are decided by commit, never by index: a
  stash commit this item put under `refs/<ns>-stash/` and the operator
  removed is left out of later passes, and the rest renumbered (a beside
  landing) or new ones put at a free index (refs-in), so a removed commit
  never comes back under another index. A linked beside checkout converges detached at the captured
  head (`head-detached-beside`) and never steps the live repository's
  native refs (`Place::beside`); a standalone one is its own repository
  and converges as any standalone landing does. The adoption proof of a
  beside checkout compares HEAD, the index entries, its `info/exclude`,
  its stash (each entry's commit, newest first) and every carried seat's
  kind, mode and bytes; the one difference it declares is a bulkload
  hand-off block at byte 0 of `AGENTS.md` or `CLAUDE.md` above the captured
  bytes, sealed as estate-handoff renders it (its `id=` is the blake3 of the
  block with the id blanked, so an edit inside the block or a block a
  person wrote is a difference), counted `beside-handoff-block`. Such a
  block is bulkload's own output: a later converge that replaces the seat
  drops it, as it drops a recorded block, and estate-handoff renders it
  again. A standalone checkout is adopted as it stands,
  never converted to a linked worktree: converting would rewrite its
  `.git` and move its objects, branches and stash into the live
  repository, and the proof needs neither, since the live repository gets
  the capture's provenance and the namespace refs either way. When the
  beside landing refuses, `--refs-in` still imports the item's branches,
  tags and stash entries (`refs/<ns>-stash/<n>`) create-only, and closure
  prints them on the refused row (`refs_in`: namespace, created, kept),
  with a `GIT_BESIDE_FOREIGN` row's `refusal_detail` naming the path.
- **Limits (2026-10-09).** A HEAD detached because its branch was checked
  out elsewhere re-attaches, and a ref the operator put back to its landed
  value converges, only on the next capture: the journal no-op (A4) reads
  nothing. An empty Git lock (a delete's, `packed-refs.lock`, the
  `HEAD.lock` Git takes for the HEAD reflog), or one with content that is
  not bulkload's, is not provably bulkload's: a killed child's refuses
  typed until the operator removes it, as Git instructs, and a live
  writer's is never touched (review 4). `HEAD.lock` serialises only the
  final HEAD write of a `git switch`: a transaction that moves a branch
  after a switch read it leaves that worktree on the branch over its old
  commit's files and index.

Import preserves divergence and leaves active HEADs, indexes and working
bytes untouched, with one ruled exception (OI-1003-Q144): a checkout
bulkload landed and nobody changed since is converged to a newer capture
of the same source, and, under `--adopt`, an existing checkout that proves
equal to its capture becomes bulkload's with nothing written to it (#226).
estate-apply refuses a planned item whose capture
the corpus does not hold (its capture refused, or none ran) with
`CAPTURE_ABSENT`, before it reads anything else; `SEALED_OBJECT_MISSING`
names only a recorded capture whose bundle, link or base the corpus lost
(S4, #219). A refs import into a destination repository that does not
exist, or into a directory Git finds no repository in (an empty directory,
an unfinished `git init`), refuses `GIT_REPOSITORY_NOT_AT_PATH` as that
item's own receipt, never `GIT_INVENTORY_MALFORMED` or
`GIT_DESTINATION_OCCUPIED`, and never aborts the apply's other items (S4,
#183). The exception is a capture of a bare repository into a path that
does not exist (#226 item 4): bulkload lands the bare repository itself.
It is built in a private `.bulkload-restore-*` stage beside the path: `init
--bare` in the capture's object format, the plan base and the capture
imported, every native ref and the stash through the ref step with the
repository marker, the captured symbolic HEAD, and the configuration
under the standalone rule. It is published by the standalone landing's
no-replace rename and borrows nothing (`alternates` at the source are
packed into the bundle). A standalone item on a plan base is never preseeded into an
existing destination: its plan base is staged into its own private stage
(estate converge, above), and an occupied standalone destination without
ledgers still refuses `GIT_DESTINATION_OCCUPIED`. Account credentials
carry privately; platform stores
may require a format transcode. Destination machine keys and Home Manager
links are preserved. Credential contents are never printed.

SQLite is captured with backup-API snapshots, never raw live WAL/SHM copies.
Composition preserves unique rows on both sides; schema or row conflicts keep
their snapshots for explicit resolution.

## Drift

A capture pass tolerates refs and worktree seats moving under it (R25), so
writers never pause for a capture. What moved is recorded, never absorbed:

- Ref and seat drift is tolerated, and since #38 (2026-10-15, OI-1003-Q11;
  S5 under OI-1003-Q152: no freeze window, the run closes by converging
  deltas while the source stays live) so are HEAD, the index and nested
  worktrees' HEADs: an agent committing, switching or staging in the
  captured checkout, or in a worktree nested in it, under the pass. Each is
  a drift row, `HeadMoved` named `HEAD` (the commit HEAD resolves to or the
  branch it names), `IndexRewritten` named `index`, `NestedWorktreeMoved`
  named by the nested worktree's path. The bundle carries the HEAD and the
  index bytes its own snapshot read, which the source held then; a move
  under the export marks it drifted in-band, so no restore or apply lays it
  down (`CAPTURE_DRIFTED`), and a move outside the export's window poisons
  the recorded key (a move away and back across those windows too, nested
  worktrees' HEADs compared with the export's own census before and after
  its pass). A key-drift capture is a coherent snapshot of the state the
  export read: it applies and the closure counts it applied. Either way
  the next pass captures the new HEAD and index, and the apply after it
  converges the landing.
- The stash is drift custody too, since OI-1003-Q161 (2026-10-15): an
  agent's `git stash push`, `pop`, `drop`, `clear` or `store` under the
  pass rewrites the stash reflog (and refs/stash with it, except a drop
  below the tip). It is a `StashMoved` row named `refs/stash`
  (`stash_drift`), beside any `refs/stash` ref row, in the same two classes
  as HEAD and the index: the bundle carries the stash its snapshot read
  (its commits and the stash order), a move under the export is never laid
  down, a move outside it poisons the key (away and back too), and the
  next pass carries the new stash, which the apply converges in the
  source's order (P87's stash row). A `pop` or `drop` of the last stash
  between a read of the ref inventory and the read of the stash reflog
  (refs/stash and its reflog deleted) reads as no stash, so it too is a
  move (`StashMoved`, or `RefRemoved` in the snapshot, or a torn key),
  never `GIT_CHILD_FAILED`; any other failure of that read still refuses.
  The EstateConverge model's raced moves include it (`stashrace`); no
  protocol step changes.
- The exclude file, configuration, the shallow frontier, foreign nests and
  gitlinks (a staged gitlink change too), or the omitted rebuildable roots
  moving under a pass still refuse `GIT_AUTHORITY_CHANGED` (R-N30). These
  residual refusals are a design choice; no operator ruling scopes them
  (the rulings cover HEAD and index, #38, and the stash, Q161).
  `git-export` and the other strict exports still refuse any drift.
  Directory-shape drift (a directory removed, a directory replaced by a file, a
  file replaced by a symlink) refuses fail-closed.
- A `.git` gitdir-pointer file below the checkout that does not name its
  own administration refuses before any byte is read, by that directory's
  path (M2r2 item 6; each was a bare `GIT_INVENTORY_MALFORMED`).
  The pointer is read as Git reads it: raw bytes after `gitdir: `, with
  trailing CR and LF stripped. A registered path is read against the
  administration directory.
  - `GIT_NEST_POINTER_STALE` covers two cases:
    - It names an administration absent on this host (missing, not a
      directory, or reached through a regular file). This happens with a
      copy of a pruned linked worktree, one carried from another host, a
      submodule whose `.git/modules` entry was removed, or a pointer through
      a dangling symlink or an absent mount. Git does not read such a
      directory as a repository.
    - It names a worktree administration of this repository whose
      registered checkout exists and points back to it. The directory is a
      copy of that live worktree, and Git would act on the live worktree's
      HEAD and index through it.
  - `GIT_NEST_WORKTREE_MOVED` covers the registered checkout being absent:
    a worktree moved without `git worktree repair`. Git still works there,
    and `git worktree repair` run in it re-registers it.

  The census stops at the first such directory, so a run names one; a
  rerun names the next. The remedy is the operator's, in the source, and
  depends on the case:
  - for a leftover copy, remove or rename its pointer;
  - for a submodule, restore its administration;
  - for a mount, bring it back;
  - for a moved worktree, repair it.

  Whether to carry an absent-administration directory as plain files
  instead is an open question. The pointer would have to be omitted or
  neutralised and recorded, because restoring it could reattach to a real
  administration on the destination.
- An object-store rewrite is drift too (WP1, S5). The export reads the
  source's pack listing with its authority; when a Git child of the pass
  fails and that listing has changed (a `gc`, `repack` or `prune` racing
  the pass through the private repository's `alternates`), the item is
  `deferred-with-drift` with one `ObjectStoreRewritten` row, no capture
  record is written, and the next pass captures the rewritten store. The
  child's failure is the one a failed v1 Git child raises,
  `GIT_CHILD_FAILED` with its stderr class (WP3), or
  `GIT_INVENTORY_MALFORMED` for output that did not parse; the same failure
  under an unchanged listing still refuses.
- The pass window runs from the pre-pass key to the post-pass key, not only
  across the export's own snapshot. A capture is clean only when the
  pre-pass key parts, the export's own before and after ref inventories, and
  the post-pass key parts all agree; a ref that vanishes before the export's
  snapshot and returns after its last ref read is drift (R-N72).
- Git authority is read once per export and carried exactly (the index
  checks validate a private copy of the carried bytes, never the live index):
  HEAD, the symbolic HEAD, the index bytes, `info/exclude`, the stash reflog, the
  configuration files and the shallow frontier. The export re-reads all of
  it at the end of its pass. HEAD, the symbolic HEAD, the index and the
  stash reflog that moved, under the export or between a key and it, even
  away and back, are named as drift rows (#38, OI-1003-Q161), so the
  recorded key is poisoned and a stale
  bundle never stands behind an equal key; the rest refuses
  `GIT_AUTHORITY_CHANGED` unless both key parts name exactly what the
  export carried.
- Index entry flags (#106): an intent-to-add entry (`git add -N`) is
  carried as index custody, `refs/carry-export/intent-to-add-v1` (path,
  mode, empty-blob flag), recorded only when there is one; its seat's bytes
  ride in the worktree tree. `git-restore`, `git-restore-linked`, estate
  apply and `git-repair-missing-index` re-mark the paths after reading the
  staged tree and check them back, so `git status --porcelain=v2` matches.
  An intent-to-add entry whose seat is gone, one whose seat's mode is not
  the entry's (`git add -N f; chmod +x f`: restore re-marks the path from
  the seat and would read 100755, #131), or one in a nest (whose index is
  not carried), refuses `GIT_INVENTORY_INTENT_TO_ADD` at capture; the
  restore verbs check the custody against the worktree tree again before
  any worktree byte is laid down. The `git-attach-*` verbs refuse a capture
  carrying any. Any other entry flag (assume-unchanged, skip-worktree)
  still refuses `GIT_INVENTORY_MALFORMED`. Git's fsmonitor validity bit
  (`CE_FSMONITOR_VALID`) is cache state, not index state: it is masked
  before an entry is classified, a source's fsmonitor never runs (every
  carry pins `core.fsmonitor=false`), and a restored index carries no
  fsmonitor extension (#131).
  A bundle whose prerequisite commits the receiving repository lacks
  refuses `GIT_INVENTORY_MISSING_PREREQUISITE`; fetch them as objects and
  retry.
- There are two drift classes. Export drift moved under the export itself:
  the bundle omits the drifted seats' bytes and carries the in-band
  `refs/carry-export/capture-drift-v1` marker. Every restore and import verb
  (estate-apply, `git-restore`, `git-restore-linked`, `git-import`,
  `git-repair-missing-index`, both `git-attach-*` verbs and
  `git-restore-registered-payload`) refuses a marked bundle with
  `CAPTURE_DRIFTED` before it writes anything, whether or not any corpus
  sidecar exists. A shallow envelope lifts the marker into its own headers
  (`shallow-drift-v1`), so the check reads bundle headers only and never
  fetches a pack. Each verb first stages the bundle into a private copy and
  reads only that copy, so the checked bytes are the imported bytes. The
  stage sits next to the bundle (the corpus, for an estate apply), falling
  back to TMPDIR. It is a full copy, never a clone, hashed while it is
  written, so apply's digest check costs no second read. A shallow envelope
  whose inner inventory carries the marker but whose headers do not still
  refuses when it is unpacked. Re-applying an item that is already done
  reads its journal first and stages nothing. Key drift moved only outside the export's window: the
  bundle is a coherent snapshot of the export's own view and applies.
- A drifted capture of either class records a poisoned key that no census
  hashes to, so it is never a reuse hit, even if its `{bundle}.drift` sidecar
  is lost or two state directories sharing one corpus interleave their
  records. The sidecar is the receipt's statement of what moved, not the
  guard. The next capture pass extends it (`capture-extended-from-drift`).
  R-N29 (apply proceeds on an occupied destination, recording uncaptured
  seats) is deferred (bulkload#48); carry v2, the W6 engine it was deferred
  to, is deleted (OI-1003-Q44, OI-1003-Q56).
- Auto-prerequisite chains (WP2, OI-1003-Q15, 2026-10-03). A capture that
  follows a retained capture of the same checkout
  declares that capture's tips as its bundle prerequisites, so it packs only
  what is new since then instead of re-packing all history. Only tips whose
  commits the source object store holds qualify; the capture's own metadata
  commits never do, so every link stays fetchable wherever the source is an
  alternate, and a pruned tip only means a larger pack. The `{bundle}.prior`
  sidecar (durable before the record) names the predecessor by name, digest,
  stat identity and chain depth. Depth is bounded by
  `git_carry::chain::CHAIN_DEPTH_LIMIT` (8): the capture after a depth-8
  bundle re-bases to a self-contained one. Estate apply verifies the chain
  (every link retained under its recorded name and digest, depths falling by
  one to a self-contained root, every prerequisite satisfied) and restores
  from one flattened bundle whose refs must equal the head bundle's. Apply
  never binds a link by stat identity: a corpus pulled to the apply host
  gives every file a new device, inode and ctime, exactly as the shared
  `.base` import binds by digest only (R-N72). Capture-side reuse and
  chaining do require the recorded identity, so a link replaced in place is
  never extended. A missing link refuses `SEALED_OBJECT_MISSING`, a link
  with other bytes `DIGEST_MISMATCH`, inconsistent depths
  `RECEIPT_BINDING_INVALID`, and a broken chain is never a reuse hit: the
  next capture re-bases. The tip query runs through the estimate's hardened
  source wrapper, and every stdin-fed git child drains its answer while its
  requests are written, so a prior with thousands of refs cannot hang a
  capture on a full pipe. The capture counters
  (`write_source_pack_bytes`, `write_source_pack_objects`,
  `read_source_pack_readback_bytes`, `census_walks`) measure it. A chained
  link or a grouped item bundle is thin (OI-1003-Q42): its deltas name
  bases its prerequisites hold, which packing reads but never writes, so
  `write_source_pack_bytes` no longer bounds the source reads of a thin
  capture; `read_source_pack_readback_bytes` sees them, as a lower bound.
  The next pass's blob-reuse fetch of a thin retained capture completes it
  with `index-pack --fix-thin`, reading every base from the source object
  store: `read_source_capture_reuse_bytes` counts those bases beside the
  bundle, and the fetch's storage reads join the readback counter.
- Reuse reads a manifest, not the bundle (Q42 lane L7; OI-1003-Q42,
  OI-1003-Q45, OI-1003-Q94, 2026-10-07). A changed capture used to fetch
  its whole retained bundle to learn what it held: 179 MB and 13.8 s of CPU
  for a 5-object change to the estate's history-heavy item. A capture now
  publishes a `{bundle}.reuse` sidecar: its regular seats, each with its
  census row and its blob, bound to the bundle by digest. The next pass
  takes from that list the seats it may reuse (the same rule as before:
  an unchanged stat identity, not racy), then asks which of their blobs it
  can read with one `cat-file --batch-check` in its private repository,
  which sees the source's object store through `alternates`. That child
  comes from the hardened Git builder: it writes nothing to the source and
  takes no lock (S2).
  - **No miss:** every blob is there, and the pass reuses them without
    opening the bundle. `read_source_capture_reuse_bytes` is 0 and
    `read_reuse_manifest_bytes` is the manifest's length (P69).
  - **A miss** is a reusable seat whose blob only the retained bundle
    holds: dirty content (untracked, ignored or uncommitted) that has not
    moved since. The pass counts the seats (`reuse_dirty_misses`) and
    fetches the bundle, exactly as before. A seat that did move is read
    again and is never a miss. A checkout that keeps one such file still
    therefore fetches its retained bundle on every changed capture: for
    it, lane L7 changed nothing. The seat is not read again instead,
    because its identity has not moved (R25).
  - **No manifest:** a capture from before lane L7, a shallow one, or one
    whose list is over 256 MiB. Its bundle is fetched, as before; old STATE
    and CORPUS need no migration.
  - **A manifest that does not match its capture** (it does not decode, it
    is bound to another digest, or it names an object that is not a blob)
    is never reused. The pass says `reuse_unavailable=manifest-mismatch`,
    reads every seat and still captures: a value, never a refusal.

  The sidecar is advisory. The bundle stays complete and self-describing,
  a restore and a whole-capture hit never read the manifest, and an engine
  from before lane L7 ignores the file and fetches the bundle. The manifest
  is durable before the `{item}.capture` record, like every other sidecar,
  so a record this engine wrote never lacks one. A crash in between leaves
  a manifest no record names, which nothing reads; the reverse order would
  restore just as well, but would leave a record whose capture every later
  pass fetches whole. P70 crashes a capture at each `estate.*` fault point
  around that publication and checks the order, that every manifest lists
  its bundle's blobs, and that the restore is exact.
- Grouped items chain under their plan base (Q42 lane L6b, fix 2;
  OI-1003-Q42, OI-1003-Q46, OI-1003-Q62, OI-1003-Q63, 2026-10-07). A later
  capture of an item on a shared plan base declares the base's commits and
  its own prior capture's source-held tips, so it too packs only what is
  new since that capture, where before it re-packed everything committed
  since the base. It carries both sidecars: `{bundle}.base` names the plan
  base and `{bundle}.prior` the prior capture. This is the code's only
  policy; there is no flag, and a build from before it refuses such a
  corpus rather than restore it wrongly (it fails closed, D4). Retained
  records need no migration: an ungrouped chain (`.prior`, no `.base`) is
  still a reuse hit and extends as before, a grouped record from before
  the change (a delta on the base alone) becomes the depth-0 root of the
  next changed capture's chain, and a record bound to a lost base still
  refuses `RECEIPT_BINDING_INVALID`. The depth bound is unchanged, so the
  ninth changed capture re-bases: it is a delta on the plan base alone and
  re-packs what the group committed since the base. P68 excludes that
  capture from its flatness bound and pins its cost; only the re-root of
  lane L8 removes it (OI-1003-Q62).
- The restore contract for a chain under a plan base (an amendment to
  R-N72, ratified as OI-1003-Q63 D2). Apply binds every base and every link
  by corpus name and recorded digest, never by stat identity. It imports
  every bound base before the oldest link, then each link in order, then
  the head, and restores from the one flattened bundle. A base must be
  self-contained. A base or a link the corpus does not hold refuses
  `SEALED_OBJECT_MISSING`, never a bare IO error (#181); other bytes refuse
  `DIGEST_MISMATCH`; a prerequisite no earlier bundle satisfies refuses
  `GIT_INVENTORY_MISSING_PREREQUISITE`. On the capture side a chain is
  intact only while every link and every bound base is retained at its
  recorded identity, so a chain whose base is gone is neither a reuse hit
  nor extended. One group has one base, so a chain binds one. It binds two
  only when the group's base record went missing while a retained record
  still bound the old base: the next pass exports a new base, as v1 did,
  and the restore imports both (D5). The base record itself is written
  no-replace: a record that appears concurrently stands, and the pass
  refuses `RECEIPT_BINDING_INVALID`.
- A capture record never names a chain nothing can restore (Q42 L6b review,
  2026-10-07). Under a plan base an export can reproduce, byte for byte, a
  bundle the corpus already holds: its link's held tips are then the base's
  own commits. It has that bundle's name, and identical bytes declare
  identical prerequisites, so three rules decide its `.prior`. A name that
  is the link, or is already in the link's own chain, gets none: the root's
  or the shallower link's recorded custody stands, and no cycle
  (root → link → root) is ever written. A `.prior` already recorded for the
  name stands while its chain is intact. When the export declared no link
  (a base's delta, or self-contained) and the name still carries a `.prior`
  whose chain is broken, that sidecar is removed, durably, before the
  record names the bundle: a chain broken by a lost base recovers on the
  next capture as the new base's delta even when that delta is the old
  head's bytes. A based bundle (its header declares prerequisites, no
  `.prior`) whose own `.base` sidecar is gone has lost its base: it refuses
  `RECEIPT_BINDING_INVALID` while its key holds and is never a chain link,
  matching apply, which refuses it `SEALED_OBJECT_MISSING`.
- Restore cost under fix 2 is not flat in the group (#147, open). Capture
  bytes are flat in the pass count; restore is not. Apply flattens every
  chained item on its own: it copies the head, every link and the plan base
  beside the corpus and writes one self-contained bundle holding them all,
  so it stages the base twice per chained item, where a delta on the base
  alone stages the base once per destination repository. The space
  preflight charges each chained item its head, links and base against the
  repository volume, and the staging peak against the corpus volume. P68
  pins it: `copied + n × base ≤ write_bundle_stage_bytes ≤ 2 × copied` for
  `n` chained items. Measured with a 1.33 MB base: 7.44 MB staged for two
  chained items at depth 1 and 11.16 MB for three, where their heads,
  links and the base once are 2.40 MB and 2.93 MB of corpus bytes.
- A drift-marked bundle may be a chain link (OI-1003-Q63 D1, #149). The
  pass after a drifted capture extends it clean, chained on the drifted
  bundle. Only that bundle's source-held tip commits become prerequisites,
  so the new capture packs again every seat the drifted pass withdrew, and
  the flattened restore advertises exactly the head's refs: none of the
  link's drift markers. The drifted bundle itself still never applies.
  Since Q42 L6a (OI-1003-Q43) these choices are one pure, total function,
  `git_carry::decide::decide`, which P67 pins to frozen rows (`tests/data/decide_rows.tsv`, rendered by the Haskell reference until R-C843 deleted it on 2026-10-17).
- A whole capture is reused (`capture-reused-after-census`) only when its key
  is unchanged and no seat is racy against its recorded pass start. A capture with a racy seat, or with no recorded pass start
  (records from before the start was recorded), takes the per-seat path
  instead (R-N76).
- An incremental pass reuses a retained blob only for a seat whose stat
  identity is unchanged and which was not racy. The transfer applies the
  same rule to its ledger and output rows (see Wire v6, racy captures). A seat stamped within one
  timestamp tick (a 2 s allowance) of the retained pass start can be
  rewritten at the same size without its identity moving, as in Git's racy
  index, so it is read again. A pass that reuses none of the blobs it was
  offered says why: `reuse_unavailable=shallow`, `retained-unreadable`,
  `pass-start-unrecorded`, `manifest-mismatch` (the retained capture's
  `.reuse` sidecar does not describe it) or `future-stamp` (a seat stamped later than the
  pass's clock blocks every whole-capture reuse until the clock passes it). A retained capture that cannot be read degrades
  to a full read, and its transient refs never reach the new bundle.
- A seat stamped later than the current pass's own clock reading is racy too
  (fail-closed on timestamps from the future). Known limit: the racy
  reference is the capturing host's wall clock, not the filesystem's. A
  filesystem whose clock runs behind the host by more than the 2 s allowance
  (an NFS or SMB server with NTP skew) can stamp a write made after the pass
  start earlier than the window, and the guard cannot see it. Bulkload never
  writes into a source to read the filesystem's clock.
- Known limit: capture records and their sidecars (`.capture`, `.parts`,
  `.drift`, `.base`, `.reuse`) are not authenticated. Anyone who can write the corpus
  can forge a record into a whole-capture reuse. The in-band drift marker
  still makes every restore verb refuse a drifted bundle, and apply still
  checks the bundle digest, but corpus integrity rests on its 0700 custody.

## Performance

[Label 2026-10-17 (R-N13), Bulkload Reset Review row E05: the two-gate
corpus race below is retired as the performance bar. Since R-C842 (Linear
TIN-4543 `ace8649c`) the gate of record is end-to-end wall-clock time to a
verified working seat, compared against an rclone sync of the same home
([SPEC](SPEC.md) R3); gate (a) stays only as a regression guard, and the
gate (b) harness `gate_b.py` is retired (N03; `m3_link.py` carries the
rclone arm). The gate text is kept as a dated record. The resume and
adoption rules after it describe the running engine and stand.]

Performance is the product bar (R23, R25). Two gates (R-N57); their order,
verdict rule, hosts and corpus are S1 in [slo.md](slo.md) (OI-1002-Q30's
B/A/B/A/B; OI-1003-Q97; OI-1003-Q194 C2):

- **(a) Loopback:** the native engine, running its full protocol over a local
  socket with no clone fast path, beats `rclone copy` as shipped on the same
  corpus and volume. Since OI-1003-Q154 (levers 1 and 2) the loopback's two
  halves are joined by an in-process hand-off: every control frame still
  crosses the socket as a wire v6 frame, and every chunk's 64-byte data
  header crosses it in v6's layout and stream order, but inside a handed
  frame (tag 0xB1), which is no v6 frame; the chunk's bytes are handed over
  in memory and hashed once, by the source (Wire v6, "Loopback hand-off").
  Whether "its full protocol over a local socket" (R-N57) covers that is the
  operator's to word; OI-1003-Q154 authorized the change. `BULKLOAD_LOOPBACK_HANDOFF=0` runs the
  plain protocol for A/B. `BULKLOAD_LOCAL_NEW_PATH_STREAM=1` adds the
  delta lever: with the hand-off, a seat new to the destination is
  streamed rather than asked for by manifest even when the destination
  holds other outputs (Wire v6, "WantManifest"; off by default). Gate (b) is unchanged: `pull` and `serve` never
  hand anything over and the destination hashes every byte.
- **(b) Cross-host:** a pull between two hosts beats rclone over sftp on the
  same corpus.

Resume (R25, R-N58): a byte the destination already holds durably is never
read again, and a seat whose stat identity (source authority, device, inode,
size, mtime and ctime at nanosecond precision) is unchanged, and was not racy
when it was recorded, is never read again. Stat identities are reuse keys, not content digests; changed files,
replaced inodes, journal gaps and lost source authority invalidate reuse, and
correctness takes precedence over a zero-reread claim. Every run reports
bytes read, bytes sent, flushes, memory and wall time.

Since #169 an output whose bytes are durable but whose row never committed
(a crash between publish and row commit, or a failed group) is adopted on
resume from its capture record, by hashing the destination's own bytes: 0
source bytes read.

- The record is an extended attribute on the output. It names the walked
  row (the seat's path and stat identity, with no store authority), the
  capture's manifest root and its size.
- A staged file gets it before its seal. An existing output adopted against
  a manifest gets it, or has a stale one replaced, once its bytes are
  verified.
- Only a non-racy capture gets one. It needs extended attributes, and on an
  adopted output a mode its owner can write. A record that could not be
  written is counted (`transfer_capture_records_unset`).
- An output with a matching row is never adopted: its `Reuse` comes from
  the row. A clean rerun adopts nothing.
- A source store that lost its authority re-keys every row, but its
  outputs' records still match, so those outputs are adopted, not re-read.
- An existing output with no matching row that no record proves is read
  again and counted (`transfer_unrowed_unproven`): no record, bytes
  rewritten in place, or a changed mode. A record that names another row
  (the seat moved since that capture) is read again and not counted.

## M2 gates (historical)

[Label 2026-10-17 (R-N13), Bulkload Reset Review row E05: the W4 and W5
corpus-race gates in this table are retired by R-C842 (Linear TIN-4543
`ace8649c`); they are not gates of record. The S2 budget rules after the
table stand (`s2_budget.py` is kept).]

[Historical, labelled 2026-10-16 (R-N13).] The M2 workstream acceptance
table as of 2026-10-05. S1–S5 in [slo.md](slo.md) replaced this framing;
gate rules, orders and targets there win. Stale here: W4's "wire v5" (the
wire is v6) and "< 1.5 s on the TinylandState corpus" (that corpus was lost;
OI-1003-Q3 sets no hard SLA); W5's neo→sting gate (b) (decided on the rig,
OI-1003-Q97, and reframed by OI-1003-Q194 C2). Rulings are recorded on
Linear TIN-4543.

| Workstream | Acceptance | Rulings |
|---|---|---|
| W2 measure | Byte, hash, flush and commit counters in every verb. Flush counts are a lower bound, because SQLite's and git's own syncs are not counted. The corpus-shaped durable floor is measured interleaved with rclone, ≥ 7 reps, with every gated sample taken under the conditions below. | R-N62, R-N81, R-N87 |
| W3 v3 engine wins | Destination BLAKE3 ≤ 1× payload. Full flushes ≤ groups + 1. The bench median improves. The < 1.5 s target moved to W4. | R-N77, R-N95 |
| W4 single pass (wire v5) | Gate (a): the loopback native engine beats rclone as shipped, 3-rep medians. Median < 1.5 s on the TinylandState corpus. File I/O ≤ 2.05× payload and hash ≤ 2× payload. Warm resume reads 0 bytes. A refused live-writer capture leaves no source ledger row. | R-N57, R-N58, R-N86, R-N95, R-N118 |
| W5 parallel and wire | Gate (b): a neo→sting pull beats rclone over sftp, 3 reps. Each result reports its percentage of the calibrated link ceiling. | R-N57, R-N64 |
| W6 M1 git carry | **Retired 2026-10-05 (OI-1003-Q44, OI-1003-Q56).** carry_v2's sender and ingest are deleted; v1 is the Git engine, and tag `carry-v2-final` holds the deleted code and this row's earlier text. The estimate half stands: `git-carry-estimate`'s thin pack equals what `upload-pack` sends for exactly the destination's held tips, offered ancestors first (P66). It pins `pack.useSparse=false` and `pack.useBitmaps=false`, adds `--shallow` with `--objects-edge-aggressive` when the destination is shallow, and refuses a shallow source with a full destination (`GIT_HAVES_UNPROVABLE`, `source_shallow_destination_full`). The segment-resume rule and the network-filesystem ingest refusal went with the ingest. | R-N60, R-N74, R-N75, R-N97, R-N113, R-N116, R-N131, OI-1003-Q44, OI-1003-Q56 |
| W6 M2 git carry | `census_walks == 1`. `bytes_read` equals the total size of the changed seats. The live-writer test passes. | R-N58 |
| W7 proofs | I1 (a record implies its bytes), I2 (no partial leaves) and I3 (a committed file means 0 source reads on resume) hold at every process-crash fault point. Power-loss ordering is proven by a syscall-trace crash-state checker. Darwin barriers are modelled device-wide, with the per-fd model available as a strict option. | R-N86, R-N88, R-N103 |

Gated benchmark samples require AC power and a 1-minute load below 2.5,
and each sample row records both (R-N81). (R-N91's quiet lanes are
read as superseded by OI-1003-Q194 C18 together with SPEC R3, "Samples run
on live hosts"; RUL-18 does not name R-N91, and how R-N81 fits SPEC R3 is
open: `docs/slo.md`, amendment 2026-10-16.) The clone fast path never
counts toward a gate (R-N57, R-N63). The clone lever is excluded
(OI-1003-Q164, Q165): the engine has no clone path, and S2 forbids one on a
source file (Live union, "No clone of a source file"). Every copy of the
sealed corpus a sample reads is an explicit read and write, checked with
`FIEMAP` before the sample. The bench rechecks its fixture after every arm,
and a shared extent refuses the sample (docs/slo.md, the 2026-10-15
amendment).

S2's budget measured alongside a real run (#165, `s2_budget.py observe` and
`alongside`; OI-1003-Q152 keeps the source live) is judged against the
source's own live load (OI-1003-Q160): the OFF windows between passes are
the baseline, with no absolute load1 cap (R-N81's 2.5 is amended for
alongside observations only; a gated `s2_budget.py run` keeps it). The
budget is unchanged (+25 % p95, +2.0 load1 over that baseline), and AC
power, at least five alternating windows of at least 300 s and recorded
load are still required. The verdict reports the OFF windows' load spread
and compares each pass with the OFF windows on both sides of it,
interpolated to the pass's time, gate by gate: a pooled PASS stands only
when every pair passes, a pooled FAIL when every pair fails a gate that
failed pooled. A PASS also needs no recovery tail in the OFF windows after
passes. Every pass is judged (a pass dropped as a short ON window is not
evidence), and the first OFF window must be clear of a pass recorded just
before the observation. This removes linear drift in the source's own
load; load that rises and falls with the passes themselves is not
separable, and the instrument lists no processes to separate it (R-N104).
Such evidence is labelled `alongside, live baseline (OI-1003-Q160)`.
During an S2-measured run the passes start on a schedule fixed (or
seeded-random) and recorded in advance, never on the host's load
(OI-1003-Q168): `s2_budget.py schedule` writes it (slot times, or a first
time, period and pass count with a seeded jitter, sealed by a digest),
`observe --schedule` records it in the observation at its start, and
`drive` starts each pass at its slot's time, marking it with its slot and
the schedule's digest and waiting for it to exit. `alongside --evidence`
refuses evidence unless the observation recorded a valid schedule made
before it began, every pass names a slot of it, no slot has two passes,
every slot inside the observation has a pass, and every pass started
within 5 s of its slot (deviations are reported in the verdict, the
summary and an `evidence.tsv` `schedule` row). So pass placement cannot
follow the source's own activity.

## Wire v6

The transfer wire is protocol 6, a hard cut with no dual stack (R-N59,
R-N118). v6 (#218, 2026-10-08) is v5 plus the SQLite snapshot seat: the
session's `SqliteMode` in `Open`, a database's `-wal` identity in `Entry`,
the `SqliteSidecar` (21) and `SqliteSnapshot` (22) controls, and the
`SQLite` result code of a `SQLITE_BACKUP_FAILED` refusal in `Refused`
(`sqlite_code`, `None` for every other code); see "SQLite snapshot seats"
below. Every other frame, the W6 reserved ones
included, is v5's. `bulkload-proto` holds the codec and its schema text, `WIRE_SCHEMA`;
a peer whose `Open` names another protocol or another BLAKE3 of the schema
(`wire_id`) is refused before anything else. Every frame is a 4-byte
big-endian length, a tag byte and a body: tag 1 is a postcard control
message, tag 2 a content chunk (a fixed 64-byte little-endian header, then the
payload, sent with `writev`), tag 3 a Git pack piece (reserved). A control
body is exactly one postcard message: bytes after it are refused
`FRAME_CODEC`, as are bytes after a record in either store, which read as a
miss (#87).

- **Entries.** The source offers each walked seat as a numbered `Entry`, up to
  1024 undecided, and the destination answers each with `Decide`: `Skip` (a
  directory or symlink it made), `Reuse` (it holds this exact stat identity
  durably; the source reads nothing), `Refuse`, `Send` or `WantManifest`.
- **Walk.** The source walk is a stream: each seat is offered as it is
  found, a directory before anything beneath it, names in byte order within
  a directory and no global sort, so the first `Entry` leaves before the
  walk ends. The walk runs at most 4096 items ahead of the wire. Every seat
  is walked and read beneath one descriptor of the source root, opened
  component by component with `O_NOFOLLOW` at each one, so a directory
  swapped for a symlink is refused, never followed out of the root. A seat
  more than 256 components below the root, or with a relative path over 4095
  bytes, is refused as a value (`PATH_DEPTH_EXCEEDED`, `PATH_TOO_LONG`) with
  its subtree, and its siblings are carried; the walk so holds at most 256
  directory descriptors, plus one while it lists a directory at the cap
  (#110). Both caps are `WalkLimits`, configurable only below these
  defaults. A directory at the depth cap is listed and refuses its contents
  only when it holds something the walk would carry; the length cap applies
  after the stat. Engine temporaries are matched before either cap, and a
  seat on another device in a same-device walk is skipped before either, so
  neither is a false refusal (#129). A capped subtree is a refusal like any
  other, by path and code: it is never counted as carried, it keeps a
  strict-completeness run from finishing its directories, and the transfer
  receipt counts it as `capped_subtrees`. Content is read with `pread`,
  never mapped.
- **Send.** The source reads the file once, chunks it, hashes each chunk and
  streams it as a data frame (entry, chunk index, offset, size, digest), then
  sends `End` with the manifest root, chunk count, size and whether the
  capture was racy. The destination
  verifies every chunk against its digest, writes it at its offset, and checks
  coverage and the root before the output is queued for its group commit.
  A local `copy`'s handed chunks are the one exception: the source's digest
  of those very bytes is their verify ("Loopback hand-off" below).
- **Loopback hand-off (local `copy` only, OI-1003-Q154 levers 1 and 2).**
  `copy` runs both halves in one process, from one build, over a socket pair
  it makes, and joins them with a hand-off queue (`transfer/handoff.rs`)
  that only it can make: `serve`, `receive` and `pull` take none, and
  neither half can be serialised, so nothing that arrives over a transport
  can stand for one. The source moves each chunk's allocation into the
  queue, then writes only its 64-byte data header on the socket under a
  tag outside v6's (0xB1), so the frame keeps a data frame's place among
  the control frames; the destination takes the chunk when it reads that
  frame. A chunk can only be made by hashing its bytes, and its bytes sit
  behind an immutable `Arc`, so a handed chunk whose digest is its frame's
  is the very buffer the source hashed to it: the destination writes it
  without hashing it again (verify once; `wire_handed_bytes` counts these
  bytes, `blake3_dest_wire_verify_bytes` every other received byte, and
  every received byte is in exactly one). Bytes read from any transport
  are always hashed. A reader without the hand-off, and the proto codec,
  refuse a handed frame `FRAME_CODEC`. The source also batches its frames
  until it is about to wait for an event, and both halves read the socket
  through a 64 KiB buffer. Credit, the windows, every check of a frame's
  index, offset, size and manifest, and every durable step are unchanged;
  crash adoption (#169) and local reuse still hash the destination's own
  bytes. `BULKLOAD_LOOPBACK_HANDOFF=0` turns all of it off.
- **WantManifest.** Chosen only when the destination could fill chunks
  itself: the output path already exists (adopt, supersede or refuse; see
  Durability), the sweep salvaged a temporary, or published outputs hold
  chunks (resume, incremental). The last is cross-file dedup. With the
  delta lever on (`BULKLOAD_LOCAL_NEW_PATH_STREAM=1`, OI-1003-Q154; off by
  default, below), a local `copy` with the hand-off does not use it for a
  seat new to both stores: no file at its path, no reuse row under its
  key here, no capture of it in the source's ledger, and no temporary
  salvaged in the session. Such an entry is answered `Send` there, so its
  first byte is placed while the rest is still being hashed, not after
  the whole file is read and hashed for a manifest. Both halves are in
  one process, so a local fill would save no wire byte: it re-reads and
  re-hashes destination bytes in place of the handed buffer, and with no
  ledger row the source reads the seat once either way. The destination
  reads the source's ledger directly (read-only, the store the source
  half opened), so a seat the source captured for another destination
  from the same source state keeps its manifest, which the source answers
  from its ledger without a read; so does a seat whose reuse row is here
  and whose output a third party removed (P79). A ledger the destination
  cannot open, or a lookup that fails, keeps the manifest.
  `transfer_new_paths_streamed` counts the streamed entries and
  `transfer_manifests_asked` every `WantManifest`. `pull`, `serve` and
  `receive`, and a `copy` with the hand-off off, keep the rule above, so
  across hosts a new file whose chunks the destination holds still
  crosses the wire as only the chunks it lacks. A file that appears at a
  streamed seat's path during the session is refused at its publish, not
  adopted, as into an empty destination; the next run asks for its
  manifest and adopts it if its bytes verify, the source reading the seat
  once more (R25 holds: no committed row vouched for it). The
  lever is off by default because a new seat's handed bytes count in
  `bytes_received`, which the tests hold to S3's inequality 2 on `copy`
  (WP0(c)): an added copy of a carried file would count whole. With it on,
  P18 and the P21 added-seat cases in `tests/s3_transfer_resume.rs`,
  `w3_engine_properties` (W3's cross-file prefix fill) and
  `a_lost_output_does_not_lose_reuse_of_its_chunks` (#59's older-holder
  fallback) fail on that bound alone. Whether handed bytes are wire bytes
  for that inequality is unruled. `just fault-harness` runs the crash,
  power-loss and resume suites with the lever on too (`delta-lever`).
  `BulkloadTransfer.tla` (deleted 2026-10-17, R-C843) already lets `RecvEntry` choose `Send` for any
  absent output, so the lever is inside the model (docs/slo.md, the
  2026-10-16 amendment). The source sends `Manifest`, from
  its ledger without reading when the stat identity is recorded; the
  destination fills what it can from verified local chunks and asks for the
  rest by index (`NeedChunks`); the source sends only those, then `End`. A
  fresh manifest is built from one read whose chunks are all kept in memory
  (512 MiB budget); a file past the budget is streamed as for `Send`
  instead, so no seat is read twice in a session.
- **Credit.** The destination grants 16 MiB of data payload and returns
  credit as it writes; the source never has more than the granted bytes in
  flight, and at most 16 entries' content. Available credit never exceeds
  the window; a grant past it is refused.
- **Ledger.** The source ledger is digest-only (R-N58): a capture records its
  row key and manifest (digests, sizes and `manifest_root`), never bytes, and
  only after its final stat check, so a refused live-writer capture leaves no
  row (R-N86). `manifest_root` (BLAKE3 in derive-key mode over each chunk's
  digest and size) replaces the whole-file hash. `SourceDone` follows the
  ledger's last commit.
- **Racy captures.** A capture whose seat's mtime or ctime falls within the
  2 s allowance of the clock read before the file was opened, or later than
  the clock read after its final stat check, is racy, exactly as in the Git
  carry (R-N76): a same-size rewrite in that tick can keep its stat
  identity. It is sent and published, but never recorded: the source keeps
  no ledger row, and `End{racy}` tells the destination to keep no output row
  under its key (its chunk hints are kept; they are re-verified on use). The
  next run reads the seat again (#86).
- **Refused seats (#186).** With `--sqlite=refuse` (the default) a seat is
  refused for its content when its first 16 bytes are a `SQLite` database or
  WAL magic (`SQLITE_STATE_CHANGED`): provider state, which only the
  `SQLite` backup path carries. With `--sqlite=snapshot` a database magic is
  a snapshot seat instead (below); a WAL magic is still refused. Those bytes are a sniff, counted as `source_sniff_bytes` and
  never as content (`source_bytes_read`, `read_source_file_bytes`). The
  source ledger remembers the refusal under the seat's row key, when the
  seat was not racy and its stat identity did not move across the sniff, so
  a later run refuses the unchanged seat with the same code without opening
  it (R25); a seat whose stat identity moved is sniffed again. The refusal
  is reported on every run (S4). The record depends on the mode (#218
  review R8): `refuse` mode writes v5's tag 1 and honours it; `snapshot` mode
  ignores tag 1 (it does not say which magic was seen) and writes tag 3 for
  a WAL magic.
- **SQLite snapshot seats (#218; `--sqlite=snapshot`; operator rulings
  OI-1003-Q146 to Q148 of 2026-10-09, Linear TIN-4543, and unruled
  defaults in `docs/agent-notes/2026-10-08-sqlite-carry-design.md`
  section 0).** The destination chooses the mode; `Open` carries it to the
  source. The default stays `--sqlite=refuse` (OI-1003-Q147): `snapshot` is
  opt-in, and a run that must carry databases, such as the full neo→sting
  pull, passes `--sqlite=snapshot` explicitly.
  - *Walk.* A regular file with a regular `<name>-wal` beside it is offered
    with that file's identity (`Entry.wal`). A regular `-wal`, `-shm` or
    `-journal` beside a regular base is never an entry: it is reported as
    `SqliteSidecar`, and its outcome is its base's, resolved when the session
    ends: covered when the base was published from a snapshot or reused as
    one, refused by name (`SQLITE_STATE_CHANGED`) otherwise. A sidecar with
    no base is offered and refused by name, as in v5. *A Git ref or reflog
    named like a sidecar is exempt* (M2r2, 2026-10-17). A branch named
    `fix-journal`, `fix-wal` or `fix-shm` has a loose ref and a reflog of
    that name beside the ref of its base name; the in-place run's cmux pull
    refused three such reflogs `SQLITE_STATE_CHANGED`. The exemption is
    decided by content, for a path with a directory named `refs` or `logs`
    above it (any administration: `.git`, a bare repository whatever its
    name, a submodule's `modules/<name>`, a linked worktree's). The file
    must read as a ref or reflog: `ref: `, or a 40- or 64-digit lowercase
    hex id followed by a newline or a space. Its base-named neighbour, when
    present, must not carry the `SQLite` magic. Such a file is offered and
    carried as an ordinary file, and its base is never offered as a
    database beside a `-wal`. Anything else under `refs/` or `logs/` keeps
    the sidecar rules: an empty file, a `SQLite` journal, WAL or
    shared-memory file, text that is not a ref, or anything unreadable.
    The prefilter is by path, so no other sidecar is read for this; a
    candidate's check reads at most 80 bytes of it and 16 of its base
    (`source_sniff_bytes`). A sidecar listed and
    gone before its stat (a live rollback-mode store's `-journal`) is still
    its base's sidecar, not an `IO` refusal. A base without the
    database magic (an encrypted store) offered beside a `-wal` that holds
    frames is refused `SQLITE_STATE_CHANGED`, never carried raw: its main
    file alone may miss the `-wal`'s transactions or hold a half-done
    checkpoint (#218 review). The refusal is remembered under its row and
    the `-wal`'s identity. Beside an empty `-wal` it is carried raw.
  - *Key.* A snapshot seat's reuse row is keyed on its row, a marker and its
    `-wal`'s identity after the backup (`sqlite_key`), never equal to a row
    key: an unchanged store is `Reuse`d with nothing opened on the source,
    and a file that is not a database keeps its row key. Its capture record
    (#169) is keyed in its own domain on the same pair (`unrowed::
    sqlite_record_key`), so an unrowed snapshot is adopted only while its
    `-wal` has not moved (review R1).
  - *Capture.* A database seat is sniffed (16 bytes, never content); one
    owned by another user than the reader is refused
    `SQLITE_SOURCE_NOT_OWNER` before `SQLite` opens it. Otherwise the source
    takes the `snapshot` verb's stepped backup (OI-1003-Q16; one at a time,
    a step budget and a 30 s cap: `BUDGET_EXCEEDED`, not remembered) from
    `canonical_root/rel_path`, opened read-only, WAL-aware and
    `SQLITE_OPEN_NOFOLLOW` (which refuses a symlink at any component of the
    path, `SQLITE_CANTOPEN_SYMLINK`, refused as `PATH_ESCAPES_ROOT`), into a
    slot in its private state (`sqlite-snapshots/`, emptied when `serve`
    starts), converts the slot to journal mode DELETE and runs
    `integrity_check` on it, the destination's own check (`quick_check`
    does not compare index content with table content). Every slot,
    streamed or kept for a manifest, is reserved against 4 GiB of slots
    alive at once before its backup, waiting up to 30 s for room, and the
    slot directory's filesystem must keep the `--min-free-percent` floor
    with it written: `BUDGET_EXCEEDED` otherwise, not remembered. A
    sniffed database's descriptor is closed only under the backup lock, so
    it never drops the POSIX locks of a backup of a hard link of the same
    store. A residual stands: a directory swapped between `SQLite`'s own
    symlink check of the path and its `open` of it, or before it opens the
    `-wal` and `-shm` by name, is not seen. The main file and the
    `-wal` are stat'ed beneath the seat's directory before and after; the
    walked inode must still be at the path (`PATH_ESCAPES_ROOT`). The
    capture is settled when the walked, pre- and post-stat main file agree,
    the `-wal` did not move (or is the empty one the read created,
    OI-1003-Q72), and neither is racy; an unsettled capture is sent as racy:
    published, never a reuse key. `SqliteSnapshot{entry, size, wal}`
    precedes the entry's content, which is the slot's chunks, never the live
    file's. No source ledger row is kept for a snapshot. A corruption the
    database's bytes alone decide (`SQLITE_INTEGRITY_CHECK_FAILED`, or
    `SQLITE_BACKUP_FAILED` whose primary code is 11 or 26) is remembered
    under the snapshot key when the capture settled.
  - *Destination.* Admission reserves the main file's and the `-wal`'s
    sizes at Decide and the snapshot's exact size when it is announced. A
    path with a `-wal`, `-journal` or `-shm` beside it is refused
    `DESTINATION_OCCUPIED` at Decide (nothing is read on the source; not
    remembered; removing the sidecar converges) and again just before
    publish. The staged file is verified (header, size, not WAL,
    `integrity_check` exactly `ok`, opened `immutable=1`) before it is
    published; a failure refuses `SQLITE_INTEGRITY_CHECK_FAILED`, whose
    default S4 disposition is `abandon` (OI-1003-Q148; printed beside the
    refusal as `default-disposition <path>: abandon`, and since WP3 PR 4
    the transfer's closure row carries it: `closure-report --transfer`). Corruption the backup step
    itself reports (`SQLITE_BACKUP_FAILED` with primary code 11
    `SQLITE_CORRUPT` or 26 `SQLITE_NOTADB`) never reaches
    `integrity_check`; it keeps its own code, and takes the same `abandon`
    default and report line: the source sends `SQLite`'s code beside the
    refusal (`Refused.sqlite_code`), and the receiver reads it
    (`transfer::default_disposition`). A backup that failed otherwise
    (busy, I/O, a hot journal) has no default.
  - *Supersede (OI-1003-Q146).* A changed store's new snapshot replaces
    the output at its path only through the superseding publish below
    (WP0(d)'s `prepare_supersede`, intent, `RENAME_EXCHANGE` and displaced
    check), and only when this store landed that output and its ownership
    proof holds (its identity is still a row this store committed: nothing
    touched it since), and no `-wal`, `-journal` or `-shm` sits beside it
    at Decide and again at the exchange's last look, immediately before
    the exchange (`StagedFile::sqlite_sidecar_appeared`, after the
    identity check). A sidecar found at that look refuses the entry
    `DESTINATION_OCCUPIED`: nothing is exchanged, the intent is settled
    with the old output's rows given back, and removing the sidecar
    converges by the exchange on the next run (not remembered,
    `dest_sqlite_sidecar_refused`). A file this store did not land, or
    landed and something touched since, is refused `DESTINATION_OCCUPIED`
    and left byte-identical, remembered as for files. The fresh publish
    (rename without replacement) makes the same last look. A crash at any
    step leaves the old snapshot or the new one at the path, whole, as for
    files. Residuals, stated: (1) a `-journal` or `-wal` created between
    that last `lstat` and the exchange (a connection that began writing in
    that window); (2) an application whose SQLite predates 3.8.3, or that
    uses a VFS whose files do not track their inode (on Linux `unix-none`,
    `unix-dotlock`, `unix-flock`, or a custom VFS), whose next write after
    the exchange can create its `-journal` beside the new file. A modern
    SQLite connection on the default `unix` VFS still open on the old
    inode does not write silently: before it opens a rollback journal the
    pager asks whether the file moved (`pager_open_journal` calls
    `databaseIsUnmoved`, `SQLITE_FCNTL_HAS_MOVED`, which `stat`s the path
    and compares the inode it opened), so its next write fails
    `SQLITE_READONLY_DBMOVED` ("attempt to write a readonly database"),
    creates no `-journal`, and the new file stays `integrity_check` ok
    (read in the bundled SQLite 3.46.0; checked 2026-10-09 with SQLite
    3.51.2, an idle connection and one in `BEGIN IMMEDIATE` that had not
    yet journaled). Its reads keep seeing the old inode until it reopens.
    A connection whose `-journal` already exists is seen by the look and
    refused. (The conservative build had refused every supersede, D7, for
    the idle-connection case, which Q146 replaces.)
    `dest_sqlite_superseded` counts the replacements.
  - *S2.* The source access is the provider verb's: the Q16 lock, the Q36
    and Q72 writes, counted (`source_wal_index_touched`,
    `source_wal_created`), and the connection's whole life as
    `source_sqlite_lock_ns` (a WAL-mode connection holds `SHARED` on the main
    file until it closes). A `snapshot`-mode session as root is refused
    `SQLITE_SOURCE_AS_ROOT` at `Open`, before anything is opened or created;
    `copy` runs `serve` as its source half, so it is refused the same way.
  - *S3.* `source_sqlite_backup_bytes` (pages stepped times page size,
    restarts included, plus the `-wal`'s size: a bound on its wal-index
    rebuild) is added to the session's source bytes; the slot's re-read is
    `read_source_snapshot_bytes`, not a source read.
- **Rows from before the racy guard.** A store created by this engine
  carries a `racy_guard` marker from its first commit. A store without it was
  written before #86, so none of its ledger or output rows is proven
  non-racy. The first writable open deletes every `captures` and `outputs`
  row in the transaction that adds the marker, and counts them in
  `transfer_legacy_rows_invalidated`; a read-only handle on an unmarked store
  serves none of them. Each such seat is read from the source once more,
  which R25 allows (its row cannot show it was not racy), and recorded under
  the guard; chunk hints are kept, so content the destination still holds is
  adopted after verification and never crosses the wire again (#125). The
  migration runs (OI-1003-Q13: a repeatable run, not a one-off) may therefore start from existing
  pre-migration state.
- **Known limit: clocks.** The racy predicate compares the source
  filesystem's mtime and ctime with the capturing host's wall clock, as in
  the Git carry. Destinations are local filesystems (OI-1001-Q17), but a
  source may not be: a source on NFS or SMB whose server clock runs behind
  the capturing host by more than the 2 s allowance can stamp a rewrite made
  during a capture earlier than the window, and the guard cannot see it; a
  server clock running ahead only makes more captures racy (fail-closed).
  Bulkload never writes into a source to read its filesystem's clock. Keep
  network-mounted sources NTP-synchronised with the capturing host, or carry
  them from a host where they are local.
- **Held.** The destination answers every `End` with `Held`. A capture is
  committed to the ledger only when the destination holds its bytes
  durably: `Held{true}` is sent once the output's group commit has
  returned (file and directory sealed, then the store commit), for a
  written output or an existing one verified against the manifest. No
  flush is added for it. So a committed capture is never read from the
  source again (R25, OI-1001-Q15): a resume reuses or adopts the final
  name. A group whose seal or store commit fails (a full disk refuses
  `DESTINATION_SPACE_INSUFFICIENT`) answers each of its entries
  `Held{false}`: neither store records them, the session finishes with the
  refusals, and the next run reads them once (#100). The sweep keeps every
  one of this store's orphaned file temporaries as a chunk source, under a
  name of the new session. They only save wire bytes: no recorded capture's
  bytes live only in a temporary. When the session finishes it removes them,
  except those an entry refused in that session had staged chunks from (a
  byte-touching refusal: verification, publication or the group commit
  failed after the entry was filled), which are kept for the retry and
  reported as left (#124, OI-1002-Q33). An entry refused before it staged
  anything keeps nothing, so a path refused on every run never keeps
  temporaries (#97). What is kept is bounded, 1024 temporaries and 4 GiB per
  session at a destination root (OI-1003-Q24, which ratified these numbers; OI-1002-Q33
  ruled that salvage is bounded); a temporary past the bound is removed and refused as a value,
  `SALVAGE_BOUND_EXCEEDED` under its current name, and its chunks are sent
  again.
- **Hints.** The destination records, per digest, every published output
  holding it, newest first; a hint is re-read and re-verified on use, and a
  miss falls through to the next.

Known limit: a fresh destination is streamed with no cross-file
deduplication, so a chunk repeated across files crosses the wire once per
file. Deduplication applies when the destination asks for a manifest.

### Reserved Git sub-stream (W6)

Control variants 12 to 19 and tag 3 are reserved for W6 git carry over the
same session and are refused today. A sub-stream carries negotiated thin
packs for one repository (R-N60). Since carry_v2's deletion (2026-10-05,
OI-1003-Q44, OI-1003-Q56) no code builds or consumes them. They stay in wire
v5, and in v6 (#218) unchanged; they go with WP3's v7 cut (design D8 of
the #218 note, unruled).

| Frame | Direction | Meaning |
|---|---|---|
| `SubOpen{sub, GitPack{repo, refs_digest}}` | source → destination | Opens sub-stream `sub` for a repository and the ref inventory it will carry. |
| `GitHaves{sub, haves, done}` | destination → source | Object ids the destination holds, in rounds, ancestors first; `done` closes negotiation. |
| `GitSegmentStart{sub, segment, objects, bytes}` | source → destination | A thin-pack segment begins, with its object and byte counts. |
| tag 3 `PackData{sub, segment, offset, size}` | source → destination | Raw pack bytes of a segment at an offset, under the same credit as data frames. |
| `SegmentEnd{sub, segment, pack_digest, objects, bytes}` | source → destination | A segment's bytes are all sent, with their BLAKE3. |
| `SegmentDurable{sub, segment}` | destination → source | The segment is ingested durably in quarantine. |
| `GitRefs{sub, updates}` | source → destination | The ref transaction to apply after every segment: name, expected old value, new value. |
| `GitCommitted{sub, transaction_digest}` | destination → source | The ref transaction committed. |
| `GitResume{sub, durable_segments}` | destination → source | On a resumed sub-stream, the segments already durable, so only the others are re-sent (W6 M1: a crash in segment k re-sends only segments ≥ k). |

**Caller retry contract (#89, OI-1001-Q17): retired 2026-10-05.** It
specified how a caller retries carry_v2's ingest locks
(`Ingest::finish_retrying`, `FenceRetry`). Those were deleted with carry_v2
(OI-1003-Q44, OI-1003-Q56), along with the `JOURNAL_OWNERSHIP_CONFLICT` and
`GIT_DESTINATION_FILESYSTEM_UNSUPPORTED` refusals that only they raised. Tag
`carry-v2-final` holds the code and the contract's text. A future receiver
for these frames needs a contract of its own.

## Durability

A record is never committed before the bytes it describes are durable on the
destination. Existing destination files are never overwritten in place, and
publication is no-replace, with one ruled exception (superseding publish,
below). A directory the engine creates is made under a tagged temporary
name, its record is bound to the new inode, and it is then renamed into place
with no-replace. The engine never adopts a directory it did not create
(R-N78, R-N102). An existing directory with no owning record is a
container only, its mode never changed and no record bound to it: when its
mode equals the source's it is accepted as before; when it differs it is
accepted with its mode kept only if it is this user's, gives its owner
read, write and search, and grants nobody else anything the source's mode
does not (the owner's own bits are not compared, so an unfinished 0700
directory under a read-only 0555 source is accepted). M2 item 2
(2026-10-16): a pull into a copy of an unfinished landing, 0700 directories
under a 0755 source, refused every directory of the copy, 160 of them, and
exited 1; the files below were decided all the same (a refused directory
refuses its own row only), so equal files were adopted and differing ones
refused. Such a directory is now counted (`dest_directory_mode_kept`) and
reported on stderr (`directory-mode-kept <path> found=<mode>
source=<mode>`); it is not in the transfer's closure row, and it never
converges to the source's mode, which would adopt it (residuals for the
operator). Any other divergent mode (wider than the source's, another
owner, no owner access) is refused `DESTINATION_OCCUPIED` as before.
Known gap, not changed here (#248): that refusal covers the directory's
own row, and its entries are still decided and published inside it, so a
private source tree can land inside a wider existing directory. An existing file
is adopted only when its bytes and its mode verify; an equal file whose
mode differs is refused.

Records commit in groups. Each file is sealed (`F_BARRIERFSYNC` on Darwin,
`fsync` elsewhere, so its mode is durable too) and renamed into place without
replacement. Each touched directory is sealed once. Each touched device other
than the state store's is fully flushed. Then one `SQLite` WAL commit
(`synchronous=FULL`, `fullfsync=ON`) makes the whole group durable; its full
flush drains the store's own device. A group whose files share the store's
device therefore needs no other device-cache flush. `--durability=strict`
fully flushes every file instead, for comparison.

A destination group closes at its file or byte bound, when the stream
drains, after 250 ms with no new output, or 1 s after its first output
(OI-1003-Q154 lever 3, fewer synced commits; the idle was 20 ms, and
`BULKLOAD_DEST_GROUP_IDLE_MS` sets it for A/B). With the 20 ms idle a local
copy of corpus v1 committed 10 to 18 destination groups on the rig; with
250 ms it is expected to commit once or twice there, as it did in sting's
informational census (1 destination group commit against 3 to 5,
2026-10-15, not of record), until the rig series measures it. Each group still
seals before its one FULL commit and every `Held` still follows that commit
(R25: `BulkloadTransfer.tla`'s (deleted 2026-10-17, R-C843) `Commit` takes any nonempty set of ready
seats, so a larger group is a behaviour the model already checks), and the
most a crash leaves unrecorded is still one group within the same bounds;
#169's adoption recovers it without a source read. No commit is relaxed:
WP0(g) relaxes only the source ledger's rows, as before.

A new store is created directly in WAL mode (OI-1003-Q154 lever 3): before
`SQLite` opens the new file, the engine writes page 1 of an empty WAL-mode
database (byte for byte `SQLite`'s own) and seals it, so no
rollback-journal commit switches it to WAL. Before, that commit synced a
`-journal` twice and the database once per store: plain `fsync`s on every
platform, since `fullfsync` is set only after the switch. The seal is one
`fsync` on Linux and one `F_BARRIERFSYNC` on Darwin (never `F_FULLFSYNC`,
which would add a device-cache flush the old path never paid); the store's
root seal and its first commit drain the device after it, so the page is on
stable media no later than the store's first WAL frame, which a WAL needs.
That ordering rests on this argument alone: the template's write and seal
are the database file's own I/O, outside the R-N88 trace, so no power-loss
proof explores them on either platform, and the Darwin seal is compiled
and tested (`just darwin-check`) but not measured on APFS.
`BULKLOAD_STORE_WAL_TEMPLATE=0` restores `SQLite`'s own switch for A/B
(`store_wal_templates` counts the stores made from the template). A crash
inside that write leaves at most one page of zeros and template bytes with
no `-wal`; the next open writes it again. A short or torn main file beside
a `-wal` is a store, never touched.

On Linux in group mode, a group of two or more outputs is sealed
device-wide instead (S1, OI-1003-Q107; `io::durable::batched`). Each output
starts write-back as it is queued (`sync_file_range(WRITE)`, not a
durability step). The group then runs one device seal per device number its
files live on (`syncfs` through that file's parent directory, opened again
for the seal, then `fsync` of one of those files), which makes
every temporary's data durable under its temporary name; renames each into
place; runs one device seal per touched device number again (`syncfs`, then
`fsync` of one touched directory), which makes the new entries durable; and
only then commits its records. No rename precedes the first seal, so no
power loss can keep a name whose data it lost. `syncfs` also flushes
whatever else is dirty on that file system, which is why a single-file group
keeps the per-file path.

Device numbers are `st_dev`, and each btrfs subvolume has its own, though
one `syncfs` commits the transaction of the whole file system. A group
spread over N subvolumes of one btrfs therefore runs N seals per step, each
a full transaction commit. Durability is unaffected, but the S1 gain is
unmeasured there and may be lost or reversed; no gain is claimed for a
multi-subvolume btrfs destination (#210 review; deduplication by the
file system's UUID is a follow-up).

`syncfs` is not in general as strong as the flushes it replaces, so four
conditions bound the batched path (OI-1003-Q107 review, 2026-10-08):

- **File system allowlist** (`io::durable::syncfs_seals`). A group is batched
  only when every device its files and their directories live on is ext4
  (or ext3, which the ext4 driver serves), xfs, btrfs or tmpfs, read with
  `fstatfs` on every file and directory for every group (nothing is cached
  by device number, which an unmount can hand to another file system); the
  ext magic also needs the mount's type in `/proc/self/mountinfo` to be
  `ext4` or `ext3`, because the ext2 driver shares it and its `syncfs` sends
  no device-cache flush. An xfs file flagged `FS_XFLAG_REALTIME` keeps its
  group on the per-file path: its data is on the realtime device, which the
  log force never flushes.
  Any other file system keeps the per-file path: on FUSE other than
  virtiofs `syncfs` never reaches the daemon, CIFS/SMB and NFS do not flush
  at the server, vfat and exFAT send no device-cache flush, overlayfs does
  not report the upper layer's write-back errors, and f2fs's `syncfs` returns
  0 without a checkpoint when checkpointing is disabled. On the allowlisted
  file systems `syncfs` writes every file's data and entries (tmpfs has
  nothing to make durable; its `fsync` is a no-op too), but even there it
  is not as strong as `fsync` on its own: see the next condition.
- **The seal's own `fsync`.** On xfs, `xfs_fs_sync_fs` only forces the log,
  and with an idle log that force returns 0 without any I/O
  (`xfs_log_force`, v6.12), so data written back in place with no log
  write after it, typical of a group of adopted outputs only, can stay in
  the drive's volatile cache; `xfs_file_fsync` sends the flush itself when
  its log force was a no-op. On ext4 after a forced shutdown, `ext4_sync_fs`
  returns 0 (v5.17 and v6.12) where `ext4_sync_file` returns `EIO`. Each
  device seal therefore ends with an `fsync` of one member file (or, in the
  second step, the touched directory), which covers both, at the cost of one
  extra (usually empty) cache flush per device per step.
- **Each file's own write-back error.** `syncfs` reports a write-back error
  against the cursor of the one descriptor it is called on, so an error an
  earlier `syncfs` elsewhere already consumed is not reported again. After
  the first `syncfs`, every file of the group is checked with
  `sync_file_range(WAIT_BEFORE | WRITE | WAIT_AFTER)`, which returns that
  file's own write-back error against its own descriptor's cursor, as its
  `fsync` would, and flushes no cache. One failure refuses the whole group
  before any rename. The `syncfs` itself runs on a directory descriptor the
  seal opens for it, whose cursor starts at the superblock's current
  error: a long-lived descriptor (a staged temporary, a destination
  directory) would report an error raised since its open, by one bad file
  or another process on a shared file system, again for every later group
  whose seal ran on another such descriptor, refusing healthy groups (S1
  stack review, 2026-10-15). The file system's own `->sync_fs` failure is
  returned whatever the cursor.
- **Kernel floor: Linux 5.17-rc3** (OI-1003-Q113 set 5.8; OI-1003-Q141, ruled
  2026-10-08, moved it to 5.17, on the two commits below). 5.8 made `syncfs` report data write-back errors, but
  until torvalds/linux commit 5679897eb104 ("vfs: make sync_filesystem
  return errors from ->sync_fs") `sync_filesystem` discarded the file
  system's own `->sync_fs` failure (journal commit, cache flush), and until
  2d86293c7075 ("xfs: return errors in xfs_fs_sync_fs") xfs ignored its own
  log force's failure. Both are first in v5.17-rc3, neither is in rc2, and
  both are in v5.17 (checked against the upstream tree 2026-10-08). An older
  kernel, a 5.17 rc1 or rc2, or one whose release cannot be read, keeps the
  per-file path; a backport in a distribution kernel below 5.17 is not
  detected.

A touched directory whose `fstat` fails has no device to seal device-wide;
a batched group seals it with its own directory flush. Darwin is unchanged.
A superseding
output (WP0(d), below) joins the first `syncfs` like any staged file, so its
data is durable before its intent is recorded and before its
`RENAME_EXCHANGE`, and the second `syncfs` makes the exchanged entry durable
before the commit that settles the intent.

Write-back while the data streams (S1, OI-1003-Q143 item 1a, extending
OI-1003-Q107). On Linux in group mode, an entry's staged file is kicked into
write-back (`sync_file_range(WRITE)`, `io::durable::kick_writeback`) each
time its placed bytes cross a multiple of 8 MiB: in the stream, and once
right after a manifest's local fill, before `NeedChunks` goes out, so a
delta's locally filled bytes are written back while the source captures the
rest. A kick makes nothing durable, never moves a descriptor's errseq cursor
(no `WAIT_AFTER`), and leaves every seal above unchanged; an error a kick
returns refuses its entry (a full device as `DESTINATION_SPACE_INSUFFICIENT`).
Each is counted (`writeback_kicks`). Strict mode and Darwin do not kick.

Receive workers (S1, OI-1003-Q143 item 3). The destination verifies each
data frame against its digest and writes it on a small pool of worker
threads (`BULKLOAD_RECV_WORKERS`, default 2, at most the spare cores; 0 is
the inline path). Every protocol decision stays on the receiving thread, in
stream order: the credit check, the index, offset, size and manifest checks,
the stage, `End`, `publish` and `Held`. Within one chunk the refusal order is
fixed (its verify or size, then the stage, then the write, then the kick),
the least chunk index wins, and the chunk that stages a streamed file is
verified on the receiving thread first, so a corrupt first chunk stages
nothing: the refusal is the inline path's. An entry's `End` waits for all
its writes before the file is checked, published or discarded, so every
write precedes its file's seal. The workers hold at most 8 MiB, each job
charged at least 4 KiB; a worker that panics answers its job as lost and the
session ends `WORKER_LOST`.

Directory batching (S1, OI-1003-Q143 item 2, R-N102). New directories are
created a level at a time: a tagged `mkdirat` each, one seal per distinct
parent, one store commit binding every record of the level, an exclusive
rename each, one seal per distinct parent again; finishing applies and seals
every final mode deepest first, then completes them all in one commit. Each
directory keeps its own order (entry sealed before its record, record
before its name, name sealed before anything inside it is decided, mode
sealed before its completion), so each recovers on its own, whatever the
others' state. The walk is depth-first, so batching siblings defers their
decisions: from the first new directory on, every walk frame is held, in
stream order, until `WalkDone`, 64 members (fewer under a small descriptor
budget: a level holds one descriptor per distinct parent) or 256 held
frames, then the batch is created and every held frame decided as it would
have been. A directory joins only when its parent is the root, a member or
a directory decided before; one under a held, undecided directory (an
existing one, whose sweep and adoption seal must come first) closes the
batch before it is looked at, and so does a directory whose path the batch
already holds (a source offering it twice), so no two members share a
record's key. A member refused at its rename (a third party's directory
appeared there) leaves its children to be decided against the file system,
as without batching. A discarded creation clears its record before its
temporary goes (#74 N3), a level whose shared seal or commit failed
included (its commit may have reached the log although it reported a
failure), and every clear is traced apart from a completion
(`DirectoryCleared`). Until R-C843 deleted it on 2026-10-17,
`docs/formal/DirectoryRecords.tla` checked the records under per-name
power loss, journal seals and reused inode numbers (`git show e3522007:docs/formal/README.md`,
"Directory records").

The finish rides the session's last output group (OI-1003-Q154 lever 3;
the SOTA study's lever 2, R-N102's finish commit unit; built 2026-10-16).
When the stream drains (every entry offered and ended), the receiving side
sets every final mode, deepest first, checking each directory's identity
as the finish does, and hands the directories to the committer with that
last group. The group renames its outputs, seals them together with the
directories (the device seal of a batched group covers every directory's
mode on its device; otherwise each directory joins the touched set and is
sealed, and fully flushed off the store's device, as the finish seals it),
and its one FULL commit records the outputs and completes the directories:
no seal of each directory and no completion commit of its own. On corpus v1
that removes four directory seals and one FULL commit from the tail
(expected on the rig, not measured there: `sqlite_directory_complete_commits`
1 to 0, `flush_dir_barrier_count` 8 to 4, `directories_folded` 0 to 4). The
order per directory is the finish's: final mode set, then sealed, then
completed. It folds only when nothing after that group can refuse: nothing
refused so far on either side (the committer checks every outcome it has
reported), no `SQLite` sidecar without its snapshot base, no salvaged
temporary left to retire, batching on and at most a batch of directories
(each held open into the group), and no final mode that takes away this
user's read, write or search permission (the group's renames need write and
search; the separate finish after a group that carried nothing, and the
next run's adoption after a group that refused, open each directory
read-only and the adoption lists it, which needs read). A group that
refused an output, or whose seal or commit fails, completes nothing, and
the session leaves every directory unfinished, as a session that refuses
does; the final modes it set stay (owner read, write and search by
construction), and the next run adopts each directory by its record,
which names that mode. A drain whose group is
already empty (the last output closed it at a file or byte bound, or an
idle or age bound closed it first) drops the finish, which then runs as
before over the modes already set. `BULKLOAD_DIRECTORY_FOLD=0`
restores the separate finish for A/B; `directories_folded` counts the
directories finished in a group. The model is `DirectoryRecords.tla`'s (deleted 2026-10-17, R-C843)
`Place` and `GroupFinish` with `CompletionAfterChildren` (a completed
directory owes no output of its run); the power-loss proof is
`every_power_loss_state_of_a_folded_finish_is_explored`, in group and
strict mode, with the final modes moved after the seals as its teeth.
The pending side cannot fold: a directory's record must be durable before
its name (R-N102), and its name before anything inside it is decided, so
that commit is the session's first durable step and has no output group to
ride.

The two stores are opened at once (S1, OI-1003-Q143 item 1b): the
destination sends `Open`, with its whole credit window, before it opens its
own store, so the source opens its store meanwhile. Every overlap of a state
root with the other state root or either root is refused before either store
exists.

The source ledger's rows are the one exception (WP0(g): OI-1003-Q20,
adopted with conditions by OI-1003-Q37, built 2026-10-07 on OI-1003-Q104).
The ledger is a cache of what the source already holds: a seat's row key
and its chunk manifest, or a remembered refusal. Its ROW commits run
`synchronous=NORMAL`, `fullfsync=OFF` (`LedgerSync::Relaxed`, the default;
`io::durable::relax_ledger_rows` on the source publisher's connection
only), so a power loss may roll back the newest of them. A lost row costs at
most one more read of its seat, and only where the destination no longer
holds that seat: a seat the destination holds is answered `Reuse` from the
destination's own committed row, and the ledger is never asked (R25,
OI-1003-Q40). A row commit that fails is counted
(`source_ledger_commit_failed`) and the transfer goes on (#163); a ledger
read that fails is a miss (`source_ledger_unreadable`). Nothing else is
relaxed: the commit that creates a store, with its authority, and the
state-root seals (#161) are fully synced on both sides, every destination
commit is `synchronous=FULL`, `fullfsync=ON`, and a checkpoint on the
relaxed connection is still a full flush. `--source-ledger-sync=full` syncs
every row commit and fails the session on the first failed one, for
comparison. Until R-C843 deleted them on 2026-10-17, the `BulkloadTransfer.tla`
configs checked the relaxed ledger (`MC_wp0g`, `MC_wp0g_deep`,
`MC_wp0g_strict`) and showed a relaxed authority breaking R25
(`MC_wp0g_authority`); P79 and `tests/power_loss.rs` take the real store
through every loss.

Superseding publish (WP0(d), OI-1003-Q18, #187). A changed seat's new bytes
replace the output at its path only when that output is this store's own,
untouched: its `(dev, ino, size, mtime, ctime)` equals a row this store
committed for the path. Any other file there is refused
`DESTINATION_OCCUPIED` and left as it is. The replacement is the exchange
design the formal model checked until R-C843 deleted it (`MC_wp0d_exchange`): the group's commit seals
the staged file, records an intent that takes the output's rows out of the
store, trades the staged name and the leaf in one `RENAME_EXCHANGE`
(`RENAME_SWAP` on Darwin), checks the displaced file, removes it when it is
the old output or exchanges it back when it is anyone else's, seals the
directory, and commits the new row with the intent settled. A power loss
leaves the old output or the new one, whole, and never a row beside other
bytes; the next sweep gives an old output still in place its rows back,
removes a displaced old output, and exchanges a displaced file of anyone
else back while the leaf holds the staged file unwritten since it was
staged (its inode, size and mtime as the intent recorded); otherwise it
keeps the displaced file aside and reports it, so writes made to the new
file after the crash are never deleted. The new file is filled from
the old output's own chunks, so only absent chunks cross the wire (WP0(c),
inequality 2). A `SQLite` snapshot output (#218) is superseded the same
way (OI-1003-Q146), with one more look: no `-wal`, `-journal` or `-shm`
beside it at Decide and immediately before the exchange ("SQLite
snapshot seats", Wire v6); `docs/formal/SqliteCarry.tla` (deleted 2026-10-17, R-C843) checks it
(`SqliteNoClobber`, `SqliteOldOrNewWhole`, `SqliteNoForeignSidecar`).

What "this store's own" covers (#187 review, 2026-10-07). An output has a
reuse row under its seat's row key, which answers `Reuse`, or an ownership
row under its path alone, which names no seat and answers nothing but "this
store published the file with this identity here". An ownership row is
written where there is no reuse row:

- for an output published or adopted from a racy capture (#86; ruled as
  built, OI-1003-Q101: an ownership row and no reuse row). It is read
  again on every run until its seat settles, and when the seat changes
  first, as an actively written file does, it is superseded;
- by the sweep, for a superseding publish whose exchange took effect and
  whose row never committed (a crash, or a failed group commit). The
  publish's record names the staged file by inode, size and mtime; when the
  leaf holds exactly that file it is this store's own. It is adopted from
  its capture record (#169) while its seat is unchanged, and superseded
  when the seat has changed again. A file written since gets no row.

A file system without an atomic exchange (one the first publish reaches
through its link fallback: NFS, SMB, exFAT) cannot supersede. The first
changed seat on a device probes for the exchange (two empty temporaries,
once per device and session). Without it the seat is refused
`DESTINATION_EXCHANGE_UNSUPPORTED` as soon as its manifest shows the output
holds other bytes: nothing is staged, no chunk is asked of the source, and
the old output keeps its row. Such a seat does not converge there. That is
the ruled behaviour (OI-1003-Q100): the refusal stands, and there is no
fallback to a rename over the output, which is the check-then-rename shape
the formal model refuted (deleted 2026-10-17, R-C843).

Refusals the destination remembers (#187 review, R25). A seat refused
`DESTINATION_OCCUPIED` or `DESTINATION_EXCHANGE_UNSUPPORTED` after its
manifest was read is not read again while nothing has changed. The
destination store records the refusal under the entry's row key (the
seat's path and stat identity) with the stat identity of the file at the
path, and answers `Decision::Refuse` with the same code when the entry is
offered again and that file still has that identity, so the source opens
nothing. It is recorded only when the capture was not racy and the file
was settled: the same before and after it was read, and not stamped within
the racy window of the read. A changed seat, a changed file, or a file
system that has gained the exchange makes the record a miss. The record
is a memo about no durable bytes; losing it costs one more source read.

Grown destination files (M2r2 A-103, 2026-10-18; review round the same
day). A landing moved by hand orphans its destination store: every file
there is then one the store does not own. An append-only transcript that
grew on the source since is a byte prefix of its seat, and was refused
`DESTINATION_OCCUPIED` (310 of them on 2026-10-17, with
`files_materialized=0`). Such a file is now extended in place by its tail:

- **Scope.** `BULKLOAD_PREFIX_EXTEND` unset (the default): append-only
  files only, a path ending `.jsonl` or `.txt`, as `estate-reseat` extends;
  `all`: any regular file; `0`: none.
- **Which files.** In scope, an existing regular file at the path with no
  owning record (no reuse or ownership row), one link (a second name would
  change with it), this user's, the source's mode, `0 < length < size`,
  that this user can open for appending (a read-only or immutable file is
  refused as before, nothing fetched), and settled: its mtime and ctime
  older than the racy window by the destination's clock, so no live writer
  is at it (a file written just now is refused as before, not remembered,
  and tried again once it is quiet). It must hold at least one whole chunk
  of the manifest: a file whose held bytes all lie in the first chunk is
  refused too, since judging it would take that chunk over the wire for a
  seat that may be refused (P21, #187). Any other file is adopted when it
  is the seat's bytes and refused `DESTINATION_OCCUPIED` otherwise, as
  before. A `SQLite` snapshot's output is never extended.
- **The proof.** Every manifest chunk that ends within the file's length is
  read back from the file and must hash to its digest; the bytes are read
  once (`read_dest_verify_existing_bytes`), and a failed proof refuses at
  the plan with no chunk asked of the source and no byte written. The
  rest, from the first chunk that does not end within the file, is staged
  in a temporary of its own and filled as any staged file is (chunks held
  here, then the wire), so only the tail and the chunk it starts in cross.
  Before anything is appended, the part of that chunk the file already
  holds is compared with the staged bytes; a difference refuses with
  nothing written, after that chunk and the tail crossed. A seat streamed
  whole in place of a manifest (a source that could not keep its chunks)
  is extended the same way from its staged copy, its whole prefix
  compared: on that path a prefix inside the first chunk is extended too,
  the wire having been spent already.
- **The append.** The file must be the one the plan proved, untouched
  since: the same `(dev, ino, size, mtime, ctime)` by the descriptor the
  plan read and by the append descriptor the plan opened (no link
  followed). An intent row (path, prior length, BLAKE3 of the prior bytes,
  prior identity) is committed, then the tail is appended through an
  `O_APPEND` descriptor in 1 MiB pieces, the file's size checked before
  and after every piece, as `estate-reseat` extends: a byte a live writer
  appended first is never written over (an `O_APPEND` write lands after
  it), and the entry is refused `DESTINATION_OCCUPIED` with every writer's
  bytes kept. The window left is a writer that appends between a size
  check and the write of one piece: both writers' bytes are kept, the size
  after the piece is not ours, and the entry is refused. The file is then
  flushed (`F_FULLFSYNC` on Darwin), its size must be the seat's, and the
  straddling chunk is re-hashed against its digest as the file now holds
  it, a second check of the head comparison.
- **The record.** The extended file is adopted: its capture record (#169)
  and its row are written as an adoption's, and the group commit seals it
  (its own sync, or the device seal) and its directory before the commit;
  that seal is what makes the row safe, and the extension's flush is
  defence in depth (`tests/power_loss.rs`). The row's chunk hints are the
  whole file's. In the same transaction a `prefix_extensions` row records
  the path, the prior length, the BLAKE3 of the prior bytes and the
  identity after, and the intent is settled. A file is counted
  (`dest_prefix_extended`) and reported (`prefix-extended PATH from=PRIOR
  to=SIZE prefix_blake3=HEX`) only once its record committed;
  `write_dest_prefix_append_bytes` counts the bytes appended, a subset of
  `write_dest_materialize_bytes`. From then on the file is this store's:
  an unchanged rerun reuses it, and a later growth supersedes it as any own
  output.
- **Remembered refusals.** A failed proof, at the plan or in the head
  comparison, is remembered `NotPrefix` under the identity the plan found
  settled, which is honoured: an unchanged rerun reads and sends nothing
  for it (R25, P21). A refusal remembered `Occupied` before the extension
  existed is not honoured for a file the extension may try: it is proved
  once more. A refusal for another reason (the file moved, a writer
  appended, an I/O error) is not remembered.
- **Crashes.** `receive.prefix_before_append`, `receive.prefix_after_append`
  and `receive.prefix_after_flush` (and the group commit's points after
  them) leave every file a byte prefix of its seat (the old one, a longer
  one, or all of it) and the staged tail aside for the sweep; the rerun
  extends or adopts what it finds. A rerun that adopts the whole file with
  an intent recorded for its path (a crash after the flush) records its
  `prefix_extensions` row from the intent, once the intent's prior bytes
  are proven to be the file's first ones. Power loss: the checker holds
  every committed record to its bytes and the file to a prefix keeping
  every old byte in every state of the traced run. Not covered by it: a
  file system that persists a size before the data it covers can leave
  zeros past the old length; that file is no longer a prefix and is
  refused `DESTINATION_OCCUPIED`, its old bytes intact, and the refusal is
  reported `prefix-extension-interrupted PATH prior=N` from the unsettled
  intent, so the operator knows the length the file was proven to hold
  (what follows was appended since, by the crashed run or a writer).

Re-keying a moved landing is not built. A destination store keys its rows
by path relative to the destination root and records each output's
`(dev, ino, size, mtime, ctime)`; it does not record the root's own path.
After a true rename within one file system (`mv` on one mount), the files
keep their inodes and stamps, so the operator passes the old DEST_STATE
with the new destination, with the same SOURCE_STATE and the same
destination-relative layout, and every unchanged file is reused as before
(the moved root directory is decided again). Two live landings must never
share one store. A copy (`cp`, or `mv` across mounts) makes new inodes: no
record could prove those files are the ones it describes without reading
them, so they fall back to adoption by content, which is safe; that is
what this extension does with a fresh store. The 2026-10-16 move used a
copy, so reusing the old store would not have helped it: the extension is
the tool for that case.

The formal model held all of this from 2026-10-07 until R-C843 deleted it on 2026-10-17 (OI-1003-Q102: model
first, then merge): the intent and its sweep, the ownership row, the
remembered refusal and the refusal without an exchange, with the
invariants `NoClobber`, `SupersedeAtomic`, `OwnershipNeverReuse`,
`RememberedRefusalSound` and `ExchangeRefusedUpFront`
(`docs/formal/README.md`, "#187's records in the model" (deleted 2026-10-17, R-C843)).

A store's state root and its database entry are sealed (the root fully
flushed) before `Store::open` returns, so before Start and any commit, and a
store without the `root_sealed` setting is sealed again on open (#161, R25).
The setting commits only after the seal returns. A sealed store reopens
without opening the root's parent; a root that still needs its seal under a
parent the agent cannot read refuses with `IO` (`EACCES`). Other private
state directories seal their own entry before records go in.

Space preflight (OI-1001-Q2): a write that would leave its destination
filesystem (`statvfs`; `statfs` on Darwin) with less than
`--min-free-percent` free (default 25%; 0 still refuses a write larger than
the space available) refuses `DESTINATION_SPACE_INSUFFICIENT` before it
starts.

Two limits (M2 round 2, item 1, 2026-10-17). On XFS (and ext4) with project
quotas, `statvfs` of a directory inside a project-quota tree reports the
project's limit and use, not the filesystem's: on sting, `/srv/fast-local/jess`
read 74 GB free under its quota against 174 GB on the filesystem, and every
check refused with no hint why. The probe now reads the path's view and, on
Linux, the view at the root of the same filesystem (the mount of the path's
device whose root is `/`, from `/proc/self/mountinfo`). When the two report
different sizes, the path's view is a project quota and the root's is the
filesystem. **The floor is a percent of each limit's own size, applied to
each:** the write must leave `--min-free-percent` of the quota's limit free
under the quota, and the same percent of the filesystem's size free on the
filesystem. Where several destinations share a filesystem, the filesystem
is judged on all their bytes and each quota on its own tree's. A refusal
names the limit that bound (`bound=filesystem`, `project-quota` or `both`)
and both limits' numbers: the verb's `space_refusal` line beside its
counters (`planned_bytes`, `filesystem_planned_bytes`, `min_free_percent`,
`filesystem_total_bytes`, `filesystem_available_bytes`, `quota=none` or
`quota=project` with `quota_total_bytes` and `quota_available_bytes`, then
`path`; the process's last refused check), repeated once after a verb that
ends `DESTINATION_SPACE_INSUFFICIENT` as `bulkload-agent: space: ...`; the
counters `space_checks`, `space_refused_filesystem` and
`space_refused_quota` count every check and every refusal by limit. A
`DESTINATION_SPACE_INSUFFICIENT` converted from a write's `ENOSPC` or
`EDQUOT` (a Git child, a store's `SQLITE_FULL`) was judged by no limit and
prints no `space:` line. Only a smaller size at the path is a quota, and
the root must still resolve to the path's device when it is read (a mount
stacked on it hides it). Where the root cannot be found or read (no
mountinfo, a device mounted only below its root or hidden, Darwin), the
path's view is the filesystem's, as before. Quotas of one size on one
device are summed together and judged by the least room any of them
reported. Known limits of the labels: XFS reports a quota's free space as
the lesser of the quota's and the filesystem's, so a full filesystem can
also read as its quota binding (`both`); a quota with only a soft limit,
or a realtime-inherit directory, reads as XFS reports it.

- `copy` and `pull` decide it per entry on wire v6. An entry decided
  `Send` or `WantManifest` reserves its size until its `Held`. Each
  admission checks every reserved byte against a probe that is refreshed on
  each group commit and every 256 MiB admitted. An entry that does not fit
  is answered `Decision::Refuse` with that code: no new frame, the session
  continues, and a later run resumes it without re-reading durable bytes
  (R25).
- `estate-apply` checks before any item runs. Each pending item charges
  its repository the bundles (the capture, its chain links and plan bases)
  the destination repository does not already hold whole, by the same test
  the import makes before it takes anything, and each bundle once per
  repository across the plan (M2 round 2, item 1: a re-captured item over
  a landed one, an adopted repository and a repository's other worktrees
  charged their whole chains again). The checkout is charged apart from
  the objects, to the workspace (a standalone's is its repository):
  nothing when the workspace ledger records this capture landed; the
  capture's own bundle when it records an earlier capture (a converge
  moves the checkout forward); the whole chain when no checkout is
  recorded (a first landing, or one a crash cut after its import or its
  `worktree add`). The corpus is charged twice the `jobs` largest pending
  bundles, for bundle and base staging. Bundle bytes are compressed, so
  the charge is a lower bound, with stated margins it does not cover: a
  checkout's expansion past its bundle, a pack's index files and a thin
  pack's completion, a linked worktree's index, and a chain's links staged
  beside its head. An item the plan cannot read, or whose destination
  cannot be stat'ed or probed, is skipped and refuses on its own.
- `estate-capture` checks per item (#101, OI-1002-Q11). Before an item's
  export writes, its estimated bundle (the larger of its census's
  regular-file bytes and the retained bundle it extends) plus every byte
  still reserved by in-flight items is checked against a fresh probe of
  CORPUS. An item that does not fit refuses
  `DESTINATION_SPACE_INSUFFICIENT` as its own receipt; the other items run
  and the next pass retries it. A reuse hit writes nothing and reserves
  nothing. Shared-base preparation is not charged.
- No preflight charges `TMPDIR`. A capture's own temporaries there (a
  private copy of the source index) and a stage beside a bundle that meet
  `ENOSPC` or `EDQUOT` (at the create or a write) refuse `SPACE_EXHAUSTED`
  with the directory that ran out and outlives the refusal (TMPDIR, or the
  directory a stage's private directory was made in), never the removed
  private directory itself (S4, #220). A Git child that reports no space
  names no directory (R-N121); its verb does. estate-apply's children write
  into the destination repository and its corpus stages, which its
  preflight charges, so they refuse `DESTINATION_SPACE_INSUFFICIENT`.
  estate-capture's children write only under PRIVATE_STATE, which nothing
  charges, so they refuse `SPACE_EXHAUSTED` naming it. A child of another
  verb (the estimate probe) refuses `SPACE_EXHAUSTED` with no path. The item refuses as its own receipt and the pass goes
  on; a pass with room retries it.
- A store out of space (`SQLITE_FULL`, or `SQLITE_IOERR_*` whose errno is
  `ENOSPC` or `EDQUOT`: a quota, or a full disk met at a sync) is a space
  refusal, never a bare `IO` and never the store's corruption (S4, #126). A
  destination group commit, which the transfer's preflight charges,
  refuses `DESTINATION_SPACE_INSUFFICIENT`. The source ledger's commit and
  `Store::open` on either side refuse `SPACE_EXHAUSTED` naming the state
  directory: no preflight charges it, and a source-side full disk is never
  reported as the destination's. Any other I/O error a store meets is `IO`
  with its errno (`EIO` where the errno cannot be read).

## Re-pointable captures: `estate-retarget` (M2 item 5, 2026-10-16)

An item's identity is the blake3 of `(source, repository, workspace)`, so
a changed destination made a new item whose capture record
(`{item}.capture`) did not exist, and the next capture exported the whole
repository again. Nothing in a capture depends on the destination: its key
is the source's (`capture_key_parts`), its bundle is named in the record as
an opaque file name, and every sidecar (`.base`, `.prior`, `.drift`,
`.nested`, `.parts`, `.reuse`) hangs off that bundle name; plan-base groups
are keyed by the source's common directory. `estate-retarget PLAN CORPUS
--map OLD=NEW [--map ...]` therefore re-points without a source read:

1. under the plan lock, each item's repository and workspace take the
   longest `OLD` that is a component-wise prefix (`NEW` replaces it); the
   source never changes;
2. the new plan is checked as `estate-add-batch` checks one (no two
   workspace targets overlap, R-N114; two items that become one refuse
   `GIT_DESTINATION_OCCUPIED`), before anything is written;
3. each moved item's record is written under its new identity with the
   old record's bytes (temporary, fsync, rename, directory fsync); one
   already there with the same bytes is kept, one with other bytes (that
   identity's own capture, e.g. a plan re-pointed back after a re-capture)
   refuses the whole retarget `RECEIPT_BINDING_INVALID`: capture again
   first, or set that record aside by hand;
4. the plan as it was is kept as `PLAN.before-retarget-<digest>`, then the
   new plan replaces it.

It prints one line per moved item (`item= was= ... capture=rekeyed|present|
absent`). Refused before anything is read: a CORPUS that is not a
directory, two maps with the same `OLD`, and any `NEW` at or under any
`OLD` (a rerun would move the items again). A crash before step 4 leaves
new records nothing reads; the rerun finishes, and a rerun after it
changes nothing. The plan lock serialises plan writers only: not to be run
beside a capture or apply of the plan. On a plan already applied, the
moved items are new: closure counts them unaccounted until applied at the
new paths (a new landing, or an `--adopt` re-key of a tree moved there by
hand), and the old landing stays; like any plan edit, it changes the
plan's digest, so a dispositions ledger or transfer manifest bound to it
refuses `RECEIPT_BINDING_INVALID`. Nothing is deleted.
The next `estate-capture` of the new plan is a reuse hit
(`capture-reused-after-census`, 0 source bytes) and the apply lands at the
new paths. Not re-keyed: apply STATE (journals and outcome records belong
to the old destination; a tree moved there by hand is re-keyed by
`estate-apply --adopt`, A11b) and the capture's private STATE (per-export
attempt directories; the next capture writes the new outcome records).

## A symlinked live path into a side root: `--side-root` (M2r2 item e, 2026-10-17)

The 10-16 in-place move met live checkout paths that were symlinks into a
side root: `~/git/<name> -> estate/gftb-20261015/git/<name>`, the trees an
earlier estate landing made. `estate-apply --adopt` refuses a checkout
spelled through a symlink (`GIT_DESTINATION_OCCUPIED`, `estate-diff`
`symlinked-path`; P97, P98), so the lane replaced each link with a `cp -a`
of its target and ran `git worktree repair` by hand
(`gftb-unlink-run.sh`, 29 links). `--side-root ROOT` (repeatable) on
`estate-apply --adopt` and `estate-diff` does it natively:

- **Which links.** For each planned repository and workspace path, the
  first symlink at or above it (from the root down). It is this verb's only
  when it names a path inside a side root; a link elsewhere is left alone
  (its items refuse as before). It is seated only when its single hop names
  a real directory strictly inside the root: the link's text, joined to the
  link's directory, is already in lexical form (no `.` or `..`) and is
  exactly the path the system resolves (no other link on the way); the
  link's own parent is a real path; the target is neither at, inside nor
  above another such link's target or the real path of any planned path
  not spelled through this link (two links into nested trees, a
  `.worktrees/` directory inside a main checkout reached by its own link,
  an item planned at the side root: all refused); and both are on one file
  system. Otherwise the link is refused (`PATH_ESCAPES_ROOT`,
  `GIT_DESTINATION_OCCUPIED`, or `IO` with `EXDEV`) and no path is changed.
  Each link is checked again right before its seat.
- **When.** After the apply's preflights (the space floor) and before any
  item, under the STATE lock and an `flock` on both parent directories (two
  applies or undos on different STATEs never move one live path at once;
  the second refuses `IO` `EWOULDBLOCK`). `--side-root` needs `--adopt`
  (`FIELD_DOMAIN_VIOLATION` otherwise): a seated tree is an existing
  checkout. A seat stays when its items then refuse; `estate-unseat` undoes
  it.
- **The seat.** A record `seat-<blake3(LIVE)>.seat` in STATE (live path,
  target, link text, the directory's device and inode, the plan, phase
  `seating`), durable first; one `renameat2(RENAME_EXCHANGE)`
  (`renameatx_np(RENAME_SWAP)` on Darwin) of the two names, so the live
  path holds the directory itself (the same inode: no byte copied, read or
  written, every `lstat` field below it kept; only the moved directory's
  own ctime changes) and the target the old link. A swap that fails with
  both names unchanged (no `RENAME_EXCHANGE` on NFS or FUSE, `ENOTDIR`)
  removes its record again. Then at once, before either parent is synced,
  the target's link is replaced by one naming the live path (made under a
  fixed temporary name, swapped in by a second exchange, the old link
  unlinked), so every path that still names the side root (a linked
  worktree's `gitdir` file, its admin directory's backlink, another tool's
  configuration) resolves to the same tree; then both parents are synced.
  The swap is atomic for the live spelling; between the two exchanges the
  side-root spelling names the old link. The apply prints `seat live=
  target= outcome=seated|seat-resumed|unseated|refused reason=`.
- **Then the items.** They adopt (A11a) or re-key (A11b) at the live path,
  as at any real directory: the adopt proofs compare canonical paths, so
  worktree links that name the side root still prove.
- **Crash.** The record's phase and what the two paths hold name every
  intermediate state of a seat (recorded, swapped, relinked) and of an
  undo (marked, swapped back, relinked, record not yet removed). Every
  `estate-apply` on the STATE, with or without `--side-root`, first
  finishes each record by its phase; a record whose paths hold none of its
  states is a `refused` line (`RECEIPT_BINDING_INVALID`) and the apply goes
  on, its paths left as they are.
- **Undo.** `estate-unseat PRIVATE_STATE [LIVE ...]` marks the record
  `unseating` (durable), swaps back, restores the live link with its own
  text and removes the record (`outcome=unseated plan=`); a tree replaced
  or a link rewritten since refuses `RECEIPT_BINDING_INVALID` before the
  mark, changing no path. An undo after the items re-keyed leaves their
  ledgers keyed to the live plan.
- **Dry run.** `estate-diff --side-root ROOT` prints one `kind:"seat"` row
  per link (`seatable` or `refused` with its code) and answers each item
  spelled through a seatable link at the directory it names (the tree the
  live path holds once seated), with the component `side-root-seat`.
  Read-only.
- **Against the copy `cp -a` made (operator decision).** No second tree,
  no disk, no byte to re-prove, and the live spelling changes atomically.
  The side root does change: each seated target becomes a link back to the
  live path, where `cp -a` left the side root untouched and duplicated it.
  No `git worktree repair` runs: the admin files keep the side-root
  spelling, which the link back resolves, so `git worktree list` shows the
  side-root paths and removing the side root (its links back) would break
  those worktrees. Repairing them (a recorded write of `.git` and `gitdir`
  files) is not built.
- Tests: `estate::seat` unit tests (exact undo; every crash step of a seat
  and of an undo, never re-seated; a failed swap leaves no record; a record
  resume cannot finish is a line; a held parent refuses; hop rules; nested
  targets and planned paths inside a target; a target the plan names);
  P120 (a main and a linked worktrees directory landed into a side root,
  seated and re-keyed, closure passing, unseated); P121 (single hop only,
  `--side-root` without `--adopt` refused).

## estate-move (M2 item 7, design; not built)

> 2026-10-18: built as `estate-move-source` (source half) and `estate-move`
> (destination half) plus `estate-apply --refs-in`, superseding "not
> built" above; what was built and where it differs from this sketch is in
> `docs/agent-notes/2026-10-18-estate-move.md` (A-20261018-110, R-N13).

Design: `docs/agent-notes/2026-10-16-estate-move-design.md`. One verb on
the destination, `estate-move [--plan-only] [--date YYYYMMDD] [--map
SRC=LIVE]... SOURCE_HOST SOURCE_ROOT... MOVE_STATE`, that does what the
2026-10-16 in-place lane scripts did by hand, each phase resumable and
recorded in `MOVE_STATE/move.ledger` only after what it describes is
durable:

1. inventory of the source roots (the remote estimate probe, read-only);
2. a presence probe of every live path (absent, git main/linked/bare,
   symlink resolved to its real path, nongit, parent shape);
3. capture on the source at `--priority=auto` and a native pull of the
   corpus;
4. per-checkout classification from `estate-diff`: `absent`, `match`
   (identical, or extra source refs only), `diverged`, `nongit`;
5. a pure plan: land absent; adopt a match in place; a diverged main is
   adopted refs-only with the source checkout beside at
   `<path>.neo-YYYYMMDD`; absent worktrees land as new worktrees of the
   live repository, present diverged ones beside; a nongit live path puts
   the whole group beside; refused items are carried as files
   (`--plan-only` stops here);
6. `estate-apply --adopt` in levels;
7. refs-in: create-only `refs/heads/neo-YYYYMMDD/<name>` (and tags) for
   each source tip the live repository lacks or holds at another value,
   recorded in the ref ledger so no converge moves them;
8. carried-as-files for the refusals Git carry cannot take (never into a
   non-empty path);
9. session pulls, `estate-reseat`, `estate-handoff`;
10. a converge loop until a pass changes nothing, ending in closure with 0
    unaccounted.

The date suffix is fixed at the first plan, no live ref is moved or
deleted, no live checkout is switched, and nothing is deleted. Open
questions for the operator are in the design note (suffix naming for a
second source, beside worktrees linked or standalone, tags in refs-in, one
command over SSH or two halves).

## Installing the agent (M2 item 4, 2026-10-16)

A `bulkload-agent` built inside `nix develop` is dynamically linked
against store paths: on Linux its ELF interpreter and `RUNPATH` name the
devShell's glibc and gcc-lib in `/nix/store` (and, first, the devShell's
own `outputs/out/lib`), on Darwin its dylibs may. None of those is a GC
root, so `nix-collect-garbage` can delete them and the installed binary
then fails to start (2026-10-16: a GC deleted the glibc of
`~/.local/bin/bulkload-agent-5beb…` mid-run). `just install-agent DEST_DIR
NAME` builds a release binary and runs `scripts/install-agent.sh`, which
copies it beside `DEST_DIR/NAME`, drops `RUNPATH` entries outside
`/nix/store` (patchelf, where present), adds one indirect GC root per store
path the copy names under `${XDG_STATE_HOME:-~/.local/state}/bulkload/
gcroots` (`nix-store --add-root`, registered in
`/nix/var/nix/gcroots/auto`), publishes `NAME` with `link(2)` (never
replacing anything), writes `NAME.sha256` and `NAME.gcroots` (never over
existing ones), and runs the selftest. A failure before the publish removes
the roots it made. The roots outlive a deleted binary: remove its
`gcroots/NAME-*` with it.

The selftest says `interpreter <path> exists= nix-store=` and `gc roots
store-paths=N rooted=R unregistered=U registry=read|unreadable
unrooted=...`: a store path counts as rooted only when a symlink in
`$BULKLOAD_GCROOTS` (default that directory) resolves into it and Nix's
registry names that symlink, so a copied or hand-made symlink is
`unregistered`. `rooted` below `store-paths` is the warning before a GC; a
binary whose interpreter is already gone cannot run its selftest: rebuild
it. Not built: a fully static musl build (a flake output with a musl
toolchain; changing `flake.nix` moves the pinned digests of the
public-read guard), which would need no roots at all.

## Agent session re-seat (R0, TIN-6587)

SPEC R0 (draft `docs/SPEC.md`, PR #236) is the user outcome: after a run,
`claude --resume` / `--continue` and `codex resume` started in each lane's
destination working directory list and resume its sessions, and every
transcript holds the source's last pass byte for byte. A run lands the
source's `~/.claude` and `~/.codex` in its landing on the destination host,
where neither agent looks; `estate-reseat` re-seats them into the
destination's **live** agent state. The facts it rests on were measured
against the real binaries (Claude Code 2.1.299, codex-cli
0.163.0-alpha.11) in throwaway homes:
`docs/agent-notes/2026-10-16-reseat-investigation.md`; the two adversarial
reviews of the first build and their fixes:
`docs/agent-notes/2026-10-16-estate-reseat-review-fixes.md`; the two
re-reviews of `8a89f9a`, the operator's rulings OI-1003-Q198 (Linear
TIN-6587, comment `fbccc49b`) and their implementation:
`docs/agent-notes/2026-10-16-estate-reseat-rereview-fixes.md`.

```
bulkload-agent estate-reseat [--check] [--map SRC=DST]... [--lanes-from-plan]
    [--plan PLAN --corpus CORPUS --source LABEL [--private-state DIR]...]
    [--codex-source-root DIR]... [--landing-claude DIR] [--landing-codex DIR]
    [--unmapped=report|by-id] [--note-marker TEXT] [--sessions-only]
    [--waive ID]... [--waive-from FILE] [--retention-margin-days N]
    [--managed-settings FILE]... [--by-id DIR]... [--no-default-by-id]
    [--no-respell] [--no-regularize] [--replace-archived-dest ROOT]...
    [--rebind-landing OLD=NEW]... [--undo]
    [--no-claude] [--no-codex] SOURCE_HOME LANDING_HOME DEST_HOME STATE
```

**Inputs.** `SOURCE_HOME` names the source host's home (`/Users/jess`) for
the lane map; `LANDING_HOME` holds the run's landed `.claude` and `.codex`
(the `state_5.sqlite` there is the run's backup-API snapshot); `DEST_HOME`
holds the live `.claude` and `.codex`; `STATE/reseat.sqlite` is the ledger.
The source is never read (R25).

**One run at a time.** A pass holds an exclusive `flock` on each
destination agent root it writes (the live `.claude` and `.codex`
directories themselves, through their real paths, so nothing is added to
them) and on `STATE/reseat.lock`; a second run refuses `RESEAT_LOCKED`
before writing. Each ledger names its temporaries with its own token
(`.<name>.<token>.bulkload-reseat`), so two ledgers never share a
temporary even outside the lock (re-review of `8a89f9a`: two unlocked runs
published each other's half-written file).

**Refused before anything is written.** `DESTINATION_SPACE_INSUFFICIENT`:
the engine's free-space rule (`--min-free-percent`, default 25; a project
quota judged as well, see the space preflight) for the bytes the pass's
placements would write on each destination file system. It is meant as an
upper bound and is decided as the placement decides (M2 round 2, item 1):
nothing for a collision, a `Continued` row, or a kept row (`Diverged`,
`Occupied`, `Rewritten`, `Unreadable`) whose landed file and destination
are both as recorded; for an owned placement, nothing when its file is gone
or changed since (the destination's) or the landed file is unchanged, else
the new tail of an append-only file or the whole of any other; otherwise
the whole content where nothing is, the bytes past an append-only regular
copy or a symlinked transcript's target, and nothing over any other file
or node; a `.zst` at its frame's declared size.

**Refused mid-pass, per link.** A symlinked transcript's regularization is
not in that check (every link's target at once held whole passes back):
each link is judged as it is copied, with the bytes the placements were
admitted for still reserved beside it, so a regularization never takes
what the placements after it write. One that does not fit is its own
`space-refused` line (`DESTINATION_SPACE_INSUFFICIENT`), kept a link, and
retried by a later pass; the links before it stay regularized. Sting's live state is on a file system under the 25%
floor, so the live run names its floor explicitly. `RESEAT_LANDING_ABSENT`: a landed
`.claude` or `.codex` the run covers is not a directory (absent, or a
symlink to a source-only volume such as neo's `/Volumes/...` codex link);
`--no-claude` / `--no-codex` leave a tool out explicitly, never silently.
`RESEAT_RETENTION_UNSAFE`: see Claude. `RESEAT_INDEX_ABSENT`: the landing
has Codex threads and the destination has no `state_5.sqlite`; Codex's
first start would backfill the placed rollouts once, under their source
`cwd`, archived, and every later insert would then conflict. Every run and
check prints one `tool` line per tool (`present`/`checked`, `empty`,
`skipped`).

**Lanes.** A lane maps a source working directory to a destination one:
`--map SRC=DST` (longest prefix first), then the plan's items
(`--lanes-from-plan`), then the default, the same path under the home's
`git` directory. The destination path is resolved to its real path (through
its deepest existing ancestor when it does not exist yet), because both
agents key by `realpath(cwd)`: on sting `~/git/lab` is
`/srv/fast-local/jess/git/lab`. A Claude session's lane comes from its
transcript's first top-level `cwd`, checked against `project_key(cwd) ==`
the directory it was landed under; a Codex thread's from its row's `cwd`
(from its rollout's `session_meta` when it has no row). A session with no
lane is reported `RESEAT_LANE_UNMAPPED` (or, with `--unmapped=by-id`,
placed under its source key or `cwd`: resumable by ID only); one whose
`cwd` is missing or keys elsewhere is `RESEAT_LANE_UNRESOLVED`. A refused
session is ledgered with its recorded `cwd` (or none) and its landed
identity, so a rerun does not scan it again (S3). The operator's rulings
(OI-1003-Q198): A, a lane resumes at `~/git/<name>` (the default lane, at
its real path); B, gftb and GloriousFlywheel sessions follow sting's own
`~/git` links to their estate paths (the same `realpath`); C, sessions
recorded in the source home itself or under `/private/tmp` or `/tmp` are
placed resumable by ID by default (`--by-id DIR` adds a prefix,
`--no-default-by-id` drops these) and so are in no lane's check. Two landed
files for one destination path (two source keys mapped onto one destination
key, as `--map` of a worktree onto its checkout does) are a collision: the
first keeps the path, the other stays in the landing,
`RESEAT_LANE_COLLISION`, and later passes read neither.

**Claude.** `projects/<project_key(real dest cwd)>/<sid>.jsonl`, with the
key function read from the binary (every UTF-16 unit outside `[A-Za-z0-9]`
becomes `-`; over 200 units, cut and suffixed with a base-36 hash). The
transcript is placed as a regular file (the picker lists regular files
only; never a symlink, never a hardlink to the landing) with the landed
mode and modification time (`--continue` takes the newest). Only when the
transcript itself is placed, adopted, extended or unchanged do its sidecar
directory `<sid>/`, `file-history/<sid>/`, `tasks/<sid>/`,
`session-env/<sid>/`, `debug/<sid>.txt`, `todos/<sid>-*` and its plans
(`plans/<slug>.md`, `plans/<slug>-agent-*.md`, by the transcript's `slug`)
go with it; a transcript kept out (occupied by a symlink, diverged, handed
over) keeps its companions in the landing too. The source key's `memory/`
merges into the destination key's, and so does everything else under the
source key that is no session's (neo's `drafts/` and `vercel-plugin/`, an
orphaned sidecar directory; S4, Q185); a key none of whose sessions has a
lane keeps those in the landing, named by one `key` line with its file
count and code. A companion or memory note kept for any reason is a `file`
line with its code. `sessions/` and `shell-snapshots/`
are host-local and never carried; `history.jsonl` and `~/.claude.json`
stay in the landing (the picker reads neither). **Retention:** before
anything is written, the pass refuses `RESEAT_RETENTION_UNSAFE` unless
every file it would place (each transcript, sidecar and per-session file,
and the plans of a known slug) keeps its source mtime past now plus the
margin (`--retention-margin-days`, default 365) under the smallest
`cleanupPeriodDays` any source Claude Code merges sets: managed settings
(`/etc/claude-code/managed-settings.json` and its `.d/` drop-ins;
`/Library/Application Support/ClaudeCode/` on macOS), the user's
`settings.json` and `settings.local.json`, and each lane's
`.claude/settings.json` and `.claude/settings.local.json` (default 30 when
none sets it). Both hosts set 3650.

**Symlinked destination transcripts** (OI-1003-Q198 (2)). On sting 388 of
417 transcripts are symlinks into `/srv/data/jess/archive`, which the
picker never lists. Before placing anything, a pass replaces each
`projects/<key>/<name>.jsonl` symlink (in a real key directory) with a
regular-file copy of its target: a byte copy (no reflink: P84
SOURCE-NO-CLONE, OI-1003-Q165, forbids every remap call) with the target's
mode and modification time, sealed, ledgered (`regularized`: link text,
link, target and copy identities, and the BLAKE3 of the bytes copied) and
exchanged in atomically; the archive copy stays. It is race-safe against a
live session appending through the link (re-review of OI-1003-Q198): before
the swap the copy is read back from its name and must be the bytes read
(length and BLAKE3), and the target's identity (inode, size, modification
time, by descriptor and by path), the link's and the copy's must be the
ones read; the target is checked again right after the exchange. Nothing
that holds a session's bytes is removed or replaced unless its bytes are
known to be held elsewhere. On any mismatch that transcript is refused
(`RESEAT_REGULARIZE_RACED`) and both copies stay intact: written before the
swap, the link and the archive are untouched and the staged copy is kept
aside as `.<name>.<token>.kept-<inode>.bulkload-reseat` (no row: a later
pass tries again); written under the swap, an unchanged copy gets the link
put back over it and is kept aside the same way, and a copy written too
stays at the path with the archive untouched (`raced`). The line's `kept`
field names a kept copy; no pass removes one. One window stays: a session
that opened the link before the swap and writes after the last check
appends to the archive. A dangling link or one to a non-file is kept
(`DESTINATION_OCCUPIED`). A regularized copy that is a byte prefix of a
landed neo session is then extended like any other. `--no-regularize`
skips it.

**Codex.** Each thread of the landed snapshot, oldest first (so a fork's or
spawned thread's parent is registered before it). A row the destination's
index already holds for the thread that this ledger did not register is
never replaced: the same `cwd` and rollout path is adopted, anything else is
`RESEAT_INDEX_CONFLICT`, and no second rollout is placed beside the
destination's own. One change is ruled (OI-1003-Q198 (1)): a row whose
`cwd` is a non-canonical spelling of the lane (it resolves to the lane's
real path, as sting's `/home/jess/git/…` rows do through the `~/git`
symlink, which the cwd-filtered picker never matches) gets **only its `cwd`
column** re-spelled to the canonical path: the old value is ledgered
(`cwd_update`, `intent`) before a `BEGIN IMMEDIATE` transaction that sets
the column only while it still holds the old value, then marked `applied`;
a `row-update` line names both values. The row is then adopted when it
names this placement (its directory spelled any way that resolves to the
same one, as sting's `~/.codex` link does), and stays a conflict (nothing
placed beside it) when it names another file. `--no-respell` skips it.
Re-spelling is not limited to mapped lanes (operator M2 round 2,
2026-10-17): before any thread, and whenever the destination index exists
(landed threads or not), every destination row whose `cwd` is spelled
through the destination home's `git` symlink or a `--map` destination
that is a symlink gets the same ledgered, reversible re-spelling to the
real path (rebuilt from its components, so `//` and `./` drop), whether or
not a landed thread maps to it and whether or not the directory still
exists, but only when the nearest existing level of the old spelling
resolves to the same level of the new one (a row that reaches another
directory through a further link, or holds `..`, is never touched). A row
whose `id` or `cwd` is not UTF-8 text is skipped and counted
(`respell-not-text`). The links are read again before each batch of 256;
one retargeted meanwhile stops the sweep (`respell-stopped`,
`respell-link-moved`). The ledger keeps the value a row held when it was
last re-spelled, so `--undo` puts back the destination's latest spelling.
`--check` reports how many rows the sweep would re-spell
(`respell_pending` on its summary line); in-lane rows spelled through
another link that resolves to the lane are re-spelled per thread and not
counted there. An undone row is neither re-spelled nor counted, ever:
`--undo` is final for this STATE (a fresh STATE re-spells it). An
in-flight re-spelling a crash left uncommitted that `--undo` then finds at
its old value is marked `undone` too (the ledger cannot tell it from one
undone before a crash).

A thread the destination already holds is not a refusal (operator M2
round 2): when its row (with the lane's `cwd`) names this thread's rollout
in another form or place (Codex's `.jsonl.zst`, an archive's `.jsonl.gz`,
the same file name in another directory, or the plain file while a
compressed form sits beside it and the plain file is absent), nothing is
placed, and the thread line says `"proved"` true or false:

- `destination-adopted` (`exactly`, `continued`): the file the row names
  holds the landed stream's whole lines, exactly or followed by what the
  destination wrote, the R0 check's rule, and `--check` accepts that row
  (also in another directory). A landing with no whole line yet is held
  by any file, as in the check.
- `destination-holds`: a destination file of the thread is there but does
  not prove it: `unreadable-form` (a `.gz`), `not-regular` (the row names
  a symlink), `unreadable` (it cannot be opened or read: that thread's
  outcome, never the pass's), `row-file-absent` (the row's file is absent
  with a form of it beside, such as a `.zst` symlink into an archive),
  `size-differs`, `hash-differs`, `ends-mid-line`; and `rowless` (a
  compressed form with no row).
- `destination-row-only`: only a row: `absent` (the row's file is not
  there and no form of it is), `rollout-root` (the rollout is recorded
  outside every `--codex-source-root`) and `landing-absent` (its landed
  rollout is absent). Nothing landed is tied to it; it is reported, not
  held, and `--check` fails it.

Without a destination row, a thread recorded outside every root stays
`RESEAT_LANE_UNMAPPED` (line `row` and summary key `rollout-root`), which
`--codex-source-root` fixes. The proof is cached in the session row by the
landed file's identity (device, inode, size, mtime) and the destination
file's, so an unchanged rerun reads no landed byte. These words are
distinct from the placement words (`adopted` is a row or file this pass
placed or found equal). Its rollout is placed at the same path under the live
`.codex` **as plain `.jsonl`**: a landed `.jsonl.zst` is decompressed. Codex
0.163 resumes a paginated thread only from the plain file (it stats
`<name>.jsonl` and never opens the `.zst`: "failed to scan paginated model
context lineage"; reproduced on real sting threads and by the canary), and
one destination form keeps a thread one file when the source compresses a
rollout between passes (the same stream; the ledger follows the landed
form). The identity of a rollout is its decompressed stream. A `.jsonl.gz`
stays in the landing, `RESEAT_UNREADABLE_FORM`. Its row is inserted into
the live `state_5.sqlite` (`BEGIN IMMEDIATE`, busy timeout, batches of 256,
beside a running app-server) with `cwd` the lane's real destination
directory (the picker matches it exactly) and `rollout_path` the real
destination path; the source's project and section links are cleared; its
`thread_dynamic_tools` and `thread_spawn_edges` rows go with it. A
destination column the snapshot cannot fill refuses `SCHEMA_MISMATCH`.
Rows commit only after their rollouts are durable. A landed rollout no
snapshot row names (a thread created after the snapshot, or one Codex never
indexed) follows the same destination rule: a destination row or another
form of its rollout there (Codex's `.jsonl.zst`, a `.jsonl.gz`) is the
destination's, nothing placed (`destination-adopted` or
`destination-holds` as above,
`RESEAT_INDEX_CONFLICT` for a row naming another file; re-review of
`8a89f9a`, MAJOR: a
plain rollout placed beside the destination's continued `.zst` made a real
`codex resume` switch the thread to the older bytes). Otherwise it is
placed and gets a row built from its header (OI-1003-Q198 D):
`session_meta` (id, timestamp, `source` as Codex stores it, provider, CLI
version, originator, thread source, history mode, agent fields), the
first `turn_context` (approval, sandbox policy, model, effort) and the
user's first prompt (title, first message and preview, which the picker
needs), with the lane's `cwd`, read from the placed destination file. The
real Codex lists such a thread in the lane's own picker and resumes it by
ID with no working-directory question (canary). A header that cannot fill
a column the destination requires leaves it row-less,
`RESEAT_INDEX_ROW_ABSENT`; resumed by ID in its lane, such a thread must be
told "Use current directory": Codex's default keeps the session directory
(the source path) and writes it into a new row, which hides the thread
from the lane's picker and which `--check` names `row-cwd-is-source`. Another landed form of a thread the snapshot
names is an `extra-form` line. A truncated or corrupt `.zst` is that
file's `RESEAT_UNREADABLE_FORM` (no temporary left, not read again while
it is unchanged), never the run's refusal. The snapshot's
threads (identity columns) are cached in the ledger with its identity: a
pass over an unchanged snapshot does not open it.
`thread_history_1.sqlite` is a projection Codex rebuilds and is not
carried; `session_index.jsonl` stays in the landing.

**Never overwrite.** Per destination file `D` and landed file `L` (the
decompressed stream of a `.zst`). JSON Lines (`.jsonl`) are placed and
extended only up to the last newline of `L`: a line the source was still
writing is held back until it is whole, so `D` never ends inside a record a
destination resume would append to (a `.jsonl` with no whole line yet is
`held`, nothing placed).

| `D` | What the pass does | Report |
|---|---|---|
| a row for another landed file | nothing | `collision`, `RESEAT_LANE_COLLISION` |
| absent | copy into `.<name>.<token>.bulkload-reseat`, mode and mtime set, sealed, published with a no-replace rename, directory sealed, then the ledger row | `placed` |
| present, no row, equal to `L` | adopt | `adopted` |
| present, no row, a strict byte prefix of an append-only `L` (`.jsonl`, `.txt`) | adopt, then append the rest | `extended` |
| present, no row, anything else | keep; `L` stays beside it in the landing | `diverged`, `RESEAT_DEST_DIVERGED` (`dest-ahead`, `RESEAT_DEST_CONTINUED`, when `L` is a prefix of `D`) |
| a symlink or other node, a regular file with more than one link (an archive copy, the landing itself), or a symlinked directory above it | never written through | `occupied`, `DESTINATION_OCCUPIED` |
| this ledger's, both identities unchanged | nothing read | `unchanged` |
| this ledger's, `L` grew (append-only) | prove every placed byte by the BLAKE3 of all of them, then append the new whole lines with `O_APPEND` under an intent row, checking `D`'s size before every write | `extended` |
| this ledger's, a destination writer appended during that extension | stop between whole lines; the destination's from then on; mtime not reset. One window stays: an append between the size check and the `write` of the same chunk (no conditional append exists); it is detected after the write, handed over, and `--check` fails it, nothing lost | `extend-raced`, `RESEAT_EXTEND_RACED` |
| this ledger's, `L` changed (not append-only: a plan, a memory note) | stage, atomic exchange, then settle by the staged file's whole identity (device, inode, size, mtime): a displaced file that is not this ledger's placement is exchanged back; a staged file edited in place since is the destination's | `superseded` |
| this ledger's, `L` no longer extends the placed bytes (anywhere in them) | keep | `source-rewritten`, `RESEAT_SOURCE_REWRITTEN` |
| this ledger's, changed at the destination (a resume appends) | the destination owns it from now on, never extended again | `dest-continued` / `both-continued` / `dest-removed`, `RESEAT_DEST_CONTINUED` |

A destination resume appends to a session on either agent, so the first one
ends sync for that session; newer landed bytes stay in the landing. A
destination checkout that moves or becomes a symlink changes its key: the
earlier placement stays and the line names `previous_key`.

**Ledger and S3.** Every placement is a row keyed by its destination path,
naming its landed file: the landed file's identity at the last pass, the
destination file's identity after bulkload's last write, the bytes placed
and their BLAKE3, and an intent while an extension or supersede is in
flight. The ledger carries its schema version (`user_version` 5: a
version 2, 3 or 4 ledger is migrated in place, a version 2 one keeping
the shared temporary name for what it left in flight; version 4 adds the
replacement kind, so a build of version 3 refuses it rather than read a
replacement as a regularization; version 5 adds the rebind history and
memo, so a build of version 4 refuses it rather than ignore a rebind on a
pass or on `--undo`); the first build's ledger is refused
`SCHEMA_MISMATCH`. An unchanged rerun reads 0
landed bytes, the Codex snapshot and every refused, unreadable or
unmapped session included (`read_reseat_landing_bytes`, also
`write_reseat_dest_bytes` 0); a grown transcript is read once from the
landing, its placed bytes proven and its tail appended (the landing is the
destination's own copy; the source is never read, R25). A live lane is
caught up by repeated passes; the pass after its session stops on the
source leaves it equal to the landing.

**A new landing root** (`--rebind-landing OLD=NEW`, repeatable; A-106
follow-up, 2026-10-18). A second landing of the same home under a new
root (home2, then home3) collides with every placement the first one made
(`RESEAT_LANE_COLLISION`: the row names another landed file), even where
the bytes are the same. With the flag, a placement row naming a landed
file under OLD (as the rows spell it; OLD may be gone) whose path under
NEW (canonicalized; it must exist) is the file this pass places (the
same stream, another form of it such as `.jsonl.zst`, or the same real
path; every pair is tried) is re-bound instead of a collision when all of
these hold:

- the row is this STATE's own placement (`ours` or `adopted`, no intent;
  a `continued` row is the destination's and stays as it is) and no
  rebind of it was undone;
- the destination is a regular file with one link whose identity is the
  one the row last wrote;
- the bytes it placed are proved the new landed file's: its first
  `placed` content bytes (decompressed for a `.zst`) hash (BLAKE3) to the
  row's digest, or, with none recorded, to the destination's own first
  bytes read by descriptor with its identity checked before and after.

The new landed file exactly those bytes, and its identity the same before
the proof, at it and after it: `rebound` (a file and session word;
nothing written; the row's landed identity set to that identity). Longer,
or changed around the proof: re-bound with the old landed identity kept,
so the same pass proves the placed bytes and appends the rest as for any
grown landing (the session's word is then that placement's, `extended`;
`reseat_rebound` counts both kinds). Anything else stays a collision; a
refused proof is remembered by the new landed file's identity
(`rebind_unproved`, apart from the history) so it is not read again while
that file is unchanged. Never inferred from names or sizes. Each rebind
appends a row to an append-only history (`rebind_history`: old and new
landed file and identity, and the row's bytes, destination identity and
digest at the rebind) in the same commit as the placement row's update,
and only while that row still names the old landed file; a chained
rebind (home2 → home3 → home4) adds a row per hop. Nothing at the
destination changes, so no fault point is needed. `--undo` puts the
rebinds back newest first (ledger only), each only while the row still
holds exactly what that rebind left; one that moved on since (extended,
superseded, continued) is `undo-kept` and keeps its binding. A pass does
not re-bind a path whose rebind was undone. `--check` reports `rebound`
(rebinds done) and `rebind_pending` (this STATE's own rows under an old
root whose new-root file exists in either form, whose destination is
still the file they last wrote, and that were not refused unchanged; the
byte proof is the pass's); without `--rebind-landing` nothing is pending.

**Undo** (`--undo`, OI-1003-Q198). It reverses what this STATE's ledger
changed in the destination's own state under the rulings: each regularized
transcript gets its symlink back (a new link with the recorded text,
exchanged in, the copy removed) and each re-spelled row its old `cwd` (in
one `BEGIN IMMEDIATE` transaction, only while the row holds the value
re-seat wrote), each only where the destination still holds exactly what
re-seat wrote; one changed since (a resume appended to the copy, a pass
extended it, Codex rewrote the row) is kept, `undo-kept`,
`RESEAT_DEST_CONTINUED`. Before it puts a link back or removes a regular
file, it verifies the archive target the row's link names: the link text
must still resolve to the recorded target, which must be a regular file
holding the recorded bytes (size and BLAKE3), unchanged up to the swap;
otherwise that row is `undo-refused`, `RESEAT_UNDO_TARGET_CHANGED`, and
nothing changes for it (a row written before the digest was recorded is
refused the same way). An undone row or link is not changed again by a
later pass. Placed sessions are not removed.

**Archive links: the landed version wins** (`--replace-archived-dest
ROOT`, repeatable; operator ruling A-106, 2026-10-18: "neo's version
wins; fork stays in archive"). Without it a destination file that is a
symlink is never written through (`occupied`). With it, a destination
transcript, subagent sidecar (`projects/<key>/<sid>/…`) or Codex rollout
that is a symlink is replaced by its landed file when all of these hold:

- the link's fully resolved target (every hop followed by the kernel) is
  a regular file under the real path of a ROOT; only the final target is
  judged, so `..` or an intermediate link cannot bring a target under a
  root it is not under;
- the link's own directory does not resolve under a ROOT, and no ROOT
  equals, contains or is under the real destination `.claude` or
  `.codex` or contains the destination home: such a ROOT is refused
  `FIELD_DOMAIN_VIOLATION` before anything is read or written (the swap
  would otherwise happen inside the archive, or take the live state for
  one). A ROOT is canonicalized when the flag is parsed; one that does
  not exist is refused;
- this STATE has no regularization or replacement row for the path (a
  replaced or undone one is never replaced again by itself);
- no process this user can see holds the target open (every
  `/proc/<pid>/fd/*` link is read, observation only; a live session would
  go on writing into the archive after the swap): such a link stays
  `occupied` and is decided again next pass;
- the landed content (whole lines, as placed) is neither the target's
  bytes (BLAKE3: the link stays, `occupied`) nor a strict prefix of them
  (the archive ahead of the landing, not a fork: the link stays,
  `dest-ahead`, `RESEAT_DEST_CONTINUED`, as for a regular file that
  continued). The ruling replaces forks, not archives ahead; replacing
  those too is an operator call.

An equal or ahead link is recorded with the target's identity (in place
of the link's) and its relation, and is not read again while the
landing and the target are unchanged; a target that changes is decided
again.

The landed content is staged beside the link (`.<name>.<token>.bulkload-reseat`)
with the landed file's mode and mtime and sealed (`fsync`). The link text,
the target's real path, identity and BLAKE3, and the link's and the
copy's identities are ledgered (`regularized`, kind `replace`, `intent`)
before anything at the path changes. The target and the link are
verified unchanged and the target not held open, the copy and the link
are exchanged atomically (`RENAME_EXCHANGE`), the link under the
temporary name and the target's identity are verified again, the link is
removed and the directory sealed (`fsync`); then, in one commit, the row
is `done` and the placement row names the landed file (`ours`), replacing
any row another landing left for the path. Later passes extend it like
any placement. Outcome `replaced-archived` (a file word;
`reseat_archived_replaced` on the counters line). The archive target is
never opened for writing, renamed or removed. A link or target that
changed before the swap, a path that held something else, or a target
written under the swap puts everything back as it was (the staged copy
removed: the landing holds its bytes) and the file stays `occupied`. An
earlier pass's `occupied` row for the path, from another landing or
without the flag, is decided again instead of being a `collision` (it
placed nothing).

Crashes settle forward at the start of the next pass, whether or not it
regularizes: before the exchange the staged copy is removed (or, if it
changed, kept aside as `.<name>.<token>.kept-<inode>.bulkload-reseat`,
never removed by a pass) and the row dropped, and the next pass decides
again; after it the row is `done`, any row of the path that is not this
ledger's own is dropped in the same commit, the line is a `replace` line
`replaced-archived` (counted), and the pass adopts the file at the path
(the landed bytes). A target written under a crashed swap leaves either
the link back with the copy kept aside (`replace-raced-restored`) or both
kept (`replace-raced-kept`), as for a regularization.

`--undo` puts the exact link text back (not the link's own timestamps)
over an unchanged copy and a verified target (the regularization's rule;
a file a later pass extended is `undo-kept`, `RESEAT_DEST_CONTINUED`) and
drops the placement row, so no later pass replaces it again. `--undo`
reverses everything this STATE changed in the destination's own state:
its replacements, its regularizations and its re-spelled Codex rows
alike; undoing only some of them is an operator call (a separate STATE
per kind of change). Run a replacement, and its undo, with no live
session on the destination: the open-file check narrows, but cannot
close, the window between it and the swap.

Power loss: the copy is durable before the intent, the intent before
the exchange, and the directory before `done`; whichever name the
directory holds after a power loss is settled by its identity like a
crash. This ordering is argued, not tested: the fault harness models a
process crash only (`_exit`), and `estate-reseat` is not wired to the
`io::crash_check` power-loss model, so dropping one of these `fsync`
calls is not caught by any test.

Regularization leaves links into a ROOT to the placement (a link no
landed file names stays a link). `--check` reports `replaced_archived`
(`done` rows) and, at placement paths now, `replace_pending` (archive
links whose landed whole lines differ in size from the target, a
compressed landing included), `replace_equal_size` (equal sizes: a pass
compares them) and `replace_proved` (proved equal or ahead, nothing
changed since); a replaced file is checked as the landed file like any
other.

**Crash safety.** Fifteen fault points (`reseat.*`, `src/fault.rs`) with
twenty-two fault-harness rows (two of them a `.jsonl.zst` rollout
decompressed into its temporary; five the rulings' mutations and their
undo; six an archive replacement at its intent, after its exchange and
before `done`, two of them over an `occupied` row from another landing
with regularization off, and its undo): a rerun
settles every crash forward from the ledger and the destination's own
bytes. A temporary with no intent is a staged
copy and is removed; an extend intent is resumed after proving the placed
bytes still hash to the row and the bytes already appended are the
landing's next ones; a supersede intent finishes or undoes the exchange by
the staged file's whole identity.

**The R0 check** (`--check`) is read-only everywhere and runs from the
landing, not the ledger, so a session no pass placed fails. Per lane:
every transcript is a regular file under the key of the real destination
directory that holds the landed transcript's whole lines, exactly (a pass)
or followed by what the destination wrote after them (`continued`: a
session resumed there, also a pass); one that ends inside a line fails
(`ends-mid-line`). Its sidecar, per-session and plan files are there (of
the same size unless the session continued). Every Codex row has the exact
`cwd` and a `rollout_path` (the placed `.jsonl`, or the `.jsonl.zst` Codex
compressed it into there, its directory spelled any way that resolves to
the same one, or the same rollout file in another directory, never a
`.gz`) whose decompressed stream holds the landed one (a file that cannot
be read fails that thread, `unreadable`)
the same way; a rollout with no snapshot row fails `row-absent` unless a
row is there, and a row carrying the source's own `cwd` (a by-ID resume
that kept the session directory) fails `row-cwd-is-source`. A longer
landed form of a thread's rollout that the destination does not hold
fails the thread (`extra-form-unplaced`), and a thread whose rollout is
recorded outside every `--codex-source-root` is unlaned
`RESEAT_LANE_UNMAPPED:rollout-root`. The lane's planned workspace is `estate-diff` `identical`
(`--plan`; `--sessions-only` waives Git and the hand-off, and without
either the lane is `incomplete`, never a pass); with `--note-marker`
(TIN-6569) its `AGENTS.md` or `CLAUDE.md` holds the marker. Every session
or thread without a lane (unmapped, unresolved, unreadable) or kept by ID
and not holding is an `unlaned` line. It exits `RESEAT_R0_FAILED` unless
something was checked, every lane passes, retention holds, and every
`unlaned` one is named by `--waive` / `--waive-from`. It never opens a
session in the live destination state, because opening appends; two
real-binary canaries (`tests/estate_reseat.rs`) run in throwaway homes
under `bwrap --unshare-net`: the installed Claude Code picker lists the
re-seated session, not a symlinked transcript, and leaves the transcript
unchanged; the installed Codex resumes (opens, and appends to) a re-seated
paginated thread whose rollout landed as `.jsonl.zst`, in an index the
same Codex made, and the check still passes (`continued`). Two more cover
the rulings: the Claude Code picker does not list a symlinked transcript
and lists it once regularized; the Codex lane picker lists a row built
from a rollout header and a row re-spelled from the `~/git` spelling
(which it did not list before), resumes the first by ID with no
working-directory question, and `--undo` puts the spelling back.

## Reclaim

Unique content is preserved before redundant containers are deleted. Content
and database rows are compared as appropriate; names and sizes are not
redundancy proof. Hardlinks mean apparent tree size is not reclaimable space.
