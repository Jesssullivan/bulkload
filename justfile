set shell := ["bash", "-euo", "pipefail", "-c"]
export PYTHONDONTWRITEBYTECODE := "1"

root := justfile_directory()

_default:
    @just --list --unsorted

build:
    cd {{ root }} && just flywheel-build //:bulkload

test:
    cd {{ root }} && just flywheel-test //:tests

# Explicit source-only fallback for an unattached development shell. This is
# not GloriousFlywheel enrollment or CI evidence.
test-local:
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

workflow-lint:
    cd {{ root }} && actionlint .github/workflows/*.yml

secrets-scan-dir:
    cd {{ root }} && if command -v gitleaks >/dev/null 2>&1; then gitleaks dir --config .gitleaks.toml --redact .; else echo "gitleaks unavailable; skipped local scan"; fi

secrets-scan-history:
    cd {{ root }} && command -v gitleaks >/dev/null 2>&1
    cd {{ root }} && gitleaks git --config .gitleaks.toml --redact .

flake-check:
    cd {{ root }} && nix flake check --no-build --no-write-lock-file

# Source gates owned by this repository's development shell.
check-source: skill-validate repo-manifest-validate python-lint shell-lint workflow-lint secrets-scan-dir

# Normal attached gate: materialize the repo tools, then use the Flywheel
# wrapper for the Bazel graph.
check:
    cd {{ root }} && nix develop .#default --command just check-source
    cd {{ root }} && just test

# Source-only local gate. It may not be cited as cache or runner proof.
check-local: check-source test-local

# Toolchain-complete source gates for CI. The repo flake owns these linters and
# scanners; the pinned GloriousFlywheel shell owns the front door and Bazel.
ci-source: check-source secrets-scan-history

# CI is fail-closed on the fleet-managed GloriousFlywheel profile and drives
# every Bazel target through the canonical cache-backed wrapper.
ci:
    cd {{ root }} && just flywheel-verify
    cd {{ root }} && just flake-check
    cd {{ root }} && nix develop .#default --command just ci-source
    cd {{ root }} && just flywheel-build //:bulkload
    cd {{ root }} && just flywheel-test //:tests

demo:
    cd {{ root }} && scripts/demo.sh

install-skill:
    cd {{ root }} && scripts/install-skill.sh --all

import? "justfile.flywheel"
