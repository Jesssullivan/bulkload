# First object fetch retains root custody

Authority: SPEC rules 2, 7 and 8, R-N13 and the operator's 2026-10-24
instruction to complete resumable estate consolidation. Lexical: XL-ACC-004.

`estate/streamed.rs:469` creates the live object directories before fetching.
The destination can remain an object-only tree when the source disconnects.
`estate_move.rs:1237` rejects that tree on retry because no completed landing
journal proves ownership and the tree is not yet a related Git repository.

The new `first_fetch` helper records the original item ID, source label,
source host and path, destination path and destination filesystem/inode
identity. Source device numbers do not establish custody.

`begin` creates only an empty live root and syncs it with its parent.
It publishes a complete intent without replacement before any `.git` or
payload write. A crash before publication leaves an empty root. No staging
tree or payload is created by this helper.

`prove` accepts only the bound root with an object-only `.git` skeleton.
A replaced root, symlink, working file, HEAD, config, index, ref or alternate
object store remains held. A pre-marker object tree is never adopted.
The intent binds the captured shallow frontier too. Only its exact bytes,
read without following links, may appear at `.git/shallow` before landing.
Another frontier remains held. An absent frontier permits no shallow file.
`finish` removes only the bound intent after normal durable completion.

The helper serves standalone ordinary main repositories and linked families.
Overlapping independent repositories and submodules keep their existing path.
Their interrupted first-fetch recovery remains open. Existing admission and
convergence rules still govern bare repositories, linked worktrees and
separate administration. The coordinator owns the minimal `estate_move`
hooks and must recheck the intent before landing.

This admission proof does not make the existing path-based landing
descriptor-atomic against a destination writer racing after the proof.
It must never bypass convergence of a completed Git repository.

Eleven tests cover failed fetch and resume, zero bytes on another fetch,
pre-marker interruption, unknown trees, replaced roots, native metadata,
symlinks, changed source identity, corrupt records and intent retirement.
The shallow tests cover exact bytes, another frontier and symlink refusal.
The state classifier names the new `.first-fetch` ledger explicitly.

Mutation receipt: actor=closure_diagnosis | target=owned branch
`codex/first-fetch-resume-20261024` | reason=retain custody across source
disconnect | ruling=SPEC rules 2, 7 and 8, R-N13, operator 2026-10-24 |
prior_state=partial root lost custody | result=helper and regression source
prepared. No live estate was changed.

Validation: `git diff --check` passes. No build or test ran on sting.
Honey targeted tests, formatting, lint and independent review remain required.
