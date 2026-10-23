# 2026-10-23: Codex rollouts linked into the archive

SPEC rules served:

- Rule 4, "Consolidate, never pick a winner": a case bulkload cannot merge
  is a typed hold.
- Rule 7, "Never clobber".
- Done item 2: the closure shows every hold, with why.
- Operator ruling A-106: the archive is never modified.

## Finding (read-only measurement)

The full receipt is
`/srv/data/jess/fast-local-relocated/bulkload/receipts/codex-rollout-coverage-20261023.{md,tsv}`.

homes-20261021 held 7,844 Codex rollouts with the reason "the live path is
a link or another kind".

- Each live path on sting is a link into
  `/srv/data/jess/archive/fast-local/state/codex`, a `--replace-archived-dest`
  root.
- All 7,844 are `*.jsonl.zst`. One link is dangling: its archive holds a
  plain `.jsonl` instead.

I sampled 600 pairs (neo against the archive), stratified, with no random
seed. Counts are weighted to the 7,844:

- ~33% are equal once decompressed. Only the zstd framing differs.
- ~4.5% are equal once the top-level `ordinal` is dropped, objects are
  compared by key, and the `/Users/jess` and `/home/jess` spellings are
  treated as one path. Without the map, none are.
- 28 continued on neo past the archive copy: the archive is a byte prefix
  of neo's.
- ~62% are Codex's rollout migration on neo (2026-09-30 19:59, under
  `codex/rollout-migrations`). neo's file is a lossy re-projection: every
  line carries an ordinal, timestamps are flattened, events are
  re-encoded, and a fork's replay is dropped. The archive holds the legacy
  original.
- Every turn and call id in neo's file is also in the archive's. Line for
  line, though, neither file is a subset of the other.

## Change (branch feat/codex-rollout-covered-20261023)

Before this change, a `*.jsonl.zst` seat whose live path links to a regular
file under an archive root was held at decision, with nothing read. Now:

- **Decide.** If the seat was covered before, it is reused. If it was held
  before against the same linked file (`consolidate.sqlite`
  `rollout_link_held`, matched by the linked file's identity), it is held
  again with its stored reason and nothing read. Otherwise bulkload asks
  for the manifest and stages the seat as an append log, in the live
  directory's private name.
- **Judge** (`consolidate::rollout::coverage`). Both sides are streamed,
  and `.zst` is decompressed on either side. Each source event is kept as
  its BLAKE3 and a count. A linked line whose leading timestamp no source
  event holds is skipped without parsing. Reading stops as soon as every
  source event is found.
  - If every event is found, the seat is recorded consolidated with the
    reason "covered: source events within the linked file,
    ordinal-normalized".
  - Otherwise the seat is held with the reason "source holds N of its M
    events the linked file lacks". When the source is all-ordinal and the
    link has no ordinal lines, the reason adds that the source is Codex's
    ordinal rewrite of a legacy rollout.
- **Writes.** Nothing is written in either case. The staged file is
  discarded, and the link and the archive are not touched.
- `TransferStats::held` reasons are now `Cow<'static, str>`, so a reason
  can carry the count.

Not changed:

- Plain `*.jsonl` archive links still go through `union_link`.
- A dangling link is still held "the live path is a link or another kind".

## Not covered by this rule

About 4,900 seats stay held, with exact counts:

- the migration rewrites;
- the 28 that continued on neo;
- the 22 whose cwd was mapped through another pair.

For the rewrites, the receipt proposes using Codex's own migration of the
archived legacy file, into a private output, as the oracle. That needs an
operator decision first.

## Validation

All on honey, `nice -n 19`:

- the targeted tests;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets --locked -- -D warnings`;
- gitleaks.

The commands and results are in the PR.
