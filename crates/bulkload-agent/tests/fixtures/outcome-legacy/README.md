# Legacy outcome records (WP3 PR 3 fixture)

Each `NAME.outcome` file is an `{item}.outcome` record exactly as the estate
writer wrote it before WP3 PR 3: the postcard encoding of the tuple
`(source, outcome, reason)`, with `source` = `/src/legacy`, no magic prefix.
One file per outcome string the legacy writer produced, three `refused`
records (a plain code, a code with an escaped path, a bare `IO` with errno),
one `refused` record whose reason names no code, and one truncated record.

`outcome::tests::checked_in_legacy_fixture_reads` asserts every file is byte
for byte the legacy encoding of its row, and reads each through the legacy
reader. Do not regenerate these with the current writer.
