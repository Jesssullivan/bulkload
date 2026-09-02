# GitHub Artifact Map: bulkload → sting Migration (2026-08-21 → 09-02)

## Navigation Preamble

The bulkload→sting migration spans three repositories:

1. **Jesssullivan/bulkload** (PRIMARY): The killed Python engine, product/skill docs, and ceremony/refusal/preflight specs. Holds all product-side work. Local clones:
   - `/Users/jess/git/bulkload` (stale, on `codex/sting-agent-cutover-v4-20260822`, commit 068b337 from 2026-08-24)
   - `/Volumes/TinylandSSD/bulkload-refactor` (active, on `refactor/idempotent-parallel-capture`, commit 7355f8c; worktrees at bl-wt-28, bulkload-engine-final)

2. **Jesssullivan/tummycrypt** (REBUILD): TCFS Rust workspace where the R25 rebuild lands as `crates/tcfs-bulkload*`. Holds the language binding, reconciliation, and storage mechanics.

3. **tinyland-inc/lab** (OPS): Ops/runbooks/sting docs. Holds the operational landing guide, tcfs fencing, and remote-dev lanes.

**Satellite:** Jesssullivan/eGreg (minor; graphical client + seat-move interlink).

---

## Jesssullivan/bulkload: PRs #1–#34

| # | Title | State | Merged At |
|---|-------|-------|-----------|
| 32 | fix(scanner): bound, recorded, refusable break-glass for the unsatisfiable final live fence | MERGED | 2026-08-29 14:33 |
| 31 | Fold custody required/seen paths only where the source measured it | MERGED | 2026-08-29 14:26 |
| 30 | fix(executor): exempt symlink mode in _verify_record (cross-kernel) | MERGED | 2026-08-29 14:18 |
| 29 | feat(transport): content-addressed native mover behind --mover, default rsync | OPEN | — |
| 28 | feat(refusal): name what refused, where, and what to do about it | OPEN | — |
| 27 | feat(doctor): read-only cross-kernel preflight that names every offending path | OPEN | — |
| 26 | feat(telemetry): default-on progress, in-phase heartbeats, honest counters | OPEN | — |
| 25 | feat(cli): exit-code table, --dry-run, --progress, and help for every option | OPEN | — |
| 23 | docs(agents): AGENTS.md carries the live-snapshot contract, not the quiesced one | MERGED | 2026-08-29 00:22 |
| 22 | Cross-kernel transport: symlink mode exemption + operator runtime pin | MERGED | 2026-08-29 00:17 |
| 21 | Wave 2: move the resident structures off the heap | MERGED | 2026-08-29 00:12 |
| 20 | perf: Wave 0 - remove redundant full-corpus reads from capture/transport | MERGED | 2026-08-29 00:10 |
| 19 | fix: byte-at-a-time reads from buffering=0 on line iteration (1122x) | MERGED | 2026-08-29 00:10 |
| 18 | Add notes.md file | OPEN | — |
| 17 | fix: preserve opaque Git-like snapshot bytes | MERGED | 2026-08-24 06:34 |
| 16 | fix: capture unborn Git workspaces | MERGED | 2026-08-24 05:43 |
| 15 | feat: capture live agent state without interruption | MERGED | 2026-08-24 00:39 |
| 14 | feat: ceremony revision 1 — operator-owned quiet windows, fail-closed quiescence probe, exit 3 | MERGED | 2026-08-24 00:20 |
| 13 | fix: make large Sting cutovers executable | MERGED | 2026-08-23 18:49 |
| 12 | fix: preserve opaque live capture state | MERGED | 2026-08-23 14:47 |
| 11 | feat: ship AgentCaptureV4 Sting cutover | MERGED | 2026-08-23 04:17 |
| 10 | docs(release): truth-bind v0.1.0 boundaries | MERGED | 2026-08-18 22:21 |
| 9 | fix(ci): add public-read front door (TIN-3115) | MERGED | 2026-08-15 09:00 |
| 8 | feat(private-state): add read-only SQLite verifier oracle | MERGED | 2026-08-17 06:57 |
| 7 | feat(private-state): bind SQLite compose request | MERGED | 2026-08-17 06:52 |
| 6 | feat(private-state): close Codex SQLite action plan | MERGED | 2026-08-17 06:49 |
| 5 | feat(private-state): plan Codex SQLite composition | MERGED | 2026-08-17 06:44 |
| 4 | feat(private-state): apply Codex auth atomically | MERGED | 2026-08-17 06:33 |
| 3 | feat(private-state): capture Codex auth and SQLite | MERGED | 2026-08-17 06:16 |
| 2 | Prove no-loss Codex session unions and typed private-state policy | MERGED | 2026-08-17 06:09 |
| 1 | Add manifest-first bulk migration skill | MERGED | 2026-08-17 06:09 |

