# Codex private auth install

This runbook covers policy v6's only private-state mutation: an attended,
typed replacement of an existing destination `CODEX_HOME/auth.json`.

It does not authorize SQLite union, composition, installation, combined
auth-plus-SQLite apply, Codex activation, deployment, or TCFS runtime work. A
v5 SQLite close/action evidence and a v6 compose request or capacity
observation are separate evidence and grant none of those authorities.

## Exact boundary

Read `codex-private-state-policy.v6.json` before proceeding. Retain any
accepted v4 SQLite opening as immutable legacy evidence; do not reinterpret it
as an install plan or consume it in this auth workflow.

- Preferred inputs: source selects `auth`; destination selects `auth` and
  `sqlite`.
- Also accepted: both source and destination select `auth` and `sqlite`.
- Source SQLite is never consumed in either input shape.
- Destination SQLite is independently captured and preserved byte-exact with
  zero mutations.
- Every install plan must keep `sqlite_union_ready=false`; mutating receipts
  must report zero SQLite mutations.
- An auth-only destination is unsupported. The installer requires existing
  destination auth for complete rollback and destination SQLite evidence for
  the preservation fence.
- Any selected SQLite root containing a live `-wal`, `-shm`, or `-journal`
  companion hard-stops immutable capture. Do not delete, checkpoint, or copy a
  sidecar through Bulkload. Either arrange an attended clean provider shutdown
  outside this workflow or stop.
- The quiescence attestation is an operator procedural claim with
  `provider_writer_proof=false`. Its nonblocking directory `flock` coordinates
  cooperating Bulkload processes only. It does not stop Codex or prove that a
  provider writer is absent.
- Apply, verify, rollback, and recovery refuse root. Keep the operator in
  control of the provider shutdown for each complete operation.
- Offline receipts prove bytes, custody, and destination SQLite preservation.
  They deliberately record `provider_runtime_acceptance_verified=false`.
  A fresh attended provider turn is required before claiming working auth.

Use the reviewed `scripts/bulkload.py` entrypoint or its Bazel-built
equivalent. The entrypoint pins policy v6 and the complete Python runtime
source inventory before importing command code. Compatibility and install
plans bind that runtime authority; every later private operation revalidates
it. Do not import and call the implementation modules directly.

## Custody and path setup

Before the first command:

1. Set `umask 077`.
2. Stop source and destination Codex writers by an operator-reviewed method.
3. Resolve the effective destination SQLite authority from configured
   `sqlite_home`, then `CODEX_SQLITE_HOME`, then `CODEX_HOME`. Pass that
   absolute path explicitly; Bulkload does not infer it.
4. Assign distinct canonical host-authority UUIDs to independent source and
   destination filesystem namespaces.
5. Use the same exact Codex version string in every related artifact.
6. Create private `0700` evidence parents outside both live Codex/SQLite roots.
7. Choose absent, pairwise non-overlapping paths for every bundle, plan,
   journal, backup, capture, attestation, and receipt.
8. Never print or inspect credential values. Review only paths, digests, sizes,
   identities, modes, schemas, and claim fields.

Attestations are short-lived (default 300 seconds; allowed range 30–900).
Create a new attestation immediately before the command it authorizes. Review
its exact SHA-256 and pass that digest back explicitly.

The examples below use uppercase digest placeholders. Replace each placeholder
only after reviewing the corresponding immutable artifact.

## 1. Capture source auth only

Create the procedural capture attestation:

```bash
python3 scripts/bulkload.py codex-private-quiescence-attest \
  --codex-home /absolute/source/codex-home \
  --output /secure/evidence/source-capture-quiescence.json \
  --operation-output /secure/evidence/source-private \
  --purpose capture \
  --capture-role source \
  --host-authority-id 11111111-1111-4111-8111-111111111111 \
  --codex-version 0.145.0 \
  --include-auth \
  --acknowledge-writers-quiesced
```

Then capture with the reviewed attestation digest:

