# bulkload

A live-host estate mover: it carries Git repositories and worktrees, agent
history and configuration, credentials, dotfiles and SQLite state from one
development machine to another while both keep working, without losing unique
content and without re-reading what the destination already holds.

## Status

The Rust engine works end to end (walk, framed chunk transport over ssh,
resumable state on both ends, Git capture and import, typed refusals) and has
moved real estate between hosts. It does not yet meet its performance bar:
on the R23 corpus (242.6 MB, 23 files) the native initial copy took
3,015 ms against rclone's 601 ms
([evidence](docs/evidence/r23-2026-09-18.md)). The M2 engine work that
addresses this is tracked in Linear project "Bulkload M2: SLO engine" and
issues #42–#49.

## Build and test

```bash
nix develop            # toolchain, linters
just rust-check        # fmt, clippy -D warnings, tests
cargo build --release -p bulkload-agent -p bulkload-bench
target/release/bulkload-agent        # prints the verb list
target/release/bulkload-bench --help
```

## Documents

- [Product contract](docs/design.md)
- [Product bar, R25](https://github.com/Jesssullivan/bulkload/issues/34)
- [Measured evidence](docs/evidence/)
