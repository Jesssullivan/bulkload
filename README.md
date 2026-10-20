# bulkload

A live-host estate mover: it carries Git repositories and worktrees, agent
history and configuration, credentials, dotfiles and SQLite state from one
development machine to another while both keep working, without losing unique
content and without re-reading what the destination already holds.

## Status

The Rust engine works end to end (walk, framed chunk transport over ssh,
resumable state on both ends, Git capture, import, convergence and adoption,
typed refusals) and has moved real estate between hosts. Resume on the
destination has never passed; the seat migration is not complete
([docs/SPEC.md](docs/SPEC.md)). It does not yet meet its performance bar:
since R-C842 (2026-10-16) the gate of record is end-to-end wall-clock time
to a verified working seat, compared against an rclone sync of the same
home, and that comparison has no result of record yet. The corpus race
(S1 gate a) stays only as a regression guard; its samples are in
[docs/slo.md](docs/slo.md) S1 and [docs/evidence/](docs/evidence/). Live status is on the P-TIN-177
milestones and their live issues; the Linear entry-point document is named
in [docs/SOURCES.md](docs/SOURCES.md).

## Build and test

```bash
nix develop            # toolchain, linters
just check-fast        # mandatory tier; run before pushing (PR CI: ci-source + ci-fault-harness)
just check-full        # mandatory + optional tier (bench stubs, history scan, Nix/Bazel)
just rust-check        # fmt, clippy -D warnings, tests
cargo build --release -p bulkload-agent -p bulkload-bench
target/release/bulkload-agent        # prints the verb list
target/release/bulkload-bench --help
```

## Documents

- [Where each fact lives](docs/SOURCES.md)
- [What bulkload must achieve](docs/SPEC.md) (#236)
- [SLOs and proofs (superseded 2026-10-20 by SPEC.md)](docs/slo.md)
- [Design](docs/design.md)
- [Measured evidence](docs/evidence/)
