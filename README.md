# bulkload

`bulkload` turns a one-off repository or `~/git` migration into a reviewed
protocol: capture two stable source catalogs, capture the destination, compile
an immutable plan, apply only explicitly safe file operations, and verify the
accepted plan against fresh destination truth while emitting a verification
receipt.

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
Runtime support requires Python 3.11 or newer, Git, and a Unix-like host.

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
composite and its pinned actions. After token and header destruction and exact
checkout verification, the trusted materializer persists the four upper-case
empty records through its owned, canonical GitHub environment command file.
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
GloriousFlywheel action, source tree, or flake. Cache endpoints remain
runner-injected authority. A repository-owned, no-value preflight validates
those raw authority-only endpoints and clears inherited credential channels
before the pinned setup action may inspect or export them; a second pass binds
the discovered reachability evidence before any Nix or Bazel command. The
exact guard is snapshotted before the selected repository consumer, then its
digest and the captured authority are rechecked immediately before a selected
Bazel call. Each Bazel leg receives a new private Bazelisk home and an empty
user home, so workspace wrappers, ambient rc files, and netrc credentials
cannot enter the public-read route. The Nix client process is configured for
token-free `bulkload-ci` public reads: access tokens, user configuration, netrc,
flake-provided configuration, post-build hooks, secret signing keys, and
plugins are all cleared. This source contract does not attest the independent
multi-user Nix daemon's own HTTP authentication or post-build policy. Pull
requests cannot request Bazel or Attic uploads through this client; only a
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
