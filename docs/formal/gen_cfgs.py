"""Render docs/formal's MC_*.cfg files and configs.tsv from one table.

Usage: python3 docs/formal/gen_cfgs.py docs/formal

The rendered files are committed; edit this table, never the outputs. Every
MC_*.cfg in the output directory is deleted and re-rendered, so a config
dropped from the table leaves no stale file behind. `just tla-check` runs
every row of configs.tsv in order (docs/formal/README.md). Sprint 2's Dhall
catalogue replaces this script (OI-1003-Q32).
"""

import pathlib
import sys

OUT = pathlib.Path(sys.argv[1])

DEFAULTS = {
    "Seats": "{a, b}",
    "MaxRuns": "2",
    "MaxCrashes": "1",
    "MaxEdits": "1",
    "MaxForeign": "0",
    "MaxCommitFails": "0",
    "SpaceRefusals": "FALSE",
    "RelaxedSourceLedger": "FALSE",
    "RelaxedAuthority": "FALSE",
    "SupersedeMode": '"off"',
    "EstateReads": "FALSE",
    "MaxBackupSteps": "2",
    "Mutation": '"none"',
    "BudgetSeconds": "600",
}

# Every safety invariant. The names are frozen (README.md, "Frozen names").
SAFETY = [
    "TypeOK",
    "R25_NoDurableReread",
    "R25_NoCommittedCaptureReread",
    "ReadOnce",
    "S3_ReadsOnlyChanged",
    "S3_UnchangedReadsZero",
    "S3_ClosedPassIsHeld",
    "RecordImpliesBytes",
    "HeldAfterCommit",
    "LedgerAfterHeld",
    "DoneAfterLedger",
    "ReuseSound",
    "LedgerSound",
    "NoClobber",
    "S2_TypedSourceAccess",
    "S2_BackupLockBounded",
    "ClosureAccounted",
]

# Every fault on: a third-party write or delete, a failed group commit (full
# disk), and the space refusal.
FAULTS = {"MaxForeign": "1", "MaxCommitFails": "1", "SpaceRefusals": "TRUE"}

# MC_main: two seats, two runs, one crash of either host or both, one source
# edit per seat, destination faults off.
MAIN = {"Seats": "{a, b}", "MaxRuns": "2", "MaxCrashes": "1", "MaxEdits": "1"}

# One seat, one run, no faults: the base of most mutation configs.
ONE = {
    "Seats": "{a}",
    "MaxRuns": "1",
    "MaxCrashes": "0",
    "MaxEdits": "0",
    "MaxForeign": "0",
    "MaxCommitFails": "0",
    "SpaceRefusals": "FALSE",
    "BudgetSeconds": "300",
}

# The constants first drafted for MC_main: two seats, three runs, every
# fault. Far too large to model-check (README.md, "The budget is
# state-level"); used only for the seeded simulation.
DRAFTED = {**MAIN, "MaxRuns": "3", **FAULTS}

# The N-version core (OI-1003-Q32): the shared bound for TLC and sprint 2's
# Haskell BFS explorer. One seat, three runs, two crashes, one source edit;
# third-party writes, commit failures, the space refusal, superseding
# publish and estate reads off.
NV_CORE = {
    "Seats": "{a}",
    "MaxRuns": "3",
    "MaxCrashes": "2",
    "MaxEdits": "1",
    "MaxForeign": "0",
    "MaxCommitFails": "0",
    "SpaceRefusals": "FALSE",
    "SupersedeMode": '"off"',
    "EstateReads": "FALSE",
}


def config(name, expect, prop, comment, over, invs, props=(), spec="Spec",
           sym=False, flags="-"):
    return {
        "name": name,
        "expect": expect,
        "prop": prop,
        "comment": comment,
        "over": over,
        "invs": list(invs),
        "props": list(props),
        "spec": spec,
        "sym": sym,
        "flags": flags,
    }


