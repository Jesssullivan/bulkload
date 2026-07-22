#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
bulkload=(python3 "$root/.agents/skills/bulkload/scripts/bulkload.py")
git_safe=(git -c core.hooksPath=/dev/null -c commit.gpgsign=false \
  -c user.name=Bulkload -c user.email=bulkload@example.invalid)
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

"${git_safe[@]}" init --template= -q -b main "$scratch/source"
"${git_safe[@]}" -C "$scratch/source" commit --allow-empty -m root -q
printf 'base\n' > "$scratch/source/tracked.txt"
"${git_safe[@]}" -C "$scratch/source" add tracked.txt
"${git_safe[@]}" -C "$scratch/source" commit -m tracked -q
"${git_safe[@]}" clone -q "$scratch/source" "$scratch/destination"

printf 'changed\n' > "$scratch/source/tracked.txt"
printf 'untracked\n' > "$scratch/source/notes.txt"

"${bulkload[@]}" capture --root "$scratch/source" --mode repo --output "$scratch/a.json"
"${bulkload[@]}" capture --root "$scratch/source" --mode repo --output "$scratch/b.json"
"${bulkload[@]}" capture --root "$scratch/destination" --mode repo --output "$scratch/destination.json"
"${bulkload[@]}" plan --source-a "$scratch/a.json" --source-b "$scratch/b.json" \
  --destination "$scratch/destination.json" --output "$scratch/plan.json"
digest="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["plan_sha256"])' "$scratch/plan.json")"
"${bulkload[@]}" apply --plan "$scratch/plan.json" --accept-plan "$digest" \
  --source-root "$scratch/source" --destination-root "$scratch/destination" \
  --state-root "$scratch/state" --receipt "$scratch/receipt.json"
"${bulkload[@]}" capture --root "$scratch/destination" --mode repo --output "$scratch/after.json"
"${bulkload[@]}" verify --plan "$scratch/plan.json" --accept-plan "$digest" \
  --destination "$scratch/after.json" \
  --output "$scratch/verification.json"

printf 'bulkload demo: PASS\n'
