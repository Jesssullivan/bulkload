#!/usr/bin/env bash
set -euo pipefail

python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 11) else 1)' || {
  printf 'install-skill: Python 3.11 or newer is required\n' >&2
  exit 1
}

original_arguments=("$@")

usage() {
  printf '%s\n' \
    'usage: install-skill.sh --all [--force]' \
    '       install-skill.sh --doctor'
}

mode=
force=false
while (($#)); do
  case "$1" in
    --all) mode=all ;;
    --doctor) mode=doctor ;;
    --force) force=true ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'install-skill: unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

if [[ -z "$mode" ]]; then
  usage >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
if [[ "$mode" == all && "${BULKLOAD_INSTALL_LOCKED:-}" != 1 ]]; then
  exec python3 "$repo_root/scripts/install_lock.py" "$0" "${original_arguments[@]}"
fi
source_skill="$repo_root/.agents/skills/bulkload"
canonical_parent="${HOME:?HOME must be set}/.agents/skills"
canonical="$canonical_parent/bulkload"
backup_parent="$HOME/.agents/backups/bulkload"
claude_parent="$HOME/.claude/skills"
claude_link="$claude_parent/bulkload"

if [[ "$mode" == doctor ]]; then
  rc=0
  if [[ -f "$canonical/SKILL.md" ]]; then
    printf 'PASS universal %s\n' "$canonical"
  else
    printf 'FAIL universal %s\n' "$canonical"
    rc=1
  fi
  if [[ -L "$claude_link" && "$(cd "$claude_link" 2>/dev/null && pwd -P)" == "$(cd "$canonical" 2>/dev/null && pwd -P)" ]]; then
    printf 'PASS claude %s -> %s\n' "$claude_link" "$canonical"
  else
    printf 'FAIL claude %s\n' "$claude_link"
    rc=1
  fi
  exit "$rc"
fi

[[ -f "$source_skill/SKILL.md" ]] || {
  printf 'install-skill: source skill missing: %s\n' "$source_skill" >&2
  exit 1
}
python3 "$repo_root/scripts/validate_skill.py" "$source_skill"

umask 077
relative_target='../../.agents/skills/bulkload'

require_real_directory_or_absent() {
  local path=$1
  if [[ -L "$path" ]]; then
    printf 'install-skill: refusing symlinked directory authority: %s\n' "$path" >&2
    exit 1
  fi
  if [[ -e "$path" && ! -d "$path" ]]; then
    printf 'install-skill: refusing non-directory authority: %s\n' "$path" >&2
    exit 1
  fi
}

for directory in \
  "$HOME" \
  "$HOME/.agents" \
  "$canonical_parent" \
  "$HOME/.agents/backups" \
  "$backup_parent" \
  "$HOME/.claude" \
  "$claude_parent"
do
  require_real_directory_or_absent "$directory"
done
[[ -d "$HOME" ]] || {
  printf 'install-skill: HOME must be an existing real directory\n' >&2
  exit 1
}

# Validate every known failure surface before changing the canonical skill.
if [[ -L "$claude_link" ]]; then
  if [[ "$(readlink "$claude_link")" != "$relative_target" ]]; then
    printf 'install-skill: refusing foreign Claude symlink: %s\n' "$claude_link" >&2
    exit 1
  fi
  claude_action='unchanged'
elif [[ -e "$claude_link" ]]; then
  printf 'install-skill: refusing existing Claude path: %s\n' "$claude_link" >&2
  exit 1
else
  claude_action='link'
fi

canonical_action=install
if [[ -e "$canonical" || -L "$canonical" ]]; then
  if [[ -d "$canonical" && ! -L "$canonical" ]] && diff -qr "$source_skill" "$canonical" >/dev/null; then
    canonical_action=unchanged
  elif [[ "$force" == true && -d "$canonical" && ! -L "$canonical" ]]; then
    canonical_action=replace
  else
    printf 'install-skill: refusing non-identical existing path: %s (use --force to preserve-and-replace a real directory)\n' "$canonical" >&2
    exit 1
  fi
fi

mkdir -p "$canonical_parent" "$claude_parent"
for directory in "$HOME/.agents" "$canonical_parent" "$HOME/.claude" "$claude_parent"; do
  require_real_directory_or_absent "$directory"
done
chmod 700 "$HOME/.agents" "$canonical_parent" "$HOME/.claude" "$claude_parent"

stage=
if [[ "$canonical_action" != unchanged ]]; then
  stage="$canonical_parent/.bulkload.stage.$$"
  [[ ! -e "$stage" && ! -L "$stage" ]] || {
    printf 'install-skill: refusing existing stage path: %s\n' "$stage" >&2
    exit 1
  }
  trap 'if [[ -n "${stage:-}" ]]; then rm -rf -- "$stage"; fi' EXIT
  mkdir "$stage"
  staged_skill="$stage/bulkload"
  cp -R "$source_skill" "$staged_skill"
  python3 "$repo_root/scripts/validate_skill.py" "$staged_skill"
fi

if [[ "$canonical_action" == replace ]]; then
  mkdir -p "$backup_parent"
  for directory in "$HOME/.agents/backups" "$backup_parent"; do
    require_real_directory_or_absent "$directory"
  done
  python3 - "$HOME/.agents/backups" "$backup_parent" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve(strict=True)
candidate = Path(sys.argv[2]).resolve(strict=True)
try:
    candidate.relative_to(root)
except ValueError as error:
    raise SystemExit("install-skill: backup path escapes private backup root") from error
PY
  chmod 700 "$HOME/.agents/backups" "$backup_parent"
  previous="$backup_parent/previous.$(date -u +%Y%m%dT%H%M%SZ).$$"
  [[ ! -e "$previous" && ! -L "$previous" ]] || {
    printf 'install-skill: refusing existing backup path: %s\n' "$previous" >&2
    exit 1
  }
  mv "$canonical" "$previous"
  if ! mv "$staged_skill" "$canonical"; then
    if ! mv "$previous" "$canonical"; then
      printf 'install-skill: replacement and rollback failed; preserved path: %s\n' "$previous" >&2
    fi
    exit 1
  fi
  rmdir "$stage"
  stage=
  chmod 700 "$previous"
  printf 'PRESERVED %s\n' "$previous"
  printf 'INSTALLED %s\n' "$canonical"
elif [[ "$canonical_action" == install ]]; then
  mv "$staged_skill" "$canonical"
  rmdir "$stage"
  stage=
  printf 'INSTALLED %s\n' "$canonical"
else
  printf 'UNCHANGED %s\n' "$canonical"
fi
trap - EXIT

if [[ "$claude_action" == link ]]; then
  ln -s "$relative_target" "$claude_link"
  printf 'LINKED %s -> %s\n' "$claude_link" "$relative_target"
else
  printf 'UNCHANGED %s\n' "$claude_link"
fi

printf '%s\n' \
  'Codex: ~/.agents/skills/bulkload' \
  'Pi: ~/.agents/skills/bulkload' \
  'Claude: ~/.claude/skills/bulkload -> ~/.agents/skills/bulkload'