CONFIGS = [
    # ---- the budget's own proof: always the first row ----------------------
    config(
        "MC_budget_selftest",
        "inconclusive",
        "WithinBudget",
        [
            "BUDGET SELF-TEST (expected INCONCLUSIVE). MC_main's constants with",
            "a 5 s budget: the search needs far longer, so WithinBudget must",
            "trip, which proves the budget is evaluated per state. The bound is",
            "MC_main's, so a broken budget still ends (as a PASS, which",
            "tla-check rejects before it runs any other config).",
        ],
        {**MAIN, "BudgetSeconds": "5"},
        ["TypeOK"],
        sym=True,
    ),
    # ---- positive: the code as it is today ---------------------------------
    config(
        "MC_main",
        "pass",
        "all",
        [
            "Main config, breadth: the code as it is today (strict source ledger,",
            "no superseding publish). Two seats, two runs, one crash of either",
            "host or both, one source edit per seat; destination faults off.",
        ],
        MAIN,
        SAFETY,
        sym=True,
    ),
    config(
        "MC_main_deep",
        "pass",
        "all",
        [
            "Main config, depth: one seat through three runs and two crashes,",
            "with a source edit and every destination fault: a third-party",
            "write or delete, a failed group commit and the space refusal.",
        ],
        {"Seats": "{a}", "MaxRuns": "3", "MaxCrashes": "2", **FAULTS},
        SAFETY,
    ),
    config(
        "MC_dest_faults",
        "pass",
        "all",
        [
            "Destination faults on two seats: a third-party write or delete, a",
            "failed group commit (full disk, #100) and the space refusal. No",
            "crash or source edit here; MC_main_deep combines them on one seat.",
        ],
        {"MaxCrashes": "0", "MaxEdits": "0", **FAULTS},
        SAFETY,
        sym=True,
    ),
    config(
        "MC_nv_core",
        "pass",
        "all",
        [
            "N-version core (OI-1003-Q32): the shared bound for TLC and sprint",
            "2's Haskell BFS explorer. One seat, three runs, two crashes, one",
            "source edit; third-party writes, commit failures, the space",
            "refusal, superseding publish and estate reads off. No SYMMETRY, so",
            "its distinct-state count (README.md) is the plain state graph's.",
        ],
        NV_CORE,
        SAFETY,
    ),
    # ---- WP0(g) / OI-1003-Q20 ----------------------------------------------
    config(
        "MC_wp0g",
        "pass",
        "all",
        [
            "WP0(g) / OI-1003-Q20, breadth: the source ledger's row commits run",
            "synchronous=NORMAL, fullfsync=OFF, so a source power loss may drop",
            "any subset of them. The store-creation commit (schema and",
            "authority) stays durable. Every safety invariant must still hold.",
        ],
        {**MAIN, "RelaxedSourceLedger": "TRUE"},
        SAFETY,
        sym=True,
    ),
    config(
        "MC_wp0g_deep",
        "pass",
        "all",
        [
            "WP0(g), depth: one seat, three runs, two crashes, every destination",
            "fault, relaxed ledger rows, durable store creation.",
        ],
        {
            "Seats": "{a}",
            "MaxRuns": "3",
            "MaxCrashes": "2",
            "RelaxedSourceLedger": "TRUE",
            **FAULTS,
        },
        SAFETY,
    ),
    # ---- WP0(d) candidate designs (no code yet) -----------------------------
    config(
        "MC_wp0d_exchange",
        "pass",
        "all",
        [
            "WP0(d) superseding publish, exchange design (no code yet):",
            "RENAME_EXCHANGE, then the displaced identity is checked against this",
            "store's rows and a foreign file is swapped back; recovery restores a",
            "displaced foreign file. One seat, three runs, one crash, one edit,",
            "one third-party write.",
        ],
        {
            "Seats": "{a}",
            "MaxRuns": "3",
            "MaxForeign": "1",
            "SupersedeMode": '"exchange"',
        },
        SAFETY,
    ),
    # ---- S2 -----------------------------------------------------------------
    config(
        "MC_s2",
        "pass",
        "all",
        [
            "S2 / WP0(b): typed source access, with estate capture's git read and",
            "the SQLite backup's per-step shared read lock interleaved with a",
            "transfer that crashes and reruns. One seat, two runs.",
        ],
        {"Seats": "{a}", "EstateReads": "TRUE"},
        SAFETY,
    ),
    # ---- liveness (never under SYMMETRY: unsound for liveness) --------------
    config(
        "MC_live",
        "pass",
        "all",
        [
            "Liveness under WF_vars(Protocol): no crash, a stable source. Every",
            "started run reaches closure (every seat applied or typed-refused),",
            "and every run is made. The space refusal stays on (a typed refusal",
            "closes an item). No SYMMETRY: it is unsound for liveness.",
        ],
        {"MaxCrashes": "0", "MaxEdits": "0", "SpaceRefusals": "TRUE"},
        ["TypeOK", "ClosureAccounted"],
        props=["RunsClose", "AllRunsFinish"],
        spec="LiveSpec",
    ),
    # ---- the drafted constants: simulation only, never model-checked --------
    config(
        "MC_main_sim",
        "simulate",
        "all",
        [
            "SIMULATION ONLY, never a model-checking result: the constants first",
            "drafted for MC_main (two seats, three runs, one crash, every fault).",
            "Random behaviours of bounded depth from a fixed seed, every",
            "invariant. No SYMMETRY (simulation does not use it).",
        ],
        DRAFTED,
        SAFETY,
        flags="-simulate num=3000 -depth 120 -seed 20261003",
    ),
    # ---- design findings: expected to fail ---------------------------------
    config(
        "MC_wp0g_authority",
        "fail",
        "R25_NoDurableReread",
        [
            "WP0(g) FINDING (expected to fail): relax the whole source store,",
            "including the commit that creates its authority. A source power loss",
            "before that commit reaches disk loses the authority; the next run keys",
            "every row anew, finds no destination row, and re-reads bytes the",
            "destination holds durably. R25 then fails.",
        ],
        {
            **ONE,
            "MaxRuns": "2",
            "MaxCrashes": "1",
            "RelaxedSourceLedger": "TRUE",
            "RelaxedAuthority": "TRUE",
        },
        ["TypeOK", "R25_NoDurableReread"],
    ),
    config(
        "MC_wp0d_check_rename",
        "fail",
        "NoClobber",
        [
            "WP0(d) FINDING (expected to fail): the naive design, check the",
            "output's identity and then rename over it, has a window in which a",
            "third-party write lands and is clobbered.",
        ],
        {
            **ONE,
            "MaxRuns": "2",
            "MaxEdits": "1",
            "MaxForeign": "1",
            "SupersedeMode": '"check_rename"',
        },
        ["TypeOK", "NoClobber"],
    ),
    config(
        "MC_neg_live_unfair",
        "fail",
        "RunsClose",
        [
            "NEGATIVE: the same run without fairness may stop short of closure,",
            "so RunsClose depends on the fairness assumption. No SYMMETRY.",
        ],
        ONE,
        ["TypeOK"],
        props=["RunsClose"],
    ),
]

