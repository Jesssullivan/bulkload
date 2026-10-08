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
  restored repository has no `origin` remote; `branch.*.remote = origin`
  stays active and fails closed until an origin is added. An HTTPS origin
  is activated as before. The mapping path is unchanged.
- Configuration is now a plan (`plan_standalone_configuration`, which holds
  every refusal) followed by an apply (writes only). `restore_staged`
  imports the capture and builds the plan in a private
  `.bulkload-restore-*` stage beside the destination, then creates the
  destination and renames the stage's `.git` into it. A configuration or
  authority refusal therefore leaves no destination.
- The attach paths check the mapping before writing the receipt.
  `restore_linked_staged` and attach document their remaining after-write
  refusals, which are concurrent-writer checks only.
- Tests: `git_carry::inert_origin_tests` (7 tests), plus
  `estate::tests::a_standalone_item_with_a_local_path_origin_restores_with_the_origin_inert`.
- docs/design.md: new "Standalone restore configuration" bullet under the
  Git carry list.

## Open

- An estate interrupted before the destination is created leaves a
  `.bulkload-restore-*` stage in the workspace's parent. Nothing reclaims it
  automatically.
- Already half-restored destinations from runs before this fix still refuse
  `GIT_DESTINATION_OCCUPIED`. They need operator disposition (move them
  aside, then rerun).
