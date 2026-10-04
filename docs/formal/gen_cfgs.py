"""Generate docs/formal MC_*.cfg and configs.tsv from one table: python3 gen_cfgs.py docs/formal."""

import pathlib
import sys

OUT = pathlib.Path(sys.argv[1])

DEFAULTS = {
    "Seats": "{a, b}",
    "MaxRuns": "2",
    "MaxCrashes": "1",
    "MaxEdits": "1",
    "MaxForeign": "1",
    "MaxCommitFails": "1",
    "SpaceRefusals": "TRUE",
    "RelaxedSourceLedger": "FALSE",
    "RelaxedAuthority": "FALSE",
    "SupersedeMode": '"off"',
    "EstateReads": "FALSE",
    "MaxBackupSteps": "2",
    "Mutation": '"none"',
    "BudgetSeconds": "900",
}

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

# name, expect, comment lines, constant overrides, invariants, properties, spec, symmetry
CONFIGS = [
    # ---- positive: the code as it is today --------------------------------
    (
        "MC_main",
        "pass",
        [
            "Main config: the code as it is today (strict source ledger, no",
            "superseding publish). Two seats, a crash and a rerun, a source edit,",
            "a third-party write, a failed group commit and the space refusal,",
            "with every safety invariant.",
        ],
        {},
        SAFETY,
        [],
        "Spec",
        True,
    ),
    (
        "MC_deep",
        "pass",
        [
            "Depth: one seat through three runs and two crashes (either host or",
            "both), with an edit, a third-party write and a failed group commit.",
        ],
        {
            "Seats": "{a}",
            "MaxRuns": "3",
            "MaxCrashes": "2",
        },
        SAFETY,
        [],
        "Spec",
        False,
    ),
    # ---- WP0(g) / OI-1003-Q20 ----------------------------------------------
    (
        "MC_wp0g",
        "pass",
        [
            "WP0(g) / OI-1003-Q20: the source ledger's rows run synchronous=NORMAL,",
            "fullfsync=OFF, so a source power loss may drop any of them. The",
            "store-creation commit (schema and authority) stays durable. R25, S3",
            "and the durability ordering must still hold.",
        ],
        {
            "RelaxedSourceLedger": "TRUE",
            "MaxCrashes": "2",
            "MaxCommitFails": "0",
        },
        SAFETY,
        [],
        "Spec",
        True,
    ),
    (
        "MC_wp0g_deep",
        "pass",
        [
            "WP0(g), depth: one seat, three runs, two crashes, relaxed ledger rows,",
            "durable authority.",
        ],
        {
            "Seats": "{a}",
            "MaxRuns": "3",
            "MaxCrashes": "2",
            "RelaxedSourceLedger": "TRUE",
        },
        SAFETY,
        [],
        "Spec",
        False,
    ),
    (
        "MC_wp0g_authority",
        "fail",
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
        ["R25_NoDurableReread"],
        [],
        "Spec",
        False,
    ),
    # ---- WP0(d) candidate designs (no code yet) ----------------------------
    (
        "MC_wp0d_exchange",
        "pass",
        [
            "WP0(d) superseding publish, exchange design (no code yet):",
            "RENAME_EXCHANGE, then the displaced identity is checked against this",
            "store's rows and a foreign file is swapped back; recovery restores a",
            "displaced foreign file.",
        ],
        {
            "Seats": "{a}",
            "MaxRuns": "3",
            "MaxCommitFails": "0",
            "SpaceRefusals": "FALSE",
            "SupersedeMode": '"exchange"',
        },
        SAFETY,
        [],
        "Spec",
        False,
    ),
    (
        "MC_wp0d_check_rename",
        "fail",
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
        ["NoClobber"],
        [],
        "Spec",
        False,
    ),
    # ---- S2 ---------------------------------------------------------------
    (
        "MC_s2",
        "pass",
        [
            "S2 / WP0(b): typed source access, with estate capture's git reads and",
            "the SQLite backup's per-step shared read lock interleaved with a",
            "transfer that crashes and reruns.",
        ],
        {
            "Seats": "{a}",
            "MaxCommitFails": "0",
            "SpaceRefusals": "FALSE",
            "MaxForeign": "0",
            "EstateReads": "TRUE",
        },
        SAFETY,
        [],
        "Spec",
        False,
    ),
    # ---- liveness ---------------------------------------------------------
    (
        "MC_live",
        "pass",
        [
            "Liveness: no crash, a stable source, a fair protocol. Every started",
            "run reaches closure (every seat applied or typed-refused), and every",
            "run is made. No symmetry: it is unsound for liveness.",
        ],
        {
            "MaxCrashes": "0",
            "MaxEdits": "0",
            "MaxForeign": "0",
            "MaxCommitFails": "0",
            "BudgetSeconds": "600",
        },
        ["TypeOK", "ClosureAccounted"],
        ["RunsClose", "AllRunsFinish"],
        "LiveSpec",
        False,
    ),
    (
        "MC_neg_live_unfair",
        "fail",
        [
            "NEGATIVE: the same run without fairness may stop short of closure,",
            "so RunsClose depends on the fairness assumption.",
        ],
        dict(ONE),
        [],
        ["RunsClose"],
        "Spec",
        False,
    ),
]