# Overrides that put a one-seat mutation config on the N-version core.
NV_MUT = {k: v for k, v in NV_CORE.items() if ONE.get(k, DEFAULTS.get(k)) != v}
CORE_NOTE = "\n\\* On the N-version core (OI-1003-Q32)."

# Mutations: (config suffix, mutation, the one property it must violate,
# overrides on ONE, comment).
MUTATIONS = [
    ("held_before_commit", "held_before_commit", "HeldAfterCommit", NV_MUT,
     "Held{true} is answered before the output's group commit." + CORE_NOTE),
    ("commit_before_fsync", "commit_before_fsync", "RecordImpliesBytes", NV_MUT,
     "the temporary is renamed and its row committed without the file seal."
     + CORE_NOTE),
    ("commit_before_dirseal", "commit_before_dirseal", "RecordImpliesBytes", {},
     "the row commits before the directory seal makes the rename durable."),
    ("adopt_without_seal", "adopt_without_seal", "RecordImpliesBytes",
     {"MaxForeign": "1"},
     "an adopted existing output is recorded without sealing it."),
    ("ledger_before_held", "ledger_before_held", "LedgerAfterHeld", {},
     "the source ledger records a capture before Held{true}."),
    ("done_before_sync", "done_before_sync", "DoneAfterLedger", {},
     "SourceDone is sent before the ledger's last commit returned."),
    ("reread_durable", "reread_durable", "R25_NoDurableReread",
     {"MaxRuns": "2", "MaxCrashes": "1"},
     "the destination never answers Reuse, so a rerun re-reads held bytes."),
    ("reread_unchanged", "reread_durable", "S3_UnchangedReadsZero",
     {"MaxRuns": "2", "MaxCrashes": "1"},
     "as reread_durable, against S3's unchanged-source clause. It needs\n"
     "\\* the crash: with the source row present, the ledger manifest and an\n"
     "\\* adopt still read nothing."),
    ("reread_changed_only", "reread_durable", "S3_ReadsOnlyChanged",
     {"MaxRuns": "2", "MaxCrashes": "1"},
     "as reread_durable, against S3's changed-seats-only inequality."),
    ("skip_output_row", "skip_output_row", "S3_ClosedPassIsHeld", {},
     "the store commit records no output row, so a closed pass holds nothing."),
    ("double_read", "double_read", "ReadOnce", {},
     "a fresh manifest's chunks are read again to serve them (#77 F1)."),
    ("src_ledger_carries_r25", "src_ledger_carries_r25", "R25_NoDurableReread",
     NV_MUT,
     "WP0(g) counterfactual: Reuse also needs the SOURCE ledger's row. Fails\n"
     "\\* even with a strict ledger: the source row always trails the\n"
     "\\* destination commit by the Held round trip, so R25 must be carried by\n"
     "\\* the destination." + CORE_NOTE),
    ("record_racy", "record_racy", "ReuseSound", {"MaxEdits": "1"},
     "a racy capture is recorded as a reuse key (#86)."),
    ("record_racy_ledger", "record_racy", "LedgerSound", {"MaxEdits": "1"},
     "as record_racy, against the source ledger's manifest."),
    ("untyped_space", "untyped_space", "ClosureAccounted",
     {"MaxCommitFails": "1"},
     "a full-disk group surfaces as a bare IO, which closes nothing (#100)."),
    ("source_write", "source_write", "S2_TypedSourceAccess", {},
     "a capture also writes the source."),
    ("pause_writer", "pause_writer", "S2_TypedSourceAccess", {},
     "a capture interrupts the source's writer to pause it."),
    ("git_optional_locks", "git_optional_locks", "S2_TypedSourceAccess",
     {"EstateReads": "TRUE"},
     "a git read runs without the optional-locks guard."),
    ("unbounded_backup", "unbounded_backup", "S2_BackupLockBounded",
     {"EstateReads": "TRUE"},
     "the SQLite backup takes its lock past max_steps."),
    ("supersede_unchecked", "supersede_unchecked", "NoClobber",
     {"MaxForeign": "1", "SupersedeMode": '"exchange"'},
     "WP0(d) exchange without the identity check."),
    ("sweep_displaced", "sweep_displaced", "NoClobber",
     {"MaxRuns": "3", "MaxCrashes": "1", "MaxEdits": "1", "MaxForeign": "1",
      "SupersedeMode": '"exchange"'},
     "WP0(d) exchange whose recovery sweeps a displaced foreign file like a\n"
     "\\* temporary."),
]

