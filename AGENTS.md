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
  files, private keys, kubeconfigs, or decrypted secret material through a
  generic file adapter. `AgentCaptureV4` is the only copy authority for Codex,
  Claude, Pi, and declared mutable-seat state. It records hashes and typed
  structure, never credential values or SQLite rows. Claude auth is a
  nonportable hold. Descendants of a declared provider root default to typed
  portable-private state; special entries, unsafe typed state, and structural
  collisions stop planning.
- Writers are briefly quiesced for each stable capture pair. They may resume
  during preliminary preseed; final authority comes from fresh quiesced source
  and destination captures and a fresh plan. Keep writers stopped for final
  stage, apply, verify, rollback, and recovery. SQLite/WAL state uses the backup
  API, composed only when schema and shared primary-key rows agree, and checked
  independently before a final receipt. Offline receipts still require a fresh
  attended provider turn before claiming runtime authentication.
- All commands use the supported `-I -S` launcher and bind the pinned Bulkload
  application-source closure into captures and plans. This does not bind the
  Python interpreter or standard-library closure.
- Never delete source data. Final destination changes require the exact plan
  digest, a sealed final stage, an exact-overwrite capacity gate, and a complete
  reflinked rollback snapshot. A failed reflink is a hard stop; never silently
  fall back to a full duplicate. Preseed writes only beneath its external stage
  root and must not mutate any live destination path.
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