```bash
python3 scripts/bulkload.py codex-private-capture \
  --codex-home /absolute/source/codex-home \
  --output-directory /secure/evidence/source-private \
  --role source \
  --host-authority-id 11111111-1111-4111-8111-111111111111 \
  --codex-version 0.145.0 \
  --include-auth \
  --acknowledge-private-capture \
  --quiescence-attestation /secure/evidence/source-capture-quiescence.json \
  --accept-quiescence-attestation SOURCE_CAPTURE_ATTESTATION_SHA256
```

This preferred source capture avoids reading or making claims about source
SQLite. If the operator instead selects source SQLite, pass the exact effective
source `--sqlite-home`, include `--include-sqlite` in both commands, and accept
that any source sidecar blocks capture. The later installer still ignores
those source SQLite artifacts.

## 2. Capture destination auth and SQLite

First prove that the operator has stopped provider writers and that the
resolved SQLite root has no live sidecars. Do not remove sidecars to satisfy
the check.

Create a destination capture attestation:

```bash
python3 scripts/bulkload.py codex-private-quiescence-attest \
  --codex-home /absolute/destination/codex-home \
  --sqlite-home /absolute/destination/sqlite-home \
  --output /secure/evidence/destination-capture-quiescence.json \
  --operation-output /secure/evidence/destination-private \
  --purpose capture \
  --capture-role destination \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --include-auth \
  --include-sqlite \
  --acknowledge-writers-quiesced
```

Capture the complete destination state set:

```bash
python3 scripts/bulkload.py codex-private-capture \
  --codex-home /absolute/destination/codex-home \
  --sqlite-home /absolute/destination/sqlite-home \
  --output-directory /secure/evidence/destination-private \
  --role destination \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --include-auth \
  --include-sqlite \
  --acknowledge-private-capture \
  --quiescence-attestation /secure/evidence/destination-capture-quiescence.json \
  --accept-quiescence-attestation DEST_CAPTURE_ATTESTATION_SHA256
```

Capture enumerates every top-level `*.sqlite` family, opens the source
immutably, uses SQLite's backup API for the evidence copy, normalizes that copy
to `DELETE` journal mode, runs `quick_check`, repeats discovery, and binds the
complete live namespace. It never copies WAL/SHM/journal companions.

## 3. Compile compatibility and install plans

Create the compatibility dossier:

```bash
python3 scripts/bulkload.py codex-private-plan \
  --source-bundle /secure/evidence/source-private \
  --destination-bundle /secure/evidence/destination-private \
  --output /secure/evidence/private-compatibility.json
```

Exit `4` is expected because the compatibility dossier is never itself
actionable. Review its exact digest, input captures, runtime authority,
versions, selected classes, blockers, and SQLite findings.

Compile the separately accepted narrow install plan:

```bash
python3 scripts/bulkload.py codex-private-install-plan \
  --compatibility-plan /secure/evidence/private-compatibility.json \
  --source-bundle /secure/evidence/source-private \
  --destination-bundle /secure/evidence/destination-private \
  --accept-compatibility-plan COMPATIBILITY_PLAN_SHA256 \
  --output /secure/evidence/private-install-plan.json
```

Do not proceed unless all of these hold:

- `ready_for_apply=true` and `blockers=[]`;
- input classes are source `["auth"]`, destination `["auth","sqlite"]`, or
  full/full;
- selected classes equal the destination classes;
- auth action is `replace-existing-after-fresh-full-backup` or the explicit
  no-mutation `preserve-identical`;
- every SQLite action is `preserve-destination-exact` with `mutates=false`;
- `sqlite_union_ready=false`;
- the destination binding exactly names the intended host authority, Codex
  home, SQLite home, and version; and
- the runtime authority exactly matches the reviewed compatibility dossier.

Review and retain the install plan's exact `plan_sha256`.

## 4. Apply the accepted auth plan

Keep the destination writer stopped. Create a fresh apply attestation bound to
the final apply receipt path and accepted install plan:

```bash
python3 scripts/bulkload.py codex-private-quiescence-attest \
  --codex-home /absolute/destination/codex-home \
  --sqlite-home /absolute/destination/sqlite-home \
  --output /secure/evidence/apply-quiescence.json \
  --operation-output /secure/evidence/apply-receipt.json \
  --purpose apply \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --include-auth \
  --include-sqlite \
  --accept-plan INSTALL_PLAN_SHA256 \
  --acknowledge-writers-quiesced
```

