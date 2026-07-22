# bulkload Development Context

`bulkload` is a manifest-first migration tool and agent skill for one-way,
reviewed movement of Git working state and selected agent context. Its default
operation is read-only.

## Start here

1. Run `git status --short --branch`.
2. Read `docs/design.md` before changing safety or manifest semantics.
3. Use `just --list`; keep Bazel targets as the CI/build/test source of truth.
4. Keep the canonical skill at `.agents/skills/bulkload/` self-contained.

## Hard rules

- Never copy credentials, auth stores, browser profiles, `.env` files, private
  keys, kubeconfigs, or decrypted secret material.
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
- Use `apply_patch` for authored edits. Do not add AI attribution to commits.

## Validation

```bash
just check
```

The minimum gate is skill validation, repository-contract validation, Python
unit tests, Bazel tests, and a redacted working-tree secret scan when gitleaks
is available.
