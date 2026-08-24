from __future__ import annotations

import ast
import os
from pathlib import Path
import re
import sys
import unittest

sys.dont_write_bytecode = True

SKILL_ROOT = ".agents/skills/bulkload"
CEREMONY_PATH = f"{SKILL_ROOT}/references/ceremony.md"
SKILL_PATH = f"{SKILL_ROOT}/SKILL.md"
VALIDATOR_PATH = "scripts/validate_skill.py"
EXPECTED_VERB_COUNT = 7
BANNED_WORDS = (
    "ensure",
    "leverage",
    "utilize",
    "simply",
    "seamless",
    "robust",
    "gracefully",
)
OPERATOR_OWNED = r"\blaunchctl\s+bootout\b"
FORBIDDEN_PROCESS_CONTROL = (
    r"\bkill\b",
    r"\bkillall\b",
    r"\bpkill\b",
    r"\bkillpg\b",
    r"\bos\.kill\b",
    r"\bsend_signal\b",
    r"\bSIGSTOP\b",
    r"\bSIGCONT\b",
    r"\bSIGTERM\b",
    r"\bSIGKILL\b",
    r"\bSIGHUP\b",
    r"\blaunchctl\s+kill\b",
    OPERATOR_OWNED,
)
OPERATOR_TOKEN = re.compile(r"operator", re.IGNORECASE)
APPLIES_TO = re.compile(r"^Applies to: bulkload (\S+)", re.MULTILINE)
MODULE_BLOCK = re.compile(r"^module\((.*?)^\)", re.MULTILINE | re.DOTALL)
MODULE_VERSION = re.compile(r'^\s*version = "([^"]+)"', re.MULTILINE)
HEADING = re.compile(r"^(#+)\s")
LIST_ITEM = re.compile(r"^\s*(?:[-*+]|\d+[.)]|>)\s")
FENCE = re.compile(r"^\s*```")
VERB_TOKEN = re.compile(r"agent-[a-z][a-z0-9-]*")
LINK = re.compile(r"\[[^]]+\]\(([^)]+)\)")


class CeremonyContractError(ValueError):
    pass


def find_workspace() -> Path:
    candidates = [Path.cwd(), Path(__file__).resolve()]
    runfiles = os.environ.get("RUNFILES_DIR")
    if runfiles:
        candidates.extend([Path(runfiles) / "_main", Path(runfiles) / "bulkload"])
    for candidate in candidates:
        for parent in [candidate, *candidate.parents]:
            if (parent / "MODULE.bazel").is_file() and (parent / "README.md").is_file():
                return parent
    raise AssertionError("cannot locate bulkload runfiles workspace")


def in_runfiles() -> bool:
    return bool(os.environ.get("RUNFILES_DIR") or os.environ.get("TEST_SRCDIR"))


def public_commands(validator: str) -> set[str]:
    tree = ast.parse(validator)
    for node in ast.walk(tree):
        if not isinstance(node, ast.Assign):
            continue
        names = {target.id for target in node.targets if isinstance(target, ast.Name)}
        if "PUBLIC_COMMANDS" not in names:
            continue
        try:
            return set(ast.literal_eval(node.value))
        except ValueError as error:
            raise CeremonyContractError(
                "the public command surface must be a literal set"
            ) from error
    raise CeremonyContractError("the validator declares no public command surface")


def command_snippets(text: str) -> list[str]:
    """Return every span a reader would read as a command, not as prose."""
    snippets: list[str] = []
    fenced = False
    for line in text.splitlines():
        if FENCE.match(line):
            fenced = not fenced
            continue
        if fenced:
            snippets.append(line.strip())
            continue
        parts = line.split("`")
        snippets.extend(part.strip() for part in parts[1 : len(parts) - 1 : 2])
    return snippets


def named_verbs(text: str) -> set[str]:
    """Collect every public verb a ceremony document names in command position."""
    verbs: set[str] = set()
    for snippet in command_snippets(text):
        words = snippet.split()
        if not words:
            continue
        candidates = [words[0]]
        candidates.extend(
            words[index + 1]
            for index, word in enumerate(words[:-1])
            if word == "bulkload"
        )
        for candidate in candidates:
            token = candidate.strip(".,;:()[]")
            if VERB_TOKEN.fullmatch(token):
                verbs.add(token)
    return verbs


