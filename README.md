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
pins bounded evidence I/O. The landed v3 planner recognizes proof-bound
append-only prefix/superset histories, compiles a close request from every
required proof digest, and only then accepts fresh source and destination A/B
captures wrapped against that request.

Codex auth and SQLite families are explicitly copy-eligible, opt-in state
classes. Policy v7 retains the narrow attended `auth.json` install, freezes
the exact accepted-H7 v6 compose-request and capacity artifacts as
validator-only legacy evidence, and adds a CLI-inaccessible read-only oracle
for independently hand-built SQLite bundle fixtures.
The preferred inputs are an auth-only source capture and an auth-plus-SQLite
destination capture. A full source capture is accepted, but source SQLite is
never consumed; destination SQLite is preserved exactly with zero mutation.
The workflow provides digest-accepted planning, atomic auth replacement,
journaling, offline verification, rollback, and interrupted-operation recovery.
The oracle cannot write a bundle or mint final verification authority. SQLite
composition, capacity reservation, publication, installation, combined apply,
and any claim that the session union was executed remain false and fail-held.
Any live WAL, SHM, or rollback-journal sidecar blocks immutable SQLite capture.

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
The landed v7 source foundation and first internal read-only-oracle boundary
are in the
[offline composition contract](.agents/skills/bulkload/references/codex-private-sqlite-offline-composer.md).

The repository is private. `v0.1.0` is not installable until an attended
release publishes its signed annotated tag. The command below deliberately
fails closed while that tag is absent; the reviewed release scope is in
[the v0.1.0 release notes](docs/release-v0.1.0.md). After publication, install
the self-contained skill for Codex, Pi, and Claude with an authenticated
GitHub CLI:

```bash
bash -c 'set -euo pipefail; repo=Jesssullivan/bulkload; tag=v0.1.0; tmp="$(mktemp -d)"; trap '\''rm -rf "$tmp"'\'' EXIT; gh repo clone "$repo" "$tmp/bulkload" -- --branch "$tag" --depth 1; local_tag="$(git -C "$tmp/bulkload" rev-parse "refs/tags/$tag^{tag}")"; remote_tag="$(gh api "repos/$repo/git/ref/tags/$tag" --jq .object.sha)"; test "$local_tag" = "$remote_tag"; test "$(gh api "repos/$repo/git/tags/$remote_tag" --jq .verification.verified)" = true; remote_commit="$(gh api "repos/$repo/git/tags/$remote_tag" --jq '\''.object | select(.type == "commit") | .sha'\'')"; test -n "$remote_commit"; test "$(git -C "$tmp/bulkload" rev-parse HEAD)" = "$remote_commit"; "$tmp/bulkload/scripts/install-skill.sh" --all'
```

The one-liner is safe to paste from fish or Bash. It refuses to execute the
installer unless GitHub reports the annotated `v0.1.0` tag signature verified
and the cloned tag object and checked-out commit exactly match that API proof.

Codex and Pi discover the canonical copy at `~/.agents/skills/bulkload`.
Claude receives a symlink at `~/.claude/skills/bulkload` to that same copy.
`scripts/install-skill.sh --all` installs only that portable skill; it does not
install a `bulkload` command on `PATH`. Invoke the installed entrypoint as
`~/.agents/skills/bulkload/scripts/bulkload.py`, or use the repository/Bazel
launcher during development.
The installer validates the canonical, private-backup, and Claude destinations
before mutation, refuses symlinked directory authority, and preserves a forced
replacement under `~/.agents/backups/bulkload` before installing it.
Runtime support requires Python 3.11 or newer linked with SQLite 3.37.0 or
newer, Git, and a Unix-like host with file locking, `pread`, and dirfd support.
Codex evidence file publication requires macOS or Linux for an OS-backed atomic
no-replace rename. Every Codex evidence command requires an owner-private file
destination; stdout publication is forbidden because it bypasses pinned output
custody and post-publication input revalidation.

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
front door. Before checked-out source is consumed, a workflow-embedded
materializer requires an empty, owned, nonsymlink workspace and creates a fresh
private home, Git config, hook template, and temporary root. The sanctioned ARC
runner and GitHub runtime are the bootstrap trust root: they supply the step
environment, masking channel, fixed system Bash/base64/env/Git binaries, and
system DNS/TLS/CA. Each matrix leg blanks shell loaders and options, dynamic
loaders, proxy override channels, CA overrides, and TLS key logging before any
step. Lower-case proxy blanks live at job scope and therefore reach the local
composite and its pinned actions. FTP follows that split explicitly:
`ftp_proxy` is job-scoped, while `FTP_PROXY` is materializer-scoped. After token
and header destruction and exact checkout verification, the trusted
materializer persists the five upper-case empty proxy records through its
owned, canonical GitHub environment command file.
Privileged non-profile Bash ignores imported functions and option state, and
the materializer removes both proxy cases and every other transport channel
again before its first child. The raw read-only GitHub token is masked, copied
to a non-exported shell variable, and removed from the environment before one
fixed base64 process receives it over stdin. Raw and encoded forms are cleared
before fetch. Only the masked Basic header enters the minimal fetch environment through
`--config-env`; automatic maintenance and commit-graph writes are disabled,
and the header is cleared before detached checkout. The step then proves an
exact, full-history, clean, alternate-free checkout with a tokenless origin and
no credential, HTTP, include, or SSH configuration. This boundary does not
claim resistance to a compromised runner or GitHub bootstrap.

