# bulkload

Bulkload means continuing work on another dev machine while both hosts keep
working: Git, worktrees, agent history and configuration, credentials, dots and
SQLite, with no lost unique content.

The Python ceremony engine is removed; its source remains in Git history.
Implementation continues in [tummycrypt PR #592](https://github.com/Jesssullivan/tummycrypt/pull/592).
That M0/M1 carrier does not establish a completed M2 migration or runtime adoption.

[The product contract](docs/design.md) defines completion.
[R25](https://github.com/Jesssullivan/bulkload/issues/34) requires incremental
resume without halting writers and measured comparison with rclone before
claiming acceleration.

Keep the [week review](docs/WEEK-REVIEW-20260829.md),
[rulings ledger](docs/wayfinding-20260829/rulings-ledger.md) and
[dialog record](docs/wayfinding-20260829/dialogs-interviews.md) as historical evidence.
Their superseded instructions and dated status are not current authority.

just check validates the remaining repository and CI contracts via Bazel.
//:bulkload is a documentation bundle, not a migration executable.
