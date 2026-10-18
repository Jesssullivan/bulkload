# 2026-10-16: re-seating agent sessions, facts before design (TIN-6587, R0)

Lane: estate-reseat investigation, worktree
`bulkload.worktrees/estate-reseat-20261016`, branch
`feat/estate-reseat-20261016` off `origin/main` `0d96dbd`. Linear TIN-6587
(Urgent). Spec: R0 in the draft `docs/SPEC.md` (PR #236, branch
`docs/spec-20261016`). Rulings: Q185 and Q192 (nothing dropped except
truly recreatable content), R25 (live host, never re-read), never
overwrite user data, R-N11 (never signal other agent sessions), R-N13.

Mutation receipt (R-N13):

- sting: this worktree and note. Throwaway HOMEs under
  `/dev/shm/reseat-lane/home-*`. Every binary run was inside `bwrap`
  with `--unshare-net`, so no API call was possible. In the same sandbox,
  `/srv/fast-local/jess/state` (the real `~/.claude` and `~/.codex`) was
  bind-mounted read-only. The live `~/.claude` and `~/.codex` were only
  read. The SQLite reads used `mode=ro`.
- Signals: the only processes stopped were this lane's own sandbox
  processes. These were Codex app-server daemons whose `HOME` was under
  `/dev/shm/reseat-lane/` and a private tmux server
  (`tmux -L reseat-lane`). No other session or process was signalled.
- neo (read-only): three ssh calls ran.
  1. Versions and paths.
  2. A counts-only inventory. Its script was written to `$TMPDIR` and
     deleted right after the run.
  3. sha256 of 36 transcripts whose IDs also exist on sting.

  The inventory checked liveness with `ps`, not `kill -0`. Nothing was
  written to neo's `~/.claude` or `~/.codex`.
- Nothing was posted to Linear.

Binaries under test: Claude Code `2.1.299`
(`/nix/store/w4qscw42…-claude-code-2.1.299/bin/claude`, a Bun single-file
build) and codex-cli `0.163.0-alpha.11`
(`/nix/store/208g4c23…-codex-0.163.0-alpha.11/bin/codex`). Both hosts run
the same versions (neo probe).

## 1. Claude Code 2.1.299: how `--resume` finds a session

Each fact was observed against the real binary. "Static" means it was read
from the binary's embedded JS. "Run" means it was exercised in a throwaway
HOME with tmux and `capture-pane`.

### 1.1 The project key

- **Config root (static).** The root is `CLAUDE_CONFIG_DIR`, or
  `$HOME/.claude`, NFC-normalized. The projects dir is `<root>/projects`.
- **Override (static).** `CLAUDE_CODE_PROJECT_DIR_NAME` replaces the key,
  but only when `CLAUDE_CONFIG_DIR` is also set and the value matches
  `^[A-Za-z0-9_-]{1,64}$`.
- **The cwd is canonical (static and run).** `originalCwd` is
  `realpathSync(process.cwd())`. Starting in a symlinked dir produced the
  target's key, and the transcript's `cwd` field holds the realpath.
  - On sting, `~/git` → `/srv/fast-local/jess/git`, so `~/git/lab` keys as
    `-srv-fast-local-jess-git-lab`.
  - On macOS, `/tmp` keys as `-private-tmp-…`.
- **The key function (static, verified by run).** From the binary:

  ```js
  function k(e){return e.replace(/[^a-zA-Z0-9]/g,"-")}
  function dM(e){let n=k(e);if(n.length<=200)return n;
    return `${n.slice(0,200)}-${Math.abs(pae(e)).toString(36)}`}
  function pae(t){let e=0;for(let n=0;n<t.length;n++)e=(e<<5)-e+t.charCodeAt(n)|0;return e}
  ```

  - Every UTF-16 code unit that is not `[A-Za-z0-9]` becomes `-`. That
    covers `/ . _`, space, `é`, and both halves of an emoji's surrogate
    pair, which gives `--`.
  - Keys longer than 200 characters are truncated to 200 and get `-` plus
    the base36 of |Java-style 32-bit hash over UTF-16 units|.
  - The run created dirs for `plain`, `dot.dir_under-score`, `café`,
    `emoji😀x`, `sp ace`, a 14-level long path and a symlink. A Python
    port reproduced all six keys, including the long-path hash suffix
    `-kwwfgr`.