**Summary:** 21 MERGED (through 2026-08-29 14:33); 10 OPEN (features, fixes, docs).

---

## Jesssullivan/bulkload: Issues

| # | Title | State |
|---|-------|-------|
| 34 | Product bar (operator ruling R25, 2026-08-29): beat rclone as a live-host, never-rewalk, piecemeal estate mover | OPEN |
| 33 | Remote probes through a fish login shell: quoted python one-liners lose their quotes (false receipt failures) | OPEN |
| 24 | Final live fence is unsatisfiable for provider roots with pruned leaves (seal censuses the snapshot copy, fence censuses the live root) | OPEN |

**Summary:** 3 OPEN (critical path blocking issue #24; shell quoting issue #33; product roadmap #34).

---

## Jesssullivan/bulkload: Remote Branches (18 total)

| Branch | Purpose | Protected |
|--------|---------|-----------|
| main | Primary | Yes |
| codex/sting-agent-cutover-v4-20260822 | Agent capture v4 cutover + AGENTS.md binding | No |
| refactor/idempotent-parallel-capture | Capture idempotency | No |
| cleanup/docs-truth-dry-ax | Documentation cleanup | No |
| design/native-mover-20260829 | Native mover experimentation (rsync alternative) | No |
| feat/default-on-telemetry-20260829 | Telemetry defaults | No |
| feat/doctor-preflight-20260829 | Preflight diagnostics | No |
| feat/doctor-preflight-20260829-fix | Preflight fix (local) | No |
| feat/exit-codes-dry-run-progress-20260829 | CLI exit codes + flags | No |
| feat/structured-refusal-20260829 | Refusal naming/messaging | No |
| fix/ceremony-engine-20260828 | Ceremony engine fix | No |
| fix/cross-kernel-transport-20260828 | Cross-kernel transport fix | No |
| fix/custody-casefold-identity-20260828 | Custody casefold fix | No |
| fix/final-engine-casefold-20260828 | Final engine casefold fix | No |
| fix/symlink-mode-portability | Symlink mode cross-OS | No |
| fix/unbuffered-line-reads | Buffering fix (1122x improvement) | No |
| perf/wave0-bigo-20260827 | Performance wave 0: big-O improvement | No |
| perf/wave2-memory-20260827 | Performance wave 2: memory reduction | No |
| Jesssullivan-patch-1 | Patch branch | No |

---

## Jesssullivan/bulkload: Local Clones

### /Users/jess/git/bulkload
- **Current branch:** codex/sting-agent-cutover-v4-20260822
- **Head commit:** 068b337 (fix: preserve opaque Git-like snapshot bytes, 2026-08-24)
- **Status:** Stale (8 days old)

### /Volumes/TinylandSSD/bulkload-refactor
- **Current branch:** refactor/idempotent-parallel-capture
- **Head commit:** 7355f8c (fix: prove the partial is ours before trusting it as resume state)
- **Worktrees:**
  - `/Volumes/TinylandSSD/bulkload-refactor` (7355f8c, refactor/idempotent-parallel-capture)
  - `/Volumes/TinylandSSD/bl-wt-28` (1264b0d, feat/structured-refusal-20260829)
  - `/Volumes/TinylandSSD/bulkload-engine-final` (b4ea518, detached HEAD)
- **Local branches:**
  - feat/doctor-preflight-20260829
  - feat/doctor-preflight-20260829-fix (LOCAL ONLY)
  - remotes/github/feat/doctor-preflight-20260829

---

## tinyland-inc/lab: Satellite PRs

| # | Title | State | Merged At |
|---|-------|-------|-----------|
| 1595 | docs(sting): retruth STING_FIRST_HOUR + REMOTE_DEV_WORKFLOW to the 08-26/08-28 rulings | OPEN | — |
| 1547 | TIN-4193: expose tcfs enforce_tls / ca_cert_path as home-manager options | OPEN | — |
| 1545 | TIN-4193: one tcfs S3 endpoint form fleet-wide | OPEN | — |
| 1534 | fix(tailscale): declare honey's subnet routes + route-withdrawal guard (TIN-4191) | OPEN | — |
| 1528 | feat(remote-dev): R9 ssh ControlMaster, R11 tmux C-a prefix, R12 lane carriers | OPEN | — |
| 1527 | fix(tcfs): unfence without ansible-inventory — static five-label set | OPEN | — |
| 1424 | feat(tin-3692): first-hour landing guide, tcfs fence pair, never-signal rule, TIN-4016 runner-overflow carrier | MERGED | 2026-08-24 04:32 |

**Summary:** 6 OPEN (ops/remote-dev/sting truth); 1 MERGED (landing guide).

---

## Jesssullivan/tummycrypt: Satellite PRs (#575, #582–#591)

| # | Title | State | Merged At |
|---|-------|-------|-----------|
| 575 | docs(design): TIN-1556 — record the D4 Q1 ruling (/tcfs/<root_id>) and the lifted fence | OPEN | — |
| 582 | feat(tcfs-core): D4 slug module — uniform /tcfs/<root_id> prefix encode/decode + cross-OS healing (TIN-1556) | OPEN | — |
| 583 | ci(contract): declare tinyland.repo.json and wire repo-manifest-validate | OPEN | — |
| 584 | ci(contract): adopt lint-runs-on as a ratchet and close the runner_label enums | OPEN | — |
| 585 | ci(release): SHA-pin actions, fail loud on cache pushes, ONE nix cache, migrate the nix jobs to tinyland-nix | OPEN | — |
| 586 | reconcile: stat-gated freshness memo for the both-exist classifier (Lane 3 / R-D 122x) | OPEN | — |
| 587 | docs(claude): root CLAUDE.md imports AGENTS.md | OPEN | — |
| 588 | docs(agents): re-baseline AGENTS.md for the neo/nix era | MERGED | 2026-09-02 12:42 |
| 589 | docs(ops): design a repeatable storage soak, retry/noise budget, and endpoint SLO (TIN-1622) | OPEN | — |
| 590 | docs(ops): correct TIN-1547's premise -- FileProvider is deliberately read-only (TIN-2853) | OPEN | — |
| 591 | feat(cli): interactive storage wizard + best-effort status verify for tcfs init (TIN-1425) | OPEN | — |

**Summary:** 10 OPEN (tcfs-bulkload crate, reconciliation, docs/ops specs); 1 MERGED (AGENTS.md baseline, 2026-09-02).

---

## Jesssullivan/eGreg: Satellite PRs

| # | Title | State | Merged At |
|---|-------|-------|-----------|
| 106 | docs: point the seat-move interlink at bulkload's live README section | MERGED | 2026-08-28 21:40 |
| 105 | et: add --gui for a graphical client frame | MERGED | 2026-08-28 20:59 |

**Summary:** 2 MERGED (GUI client, seat-move interlink).

---

## Summary by Repository and State

| Repository | Total PRs | Merged | Open | Open Issues |
|------------|-----------|--------|------|-------------|
| Jesssullivan/bulkload | 32 | 21 | 10 | 3 |
| tinyland-inc/lab | 7 | 1 | 6 | — |
| Jesssullivan/tummycrypt | 11 | 1 | 10 | — |
| Jesssullivan/eGreg | 2 | 2 | — | — |

**Grand Total:** 52 PRs (25 merged, 27 open); 3 blocking issues.

---

## Key Blocking Issues

- **#24 (bulkload):** Final live fence unsatisfiable for provider roots with pruned leaves — core reconciliation logic blocker
- **#33 (bulkload):** Remote probe shell quoting losses — affects cross-kernel transport reliability
- **#34 (bulkload):** Product roadmap — beat rclone, never-rewalk, piecemeal estate mover (R25 operator ruling)

---

## Critical Dates

- **Migration window:** 2026-08-21 → 09-02 (12 days)
- **Last bulkload merge:** 2026-08-29 14:33 (PR #32)
- **Last tummycrypt merge:** 2026-09-02 12:42 (PR #588, AGENTS.md baseline)
- **stale local clone:** /Users/jess/git/bulkload (2026-08-24, 9 days old)
