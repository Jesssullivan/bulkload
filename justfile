set shell := ["bash", "-euo", "pipefail", "-c"]
export PYTHONDONTWRITEBYTECODE := "1"

root := justfile_directory()

_default:
    @just --list --unsorted

build:
    cd {{ root }} && bazelisk build //:bulkload

test:
    cd {{ root }} && bazelisk test //:tests

skill-validate:
    cd {{ root }} && python3 scripts/validate_skill.py --self-test .agents/skills/bulkload

repo-manifest-validate:
    cd {{ root }} && python3 scripts/validate_repo_manifest.py tinyland.repo.json

python-lint:
    cd {{ root }} && ruff check .agents/skills/bulkload/scripts scripts tests
    cd {{ root }} && ruff format --check .agents/skills/bulkload/scripts scripts tests

shell-lint:
    cd {{ root }} && shellcheck scripts/install-skill.sh scripts/demo.sh

secrets-scan-dir:
    cd {{ root }} && if command -v gitleaks >/dev/null 2>&1; then gitleaks dir --config .gitleaks.toml --redact .; else echo "gitleaks unavailable; skipped local scan"; fi

check: skill-validate repo-manifest-validate python-lint shell-lint test secrets-scan-dir

demo:
    cd {{ root }} && scripts/demo.sh

install-skill:
    cd {{ root }} && scripts/install-skill.sh --all
