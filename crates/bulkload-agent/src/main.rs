//! Native ordinary-file transport and offline provider composition on Unix.
//!
//! Argument parsing is hand-rolled on purpose. `clap` is not on the R34
//! dependency allowlist for the agent, and a binary whose whole point is a
//! closed dependency graph should not grow a parser crate to read one
//! subcommands.

use bulkload_agent::refuse::RefuseAt as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use bulkload_agent::freshness::{Freshness, FreshnessCache as _, MemoryCache, StatIdentity};
use bulkload_agent::hash;
use bulkload_agent::walk::{self, HashPolicy, WalkOptions};
use bulkload_proto::{BulkloadRefusal, Control, FileKind, Frame, Result, RowSchema};

// `estate-move-source` and `estate-move` (M2 item 7): the verbs above,
// composed.
mod estate_move;

const USAGE: &str = "\
bulkload-agent -- ordinary-file transport and offline SQLite composition

USAGE:
    bulkload-agent [--durability=group|strict] [--min-free-percent=N]
                   [--priority=background|normal|auto] [--escalate-after=SECONDS]
                   [--source-ledger-sync=relaxed|full]
                   [--sqlite=refuse|snapshot] <SUBCOMMAND>

SUBCOMMANDS:
    selftest    Hash a temporary file and round-trip a postcard frame
    walk PATH   Stat-walk PATH and print the row and refusal counts
    copy SOURCE DEST SOURCE_STATE DEST_STATE
                Native local copy with private resumable chunk stores; the
                session's outcomes are recorded in
                DEST_STATE/transfer.closure.d, one record per transfer, for
                closure-report --transfer (WP3 PR 4). The S2 overlap checks
                run before anything is written; a session that ends refused
                keeps the earlier session's refused rows
    pull HOST SOURCE DEST SOURCE_STATE DEST_STATE [REMOTE_EXECUTABLE [SSH_CONFIG]]
                Native SSH pull; remote bulkload-agent must be installed; the
                session is recorded in DEST_STATE as copy's is
    serve       Serve one framed request on stdin/stdout (for SSH)
    git-export REPO NEW_CAPTURE_DIR [--include-rebuildable]
                Archive refs/stashes and staged/worktree trees in a bundle
    git-carry-estimate [--state-dir DIR] SOURCE_REPO DEST [SOURCE_REPO DEST ...]
                Read-only: report what a negotiated thin-pack carry would move
                from SOURCE_REPO to DEST (a local path or HOST:PATH over
                ssh -T -oBatchMode=yes); missing_thin_pack_bytes is the gate
                metric (R-N74). Refuses a partial or differently-shallow DEST
                (R-N75); a refused pair prints refused=CODE and the verb exits
                nonzero. A child's
                stderr is never printed (R-N121): only stderr_class=, plus
                stderr_keyed_blake3= and stderr_file= when --state-dir keeps
                the raw bytes in DIR/stderr/<digest>.log (mode 0600). DIR
                must be yours, mode 0700 or tighter, no ACL, and outside
                every SOURCE_REPO and local DEST
    estate-add PLAN SOURCE_REPO DEST_REPO [ABSENT_WORKSPACE]
                Append an explicit reviewed item; no automatic worktree proliferation
    estate-show PLAN
                Print the exact selected sources, repositories and restore targets
    estate-retarget PLAN CORPUS --map OLD=NEW [--map OLD=NEW]...
                Re-point the plan's repositories and workspaces (longest
                component prefix OLD becomes NEW; sources never change) and
                re-key each moved item's capture record in CORPUS, so the next
                capture reuses it and reads no source byte (M2 item 5). The
                plan as it was is kept as PLAN.before-retarget-<digest>
    estate-add-batch PLAN SOURCE DEST WORKSPACE_OR_DASH [SOURCE DEST WORKSPACE_OR_DASH ...]
                Append selected items in one linear plan update
    estate-capture PLAN PRIVATE_STATE CORPUS JOBS [--include-rebuildable]
                Capture reviewed Git items with successful capture reuse (jobs 1 or 2)
    estate-apply [--adopt] [--exclude-union] [--beside SUFFIX] [--refs-in] [--advance-ancestor] [--activate-origin PREFIX]... [--side-root ROOT]... PLAN CORPUS PRIVATE_STATE SOURCE JOBS
                Import refs and restore only explicitly selected absent workspaces.
                A landed checkout bulkload proves unchanged converges to the
                current capture. --adopt (#226, OI-1003-Q171/Q172): an existing
                checkout bulkload never landed becomes bulkload's when it proves
                equal to the capture (HEAD, index entries, a standalone item's
                exclude, every carried seat's kind, mode and bytes), with nothing
                moved: ledgers written in place, absent carried refs created,
                equal ones adopted, differing and destination-only ones kept; a
                checkout that differs refuses GIT_DESTINATION_DIVERGED; one in
                an unrelated or another repository, spelled through a symlink,
                or a linked worktree whose admin directory names another path
                refuses GIT_DESTINATION_OCCUPIED, all with nothing written. A
                refs-only item's related or empty repository is adopted the same
                way (an unrelated one refuses GIT_DESTINATION_OCCUPIED after the
                provenance import), and a ledger naming another plan's item is
                re-keyed after its proof when the capture is the landed tree's
                (equal, or its HEAD descends from the ledger's). A bare capture into an absent path
                lands a bare repository bulkload makes (with or without
                --adopt). --activate-origin PREFIX (repeatable): a first landing
                activates an scp-style or ssh:// origin starting with PREFIX
                (never a local path); otherwise only HTTPS origins are active.
                --exclude-union (M2 round 2): a first linked landing whose
                repository's common info/exclude is not the capture's appends
                the captured lines it lacks instead of refusing
                GIT_IGNORE_POLICY_CONFLICT: a recorded write (STATE
                ITEM-SOURCE.exclude-union; receipt exclude-appended=N) that never
                removes or reorders a line and changes what EVERY worktree of the
                repository ignores. Where a line would be appended it still
                refuses for a negation (!pattern) on either side, an exclude a
                bulkload ledger owns, a symlinked info or an exclude that is not
                a regular file with one link this user owns.
                --beside SUFFIX (M2 round 2; requires --adopt): a workspace
                item whose existing checkout the adopt proof refuses
                GIT_DESTINATION_DIVERGED is left as it is and the capture lands
                at the workspace path with SUFFIX appended (as estate-carry-files
                --beside SUFFIX), a detached linked worktree of the planned
                repository (never appending to its shared info/exclude). Every
                pass lands it (gap C12, A-115): this item's own beside landing
                of an older capture converges (proved unchanged first, else
                GIT_DESTINATION_MODIFIED; a linked one stays detached, no live
                ref stepped), another item's landing is re-keyed (A11b) and
                converged, and a standalone checkout no ledger names is adopted
                in place when it proves equal (HEAD, index, exclude, stash and
                every carried seat; a sealed bulkload hand-off block at byte
                0 of AGENTS.md/CLAUDE.md the one difference allowed). Anything
                else at the beside path refuses GIT_BESIDE_FOREIGN path=...
                with nothing there written, and the item's branches, tags and
                stash entries still go in under --refs-in. A beside landing this item
                made and the operator removed is not laid down again
                (GIT_DESTINATION_MODIFIED). Every captured ref goes in as
                refs/NS/NAME and each stash entry as refs/NS-stash/N (NS:
                SUFFIX without its leading '.', one plain ref component),
                recorded write-ahead in STATE ITEM-SOURCE.beside: created, or
                moved where this item's own earlier ref still holds the value it
                created; outcome and journal workspace-beside, closure
                applied-beside. A ref namespace holding anything else, or a
                captured ref kept at another value, refuses
                GIT_DESTINATION_OCCUPIED.
                --refs-in (M2 item 7; needs --adopt and --beside): after an
                item adopts, lands or converges its repository (not a beside
                landing), each captured branch and tag the repository lacks
                or holds at another value goes in create-only as
                refs/NS/heads/NAME (refs/NS/tags/NAME), recorded write-ahead
                in STATE ITEM-SOURCE.refs-in; receipt refs-in-created=N
                refs-in-present=N refs-in-kept=N refs-in-retired=N. No ref
                moves: a ref of that name held at another value (an earlier
                pass's) is kept and counted, the source's value staying in its
                provenance refs; one this item created and the operator
                removed is not created again (retired). When a beside landing
                refuses (gap C12), its refs-in still runs, stash entries
                included as refs/NS-stash/N, and closure prints refs_in on the
                refused row. A ref named refs/NS itself refuses the item
                GIT_DESTINATION_OCCUPIED before it writes anything.
                With --refs-in, a capture with no tracked change (index and
                tracked files its HEAD's) whose HEAD a live ref or a captured
                branch or tag brings in lands NO tree beside (operator ruling
                2026-10-16 21:24Z, 'refs in always; a tree beside ONLY where
                neo has uncommitted work'): its refs go in (stash entries as
                refs/NS-stash/N) and its untracked and ignored files are
                reconciled into the live checkout one by one, create-only:
                absent placed, equal adopted, different landed as
                FILE+SUFFIX (a directory under a live file, under
                DIR+SUFFIX/); a FILE+SUFFIX holding something else refuses
                GIT_BESIDE_FOREIGN path=... after the rest. Recorded in STATE
                ITEM-SOURCE.files; receipt beside-tree=none files-tracked=N
                files-placed=N files-adopted=N files-beside=N ...; outcome
                and journal workspace-refs-in, closure applied (with files
                and refs_in). The live checkout's HEAD, index, tracked files,
                refs and info/exclude are never touched.
                --advance-ancestor (operator ruling 2026-10-19, 'Yes, advance
                those'; needs --adopt and --beside): a live checkout bulkload
                did not land that differs from the capture is advanced in
                place instead of landing beside, when (proved before any
                write and again under the destination lock) its HEAD is an
                ancestor of the captured HEAD (else GIT_ADVANCE_NOT_ANCESTOR),
                its branch is the captured one, or both are detached (else
                GIT_ADVANCE_BRANCH_MISMATCH: never detached or switched), that
                branch is a plain ref at the live HEAD no other worktree or
                pending intent holds (else GIT_ADVANCE_BRANCH_BUSY), and its
                worktree and index are clean: nothing staged, unstaged or
                untracked but ignored files, no operation in progress, no
                assume-unchanged or skip-worktree entry (else
                GIT_ADVANCE_NOT_CLEAN); a change since the proof refuses
                GIT_ADVANCE_RACED. The advance moves that branch (or the
                detached HEAD) by compare-and-swap from the proved tip, then
                lays down the captured tree, dirty and untracked files and
                index as a landing does; an ignored entry of the live
                checkout where the capture lands one is never touched (left,
                counted advance-kept=N; the capture's bytes stay in its
                provenance), nor are other branches, the stash, other
                worktrees, configuration or info/exclude. The checkout is
                then landed (workspace-restored, ledgered; recorded in its
                .git as advanced), the captured refs and stash entries go in
                under --refs-in, and the item's beside landing, brought to
                the capture as every beside pass brings it, is retired
                (renamed aside, then removed) only when it is this item's
                and proves equal to the live tree (HEAD, index, every file,
                a sealed hand-off block the one difference allowed; nothing
                its Git holds unreachable from the live repository), else
                kept with a reason (receipt advance-beside=retired|kept|
                absent advance-beside-reason=...). A refused advance writes
                nothing at the live checkout, lands beside in the same pass,
                and is recorded in PRIVATE_STATE ITEM-SOURCE.advance: closure
                holds the item refused-pending-review with that code until
                closure-dispose. A crash is finished by the rerun.
                --side-root ROOT (repeatable; M2r2 item e; needs --adopt):
                after the preflights and before any item, each planned path
                spelled through a symlink whose one hop names a directory
                inside ROOT (not nested with another seat's target or a
                planned path) is seated: one exchange gives the live path the
                directory (same inode, no byte copied) and a link back
                replaces the side-root path, recorded in PRIVATE_STATE
                (`seat live= target= outcome=seated|seat-resumed|unseated|refused
                reason=`; an interrupted seat or undo is finished first); the
                items then adopt or re-key at the live path, and a seat stays
                if they refuse
    estate-move-source [--map SRC=LIVE]... [--jobs N] [--include-rebuildable] MOVE_STATE SOURCE_ROOT...
                M2 item 7, the source half of estate-move (run on the
                source; auto priority, as estate-capture): every Git
                repository under each SOURCE_ROOT (the walk stops at a
                repository and does not follow symlinks), grouped as Git
                groups it: the main and every worktree it registers, wherever
                they are (a linked worktree under a root brings its main), a
                bare repository refs-only. Each path's live path is the
                longest --map prefix's, else the same path. One line per item
                (estate-move-source item=ID kind=main|linked|bare) and per
                note: what is not planned (a symlink, plain files, a prunable
                worktree, an unreadable repository or entry, a directory its
                repository does not list, and each nested repository a
                capture holds as custody only) is printed, counted and
                written to MOVE_STATE/carry/move-source.notes; estate-move
                reports it not moved. New items are appended to
                MOVE_STATE/carry/plan.postcard (estate-add-batch; existing
                ones never change, and a source planned at another live path
                refuses RECEIPT_BINDING_INVALID), then the plan is captured
                (estate-capture, jobs N, default 2) into
                MOVE_STATE/carry/corpus, private state in MOVE_STATE/private.
                Item refusals are written to move-source.refusals and do not
                fail the verb; a refusal before every item answered does.
                Each pass writes its mark to move-source.started first and,
                once every item answered, to move-source.done. MOVE_STATE is
                0700 and flocked
    estate-move --label LABEL --source SOURCE [--plan-only] [--exclude-union] [--advance-ancestor] [--no-hand-off]
                [--activate-origin PREFIX]... [--side-root ROOT]... [--jobs N] [--dispositions LEDGER]
                [--sessions SOURCE_HOME [--codex-source-root DIR] [--dest-home DIR]
                 [--map SRC=DST]... [--reseat-arg ARG]...]
                [--remote-executable PATH [--ssh-config FILE]]
                HOST|- SOURCE_MOVE_STATE MOVE_STATE
                M2 item 7 (design 2026-10-16-estate-move-design.md), the
                destination half: one pass of the move, composed of the verbs
                above. LABEL (1-64 of A-Z a-z 0-9 - _, not a Git or bulkload
                ref namespace) names the beside suffix .LABEL and the ref
                namespace refs/LABEL/; SOURCE is the provenance label every
                apply, carry and closure is given. LABEL, SOURCE, HOST and
                SOURCE_MOVE_STATE are bound in MOVE_STATE/move.ledger at the
                first run (another refuses RECEIPT_BINDING_INVALID). HOST - is
                this host (copy; the two states must not nest), else pull over
                SSH; serve's state is SOURCE_MOVE_STATE/serve/NAME. Global
                flags (--min-free-percent=N, --priority=, --durability=) apply
                to every phase; --sqlite is always snapshot here.
                1 pull: SOURCE_MOVE_STATE/carry into MOVE_STATE/carry;
                SOURCE_CHANGED_AFTER_SNAPSHOT when its marks show a source
                pass that has not finished (rerun once it has). 2 plan: per
                item, the live path's presence (absent, empty, git, not-git,
                symlinked: the path or an ancestor, so map to real paths;
                unreadable) and its capture (held; refused, carried as files;
                transient or pending, left for the next source pass) give one
                action: land, adopt (the apply proves it or lands it beside),
                refs-only, carry-live, carry-beside or refuse (the apply's
                typed refusal, pending review). One estate-move item= line
                each, a histogram line and the source notes' counts. A carry
                once started stays where it went while the item is not
                captured. --plan-only stops here, recording nothing, after an
                estate-diff preview (MOVE_STATE/diff.jsonl). 3 apply:
                estate-apply --adopt --beside .LABEL --refs-in (with
                --exclude-union, --advance-ancestor, --activate-origin,
                --side-root as given)
                into MOVE_STATE/private, --jobs N (default 2); when every
                item answered, item
                refusals are each item's and the pass goes on, else it stops.
                A beside landing converges to a newer capture when its proof
                holds (gap C12). 4 carry: estate-carry-files for
                the carry items, at the live path while it is still absent or
                empty, else with --beside .LABEL. 5 hand-off: estate-handoff
                for the items whose workspace this pass landed or adopted
                (unless --no-hand-off). 6 with --sessions: SOURCE_HOME/.claude
                and the Codex root (--codex-source-root, default
                SOURCE_HOME/.codex) into MOVE_STATE/landing, then, when both
                pulls completed, estate-reseat --lanes-from-plan with each
                --map and --reseat-arg (one estate-reseat option per flag,
                e.g. --no-regularize, --unmapped=by-id) into DEST_HOME
                (--dest-home, default $HOME), state MOVE_STATE/reseat. 7
                closure: closure-report over the plan (with --dispositions, the
                closure-dispose ledger joined), written to
                MOVE_STATE/closure.json, summarised on an estate-move closure
                line with not_moved= (the source notes). The gate passes only
                when every item is accounted and reviewed and nothing is noted
                not moved; an estate-move gate= line names the next step. The
                verb exits with the first phase's refusal, else
                CLOSURE_UNACCOUNTED while the gate fails. The next converge
                pass is estate-move-source again on the source, then
                estate-move. Nothing is deleted, no live ref moves, no live
                checkout is switched
    estate-unseat PRIVATE_STATE [LIVE ...]
                M2r2 item e: undo each seat estate-apply --side-root recorded in
                PRIVATE_STATE (or only those of the LIVE paths given): the
                directory back at its side-root path (the same inode), the
                link back at the live path with its own text, the record
                removed (the record is marked unseating first, so an
                interrupted undo is finished, never re-seated). Refuses
                RECEIPT_BINDING_INVALID, changing no path, when either path no
                longer holds what the seat left.
    estate-diff [--since NS] [--scan DIR]... [--activate-origin PREFIX]... [--side-root ROOT]... [--beside SUFFIX] PLAN CORPUS SOURCE PRIVATE_STATE [PRIVATE_STATE ...]
                Read-only: classify each plan item against the destination now
                (identical, source-only, source-ahead, dest-ahead, diverged with a
                ledger; differs with a hint without one), with what estate-apply
                and --adopt do there, the components that differ, per-ref rows,
                destination work since NS (ns since the epoch), counted
                rebuildable omissions and the origin a first landing activates;
                plus worktrees of planned repositories no item plans and, with
                --scan, Git directories under DIR in no planned family
                (dest-only). One bulkload.estate-diff.v1 JSON line per row
    estate-handoff [--check | --clear [--clear-carried] | --dry-run] [--item ID]...
                [--workspace PATH]... [--sibling LANDED_PREFIX=LIVE_PREFIX]...
                [--readme] [--renew] [--as-of NS] PLAN CORPUS PRIVATE_STATE SOURCE
                TIN-6569, OI-1003-Q183: write a dated hand-off block at byte 0 of
                AGENTS.md and CLAUDE.md in each landed workspace (absolute paths,
                capture and landing facts, estate-diff's ref classes, the commands
                to converge and consolidate before any other work), recorded in
                its own ledger beside workspace.landed so estate-apply keeps an
                intact block instead of refusing the seat (and re-renders it
                after a converge). A missing target is
                created untracked; an existing AGENTS.override.md is prefixed too;
                README.md only with --readme; neither is ever created. The block
                is written whatever the repository's remotes (OI-1003-Q188); no
                URL is read for it or printed. A symlink other than one target
                to another refuses HANDOFF_TARGET_UNSUPPORTED; an edited block of
                this landing's HANDOFF_BLOCK_MODIFIED; an unlanded item
                HANDOFF_NOT_LANDED; all with nothing written. A block another
                landing rendered, or one of this landing's that the target's
                landed blob itself starts with (a round trip; each carried with
                the source's bytes, whatever is written below it since) is
                content: kept below this one, never cleared unless --clear
                --clear-carried asks (OI-1003-Q190: a recorded edit of landed
                content that converge accepts); a block of this landing's that
                its record does not name and the landed blob does not start
                with is an edited block. --check is read-only (state, open
                rows, ready: the commit rules and a clean working tree, read
                with no filter driver run, in the live sibling and in each
                destination-only worktree) and exits HANDOFF_MARKER_COMMITTED
                when a token line the source's history never added is staged
                or added by a commit the carried capture does not reach (any
                branch, tag, remote-tracking ref, stash entry or HEAD; a token
                already in the source's history is reported as
                source_history, OI-1003-Q191); --clear restores the bytes and
                finishes a crashed clear; --dry-run prints the blocks. A rerun
                keeps the options the record was rendered with unless they are
                given again; an unchanged rerun reads and writes nothing. One
                JSON line per workspace (schema bulkload.estate-handoff.v1, the
                verb's own name)
    estate-carry-files [--dry-run] [--beside SUFFIX] [--item ID]... [--bind ID=DEST_STATE]...
                [--bind-only] [--recarry]
                [--host HOST --source-state-root DIR [--remote-executable PATH] [--ssh-config FILE]]
                PLAN CORPUS PRIVATE_STATE SOURCE CARRY_STATE
                M2r2 b: carry each planned item whose capture refused (no
                capture record; its outcome CAPTURE_ABSENT) as files,
                byte-exact, by one recorded transfer session (`copy`, or
                `pull` from HOST with the remote source state under DIR) into
                its workspace, or beside it (the workspace path with SUFFIX
                appended), the session recorded in CARRY_STATE/<item>/dest.
                An equal destination file is adopted; a differing one is
                refused and never overwritten, and the item is then not
                carried. --bind ID=DEST_STATE binds an item to a session
                this transport already recorded in DEST_STATE (same source,
                same destination, completed, no refused path) instead of
                running one: nothing is transferred or written under the
                destination. Whatever landed, each directory below the
                destination holding a `.git` entry is resolved as Git
                resolves it (rev-parse, object alternates, symlinks in its
                administration); anything outside the carried tree refuses
                GIT_CARRY_POINTER_ATTACHED (the bytes stay for review). A
                carried item gets a `{item}-{blake3(SOURCE)}.carried` record
                and the outcome carried-as-files, which closure-report counts
                `carried` while the transfer record and the `.git`
                resolutions prove it. A refs-only item rides on a carried
                workspace item of the same source (via=). Every --item and
                --bind must name a planned eligible item (a bind, a workspace
                item), or the run refuses FIELD_DOMAIN_VIOLATION before any
                write; --bind-only refuses a selected workspace item with no
                bind. An item already carried whose carry still proves is
                kept with no session unless --recarry or a bind for it asks;
                a rerun that does not carry an item that was carried writes
                CAPTURE_ABSENT back (and for refs-only items riding on it).
                --dry-run reads only and prints what each item would do.
                One line per item
    estate-beside-retire [--delete] [--scan ROOT]... [--depth N] SUFFIX SOURCE PRIVATE_STATE [BESIDE ...]
                Operator ruling 2026-10-16 21:24Z: prove each beside tree
                (each BESIDE, and every directory named *SUFFIX whose live
                path exists under a --scan ROOT, to --depth levels, default
                6, never into a checkout) a subset of its live checkout (the
                name without SUFFIX): empty; or a linked worktree of the
                live repository or a standalone repository, tracked-clean,
                its HEAD reachable from a live ref (provenance aside),
                nothing its Git holds lost (no lock, per-worktree ref, own
                worktree or hook; every commit its refs, stash and reflogs
                name reachable in the live repository; a standalone tree's
                branches and tags held there by name, natively or under
                refs/NS/, else created there create-only), and every
                untracked or ignored entry present and equal in the live
                checkout or landable there as a file-level beside
                (FILE+SUFFIX; rebuildable roots counted, not reconciled).
                One line per tree: verdict=retirable with the counts and
                bytes, or verdict=refused reason=... (typed; nothing
                written), then a summary. Report only: nothing is written.
                --delete: each retirable tree's entries are reconciled into
                the live checkout and its refs created, it is proved again,
                ledgered in PRIVATE_STATE (beside-retire-TAG.retired: path,
                live path, HEAD, counts, refs, bytes, a standalone tree's
                config and info/exclude) and removed crash-safely (renamed
                aside, re-proved by prints, its administration and tree
                deleted); outcome=retired, or outcome=kept reason=... A
                rerun finishes a removal a crash cut short.
    closure-report [--attest LEDGER.json] [--dispositions LEDGER] [--transfer DEST_STATE ...] [--transfer-manifest MANIFEST] [--omissions-from-destination] PLAN CORPUS SOURCE PRIVATE_STATE [PRIVATE_STATE ...]
                Read-only: join PLAN with CORPUS's capture records and each
                PRIVATE_STATE's typed outcome records (legacy string records
                are mapped) and journals (later directories override earlier
                outcome records), with SOURCE the label estate-apply was
                given. Prints a bulkload.closure.v5 JSON ledger: every planned
                item is applied (workspace, exact current-capture journal),
                applied-beside (estate-apply --beside landed the capture beside
                a differing workspace; exact workspace-beside journal),
                refused-pending-review (typed code; a bare IO or FRAME_CODEC
                is not typed, nor is a code retired from the taxonomy),
                refused (a typed code a review disposes), or referenced-only
                (no workspace planned, exact refs journal), else unaccounted.
                Stale and foreign journals are listed. `verdict` is always
                the native one. `gate` is S4: every item accounted and every
                typed refusal reviewed, so it can fail while `verdict`
                passes and `unaccounted` is 0; it exits nonzero with
                CLOSURE_UNACCOUNTED when it fails. The omissions block
                reads each capture's .omissions sidecar; a capture from
                before it is read from its bundle's header (no omission ref:
                zero roots), and why an item stays unknown is printed on
                stderr (omissions-unknown REASON=N ...).
                Options, only before PLAN, each at most once:
                --omissions-from-destination: for a capture from before the
                sidecar whose bundle names an omission list, read that list
                by oid from the item's destination repository (reads only).
                --attest LEDGER.json: join a bulkload.closure-ledger.v1
                attestation ledger for items closed by audit, not by a verb.
                The ledger must name this PLAN and SOURCE (plan,
                source_label). Its rows are reported in a separate attested
                block and never override a native record or change
                `verdict`; a row closes only a natively unaccounted item
                whose own record is not an untyped or retired-code refusal,
                with matching source and current capture digest, typed
                refusal, basis other than native-closure-report, and
                evidence.
                --dispositions LEDGER: join the closure-dispose ledger bound
                to this PLAN (its path and its bytes) and SOURCE (S4)
                --transfer DEST_STATE (repeatable, WP3 PR 4): join every copy
                or pull session DEST_STATE records (transfer.closure.d) as
                rows of the same report and gate (one DEST_STATE named twice
                under any spelling, or a row joined twice, refuses
                FIELD_DOMAIN_VIOLATION): the session (applied when
                it ended; unaccounted when it never ended, left no record, or
                refused with a bare IO), one row per refused path (typed:
                pending review; SQLITE_INTEGRITY_CHECK_FAILED and a corrupt
                SQLITE_BACKUP_FAILED carry the ruled default abandon,
                OI-1003-Q148, unless a review says otherwise) and one covered
                row per live SQLite sidecar a snapshot covers
                --transfer-manifest MANIFEST: join exactly the transfers
                transfer-expect listed, bound to PLAN: a listed transfer with
                no record, or whose session started before the manifest's
                since, is unaccounted
    closure-dispose [--attest LEDGER.json] [--transfer DEST_STATE ...] [--transfer-manifest MANIFEST] LEDGER PLAN CORPUS SOURCE ITEM|--policy CODE accept|re-carry|abandon REVIEWER YYYY-MM-DD [PRIVATE_STATE ...]
                Record an operator review of a typed refusal (S4) in LEDGER,
                created when absent and bound to PLAN (its path and a digest
                of its bytes; PLAN must be a plan) and SOURCE. With ITEM: the
                refusal with CODE that planned ITEM holds now, read as
                closure-report reads it from CORPUS and each PRIVATE_STATE
                (at least one; --attest joins an attestation ledger, so an
                attested refusal can be reviewed). The row is bound to that
                refusal instance (the item's current capture and outcome
                record, printed as `instance`): it does not dispose a later
                refusal against another capture or record, and an ITEM that
                holds no such refusal refuses (RECEIPT_BINDING_INVALID). With
                --policy: a standing policy for every refusal with CODE under
                this PLAN and SOURCE, now or later; CORPUS and PRIVATE_STATE
                are not read. Rows are appended; the latest item row for the
                instance wins, then the latest policy. IO and FRAME_CODEC
                name no cause and refuse (FIELD_DOMAIN_VIOLATION), as does a
                CODE the taxonomy does not hold. --transfer and
                --transfer-manifest join transfer rows as closure-report
                does, so ITEM may name one
    transfer-expect MANIFEST PLAN copy SOURCE DEST DEST_STATE
    transfer-expect MANIFEST PLAN pull HOST SOURCE DEST DEST_STATE
                List one transfer a run expects in MANIFEST
                (bulkload.transfer-manifest.v1), created bound to PLAN with
                since = now when absent (create it when the run, or its
                closing pass, starts). Paths are named as copy and pull
                record them
    estate-reseat [--check] [--map SRC=DST]... [--lanes-from-plan]
                  [--plan PLAN --corpus CORPUS --source LABEL [--private-state DIR]...]
                  [--codex-source-root DIR]... [--landing-claude DIR] [--landing-codex DIR]
                  [--unmapped=report|by-id] [--note-marker TEXT] [--sessions-only]
                  [--waive ID]... [--waive-from FILE] [--retention-margin-days N]
                  [--managed-settings FILE]... [--by-id DIR]... [--no-default-by-id]
                  [--no-respell] [--no-regularize] [--replace-archived-dest ROOT]...
                  [--rebind-landing OLD=NEW]... [--undo]
                  [--no-claude] [--no-codex] SOURCE_HOME LANDING_HOME DEST_HOME STATE
                R0 (TIN-6587): re-seat the landed agent sessions (LANDING_HOME's
                .claude and .codex, the run's copy of SOURCE_HOME's) into
                DEST_HOME's live .claude and .codex, so claude --resume and
                codex resume in each lane's destination directory list and
                resume them, byte-identical. A lane maps a source working
                directory to a destination one: --map SRC=DST (longest prefix
                first), the plan's items with --lanes-from-plan, else the same
                path under the home's git directory, at its real path. Claude
                transcripts go to projects/<key of the real destination cwd>/
                as regular files with their source mtime, with their sidecar
                directory, per-session state and plans; Codex rollouts go to
                the same place under .codex as plain .jsonl (a .jsonl.zst is
                decompressed: Codex resumes a paginated thread only from the
                plain file) and their thread rows are inserted into its
                state_5.sqlite with the destination cwd (insert only; a row
                the destination already holds is never changed). A rollout
                with no snapshot row is placed with a row built from its
                header (OI-1003-Q198 D; RESEAT_INDEX_ROW_ABSENT when it cannot
                fill one); a destination row or rollout form for a thread is
                the destination's, nothing placed: destination-adopted when
                the file its row names holds the landed lines, else
                destination-holds (a file there, unproved) or
                destination-row-only (a row only), proof named, and
                RESEAT_INDEX_CONFLICT for a row naming another file. Every
                existing row whose cwd is spelled through the home's git or a
                --map symlink gets its cwd column re-spelled to the canonical
                path, lane or not, ledgered (OI-1003-Q198 (1); --no-respell;
                --check counts them). Symlinked destination transcripts
                (projects/*/*.jsonl) are first regularized into regular-file
                copies of their targets with the targets' mtime, the targets
                kept, ledgered (OI-1003-Q198 (2); --no-regularize); a target
                written during it is RESEAT_REGULARIZE_RACED with both copies
                kept (the line's kept field names a copy kept aside).
                --rebind-landing OLD=NEW (repeatable; A-106 follow-up): a
                placement row naming a landed file under OLD (as the rows
                spell it; OLD may be gone) is re-bound to the same file under
                NEW (must exist) instead of a collision, only when the row is
                this STATE's own (placed, adopted or continued), the
                destination is the single-link regular file it last wrote,
                and the bytes it placed are proved NEW's file's first bytes
                (BLAKE3 against the row's digest, or the destination's own
                read by descriptor): exactly them, rebound (nothing written);
                longer, re-bound then extended as usual; never inferred from
                size. --undo puts each rebind back (ledger only).
                --replace-archived-dest ROOT (repeatable; A-106: the landed
                version wins, the fork stays in the archive): a destination
                transcript, subagent sidecar or rollout that is a symlink
                whose fully resolved target is a regular file under ROOT's
                real path, and whose landed file is a fork of that target,
                is replaced by the landed file (staged beside it with its
                mtime, sealed, exchanged atomically, directory sealed),
                outcome replaced-archived; the link text, the target's real
                path, identity and BLAKE3 are ledgered first, the target is
                verified unchanged before and after the exchange and never
                written or removed; an equal landing leaves the link
                (occupied), a landing that is a prefix of the target too
                (dest-ahead), and a target a process holds open stays
                (occupied); a ROOT overlapping the live .claude or .codex
                (or containing DEST_HOME) is refused FIELD_DOMAIN_VIOLATION;
                links into ROOT are not regularized; --check counts
                replaced_archived, replace_pending, replace_equal_size and
                replace_proved. Run it, and its --undo, with no live session
                on the destination. --undo
                puts back every symlink and old cwd this STATE changed, where
                the destination still holds exactly what re-seat wrote and
                the archive target holds the recorded bytes (else
                RESEAT_UNDO_TARGET_CHANGED, nothing changed). Home
                and /private/tmp (and /tmp) sessions are placed resumable by
                ID (OI-1003-Q198 C; --by-id DIR adds a prefix,
                --no-default-by-id drops the defaults). One run at a time:
                each destination root and STATE are flocked (RESEAT_LOCKED);
                the engine's --min-free-percent floor holds for the bytes a
                pass would write (DESTINATION_SPACE_INSUFFICIENT). JSON Lines are placed and extended
                in whole lines only. Never overwrites: a copy already there is
                adopted when equal, extended when a byte prefix (append-only
                files), and otherwise kept and reported (RESEAT_DEST_DIVERGED);
                a placed file changed at the destination is the destination's
                from then on (RESEAT_DEST_CONTINUED, RESEAT_EXTEND_RACED);
                two landed files for one destination path are
                RESEAT_LANE_COLLISION. Every placement is ledgered in
                STATE/reseat.sqlite: an unchanged rerun reads 0 landed bytes
                (read_reseat_landing_bytes) and a later pass proves the placed
                bytes and appends only what grew. Refuses before writing:
                RESEAT_LANDING_ABSENT (a landed .claude or .codex missing; use
                --no-claude / --no-codex), RESEAT_RETENTION_UNSAFE (the
                smallest cleanupPeriodDays of the destination's managed, user
                and lane settings would delete a re-seated file within
                --retention-margin-days, default 365), RESEAT_INDEX_ABSENT (no
                destination Codex index to register in). --unmapped=by-id
                places sessions with no lane under their source key / cwd,
                resumable by ID only (default: report). --check is the
                read-only R0 acceptance check: per lane, the transcripts and
                rollouts the pickers read there hold the landed ones (exactly,
                or continued at the destination), Git is estate-diff identical
                (--plan, or --sessions-only to waive), the hand-off note holds
                the marker (--note-marker); every session or thread without a
                lane, or failed by ID, fails it unless --waive (or
                --waive-from, one ID per line) names it; exits RESEAT_R0_FAILED
                unless all of that holds and something was checked. One
                bulkload.estate-reseat.v1 JSON line per row
    git-import REPO BUNDLE SOURCE
                Preserve bundle refs in a content-addressed carry namespace
    git-restore BUNDLE ABSENT_DEST SOURCE
                Restore captured staged/unstaged work into a new repository
    git-restore-linked BUNDLE REPOSITORY ABSENT_DEST SOURCE
                Restore captured work into a new linked worktree without switching others
    git-repair-missing-index BUNDLE REPOSITORY SOURCE NEW_RECEIPT [PLAN CORPUS PRIVATE_STATE]
                Create a missing same-HEAD staged index only; never rewrite payload.
                With PLAN CORPUS PRIVATE_STATE, bind BUNDLE to the planned item
                whose CORPUS capture it is and record apply-style receipts in
                PRIVATE_STATE: journal refs-imported, outcome index-repaired
                (or refused with its code), so closure-report reads it natively.
                An item that plans a workspace never binds (estate-apply
                restores it)
    git-attach-matching-payload BUNDLE REPOSITORY DESTINATION SOURCE NEW_RECEIPT
                Attach exact matching payload using existing common Git administration
    git-attach-standalone-payload BUNDLE DESTINATION SOURCE NEW_RECEIPT ORIGIN_FROM ORIGIN_TO
                Attach exact payload with retained config and explicit local origin mapping
    git-restore-registered-payload BUNDLE REPOSITORY DESTINATION ADMIN SOURCE NEW_RECEIPT
                Restore absent payload only, preserving matching retained registration/index
    snapshot SOURCE OUTPUT [MAX_STEPS]
                Capture live SQLite through its online backup API
    compose BASE INCOMING OUTPUT SOURCE_ID [MAX_STEPS]
                Compose retained SQLite snapshots into a private candidate
    compose-state BASE INCOMING OUTPUT SOURCE_ID SOURCE_HOME DEST_HOME [MAX_STEPS]
                Compose Codex state with retained rollout path mapping
    hydrate-state SNAPSHOT SOURCE_HOME DEST_HOME MAX_BYTES JOBS [GZIP ZSTD]
                Add missing raw rollouts from retained compressed files; never replace
    apply-state-candidate LIVE BASE CANDIDATE MAX_ROWS
                Explicit external live-state import; requires operator authorization
    help        Print this message

BOUNDARIES:
    --durability=group (the default) seals each file with a barrier and makes
    each group of files durable with one SQLite commit; --durability=strict
    fully flushes every file (A/B comparison). pull passes strict to serve.
    --source-ledger-sync=relaxed (the default; WP0(g), OI-1003-Q37) commits
    the SOURCE ledger's rows without syncing them: a power loss may lose the
    newest rows, and each lost row costs at most one more read of its seat.
    A failed row commit is counted (source_ledger_commit_failed), not fatal.
    The store's creation commit (its authority) and every destination
    commit stay fully synced. --source-ledger-sync=full syncs every row
    commit and fails the session on the first failed one (A/B comparison).
    pull passes full to serve.
    --min-free-percent=N (0-100, default 25): DESTINATION_SPACE_INSUFFICIENT
    when planned bytes would leave the destination filesystem (statvfs) with
    less than N% free, or, inside a project-quota tree (where statvfs
    reports the quota; Linux), the quota with less than N% of its limit
    free. The refusal says which limit bound and both numbers (a
    space_refusal line; counters space_refused_filesystem and
    space_refused_quota). copy/pull refuse each entry that does not fit, as a
    value, before requesting its content: the bytes of every entry decided
    and not yet Held count against a probe refreshed on each group commit,
    so the session continues and stays resumable. estate-apply refuses before
    any item, planning the bundle bytes each destination repository does not
    already hold (a lower bound).
    --priority=background|normal|auto (WP0(f)): serve, snapshot,
    git-carry-estimate, git-export and copy read a live source, so they enter
    background CPU and IO priority before anything else (Linux nice 19 and the
    idle IO class; Darwin IOPOL_THROTTLE, QOS_CLASS_BACKGROUND and nice 19),
    inherited by every thread and child; other verbs run at normal priority.
    estate-capture defaults to auto (M2 item 3): background when a terminal
    was read or written within 300 s, normal otherwise. --priority=normal is
    the explicit opt-out (gate (a)); every counters line records priority=
    and priority_from=default|flag|auto (and priority_auto=idle|interactive|
    unknown).
    --escalate-after=SECONDS (M2 item 3; estate-capture's default 300, any
    other verb only when given; 0 off): a process at background priority
    whose counters (and, on Linux, its descendants' IO) do not move for
    SECONDS moves its IO (Linux: and its descendants') back to the class its
    nice value implies, once; the counters line then says
    priority_escalate_after=SECONDS, and priority_escalations,
    priority_escalated_ns, priority_escalated_threads and
    priority_escalation_failures record it.
    copy/pull require an existing destination directory.
    copy/pull replace only their own untouched outputs, when the source seat
    changed (superseding publish, WP0(d)); any other divergent destination
    file is preserved and refused DESTINATION_OCCUPIED, and remembered: an
    unchanged rerun does not read its seat again. On a file system with no
    atomic exchange a changed seat is refused
    DESTINATION_EXCHANGE_UNSUPPORTED before it is staged. With
    --sqlite=refuse (the default) they refuse live SQLite files, sniffing
    each once (source_sniff_bytes on the counters line) and remembering the
    refusal, and every -wal, -shm and -journal by name, except a Git ref or
    reflog so named: below a directory named refs or logs, reading as a ref
    or a reflog line, beside no SQLite database (M2r2).
    --sqlite=snapshot (#218; copy and pull, carried to serve in its Open):
    a database is carried as a backup-API snapshot the source takes into its
    private state (SOURCE_STATE/sqlite-snapshots, at most 4 GiB of slots
    at once, and never past the --min-free-percent floor of that
    filesystem: BUDGET_EXCEEDED), never its live bytes, and verified
    (integrity_check, at the source and again at the destination) before
    it is published, in journal mode DELETE. A file without the SQLite
    magic beside a -wal that holds frames (an encrypted store) is refused
    SQLITE_STATE_CHANGED, never carried raw. An unchanged store is reused with nothing opened: its key is its
    row and its -wal's identity. A -wal, -shm or -journal beside a database
    (a Git ref or reflog so named aside) is never carried: it is covered when its database was carried as a
    snapshot, and refused SQLITE_STATE_CHANGED otherwise. A changed store
    supersedes its own older snapshot output through the same exchange
    (OI-1003-Q146) only when no -wal, -journal or -shm sits beside the
    path here, checked at the decision and again just before the exchange;
    a path with one beside it, or holding a file this store did not land
    or that was touched since, is refused DESTINATION_OCCUPIED and left as
    it is. A store failing integrity_check is refused
    SQLITE_INTEGRITY_CHECK_FAILED (disposition abandon, OI-1003-Q148).
    The default stays --sqlite=refuse (OI-1003-Q147): a pull that must
    carry databases passes --sqlite=snapshot.
    The source half refuses SQLITE_SOURCE_AS_ROOT as root
    (the whole session, before anything is opened) and
    SQLITE_SOURCE_NOT_OWNER for a database another user owns. The
    transfer line reports sqlite_mode=, sqlite_snapshots= and
    sqlite_sidecars_covered=.
    They enumerate the source each run; completed content is resumable.
    File manifests allow 131072 chunks and frames at most 8 MiB; oversized files refuse.
    Git-native divergent union is not supplied by copy/pull.
    compose commands write offline candidates, never install live databases.
    snapshot, compose, compose-state, hydrate-state and apply-state-candidate
    refuse SQLITE_SOURCE_AS_ROOT when run as root (OI-1003-Q76): opened by
    root, SQLite re-applies ownership to a database's -wal and -shm, a source
    metadata write. Run them as the database's owner.
    Capture omits a fixed rebuildable set (target, node_modules, .venv, ...) at
    any depth when Git tracks nothing beneath it, records each omitted root and
    its size as custody, and carries every other untracked and ignored file.
    --include-rebuildable carries the rebuildable set too, at full fidelity.
    A foreign repository nested in a checkout, or a gitlink, is custody named
    per nest (path, admin, HEAD, unpushed commits, carried-ignored count) on
    git-export stderr and in every estate receipt; its tracked content and
    history are its own item and are not carried, its ignored files are
    (R-N89) unless the nest is planned as its own estate item, which then
    owns them and is named as carried-by=<item> (R-N114). A nest with any
    staged, unstaged or untracked change, hidden index flags, a conversion
    attribute, a stash, a detached-only commit, an operation in progress,
    filter commands, a populated submodule, or outer-tracked paths under it
    refuses (R-N73, R-N83, R-N115).
    estate-capture tolerates refs and worktree seats moving under a pass (R25):
    the item reports outcome=captured-with-drift with reason=drift=N, the N
    rows ride in the CORPUS {bundle}.drift sidecar (the bundle's
    refs/carry-export/capture-drift-v1 names the export's share), drifted
    seats are omitted from the bundle, and the next estate-capture re-reads
    exactly those seats plus any racy seat (stamped within 2 s of the pass
    start) and reuses every other blob (capture-extended-from-drift,
    source_bytes_read). A pass reusing nothing it was offered says why:
    reuse_unavailable=shallow|retained-unreadable|pass-start-unrecorded|
    future-stamp|manifest-mismatch. A changed capture reuses its retained
    capture's blobs from that capture's {bundle}.reuse manifest and fetches
    the bundle only for a seat whose blob the source object store does not
    hold (counted: reuse_dirty_misses).
    A gc, repack or prune that rewrites the source's object store under a
    pass, so that a Git child reading through it fails, is drift custody too:
    outcome=deferred-with-drift with one ObjectStoreRewritten objects/pack
    row, no capture record, and the next estate-capture captures the
    rewritten store.
    A bundle that drifted under its export carries an in-band marker, and
    estate-apply and every git-restore/import/attach/repair verb refuse it
    with CAPTURE_DRIFTED; run estate-capture again first. An agent
    committing, switching, staging or stashing under a pass is drift
    custody too (#38, OI-1003-Q161, S5): HeadMoved HEAD, IndexRewritten
    index, StashMoved refs/stash and NestedWorktreeMoved rows, the bundle
    carries the HEAD, index and stash its snapshot read, and the next
    estate-capture carries the new ones. Exclude, config, shallow or
    nested-repository changes under a pass still refuse
    GIT_AUTHORITY_CHANGED;
    git-export never tolerates drift.

COUNTERS:
    Every verb ends with machine-readable key=value lines: `counters` (bytes
    read/written per stage, BLAKE3 bytes, flushes by kind, SQLite commits by
    kind, elapsed_ns). copy and pull also print `transfer_timing` and
    `chunk_timing`. copy and pull print them on stdout; serve prints its
    source-side lines on stderr (stdout is the wire; ssh relays stderr), and
    every other verb prints on stderr so its stdout contract is unchanged.
";

/// Verbs that read a live source on the host they run on. Each enters
/// background CPU and IO priority before anything else unless
/// `--priority=normal` opts out, and the opt-out is recorded (WP0(f),
/// OI-1003-Q17), except the [`AUTO_VERBS`], whose default is `auto` (M2
/// item 3). `copy` is here because it reads the source in-process (it is
/// gate (a)'s verb); `pull` is not, since its source half is the remote
/// `serve`.
const SOURCE_SIDE_VERBS: &[&str] = &[
    "serve",
    "estate-capture",
    "estate-move-source",
    "snapshot",
    "git-carry-estimate",
    "git-export",
    "copy",
];

/// Verbs whose default class is `auto` (M2 item 3): background when a
/// person is at the host, normal otherwise (`priority::presence`).
const AUTO_VERBS: &[&str] = &["estate-capture", "estate-move-source"];

/// A `--priority=` value: a class, or `auto`.
#[derive(Clone, Copy)]
enum Requested {
    Class(bulkload_agent::priority::PriorityClass),
    Auto,
}

impl std::str::FromStr for Requested {
    type Err = BulkloadRefusal;

    fn from_str(value: &str) -> Result<Self> {
        if value == "auto" {
            Ok(Self::Auto)
        } else {
            value.parse().map(Self::Class)
        }
    }
}

/// Where a verb's priority class came from, as its counters line records it.
#[derive(Clone, Copy)]
struct Priority {
    class: bulkload_agent::priority::PriorityClass,
    /// `true` when `--priority=` chose it, `false` for the verb's default.
    explicit: bool,
    /// What `auto` found, when the class came from it.
    auto: Option<bulkload_agent::priority::Presence>,
    /// The starvation watcher's threshold in seconds, when one runs.
    escalate_after: Option<u64>,
}

impl Priority {
    fn render(self) -> String {
        let from = match (self.auto, self.explicit) {
            (Some(_), _) => "auto",
            (None, true) => "flag",
            (None, false) => "default",
        };
        let mut line = format!("priority={} priority_from={from}", self.class.label());
        if let Some(presence) = self.auto {
            line.push_str(" priority_auto=");
            line.push_str(presence.label());
        }
        if let Some(after) = self.escalate_after {
            use std::fmt::Write as _;
            let _ = write!(line, " priority_escalate_after={after}");
        }
        line
    }
}

#[allow(clippy::too_many_lines)] // One dispatch table.
fn main() -> ExitCode {
    let started = std::time::Instant::now();
    // OI-1001-Q2: the binary keeps a 25% free-space floor unless told otherwise.
    bulkload_agent::space::set_min_free_percent(bulkload_agent::space::DEFAULT_MIN_FREE_PERCENT);
    let mut requested = None;
    let mut escalate_after = None;
    let mut args = match global_flags(
        std::env::args_os().skip(1).collect(),
        (&mut requested, &mut escalate_after),
    ) {
        Ok(args) => args.into_iter(),
        Err(refusal) => {
            eprintln!("bulkload-agent: refused: {refusal}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let command = args.next();
    let verb = command
        .as_ref()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_owned();
    // WP0(f): before any thread or child exists, so all of them inherit it.
    let priority = match enter_priority(&verb, requested, escalate_after, started) {
        Ok(priority) => priority,
        Err(refusal) => {
            eprintln!("bulkload-agent: refused: {refusal}");
            return ExitCode::FAILURE;
        }
    };
    // macOS starts at 256 open files; take the hard limit the host allows.
    let _ = bulkload_agent::limits::raise_descriptor_limit();
    let outcome = match command.as_ref().and_then(|value| value.to_str()) {
        Some("selftest") => selftest(),
        Some("walk") => {
            if let Some(path) = args.next() {
                walk_command(Path::new(&path))
            } else {
                eprintln!("bulkload-agent: walk requires a PATH\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
        Some(
            "copy" | "pull" | "snapshot" | "compose" | "compose-state" | "git-export"
            | "git-import" | "git-restore" | "git-restore-linked",
        ) => native_command(
            command
                .as_ref()
                .and_then(|value| value.to_str())
                .unwrap_or(""),
            &args.collect::<Vec<_>>(),
        ),
        Some("git-carry-estimate") => estimate_command(&args.collect::<Vec<_>>()),
        Some("hydrate-state") => hydrate_command(&args.collect::<Vec<_>>()),
        Some("git-repair-missing-index") => repair_index_command(&args.collect::<Vec<_>>()),
        Some("git-restore-registered-payload") => registered_command(&args.collect::<Vec<_>>()),
        Some("git-attach-matching-payload") => attach_payload_command(&args.collect::<Vec<_>>()),
        Some("git-attach-standalone-payload") => {
            attach_standalone_command(&args.collect::<Vec<_>>())
        }
        Some(
            name @ ("estate-add" | "estate-add-batch" | "estate-show" | "estate-capture"
            | "estate-apply" | "estate-diff" | "estate-handoff" | "estate-retarget"
            | "estate-unseat"),
        ) => estate_command(name, &args.collect::<Vec<_>>()),
        Some("estate-carry-files") => carry_files_command(&args.collect::<Vec<_>>()),
        Some("estate-beside-retire") => beside_retire_command(&args.collect::<Vec<_>>()),
        Some(name @ ("estate-move-source" | "estate-move")) => estate_move::command(name, args),
        Some("apply-state-candidate") => apply_state_command(&args.collect::<Vec<_>>()),
        Some("closure-report") => closure_command(&args.collect::<Vec<_>>()),
        Some("closure-dispose") => dispose_command(&args.collect::<Vec<_>>()),
        Some("transfer-expect") => expect_command(&args.collect::<Vec<_>>()),
        Some("estate-reseat") => reseat_command(&args.collect::<Vec<_>>()),
        Some("serve") => {
            let (input, output) = (std::io::stdin(), std::io::stdout());
            bulkload_agent::transfer::tune_stream(&input);
            bulkload_agent::transfer::tune_stream(&output);
            // The reader thread owns stdin for the session; the process
            // exit closes the transport.
            bulkload_agent::transfer::serve(input, &mut output.lock())
        }
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("bulkload-agent: unknown subcommand {other:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        None => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    report_counters(&verb, started, priority);
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("bulkload-agent: refused: {refusal}");
            // M2 round 2, item 1: which limit refused, and both numbers;
            // only for this verb's own space refusal, and once (review A3).
            if refusal == BulkloadRefusal::DestinationSpaceInsufficient {
                if let Some(space) = bulkload_agent::space::take_refusal() {
                    eprintln!("bulkload-agent: space: {}", space.fields());
                }
            }
            ExitCode::FAILURE
        }
    }
}

/// The verb's class: `requested`, or its default; entered before any
/// thread or child exists, so all of them inherit it (WP0(f)). M2 item 3:
/// `auto` (estate-capture's default) is background only when a person is at
/// the host, and a background process of an `auto` verb, or of any verb
/// given `--escalate-after`, starts the bounded-starvation watcher.
fn enter_priority(
    verb: &str,
    requested: Option<Requested>,
    escalate_after: Option<u64>,
    started: std::time::Instant,
) -> Result<Priority> {
    use bulkload_agent::priority::PriorityClass;
    let wanted = requested.unwrap_or_else(|| {
        if AUTO_VERBS.contains(&verb) {
            Requested::Auto
        } else if SOURCE_SIDE_VERBS.contains(&verb) {
            Requested::Class(PriorityClass::Background)
        } else {
            Requested::Class(PriorityClass::Normal)
        }
    });
    let priority = match wanted {
        Requested::Class(class) => Priority {
            class,
            explicit: requested.is_some(),
            auto: None,
            escalate_after: None,
        },
        Requested::Auto => {
            let presence = bulkload_agent::priority::presence();
            Priority {
                class: presence.class(),
                explicit: requested.is_some(),
                auto: Some(presence),
                escalate_after: None,
            }
        }
    };
    let mut priority = priority;
    if priority.class == PriorityClass::Background {
        bulkload_agent::priority::enter_background()?;
        // Escalation is on by default only for the `auto` verbs; any other
        // verb keeps WP0(f) as ratified unless the flag asks for it.
        let after = escalate_after.or_else(|| {
            AUTO_VERBS
                .contains(&verb)
                .then_some(bulkload_agent::priority::DEFAULT_ESCALATE_SECS)
        });
        if let Some(after) = after.filter(|after| *after > 0) {
            // After the class is set, so the watcher inherits it like any
            // other thread.
            bulkload_agent::priority::watch_starvation(
                std::time::Duration::from_secs(after),
                started,
            );
            priority.escalate_after = Some(after);
        }
    }
    Ok(priority)
}

/// Remove `--durability=MODE`, `--source-ledger-sync=MODE` and
/// `--min-free-percent=N` from the arguments and apply them process-wide,
/// `--priority=CLASS|auto` into `priority` and `--escalate-after=SECONDS`
/// into `escalate_after`.
fn global_flags(
    args: Vec<std::ffi::OsString>,
    (priority, escalate_after): (&mut Option<Requested>, &mut Option<u64>),
) -> Result<Vec<std::ffi::OsString>> {
    let mut rest = Vec::with_capacity(args.len());
    for arg in args {
        if let Some(class) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--priority="))
        {
            *priority = Some(class.parse()?);
            continue;
        }
        if let Some(seconds) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--escalate-after="))
        {
            *escalate_after = Some(
                seconds
                    .parse()
                    .map_err(|_| BulkloadRefusal::FieldDomainViolation)?,
            );
            continue;
        }
        if let Some(mode) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--sqlite="))
        {
            bulkload_agent::transfer::set_sqlite_mode(bulkload_agent::transfer::parse_sqlite_mode(
                mode,
            )?);
            continue;
        }
        if let Some(mode) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--source-ledger-sync="))
        {
            bulkload_agent::durable::set_ledger_sync(mode.parse()?);
            continue;
        }
        match arg
            .to_str()
            .and_then(|value| value.strip_prefix("--durability="))
        {
            Some(mode) => bulkload_agent::durable::set_durability(mode.parse()?),
            None => match arg
                .to_str()
                .and_then(|value| value.strip_prefix("--min-free-percent="))
            {
                Some(percent) => bulkload_agent::space::set_min_free_percent(
                    bulkload_agent::space::parse_percent(percent)?,
                ),
                None => rest.push(arg),
            },
        }
    }
    Ok(rest)
}

// OI-1001-Q2: bulkload's own closure gate. The JSON ledger goes to stdout
// whether or not it passes; the verdict is the exit status.
fn closure_command(args: &[std::ffi::OsString]) -> Result<()> {
    // #95, #133: `--attest LEDGER.json` and (S4) `--dispositions LEDGER` are
    // options only before PLAN, each at most once. Everywhere else every
    // argument is positional, so a PRIVATE_STATE literally named `--attest`
    // is still a state.
    let mut attest = None;
    let mut dispositions = None;
    let mut manifest = None;
    let mut transfers = Vec::new();
    let mut read_destinations = false;
    let mut positional = args;
    loop {
        // M2 round 2, d: read a pre-sidecar capture's omission list from its
        // destination repository's objects; only before PLAN.
        if let [flag, rest @ ..] = positional {
            if flag == "--omissions-from-destination" {
                if read_destinations {
                    return Err(BulkloadRefusal::FieldDomainViolation);
                }
                read_destinations = true;
                positional = rest;
                continue;
            }
        }
        // WP3 PR 4: `--transfer DEST_STATE`, repeatable, also only before PLAN.
        if let [flag, state, rest @ ..] = positional {
            if flag == "--transfer" {
                transfers.push(PathBuf::from(state));
                positional = rest;
                continue;
            }
        }
        let (slot, ledger, rest) = match positional {
            [flag, ledger, rest @ ..] if flag == "--attest" => (&mut attest, ledger, rest),
            [flag, ledger, rest @ ..] if flag == "--dispositions" => {
                (&mut dispositions, ledger, rest)
            }
            // Review finding 3: the run's expected transfers, bound to PLAN.
            [flag, path, rest @ ..] if flag == "--transfer-manifest" => (&mut manifest, path, rest),
            [flag]
                if flag == "--attest"
                    || flag == "--dispositions"
                    || flag == "--transfer"
                    || flag == "--transfer-manifest" =>
            {
                return Err(BulkloadRefusal::RequiredFieldMissing)
            }
            _ => break,
        };
        if slot.replace(PathBuf::from(ledger)).is_some() {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        positional = rest;
    }
    let [plan, corpus, source, states @ ..] = positional else {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    };
    if states.is_empty() {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let source = source.to_str().ok_or(BulkloadRefusal::PathNotPortable)?;
    let states: Vec<PathBuf> = states.iter().map(PathBuf::from).collect();
    let ledger = bulkload_agent::estate::ledger_with(
        Path::new(plan),
        Path::new(corpus),
        source,
        &states,
        read_destinations,
    )?;
    let mut report = bulkload_agent::closure::Report::from_ledger(&ledger);
    join_transfer_inputs(
        &mut report,
        &transfers,
        manifest.as_deref(),
        Path::new(plan),
    )?;
    if let Some(attest) = attest {
        report.attest(&bulkload_agent::closure::AttestationLedger::read(
            &attest,
            Path::new(plan),
            source,
        )?)?;
    }
    if let Some(dispositions) = dispositions {
        report.dispose(&bulkload_agent::disposition::Ledger::read(
            &dispositions,
            Path::new(plan),
            source,
        )?);
    }
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", report.to_json()).refuse_at("main::closure_command")?;
    stdout.flush().refuse_at("main::closure_command")?;
    // M2 round 2, d: why each unknown item's omissions are unknown.
    if !report.omissions.unknown_because.is_empty() {
        let reasons = report
            .omissions
            .unknown_because
            .iter()
            .map(|(reason, items)| format!("{reason}={items}"))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("omissions-unknown {reasons}");
    }
    report.gate()
}

// WP3 PR 4 and review finding 3: the transfer rows a report joins, from
// `--transfer` states and a `--transfer-manifest` bound to PLAN.
fn join_transfer_inputs(
    report: &mut bulkload_agent::closure::Report,
    transfers: &[PathBuf],
    manifest: Option<&Path>,
    plan: &Path,
) -> Result<()> {
    report.join_transfers(transfers)?;
    if let Some(manifest) = manifest {
        report.join_manifest(&bulkload_agent::transfer_outcome::Manifest::read(
            manifest, plan,
        )?)?;
    }
    Ok(())
}

// S4 (WP3 PR 3): record one operator review of a typed refusal. Positional,
// like estate-add; `--policy` stands in for ITEM. An item review is bound to
// the refusal instance the report holds now, so the verb reads the same
// inputs as closure-report. Prints the recorded row.
fn dispose_command(args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::disposition::{self, Decision, Row, Scope};
    fn text(value: &std::ffi::OsString) -> Result<&str> {
        value.to_str().ok_or(BulkloadRefusal::PathNotPortable)
    }
    let mut attest = None;
    let mut manifest = None;
    let mut transfers = Vec::new();
    let mut positional = args;
    loop {
        match positional {
            // Each at most once.
            [flag, value, rest @ ..] if flag == "--attest" || flag == "--transfer-manifest" => {
                let slot = if flag == "--attest" {
                    &mut attest
                } else {
                    &mut manifest
                };
                if slot.replace(PathBuf::from(value)).is_some() {
                    return Err(BulkloadRefusal::FieldDomainViolation);
                }
                positional = rest;
            }
            // WP3 PR 4: transfer rows can be reviewed like planned items.
            [flag, state, rest @ ..] if flag == "--transfer" => {
                transfers.push(PathBuf::from(state));
                positional = rest;
            }
            [flag]
                if flag == "--attest" || flag == "--transfer" || flag == "--transfer-manifest" =>
            {
                return Err(BulkloadRefusal::RequiredFieldMissing)
            }
            _ => break,
        }
    }
    let [ledger, plan, corpus, source, target, code, decision, reviewer, date, states @ ..] =
        positional
    else {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    };
    let (plan, source, code) = (Path::new(plan), text(source)?, text(code)?);
    // WP3 PR 4: a transfer row the report joined may be reviewed too.
    let mut joined = Vec::new();
    let scope = match text(target)? {
        // A standing policy names no instance: nothing else is read.
        "--policy" => Scope::Policy,
        item => {
            // Validate the row's own fields before reading any state.
            if !bulkload_agent::outcome::is_typed_code(code) {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
            if states.is_empty() {
                return Err(BulkloadRefusal::RequiredFieldMissing);
            }
            let states: Vec<PathBuf> = states.iter().map(PathBuf::from).collect();
            let estate = bulkload_agent::estate::ledger(plan, Path::new(corpus), source, &states)?;
            let mut report = bulkload_agent::closure::Report::from_ledger(&estate);
            join_transfer_inputs(&mut report, &transfers, manifest.as_deref(), plan)?;
            if let Some(attest) = &attest {
                report.attest(&bulkload_agent::closure::AttestationLedger::read(
                    attest, plan, source,
                )?)?;
            }
            // The item must hold this typed refusal now: no review is
            // written ahead of the refusal it disposes.
            let instance = report
                .reviewable(item, code)
                .ok_or(BulkloadRefusal::ReceiptBindingInvalid)?;
            joined.extend(
                report
                    .rows
                    .iter()
                    .filter(|row| row.kind != bulkload_agent::closure::RowKind::Estate)
                    .map(|row| row.item.clone()),
            );
            Scope::Item {
                item: item.to_owned(),
                instance: instance.to_owned(),
            }
        }
    };
    let row = Row::new(
        scope,
        code,
        Decision::parse(text(decision)?)?,
        text(reviewer)?,
        text(date)?,
    )?;
    let recorded = disposition::record_joined(Path::new(ledger), plan, source, row, &joined)?;
    let mut stdout = std::io::stdout().lock();
    if let Some(row) = recorded.rows.last() {
        writeln!(
            stdout,
            "disposition recorded scope={} item={} instance={} refusal={} decision={} reviewer={:?} date={} rows={}",
            row.basis(),
            row.item().unwrap_or("-"),
            row.instance().unwrap_or("-"),
            row.code(),
            row.decision().name(),
            row.reviewer(),
            row.date(),
            recorded.rows.len(),
        )
        .refuse_at("main::dispose_command")?;
    }
    stdout.flush().refuse_at("main::dispose_command")?;
    Ok(())
}

// The only capture-policy word the agent accepts; anything else is a typo, and
// a typo must not silently change what a 40-minute capture carries.
fn capture_policy(
    argument: Option<&std::ffi::OsString>,
) -> Result<bulkload_agent::git_carry::CapturePolicy> {
    match argument.and_then(|value| value.to_str()) {
        None => Ok(bulkload_agent::git_carry::CapturePolicy::default()),
        Some("--include-rebuildable") => {
            Ok(bulkload_agent::git_carry::CapturePolicy::including_rebuildable())
        }
        Some(_) => Err(BulkloadRefusal::FieldDomainViolation),
    }
}

fn export_command(
    repo: &Path,
    capture: &Path,
    policy: bulkload_agent::git_carry::CapturePolicy,
) -> Result<()> {
    let export =
        bulkload_agent::git_carry::export_repository_with_policy(repo, capture, None, policy)?;
    // Custody goes to stderr so stdout stays exactly the bundle path.
    for omission in &export.omitted {
        eprintln!(
            "omitted-rebuildable path={} bytes={} entries={}",
            String::from_utf8_lossy(&omission.rel_path),
            omission.bytes,
            omission.entries
        );
    }
    // Escaped and quoted: a newline in a nest path cannot forge a line (F8).
    for nested in &export.nested_repositories {
        eprintln!("{}", nested.receipt_line());
    }
    println!("{}", export.bundle.display());
    Ok(())
}

fn native_command(command: &str, args: &[std::ffi::OsString]) -> Result<()> {
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    if command == "git-restore-linked" && args.len() == 4 {
        let source = args
            .get(3)
            .and_then(|value| value.to_str())
            .ok_or(BulkloadRefusal::PathNotPortable)?;
        bulkload_agent::git_carry::restore_linked(path(0)?, path(1)?, path(2)?, source)?;
        println!("linked restoration complete");
        return Ok(());
    }
    match command {
        "git-restore" if args.len() == 3 => {
            let source = args
                .get(2)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            bulkload_agent::git_carry::restore_bundle(path(0)?, path(1)?, source)?;
            println!("{}", path(1)?.display());
            Ok(())
        }
        "git-export" if (2..=3).contains(&args.len()) => {
            export_command(path(0)?, path(1)?, capture_policy(args.get(2))?)
        }
        "git-import" if args.len() == 3 => {
            let source = args
                .get(2)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let count = bulkload_agent::git_carry::import_bundle(path(0)?, path(1)?, source)?;
            println!("{count}");
            Ok(())
        }
        "copy" if args.len() == 4 => {
            // WP3 PR 4: the session's outcomes are closure rows.
            copy_recorded(path(0)?, path(1)?, path(2)?, path(3)?)
        }
        "pull" if (5..=7).contains(&args.len()) => pull_recorded(args),
        "snapshot" if (2..=3).contains(&args.len()) => {
            bulkload_agent::provider_sqlite::snapshot(path(0)?, path(1)?, steps(args.get(2))?)?;
            println!("snapshot complete");
            Ok(())
        }
        "compose" if (4..=5).contains(&args.len()) => {
            let source_id = args
                .get(3)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let stats = bulkload_agent::provider_sqlite::compose_snapshots(
                path(0)?,
                path(1)?,
                path(2)?,
                source_id,
                steps(args.get(4))?,
            )?;
            println!(
                "inserted={} equivalent={} preserved={} unresolved={} paths_corrected={} unavailable_rollouts={}",
                stats.inserted, stats.equivalent, stats.preserved, stats.unresolved,
                stats.paths_corrected, stats.unavailable_rollouts
            );
            Ok(())
        }
        "compose-state" if (6..=7).contains(&args.len()) => {
            let source_id = args
                .get(3)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let mapping = bulkload_agent::provider_sqlite::PathMapping {
                source_home: path(4)?,
                destination_home: path(5)?,
            };
            let stats = bulkload_agent::provider_sqlite::compose_state_snapshots(
                path(0)?,
                path(1)?,
                path(2)?,
                source_id,
                steps(args.get(6))?,
                &mapping,
            )?;
            println!(
                "inserted={} equivalent={} preserved={} unresolved={} paths_corrected={} unavailable_rollouts={}",
                stats.inserted, stats.equivalent, stats.preserved, stats.unresolved,
                stats.paths_corrected, stats.unavailable_rollouts
            );
            Ok(())
        }
        _ => Err(BulkloadRefusal::RequiredFieldMissing),
    }
}

// Read-only on both sides: no fetch, no object or ref write (R-N60 baseline).
//
// Each pair prints one whole block, built before any of it is written. A
// refused pair prints `refused=CODE` in place of its measurements, with
// `refused_reason=`, `stderr_class=` and the store's fields when present; later
// pairs still run, and the verb exits nonzero with the first refusal (F13).
// A child's stderr is classified, never echoed (R-N121). With `--state-dir`,
// its raw bytes go to a private 0600 file whose path is printed as
// `stderr_file=`, with a digest keyed by the state dir's key; without it,
// nothing is written and no digest is printed.
// The estimate verb's `Refused` carries a `BulkloadRefusal`, whose
// path-carrying variants (#53) put it just over clippy's 128-byte
// large-error threshold; see git_carry::estimate.
#[allow(clippy::result_large_err)]
fn estimate_command(args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::git_carry::estimate::{estimate_with, Destination, Refused, StderrStore};
    let (state_dir, args) = match args {
        [flag, dir, rest @ ..] if flag == "--state-dir" => (Some(Path::new(dir)), rest),
        _ => (None, args),
    };
    let store = state_dir.map(StderrStore::open).transpose()?;
    if args.is_empty() || !args.len().is_multiple_of(2) {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let mut stdout = std::io::stdout().lock();
    let mut first = None;
    for (index, pair) in args.chunks_exact(2).enumerate() {
        let [source, destination] = pair else {
            return Err(BulkloadRefusal::RequiredFieldMissing);
        };
        let source = Path::new(source);
        let mut block = vec![
            format!("source={}", source.display()),
            format!("destination={}", Path::new(destination).display()),
        ];
        let outcome = Destination::parse(destination)
            .map_err(Refused::from)
            .and_then(|destination| estimate_with(source, &destination, store.as_ref()));
        match outcome {
            Ok(estimate) => block.extend(estimate.lines()),
            Err(refused) => {
                block.extend(refused.lines());
                first.get_or_insert(refused.refusal);
            }
        }
        emit(&mut stdout, index, &block)?;
    }
    first.map_or(Ok(()), Err)
}

fn emit(stdout: &mut impl std::io::Write, index: usize, block: &[String]) -> Result<()> {
    if index > 0 {
        writeln!(stdout).refuse_at("main::emit")?;
    }
    for line in block {
        writeln!(stdout, "{line}").refuse_at("main::emit")?;
    }
    stdout.flush().refuse_at("main::emit")?;
    Ok(())
}

fn repair_index_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 4 && args.len() != 7 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(2)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    if args.len() == 7 {
        // #95: record the repair in an apply state directory, bound to its
        // plan item through CORPUS's capture record.
        bulkload_agent::estate::repair_missing_index(
            path(0)?,
            path(1)?,
            source,
            path(3)?,
            &bulkload_agent::estate::RepairLedger {
                plan: path(4)?,
                corpus: path(5)?,
                state: path(6)?,
            },
        )?;
        println!(
            "missing index repaired; outcome index-repaired recorded; payload parity not asserted"
        );
    } else {
        bulkload_agent::git_carry::repair_missing_index(path(0)?, path(1)?, source, path(3)?)?;
        println!("missing index repaired; payload parity not asserted");
    }
    Ok(())
}

fn registered_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 6 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(4)
        .and_then(|arg| arg.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    bulkload_agent::git_carry::registered::restore(
        path(0)?,
        path(1)?,
        path(2)?,
        path(3)?,
        source,
        path(5)?,
    )?;
    println!("registered payload restored; original administration and staging preserved");
    Ok(())
}
fn attach_standalone_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 6 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(2)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    bulkload_agent::git_carry::attach_standalone_payload(
        path(0)?,
        path(1)?,
        source,
        path(3)?,
        path(4)?,
        path(5)?,
    )?;
    println!("standalone payload attached; config retained, selected origin mapping activated");
    Ok(())
}

fn attach_payload_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 5 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(3)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    bulkload_agent::git_carry::attach_matching_payload(
        path(0)?,
        path(1)?,
        path(2)?,
        source,
        path(4)?,
    )?;
    println!("matching payload attached; existing common Git administration preserved");
    Ok(())
}

// Escaped paths are intentional: receipts must not permit embedded newlines.
#[allow(clippy::unnecessary_debug_formatting, clippy::too_many_lines)]
fn estate_command(command: &str, args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::estate;
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let jobs = || {
        args.last()
            .and_then(|arg| arg.to_str())
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse::<usize>()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    };
    let receipt = print_receipt;
    match command {
        "estate-retarget" => {
            let mut positional = Vec::new();
            let mut maps = Vec::new();
            let mut rest = args.iter();
            while let Some(arg) = rest.next() {
                if arg.to_str() == Some("--map") {
                    let value = rest
                        .next()
                        .and_then(|value| value.to_str())
                        .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
                    let (old, new) = value
                        .split_once('=')
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?;
                    maps.push((PathBuf::from(old), PathBuf::from(new)));
                } else {
                    positional.push(Path::new(arg));
                }
            }
            let [plan, corpus] = positional[..] else {
                return Err(BulkloadRefusal::RequiredFieldMissing);
            };
            if maps.is_empty() {
                return Err(BulkloadRefusal::RequiredFieldMissing);
            }
            let mut output = std::io::stdout().lock();
            for row in estate::retarget(plan, corpus, &maps)? {
                writeln!(
                    output,
                    "item={} was={} source={:?} repository={:?} workspace={:?} capture={}",
                    row.now_id,
                    row.was_id,
                    row.now.source,
                    row.now.repository,
                    row.now.workspace,
                    row.capture
                )
                .refuse_at("main::estate_command")?;
            }
            output.flush().refuse_at("main::estate_command")
        }
        "estate-show" if args.len() == 1 => {
            let mut output = std::io::stdout().lock();
            for item in estate::inspect(path(0)?)? {
                writeln!(output, "{item:?}").refuse_at("main::estate_command")?;
            }
            output.flush().refuse_at("main::estate_command")?;
            Ok(())
        }
        "estate-add" if (3..=4).contains(&args.len()) => {
            estate::add(path(0)?, path(1)?, path(2)?, args.get(3).map(Path::new))
        }
        "estate-add-batch" if args.len() >= 4 && (args.len() - 1).is_multiple_of(3) => {
            let mut items = Vec::new();
            for group in args
                .get(1..)
                .ok_or(BulkloadRefusal::RequiredFieldMissing)?
                .chunks_exact(3)
            {
                let source = group
                    .first()
                    .map(PathBuf::from)
                    .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
                let repository = group
                    .get(1)
                    .map(PathBuf::from)
                    .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
                let workspace = group.get(2).filter(|p| *p != "-").map(PathBuf::from);
                items.push(estate::Item {
                    source,
                    repository,
                    workspace,
                });
            }
            estate::add_batch(path(0)?, &items)
        }
        "estate-capture" if (4..=5).contains(&args.len()) => {
            let policy = capture_policy(args.get(4))?;
            let jobs = args
                .get(3)
                .and_then(|arg| arg.to_str())
                .ok_or(BulkloadRefusal::FieldDomainViolation)?
                .parse::<usize>()
                .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
            estate::capture_with_policy(path(0)?, path(1)?, path(2)?, jobs, policy, &receipt)
        }
        "estate-apply" => {
            let (options, rest) = estate_options(args)?;
            if rest.len() != 5 || options.since.is_some() || !options.scan.is_empty() {
                return Err(BulkloadRefusal::RequiredFieldMissing);
            }
            // `--refs-in` names its refs under the `--beside` namespace;
            // `--advance-ancestor` falls back to the beside landing.
            if (options.refs_in || options.advance_ancestor)
                && (options.beside.is_none() || !options.adopt)
            {
                return Err(BulkloadRefusal::RequiredFieldMissing);
            }
            let path = |i: usize| -> Result<&Path> {
                rest.get(i)
                    .map(Path::new)
                    .ok_or(BulkloadRefusal::RequiredFieldMissing)
            };
            let source = rest
                .get(3)
                .and_then(|arg| arg.to_str())
                .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            estate::apply_reporting(
                path(0)?,
                path(1)?,
                path(2)?,
                source,
                (
                    jobs()?,
                    &estate::ApplyOptions {
                        adopt: options.adopt,
                        activate_origin: options.activate_origin,
                        side_roots: options.side_roots,
                        exclude_union: options.exclude_union,
                        beside: options.beside,
                        refs_in: options.refs_in,
                        advance_ancestor: options.advance_ancestor,
                    },
                ),
                &receipt,
                // TIN-6569: the H step's line, after every item's receipt.
                &mut |line| {
                    let mut output = std::io::stdout().lock();
                    writeln!(output, "{line}").refuse_at("main::estate_command")?;
                    output.flush().refuse_at("main::estate_command")
                },
            )
        }
        "estate-diff" => {
            let (options, rest) = estate_options(args)?;
            if rest.len() < 4
                || options.adopt
                || options.exclude_union
                || options.refs_in
                || options.advance_ancestor
            {
                return Err(BulkloadRefusal::RequiredFieldMissing);
            }
            let source = rest
                .get(2)
                .and_then(|arg| arg.to_str())
                .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            let states: Vec<PathBuf> = rest.iter().skip(3).map(PathBuf::from).collect();
            let mut output = std::io::stdout().lock();
            estate::diff(
                Path::new(rest.first().ok_or(BulkloadRefusal::RequiredFieldMissing)?),
                Path::new(rest.get(1).ok_or(BulkloadRefusal::RequiredFieldMissing)?),
                source,
                &states,
                &estate::DiffOptions {
                    since: options.since,
                    scan: options.scan,
                    activate_origin: options.activate_origin,
                    beside: options.beside,
                    side_roots: options.side_roots,
                },
                &mut |line| writeln!(output, "{line}").refuse_at("main::estate_command"),
            )
            .map(drop)?;
            output.flush().refuse_at("main::estate_command")
        }
        "estate-unseat" if !args.is_empty() => {
            let lives: Vec<PathBuf> = args.iter().skip(1).map(PathBuf::from).collect();
            let mut output = std::io::stdout().lock();
            estate::unseat(path(0)?, &lives, &mut |line| {
                writeln!(output, "{line}").refuse_at("main::estate_command")
            })?;
            output.flush().refuse_at("main::estate_command")
        }
        "estate-handoff" => {
            let (options, rest) = handoff_options(args)?;
            if rest.len() != 4 {
                return Err(BulkloadRefusal::RequiredFieldMissing);
            }
            let path = |i: usize| -> Result<&Path> {
                rest.get(i)
                    .map(Path::new)
                    .ok_or(BulkloadRefusal::RequiredFieldMissing)
            };
            let source = rest
                .get(3)
                .and_then(|arg| arg.to_str())
                .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            let mut output = std::io::stdout().lock();
            let outcome = estate::handoff(
                path(0)?,
                path(1)?,
                path(2)?,
                source,
                &options,
                &mut |line| writeln!(output, "{line}").refuse_at("main::estate_command"),
            );
            output.flush().refuse_at("main::estate_command")?;
            outcome.map(drop)
        }
        _ => Err(BulkloadRefusal::RequiredFieldMissing),
    }
}

// One estate receipt line (`estate-capture`, `estate-apply`, and the phases
// of `estate-move-source` and `estate-move` that run them).
#[allow(clippy::unnecessary_debug_formatting)] // Paths print escaped and quoted.
fn print_receipt(row: &bulkload_agent::estate::Receipt) -> Result<()> {
    let mut output = std::io::stdout().lock();
    write!(
        output,
        "item={} source={:?} outcome={} reason={:?} source_bytes_read={}",
        row.item, row.source, row.outcome, row.reason, row.bytes_read
    )
    .refuse_at("main::estate_command")?;
    if let Some(why) = row.reuse_unavailable {
        write!(output, " reuse_unavailable={why}").refuse_at("main::estate_command")?;
    }
    writeln!(output).refuse_at("main::estate_command")?;
    // One line per drifted ref or seat, after the item line, so a clean
    // item stays one line.
    for line in &row.drift {
        writeln!(output, "item={} drift={line}", row.item).refuse_at("main::estate_command")?;
    }
    // One line per nest after the item line, so a nest-free item stays
    // one line (R-N73). Paths inside are byte-escaped.
    for line in &row.nested {
        writeln!(output, "item={} {line}", row.item).refuse_at("main::estate_command")?;
    }
    output.flush().refuse_at("main::estate_command")?;
    Ok(())
}

// R0 (TIN-6587): re-seat landed agent sessions, or check that they hold.
#[allow(clippy::too_many_lines)]
fn reseat_command(args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::reseat::{self, GitCheck, Unmapped};
    let mut check = false;
    let mut maps = Vec::new();
    let mut from_plan = false;
    let (mut plan, mut corpus, mut source) = (None, None, None);
    let mut states = Vec::new();
    let mut roots = Vec::new();
    let (mut landing_claude, mut landing_codex) = (None, None);
    let mut unmapped = Unmapped::Report;
    let mut marker = None;
    let mut sessions_only = false;
    let mut waived = std::collections::BTreeSet::new();
    let mut margin = None;
    let mut managed = Vec::new();
    let (mut claude, mut codex) = (true, true);
    let (mut undo, mut respell, mut regularize, mut default_by_id) = (false, true, true, true);
    let mut by_id = Vec::new();
    let mut rebind = Vec::new();
    let mut archived = Vec::new();
    let mut at = 0;
    let value = |at: usize| args.get(at).ok_or(BulkloadRefusal::RequiredFieldMissing);
    while let Some(arg) = args.get(at).and_then(|arg| arg.to_str()) {
        let mut step = 1;
        match arg {
            "--check" => check = true,
            "--lanes-from-plan" => from_plan = true,
            "--sessions-only" => sessions_only = true,
            "--no-claude" => claude = false,
            "--no-codex" => codex = false,
            "--undo" => undo = true,
            "--no-respell" => respell = false,
            "--no-regularize" => regularize = false,
            "--no-default-by-id" => default_by_id = false,
            "--unmapped=report" => unmapped = Unmapped::Report,
            "--unmapped=by-id" => unmapped = Unmapped::ById,
            "--rebind-landing" => {
                let (from, to) = value(at + 1)?
                    .to_str()
                    .ok_or(BulkloadRefusal::PathNotPortable)?
                    .split_once('=')
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
                let (from, to) = (Path::new(from), Path::new(to));
                if !from.is_absolute() || !to.is_absolute() {
                    return Err(BulkloadRefusal::PathNotAbsolute);
                }
                // The new root must exist (judged by its real path); the
                // old one may be gone and is taken as the rows recorded it.
                let to =
                    std::fs::canonicalize(to).refuse_at("main::reseat_command::rebind_landing")?;
                rebind.push((from.to_path_buf(), to));
                step = 2;
            }
            "--map" => {
                let (from, to) = value(at + 1)?
                    .to_str()
                    .ok_or(BulkloadRefusal::PathNotPortable)?
                    .split_once('=')
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
                if !Path::new(from).is_absolute() || !Path::new(to).is_absolute() {
                    return Err(BulkloadRefusal::PathNotAbsolute);
                }
                maps.push((PathBuf::from(from), PathBuf::from(to)));
                step = 2;
            }
            "--plan"
            | "--corpus"
            | "--private-state"
            | "--codex-source-root"
            | "--landing-claude"
            | "--landing-codex"
            | "--managed-settings"
            | "--by-id"
            | "--replace-archived-dest"
            | "--waive-from" => {
                let path = PathBuf::from(value(at + 1)?);
                match arg {
                    "--plan" => plan = Some(path),
                    "--corpus" => corpus = Some(path),
                    "--private-state" => states.push(path),
                    "--codex-source-root" => roots.push(path),
                    "--landing-claude" => landing_claude = Some(path),
                    "--managed-settings" => managed.push(path),
                    "--by-id" => {
                        if !path.is_absolute() {
                            return Err(BulkloadRefusal::PathNotAbsolute);
                        }
                        by_id.push(path);
                    }
                    "--replace-archived-dest" => {
                        if !path.is_absolute() {
                            return Err(BulkloadRefusal::PathNotAbsolute);
                        }
                        // Judged by its real path: a root that does not
                        // exist names nothing to replace and is refused.
                        archived.push(
                            std::fs::canonicalize(&path)
                                .refuse_at("main::reseat_command::replace_archived_dest")?,
                        );
                    }
                    "--waive-from" => {
                        let text = std::fs::read_to_string(&path)
                            .refuse_at("main::reseat_command::waive_from")?;
                        for line in text.lines() {
                            let id = line.split('#').next().unwrap_or_default().trim();
                            if !id.is_empty() {
                                waived.insert(id.to_owned());
                            }
                        }
                    }
                    _ => landing_codex = Some(path),
                }
                step = 2;
            }
            "--waive" => {
                waived.insert(
                    value(at + 1)?
                        .to_str()
                        .filter(|id| !id.is_empty())
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?
                        .to_owned(),
                );
                step = 2;
            }
            "--retention-margin-days" => {
                margin = Some(
                    value(at + 1)?
                        .to_str()
                        .and_then(|days| days.parse::<i64>().ok())
                        .filter(|days| *days >= 0)
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?,
                );
                step = 2;
            }
            "--source" => {
                source = Some(
                    value(at + 1)?
                        .to_str()
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?
                        .to_owned(),
                );
                step = 2;
            }
            "--note-marker" => {
                marker = Some(
                    value(at + 1)?
                        .to_str()
                        .filter(|marker| !marker.is_empty())
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?
                        .to_owned(),
                );
                step = 2;
            }
            other if other.starts_with("--") => return Err(BulkloadRefusal::FieldDomainViolation),
            _ => break,
        }
        at += step;
    }
    let rest = args.get(at..).unwrap_or_default();
    let [source_home, landing_home, dest_home, state] = rest else {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    };
    let mut options = reseat::Options::new(
        Path::new(source_home),
        Path::new(landing_home),
        Path::new(dest_home),
        Path::new(state),
    );
    if !options.source_home.is_absolute() || !options.dest_home.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    if let Some(path) = landing_claude {
        options.landing_claude = path;
    }
    if let Some(path) = landing_codex {
        options.landing_codex = path;
    }
    if !roots.is_empty() {
        options.codex_source_roots = roots;
    }
    options.replace_archived = archived;
    if from_plan {
        let plan = plan.as_ref().ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        for item in bulkload_agent::estate::inspect(plan)? {
            if let Some(workspace) = item.workspace {
                maps.push((item.source, workspace));
            }
        }
    }
    options.maps = maps;
    options.rebind = rebind;
    options.unmapped = unmapped;
    options.claude = claude;
    options.codex = codex;
    options.handoff_marker = marker;
    options.sessions_only = sessions_only;
    options.waived = waived;
    if let Some(margin) = margin {
        options.retention_margin_days = margin;
    }
    if !managed.is_empty() {
        options.managed_settings = managed;
    }
    if !default_by_id {
        options.by_id_exact.clear();
        options.by_id_prefixes.clear();
    }
    options.by_id_prefixes.extend(by_id);
    options.respell_cwd = respell;
    options.regularize = regularize;
    options.git = match (plan, corpus, source) {
        (Some(plan), Some(corpus), Some(source)) => Some(GitCheck {
            plan,
            corpus,
            source,
            states,
        }),
        (_, None, None) if states.is_empty() => None,
        _ => return Err(BulkloadRefusal::RequiredFieldMissing),
    };
    let mut output = std::io::stdout().lock();
    let mut emit = |line: &str| writeln!(output, "{line}").refuse_at("main::reseat_command");
    if check && undo {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    if undo {
        reseat::undo(&options, &mut emit).map(drop)?;
    } else if check {
        reseat::check(&options, &mut emit)?.verdict()?;
    } else {
        reseat::reseat(&options, &mut emit).map(drop)?;
    }
    output.flush().refuse_at("main::reseat_command")
}

/// `estate-handoff`'s options, before its positional arguments
/// (TIN-6569, OI-1003-Q183).
fn handoff_options(
    args: &[std::ffi::OsString],
) -> Result<(
    bulkload_agent::estate::HandoffOptions,
    &[std::ffi::OsString],
)> {
    let mut options = bulkload_agent::estate::HandoffOptions::default();
    let mut at = 0;
    let value = |at: usize| args.get(at).ok_or(BulkloadRefusal::RequiredFieldMissing);
    while let Some(arg) = args.get(at).and_then(|arg| arg.to_str()) {
        let flag = |set: &mut bool| {
            if *set {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
            *set = true;
            Ok(())
        };
        let mut mode = |to: bulkload_agent::estate::HandoffMode| {
            if options.mode != bulkload_agent::estate::HandoffMode::Render {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
            options.mode = to;
            Ok(())
        };
        match arg {
            "--check" => mode(bulkload_agent::estate::HandoffMode::Check)?,
            "--clear" => mode(bulkload_agent::estate::HandoffMode::Clear)?,
            "--dry-run" => mode(bulkload_agent::estate::HandoffMode::DryRun)?,
            "--renew" => flag(&mut options.renew)?,
            "--readme" => flag(&mut options.readme)?,
            "--clear-carried" => flag(&mut options.clear_carried)?,
            "--item" => {
                options.items.push(
                    value(at + 1)?
                        .to_str()
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?
                        .to_owned(),
                );
                at += 1;
            }
            "--workspace" => {
                options.workspaces.push(PathBuf::from(value(at + 1)?));
                at += 1;
            }
            "--sibling" => {
                let pair = value(at + 1)?;
                let (landed, live) = pair
                    .as_encoded_bytes()
                    .iter()
                    .position(|byte| *byte == b'=')
                    .map(|split| {
                        let bytes = pair.as_encoded_bytes();
                        (
                            bytes.get(..split).unwrap_or_default(),
                            bytes.get(split + 1..).unwrap_or_default(),
                        )
                    })
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
                let path = |bytes: &[u8]| {
                    PathBuf::from(
                        <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(bytes),
                    )
                };
                options.siblings.push((path(landed), path(live)));
                at += 1;
            }
            "--as-of" if options.as_of.is_none() => {
                options.as_of = Some(
                    value(at + 1)?
                        .to_str()
                        .and_then(|ns| ns.parse::<i128>().ok())
                        .filter(|ns| *ns >= 0)
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?,
                );
                at += 1;
            }
            other if other.starts_with("--") => return Err(BulkloadRefusal::FieldDomainViolation),
            _ => break,
        }
        at += 1;
    }
    Ok((options, args.get(at..).unwrap_or_default()))
}

/// The options `estate-apply` and `estate-diff` take before their
/// positional arguments (#226).
// Each bool is one independent command-line flag.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
struct EstateOptions {
    adopt: bool,
    exclude_union: bool,
    beside: Option<String>,
    refs_in: bool,
    advance_ancestor: bool,
    activate_origin: Vec<String>,
    since: Option<i128>,
    scan: Vec<PathBuf>,
    side_roots: Vec<PathBuf>,
}

fn estate_options(args: &[std::ffi::OsString]) -> Result<(EstateOptions, &[std::ffi::OsString])> {
    let mut options = EstateOptions::default();
    let mut at = 0;
    let value = |at: usize| args.get(at).ok_or(BulkloadRefusal::RequiredFieldMissing);
    while let Some(arg) = args.get(at).and_then(|arg| arg.to_str()) {
        match arg {
            "--adopt" if !options.adopt => {
                options.adopt = true;
                at += 1;
            }
            "--exclude-union" if !options.exclude_union => {
                options.exclude_union = true;
                at += 1;
            }
            "--refs-in" if !options.refs_in => {
                options.refs_in = true;
                at += 1;
            }
            "--advance-ancestor" if !options.advance_ancestor => {
                options.advance_ancestor = true;
                at += 1;
            }
            "--beside" if options.beside.is_none() => {
                options.beside = Some(
                    value(at + 1)?
                        .to_str()
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?
                        .to_owned(),
                );
                at += 2;
            }
            "--activate-origin" => {
                let prefix = value(at + 1)?
                    .to_str()
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
                if !bulkload_agent::git_carry::origin_prefix_allowed(prefix) {
                    return Err(BulkloadRefusal::FieldDomainViolation);
                }
                options.activate_origin.push(prefix.to_owned());
                at += 2;
            }
            "--since" if options.since.is_none() => {
                options.since = Some(
                    value(at + 1)?
                        .to_str()
                        .and_then(|since| since.parse::<i128>().ok())
                        .filter(|since| *since >= 0)
                        .ok_or(BulkloadRefusal::FieldDomainViolation)?,
                );
                at += 2;
            }
            "--scan" => {
                options.scan.push(PathBuf::from(value(at + 1)?));
                at += 2;
            }
            "--side-root" => {
                options.side_roots.push(PathBuf::from(value(at + 1)?));
                at += 2;
            }
            other if other.starts_with("--") => return Err(BulkloadRefusal::FieldDomainViolation),
            _ => break,
        }
    }
    Ok((options, args.get(at..).unwrap_or_default()))
}

fn apply_state_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 4 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let max_rows = args
        .get(3)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::FieldDomainViolation)?
        .parse()
        .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
    bulkload_agent::provider_sqlite::online::apply_state_candidate(
        path(0)?,
        path(1)?,
        path(2)?,
        max_rows,
        &|receipt| {
            let mut output = std::io::stdout().lock();
            writeln!(
                output,
                "table={} inserted={} corrected={} conflicts={}",
                receipt.table, receipt.inserted, receipt.corrected, receipt.conflicts
            )
            .refuse_at("main::apply_state_command")?;
            output.flush().refuse_at("main::apply_state_command")?;
            Ok(())
        },
    )
}

fn hydrate_command(args: &[std::ffi::OsString]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    if args.len() != 5 && args.len() != 7 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let number = |index: usize| -> Result<u64> {
        args.get(index)
            .and_then(|value| value.to_str())
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    };
    let mapping = bulkload_agent::provider_sqlite::PathMapping {
        source_home: path(1)?,
        destination_home: path(2)?,
    };
    let gzip = args
        .get(5)
        .map_or_else(|| Path::new("/usr/bin/gzip"), Path::new);
    let zstd = args
        .get(6)
        .map_or_else(|| Path::new("/usr/bin/zstd"), Path::new);
    let receipt = |report: &bulkload_agent::provider_sqlite::hydrate::Hydrated| -> Result<()> {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "published={} bytes={} blake3={} source_identity={:?} path={} source={}",
            report.published,
            report.bytes,
            report.blake3,
            report.source_identity,
            report.path.as_os_str().as_bytes().escape_ascii(),
            report.source.as_os_str().as_bytes().escape_ascii()
        )
        .refuse_at("main::hydrate_command")?;
        output.flush().refuse_at("main::hydrate_command")?;
        Ok(())
    };
    let reports = bulkload_agent::provider_sqlite::hydrate::hydrate_state(
        path(0)?,
        &mapping,
        number(3)?,
        usize::try_from(number(4)?).map_err(|_| BulkloadRefusal::BudgetExceeded)?,
        gzip,
        zstd,
        &receipt,
    )?;
    println!("hydrated_files={}", reports.len());
    Ok(())
}

// WP3 PR 4: a copy session's outcomes are closure rows, keyed on the
// canonical SOURCE and DEST so a rerun spelled another way names the same
// rows. S2 (review finding 1): the overlap decision is made before the
// record's first write, so a refused overlap writes nothing anywhere.
fn copy_recorded(source: &Path, dest: &Path, source_state: &Path, dest_state: &Path) -> Result<()> {
    let stats = bulkload_agent::transfer_outcome::record_session(
        dest_state,
        "copy",
        &bulkload_agent::transfer_outcome::copy_source(source),
        &bulkload_agent::transfer_outcome::stable(dest),
        "transfer::copy",
        || bulkload_agent::transfer::copy_roots_apart(source, dest, source_state, dest_state),
        || bulkload_agent::transfer::copy(source, dest, source_state, dest_state),
    )?;
    report_transfer(&stats)
}

// WP3 PR 4: a pull session's outcomes are closure rows, keyed on
// HOST:SOURCE and the canonical DEST, recorded in DEST_STATE around the
// session (`pull_command`, whose ssh child the S2 registry pins).
fn pull_recorded(args: &[std::ffi::OsString]) -> Result<()> {
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let host = args.first().ok_or(BulkloadRefusal::RequiredFieldMissing)?;
    bulkload_agent::transfer_outcome::record_session(
        path(4)?,
        "pull",
        &bulkload_agent::transfer_outcome::pull_source(host, path(1)?),
        &bulkload_agent::transfer_outcome::stable(path(2)?),
        "transfer::pull",
        || bulkload_agent::transfer::receive_roots_apart(path(2)?, path(4)?),
        || pull_command(args),
    )
    .and_then(|stats| report_transfer(&stats))
}

// M2r2 b: `estate-carry-files`. A `pull` carries through `pull_command`,
// the remote source state per item under --source-state-root.
// `estate-beside-retire [--delete] [--scan ROOT]... [--depth N] SUFFIX
// SOURCE PRIVATE_STATE [BESIDE ...]` (operator ruling 2026-10-16 21:24Z).
fn beside_retire_command(args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::estate::beside_retire::{self, RetireOptions};
    let mut options = RetireOptions {
        depth: beside_retire::SCAN_DEPTH,
        ..RetireOptions::default()
    };
    let mut at = 0;
    let value = |at: usize| args.get(at).ok_or(BulkloadRefusal::RequiredFieldMissing);
    while let Some(arg) = args.get(at).and_then(|arg| arg.to_str()) {
        match arg {
            "--delete" if !options.delete => {
                options.delete = true;
                at += 1;
            }
            "--scan" => {
                options.scan.push(PathBuf::from(value(at + 1)?));
                at += 2;
            }
            "--depth" => {
                options.depth = value(at + 1)?
                    .to_str()
                    .and_then(|depth| depth.parse::<usize>().ok())
                    .filter(|depth| *depth > 0)
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
                at += 2;
            }
            other if other.starts_with("--") => return Err(BulkloadRefusal::FieldDomainViolation),
            _ => break,
        }
    }
    let rest = args.get(at..).unwrap_or_default();
    let text = |i: usize| -> Result<String> {
        rest.get(i)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?
            .to_str()
            .map(str::to_owned)
            .ok_or(BulkloadRefusal::FieldDomainViolation)
    };
    options.suffix = text(0)?;
    options.source = text(1)?;
    options.state = PathBuf::from(rest.get(2).ok_or(BulkloadRefusal::RequiredFieldMissing)?);
    if !options.state.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    options.paths = rest
        .get(3..)
        .unwrap_or_default()
        .iter()
        .map(PathBuf::from)
        .collect();
    if options.paths.is_empty() && options.scan.is_empty() {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    beside_retire::run(&options, &mut |line| {
        let mut output = std::io::stdout().lock();
        writeln!(output, "{line}").refuse_at("main::beside_retire")?;
        output.flush().refuse_at("main::beside_retire")
    })
    .map(drop)
}

#[allow(clippy::too_many_lines)] // Argument parsing, then one call.
fn carry_files_command(args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::estate::carried::{self, CarryJob, CarryOptions, Transport};
    struct Pull {
        host: std::ffi::OsString,
        source_state_root: PathBuf,
        remote: Option<std::ffi::OsString>,
        config: Option<std::ffi::OsString>,
    }
    impl Transport for Pull {
        fn verb(&self) -> &'static str {
            "pull"
        }
        fn source_key(&self, source: &Path) -> String {
            bulkload_agent::transfer_outcome::pull_source(&self.host, source)
        }
        fn roots_apart(&self, job: &CarryJob) -> Result<()> {
            bulkload_agent::transfer::receive_roots_apart(&job.destination, &job.destination_state)
        }
        fn run(&self, job: &CarryJob) -> Result<bulkload_agent::transfer::TransferStats> {
            let mut args: Vec<std::ffi::OsString> = vec![
                self.host.clone(),
                job.source.clone().into_os_string(),
                job.destination.clone().into_os_string(),
                self.source_state_root.join(&job.item).into_os_string(),
                job.destination_state.clone().into_os_string(),
            ];
            // `pull` takes SSH_CONFIG only after REMOTE_EXECUTABLE; the
            // parser requires the one with the other.
            args.extend(self.remote.iter().cloned());
            args.extend(self.config.iter().cloned());
            pull_command(&args)
        }
    }
    let mut options = CarryOptions::default();
    let (mut host, mut root, mut remote, mut config) = (None, None, None, None);
    let mut at = 0;
    let value = |at: usize| {
        args.get(at)
            .cloned()
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    while let Some(arg) = args.get(at).and_then(|arg| arg.to_str()) {
        match arg {
            "--dry-run" if !options.dry_run => {
                options.dry_run = true;
                at += 1;
                continue;
            }
            "--bind-only" if !options.bind_only => {
                options.bind_only = true;
                at += 1;
                continue;
            }
            "--recarry" if !options.recarry => {
                options.recarry = true;
                at += 1;
                continue;
            }
            "--beside" if options.beside.is_none() => options.beside = Some(value(at + 1)?),
            "--item" => options.items.push(
                value(at + 1)?
                    .into_string()
                    .map_err(|_| BulkloadRefusal::FieldDomainViolation)?,
            ),
            "--bind" => {
                let pair = value(at + 1)?
                    .into_string()
                    .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
                let (item, state) = pair
                    .split_once('=')
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
                if !Path::new(state).is_absolute()
                    || options
                        .bind
                        .insert(item.to_owned(), PathBuf::from(state))
                        .is_some()
                {
                    return Err(BulkloadRefusal::FieldDomainViolation);
                }
            }
            "--host" if host.is_none() => host = Some(value(at + 1)?),
            "--source-state-root" if root.is_none() => root = Some(PathBuf::from(value(at + 1)?)),
            "--remote-executable" if remote.is_none() => remote = Some(value(at + 1)?),
            "--ssh-config" if config.is_none() => config = Some(value(at + 1)?),
            other if other.starts_with("--") => return Err(BulkloadRefusal::FieldDomainViolation),
            _ => break,
        }
        at += 2;
    }
    let rest = args.get(at..).unwrap_or_default();
    let [plan, corpus, state, source, carry_state] = rest else {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    };
    let source = source
        .to_str()
        .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    if config.is_some() && remote.is_none() {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let local = carried::LocalCopy;
    let pull;
    let transport: &dyn Transport = match (host, root) {
        (Some(host), Some(source_state_root)) => {
            pull = Pull {
                host,
                source_state_root,
                remote,
                config,
            };
            &pull
        }
        (None, None) if remote.is_none() && config.is_none() => &local,
        _ => return Err(BulkloadRefusal::RequiredFieldMissing),
    };
    let mut output = std::io::stdout().lock();
    carried::carry_files(
        Path::new(plan),
        Path::new(corpus),
        Path::new(state),
        source,
        Path::new(carry_state),
        &options,
        transport,
        &mut |line| {
            writeln!(output, "{line}").refuse_at("main::carry_files_command")?;
            output.flush().refuse_at("main::carry_files_command")
        },
    )
}

// Review finding 3 (S4): list one transfer a run expects in its manifest,
// bound to PLAN; `closure-report --transfer-manifest` joins exactly these.
fn expect_command(args: &[std::ffi::OsString]) -> Result<()> {
    use bulkload_agent::transfer_outcome::{self as outcome, Expected};
    let [manifest, plan, verb, rest @ ..] = args else {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    };
    let expected = match (verb.to_str(), rest) {
        (Some("copy"), [source, destination, state]) => Expected {
            verb: "copy".to_owned(),
            source: outcome::copy_source(Path::new(source)),
            destination: outcome::stable(Path::new(destination)),
            state: outcome::stable(Path::new(state)),
        },
        (Some("pull"), [host, source, destination, state]) => Expected {
            verb: "pull".to_owned(),
            source: outcome::pull_source(host, Path::new(source)),
            destination: outcome::stable(Path::new(destination)),
            state: outcome::stable(Path::new(state)),
        },
        (Some("copy" | "pull"), _) => return Err(BulkloadRefusal::RequiredFieldMissing),
        _ => return Err(BulkloadRefusal::FieldDomainViolation),
    };
    let item = expected.item();
    let written = outcome::expect(Path::new(manifest), Path::new(plan), expected)?;
    println!(
        "transfer-expect item={item} transfers={} since={}",
        written.transfers.len(),
        written.since
    );
    Ok(())
}

fn pull_command(args: &[std::ffi::OsString]) -> Result<bulkload_agent::transfer::TransferStats> {
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let host = args.first().ok_or(BulkloadRefusal::RequiredFieldMissing)?;
    let remote = match args.get(5) {
        None => "bulkload-agent",
        Some(value) => {
            let value = value.to_str().ok_or(BulkloadRefusal::PathNotPortable)?;
            if !Path::new(value).is_absolute()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
            {
                return Err(BulkloadRefusal::PathNotPortable);
            }
            value
        }
    };
    let mut ssh = Command::new("ssh");
    ssh.args(["-T", "-oBatchMode=yes", "-oConnectTimeout=15"]);
    if let Some(config) = args.get(6) {
        if !Path::new(config).is_absolute() {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        ssh.arg("-F").arg(config);
    }
    ssh.arg("--").arg(host).arg(remote);
    if bulkload_agent::durable::durability() == bulkload_agent::durable::Durability::Strict {
        ssh.arg("--durability=strict");
    }
    // The remote `serve` is the source half: its ledger takes this mode.
    if bulkload_agent::durable::ledger_sync() == bulkload_agent::durable::LedgerSync::Full {
        ssh.arg("--source-ledger-sync=full");
    }
    let mut child = ssh
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .refuse_at("main::pull_command")?;
    let result = {
        let mut output = child.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
        let mut input = child.stdout.take().ok_or(BulkloadRefusal::Io(None))?;
        bulkload_agent::transfer::tune_stream(&output);
        bulkload_agent::transfer::tune_stream(&input);
        bulkload_agent::transfer::receive(
            &mut input,
            &mut output,
            path(1)?,
            path(3)?,
            path(2)?,
            path(4)?,
        )
    };
    let exit_status = child.wait().refuse_at("main::pull_command")?;
    let stats = result?;
    // S4 (TIN-6592): the session ended but its transport did not end clean.
    if !exit_status.success() {
        return Err(BulkloadRefusal::TransportLost);
    }
    Ok(stats)
}

fn steps(value: Option<&std::ffi::OsString>) -> Result<u32> {
    value.map_or(Ok(1_000_000), |value| {
        value
            .to_str()
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    })
}

/// Print the verb's process-scope counters (M2 W2, TIN-4541).
///
/// copy and pull print on stdout beside their transfer line; serve's stdout is
/// the wire, so it and every other verb print on stderr.
fn report_counters(verb: &str, started: std::time::Instant, priority: Priority) {
    use bulkload_agent::counters::{elapsed_ns, Counters};
    use bulkload_agent::transfer::TransferTiming;
    use bulkload_agent::transfer_store::ChunkTiming;
    let side = match verb {
        "copy" => "both",
        "pull" => "destination",
        "serve" => "source",
        _ => "local",
    };
    let prefix = format!(
        "verb={verb} side={side} scope=process {}",
        priority.render()
    );
    let mut lines = Vec::new();
    if matches!(verb, "copy" | "pull" | "serve") {
        lines.push(format!(
            "transfer_timing {prefix} {}",
            TransferTiming::snapshot().render()
        ));
        lines.push(format!(
            "chunk_timing {prefix} {}",
            ChunkTiming::snapshot().render()
        ));
    }
    lines.push(format!(
        "counters {prefix} elapsed_ns={} {}",
        elapsed_ns(started),
        Counters::snapshot().render()
    ));
    // The last refused free-space check: which limit bound (the filesystem
    // or a project quota) and both limits' numbers (M2 round 2, item 1).
    if let Some(space) = bulkload_agent::space::last_refusal() {
        lines.push(format!("space_refusal {prefix} {}", space.fields()));
    }
    if matches!(verb, "copy" | "pull") {
        let mut output = std::io::stdout().lock();
        for line in &lines {
            let _ = writeln!(output, "{line}");
        }
        let _ = output.flush();
    } else {
        let mut output = std::io::stderr().lock();
        for line in &lines {
            let _ = writeln!(output, "{line}");
        }
    }
}

fn report_transfer(stats: &bulkload_agent::transfer::TransferStats) -> Result<()> {
    println!(
        "completed={} reused={} bytes_received={} source_bytes_read={} refusals={} \
         source_engine_temporaries={} capped_subtrees={} sqlite_mode={} sqlite_snapshots={} \
         sqlite_sidecars_covered={}",
        stats.completed,
        stats.reused,
        stats.bytes_received,
        stats.source_bytes_read,
        stats.refusals.len(),
        stats.source_engine_temporaries.len(),
        stats.capped_subtrees(),
        bulkload_agent::transfer::sqlite_mode_label(stats.sqlite_mode),
        stats.sqlite_snapshots.len(),
        stats.sqlite_sidecars_covered.len()
    );
    for (path, code) in &stats.refusals {
        // S4 (TIN-6592): an IO raised here prints its errno, as its record
        // keeps it.
        match stats.refusal_errnos.get(path) {
            Some(errno) => eprintln!("refused {}: {code} (errno {errno})", path.escape_ascii()),
            None => eprintln!("refused {}: {code}", path.escape_ascii()),
        }
        if let Some(disposition) = stats.default_disposition(path, code) {
            eprintln!(
                "default-disposition {}: {disposition} ({code}, OI-1003-Q148)",
                path.escape_ascii()
            );
        }
    }
    if stats.temporaries_removed > 0 {
        eprintln!("temporaries-removed {}", stats.temporaries_removed);
    }
    for path in &stats.temporaries_left {
        eprintln!("temporary-left {}", path.escape_ascii());
    }
    for path in &stats.source_engine_temporaries {
        eprintln!("source-engine-temporary {}", path.escape_ascii());
    }
    for path in &stats.directories_fallback {
        eprintln!(
            "directory-created-by-mkdir-fallback {}",
            path.escape_ascii()
        );
    }
    for (path, prior, prefix, size) in &stats.prefix_extended {
        eprintln!(
            "prefix-extended {} from={prior} to={size} prefix_blake3={}",
            path.escape_ascii(),
            blake3::Hash::from_bytes(*prefix).to_hex()
        );
    }
    for (path, prior) in &stats.prefix_interrupted {
        eprintln!(
            "prefix-extension-interrupted {} prior={prior} (a crashed extension proved the first {prior} bytes; what follows was appended since)",
            path.escape_ascii()
        );
    }
    for (path, found, source) in &stats.directories_mode_kept {
        eprintln!(
            "directory-mode-kept {} found={found:04o} source={source:04o}",
            path.escape_ascii()
        );
    }
    // S4 (#231): every path ran; the refused ones are in the record.
    if stats.refusals.is_empty() {
        Ok(())
    } else {
        Err(BulkloadRefusal::ItemsRefused)
    }
}

/// Exercise the pieces M1 actually ships: hash a real file off disk, put its
/// row in a frame, encode it with postcard, decode it back, and prove the
/// round trip is exact.
fn selftest() -> Result<()> {
    println!("bulkload-agent selftest");

    let path = scratch_path("selftest");
    let payload = b"tcfs bulkload M1 selftest payload";
    write_scratch(&path, payload)?;

    let digest = hash::hash_file(&path);
    let meta = std::fs::metadata(&path);
    let cleanup = std::fs::remove_file(&path);

    let digest = digest?;
    let meta = meta.refuse_at("main::selftest")?;
    cleanup.refuse_at("main::selftest")?;

    if digest != hash::hash_bytes(payload) {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    println!("  hashed        {} bytes", payload.len());
    println!("  blake3        {}", hex(&digest));
    println!("  crc32c        {:08x}", hash::checksum(payload));
    println!("  cdc chunks    {}", hash::chunk_boundaries(payload).len());

    let row = row_for(payload.len(), &meta, digest);
    let identity = StatIdentity::from_row(&row);
    let mut cache = MemoryCache::new();
    if cache.lookup(&identity)? != Freshness::Stale {
        return Err(BulkloadRefusal::ContractSelfInconsistent);
    }
    cache.record(&identity)?;
    if cache.lookup(&identity)? != Freshness::Fresh {
        return Err(BulkloadRefusal::ContractSelfInconsistent);
    }
    println!("  freshness     stale -> record -> fresh (ok)");

    let frame = Frame::Control(Control::Entry {
        entry: 0,
        row,
        wal: None,
    });
    let encoded = frame.encode()?;
    let (decoded, consumed) = Frame::decode(&encoded)?;
    if decoded != frame || consumed != encoded.len() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    println!("  frame bytes   {}", encoded.len());
    println!("  frame decoded {decoded:?}");
    println!("  round trip    exact (ok)");
    // M2 item 4: whether this binary survives a Nix garbage collection.
    let image = std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::read(exe).ok());
    println!("  interpreter   {}", interpreter_report(image.as_deref()));
    println!("  gc roots      {}", gc_roots_report(image.as_deref()));
    println!("selftest: ok");
    Ok(())
}

/// The selftest's interpreter line (M2 item 4, 2026-10-16: a GC deleted the
/// glibc an installed agent was linked against): this binary's ELF
/// interpreter (`PT_INTERP`), whether it exists, and whether it lives in
/// `/nix/store`, where a GC can delete it. A binary whose interpreter is
/// already gone cannot start, so this and [`gc_roots_report`] are the
/// warning before a GC, not after.
fn interpreter_report(image: Option<&[u8]>) -> String {
    let Some(interpreter) = image.and_then(elf_interpreter) else {
        return "none (no PT_INTERP: not a 64-bit little-endian dynamic ELF image)".to_owned();
    };
    format!(
        "{interpreter} exists={} nix-store={}",
        if Path::new(&interpreter).exists() {
            "yes"
        } else {
            "no"
        },
        if interpreter.starts_with("/nix/store/") {
            "yes"
        } else {
            "no"
        },
    )
}

/// The selftest's GC-root line: every `/nix/store` path this binary names
/// (interpreter, `RUNPATH`, dylibs: the paths `scripts/install-agent.sh`
/// roots), and how many have a GC root that Nix registered: a symlink in
/// `$BULKLOAD_GCROOTS` (default `${XDG_STATE_HOME:-~/.local/state}/
/// bulkload/gcroots`) resolving into the path, itself named by an entry of
/// `/nix/var/nix/gcroots/auto`. A symlink Nix does not know of (copied, or
/// made by hand) is `unregistered`, not rooted.
fn gc_roots_report(image: Option<&[u8]>) -> String {
    let Some(image) = image else {
        return "unknown (this binary could not be read)".to_owned();
    };
    let paths = store_paths(image);
    let directory = std::env::var_os("BULKLOAD_GCROOTS")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                std::env::var_os("XDG_STATE_HOME")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state"))
                    })
                    .unwrap_or_default()
                    .join("bulkload/gcroots")
            },
            PathBuf::from,
        );
    let roots: Vec<(PathBuf, PathBuf)> = std::fs::read_dir(&directory)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let link = entry.path();
                    let target = std::fs::read_link(&link).ok()?;
                    Some((link, target))
                })
                .collect()
        })
        .unwrap_or_default();
    let registered: Option<std::collections::BTreeSet<PathBuf>> =
        std::fs::read_dir("/nix/var/nix/gcroots/auto")
            .ok()
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                    .collect()
            });
    let (mut rooted, mut unregistered, mut unrooted) = (0, 0, Vec::new());
    for path in &paths {
        let links: Vec<&PathBuf> = roots
            .iter()
            .filter(|(_, target)| target.starts_with(path))
            .map(|(link, _)| link)
            .collect();
        if links.is_empty() {
            unrooted.push(path.display().to_string());
        } else if registered
            .as_ref()
            .is_none_or(|known| links.iter().any(|link| known.contains(*link)))
        {
            rooted += 1;
        } else {
            unregistered += 1;
            unrooted.push(path.display().to_string());
        }
    }
    format!(
        "store-paths={} rooted={rooted} unregistered={unregistered} registry={} unrooted={}",
        paths.len(),
        if registered.is_some() {
            "read"
        } else {
            "unreadable"
        },
        if unrooted.is_empty() {
            "-".to_owned()
        } else {
            unrooted.join(",")
        }
    )
}

/// Every `/nix/store/<hash>-<name>` the image names, top-level entries only,
/// sorted and unique (the store's hash alphabet, then its name characters).
fn store_paths(image: &[u8]) -> Vec<PathBuf> {
    const PREFIX: &[u8] = b"/nix/store/";
    let hash = |byte: &u8| matches!(byte, b'0'..=b'9' | b'a'..=b'd' | b'f'..=b'n' | b'p'..=b's' | b'v'..=b'z');
    let name = |byte: &u8| byte.is_ascii_alphanumeric() || b"+._?=-".contains(byte);
    let mut found = std::collections::BTreeSet::new();
    let mut at = 0;
    while let Some(offset) = image.get(at..).and_then(|rest| {
        rest.windows(PREFIX.len())
            .position(|window| window == PREFIX)
    }) {
        let start = at + offset;
        let body = start + PREFIX.len();
        at = body;
        let Some(digest) = image.get(body..body + 32) else {
            break;
        };
        if !digest.iter().all(hash) || image.get(body + 32) != Some(&b'-') {
            continue;
        }
        let end = image.get(body + 33..).map_or(body + 33, |rest| {
            body + 33 + rest.iter().take_while(|byte| name(byte)).count()
        });
        if end > body + 33 {
            if let Some(bytes) = image.get(start..end) {
                found.insert(PathBuf::from(String::from_utf8_lossy(bytes).into_owned()));
            }
        }
    }
    found.into_iter().collect()
}

/// The `PT_INTERP` path of a 64-bit little-endian ELF image, if it has one.
/// Every offset is checked: a malformed image yields `None`, never a panic.
fn elf_interpreter(image: &[u8]) -> Option<String> {
    const PT_INTERP: u32 = 3;
    let bytes = |at: usize, len: usize| image.get(at..at.checked_add(len)?);
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes(at, 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes(at, 4)?.try_into().ok()?));
    let u64_at =
        |at: usize| usize::try_from(u64::from_le_bytes(bytes(at, 8)?.try_into().ok()?)).ok();
    // ELFCLASS64, ELFDATA2LSB.
    if image.get(..4)? != b"\x7fELF" || image.get(4)? != &2 || image.get(5)? != &1 {
        return None;
    }
    let (phoff, phentsize, phnum) = (u64_at(0x20)?, usize::from(u16_at(0x36)?), u16_at(0x38)?);
    (0..usize::from(phnum)).find_map(|index| {
        let header = phoff.checked_add(index.checked_mul(phentsize)?)?;
        if u32_at(header)? != PT_INTERP {
            return None;
        }
        let (offset, size) = (
            u64_at(header.checked_add(8)?)?,
            u64_at(header.checked_add(32)?)?,
        );
        let found = bytes(offset, size)?;
        let found = found.split(|byte| *byte == 0).next()?;
        String::from_utf8(found.to_vec()).ok()
    })
}

fn walk_command(root: &Path) -> Result<()> {
    let root = std::fs::canonicalize(root).refuse_at("main::walk_command")?;
    let mut cache = MemoryCache::new();
    let options = WalkOptions {
        hash_policy: HashPolicy::Never,
        ..WalkOptions::new(root)
    };
    let outcome = walk::walk(&options, &mut cache)?;
    println!("rows                     {}", outcome.rows.len());
    println!("refusals                 {}", outcome.refusals.len());
    println!(
        "capped_subtrees          {}",
        outcome
            .refusals
            .iter()
            .filter(|seat| walk::is_cap_refusal(seat.refusal.code()))
            .count()
    );
    println!(
        "engine_temporaries       {}",
        outcome.engine_temporaries.len()
    );
    println!("seats_seen               {}", outcome.stats.seats_seen);
    println!("bytes_seen               {}", outcome.stats.bytes_seen);
    println!("fresh_skipped            {}", outcome.stats.fresh_skipped);
    println!(
        "bytes_reread_on_resume   {}",
        outcome.stats.bytes_reread_on_resume
    );
    println!(
        "files_statted_twice      {}",
        outcome.stats.files_statted_twice
    );
    // Every declined seat by path and code, so a capped subtree (#129) is
    // attributable, never only a count.
    for seat in &outcome.refusals {
        eprintln!(
            "refused {}: {}",
            seat.rel_path.escape_ascii(),
            seat.refusal.code()
        );
    }
    Ok(())
}

fn row_for(len: usize, meta: &std::fs::Metadata, digest: [u8; 32]) -> RowSchema {
    use std::os::unix::fs::MetadataExt as _;
    RowSchema {
        rel_path: b"selftest".to_vec(),
        kind: FileKind::Regular,
        dev: meta.dev(),
        ino: meta.ino(),
        size: u64::try_from(len).unwrap_or(u64::MAX),
        mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        ctime_ns: i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
        mode: meta.mode(),
        nlink: meta.nlink(),
        link_target: None,
        blake3: Some(digest),
    }
}

fn scratch_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("bulkload-agent-{name}-{}", std::process::id()));
    path
}

fn write_scratch(path: &std::path::Path, payload: &[u8]) -> Result<()> {
    let mut file = std::fs::File::create(path).refuse_at("main::write_scratch")?;
    file.write_all(payload).refuse_at("main::write_scratch")?;
    bulkload_agent::counters::sync_full(&file).refuse_at("main::write_scratch")?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut acc, byte| {
        let _ = write!(acc, "{byte:02x}");
        acc
    })
}
