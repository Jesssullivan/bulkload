# 2026-10-24: bounded descriptors for source retirement

Authority: R-N13 and the operator's 2026-10-24 bulkload completion request.
SPEC rule 6 says "Never halt or hinder". Rule 8 says "A rerun converges".
Lexical: XL-ACC-004.

## Failure and change

The October 23 dry run exhausted descriptors before proving source holders.
Its receipt records 29,406 directory-open failures and 10,235 unknown holder checks:
`/srv/data/jess/fast-local-relocated/bulkload/receipts/retire-codex-dryrun-20261023.out`.

`retire::answer` retained a parent directory handle for every checked candidate.
The holder probe ran only after all candidates had been checked.
The handles exhausted the process limit on the real estate.

Candidates now pass through batches using at most one quarter of the soft descriptor limit, capped at 200.
The quarter leaves room for existing handles, holder probes and path traversal.
At the observed Darwin limit of 10,240, 39,702 candidates need 199 holder probes.
An unreadable limit uses a batch of 16. No process limit is raised.
Each batch releases its handles before the next batch opens paths.
The content proof, holder check and final descriptor-relative identity check remain required.
Unknown holders still skip retirement. The fix does not raise descriptor limits.

Darwin already uses `/usr/sbin/lsof` in the base commit `5b5efd7`.
No PATH fallback or executable change was needed.

## Verification

The new regression lowers the descriptor limit to 64 in an isolated subprocess.
It checks 256 files with distinct parent directories.
A dry run must prove every candidate without removing files.
A real fixture run must remove those files, then report them absent on rerun.
The descriptor limit must remain 64.

No build or test runs on sting or neo.
Remote honey and Darwin results remain pending coordinator scheduling.
Only fixture retirement is permitted by this lane. No estate source file was retired.
