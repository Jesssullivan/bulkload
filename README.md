# bulkload

A live-host estate mover: it carries Git repositories and worktrees, agent
history and configuration, credentials, dotfiles and SQLite state from one
development machine to another while both keep working, without losing unique
content and without re-reading what the destination already holds.

## Status

The Rust engine works end to end (walk, framed chunk transport over ssh,
resumable state on both ends, Git capture, import, convergence and adoption,
typed refusals) and has moved real estate between hosts. It does not yet
meet its performance bar (S1, faster than rclone): the status and the latest
sample of record are in [docs/slo.md](docs/slo.md) S1, and every sample is
in [docs/evidence/](docs/evidence/). Live status is on the P-TIN-177
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
- [SLOs and proofs](docs/slo.md)
- [Design](docs/design.md)
- [Measured evidence](docs/evidence/)
