# Linear Artifact Map: Bulkload→Sting Migration
**Generated 2026-09-02** | TIN tracker + gstack automation lane D

## Reading Order for New Agents

1. **Migration core (TIN-3268, TIN-3692, TIN-3046)**: Codex session continuity + auth/SQLite over Neo→Sting; dev-seat adoption gate extended to Aug 27; external SSD relocation in review.
2. **Incident recovery (TIN-4189, TIN-4198, TIN-4193)**: Aug 28–29 etcd quorum loss + dual-voter power outage; fabric coherence tranche now executing; PDU/BMC decision pending.
3. **TCFS enablement (TIN-2306, TIN-1556, TIN-1417)**: Stop-rule clearance blocking broad ~/git/home claims; stable root identity + per-device crypto rungs; headless auth defect in backlog.
4. **Release + cleanup (TIN-4194, TIN-3418)**: v0.12.19 ready (ARS runners migrated, PZM darwin post-Sep 4); Codex consolidation (198→retiring).
5. **eGreg + runner-substrate (TIN-3080, TIN-4016)**: Daemon relocation honey→sting in progress; CI role-conflict resolved; acceptance proof pending.

---

## Tickets by Category (15 total)

### 🚀 Migration-Core (6)
Bulkload→Sting cutover: Codex continuity, dev-seat adoption, state relocation, release pipeline, runner orchestration.

| ID | Title | Status | Due | Role in Migration |
|---|---|---|---|---|
| TIN-3268 | bulkload: no-loss Codex sessions, auth, and SQLite composition for Neo→Sting | **In Progress** | — | Reusable Codex continuity protocol; foundation landed serially; final private-state cutover awaiting Neo writers quiesce |
| TIN-3692 | Sting dev-seat adoption gate — extended to 2026-08-27 (C1 window 2) | **In Review** | 2026-08-27 | Dev-seat adoption gate (who, tmux, PR work); C1 extended Aug 27; C2 fallback re-labels sting as agent host |
| TIN-3046 | Neo: relocate complete Codex state to encrypted TinylandState SSD | **In Review** | — | Cutover executed Aug 4; 389 GiB corpus on external SSD; 245 GiB internal awaits reclamation |
| TIN-4016 | sting: held runner-substrate role vs ARC compute-expansion scale set scheduling CI on sting node | **Done** | — | Conflict resolved: node affinity constraint on ARC runners; role-conflict eliminated |
| TIN-4194 | v0.12.19: cut through the migrated release flow (ARS runners + ci-templates pins) | **Todo** | — | Release pipeline migration ready; ARS runners + ci-templates pins; darwin closure via PZM post-Sep 4 |
| TIN-3080 | Relocate eGreg emacs daemon honey→sting (NEW-E) | **In Progress** | — | eGreg cockpit path Neo→Sting; daemon merged, TUI responding; requires attended acceptance + cockpit proof |

### 🔥 Incident Response (2)
Aug 28–29 critical outages: etcd quorum loss, dual-voter power layer simultaneous failure. Fabric coherence remediation in progress.

| ID | Title | Status | Due | Role in Migration |
|---|---|---|---|---|
| TIN-4189 | RKE2 etcd lost quorum/leader 2026-08-28 ~21:39Z: bumble unreachable, honey+sting crash-loop | **In Review** | — | Incident: etcd leaderless 2h; bumble VLAN/tailnet down; tailscale proxies down; R0–R3 repair ladder provided |
| TIN-4198 | Dual-voter power-layer outage 2026-08-29 11:33Z: honey + bumble dark, quorum lost ~2h | **In Review** | — | Simultaneous dual loss on power layer; OOB rung gap cost two rack walks in 14h; PDU/BMC decision needed |

### 🔧 TCFS Follow-On (5)
Post-stop-rule clearance: headless auth, stable root identity, per-device crypto wrapping, large-file streaming. Blocks ~/git/home broad claims.

