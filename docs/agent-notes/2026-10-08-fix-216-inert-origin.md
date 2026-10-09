# 2026-10-08 fix-216 inert origin

Lane: #216, rulings OI-1003-Q129 and OI-1003-Q135. Branch
`fix/216-inert-origin-20261008` (worktree `bulkload.worktrees/fix-216-origin-20261008`,
off origin/main `ffa932e`). Not committed, pushed or opened as a PR; that is
left to the coordinator.

## Done

- A standalone restore (`restore_staged`, the estate-apply standalone item)
  no longer refuses `GIT_AUTHORITY_CHANGED` for a non-HTTPS origin.
  `remote.origin.url` and every other `remote.origin.*` key are now
  preserved-only and named in `configuration-activation.postcard`. The
  restored repository has no `origin` remote. `branch.*.remote` and
  `branch.*.pushremote` naming `origin` are preserved-only too (review
  round 1): Git otherwise reads `origin` as a path, and a carried `origin`
  entry in the worktree was fetched from and pushed into. An HTTPS origin
  is activated as before. The mapping path is unchanged.
- Configuration is now a plan (`plan_standalone_configuration`, which holds
  every refusal) followed by an apply (writes only). `restore_staged`
  builds the whole checkout (import, plan, worktree, index, modes,
  configuration apply) in a private `.bulkload-restore-*` stage beside the
  destination and publishes it by one no-replace rename
  (`publish_restored`, with the R-N119 per-entry fallback). Any refusal, a
  full disk or a killed run therefore leaves no destination (review round
  1; the first version staged only `.git`).
- A single-component relative destination (`git-restore b restored`)
  restores again; `linked_destination` treats an empty parent as `.`.
- Attach: a non-absolute mapping or a `to` that is not a repository
  refuses before the receipt is written. A mapped-origin mismatch or a
  capture without an origin refuses after the receipt is written but
  before `.git` is published; a rerun into that receipt now refuses
  `GIT_DESTINATION_OCCUPIED` instead of a bare EEXIST.
- `restore_linked_staged` is not staged: a materialization refusal after
  `worktree add` still leaves a partial worktree (documented; see Open).
- Tests: `git_carry::inert_origin_tests` (13 tests), plus
  `estate::tests::a_standalone_item_with_a_local_path_origin_restores_with_the_origin_inert`.
- docs/design.md: new "Standalone restore configuration" bullet under the
  Git carry list.

## Open

- A killed restore leaves a `.bulkload-restore-*` stage (now possibly a
  full checkout) in the workspace's parent, inside an enclosing item's
  restored worktree for a nested item. Nothing reclaims it automatically.
  A later capture of the enclosing workspace sees it as a nested
  repository: its `.git` is never carried, and a dirty or unborn nest
  refuses the capture, so it is surfaced rather than carried.
- Linked worktree restore: stage with `--no-checkout` and `git worktree
  move` into place, so it matches the standalone guarantee.
- Already half-restored destinations from runs before this fix still refuse
  `GIT_DESTINATION_OCCUPIED`. They need operator disposition (move them
  aside, then rerun).
