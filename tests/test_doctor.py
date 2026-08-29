"""Synthetic-tree coverage for the read-only `doctor` preflight.

Every case here reconstructs, in a temporary tree, one of the cross-kernel
defects the 2026-08-25..28 ceremony discovered only after bytes had moved.
No test contacts a real host: the peer probes run against a stub `ssh` that
answers from a script.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest

from bulkload_lib.cli import build_parser, main
from bulkload_lib.model import BulkloadError, canonical_bytes, sha256_bytes
from bulkload_lib.scanner import (
    DOCTOR_REPORT_SCHEMA,
    NON_POSIX_LOGIN_SHELLS,
    SSH_OPTIONS,
    canonical_path_map,
    path_identity,
    run_doctor,
)


STUB_SSH = """#!/bin/sh
# Stub peer: answers the doctor's probes without a network or a host.
while [ "$1" != "--" ]; do shift; done
shift
host="$1"
shift
case "$1" in
  */uname) echo "%(kernel)s" ;;
  */env) printf 'PATH=/usr/bin\\nSHELL=%(shell)s\\n' ;;
  printf) %(posix)s ;;
  */bulkload*) echo "bulkload %(version)s" ;;
  *) echo "unexpected probe: $*" >&2; exit 127 ;;
esac
exit 0
"""


def _stub_ssh(
    directory: Path,
    *,
    kernel: str = "Linux",
    shell: str = "/bin/bash",
    posix: bool = True,
    version: str = "0.2.0",
) -> str:
    script = directory / "ssh"
    script.write_text(
        STUB_SSH
        % {
            "kernel": kernel,
            "shell": shell,
            # A fish login shell cannot parse `${name-default}` at all, so the
            # stub refuses the command the way fish's parser does.
            "posix": (
                "echo bulkload-posix-ok"
                if posix
                else "echo 'fish: invalid variable expansion' >&2; exit 127"
            ),
            "version": version,
        }
    )
    script.chmod(0o755)
    return os.fspath(script)


class DoctorTreeCase(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.home = self.root / "home"
        self.git = self.home / "git"
        self.evidence = self.root / "evidence"
        for path in (self.home / ".codex", self.home / ".claude", self.git):
            path.mkdir(parents=True)
        self.evidence.mkdir(parents=True)

    def repository(self, name: str) -> Path:
        repository = self.git / name
        (repository / ".git" / "objects").mkdir(parents=True)
        (repository / ".git" / "refs").mkdir(parents=True)
        (repository / ".git" / "HEAD").write_text("ref: refs/heads/main\n")
        return repository

    def doctor(self, **overrides: object) -> dict[str, object]:
        arguments: dict[str, object] = {
            "role": "source",
            "home": self.home,
            "git_root": self.git,
            "codex_root": None,
            "claude_root": None,
            "pi_root": None,
            "seats": [],
            "path_map": canonical_path_map([(os.fspath(self.home), "/home/jess")]),
            "capture_output": self.evidence / "source-a.json",
            "snapshot_base_seal": None,
            "engine_version": "0.2.0",
        }
        arguments.update(overrides)
        return run_doctor(**arguments)  # type: ignore[arg-type]

    def check(self, report: dict[str, object], code: str) -> dict[str, object]:
        for item in report["checks"]:  # type: ignore[index]
            if item["code"] == code:
                return item
        raise AssertionError(f"doctor emitted no {code} check")


class ReportShapeTests(DoctorTreeCase):
    def test_clean_tree_passes_and_seals_its_report(self) -> None:
        report = self.doctor()
        self.assertEqual(report["schema"], DOCTOR_REPORT_SCHEMA)
        self.assertTrue(
            report["ok"],
            [item for item in report["checks"] if item["status"] == "fail"],
        )
        self.assertEqual(report["summary"]["fail"], 0)
        self.assertEqual(
            report["report_sha256"],
            sha256_bytes(
                canonical_bytes(
                    {
                        key: value
                        for key, value in report.items()
                        if key != "report_sha256"
                    }
                )
            ),
        )
        self.assertIsNone(report["peer"])
        self.assertEqual(
            report["runtime"]["presented_sha256"],
            report["runtime"]["measured_sha256"],
        )

    def test_report_is_canonical_json_and_every_check_is_typed(self) -> None:
        report = self.doctor()
        decoded = json.loads(canonical_bytes(report))
        for check in decoded["checks"]:
            self.assertEqual(
                sorted(check),
                [
                    "code",
                    "expected",
                    "finding_count",
                    "findings",
                    "observed",
                    "remedy",
                    "status",
                    "summary",
                    "truncated",
                ],
            )
            self.assertIn(check["status"], ("pass", "warn", "fail"))
            self.assertTrue(check["remedy"])

    def test_role_and_budgets_are_validated(self) -> None:
        with self.assertRaises(BulkloadError):
            self.doctor(role="peer")
        with self.assertRaises(BulkloadError):
            self.doctor(max_entries=0)

    def test_home_is_bound_but_never_walked(self) -> None:
        # Capture uses home to default the provider roots and to translate the
        # destination home (capture_agent_state root_bindings); it never walks
        # it, so neither does the doctor.
        (self.home / "unrelated").mkdir()
        (self.home / "unrelated" / "payload").write_text("x")
        report = self.doctor()
        walked = {
            root["label"]: root["walked"]
            for root in report["declared_roots"]  # type: ignore[index]
        }
        self.assertFalse(walked["home"])
        self.assertTrue(walked["git"])
        self.assertLess(report["scanned_entries"], 8)


class CaseFoldTests(DoctorTreeCase):
    def test_identity_folds_case_and_composition(self) -> None:
        self.assertEqual(path_identity("/A/B"), path_identity("/a/b"))
        # Decomposed 'e' + combining acute against the precomposed form: APFS
        # stores one, a linux filesystem the other, and they are one path.
        self.assertEqual(path_identity("/cafe\u0301"), path_identity("/caf\u00e9"))
        self.assertNotEqual(path_identity("/a/b"), path_identity("/a/c"))

    def test_two_roots_that_land_on_one_destination_identity_collide(self) -> None:
        # The GloriousFlywheel class, expressed portably: two disjoint source
        # subtrees whose *destination* spellings differ only by case. On a
        # folding source they are two files; on a byte-exact destination they
        # are one, and the second silently overwrites the first.
        seat = self.root / "still-a"
        seat.mkdir(parents=True)
        (seat / "history.db").write_text("x")
        other = self.root / "still-b"
        other.mkdir(parents=True)
        (other / "history.db").write_text("y")
        report = self.doctor(
            seats=[("first", seat, "directory"), ("second", other, "directory")],
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(seat), "/home/jess/state/Atuin"),
                    (os.fspath(other), "/home/jess/state/atuin"),
                ]
            ),
        )
        collision = self.check(report, "case-fold-collision")
        self.assertEqual(collision["status"], "fail")
        named = {path for group in collision["findings"] for path in group["paths"]}
        self.assertIn(os.fspath(seat / "history.db"), named)
        self.assertIn(os.fspath(other / "history.db"), named)
        self.assertFalse(report["ok"])

    def test_the_walk_budget_is_reported_not_hidden(self) -> None:
        repository = self.repository("one")
        for index in range(12):
            (repository / f"file-{index}").write_text("x")
        report = self.doctor(max_entries=4)
        self.assertFalse(report["complete"])
        self.assertEqual(self.check(report, "scan-complete")["status"], "warn")
        self.assertIn("still walking", self.check(report, "scan-complete")["observed"])


class GitPointerTests(DoctorTreeCase):
    def test_worktree_pointer_spelled_differently_from_the_directory(self) -> None:
        repository = self.repository("repo")
        worktree = self.git / "Repo.worktrees"
        worktree.mkdir()
        (worktree / ".git").write_text(f"gitdir: {repository}/.git/worktrees/w1\n")
        registration = repository / ".git" / "worktrees" / "w1"
        registration.mkdir(parents=True)
        # The recorded spelling is the one `git worktree add` was given; the
        # directory holds another. Only a listing can tell them apart.
        (registration / "gitdir").write_text(f"{self.git}/repo.worktrees/.git\n")
        report = self.doctor()
        check = self.check(report, "git-pointer-spelling")
        if not (self.git / "repo.worktrees").exists():
            # A byte-exact filesystem cannot express this defect at all: the
            # pointer simply dangles, which the sibling check reports.
            self.assertEqual(check["status"], "pass")
            self.assertEqual(self.check(report, "git-pointer-target")["status"], "warn")
            return
        self.assertEqual(check["status"], "fail")
        finding = check["findings"][0]
        self.assertEqual(finding["kind"], "worktree-gitdir")
        self.assertEqual(finding["observed"], os.fspath(worktree / ".git"))
        self.assertEqual(
            finding["recorded"], os.fspath(self.git / "repo.worktrees" / ".git")
        )
        self.assertFalse(report["ok"])

    def test_dangling_alternates_pointer_is_named(self) -> None:
        repository = self.repository("repo")
        info = repository / ".git" / "objects" / "info"
        info.mkdir(parents=True)
        (info / "alternates").write_text(f"{self.git}/absent/.git/objects\n")
        check = self.check(self.doctor(), "git-pointer-target")
        self.assertEqual(check["status"], "warn")
        self.assertEqual(check["findings"][0]["kind"], "alternates")

    def test_intact_pointers_pass(self) -> None:
        repository = self.repository("repo")
        worktree = self.git / "repo.worktrees"
        worktree.mkdir()
        registration = repository / ".git" / "worktrees" / "w1"
        registration.mkdir(parents=True)
        (worktree / ".git").write_text(f"gitdir: {registration}\n")
        (registration / "gitdir").write_text(f"{worktree}/.git\n")
        report = self.doctor()
        self.assertEqual(self.check(report, "git-pointer-spelling")["status"], "pass")
        self.assertEqual(self.check(report, "git-pointer-target")["status"], "pass")


class PathMapTests(DoctorTreeCase):
    def test_an_unmapped_seat_is_named_before_capture_reads_it(self) -> None:
        # STATUS 2026-08-26T11:19:16Z: the seat-still maps were missing and
        # capture reached seat translation for the first time hours in.
        seat = self.root / "still" / "atuin"
        seat.mkdir(parents=True)
        report = self.doctor(seats=[("atuin", seat, "directory")])
        check = self.check(report, "path-map-coverage")
        self.assertEqual(check["status"], "fail")
        self.assertEqual([item["label"] for item in check["findings"]], ["seat:atuin"])
        self.assertEqual(check["findings"][0]["path"], os.fspath(seat))

    def test_the_snapshot_root_is_not_required_to_translate(self) -> None:
        # Capture rewrites every snapshot path back to its live spelling
        # (_rewrite_catalog_to_live), so demanding a map entry for it would be
        # a refusal the engine does not make.
        report = self.doctor()
        self.assertEqual(self.check(report, "path-map-coverage")["status"], "pass")
        snapshot = next(
            root
            for root in report["declared_roots"]  # type: ignore[index]
            if root["label"] == "snapshot"
        )
        self.assertIsNone(snapshot["destination"])

    def test_two_maps_that_alias_one_destination_fail(self) -> None:
        report = self.doctor(
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(self.root / "a"), "/srv/shared"),
                    (os.fspath(self.root / "b"), "/srv/shared"),
                ]
            )
        )
        check = self.check(report, "path-map-alias")
        self.assertEqual(check["status"], "fail")
        self.assertEqual(
            check["findings"][0]["reason"],
            "two path-map sources share one destination",
        )

    def test_nested_sources_may_reparent(self) -> None:
        # The ceremony's own map: /Users/jess -> /home/jess while
        # /Users/jess/git -> /srv/fast-local/jess/git. Longest prefix wins.
        report = self.doctor(
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(self.git), "/srv/fast-local/jess/git"),
                ]
            )
        )
        self.assertEqual(self.check(report, "path-map-alias")["status"], "pass")
        self.assertEqual(self.check(report, "path-map-coverage")["status"], "pass")

    def test_destination_role_translates_to_itself(self) -> None:
        seat = self.root / "unmapped-seat"
        seat.mkdir()
        report = self.doctor(role="destination", seats=[("seat", seat, "directory")])
        self.assertEqual(self.check(report, "path-map-coverage")["status"], "pass")


class SeatShapeTests(DoctorTreeCase):
    def test_a_file_seat_naming_a_directory_is_refused_up_front(self) -> None:
        directory = self.home / ".claude" / "projects"
        directory.mkdir(parents=True)
        report = self.doctor(
            seats=[("projects", directory, "file")],
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(directory), "/home/jess/projects"),
                ]
            ),
        )
        check = self.check(report, "seat-path-shape")
        self.assertEqual(check["status"], "fail")
        self.assertEqual(
            check["findings"][0]["reason"],
            "declared file seat is a directory; use --seat",
        )

    def test_a_directory_seat_naming_a_file_is_refused_up_front(self) -> None:
        singleton = self.home / ".claude.json"
        singleton.write_text("{}")
        report = self.doctor(
            seats=[("claudejson", singleton, "directory")],
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(singleton), "/home/jess/.claude.json"),
                ]
            ),
        )
        check = self.check(report, "seat-path-shape")
        self.assertEqual(check["status"], "fail")
        self.assertIn("use --file-seat", check["findings"][0]["reason"])

    def test_a_well_shaped_file_seat_passes_and_is_not_walked(self) -> None:
        singleton = self.home / ".claude.json"
        singleton.write_text("{}")
        report = self.doctor(
            seats=[("claudejson", singleton, "file")],
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(singleton), "/home/jess/.claude.json"),
                ]
            ),
        )
        self.assertEqual(self.check(report, "seat-path-shape")["status"], "pass")
        self.assertTrue(report["ok"])

    def test_a_symlinked_file_seat_is_named(self) -> None:
        backing = self.root / "state" / "claude.json"
        backing.parent.mkdir(parents=True)
        backing.write_text("{}")
        link = self.home / ".claude.json"
        link.symlink_to(backing)
        report = self.doctor(
            seats=[("claudejson", link, "file")],
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(link), "/home/jess/.claude.json"),
                ]
            ),
        )
        check = self.check(report, "seat-path-shape")
        self.assertEqual(check["status"], "fail")
        self.assertIn("symlink", check["findings"][0]["reason"])

    def test_duplicate_and_malformed_seat_names_are_named(self) -> None:
        first = self.root / "one"
        second = self.root / "two"
        first.mkdir()
        second.mkdir()
        report = self.doctor(
            seats=[("Seat", first, "directory"), ("Seat", second, "directory")],
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(first), "/home/jess/one"),
                    (os.fspath(second), "/home/jess/two"),
                ]
            ),
        )
        reasons = [
            item["reason"] for item in self.check(report, "seat-path-shape")["findings"]
        ]
        self.assertIn("seat name is not a legal identifier", reasons)
        self.assertTrue(any("seat name repeats" in reason for reason in reasons))

    def test_a_seat_inside_a_walked_root_is_named(self) -> None:
        seat = self.home / ".codex" / "sessions"
        seat.mkdir(parents=True)
        report = self.doctor(
            seats=[("sessions", seat, "directory")],
            path_map=canonical_path_map(
                [
                    (os.fspath(self.home), "/home/jess"),
                    (os.fspath(seat), "/home/jess/sessions"),
                ]
            ),
        )
        check = self.check(report, "seat-double-carry")
        self.assertEqual(check["status"], "warn")
        self.assertEqual(
            check["findings"][0]["reason"],
            "seat is inside declared root provider:codex",
        )
        # A double carry is a judgement call, not a refusal.
        self.assertTrue(report["ok"])


class SqliteSidecarTests(DoctorTreeCase):
    def test_an_orphan_sidecar_in_a_typed_root_fails(self) -> None:
        (self.home / ".codex" / "history.db-wal").write_text("debris")
        check = self.check(self.doctor(), "sqlite-sidecar-orphan")
        self.assertEqual(check["status"], "fail")
        self.assertEqual(check["findings"][0]["kind"], "wal")
        self.assertTrue(check["findings"][0]["typed"])
        self.assertEqual(
            check["findings"][0]["missing_primary"],
            os.fspath(self.home / ".codex" / "history.db"),
        )

    def test_a_live_database_with_its_sidecars_is_clean(self) -> None:
        for suffix in ("", "-wal", "-shm"):
            (self.home / ".codex" / f"history.db{suffix}").write_text("x")
        self.assertEqual(
            self.check(self.doctor(), "sqlite-sidecar-orphan")["status"], "pass"
        )

    def test_a_journal_suffixed_source_file_only_warns(self) -> None:
        # STATUS 2026-08-25T13:40:08Z swept a -journal class that included
        # three ordinary nixpkgs source files. Outside a typed root that is a
        # warning, not a refusal.
        repository = self.repository("nixpkgs")
        (repository / "sql-journal").write_text("not a sidecar")
        check = self.check(self.doctor(), "sqlite-sidecar-orphan")
        self.assertEqual(check["status"], "warn")
        self.assertFalse(check["findings"][0]["typed"])


class DeclaredRootTests(DoctorTreeCase):
    def test_a_missing_git_root_is_named(self) -> None:
        report = self.doctor(git_root=self.root / "absent")
        check = self.check(report, "declared-root-presence")
        self.assertEqual(check["status"], "fail")
        self.assertEqual(check["findings"][0]["label"], "git")

    def test_evidence_inside_a_live_root_is_named(self) -> None:
        report = self.doctor(capture_output=self.git / "evidence" / "source-a.json")
        check = self.check(report, "declared-root-presence")
        self.assertEqual(check["status"], "fail")
        self.assertIn("overlaps live root", check["findings"][0]["reason"])

    def test_an_absent_provider_root_is_not_a_defect(self) -> None:
        report = self.doctor(pi_root=self.root / "absent-pi")
        self.assertEqual(self.check(report, "declared-root-presence")["status"], "pass")


class RuntimeParityTests(DoctorTreeCase):
    def _with_environment(self, **values: str | None) -> None:
        for key, value in values.items():
            previous = os.environ.get(key)
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
            self.addCleanup(
                lambda key=key, previous=previous: (
                    os.environ.__setitem__(key, previous)
                    if previous is not None
                    else os.environ.pop(key, None)
                )
            )

    def test_a_peer_digest_that_differs_fails(self) -> None:
        report = self.doctor(peer_runtime_source_sha256="a" * 64)
        check = self.check(report, "runtime-source-parity")
        self.assertEqual(check["status"], "fail")
        self.assertEqual(check["findings"][0]["observed"], "a" * 64)
        self.assertFalse(report["ok"])

    def test_a_matching_peer_digest_passes(self) -> None:
        measured = self.doctor()["runtime"]["measured_sha256"]
        report = self.doctor(peer_runtime_source_sha256=measured)
        self.assertEqual(self.check(report, "runtime-source-parity")["status"], "pass")

    def test_an_active_pin_that_masks_the_closure_warns(self) -> None:
        self._with_environment(
            BULKLOAD_RUNTIME_SOURCE_PIN="b" * 64,
            BULKLOAD_RUNTIME_SOURCE_SHA256="b" * 64,
        )
        report = self.doctor()
        check = self.check(report, "runtime-source-parity")
        self.assertEqual(check["status"], "warn")
        self.assertEqual(report["runtime"]["pin"], "active")
        self.assertEqual(report["runtime"]["presented_sha256"], "b" * 64)
        self.assertNotEqual(
            report["runtime"]["measured_sha256"], report["runtime"]["presented_sha256"]
        )
        self.assertTrue(report["ok"])

    def test_a_malformed_pin_fails_instead_of_raising(self) -> None:
        self._with_environment(
            BULKLOAD_RUNTIME_SOURCE_PIN="not-a-digest",
            BULKLOAD_RUNTIME_SOURCE_SHA256="not-a-digest",
        )
        report = self.doctor()
        check = self.check(report, "runtime-source-parity")
        self.assertEqual(check["status"], "fail")
        self.assertEqual(report["runtime"]["pin"], "malformed")
        self.assertEqual(
            report["runtime"]["presented_sha256"],
            report["runtime"]["measured_sha256"],
        )


class PeerProbeTests(DoctorTreeCase):
    def test_a_linux_peer_is_read_without_writing_to_it(self) -> None:
        report = self.doctor(
            peer_ssh_host="jess@sting",
            ssh_path=_stub_ssh(self.root, kernel="Linux"),
        )
        self.assertEqual(report["peer"]["platform"], "linux")
        self.assertEqual(report["peer"]["path_identity"], "byte-exact")
        self.assertTrue(report["peer"]["reachable"])
        self.assertEqual(self.check(report, "peer-reachable")["status"], "pass")
        self.assertEqual(self.check(report, "peer-login-shell")["status"], "pass")

    def test_a_fish_login_shell_warns_and_suggests_bash(self) -> None:
        report = self.doctor(
            peer_ssh_host="jess@sting",
            ssh_path=_stub_ssh(
                self.root, shell="/run/current-system/sw/bin/fish", posix=False
            ),
        )
        check = self.check(report, "peer-login-shell")
        self.assertEqual(check["status"], "warn")
        self.assertIn("fish", check["findings"][0]["login_shell"])
        self.assertFalse(check["findings"][0]["posix_expansion"])
        self.assertIn("bash -s", check["remedy"])
        # A warning names a hazard; it does not refuse the ceremony.
        self.assertTrue(report["ok"])
        self.assertIn("fish", NON_POSIX_LOGIN_SHELLS)

    def test_a_darwin_source_against_a_linux_peer_reports_both_kernels(self) -> None:
        link = self.git / "link"
        link.symlink_to(self.home)
        report = self.doctor(
            peer_ssh_host="sting", ssh_path=_stub_ssh(self.root, kernel="Linux")
        )
        symlink = self.check(report, "symlink-mode-portability")
        identity = self.check(report, "path-identity-pair")
        if sys.platform == "darwin":
            mode = f"{stat.S_IMODE(link.lstat().st_mode):04o}"
            if mode != "0777":
                self.assertEqual(symlink["status"], "warn")
                self.assertEqual(symlink["findings"][0]["path"], os.fspath(link))
                self.assertIn("validate_snapshot_custody", symlink["remedy"])
            self.assertEqual(identity["status"], "warn")
            self.assertEqual(identity["observed"]["peer"], "byte-exact")
        # Warnings never flip the exit status on their own.
        self.assertEqual(report["summary"]["fail"], 0)

    def test_a_same_kernel_pair_compares_no_modes(self) -> None:
        report = self.doctor(
            peer_ssh_host="sting",
            ssh_path=_stub_ssh(self.root, kernel=sys.platform.title()),
        )
        check = self.check(report, "symlink-mode-portability")
        self.assertEqual(check["status"], "pass")
        self.assertIn("no peer kernel to compare", check["observed"])

    def test_a_peer_engine_of_another_version_fails(self) -> None:
        report = self.doctor(
            peer_ssh_host="sting",
            peer_bulkload="/opt/bulkload/scripts/bulkload.py",
            ssh_path=_stub_ssh(self.root, version="0.1.0"),
        )
        check = self.check(report, "peer-engine-version")
        self.assertEqual(check["status"], "fail")
        self.assertEqual(check["observed"], "bulkload 0.1.0")
        self.assertEqual(check["expected"], "bulkload 0.2.0")

    def test_a_matching_peer_engine_passes(self) -> None:
        report = self.doctor(
            peer_ssh_host="sting",
            peer_bulkload="/opt/bulkload/scripts/bulkload.py",
            ssh_path=_stub_ssh(self.root),
        )
        self.assertEqual(self.check(report, "peer-engine-version")["status"], "pass")

    def test_an_unreachable_peer_fails_without_stopping_the_report(self) -> None:
        script = self.root / "ssh"
        script.write_text("#!/bin/sh\necho 'Permission denied' >&2\nexit 255\n")
        script.chmod(0o755)
        report = self.doctor(peer_ssh_host="sting", ssh_path=os.fspath(script))
        self.assertEqual(self.check(report, "peer-reachable")["status"], "fail")
        self.assertFalse(report["peer"]["reachable"])
        self.assertEqual(self.check(report, "case-fold-collision")["status"], "pass")

    def test_the_probe_uses_the_transport_option_vector(self) -> None:
        recorder = self.root / "ssh"
        log = self.root / "argv.log"
        recorder.write_text(
            f'#!/bin/sh\nprintf "%s\\n" "$*" >> {log}\necho Linux\nexit 0\n'
        )
        recorder.chmod(0o755)
        self.doctor(peer_ssh_host="sting", ssh_path=os.fspath(recorder))
        recorded = log.read_text().splitlines()
        self.assertTrue(recorded)
        for line in recorded:
            for option in SSH_OPTIONS:
                self.assertIn(option, line)
            self.assertIn(" -- sting ", line)

    def test_a_malformed_peer_authority_is_refused(self) -> None:
        with self.assertRaises(BulkloadError):
            self.doctor(peer_ssh_host="sting; rm -rf /")


class CommandSurfaceTests(DoctorTreeCase):
    def test_every_doctor_flag_documents_itself(self) -> None:
        parser = build_parser()
        subparsers = next(
            action
            for action in parser._actions
            if isinstance(action, __import__("argparse")._SubParsersAction)
        )
        doctor = subparsers.choices["doctor"]
        bare = [
            action.option_strings
            for action in doctor._actions
            if action.option_strings and not action.help
        ]
        self.assertEqual(bare, [])

    def test_a_failing_preflight_writes_the_report_and_exits_one(self) -> None:
        (self.home / ".codex" / "history.db-wal").write_text("debris")
        output = self.evidence / "doctor.json"
        code = main(
            [
                "doctor",
                "--role",
                "source",
                "--home",
                os.fspath(self.home),
                "--git-root",
                os.fspath(self.git),
                "--path-map",
                f"{self.home}=/home/jess",
                "--capture-output",
                os.fspath(self.evidence / "source-a.json"),
                "--output",
                os.fspath(output),
            ]
        )
        self.assertEqual(code, 1)
        report = json.loads(output.read_text())
        self.assertFalse(report["ok"])
        self.assertEqual(report["schema"], DOCTOR_REPORT_SCHEMA)
        self.assertEqual(
            [check["code"] for check in report["checks"] if check["status"] == "fail"],
            ["sqlite-sidecar-orphan"],
        )
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_a_clean_preflight_exits_zero(self) -> None:
        output = self.evidence / "doctor.json"
        code = main(
            [
                "doctor",
                "--role",
                "source",
                "--home",
                os.fspath(self.home),
                "--git-root",
                os.fspath(self.git),
                "--path-map",
                f"{self.home}=/home/jess",
                "--capture-output",
                os.fspath(self.evidence / "source-a.json"),
                "--output",
                os.fspath(output),
            ]
        )
        self.assertEqual(code, 0)
        self.assertTrue(json.loads(output.read_text())["ok"])

    def test_the_report_may_not_be_written_inside_a_live_root(self) -> None:
        code = main(
            [
                "doctor",
                "--role",
                "source",
                "--home",
                os.fspath(self.home),
                "--git-root",
                os.fspath(self.git),
                "--path-map",
                f"{self.home}=/home/jess",
                "--capture-output",
                os.fspath(self.evidence / "source-a.json"),
                "--output",
                os.fspath(self.git / "doctor.json"),
            ]
        )
        self.assertEqual(code, 1)
        self.assertFalse((self.git / "doctor.json").exists())


if __name__ == "__main__":
    unittest.main()