| ID | Title | Status | Due | Role in Migration |
|---|---|---|---|---|
| TIN-2306 | tcfs: stop-rule clearance — enroll 2-3 small clean repos, drive TWO through R0-R5 both directions | **In Progress** | — | Stop-rule requirement: two clean repos R0–R5 bidirectionally; unblocks R6 (bumble) + multi-repo scope |
| TIN-2653 | tcfs auth: session token unusable over headless ssh — keychain write-only, 8-char print | **Backlog** | — | Headless auth defect: token saved to keychain but unreachable; 8-char print prevents TCFS_SESSION_TOKEN env fallback |
| TIN-1556 | tcfs: stable root identity and broad-directory ownership model | **In Progress** | 2026-08-31 | Stable root IDs for ~/git/Documents/dotfiles; root adoption/refusal semantics; cross-machine convergence proof |
| TIN-1417 | tcfs: Real per-device cryptographic identity + wrapped per-device subkeys | **In Progress** | 2026-10-31 | Real X25519 keypairs per device; per-device file-key wrapping behind default-off flag; revocation removes device |
| TIN-1419 | tcfs: Streaming large-file IO in FUSE / FileProvider (replace 10 GiB in-RAM buffer) | **Backlog** | 2026-06-21 | Remove 10 GiB in-RAM write buffer; chunked streaming with crash recovery; 50 GB write proof, peak RSS <500 MB |

### 🧹 Hygiene & Consolidation (2)
Fabric coherence tranche 1; Codex stream consolidation (198 branches, 87% unmoved).

| ID | Title | Status | Due | Role in Migration |
|---|---|---|---|---|
| TIN-4193 | Fabric coherence tranche 1 (R20): F1 one endpoint form + TLS, F2 fleet device registry, F3 unstick git-roam root | **In Progress** | — | Operator-ratified fabric coherence; F0 storage/control-plane done; F1–F3 rungs (endpoint form, device registry, git-roam conflict resolve) |
| TIN-3418 | Codex stream takeover: 198 codex branches, 87% never opened a PR — consolidation plan | **In Progress** | — | Codex consolidation: 198 branches (149 blahaj + 49 lab); 122 blahaj never reviewed; 18 identical-diff clusters; triage & retire plan |

---

## Project Status

### Sting Dev-Box: headless lane for tcfs + cmux-agent + eGreg (move plan)
- **ID**: `75e0a208-eea5-4815-9def-61f25cdf554e`
- **Status**: Active (no explicit project status updates on file)
- **Latest Update**: None recorded (project serves as migration orchestration board)
- **Role**: Centralized tracker for Sting adoption gate, eGreg cockpit relocation, tcfs + cmux-agent integration, runner role clarification

---

## Initiative Status

### Cordillera - Tinyland Remote-Everything Program
- **Status**: Active | **Health**: ⚠️ At Risk
- **Target**: Open-ended
- **Latest Update**: 2026-08-11 by Jess Sullivan
  > GF-range truth: protected-main Nix loop works, but ordinary inline acceleration not universal. Temporary overlay authority defect on :8081/:8980. Current route TIN-3594→TIN-2609→TIN-3356/3380→TIN-2221/1445→TIN-2730. Schema-4 promotions zero. Architecture coherent but live recursive dogfood loop incomplete.

### Tummycrypt — Daily Driver Track
- **Status**: Active | **Health**: ⚠️ At Risk
- **Target**: 2026-10-31 (extended from 2026-08-31; PerDevice wrap-mode commitment unreachable)
- **Latest Update**: 2026-08-27 by Jess Sullivan
  > Completions since 07-14: TIN-2860 (config-secret), TIN-2863 (versioned root identity), TIN-2853 (stable-root routing), TIN-2854 (HTTPS-by-default), TIN-2347 (ghost-device revocation). **Critical blockers**: TIN-2658 (live prod git-roam-tool stuck 6-path conflict), TIN-2306 (stop-rule clearance still open). TIN-1423 (autoupdate) ~2 months overdue. TIN-1417 (per-device crypto) untouched 4 weeks.

---

## Summary

**15 tickets tracked** across migration-core, incident response, tcfs follow-on, and hygiene. Two initiatives at-risk (Cordillera, Tummycrypt) due to infrastructure outages (TIN-4189, TIN-4198) and production git-roam conflict (TIN-2658). Critical path: TIN-2306 (stop-rule clearance) → TIN-1556 (stable root) → TIN-1417 (per-device crypto) before broad ~/git/home claims. Release TIN-4194 staged; eGreg relocation TIN-3080 awaiting acceptance proof; Codex continuity TIN-3268 final cutover pending Neo writer quiesce.
