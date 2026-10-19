# 2026-10-19: single-file `copy`/`pull`, `serve` state parent, vanished move roots

Lane: `fix/single-file-pull-and-serve-state-20261019` (PR TBD), from
`origin/main` `01be8c96efab`. Edits on sting; every build and test on honey
(worktree `~/git-build/bulkload-01be8c96efab/wt-single-file-pull-serve-state`,
own target dir, `TMPDIR=/var/tmp/jess-bulkload-tests/single-file-pull-serve-state`
per the coordinator's host fact: honey `/home` is 88 % full and the tests
refuse below 25 % free; the directory is removed after the run). Nothing run
against the live estate. Rulings: R-N13; plan of record rev 2 (tummycrypt
`docs/agent-notes/2026-10-19-bulkload-reset-review.md`, gap C6); operator
ruling OI-1003-Q196 (no build on neo or sting). Merges through the local
gate (OI-1003-Q199).

Requirement served, quoted: SPEC R0 "R0 also covers dots, configs and loose
home files, whether or not a session is attached (SEAT-05, SEAT-06, RES-06,
KEEP-01, KEEP-10, KEEP-26)"; SPEC R1 "Everything is carried, regardless of
content"; SPEC R2.2 "DEST is live too, and is never clobbered"; Acceptance 1
"every refusal is typed and dispositioned" (SEAT-12, `slo.md` S4) for #277
and #279. Issues #232, #276, #277, #279.

## What was wrong

- #232 (C6): `pull`/`copy` opened the source with `O_DIRECTORY`
  (`io/sys_posix.rs` `open_root`) and refused an absent DEST, so the ~10
  plain files at the top of neo's `~/git` (`clockdiag-receipt.txt`,
  `printstack_estate.md`, `*.json`, `*.patch.tmp`) could be moved by no
  verb; `estate-move-source` listed them as `unreadable IO`.
