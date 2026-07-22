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
mkdir -p "$canonical_parent" "$claude_parent"
chmod 700 "$HOME/.agents" "$canonical_parent" "$HOME/.claude" "$claude_parent" 2>/dev/null || true

if [[ -e "$canonical" || -L "$canonical" ]]; then
  if [[ -d "$canonical" && ! -L "$canonical" ]] && diff -qr "$source_skill" "$canonical" >/dev/null; then
    printf 'UNCHANGED %s\n' "$canonical"
  elif [[ "$force" == true && -d "$canonical" && ! -L "$canonical" ]]; then
    previous="$canonical.previous.$(date -u +%Y%m%dT%H%M%SZ)"
    mv "$canonical" "$previous"
    printf 'PRESERVED %s\n' "$previous"
  else
    printf 'install-skill: refusing non-identical existing path: %s (use --force to preserve-and-replace a real directory)\n' "$canonical" >&2
    exit 1
  fi
fi

if [[ ! -e "$canonical" && ! -L "$canonical" ]]; then
  stage="$canonical_parent/.bulkload.stage.$$"
  trap 'rm -rf "$stage"' EXIT
  rm -rf "$stage"
  cp -R "$source_skill" "$stage"
  [[ -f "$stage/SKILL.md" ]]
  mv "$stage" "$canonical"
  trap - EXIT
  printf 'INSTALLED %s\n' "$canonical"
fi

relative_target='../../.agents/skills/bulkload'
if [[ -L "$claude_link" ]]; then
  if [[ "$(readlink "$claude_link")" != "$relative_target" ]]; then
    printf 'install-skill: refusing foreign Claude symlink: %s\n' "$claude_link" >&2
    exit 1
  fi
  printf 'UNCHANGED %s\n' "$claude_link"
elif [[ -e "$claude_link" ]]; then
  printf 'install-skill: refusing existing Claude path: %s\n' "$claude_link" >&2
  exit 1
else
  ln -s "$relative_target" "$claude_link"
  printf 'LINKED %s -> %s\n' "$claude_link" "$relative_target"
fi

printf '%s\n' \
  'Codex: ~/.agents/skills/bulkload' \
  'Pi: ~/.agents/skills/bulkload' \
  'Claude: ~/.claude/skills/bulkload -> ~/.agents/skills/bulkload'
