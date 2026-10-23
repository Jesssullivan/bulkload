# 2026-10-23: the operator's rulings on the held Codex rollouts

These two rulings were given on 2026-10-23 through AskUserQuestion and
relayed by the lane's coordinator. They follow the measurement in
`2026-10-23-codex-rollout-coverage.md` (receipt
`codex-rollout-coverage-20261023.{md,tsv}`) and #325 (f01f135).

## Ruling 1: "Yes, take neo's full file" (the 28 sessions that continued on neo)

When to apply it:

- The seat is a `*.jsonl.zst` whose live path links into an archive root.
- The linked file's events are a strict prefix of the source's, in order.
  "Events" uses #325's comparison: decompressed, top-level `ordinal`
  dropped, objects compared by key, host paths mapped.
- `consolidate::rollout::Coverage::Extends` reports this case.

What happens:

- The source's file, whole, replaces the link at the live name, through
  `append_log::replace_link`. This is `union_link`'s trade with the
  source's staged bytes as the output: it is made durable, then exchanged
  for the link only while the link and its target are as read.
- The swap is on the link name itself. Nothing is written through the
  link, and the archive is never touched (A-106).
- The outcome is recorded as "extended: source superset of the linked
  archive file; link replaced by a regular file".
- A rerun finds the regular file recorded consolidated and reads nothing.

## Ruling 2: "Archive original covers it" (Codex's migration rewrites)

When to apply it:

- The source is Codex's ordinal rewrite of the legacy file the link
  holds: every source line carries an ordinal and no linked line does.
- Every `turn_id` and `call_id` key in the source is also in the linked
  file.
  - Ids are matched by their bytes. A key inside an escaped string never
    matches.
  - The migration's made-up `rollout-<n>` turn ids are left out, because
    no legacy file holds them. The measurement of the 393 divergent pairs
    left them out too.

What happens:

- The seat is recorded "covered: archived original holds every turn and
  call id of Codex's rewrite". Nothing is written and the link stays.
- If any id is missing, the seat stays held with "... the archived
  original lacks K of its T turn and call ids". That hold is remembered
  against the linked file's identity, as in #325.

Precedence: covered by events, then extended, then covered by ids, then
held.

## Proofs for a later retire

A covered seat (by events, or by ids) is also recorded in
`consolidate.sqlite` `rollout_link_outcome`, against the identity digest
of the linked file it was judged against. The source-side retire (next
PR) checks this proof still holds before it removes a source file.

## The dangling link

`sessions/2026/08/12/rollout-2026-08-12T23-30-47-019ff92c-260a-7983-b225-fe76a4ebc8b5.jsonl.zst`
links to a `.zst` that the archive does not hold. The archive holds a
plain `.jsonl` under that name instead, and neo's rollout is a byte prefix
of it.

Neither ruling covers this case. The seat stays held, and its reason now
names the dangling target and the other form found instead:
"the live path is a dangling link: X does not exist; Y is there instead".
It is reported to the coordinator.

## Not changed

- A `.zst` landed as a regular file by ruling 1 is opaque to the append-log
  union. If the session later grows on neo, the next pass holds it as
  "opaque: the bytes differ".