# Mutations: (name, mutation, invariant, overrides, comment)
MUTATIONS = [
    ("held_before_commit", "held_before_commit", "HeldAfterCommit", {},
     "Held{true} is answered before the output's group commit."),
    ("commit_before_fsync", "commit_before_fsync", "RecordImpliesBytes", {},
     "the temporary is renamed and its row committed without the file seal."),
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
     {"MaxRuns": "2", "MaxCrashes": "1"},
     "WP0(g) counterfactual: Reuse also needs the SOURCE ledger's row. Fails\n"
     "\\* even with a strict ledger: the source row always trails the\n"
     "\\* destination commit by the Held round trip, so R25 must be carried by\n"
     "\\* the destination."),
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
     "a capture signals the source's writer to pause it."),
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

for name, mutation, invariant, over, comment in MUTATIONS:
    CONFIGS.append(
        (
            f"MC_neg_{name}",
            "fail",
            [f"NEGATIVE: {comment}", f"Mutation {mutation} must violate {invariant}."],
            {**ONE, "Mutation": f'"{mutation}"', **over},
            [invariant],
            [],
            "Spec",
            False,
        )
    )

rows = []
for name, expect, comment, over, invs, props, spec, sym in CONFIGS:
    constants = {**DEFAULTS, **over}
    width = max(len(k) for k in constants)
    lines = [f"\\* {line}" for line in comment]
    lines.append("\\* Expected outcome: docs/formal/configs.tsv; results: README.md.")
    lines.append(f"SPECIFICATION {spec}")
    lines.append("CONSTANTS")
    for key, value in constants.items():
        lines.append(f"    {key.ljust(width)} = {value}")
    if sym:
        lines.append("SYMMETRY SeatSymmetry")
    for inv in invs:
        lines.append(f"INVARIANT {inv}")
    lines.append("INVARIANT WithinBudget")
    for prop in props:
        lines.append(f"PROPERTY {prop}")
    (OUT / f"{name}.cfg").write_text("\n".join(lines) + "\n", encoding="utf-8")
    if expect == "pass":
        match = "-"
    elif props and not invs:
        match = "Temporal properties were violated"
    else:
        match = f"Invariant {invs[0]} is violated"
    rows.append(f"{name}\t{expect}\t{match}\t-")

header = [
    "# TLC configs checked by `just tla-check` (docs/formal/README.md).",
    "# Columns (tab-separated): config, expect (pass | fail), the TLC message a",
    "# failing config must print (- for pass), extra TLC flags (- for none).",
    "# A fail row names exactly one property; WithinBudget never counts.",
]
(OUT / "configs.tsv").write_text("\n".join(header + rows) + "\n", encoding="utf-8")
print(len(rows), "configs")
