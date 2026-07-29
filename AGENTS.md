# bulkload Development Context

`bulkload` is a manifest-first migration tool and agent skill for one-way,
reviewed movement of Git working state and selected agent context. Its default
operation is read-only.

## Start here

1. Run `git status --short --branch`.
2. Read `docs/design.md` before changing safety or manifest semantics.
3. Use `just --list`; keep Bazel targets as the CI/build/test source of truth.
4. Keep the canonical skill at `.agents/skills/bulkload/` self-contained.
5. Read the imported `justfile.flywheel` contract before changing build or CI
   routing.

## Hard rules

- Never promote credentials, auth stores, databases, browser profiles, `.env`
  files, private keys, kubeconfigs, or decrypted secret material through the
  generic repository-file adapter. Codex `auth.json` and provider-owned SQLite
  families are copy-eligible only through an explicit typed, private,
  provider-specific plan. Policy v4 implements an attended atomic `auth.json`
  install plus a separate four-pass, session-bound SQLite classification
  opening request. Prefer an auth-only source capture and an auth-plus-SQLite
  destination capture for auth install; a full source capture is accepted but
  its SQLite is never consumed by that installer. Destination SQLite must
  remain byte-exact with zero auth-install mutations. SQLite composition,
  publication, installation, and combined apply remain fail-held. Never log
  credential values or raw database contents. Any live
  SQLite WAL, SHM, or rollback-journal sidecar hard-stops immutable capture.
- A private-state quiescence attestation is an operator procedural fence with
  `provider_writer_proof=false`. Its advisory `flock` coordinates cooperating
  Bulkload processes only; it does not stop or prove the absence of provider
  writers.
- Private-state operations must use the pre-import pinned runtime closure bound
  into their plans. Offline apply/verify receipts do not prove working provider
  authentication; require a fresh attended provider turn before claiming it.
- Never delete source data. Never delete destination data in v1.
- Never apply a plan without an exact plan digest supplied by the operator.
- Require two byte-stable source catalogs before creating an actionable plan.
- Treat Git refs/objects, worktree bytes, worktree administration, agent
  transcripts, generated caches, and authentication as separate state classes.
- Do not infer worktree identity from a basename; record path, branch, HEAD,
  common Git directory, dirt, and remote backing separately.
- Do not invoke `cmux` or another terminal multiplexer. Migration tooling may
  inventory session artifacts, but terminal ownership remains with the operator.
- Do not execute Home Manager switches, deploys, activations, credential
  ceremonies, or TCFS runtime operations from this repository.
- CI runs only on the GloriousFlywheel `tinyland-nix` capability class. Never
  add a GitHub-hosted label, bare `self-hosted`, dynamic fallback, repo-shaped
  workflow label, or an unaudited reusable workflow.
- Keep `.bazelrc.flywheel` endpoint-free and drive Bazel through
  `gloriousflywheel-bazel`. PRs are cache-read-only; cache hits do not prove
  remote execution.
- Use `apply_patch` for authored edits. Do not add AI attribution to commits.

## Validation

```bash
just check
```

`just check` requires an attached GloriousFlywheel profile and is the normal
build/test path. `just ci` additionally verifies the profile, evaluates the
flake, scans committed history, and exercises `//:bulkload` plus `//:tests` on
the shared cache. `just check-local` is an explicit source-only fallback for an
unattached development shell and is never cache, runner, or enrollment proof.