Apply:

```bash
python3 scripts/bulkload.py codex-private-apply \
  --install-plan /secure/evidence/private-install-plan.json \
  --compatibility-plan /secure/evidence/private-compatibility.json \
  --source-bundle /secure/evidence/source-private \
  --destination-before-bundle /secure/evidence/destination-private \
  --destination-codex-home /absolute/destination/codex-home \
  --destination-sqlite-home /absolute/destination/sqlite-home \
  --rollback-directory /secure/evidence/auth-rollback \
  --post-capture-directory /secure/evidence/destination-post-apply \
  --recovery-capture-directory /secure/evidence/apply-failure-recovery \
  --journal /secure/evidence/apply-journal.jsonl \
  --receipt /secure/evidence/apply-receipt.json \
  --accept-plan INSTALL_PLAN_SHA256 \
  --destination-host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --quiescence-attestation /secure/evidence/apply-quiescence.json \
  --accept-quiescence-attestation APPLY_ATTESTATION_SHA256 \
  --acknowledge-private-apply
```

The operation revalidates both plans, both captures, the pinned runtime, live
destination roots, auth preconditions, and the complete SQLite namespace. It
creates a durable rollback copy before same-directory auth replacement,
journals each transition, captures destination state again, and requires every
SQLite family to remain exact. A successful receipt must report zero SQLite
mutations, offline verification true, and provider runtime acceptance false.

If apply exits nonzero after creating a journal, do not rerun apply and do not
delete staging, backup, capture, or journal artifacts. Use recovery below.

## 5. Verify independently

Keep the writer stopped and create a new verify attestation. It must use a
different attestation ID and bind both the plan and apply receipt:

```bash
python3 scripts/bulkload.py codex-private-quiescence-attest \
  --codex-home /absolute/destination/codex-home \
  --sqlite-home /absolute/destination/sqlite-home \
  --output /secure/evidence/verify-quiescence.json \
  --operation-output /secure/evidence/verify-receipt.json \
  --purpose verify \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --include-auth \
  --include-sqlite \
  --accept-plan INSTALL_PLAN_SHA256 \
  --accept-apply-receipt APPLY_RECEIPT_SHA256 \
  --acknowledge-writers-quiesced
```

```bash
python3 scripts/bulkload.py codex-private-verify \
  --install-plan /secure/evidence/private-install-plan.json \
  --compatibility-plan /secure/evidence/private-compatibility.json \
  --source-bundle /secure/evidence/source-private \
  --destination-before-bundle /secure/evidence/destination-private \
  --apply-receipt /secure/evidence/apply-receipt.json \
  --destination-codex-home /absolute/destination/codex-home \
  --destination-sqlite-home /absolute/destination/sqlite-home \
  --capture-directory /secure/evidence/destination-verify \
  --receipt /secure/evidence/verify-receipt.json \
  --accept-plan INSTALL_PLAN_SHA256 \
  --accept-apply-receipt APPLY_RECEIPT_SHA256 \
  --destination-host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --quiescence-attestation /secure/evidence/verify-quiescence.json \
  --accept-quiescence-attestation VERIFY_ATTESTATION_SHA256 \
  --acknowledge-private-verify
```

Only after this fresh offline proof may the operator restart Codex and perform
one fresh attended provider turn. Successful model/API activity is the
provider acceptance proof; the Bulkload receipt is not.

If the provider turn fails, stop and diagnose. Once Codex has been restarted,
do not assert that there were no post-apply writes and do not use the manual
rollback path below unless an independent proof establishes that assertion.

## 6. Manual rollback before provider writes

Manual rollback is available only with an exact apply receipt and an honest
operator assertion that no provider write occurred after apply. Keep writers
stopped and create a fresh rollback attestation bound to the plan, apply
receipt, and rollback receipt path:

```bash
python3 scripts/bulkload.py codex-private-quiescence-attest \
  --codex-home /absolute/destination/codex-home \
  --sqlite-home /absolute/destination/sqlite-home \
  --output /secure/evidence/rollback-quiescence.json \
  --operation-output /secure/evidence/rollback-receipt.json \
  --purpose rollback \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --include-auth \
  --include-sqlite \
  --accept-plan INSTALL_PLAN_SHA256 \
  --accept-apply-receipt APPLY_RECEIPT_SHA256 \
  --acknowledge-writers-quiesced
```