- #276: `estate-move` named `SOURCE_MOVE_STATE/serve/<name>` as the remote
  serve state but made `serve/` only in its same-host branch;
  `serve`'s `canonical_state` canonicalised the absent parent and died
  `IO (errno 2)`, which the destination saw as `TRANSPORT_LOST`. A second
  site showed on the live runs (coordinator, #276 comment): the carry
  phase passes `--source-state-root SOURCE_MOVE_STATE/serve/carry` to
  `estate-carry-files`, so each item's remote state is
  `serve/carry/<item>`, two levels below `serve/`; every carry died the
  same way, `carried=refused reason=TRANSPORT_LOST`.
- #277: `estate-move-source` canonicalised every `SOURCE_ROOT` with `?`, so
  13 worktree directories the live neo lane had just removed ended the pass
  before its inventory, with a bare `IO (errno 2)` no review can close.
- #279: a second `estate-move` on a `MOVE_STATE` whose `move.lock` another
  run held (an `estate-move --plan-only` still in its preview) exited
  `IO (errno 11)`, the non-blocking `flock`'s `EWOULDBLOCK`, unaccounted in
  closure and naming nothing.

## What changed

`copy [--file]`, `pull [--file]`, `serve [--file]` (#232). `RootKind::File`
on the destination; `pull --file` runs the source half as `serve --file`,
a fixed literal on the registered ssh builder (`source_command_registry.rs`
shape updated). The wire is unchanged (v6): a file session is a tree
session whose root descriptor is the file's parent and whose walk visits
one name (`Walker::single_seat`), so it carries one `Entry` named by the
source leaf; the destination lands it under DEST's own leaf
(`Inbound::land`), so its store rows, reuse keys and refusals are keyed by
the path the file lives at and an unchanged rerun reads 0 source bytes.
Adopt-if-identical and `DESTINATION_OCCUPIED` are the tree's own paths,
untouched. A `--file` session offered anything but one regular row
refuses `SOURCE_ROOT_KIND_MISMATCH`.

Absent DEST: `copy`/`pull` make the tree root themselves (0700,
`make_destination_root`) when its parent exists, after the S2 overlap
checks and only once the source's `Start` arrives (its root opened as the
kind asked for), so a session the source refuses leaves no empty DEST
(R4); a missing parent is `DESTINATION_PARENT_MISSING`. A symlink at DEST
is still refused: an existing tree is opened by the path given, without
following it. No `--create-dest` flag: SPEC R4 has no
"never write where not asked" sentence; it says each tree lands once at
its live path and no landing archives or stale run directories are left.
R2.2 forbids overwriting, resetting or deleting DEST work. Making the one
directory the operator named as DEST writes exactly where asked and
touches nothing that exists, so the flag would gate nothing the SPEC
protects. A parent is not asked for, so it is never made.

Typed root refusals (five new codes, each with a non-test constructor,
`refusal_taxonomy.rs`): `SOURCE_ROOT_MISSING`, `SOURCE_ROOT_UNREADABLE`
(carries the errno, like `IO`), `SOURCE_ROOT_KIND_MISMATCH`,
`DESTINATION_PARENT_MISSING`, `DESTINATION_KIND_MISMATCH`. Per-seat
typing (EACCES on a file beneath a root) is plan rev 2 gap C5 / wire v7,
another lane's; `refuse::source_root` types only the root itself.

#276, both halves, and why both: `estate-move-source` makes
`MOVE_STATE/serve` and `MOVE_STATE/serve/carry` (0700) beside `carry/` and
`private/` because the state layout is that verb's, so it is complete
before any transfer and visible to the operator; `serve` makes every
missing directory above its state root (`ensure_state_parents`,
`create_dir_all` semantics, as the coordinator asked after the second
site) because `serve` owns its own precondition whoever drives it: `pull`
by hand with a nested state path, `estate-carry-files --source-state-root`,
or a newer destination half against an older source tree. Neither alone
covers the other's callers. Each level is made 0700 with `mkdirat` beneath
the descriptor of the one above and entered `O_NOFOLLOW`, so a name
swapped for a symlink in between refuses `PATH_ESCAPES_ROOT`; the nearest
existing ancestor is the operator's path, symlinks and all, as the store's
own earlier components are. Each new entry is made durable before the next
(R-N102). `canonical_state` predicts through every missing level, so the
S2 overlap check still runs before anything is made: a chain that would
lie inside the source refuses `SNAPSHOT_ROOTS_OVERLAP` with nothing made.

#277: `estate::moving::walk` turns a root that fails to canonicalise into
a `Note` (`source-vanished`, code `SOURCE_ROOT_MISSING`, for `ENOENT`;
`unreadable` with the typed code otherwise) and continues; the inventory
line gains `source_vanished=`; only a pass with no usable root refuses,
with the first root's refusal.

#279: `estate::moving::lock` maps the lock's `EWOULDBLOCK` to
`MOVE_STATE_BUSY path="MOVE_STATE/move.lock"` (a sixth new code); closure
treats it as rerun-only (`state-busy`), like `TRANSPORT_LOST`: nothing ran,
so no review closes it. Not done, on purpose: the holder's pid (AGENTS.md
R-N104, "Never check whether a PID is alive"; printing one invites exactly
that check) and `--wait` (a driver retries on the typed code; a blocking
wait would stall a lane silently). The shared `estate::exclusive` and its
`estate.lock` callers are unchanged; they can take the same mapping if
that contention is ever seen live.

Out of scope, unchanged: `estate_move.rs` sessions phase and `transfer()`
(other lanes); per-seat source error typing (C5); SQLite slot budget.

## Review round (#283 at `5622280b`, independent adversarial review)