- **Long-path fallback (static).** When the exact hashed dir is missing,
  the resolver scans dirs with the same 200-character prefix. It accepts
  one only if a transcript's head `cwd`, or a tail `{"type":"relocated",
  "relocatedCwd":…}` record, equals the current cwd. A re-seated long-path
  session must therefore sit under the exact hashed key: its recorded cwd
  is the SOURCE path and would not match.

### 1.2 What the picker reads (run)

Lab layout:

- DEST cwd `D` with linked worktree `D2`.
- Six synthetic sessions:
  - N1: native, `cwd=D`, in `key(D)`.
  - R1: recorded `cwd=/Users/jess/git/lab`, placed byte-identical in
    `key(D)`.
  - U1: same recorded cwd, left in `-Users-jess-git-lab`.
  - L1: in `key(D)`, but as a symlink to a regular file elsewhere.
  - W1: in `key(D2)`.
  - X1: recorded `cwd=/Users/jess/git/other`, in `key(D)`.

What the picker showed:

- **The directory decides, not the recorded cwd.** In `D`, the default
  picker listed X1, R1 and N1. X1 has a foreign recorded cwd and was still
  listed. Resuming it worked in `D` with no relocation prompt.
- **A symlinked `<sid>.jsonl` is invisible.** L1 is missing from both the
  default and the all-projects views, because the enumeration filters on
  `Dirent.isFile()`. A symlinked key directory holding regular files is
  listed normally (run in `home-symdir`).
- **Other keys need a toggle.** W1 appears only after Ctrl+W (all
  worktrees). U1 appears only after Ctrl+A (all projects). The Ctrl+A view
  shows a column with the recorded cwd.
- **No index.** No `history.jsonl`, `sessions/` or `~/.claude.json` entry
  existed, and the picker still listed everything. The listing comes from
  the directory alone. `history.jsonl` is prompt (up-arrow) history only,
  records `{display, pastedContents, project, sessionId, timestamp}`.
- **`--continue` picks the newest file by mtime** in `key(cwd)`. It
  continued R1 after R1's mtime was made newest. The carry must preserve
  mtimes.
- **`--resume <id>` searches further.** It checks the current key, then
  the long-path variants, then the other worktrees of the repo, then a scan
  of all of `projects/` for `<id>.jsonl` (the
  `tengu_transcript_id_scan_fallback` path). U1, L1 and W1 all resumed by
  ID from `D`.

### 1.3 Resuming writes to the transcript (run)

Opening a session without sending a prompt appends records:
`atis-latch`, `mode`, `permission-mode` and `cost-state`. In this run R1
went from 1,061 to 1,633 bytes. The earlier bytes stayed an exact prefix.
The append goes to the file where the session was found: the old
`-Users-jess-git-lab` dir for U1, and through the symlink for L1.

Consequences:

- **(a) The R0 check must never open a session in the live DEST state.**
- **(b) The first DEST resume ends sync for that session.** After it, the
  DEST file is no longer a prefix of SOURCE, so a later SOURCE append
  cannot extend it.

### 1.4 Per-session state outside the transcript

Static evidence comes from `claude purge`'s enumeration and the path
builders. Run evidence comes from the sting layout.

- **Beside the transcript**, in `projects/<key>/<sid>/`:
  - `subagents/agent-<id>.jsonl` and `.meta.json`. These are `isSidechain`
    lines with the parent `sessionId`. Their own `cwd` can be any
    worktree; they live under the parent's session dir, not their cwd's
    key.
  - `workflows/wf_*.json`, which carry an absolute `scriptPath`, and
    `workflows/scripts/`.
  - `tool-results/*.txt`.
  - `custom-title.json`.
  - `skill-injections.jsonl`.

  The `tool-results` dir is computed at runtime as
  `key(originalCwd)/<sid>/tool-results`, so it must move with the
  transcript. The transcript text also quotes absolute
  `…/.claude/projects/<srckey>/<sid>/tool-results/…` paths. Those stay as
  SOURCE paths. That is historical text and cannot be fixed without
  rewriting bytes.
- **Per project**: `projects/<key>/memory/` (auto-memory notes).
- **Global, per session**:
  - `file-history/<sid>/<hash>@vN`, referenced by session ID;
  - `tasks/<sid>/`;
  - `debug/<sid>.txt`;
  - `session-env/<sid>/`;
  - `todos/<sid>-agent-*.json` (legacy);
  - `plans/<slug>.md`, a flat namespace keyed by the transcript's `slug`
    field, so the same slug on two hosts can collide.
- **Global, per cwd**: `~/.claude.json` `projects[<abs cwd>]` holds trust,
  allowed tools and MCP servers. It is not needed for listing. A missing
  entry only means a trust dialog on first start.
- **Host-local, never carry**: `sessions/<pid>.json` with its `.key` is
  the live-process registry (`pid`, `procStart`, peer token). Also
  host-local: `shell-snapshots/`.
- **Retention.** `cleanupPeriodDays` deletes old transcripts at startup
  (default 30). Both hosts set `3650`. The carry keeps SOURCE mtimes, so a
  DEST with the default would delete re-seated sessions. The re-seat
  preflight must check this setting.

## 2. Codex 0.163.0-alpha.11: how `codex resume` finds a thread

### 2.1 Storage

- **Rollouts.** These are `sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`
  or `.jsonl.zst`, plus `archived_sessions/`.
  - Line 0 is `session_meta`, with `id`, `cwd`, `source`, `originator`,
    `history_mode` and optional `forked_from_id` and
    `history_base{thread_id, end_ordinal_exclusive, end_byte_offset}`. A
    fork base therefore depends on the parent rollout's bytes.
  - Codex itself compresses rollouts to `.jsonl.zst`
    (`rollout/src/compression.rs`).
  - Per TIN-5268 and lab finding #2012, Codex reads only `.jsonl` and
    `.jsonl.zst`, never `.jsonl.gz`.
- **`state_5.sqlite`, table `threads`.** It holds `id`, `rollout_path`
  (absolute), `cwd`, `source`, `preview`, `archived`, `*_at_ms` and more,
  with an index on `(archived, cwd, …)`. It also holds
  `thread_spawn_edges`, `thread_dynamic_tools` and `backfill_state`.
- **`thread_history_1.sqlite`** is a projection of rollouts by byte offset
  (`thread_history_projection_state.next_rollout_byte_offset`). That makes
  it recreatable: R1 resumed with no projection rows.
- **`session_index.jsonl`** holds `{id, thread_name, updated_at}`.
  **`history.jsonl`** holds prompt history `{session_id, text, ts}`.

### 2.2 Picker behavior (run)

Lab layout: CN1 (`cwd=D`), CR1 (`cwd=/Users/jess/git/lab`), CW1
(`cwd=D2`, a worktree), and CL1, a symlinked rollout. All were written into
a fresh `CODEX_HOME` with no database.

- **First start backfills once.** The fresh `state_5.sqlite` was built
  from the rollout files, and `backfill_state` was set to `complete` with
  a watermark.
  - The symlinked rollout CL1 was skipped.
  - `threads.cwd` was taken from `session_meta.cwd`.
- **The cwd filter uses `threads.cwd`, matched exactly against the
  canonical cwd.**
  - In `D` the picker showed CN1 and CW1. Codex groups sibling worktrees.
  - It hid CR1, whose recorded cwd is the SOURCE path. `--all` shows CR1.
  - With `threads.cwd` spelled through a symlink (`$H/g/lab` for
    `$H/git/lab`), the thread was hidden from both spellings of the cwd.
- **Rollouts added after the backfill are invisible.** Two later rollouts
  (CN2, CR2) never appeared in the picker: no row, no listing.
- **Registering a row is enough.**
  - `UPDATE threads SET cwd=<D>` for CR1, with the rollout bytes
    unchanged, made CR1 appear in `D`.
  - Resuming CR1 then gave no working-directory prompt.
  - A row inserted with `BEGIN IMMEDIATE` while the app-server daemon was
    running (CR3) appeared in a new picker.
- **`codex resume <id>` scans files.** It found the unregistered CN2, CR2
  and CL1 by UUID and upserted their rows.
  - CL1's row records the symlink target as `rollout_path`.
  - CR2's `cwd` came from the rollout, so it prompted "Use session
    directory (/Users/jess/git/lab) / Use current directory".
- **Resuming appends.** Opening a thread appended a
  `thread_settings_applied` event. The original 40,630 bytes stayed an
  exact prefix. The consequence is the same as in 1.3.

## 3. neo inventory (read-only, counts only, 2026-10-16 ~15:36Z)

- **Paths.** `~/.claude` is a real dir, `~/git` is a real dir, and
  `~/.codex` → `/Volumes/TinylandState/tinyland-state/codex`.
- **Claude.**
  - 112 project keys; 39 hold transcripts, 36 of those `~/git` lanes.
  - **86 transcripts**: 2.42 GB, all regular files.
  - **56,326 sidecar files, 14.3 GB** (subagents, tool-results,
    workflows).
  - 73 keys hold no transcript: 34 have only `memory/`, 22 have only
    sidecars, the rest neither. This matches retention having removed old
    transcripts before `3650` was set. The orphaned sidecars still count as
    data (R1).
  - 70 plans, 31 file-history sessions, 644 `session-env` dirs, and
    18,144 history lines.
  - The run plan sizes neo `~/.claude` at 59.3 GiB, mostly
    `agent-notes-rescue`, `projects` and `jobs`
    (`neo-full-20261016/plan.md` §3).
- **Live now.**
  - Registry entries with live PIDs: `~/git/lab` session `336ff91f…`
    (busy) and `~/git/blahaj` session `a899fb8f…` (shell).
  - No Codex process; no thread or rollout updated in the last 10
    minutes.
- **Codex.**
  - 12,457 threads: 75 `cli`, 9 `vscode`, 321 `exec`, 12,052 subagent.
    0 archived; 189 distinct cwds, 140 under `~/git`.
  - Rollout files: 1,190 `.jsonl` (19.1 GB), 6,118 `.jsonl.zst`
    (4.2 GB), 1,776 archived `.zst` (0.4 GB), 3,519 `.gz` (10.7 GB, which
    Codex cannot read), 3 `.json`, 3 `.tgz`.
  - `rollout_path` roots:
    - `~/.codex/sessions`: 12,257 threads.
    - `/Volumes/TinylandState/…/codex/sessions`: 195.
    - Ephemeral `/private/tmp/claude-501/oauth-mux-codex-*/sessions`: 2
      rows. These are already dangling-prone; see the TIN-1591 note in the
      wrapper.
    - oauth-mux managed homes: 3.

Lanes holding Claude transcripts. "DEST" is the same path under sting's
`~/git`. The "IDs on sting" column counts the session IDs found anywhere
on sting, in any form (section 4).

| SOURCE cwd (neo) | sessions | transcript MB | sidecar files / MB | live | DEST ~/git path on sting | DEST key dir exists | IDs on sting |
|---|---:|---:|---:|---|---|---|---|
| `~/git/lab` | 6 | 445 | 10025 / 3078 | 1 | ~/git/lab | yes | 5/6 |
| `~/git/GloriousFlywheel` | 2 | 362 | 12967 / 3121 |  | ~/git/GloriousFlywheel | yes | 2/2 |
| `~/git/blahaj` | 5 | 469 | 11882 / 2309 | 1 | ~/git/blahaj | yes | 3/5 |
| `~/git/greatfallstoolbus.org` | 4 | 259 | 4920 / 1273 |  | symlink → `estate/gftb-20261015/git/…` | no | 4/4 |
| `~/git/tinyland.dev` | 2 | 56 | 4323 / 1201 |  | ~/git/tinyland.dev | yes | 2/2 |
| `~/git/glorious.build` | 1 | 89 | 2114 / 696 |  | ~/git/glorious.build | yes | 1/1 |
| `~/git/spear_resumes` | 1 | 234 | 1324 / 461 |  | ~/git/spear_resumes | yes | 1/1 |
| `~/git/legalab` | 4 | 151 | 2055 / 509 |  | ~/git/legalab | yes | 4/4 |
| `~/git/tummycrypt` | 2 | 84 | 1456 / 476 |  | ~/git/tummycrypt | yes | 2/2 |
| `~/git/MassageIthaca` | 3 | 103 | 1129 / 315 |  | ~/git/MassageIthaca | yes | 3/3 |
| `~/git/jesssullivan.github.io` | 1 | 63 | 1770 / 258 |  | ~/git/jesssullivan.github.io | yes | 1/1 |
| `~` | 3 | 8 | 754 / 239 |  | not under ~/git | n/a | 2/3 |
| `~/git/video-utils` | 1 | 12 | 634 / 179 |  | ~/git/video-utils | yes | 0/1 |
| `~/git/meta` | 2 | 35 | 372 / 66 |  | symlink → `estate/gftb-20261015/git/meta` | no | 2/2 |
| `~/git/softconnect` | 1 | 7 | 282 / 82 |  | ~/git/softconnect | yes | 0/1 |
| `~/git/finances` | 1 | 14 | 162 / 48 |  | ~/git/finances | yes | 1/1 |
| `~/git/eGreg` | 1 | 9 | 89 / 27 |  | ~/git/eGreg | yes | 1/1 |
| 4 `gftb-wt-*` and `greatfallstoolbus.org.worktrees/lm1-{email-change,sweeps,departure-v2}` lanes | 9 | 8 | 3 / 0 |  | symlinks → `estate/gftb-20261015/git/…` | no | 0/9 |
| 12 `great-falls-tool-bus-infra*`, `greatfallstoolbus.org.worktrees/{lm1-recovery,tin-4216…}` and `account-controller.worktrees/review-pr15…` lanes | 22 | 9 | 13 / 0 |  | missing on sting | no | 0/22 |
| `~/git/tummycrypt.worktrees/tcfs-bulkload-m1-20260902` | 1 | 0.2 | 0 / 0 |  | missing (key dir exists) | yes | 1/1 |
| `~/git/UnnaturalScrollWheels` | 1 | 0 | 0 / 0 |  | symlink → `/srv/data/jess/archive/fast-local/git/…` | no | 1/1 |
| `/private/tmp` and `/private/tmp/claude-501/…/scratchpad/mail` | 10 | 2 | 2 / 0 |  | not under ~/git | n/a | 0/10 |

## 4. sting inventory: what neo's sessions look like there today

- **State root.** `~/.claude` → `/srv/fast-local/jess/state/claude`.
- **Keys.** 215 project keys: 80 `-Users-jess*` (copies from the
  2026-09-30 transfers) and 6 `-home-jess*`. Four of those are symlinked
  to `-srv-fast-local-*` keys.
- **Transcripts.** 417 in all: **388 are symlinks** into
  `/srv/data/jess/archive/fast-local/state/claude/projects/…`, with link
  mtime 2026-10-10 05:53, and only 29 are regular files. By fact 1.2,
  **93% of sting's own Claude sessions are invisible to `claude --resume`
  on sting today.** They can still be resumed by explicit ID.
  - Run evidence: a throwaway HOME holding the 7 real symlinks of key
    `-srv-fast-local-jess-git-tummycrypt` shows "No conversations found in
    this project."
- **The Oct-1 repair has regressed.** `neo-claude-repair-20261001T150701Z/
  RESUME-MATRIX.md` says the picker worked for 45 checkouts. Today:
  - 43 of its 45 Claude session IDs are symlinked transcripts, so they are
    invisible.
  - One (`tummycrypt.dev-site`) moved key because its checkout now
    resolves to `/srv/data/jess/archive/fast-local/git/…`.
  - 22 of the 45 checkout paths no longer exist.
- **neo's 86 transcripts on sting** were found by session ID across the
  live state, the archive and the four `/srv/data/jess/neo-*` landing
  dirs.
  - 50 have no copy anywhere on sting.
  - 36 have a copy. Classified by sha256 against neo now (full file, and
    the neo prefix of each sting copy's length), the best copy is 28
    identical and 8 an older prefix. None is newer.
  - **Copies in the live `~/.claude` (27, all symlinks):**
    - 12 identical, all under `-Users-jess*` keys;
    - 5 older prefixes, all under `-Users-jess*` keys (blahaj `f1c4ead8`,
      GloriousFlywheel `55ec2ce1`, greatfallstoolbus.org `50c884b2`,
      spear_resumes `42bcb57d`, tinyland.dev `28255a01`);
    - 10 divergent, all under `-srv-fast-local-*` keys. These are the
      Oct-1 "structured path relocation" copies. Line diff against the
      Sep-30 landing copy: in `3025961c`, 15,718 lines differ only by
      `/Users/jess/git/`→`/srv/fast-local/jess/git/`, and 1,940 lines
      differ otherwise. Some were then extended by sting resumes. They are
      not byte-identical and cannot be prefix-extended.
  - No neo session is both byte-identical and visible to the picker on
    sting. **R0 holds for no neo lane today.**
  - 11 `-Users-jess*` transcripts on sting (5.6 MB) no longer exist on
    neo. They are now unique to sting and must be kept.
- **Codex on sting.**
  - `state_5.sqlite` has 13,089 threads, 11,433 of whose IDs are also on
    neo.
  - The overlapping rows mostly have `cwd=/home/jess/git/…` or
    `/home/jess/o/lab`. Because `~/git` is a symlink, those spellings
    never match the canonical cwd (fact 2.2). By that fact, the roughly
    9,242 threads with `/home/jess/git/…` cwds are hidden from the
    cwd-filtered `codex resume` on sting. This was not checked per row in
    the live picker.
  - Rollouts: 14,431 symlinks (14,047 into the archive, 377 into
    `neo-transfer-20260930T164001Z/live/codex`), 949 regular files, 2
    dangling links.
- **The current run has not landed home state yet.**
  `neo-full-20261016` lands git into
  `/srv/fast-local/jess/estate/neo-full/git/<name>`, not `~/git/<name>`.
  - Its `estate-apply` (pid 383373) was running during this
    investigation and was not touched.
  - `estate/neo-full/home/` is empty. Agent state is planned to land
    there (`plan.md` §2).
  - The family exclusions mean GloriousFlywheel (Q172) and gftb (Q145)
    git content is out of this run's scope. D4b, which decides
    family-named transcripts (432 rows, 12.2 GiB), is still pending.

## 5. Recommended design: `estate-reseat`

Implementation belongs in the TCFS Rust workspace (bulkload `AGENTS.md`).
This note is the contract input.

### 5.1 Inputs

- **The run's landed SOURCE agent state on DEST.** Re-seat copies from
  `estate/<run>/home/.claude` and `…/codex` (with the SQLite snapshot).
  It never re-reads SOURCE (R25). Delta passes update the landing, and
  re-seat reads only the new tail bytes of grown transcripts.
- **A lane map, SOURCE cwd → DEST cwd.** The default is the same path
  relative to `~/git`, resolved on DEST with `realpath`. When the DEST cwd
  does not exist yet, use lexical resolution through the existing
  ancestors. The map must cover:
  - home (`~`) and `~/.claude/jobs/…`/`~/.claude/worktrees/…` cwds:
    home-relative;
  - `/private/tmp/…` cwds: no DEST cwd. The transcripts are kept in the
    landing, can be resumed by ID, and are reported.
- **The lane's SOURCE cwd** comes from the transcript's first `cwd` field,
  checked against `key(cwd) == srckey`. All neo transcripts passed that
  check. It does not come from decoding the key, which is lossy.

### 5.2 Claude placement (per session)

1. **Compute the DEST key.** `dest_key = dM(realpath_DEST(dest_cwd))`,
   using the exact function in 1.1, including the long-path hash. The
   ledger records `dest_key`, and each pass re-derives it. If a DEST
   checkout moves or becomes a symlink (gftb, `UnnaturalScrollWheels`),
   the key changes and the placement is reported as stale. It is never
   silently left.
2. **Write regular files** into the DEST **live**
   `~/.claude/projects/<dest_key>/<sid>.jsonl`, by copy or reflink.
   - Never a symlink: it would be invisible.
   - Never a hardlink to the landing: a DEST resume would then also
     append to the landing copy.
   - Keep the SOURCE mtime, so `--continue` and the ordering hold.
3. **Copy the sidecar dir `<sid>/`** beside the transcript, as regular
   files.
4. **Carry the global per-session state:**
   - `file-history/<sid>`, `tasks/<sid>`, `session-env/<sid>`,
     `debug/<sid>.txt` and `todos/<sid>-*`;
   - `plans/<slug>.md`: identical → skip; absent → place; different →
     keep DEST's file, put SOURCE's variant in the landing and report it;
   - `projects/<key>/memory/`: merge as a union, keeping both variants on
     conflict.
5. **Optional additive merges:**
   - `history.jsonl`: append the missing lines, deduplicated on
     `(timestamp, sessionId)`. The lines stay unchanged, so their
     `project` field remains the SOURCE path.
   - `~/.claude.json` `projects[<dest_cwd>]`: add only when absent.

   Neither is needed for the picker.
6. **Never carry** `sessions/*.json` or `*.key`, or `shell-snapshots/`.
   These are host-local, and the `sessions` registry would claim foreign
   PIDs.

### 5.3 Codex placement (per thread)

1. **Place the rollout** byte-identical as a regular file under the DEST
   `CODEX_HOME/sessions/YYYY/MM/DD/<same name>` or `archived_sessions/…`.
   - Keep the `.jsonl` or `.jsonl.zst` form as SOURCE has it.
   - Carry `.gz`, `.json` and `.tgz` as files only. Codex cannot read them.
2. **Register the thread.** No supported import path was found in this
   CLI. In the DEST `state_5.sqlite`, through SQLite with
   `BEGIN IMMEDIATE` and a busy timeout, `INSERT` the SOURCE row from the
   landed snapshot with two changes:
   - `cwd` = `realpath_DEST(dest_cwd)`;
   - `rollout_path` = the absolute real DEST path.

   Insert the matching `thread_spawn_edges` and `thread_dynamic_tools`
   rows as well. The insert is `ON CONFLICT DO NOTHING`. An existing row
   (11,433 exist on sting) is reported, never updated, unless a ruling
   allows it.
3. **Place fork parents first.** A thread is registered only after the
   parents named in `forked_from_id` and `history_base.thread_id`.
4. **Do not carry `thread_history_1.sqlite` rows.** They are a recreatable
   projection, and the landing keeps the whole store anyway.
   `session_index.jsonl` and `history.jsonl` are merged additively.

### 5.4 Never overwrite, and keep live lanes current

Each placement writes one ledger row:

- tool and ID;
- landing path, size and sha256;
- DEST path, `dest_key` or registered row, and the realpath of the DEST
  cwd;
- placed size, sha256 and mtime;
- the pass.

On each later pass:

- **Unchanged landing** (size and mtime equal the ledger): no read
  (S3, zero re-read).
- **The landing grew, and the DEST file still hashes to the ledger's
  placed hash:** append only the new SOURCE tail, verify the full hash,
  and update the ledger. The DEST hash can come from the stored digest
  plus the appended bytes, so the DEST file is not re-hashed. This is how
  neo's live lab and blahaj sessions are caught up. The final pass after
  they stop leaves DEST byte-identical.
- **A DEST file that differs from the ledger** means someone resumed the
  session on DEST (1.3, 2.2). The session is then **DEST-owned**:
  - stop extending it;
  - keep the newer SOURCE bytes in the landing;
  - report "both hosts continued `<sid>`" for explicit resolution (R1).
- **A DEST file present before the first pass:**
  - identical → adopt it into the ledger;
  - a strict prefix of SOURCE → extend it;
  - anything else → leave it, and keep both side by side.

Existing copies are kept as they are. That covers the 17 `-Users-jess*`
symlinks, the 10 Oct-1 relocated copies and the 11 sting-only transcripts.

### 5.5 The R0 check (read-only, per lane, on DEST)

1. **A static check that mirrors facts 1.1 to 1.2 and 2.2:**
   - `projects/<dM(realpath(dest_cwd))>/<sid>.jsonl` is a regular file
     whose sha256 equals the ledger's last SOURCE pass, and its sidecars
     are present;
   - a Codex row has `cwd == realpath(dest_cwd)` and `rollout_path` is an
     existing regular file whose hash matches;
   - the git state matches;
   - the hand-off note (TIN-6569) is present.
2. **A real-binary canary on each run.** Run DEST's installed `claude` and
   `codex` pickers under `bwrap --unshare-net` with a throwaway
   `HOME`/`CODEX_HOME`. Fill those homes with reflinks of the lane's
   files and a backup-API copy of `state_5.sqlite`. Check that the
   captured picker lists each session ID's title or preview. This catches
   key-function or filter drift on a version bump.

   Never open a session in the live DEST state: opening appends (1.3,
   2.2).

## 6. Decisions the operator must make

The design cannot settle these. They are material.

1. **The DEST cwd per lane.** This run lands git at
   `estate/neo-full/git/<name>`.
   - Options: key sessions to the converged `~/git/<name>` (recommended:
     that is where `claude --resume` will be typed), or to the landing
     path.
   - Keying a transcript to both creates two copies that diverge at the
     first resume.
   - The `~/git/<name>` option requires adopt or converge to run before
     re-seat.
2. **The gftb and GloriousFlywheel lanes.** Their git is out of this run
   by family ruling (Q145, Q172), yet neo holds 41 of their transcripts:
   37 gftb (with its worktrees), 2 `meta` and 2 GloriousFlywheel. On
   sting, the gftb `~/git` entries are symlinks into
   `estate/gftb-20261015`. Are their sessions re-seated, and keyed to which
   path? This ties into D4b.
3. **sting's own invisible sessions.** These are outside neo's R0 but are
   the same user expectation:
   - 388 symlinked Claude transcripts;
   - about 9,242 Codex threads whose `cwd` is spelled through the `~/git`
     symlink.

   Materializing regular files, or re-spelling `threads.cwd`, changes DEST
   state. It needs a ruling, and TIN-5268's preservation hold applies.
4. **Re-spelling existing Codex rows.** For the 11,433 already-registered
   rows, may re-seat update `cwd` and `rollout_path` in place, or only
   insert new rows?
5. **`/private/tmp` and home cwds.** Should their sessions be kept as
   resumable-by-ID only, with no lane?

## 7. Open edges (verify in the implementation)

- **Duplicate IDs in the all-projects view.** A session ID can be both in
  an old `-Users-jess*` dir and in its new key. `--resume <id>` prefers
  the current key's path first, but the Ctrl+A view may list the session
  twice.
- **Codex compression races.** Codex may compress `.jsonl` →
  `.jsonl.zst` between passes, on SOURCE or DEST. Identity must use the
  decompressed stream, and the ledger must follow the rename.
- **Live DEST processes.** Writing rows into the live DEST `state_5.sqlite`
  while sting's shared app-server daemon runs worked in the sandbox (2.2).
  Reproduce it on the real daemon in the canary before the first live
  apply.
