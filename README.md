# bulkload

`bulkload` turns a one-off repository or `~/git` migration into a reviewed
protocol: capture two stable source catalogs, capture the destination, compile
an immutable plan, apply only explicitly safe file operations, and verify the
accepted plan against fresh destination truth while emitting a verification
receipt. It also provides a dry-run-only Codex rollout adapter that proposes
destination-absent UUIDs and proof-bound source-superset promotions only after
stable source and destination A/B captures. Its v2 capture evidence binds typed
directory claims and root lineage to an explicit filesystem-authority ID,
blocks divergent common sessions and portable file/directory collisions, and
pins bounded evidence I/O. The in-review v3 planner recognizes proof-bound
append-only prefix/superset histories, compiles a close request from every
required proof digest, and only then accepts fresh source and destination A/B
captures wrapped against that request.

Codex auth and SQLite families are explicitly copy-eligible, opt-in state
classes. Policy v7 retains the narrow attended `auth.json` install, freezes
the exact v6 compose-request and capacity artifacts as validator-only legacy
evidence, and adds a CLI-inaccessible read-only oracle for independently
hand-built SQLite bundle fixtures.
The preferred inputs are an auth-only source capture and an auth-plus-SQLite
destination capture. A full source capture is accepted, but source SQLite is
never consumed; destination SQLite is preserved exactly with zero mutation.
The workflow provides digest-accepted planning, atomic auth replacement,
journaling, offline verification, rollback, and interrupted-operation recovery.
The oracle cannot write a bundle or mint final verification authority.
SQLite composition, capacity reservation, publication, installation, combined
apply, and any claim that the session union was executed remain false and
fail-held. Any live WAL, SHM, or rollback-journal sidecar blocks immutable
SQLite capture.

Quiescence evidence is an operator procedural assertion with
`provider_writer_proof=false`; its advisory lock coordinates Bulkload only.
Private runtime and policy sources are pinned before import and bound through
the plans. An offline receipt proves bytes and preservation invariants, not
working provider authentication. A fresh attended provider turn is the final
acceptance proof. See the
[private auth install runbook](.agents/skills/bulkload/references/codex-private-auth-install.md).
The plan-only database contracts are in the
[SQLite opening-plan reference](.agents/skills/bulkload/references/codex-private-sqlite-compose-plan.md)
and [v5 close/action-plan reference](.agents/skills/bulkload/references/codex-private-sqlite-compose-action-plan.md).
The frozen v6 request and volatile capacity contract is in the
[compose-request reference](.agents/skills/bulkload/references/codex-private-sqlite-compose-request.md).
The staged writer, receipt, and first v7 read-only-oracle boundary is in the
[offline composition contract](.agents/skills/bulkload/references/codex-private-sqlite-offline-composer.md).

The repository is private. Install the self-contained skill for Codex, Pi, and
Claude with an authenticated GitHub CLI:

```bash
bash -c 'set -euo pipefail; repo=Jesssullivan/bulkload; tag=v0.1.0; tmp="$(mktemp -d)"; trap '\''rm -rf "$tmp"'\'' EXIT; gh repo clone "$repo" "$tmp/bulkload" -- --branch "$tag" --depth 1; local_tag="$(git -C "$tmp/bulkload" rev-parse "refs/tags/$tag^{tag}")"; remote_tag="$(gh api "repos/$repo/git/ref/tags/$tag" --jq .object.sha)"; test "$local_tag" = "$remote_tag"; test "$(gh api "repos/$repo/git/tags/$remote_tag" --jq .verification.verified)" = true; remote_commit="$(gh api "repos/$repo/git/tags/$remote_tag" --jq '\''.object | select(.type == "commit") | .sha'\'')"; test -n "$remote_commit"; test "$(git -C "$tmp/bulkload" rev-parse HEAD)" = "$remote_commit"; "$tmp/bulkload/scripts/install-skill.sh" --all'
```

The one-liner is safe to paste from fish or Bash. It refuses to execute the
installer unless GitHub reports the annotated `v0.1.0` tag signature verified
and the cloned tag object and checked-out commit exactly match that API proof.

Codex and Pi discover the canonical copy at `~/.agents/skills/bulkload`.
Claude receives a symlink at `~/.claude/skills/bulkload` to that same copy.
The installer validates the canonical, private-backup, and Claude destinations
before mutation, refuses symlinked directory authority, and preserves a forced
replacement under `~/.agents/backups/bulkload` before installing it.
Runtime support requires Python 3.11 or newer linked with SQLite 3.37.0 or
newer, Git, and a Unix-like host with file locking, `pread`, and dirfd support.
Codex evidence file publication requires macOS or Linux for an OS-backed atomic
no-replace rename.

For development:

```bash
nix develop
just flywheel-doctor
just flywheel-verify
just check
just demo
```

The normal build/test path is attached to GloriousFlywheel and fails closed if
the fleet profile is missing or contradictory. CI uses only the on-prem
`tinyland-nix` capability-class ARC pool and the repository-local public-read
front door. That front door calls only the public, immutable
`tinyland-inc/ci-templates` v2.13.0 actions at commit
`139bd4c7deabbe07c918dc764a3b9f054066431d`; it never resolves a private
GloriousFlywheel action, source tree, or flake. Cache endpoints remain
runner-injected authority. A repository-owned, no-value preflight validates
those raw authority-only endpoints and clears inherited credential channels
before the pinned setup action may inspect or export them; a second pass binds
the discovered reachability evidence before any Nix or Bazel command. The
exact guard is snapshotted before repository-owned source gates, then its
digest and the captured authority are rechecked immediately before each Bazel
call. Each call receives a new private Bazelisk home and an empty user home, so
workspace wrappers, ambient rc files, and netrc credentials cannot enter the
public-read route. The Nix client process is configured for token-free
`bulkload-ci` public reads: access tokens, user configuration, netrc,
flake-provided configuration, post-build hooks, secret signing keys, and
plugins are all cleared. This source contract does not attest the independent
multi-user Nix daemon's own HTTP authentication or post-build policy. Pull
requests cannot request Bazel or Attic uploads through this client; only a
trusted push to `main` may warm the shared Bazel cache. The front door never
selects a remote executor. A cache hit is cache evidence, not proof of REAPI
remote execution.

For a source-only check on an intentionally unattached machine, run
`nix develop --command just check-local`. That fallback is not CI or enrollment
evidence and must never replace the shared runner path.

The protocol is in [docs/design.md](docs/design.md), and the dated real-world
evidence is in [docs/neo-sting-retrospective.md](docs/neo-sting-retrospective.md).
The canonical agent workflow is in
[.agents/skills/bulkload/SKILL.md](.agents/skills/bulkload/SKILL.md).

CLI exits are claim-bearing: `0` means the requested positive claim holds;
`3` is an incomplete capture, `4` is a written but blocked plan, `5` is a
written failed verification, and `2` is an unsafe or malformed request. See the
skill for the full protocol and exit contract.
