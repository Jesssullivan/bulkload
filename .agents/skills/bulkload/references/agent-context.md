# Agent-state guide

Agent homes are provider-owned state, not generic dotfile trees.
Capture them only through `AgentCaptureV4` immutable-live A/base-B custody, with
evidence outside every live root.

Sessions remain alive.
Matching pre/post-transport live epochs enforce the final no-interaction
interval, which stays in force through Sting verification and Neo's matching
post-verify cutover-release epochs.

## Codex

Included classes are:

- every SQLite database family and committed WAL state;
- `sessions/` and `archived_sessions/` JSONL;
- `history.jsonl`;
- goals, memory, and queue state;
- rules, skills, prompts, and configuration classified as portable state; and
- `auth.json` as typed source-authority auth.

Caches, logs, shell snapshots, `.tmp/`, `plugins/cache/`, model caches, and
version probes are pruned regenerate classes before symlink admission.
Other descendants default to byte-exact `portable-private`.
Special entries and unsafe typed-state symlinks block.
A relative Codex session link that remains inside the root and names
`archived_sessions/` is portable.

SQLite capture uses a consistent backup while comparing main/WAL/SHM/journal
family metadata before and after.
The manifest stores schema and type-tagged row hashes, never values.
Composition unions compatible ordinary tables.
Trigger, view, virtual-table, or corrupt state blocks.
Schema, shared-row, and duplicate-key conflicts stage the source snapshot with
exact destination rollback custody.

## Claude

Portable classes include projects, JSONL transcripts, history, todos, plans,
memory, queue, commands, agents, skills, settings, and unknown-named binary
state.
Only explicitly typed path-bearing JSON/JSONL/text classes rewrite reviewed
source prefixes at stage time.
Claude's path-encoded project directory prefix is rewritten as well.
Both input and translated digests are bound before publication.

Claude credential files are `nonportable-auth`.
They produce a hold, preserve the destination file, and require an attended
login.
A hold is known state and does not silently disappear from plan or verification.

## Pi

Portable classes include sessions, history, archives, state, memory, queue,
skills, prompts, settings, and model configuration.
Pi auth is a typed source-authority operation with the same rollback and receipt
rules as Codex.
Generated caches/logs and `.tmp/` regenerate.
Other descendants default to portable-private exact bytes.
Unsupported special entries block.

## Managed authority, roots, and seats

Both roles use the same component-bounded managed exclusions.
Exact allowed leaves include Codex `AGENTS.md`, `config.toml`, `instructions.md`,
and managed rules/prompts/skills; Claude agents/commands/skills; and Pi
`AGENTS.md`, `APPEND_SYSTEM.md`, agents/commands/prompts/skills/tinyland.
Broad, overlapping, or private-child exclusions are rejected.

Logical roots drive translation.
Nofollow-proven backing roots drive custody and writes, leaving destination
install links unchanged.
Directory seats cover Claude Desktop, Emacs, disjoint gregs-org, Atuin, and
gstack.
File seats cover `.bash_history` and optional-absent `.zsh_history`.
Whole-home seats are invalid.

## Union rules

JSONL manifests record each exact record digest.
Equal files are a no-op unless mode differs.
Source supersets install.
Destination supersets remain.
True forks install source with exact destination rollback custody.
Destination-only session identities remain.
Shared non-append portable files are source authoritative and reversible.

Provider auth and SQLite never pass through the Git/non-Git file adapter.
Never let authentication contents, transcript contents, SQLite rows, or symlink
targets appear in logs, chat, plan summaries, or receipts.

## Acceptance after apply

An offline green receipt is necessary but not sufficient.
Retain rollback custody, then perform fresh attended checks for each provider:

1. authenticate or confirm the deliberate auth hold;
2. enumerate historical sessions through the provider UI/CLI;
3. resume one selected source-only and one destination-only session;
4. confirm dialog continuity with a non-secret nonce;
5. create and resume a new destination-native session; and
6. confirm no provider rewrote or rejected the migrated database family.

Record those acceptance results outside Bulkload's offline receipt. If a
provider rejects state, stop that provider and use `agent-rollback` or the
journaled recovery path; never hand-edit its database.
