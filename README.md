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
Runtime support requires Python 3.11 or newer, Git, and a Unix-like host.

For development:

```bash
nix develop
just check
just demo
```

The protocol is in [docs/design.md](docs/design.md), and the dated real-world
evidence is in [docs/neo-sting-retrospective.md](docs/neo-sting-retrospective.md).
The canonical agent workflow is in
[.agents/skills/bulkload/SKILL.md](.agents/skills/bulkload/SKILL.md).

CLI exits are claim-bearing: `0` means the requested positive claim holds;
`3` is an incomplete capture, `4` is a written but blocked plan, `5` is a
written failed verification, and `2` is an unsafe or malformed request. See the
skill for the full protocol and exit contract.
