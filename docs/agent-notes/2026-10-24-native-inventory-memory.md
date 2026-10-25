# 2026-10-24: source inventory retains paths instead of file rows

Authority: SPEC rules 5 and 6 and the operator's October 24 direction.
R-N13. Lexical: XL-ACC-004.

| stream | owner | repo/branch | state/evidence | next |
| --- | --- | --- | --- | --- |
| Source inventory | Bulkload coordinator | codex/inventory-memory-20261024 | Source implemented, validation pending | Targeted honey and pzm validation |

`moving::nests` previously called `nested_repositories`, whose full filesystem
census allocated a `RowSchema` for each ordinary and rebuildable file.
Inventory only needed the nested paths. It now uses `streamed_omissions`
with rebuildable traversal enabled, preserving explicit rebuildable-nest notes.
The walker keeps one directory iterator per active depth and retained result
paths. It does not collect ordinary file metadata.

Dirty nests and stale pointers are judged as their own inventory items.
A malformed sibling cannot suppress discovery of a valid nested repository.
Unreadable walks produce typed notes. No new exclusion or staging path exists.

Native source logs announce preflight, inventory and capture before each starts.
The event rows are diagnostics. Existing completion rows and DONE marks retain
their meaning.

Tests cover dirty nests, rebuildable notes, tracked output paths, ordinary files,
non-Git dot directories, missing roots, longest path maps and stale pointers.
The inventory regression requires zero full census walks across 2,048 files.

Neo's repeated quiet SSH exits do not establish an out-of-memory cause.
The first run preceded a host restart. Later runs ended with the same boot time.
Selected crash reports and filtered logs supplied no bulkload termination reason.
The source call graph establishes a memory cost, not its role in those exits.

Validation pending. No build or test ran on neo or sting.

## Qualification, 2026-10-24

Honey and PZM pass four inventory integration cases, five source-root
cases, nine moving unit cases, both plain-walk and Bazel-layout cases,
two nested-move cases and the rebuildable-nest move case. Formatting,
workspace clippy with warnings denied and gitleaks pass on both hosts.
The first Darwin lint found a redundant fixture clone. It was removed.
The corrected Darwin run passes. Independent review found no blocking defect.

The bound is traversal depth plus discovered paths. The outer ordinary-tree
walk still collects one directory's entries. This is not globally constant
memory and does not prove the SSH termination cause.

The prior long attempt eventually emitted a provisional inventory before
the owned controller was interrupted. It did not complete capture. Its
1,318 planned items cannot be combined with the older applied numerator.
The next selected source build will emit phase starts before quiet work.

Mutation receipt: actor=coordinator | target=owned source increment |
reason=remove unnecessary retained file-row census | ruling=operator
2026-10-24, SPEC rules 5 and 6, R-N13 | prior_state=path discovery retained
all file rows | result=reviewed path-only discovery with remote qualification.