Run rollback with absent preflight, post-capture, recovery-capture, journal,
and receipt outputs:

```bash
python3 scripts/bulkload.py codex-private-rollback \
  --install-plan /secure/evidence/private-install-plan.json \
  --compatibility-plan /secure/evidence/private-compatibility.json \
  --source-bundle /secure/evidence/source-private \
  --destination-before-bundle /secure/evidence/destination-private \
  --apply-receipt /secure/evidence/apply-receipt.json \
  --rollback-directory /secure/evidence/auth-rollback \
  --destination-codex-home /absolute/destination/codex-home \
  --destination-sqlite-home /absolute/destination/sqlite-home \
  --preflight-capture-directory /secure/evidence/rollback-preflight \
  --post-capture-directory /secure/evidence/rollback-post \
  --recovery-capture-directory /secure/evidence/rollback-failure-recovery \
  --journal /secure/evidence/rollback-journal.jsonl \
  --receipt /secure/evidence/rollback-receipt.json \
  --accept-plan INSTALL_PLAN_SHA256 \
  --accept-apply-receipt APPLY_RECEIPT_SHA256 \
  --destination-host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --quiescence-attestation /secure/evidence/rollback-quiescence.json \
  --accept-quiescence-attestation ROLLBACK_ATTESTATION_SHA256 \
  --acknowledge-private-rollback \
  --acknowledge-no-post-apply-writes
```

Rollback independently captures the current destination, restores the exact
backed-up auth, preserves SQLite with zero mutations, captures again, and
emits an offline receipt. It still does not prove provider runtime acceptance.

## 7. Recover an interrupted mutation

Recovery is for a durable apply or rollback journal whose terminal receipt was
not safely published. Do not edit or truncate the journal manually.

1. Keep provider writers stopped.
2. Review and accept the journal file's exact SHA-256.
3. Classify the journal from its validated first event. An apply journal starts
   with `operation="apply"` and `event="prepared"`; a rollback journal starts
   with `operation="rollback"` and `event="rollback-prepared"`. If that
   classification is unavailable or ambiguous, stop. Do not guess from which
   receipt files happen to exist.
4. Select exactly one argument matrix:
   - **Apply journal:** omit both `--apply-receipt` and
     `--accept-apply-receipt`. This remains mandatory even if the interrupted
     apply's terminal receipt exists; recovery discovers and validates that
     receipt only at the original path bound into the apply journal.
   - **Rollback journal:** require `--apply-receipt` to name the original
     successful apply receipt that the rollback was attempting to invert, and
     require its exact digest through `--accept-apply-receipt`. The rollback
     journal binds both values. Never substitute an interrupted rollback
     receipt.
5. Create a fresh `recover` attestation bound to the install plan, accepted
   journal digest, the apply-receipt digest only for a rollback journal, and
   the recovery receipt path.
6. Run `codex-private-recover` with absent preflight/post-capture/receipt
   outputs and the same exact authorities.

### Recover an apply journal

The apply-journal form never accepts apply-receipt arguments:

```bash
python3 scripts/bulkload.py codex-private-quiescence-attest \
  --codex-home /absolute/destination/codex-home \
  --sqlite-home /absolute/destination/sqlite-home \
  --output /secure/evidence/recovery-quiescence.json \
  --operation-output /secure/evidence/recovery-receipt.json \
  --purpose recover \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --include-auth \
  --include-sqlite \
  --accept-plan INSTALL_PLAN_SHA256 \
  --accept-journal APPLY_JOURNAL_SHA256 \
  --acknowledge-writers-quiesced
```

