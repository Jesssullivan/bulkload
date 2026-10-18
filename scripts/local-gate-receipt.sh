#!/usr/bin/env bash
# Run the bulkload merge gate and print its receipt (OI-1003-Q199,
# 2026-10-16, Linear TIN-4543 comment b208cbae).
#
# Usage: scripts/local-gate-receipt.sh [--pr N [--post]] [--kind docs|code]
#                                      [--no-fetch] [--allow-overlay]
#
# Run it on sting, in a clean checkout of the exact PR head already brought
# up to date with main. It needs nix and git (and gh with --pr); the gate
# itself runs in HEAD's own dev shell, not the caller's. It:
#   1. refuses a Darwin host and neo (OI-1003-Q196: no builds or tests on
#      neo; Darwin code is checked by `just darwin-check` on pzm);
#   2. refuses a dirty tree, tracked or untracked, and an overlay: an
#      ignored or excluded file other than build output (`.envrc.local`,
#      `.env.local`, `.direnv/`, ...), a tracked file marked skip-worktree
#      or assume-unchanged, or a cargo config above the checkout or in
#      ~/.cargo. --allow-overlay runs anyway and records the overlay;
#   3. refuses a HEAD that does not contain origin/main (fetched first
#      unless --no-fetch);
#   4. derives the kind from the diff against main: docs when every changed
#      path is Markdown, else code. --kind code raises docs to code; --kind
#      docs on a code diff is refused;
#   5. with --pr, refuses unless PR N is open against main with HEAD as its
#      head commit;
#   6. from the repository root, runs CI's gate exactly: `nix flake check
#      --no-build --no-write-lock-file`, then `nix develop
#      --ignore-environment` keeping only CI's HOME, NIX_*_HOME and
#      XDG_*_HOME, running `just local-gate`. Ambient BULKLOAD_*, RUSTFLAGS,
#      CARGO_* and PATH do not reach the gate. It logs to
#      $BULKLOAD_LOCAL_GATE_DIR (default
#      ${XDG_STATE_HOME:-$HOME/.local/state}/bulkload/local-gate);
#   7. re-checks HEAD and the tree, prints one receipt line led by PASS or
#      FAIL (head, tree, main, flake.lock and log digests, exit code, kind,
#      overlay) and appends it to receipts.tsv there;
#   8. with --post, checks the PR head again and comments on the PR: a PASS
#      as "Local gate receipt", a FAIL as "Local gate FAILED", never as a
#      receipt.
#
# A receipt is valid only while its head is the PR head and its main is
# main's current tip; `just local-gate-verify --pr N` checks both right
# before the merge.
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
kind_flag=""
fetch=true
allow_overlay=false
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
      kind_flag=$2
      shift 2
      ;;
    --no-fetch)
      fetch=false
      shift
      ;;
    --allow-overlay)
      allow_overlay=true
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
for tool in git nix sha256sum ${pr:+gh}; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is not on PATH"
done

if [[ -n "$(git status --porcelain=v1 --untracked-files=normal)" ]]; then
  die "the tree is dirty; the receipt attests HEAD's tree only"
fi

# Overlays: what could steer the build while `tree` attests HEAD. Build
# output and tool caches are not overlays.
overlay=""
while IFS= read -r -d '' path; do
  case "$path" in
    target/ | bazel-* | .ruff_cache/ | .pytest_cache/ | __pycache__/ | */__pycache__/) ;;
    *) overlay+="ignored:$path"$'\n' ;;
  esac
done < <(git ls-files -z --others --ignored --exclude-standard --directory)
while IFS= read -r -d '' entry; do
  case "${entry%% *}" in
    S) overlay+="skip-worktree:${entry#* }"$'\n' ;;
    [a-z]) overlay+="assume-unchanged:${entry#* }"$'\n' ;;
  esac
done < <(git ls-files -v -z)
dir=$(dirname "$root")
for config_dir in "$HOME/.cargo" "${CARGO_HOME:-$HOME/.cargo}"; do
  for config in "$config_dir/config" "$config_dir/config.toml"; do
    if [[ -e "$config" && "$overlay" != *"cargo-config:$config"$'\n'* ]]; then
      overlay+="cargo-config:$config"$'\n'
    fi
  done
