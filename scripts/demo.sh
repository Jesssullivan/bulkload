#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
bulkload=("$root/.agents/skills/bulkload/scripts/bulkload.py")
rsync_path="$(command -v rsync)"
git_safe=(git -c core.hooksPath=/dev/null -c commit.gpgsign=false \
  -c user.name=Bulkload -c user.email=bulkload@example.invalid)
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

source_home="$scratch/source"
destination_home="$scratch/destination"
source_git="$source_home/git"
destination_git="$destination_home/git"
mkdir -p "$source_git" "$destination_git"
for home in "$source_home" "$destination_home"; do
  mkdir -p "$home/.codex" "$home/.claude" "$home/.pi/agent"
done

"${git_safe[@]}" init --template= -q -b main "$source_git/repo"
printf 'base\n' > "$source_git/repo/tracked.txt"
"${git_safe[@]}" -C "$source_git/repo" add tracked.txt
"${git_safe[@]}" -C "$source_git/repo" commit -m tracked -q
"${git_safe[@]}" clone -q "$source_git/repo" "$destination_git/repo"
printf 'changed\n' > "$source_git/repo/tracked.txt"
printf 'untracked\n' > "$source_git/repo/notes.txt"
printf '{"token":"source-demo"}\n' > "$source_home/.codex/auth.json"
printf '{"token":"destination-demo"}\n' > "$destination_home/.codex/auth.json"

capture() {
  local role=$1 home=$2 git_root=$3 output=$4
  "${bulkload[@]}" agent-capture \
    --role "$role" \
    --home "$home" \
    --git-root "$git_root" \
    --rsync-path "$rsync_path" \
    --path-map "$source_home=$destination_home" \
    --path-map "$source_git=$destination_git" \
    --acknowledge-writers-quiesced \
    --output "$output"
}

capture source "$source_home" "$source_git" "$scratch/source-a.json"
capture source "$source_home" "$source_git" "$scratch/source-b.json"
capture destination "$destination_home" "$destination_git" "$scratch/destination-a.json"
capture destination "$destination_home" "$destination_git" "$scratch/destination-b.json"

"${bulkload[@]}" agent-plan \
  --source-a "$scratch/source-a.json" \
  --source-b "$scratch/source-b.json" \
  --destination-a "$scratch/destination-a.json" \
  --destination-b "$scratch/destination-b.json" \
  --output "$scratch/plan.json"
digest="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["plan_sha256"])' "$scratch/plan.json")"

for phase in preseed final; do
  "${bulkload[@]}" agent-stage \
    --phase "$phase" \
    --plan "$scratch/plan.json" \
    --accept-plan-sha256 "$digest" \
    --stage-root "$scratch/stage" \
    --allow-accounted-copy \
    --capacity-reserve-bytes 0 \
    --output "$scratch/$phase-receipt.json"
done

"${bulkload[@]}" agent-apply \
  --plan "$scratch/plan.json" \
  --stage-receipt "$scratch/final-receipt.json" \
  --accept-plan-sha256 "$digest" \
  --journal "$scratch/apply-journal.json" \
  --rollback-root "$scratch/rollback" \
  --capacity-reserve-bytes 0 \
  --output "$scratch/apply-receipt.json"

"${bulkload[@]}" agent-verify \
  --plan "$scratch/plan.json" \
  --stage-receipt "$scratch/final-receipt.json" \
  --apply-receipt "$scratch/apply-receipt.json" \
  --output "$scratch/verification.json"

python3 -c 'import json,sys; assert json.load(open(sys.argv[1]))["verified"]' "$scratch/verification.json"
printf 'bulkload demo: PASS\n'
