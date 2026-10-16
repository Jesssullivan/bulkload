#!/usr/bin/env bash
# Does a diff touch Darwin code? (OI-1003-Q199, 2026-10-16.) A yes means the
# PR needs `just darwin-check` on pzm before it merges, never on neo
# (OI-1003-Q196); if pzm is on hold, the check is deferred and recorded.
#
# Usage: scripts/darwin-touched.sh [BASE [HEAD]]   (default: origin/main HEAD)
#
# It reads the diff a PR shows (BASE...HEAD, from their merge base), renames
# split into delete and add, and counts a file when:
#   - its path names Darwin (darwin, macos or apple; e.g. io/sys_darwin.rs);
#   - it is Rust and either side carries a Darwin cfg: target_os = "macos" or
#     "ios", target_vendor = "apple", or not(target_os = "linux"), which is
#     compiled everywhere but Linux;
#   - it fixes what Darwin compiles: a Cargo.toml, Cargo.lock,
#     rust-toolchain.toml, flake.nix or flake.lock.
# It prints one `darwin: <path> (<reason>)` line per such file, then a
# verdict. Exit 0: touched; 1: not touched; 2: error.
set -euo pipefail

if [[ $# -gt 2 ]]; then
  sed -n '6p' "$0" | sed 's/^# //' >&2
  exit 2
fi
base=${1:-origin/main}
head=${2:-HEAD}
for rev in "$base" "$head"; do
  if ! git rev-parse --verify -q "$rev^{commit}" >/dev/null; then
    printf 'darwin-touched: unknown revision %s\n' "$rev" >&2
    exit 2
  fi
done
merge_base=$(git merge-base "$base" "$head")
marker='target_os *= *"(macos|ios)"|target_vendor *= *"apple"|not\( *target_os *= *"linux" *\)'

carries_marker() {
  local rev=$1 path=$2
  git cat-file -e "$rev:$path" 2>/dev/null &&
    git show "$rev:$path" | grep -Eq "$marker"
}

touched=0
while IFS= read -r -d '' path; do
  reason=""
  shopt -s nocasematch
  if [[ "$path" =~ (darwin|macos|apple) ]]; then
    reason="Darwin path"
  fi
  shopt -u nocasematch
  case "${path##*/}" in
    Cargo.toml | Cargo.lock | rust-toolchain.toml | flake.nix | flake.lock)
      reason=${reason:-build inputs}
      ;;
  esac
  if [[ -z "$reason" && "$path" == *.rs ]]; then
    if carries_marker "$merge_base" "$path" || carries_marker "$head" "$path"; then
      reason="Darwin cfg"
    fi
  fi
  if [[ -n "$reason" ]]; then
    printf 'darwin: %s (%s)\n' "$path" "$reason"
    touched=$((touched + 1))
  fi
done < <(git diff --no-renames --name-only -z "$merge_base" "$head")

if [[ $touched -gt 0 ]]; then
  printf 'darwin-touched: yes, %d file(s); run darwin-check on pzm before merge (OI-1003-Q199)\n' "$touched"
  exit 0
fi
printf 'darwin-touched: no; no darwin-check needed (OI-1003-Q199)\n'
exit 1