`origin/main` `9854236` (#281, #282) merged in first with a merge commit
(`81f10ef`; one conflict, the `Display` path arm in `refusal.rs`, both
variants kept). Then, in the coordinator's order:

1. HIGH, fixed: `--file` followed a symlink at SOURCE (`read_open` and
   `copy`'s S2 pre-check canonicalized the leaf). Only the parent is
   resolved now; a symlink leaf is carried as a symlink row with its
   literal target byte for byte (chosen over refusing it: home-manager and
   stow dotfiles are links, and R1 carries links as links). The
   destination's existing symlink landing gives the rest: create when
   absent, adopt an identical link, `DESTINATION_OCCUPIED` otherwise.
2. MED-HIGH, fixed for `--file`: the leaf is not stat'ed before `Start`;
   the single-seat walk emits `SOURCE_VANISHED` for an absent leaf and
   `SOURCE_ROOT_KIND_MISMATCH` for a directory or special file, as rows
   that cross wire v6 typed into the destination's record. A missing tree
   root or `--file` parent is still the source's whole-session refusal
   before `Start` and still arrives as `TRANSPORT_LOST` over `pull` (gap,
   below).
3. MED, fixed: a symlinked DEST is `DESTINATION_KIND_MISMATCH`, never
   followed: a tree's root up front (`resolve_destination`), and a regular
   output landing on a symlink to a file, a directory or nothing, per path
   (`materialize::open_regular` maps the leaf's `ELOOP`; this also types
   the same case inside a tree).
4. MED, fixed: the source authority names the directory walked (for
   `--file`, the parent), so a later tree pass of that parent with the same
   states reuses the file's rows (0 bytes) and supersedes a changed one.
5. LOW-MED, fixed: a first `estate-move-source` pass that finds no
   repository writes an empty plan and completes; a root that cannot be
   listed (mode 000, or gone since it resolved) is a typed note
   (`SOURCE_ROOT_UNREADABLE` / `SOURCE_ROOT_MISSING`) and unusable; an
   entry gone mid-walk is `SOURCE_VANISHED`. A nested directory that fails
   with anything but `ENOENT` keeps its `IO` errno (per-seat typing is gap
   C5, wire v7, another lane's).
6. LOW, fixed: the destination store opens before an absent DEST is made,
   a made root still empty is removed (`remove_dir`) when this side's setup
   then refuses, and the made root is 0755 under the umask, as
   `estate-carry-files` makes its destinations (this replaces the 0700 the
   brief named; the coordinator asked to align with carry). Superseded by
   verification B below: the source root's mode, else 0700.
7. LOW, no change: a hand-made group- or other-readable `MOVE_STATE/serve`
   refuses `PATH_ESCAPES_ROOT`; the live neo `MOVE_STATE`s are 0700
   (reviewer checked).

Honey, this round: an `estate_move` A/B stage of mine hung in `git fetch`
from 01:14 (no lock, the remote answered later; most likely a dead HTTPS
connection); it was never signalled (R-N11). The jobs queued behind it
waited on files, so I wrote its exit marker myself to release them
(R-N104: wait on files), and later swapped two stale queued scripts for
no-ops by atomic rename. The A/B script now bounds its fetch
(`timeout 300`). The lean target dir (`target-sf-v3`, debuginfo off)
replaced a 12 GB one after the coordinator reported `/home` at 100 %.

## Verification round (#283 at `633ff463`)

The coordinator's verification review found all six first-review items
fixed and the branch data-safe, with two small defects, fixed in
`8d9ac60`:

- A: a DEST written with a trailing `/` (or `/.`) followed a symlink at its
  last component (`lstat("dst/")` reports the target; the no-follow open
  follows a path ending in `/`). `resolve_destination` rebuilds DEST from
  its components before the lstat and the open, so a link there is
  `DESTINATION_KIND_MISMATCH`.
- B: a newly made DEST root was 0755 whatever the source, so
  `copy ~/.ssh /new/.ssh` landed world-listable. `copy` makes it with the
  source root's permission bits (`& 0o777`), never wider, narrowed by the
  umask; `pull` makes it 0700, since wire v6 carries no root row.

Filed, not fixed (coordinator): #287 (C: a `copy --file` closure record is
keyed by a symlink leaf's target) and #288 (D: cross-verb ownership
flip-flop; the review's repro was not in this lane's hands, so the issue
gives this lane's reading of the mechanism to be checked against it).

## Tests

All on honey, never on sting or neo (OI-1003-Q196), `nice -n 19 nix
develop .#default`, own `CARGO_TARGET_DIR`,
`TMPDIR=/var/tmp/jess-bulkload-tests/single-file-pull-serve-state`
(removed after). Code head of record `b0227c838480`; the head after it
adds only this note.

| Check (`b0227c838480`) | Result |
|---|---|
| `cargo fmt --all -- --check` | ok |
| `cargo clippy -p bulkload-agent -p bulkload-proto --all-targets --locked -D warnings` | ok |
| `just rust-check` lines 65 to 67 (fmt, `clippy --workspace`, `clippy --features io-trace`) | ok |
| `cargo test -p bulkload-agent --locked --no-fail-fast` | 42 target results ok, 0 failed (lib 920 passed, 6 ignored; `single_file_copy` 15, `estate_move_source_roots` 6, `refusal_taxonomy` 5, `source_command_registry` 22) |
| `cargo test -p bulkload-proto` | 18 passed |
| `tests/estate_move.rs`, twice | ok, ok (load 15 to 17) |
| `just fault-harness` (mandatory) | ok: `fault_harness` 176 passed, `power_loss` 19 passed, `io::` lib 72 passed, and its four sub-recipes |
| `just rust-check` line 68 (`rust-test`, five concurrent groups, load ~90) | FAIL in groups 1 and 2, both outside this change (below); groups 3 to 5 ok |

The two `rust-test` failures:

- `transfer::tests::sqlite_carry::q146_a_changed_store_supersedes_its_own_output_through_the_exchange`
  asserts a process-global counter (`SourceSqliteSnapshots`) moved by
  exactly 1 and saw 2: another lib test took a snapshot in the same
  process at the same time. It passed in the `cargo test` run above, and
  its module rerun alone on the same head under the coordinator's limits
  (4 build jobs, 4 test threads) passed 37/37 at load 82 to 91. This
  branch adds no lib test that takes a snapshot.
- `estate_move.rs` `one_pass_lands_adopts_besides_carries_and_accounts_for_every_item`
  fails on unmodified `origin/main` `01be8c96efab` too (A/B, same
  `TMPDIR`: run 1 ok, run 2 FAILED, load 74): the converge pass recaptures
  `diverged` only when its fixture finishes within the 2 s racy window.

`materialize::tests::the_ext_magic_reads_its_mount_type_by_major_and_minor`
failed earlier in this lane (its fixture hard-coded device 253:0, honey's
`/var/tmp`); `main`'s `638ef089` fixed it and arrived with the merge.

`just darwin-touched`: no, so no darwin-check is needed (OI-1003-Q199).

## Open

- Merge gate: `just local-gate-receipt --pr 283 --post` on the exact head,
  and the lanes' adversarial review of the review round (OI-1003-Q173).
  This lane does not merge.
- A missing tree root, or a missing `--file` parent, is still the source's
  whole-session refusal before `Start`, so over `pull` it arrives as
  `TRANSPORT_LOST` (its typed code is on the source's stderr). Typing it
  over the wire needs a session-refusal frame or a root row (wire v7).
- A nested directory beneath a move root that fails with anything but
  `ENOENT` keeps `IO` with its errno (gap C5, wire v7).
- `transfer_outcome::copy_source` still names a `copy --file` source by its
  canonical path, which follows a symlink leaf: two links to one target
  would share a closure record key. Left as is so `transfer-expect`
  manifests keep matching; cosmetic for closure naming, not for bytes.
- The `~/git` top-level files on neo still need a driver (a loop of
  `pull --file`, or a seat list); this PR gives the verb. The first
  neo-to-sting `pull --file` is the live acceptance, under the live-lane
  rules; `pull --file` is otherwise proven through an `ssh` stand-in.
- `rust-test` runs its groups concurrently; under the coordinator's
  one-test-run limit the workspace tests were run sequentially in its
  place.