def ceremony_sections(readme: str) -> str:
    collected: list[str] = []
    depth: int | None = None
    for line in readme.splitlines():
        heading = HEADING.match(line)
        if heading:
            level = len(heading.group(1))
            if depth is not None and level <= depth:
                depth = None
            if depth is None and "ceremony" in line.lower():
                depth = level
                continue
        if depth is not None:
            collected.append(line)
    return "\n".join(collected)


def logical_lines(text: str) -> list[tuple[int, str]]:
    """Join each Markdown step with its wrapped continuation lines.

    A ceremony step is the unit an operator reads. Markdown wraps one step over
    several source lines, so the step, not the source line, is the scan unit.
    """
    blocks: list[tuple[int, str]] = []
    number_of_block = 0
    block: list[str] = []
    for number, raw in enumerate(text.splitlines(), start=1):
        line = raw.rstrip()
        continues = (
            bool(line.strip())
            and line.startswith((" ", "\t"))
            and LIST_ITEM.match(line) is None
        )
        if continues:
            block.append(line.strip())
            continue
        if block:
            blocks.append((number_of_block, " ".join(block)))
            block = []
        if not line.strip():
            continue
        number_of_block = number
        block = [line.strip()]
    if block:
        blocks.append((number_of_block, " ".join(block)))
    return blocks


def process_control_violations(text: str) -> list[str]:
    """Report every process-control verb a ceremony document instructs.

    One exception stands. The operator owns `launchctl bootout`, so the step
    that names it must also name the operator who runs it.
    """
    violations: list[str] = []
    for number, step in logical_lines(text):
        for pattern in FORBIDDEN_PROCESS_CONTROL:
            if not re.search(pattern, step):
                continue
            if pattern == OPERATOR_OWNED and OPERATOR_TOKEN.search(step):
                continue
            violations.append(f"line {number}: {pattern}")
    return violations


def banned_words(text: str) -> list[str]:
    found: list[str] = []
    for word in BANNED_WORDS:
        if re.search(rf"\b{word}[a-z]*\b", text, re.IGNORECASE):
            found.append(word)
    return found


def module_version(module: str) -> str:
    block = MODULE_BLOCK.search(module)
    if block is None:
        raise CeremonyContractError("MODULE.bazel declares no module block")
    version = MODULE_VERSION.search(block.group(1))
    if version is None:
        raise CeremonyContractError("MODULE.bazel declares no module version")
    return version.group(1)


class CeremonyContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = find_workspace()
        cls.ceremony_path = cls.root / CEREMONY_PATH
        cls.ceremony = cls.ceremony_path.read_text(encoding="utf-8")
        cls.readme = (cls.root / "README.md").read_text(encoding="utf-8")
        cls.skill = (cls.root / SKILL_PATH).read_text(encoding="utf-8")
        cls.validator = (cls.root / VALIDATOR_PATH).read_text(encoding="utf-8")
        cls.module = (cls.root / "MODULE.bazel").read_text(encoding="utf-8")

    def test_ceremony_reference_is_an_exact_file_the_skill_links(self) -> None:
        self.assertTrue(self.ceremony_path.is_file())
        if not in_runfiles():
            self.assertFalse(self.ceremony_path.is_symlink())
        targets = {
            match.group(1).split("#", 1)[0] for match in LINK.finditer(self.skill)
        }
        self.assertIn("references/ceremony.md", targets)

    def test_every_named_verb_is_a_public_command(self) -> None:
        commands = public_commands(self.validator)
        self.assertEqual(len(commands), EXPECTED_VERB_COUNT)
        sources = (("README.md", self.readme), (CEREMONY_PATH, self.ceremony))
        for source, text in sources:
            unknown = sorted(named_verbs(text) - commands)
            self.assertEqual(unknown, [], f"{source} names an unsupported verb")

    def test_ceremony_header_pins_the_module_version(self) -> None:
        header = APPLIES_TO.search(self.ceremony)
        self.assertIsNotNone(header, "the ceremony must declare what it applies to")
        self.assertEqual(header.group(1), module_version(self.module))

    def test_ceremony_documents_instruct_no_unowned_process_control(self) -> None:
        self.assertEqual(process_control_violations(self.ceremony), [])
        readme_ceremony = ceremony_sections(self.readme)
        self.assertNotEqual(readme_ceremony.strip(), "", "README owns the ceremony")
        self.assertEqual(process_control_violations(readme_ceremony), [])

    def test_ceremony_reference_uses_the_controlled_vocabulary(self) -> None:
        self.assertEqual(banned_words(self.ceremony), [])


if __name__ == "__main__":
    unittest.main()
