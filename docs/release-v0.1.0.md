# bulkload v0.1.0 release notes

This document defines the reviewed scope for Bulkload's prospective first
private, signed release. No `v0.1.0` release exists until its signed annotated
tag is published and independently verified. That release tag is the install
authority; source version strings or a green default branch are not releases.

## Included

- Repository and `~/git` catalog, plan, additive apply, and independent verify
  flows with explicit digest acceptance.
- Bounded Codex session-union capture and proof-gated planning.
- The policy-v7 attended `auth.json` install, rollback, recovery, and offline
  verification path with exact destination SQLite preservation.
- An internal, CLI-inaccessible read-only SQLite oracle for independently
  constructed fixtures.
- The user-scope installer for the canonical Codex/Pi skill and Claude symlink.
- Cache-backed local CI on the `tinyland-nix` capability; no remote execution
  claim.

## Explicitly not included

- SQLite writer, composition, independent final verifier, publication,
  installation, combined apply, or cutover.
- Provider-runtime acceptance or a completed source-to-destination session
  union. Those require a fresh attended provider turn after offline evidence.
- A global `bulkload` executable on `PATH`; `--all` installs the skill only.
- Continuous synchronization, TCFS runtime integration, deletion/mirror mode,
  or broad all-repository/all-agent/all-dotfile continuity.

The installed entrypoint is
`~/.agents/skills/bulkload/scripts/bulkload.py`. Every private-state operation
retains the existing quiescence, exact-digest, rollback, and provider-turn
boundaries documented in the skill and runbook.