That front door calls only the public, immutable
`tinyland-inc/ci-templates` v2.13.0 actions at commit
`139bd4c7deabbe07c918dc764a3b9f054066431d`; it never resolves a private
GloriousFlywheel action, source tree, or flake. Attic and Bazel cache endpoint
locations remain runner-injected authority. A repository-owned, no-value
preflight validates those raw authority-only endpoints and clears inherited
credential channels before the pinned setup action may inspect or export them;
a second pass binds the discovered reachability evidence before any Nix or
Bazel command. Before
discovery, command lookup is the reviewed Nix profile plus the fixed system
path; after the pinned setup action contributes that profile through
`GITHUB_PATH`, each step supplies only the reviewed system suffix. The guard
refuses any other effective path.

`GRPC_PROXY_EXP` is an absence-only fence, not an empty-value fence: grpc-java
treats a present-empty value as an active proxy request and resolves it to
localhost port 80. The guard scans the NUL-framed process environment and
rejects both present-empty and nonempty forms before Nix setup and again before
the Java/gRPC/Bazel consumers. Successful runner command files never create
the variable. The fixed-Git fetch remains governed by its separate minimal
transport environment.

The exact guard is snapshotted before the selected repository consumer, then
its captured bytes, digest, and authority are rechecked immediately before the
selected Bazel call. Each Bazel leg receives a new private Bazelisk home, empty
user home, test temporary root, and Nix/XDG runtime home. The source leg uses a
separate private runtime home and enters `nix develop --ignore-environment`,
retaining only `HOME` and the eight reviewed Nix/XDG home selectors. Workspace
wrappers, ambient rc files, persistent caches, inherited username path
components, and netrc credentials therefore cannot enter the public-read
route. This is same-UID authority hygiene, not a filesystem sandbox.

The Nix client uses the canonical direct `/nix/store` with `store = local` and
`allow-symlinked-store = false`. Its only substituters and public keys are the
runner-injected `bulkload-ci` `main` cache plus `cache.nixos.org`; signatures
remain required. Store, daemon-socket, mirror, HTTP-family proxy, certificate,
curl, JVM, Bazel shell, and Bazelisk command selectors are empty;
`GRPC_PROXY_EXP` is absent. Trusted substituters,
remote builders, build and diff hooks, access tokens, user configuration,
netrc, flake-provided configuration, secret signing keys, and plugins are also
cleared and checked from effective Nix settings. Ambient `GIT_*`, `JUST_*`,
`NIX_MIRRORS_*`, exported Bash functions, and `SHELLCHECK_OPTS` fail closed.
Pull requests cannot request Bazel or Attic uploads through this client; only a
trusted push to `main` may warm the shared Bazel cache. The front door never
selects a remote executor. A cache hit is cache evidence, not proof of REAPI
remote execution.

CI is one literal, fail-fast-disabled matrix with independently scheduled
`source`, `build`, and `test` legs. Every leg runs exactly two workflow steps:
the embedded materializer and the repository-local composite. The composite
validates that literal gate, establishes the shared public-read boundary, and
selects exactly one terminal repository consumer: source gates, Bazel
`//:bulkload`, or Bazel `//:tests`. All later branch definitions are mutually
exclusive and skipped, so no process or action executes after the selected
consumer. The materializer registers no checkout post, and the exact nested
action pins contain no post or nested action after their Bazel invocation. The
contract tests also reject any composite step that redeclares a job-fenced
shell, function, loader, proxy, CA, or keylog channel.

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
