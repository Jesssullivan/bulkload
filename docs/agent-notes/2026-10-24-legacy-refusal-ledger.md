# Legacy refusal listing, 2026-10-24

Authority: SPEC goal 1 and rules 2, 4 and 8. The coordinator requested
bounded handling of the legacy `refused.tsv` receipt. Lexical: XL-ACC-004.

Read-only inspection of `homes-20261021/refused.tsv` found 17,478 complete
LF-terminated records, each with three tab-separated fields: refusal code,
session label and relative seat path. The file has no header. Its size is
2,274,691 bytes and its longest record is 241 bytes. It predates the latest
closure, so it is historical evidence rather than the current refusal list.

`state_payload.rs` recognizes only that root filename after validating every
record. The validator accepts the observed refusal vocabulary, native Claude
and Codex labels and relative seat paths. It bounds each record at 4,096
bytes. Extra fields, incomplete records, control bytes, unknown labels and
absolute or traversing paths fail validation. Other TSV paths remain payload.

The file is opened without following links. Its identity, length and write
times must match the listing before and after the read. Validation changes
no receipt, state file, closure disposition or transfer outcome. Current
closure still owns acceptance. The legacy listing counts as ledger bytes.

Fixture tests cover a valid listing, unrelated and nested TSV files, retired
directories, malformed and oversized records, symlinks and changed identity.
`git diff --check` passed. No build, test or formatter ran on sting or neo.
Honey validation remains for the coordinator. No live database was opened
and no estate bytes moved.

## Validation, 2026-10-24

Honey and PZM each pass all nine state-payload tests and the native
integration case that refuses actual payload left in move state. Formatting,
workspace clippy with warnings denied and gitleaks pass on both hosts.
Independent review found no blocking defect. The open helper includes
`O_NOFOLLOW` and `O_NONBLOCK`. Descriptor and named-path metadata are checked.
High-bit path bytes remain valid Unix path bytes.

The actual legacy receipt remains unchanged. This code has not been selected
by a live homes run yet. Its classification cannot clear current refusals.

Mutation receipt: actor=coordinator | target=owned source increment |
reason=classify a validated historical receipt accurately | ruling=operator
"proceed, neo online", SPEC goal 1 and rules 2, 4 and 8, R-N13 |
prior_state=legacy evidence counted as payload | result=reviewed source and
focused checks pass, live application pending.