done
while :; do
  for config in "$dir/.cargo/config" "$dir/.cargo/config.toml"; do
    if [[ -e "$config" && "$overlay" != *"cargo-config:$config"$'\n'* ]]; then
      overlay+="cargo-config:$config"$'\n'
    fi
  done
  [[ "$dir" != / ]] || break
  dir=$(dirname "$dir")
done
if [[ -n "$overlay" && "$allow_overlay" != true ]]; then
  die "overlay present; remove it or pass --allow-overlay to record it:"$'\n'"$overlay"
fi
if [[ -n "$overlay" ]]; then
  overlay_field=$(printf '%s' "$overlay" | tr '\n' ',' | tr -c 'A-Za-z0-9._/:,+@=-' '?')
  overlay_field=${overlay_field%,}
else
  overlay_field=none
fi

if [[ "$fetch" == true ]]; then
  git fetch -q origin main
fi
head=$(git rev-parse --verify HEAD)
tree=$(git rev-parse --verify 'HEAD^{tree}')
main=$(git rev-parse --verify origin/main)
git merge-base --is-ancestor "$main" "$head" ||
  die "HEAD does not contain origin/main ${main:0:12}; bring the PR up to date first"

kind=docs
while IFS= read -r -d '' path; do
  [[ "$path" == *.md ]] || kind=code
done < <(git diff --no-renames --name-only -z "$main" "$head")
if [[ "$kind_flag" == docs && "$kind" == code ]]; then
  die "--kind docs refused: the diff against main changes non-Markdown paths, so the kind is code"
fi
kind=${kind_flag:-$kind}

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

# CI's gate, verbatim but for the recipe (the contract test derives this
# block from the action): the flake check, then a clean dev shell.
cd "$root"
gate=0
(
  nix flake check --no-build --no-write-lock-file || exit
  exec nix develop --no-write-lock-file --ignore-environment \
    --keep HOME \
    --keep NIX_CACHE_HOME \
    --keep NIX_CONFIG_HOME \
    --keep NIX_DATA_HOME \
    --keep NIX_STATE_HOME \
    --keep XDG_CACHE_HOME \
    --keep XDG_CONFIG_HOME \
    --keep XDG_DATA_HOME \
    --keep XDG_STATE_HOME \
    .#default --command just local-gate
) >"$log" 2>&1 || gate=$?
finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)

[[ "$(git rev-parse HEAD)" == "$head" &&
  -z "$(git status --porcelain=v1 --untracked-files=normal)" ]] ||
  die "HEAD or the tree changed while the gate ran; no receipt"

if [[ "$gate" -eq 0 ]]; then
  verdict=PASS
else
  verdict=FAIL
fi
flake_lock=$(sha256sum flake.lock | cut -d' ' -f1)
log_sha=$(sha256sum "$log" | cut -d' ' -f1)
receipt="$verdict | local terminal gate ($host) | PR #${pr:-none} | head $head"
receipt+=" | tree $tree | main $main | flake.lock sha256 $flake_lock"
receipt+=" | log sha256 $log_sha | exit $gate | $started..$finished"
receipt+=" | kind $kind | overlay $overlay_field"
receipt+=" | nix flake check, nix develop --ignore-environment: just local-gate"
receipt+=" | OI-1003-Q199 R-N13"
printf '%s\n' "$receipt" >>"$state_dir/receipts.tsv"
printf '%s\n' "$receipt"
printf 'local-gate-receipt: log %s\n' "$log" >&2

if [[ "$post" == true ]]; then
  status=4
  [[ "$(pr_head)" == "OPEN main $head" ]] ||
    die "PR #$pr head moved off $head during the gate; not posted"
  if [[ "$verdict" == PASS ]]; then
    heading="Local gate receipt (OI-1003-Q199)"
  else
    heading="Local gate FAILED (OI-1003-Q199); not a receipt"
  fi
  gh pr comment "$pr" --body "$heading: \`$receipt\`" >/dev/null ||
    die "posting to PR #$pr failed"
  unset status
fi
exit "$gate"