```bash
python3 scripts/bulkload.py codex-private-recover \
  --install-plan /secure/evidence/private-install-plan.json \
  --compatibility-plan /secure/evidence/private-compatibility.json \
  --source-bundle /secure/evidence/source-private \
  --destination-before-bundle /secure/evidence/destination-private \
  --journal /secure/evidence/apply-journal.jsonl \
  --destination-codex-home /absolute/destination/codex-home \
  --destination-sqlite-home /absolute/destination/sqlite-home \
  --preflight-capture-directory /secure/evidence/recovery-preflight \
  --post-capture-directory /secure/evidence/recovery-post \
  --receipt /secure/evidence/recovery-receipt.json \
  --accept-plan INSTALL_PLAN_SHA256 \
  --accept-journal APPLY_JOURNAL_SHA256 \
  --destination-host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --quiescence-attestation /secure/evidence/recovery-quiescence.json \
  --accept-quiescence-attestation RECOVERY_ATTESTATION_SHA256 \
  --acknowledge-private-recovery
```

Do not add apply-receipt arguments to either command. A journal-bound original
apply receipt, if present, is checked automatically; a missing terminal receipt
is part of the bounded recovery classification.

### Recover a rollback journal

The rollback-journal form requires the original apply receipt. Bind its digest
in the fresh attestation:

```bash
python3 scripts/bulkload.py codex-private-quiescence-attest \
  --codex-home /absolute/destination/codex-home \
  --sqlite-home /absolute/destination/sqlite-home \
  --output /secure/evidence/rollback-recovery-quiescence.json \
  --operation-output /secure/evidence/rollback-recovery-receipt.json \
  --purpose recover \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --include-auth \
  --include-sqlite \
  --accept-plan INSTALL_PLAN_SHA256 \
  --accept-journal ROLLBACK_JOURNAL_SHA256 \
  --accept-apply-receipt APPLY_RECEIPT_SHA256 \
  --acknowledge-writers-quiesced
```

Then pass the same accepted digest with the exact original apply-receipt path:

```bash
python3 scripts/bulkload.py codex-private-recover \
  --install-plan /secure/evidence/private-install-plan.json \
  --compatibility-plan /secure/evidence/private-compatibility.json \
  --source-bundle /secure/evidence/source-private \
  --destination-before-bundle /secure/evidence/destination-private \
  --journal /secure/evidence/rollback-journal.jsonl \
  --apply-receipt /secure/evidence/apply-receipt.json \
  --destination-codex-home /absolute/destination/codex-home \
  --destination-sqlite-home /absolute/destination/sqlite-home \
  --preflight-capture-directory /secure/evidence/rollback-recovery-preflight \
  --post-capture-directory /secure/evidence/rollback-recovery-post \
  --receipt /secure/evidence/rollback-recovery-receipt.json \
  --accept-plan INSTALL_PLAN_SHA256 \
  --accept-journal ROLLBACK_JOURNAL_SHA256 \
  --accept-apply-receipt APPLY_RECEIPT_SHA256 \
  --destination-host-authority-id 22222222-2222-4222-8222-222222222222 \
  --codex-version 0.145.0 \
  --quiescence-attestation \
    /secure/evidence/rollback-recovery-quiescence.json \
  --accept-quiescence-attestation ROLLBACK_RECOVERY_ATTESTATION_SHA256 \
  --acknowledge-private-recovery
```

Never omit either rollback-journal apply-receipt binding, present a receipt
path without its digest, or bind a digest not represented in the fresh
attestation. Recovery rejects a journal/receipt path or digest mismatch before
mutation.

Recovery first performs read-only classification under the pinned runtime,
procedural fence, and cooperating lock. It can quarantine and repair only a
validated torn final journal tail, then chooses the bounded forward or rollback
transition supported by the durable events and live auth identity. Preserve
the validated journal; after a torn-tail repair, preserve both the repaired
journal and quarantine containing the removed bytes. Preserve all captures and
the recovery receipt.

## Evidence and final claim

Retain owner-private:

- source and destination capture attestations and bundles;
- compatibility and install plans plus accepted digests;
- apply/verify/rollback/recovery attestations;
- journal, external rollback directory, captures, and receipts;
- exact policy/runtime authority records;
- any v4 SQLite opening and v5 cross-plane close/action-plan evidence as a
  separate, non-authorizing chain when that workstream is also in scope; and
- the attended provider-turn result without credential values.

The strongest Bulkload-only claim is: accepted source auth bytes were installed
or found identical, destination SQLite remained exact with zero mutations, and
fresh offline verification passed. Claim working destination auth or
`SESSION_NATIVE` only after the separate fresh attended provider turn.
