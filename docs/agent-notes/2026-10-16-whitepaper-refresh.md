# 2026-10-16: whitepaper refresh toward publication (OI-1003-Q155)

Lane: whitepaper subagent, run by the coordinator's workflow in
`bulkload.worktrees/s1-stack-20261015` (branch `feat/s1-stack-20261015`,
HEAD `f9a0c0e`, the `origin/main` merge `a8d837a` open and staged). Staged,
not committed: the coordinator commits signed. Nothing posted to Linear or
GitHub.

Rulings: OI-1003-Q155 (publishable quality), Q7 (proof package), Q173,
Q154, Q165, Q166, Q143, Q160, Q168, R-N13. Mutation receipt: the two files
below and this note; scratch downloads for reference checks under the
session scratchpad only. Read-only on mbp-13 (the 2026-10-15 levers series'
evidence files) and on sting's GFTB lane directory.

## Done

- `docs/whitepaper/bulkload.md` rewritten to this tree: wire v6 and the
  loopback hand-off; WP0(g) landed; the S1 stack (#210, throughput, levers,
  fold) with what each changes about durability; the superseding publish;
  estate convergence (Q144, Q149, EstateConverge); the SQLite snapshot seat
  (Q146–Q148, SqliteCarry); S4 over transfers (#225, P86); S5 HEAD, index
  and stash custody (#38, Q161, P87); S2 registry, lock trace, the clone
  exclusion and why (Q164–Q166, P84), the alongside method (Q160, Q168 on
  main); five formal modules; results labelled gated, informational (under
  load, never of record), field, or pending; the GFTB carry as field
  evidence; limits; a new section 9, open before publication.
- `docs/whitepaper/bibliography.md`: fifteen entries added and verified on
  2026-10-16 (Crossref plus OpenAlex abstracts for the two papers; fetched
  or downloaded pages and source files for the rest), [Rclone] re-verified;
  entries sorted per section.
- Two evidence files disagree on whether the 2026-10-07 copy was CPU-bound
  (#210's sample against the profile); the paper states both and follows
  the profile.

## Validation

`just secrets-scan-dir` (no leaks), `just contract-test` (24 OK),
`just repo-manifest-validate` (PASS), inside `nix develop`; a link and
anchor check of both files; every cited key resolves in the bibliography.

## Open

The paper's section 9 lists the publication gaps. The ones for the
coordinator first: commit the GFTB receipts and the 2026-10-15 levers
series as evidence files; land the stack (Q173); re-pin to `main` with
#228; fix the documentation drift the paper's section 7 names (design.md
on convergence and salvage, slo.md lacking Q143/Q154/Q35 text, the formal
README's header); close #181 and #187 on GitHub; recount the property
plan.