for suffix, mutation, invariant, over, comment in MUTATIONS:
    CONFIGS.append(
        config(
            f"MC_neg_{suffix}",
            "fail",
            invariant,
            [f"NEGATIVE: {comment}", f"Mutation {mutation} must violate {invariant}."],
            {**ONE, "Mutation": f'"{mutation}"', **over},
            ["TypeOK", invariant],
        )
    )

EXPECTS = {"pass", "fail", "simulate", "inconclusive"}
names = [c["name"] for c in CONFIGS]
assert len(names) == len(set(names)), "config names must be unique"
assert CONFIGS[0]["expect"] == "inconclusive", "the budget self-test runs first"

for stale in OUT.glob("MC_*.cfg"):
    stale.unlink()

rows = []
for c in CONFIGS:
    assert c["expect"] in EXPECTS, c["name"]
    assert "WithinBudget" not in c["invs"], "every config gets WithinBudget once"
    if c["props"]:
        assert not c["sym"], f"{c['name']}: SYMMETRY is unsound for liveness"
    named = [p for p in c["invs"] + c["props"] if p != "TypeOK"]
    if c["expect"] == "fail":
        # TypeOK rides along so a type error shows up as a wrong outcome; the
        # one other property is the one the config must violate.
        assert named == [c["prop"]], f"{c['name']} must name exactly its property"
    elif c["expect"] == "inconclusive":
        assert c["prop"] == "WithinBudget" and not named, c["name"]
    else:
        assert c["prop"] == "all", c["name"]
    constants = {**DEFAULTS, **c["over"]}
    width = max(len(k) for k in constants)
    lines = [f"\\* {line}" for line in c["comment"]]
    lines.append("\\* Rendered by gen_cfgs.py; expected outcome in configs.tsv.")
    lines.append(f"SPECIFICATION {c['spec']}")
    lines.append("CONSTANTS")
    for key, value in constants.items():
        lines.append(f"    {key.ljust(width)} = {value}")
    if c["sym"]:
        lines.append("SYMMETRY SeatSymmetry")
    for inv in c["invs"]:
        lines.append(f"INVARIANT {inv}")
    lines.append("INVARIANT WithinBudget")
    for prop in c["props"]:
        lines.append(f"PROPERTY {prop}")
    (OUT / f"{c['name']}.cfg").write_text("\n".join(lines) + "\n", encoding="utf-8")
    rows.append("\t".join([c["name"], c["expect"], c["prop"], c["flags"]]))

header = [
    "# TLC configs checked by `just tla-check`, in this order (README.md).",
    "# Rendered by gen_cfgs.py; edit its table, not this file.",
    "# Tab-separated columns:",
    "#   name            the config, MC_<name>.cfg",
    "#   expect          pass | fail | simulate | inconclusive",
    "#   named-property  fail: the one property it must violate;",
    "#                   inconclusive: WithinBudget; otherwise all",
    "#   flags           extra TLC flags, one argv element per",
    "#                   space-separated word (- for none)",
    "name\texpect\tnamed-property\tflags",
]
(OUT / "configs.tsv").write_text("\n".join(header + rows) + "\n", encoding="utf-8")
print(len(rows), "configs")
