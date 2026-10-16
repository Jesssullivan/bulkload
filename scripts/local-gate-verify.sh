#!/usr/bin/env bash
# Is PR N's local-gate receipt still valid? (OI-1003-Q199, 2026-10-16.)
#
# Usage: scripts/local-gate-verify.sh --pr N
#
# A receipt is valid only while its head is the PR's head and its main is
# main's current tip; it goes stale when either moves (another PR merging
# moves main). Run this right before `gh pr merge --match-head-commit`. It
# reads from GitHub the PR's state, base and head, main's tip, the head's
# tree and the PR's comments, and passes only when the newest gate comment
# from a writer (OWNER, MEMBER or COLLABORATOR) whose head and main match is
# a "Local gate receipt" whose line leads with PASS, records exit 0 and
# carries the head's tree. A newer "Local gate FAILED" for the same head and
# main fails it.
#
# Exit status: 0 valid; 2 usage; 3 not valid. It prints the receipt it
# accepted. gh reads its own token; nothing here prints a credential.
set -euo pipefail

die() {
  printf 'local-gate-verify: %s\n' "$*" >&2
  exit 3
}

usage() {
  sed -n '4p' "$0" | sed 's/^# //' >&2
  exit 2
}

pr=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --pr)
      [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || usage
      pr=$2
      shift 2
      ;;
    *) usage ;;
  esac
done
[[ -n "$pr" ]] || usage

pr_state=$(gh pr view "$pr" --json state,baseRefName,headRefOid \
  -q '[.state, .baseRefName, .headRefOid] | join(" ")')
read -r state base head <<<"$pr_state"
[[ "$state $base" == "OPEN main" && "$head" =~ ^[0-9a-f]{40}$ ]] ||
  die "PR #$pr is not open against main ($pr_state)"
main=$(gh api "repos/{owner}/{repo}/git/ref/heads/main" -q .object.sha)
tree=$(gh api "repos/{owner}/{repo}/commits/$head" -q .commit.tree.sha)
[[ "$main" =~ ^[0-9a-f]{40}$ && "$tree" =~ ^[0-9a-f]{40}$ ]] ||
  die "could not read main's tip or the head's tree from GitHub"
comments=$(gh api --paginate "repos/{owner}/{repo}/issues/$pr/comments" \
  -q '.[] | [.author_association, .body] | @tsv')

shape='\| head ([0-9a-f]{40}) \| tree ([0-9a-f]{40}) \| main ([0-9a-f]{40}) \|.* \| exit ([0-9]+) \| '
found=""
found_tree=""
found_exit=""
found_line=""
while IFS=$'\t' read -r association body; do
  case "$association" in
    OWNER | MEMBER | COLLABORATOR) ;;
    *) continue ;;
  esac
  case "$body" in
    "Local gate receipt (OI-1003-Q199): \`PASS | "*) verdict=PASS ;;
    "Local gate FAILED (OI-1003-Q199); not a receipt: \`FAIL | "*) verdict=FAIL ;;
    *) continue ;;
  esac
  line=${body#*: \`}
  line=${line%\`}
  [[ "$line" =~ $shape ]] || continue
  receipt_head=${BASH_REMATCH[1]}
  receipt_main=${BASH_REMATCH[3]}
  [[ "$receipt_head" == "$head" && "$receipt_main" == "$main" ]] || continue
  found=$verdict
  found_tree=${BASH_REMATCH[2]}
  found_exit=${BASH_REMATCH[4]}
  found_line=$line
done <<<"$comments"

[[ -n "$found" ]] ||
  die "no gate result on PR #$pr for head $head and main $main; a receipt is valid only while both match (run: just local-gate-receipt --pr $pr --post)"
[[ "$found" == PASS ]] ||
  die "the newest gate result for head ${head:0:12} and main ${main:0:12} is a FAIL: $found_line"
[[ "$found_tree" == "$tree" && "$found_exit" == 0 ]] ||
  die "the receipt's tree or exit does not match head $head (tree $tree): $found_line"
printf 'local-gate-verify: ok, PR #%s head %s main %s: %s\n' "$pr" "$head" "$main" "$found_line"
