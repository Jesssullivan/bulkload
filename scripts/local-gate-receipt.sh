#!/usr/bin/env bash
# Run the bulkload merge gate and print its receipt (OI-1003-Q199,
# 2026-10-16, Linear TIN-4543 comment b208cbae).
#
# Usage: scripts/local-gate-receipt.sh [--pr N [--post]] [--kind docs|code]
#                                      [--no-fetch]
#
# Run it on sting inside `nix develop .#default`, in a clean checkout of the
# exact PR head already brought up to date with main. It:
#   1. refuses a Darwin host and neo (OI-1003-Q196: no builds or tests on
#      neo; Darwin code is checked by `just darwin-check` on pzm);
#   2. refuses a dirty tree, tracked or untracked, and a HEAD that does not
#      contain origin/main (fetched first unless --no-fetch);
#   3. with --pr, refuses unless PR N is open against main with HEAD as its
#      head commit;
#   4. runs `just local-gate`, logging to $BULKLOAD_LOCAL_GATE_DIR (default
#      ${XDG_STATE_HOME:-$HOME/.local/state}/bulkload/local-gate);
#   5. prints one receipt line (head, tree, main, flake.lock and log
#      digests, exit code) and appends it to receipts.tsv there;
#   6. with --post, checks the PR head again and comments the receipt on it.
#
# Exit status: the gate's own; 2 for usage; 3 when a check refuses; 4 when
# the post fails. Nothing here prints a credential: gh reads its own token
# and the receipt holds only refs, digests and the exit code.
set -euo pipefail

die() {
  printf 'local-gate-receipt: %s\n' "$*" >&2
  exit "${status:-3}"
}

usage() {
  sed -n '5,6p' "$0" | sed 's/^# //' >&2
  exit 2
}

pr=""
post=false
kind=code
fetch=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --pr)
      [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || usage
      pr=$2
      shift 2
      ;;
    --post)
      post=true
      shift
      ;;
    --kind)
      [[ $# -ge 2 && "$2" =~ ^(docs|code)$ ]] || usage
      kind=$2
      shift 2
      ;;
    --no-fetch)
      fetch=false
      shift
      ;;
    *) usage ;;
  esac
done
if [[ "$post" == true && -z "$pr" ]]; then
  usage
fi

root=$(git rev-parse --show-toplevel)
cd "$root"

host=$(uname -n)
host=${host%%.*}
if [[ "$(uname -s)" == Darwin || "$host" == neo* ]]; then
  die "refused on $host: the merge gate runs on sting; Darwin code runs darwin-check on pzm (OI-1003-Q196)"
fi
for tool in just cargo actionlint gitleaks bazelisk shellcheck ruff; do
  command -v "$tool" >/dev/null 2>&1 ||
    die "$tool is not on PATH; run inside nix develop .#default"
done

if [[ -n "$(git status --porcelain=v1 --untracked-files=normal)" ]]; then
  die "the tree is dirty; the receipt attests HEAD's tree only"
fi
if [[ "$fetch" == true ]]; then
  git fetch -q origin main
fi
head=$(git rev-parse --verify HEAD)
tree=$(git rev-parse --verify 'HEAD^{tree}')
main=$(git rev-parse --verify origin/main)
git merge-base --is-ancestor "$main" "$head" ||
  die "HEAD does not contain origin/main ${main:0:12}; bring the PR up to date first"

pr_head() {
  gh pr view "$pr" --json state,baseRefName,headRefOid \
    -q '[.state, .baseRefName, .headRefOid] | join(" ")'
}
if [[ -n "$pr" ]]; then
  [[ "$(pr_head)" == "OPEN main $head" ]] ||
    die "PR #$pr is not open against main with head $head"
fi

state_dir=${BULKLOAD_LOCAL_GATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/bulkload/local-gate}
mkdir -p "$state_dir"
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
log="$state_dir/${head:0:12}-${started//:/}.log"

gate=0
just --justfile "$root/justfile" --working-directory "$root" local-gate >"$log" 2>&1 ||
  gate=$?
finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)

[[ "$(git rev-parse HEAD)" == "$head" &&
  -z "$(git status --porcelain=v1 --untracked-files=normal)" ]] ||
  die "HEAD or the tree changed while the gate ran; no receipt"

flake_lock=$(sha256sum flake.lock | cut -d' ' -f1)
log_sha=$(sha256sum "$log" | cut -d' ' -f1)
receipt="local terminal gate ($host) | PR #${pr:-none} | head $head | tree $tree"
receipt+=" | main $main | flake.lock sha256 $flake_lock | log sha256 $log_sha"
receipt+=" | exit $gate | $started..$finished | kind $kind | just local-gate"
receipt+=" | OI-1003-Q199 R-N13"
printf '%s\n' "$receipt" >>"$state_dir/receipts.tsv"
printf '%s\n' "$receipt"
printf 'local-gate-receipt: log %s\n' "$log" >&2

if [[ "$post" == true ]]; then
  status=4
  [[ "$(pr_head)" == "OPEN main $head" ]] ||
    die "PR #$pr head moved off $head during the gate; not posted"
  gh pr comment "$pr" --body "Local gate receipt (OI-1003-Q199): \`$receipt\`" >/dev/null ||
    die "posting to PR #$pr failed"
  unset status
fi
exit "$gate"
