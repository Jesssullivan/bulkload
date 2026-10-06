//! P76 SOURCE-COMMAND-REGISTRY (S2: never interrupt the source; OI-1003-Q5,
//! OI-1003-Q9, OI-1003-Q16 typed source access).
//!
//! Every child process the workspace's non-test code builds with
//! `std::process::Command` is registered here, crate by crate. The one
//! source-safe Git builder is the agent's `git_carry::git`: it runs every Git
//! child with `--no-optional-locks`, the `git_env` table's `-c` overrides
//! (hooks, fsmonitor, automatic gc and maintenance off) and environment
//! (`GIT_OPTIONAL_LOCKS=0`, no lazy fetch, no prompt, no system or global
//! configuration), and a discovery ceiling (WP1, #145). The process enters
//! background CPU and IO priority before any verb runs (WP0(f),
//! OI-1003-Q17/Q25), and every child inherits it.
//!
//! **The scan.** Like `refusal_taxonomy.rs`, it reads every `crates/*/src`
//! tree as text. It drops comments, string and char literals, every item
//! behind `#[cfg(test)]` or `#[cfg(all(test, ..))]`, and every file a parent
//! declares as a module behind those attributes (a file is never dropped for
//! its name). It then finds every `Command` token followed by `::new`,
//! whatever path leads to it. A site is named
//! `crate::module[::inline module]::function(program)`, never by line.
//!
//! **The registry.** Every site is the sanctioned builder or one entry of
//! [`BYPASSES`]. An entry pins how many times its id occurs (once), the text
//! of the child it builds (its *shape*: the source from `Command::new` to the
//! spawn, or to the end of a builder function, without comments or
//! whitespace) and, for a builder function, every call of it. The ids are a
//! subset of [`FROZEN`], so an entry cannot be swapped for another child.
//!
//! **What a text scan cannot find**, and how each gap is closed or stated:
//!
//! - A renamed `Command` (`use .. as`, a `type` alias, an `impl .. for
//!   Command`) is refused outright ([`evasions`]).
//! - A child started without `Command` (`fork`, `exec*`, `posix_spawn*`,
//!   `system`, `popen`, the raw syscall numbers, `CommandExt`) is refused
//!   outright, in every crate.
//! - A macro that assembles those names from pieces, a foreign function
//!   declared under another `link_name`, and a child started by a dependency
//!   are **not** found. `link_name` itself is refused, and the dependency
//!   wall is `dep_graph.rs`.
//!
//! **What callers do with the sanctioned builder.** `git_carry::git` returns
//! a `Command`, and a caller could undo the contract or run a writer on a
//! source. The scan cannot tell a source repository from a capture's private
//! one: that needs two builder types, which is production work reported on
//! bulkload#188. What is checked instead:
//!
//! - no agent code calls `env_clear` or `envs`;
//! - no literal outside `git_env` assigns a Git configuration key, except
//!   the entries of [`EXTRA_CONFIG`], and the probe script repeats the
//!   table's own values;
//! - no literal outside `git_env` and the probe builder names a variable
//!   that the contract sets or that injects configuration;
//! - every Git subcommand literal in the `git_carry` tree is a read
//!   ([`READS`]) or a registered writer with a ceiling ([`WRITERS`]);
//! - dynamically, a `git` wrapper first on `PATH` records every Git child of
//!   a real `git-export`, a real local `git-carry-estimate` and a real
//!   `estate-capture`. Each carries the contract, none overrides it with a
//!   later `-c`, and every child aimed at the source repository runs a read.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::ops::Range;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The one source-safe Git builder.
const SANCTIONED: &str = "agent::git_carry::git(\"git\")";

/// A child builder in non-test code that is not [`SANCTIONED`].
struct Bypass {
    /// `crate::module::function(program)`. It occurs exactly once.
    id: &'static str,
    /// The child as built, comments and whitespace removed: from
    /// `Command::new` to its `spawn`/`output`/`status`, or to the end of the
    /// function when the function returns the command. Every agent entry
    /// pins one; the operator tools pin the Git children only.
    shape: Option<&'static str>,
    /// For a function that returns the command: every non-test call of it,
    /// as (calling function, the call in its enclosing expression).
    callers: &'static [(&'static str, &'static str)],
    /// Why the bypass is tolerated.
    why: &'static str,
}

/// Every child builder outside [`SANCTIONED`], each argued. The agent's four
/// are filed as bulkload#188; the operator tools' are argued as never aimed
/// at a source. This list only shrinks: its ids are a subset of [`FROZEN`].
const BYPASSES: &[Bypass] = &[
    Bypass {
        id: "agent::git_carry::estimate::local_probe(\"bash\")",
        shape: Some(
            "Command::new(\"bash\");command.args([\"-s\",\"--\"]).arg(repository);\
             forkeyin[\"GIT_DIR\",\"GIT_WORK_TREE\",\"GIT_INDEX_FILE\",\
             \"GIT_OBJECT_DIRECTORY\",\"GIT_ALTERNATE_OBJECT_DIRECTORIES\",\
             \"GIT_COMMON_DIR\",\"GIT_NAMESPACE\",\"GIT_CONFIG_COUNT\",\
             \"GIT_CONFIG_PARAMETERS\",]{command.env_remove(key);}\
             command.env(\"GIT_NO_LAZY_FETCH\",\"1\").env(\"LC_ALL\",\"C\")\
             .env(\"LANGUAGE\",\"\");command}",
        ),
        callers: &[
            (
                "agent::git_carry::estimate::estimate_with",
                "run_probe(&mutlocal_probe(source),store)",
            ),
            (
                "agent::git_carry::estimate::estimate_with",
                "run_probe(&mutlocal_probe(path),store)",
            ),
            (
                "agent::git_carry::carry_v2::probe",
                "run_probe(&mutlocal_probe(path),store)",
            ),
            (
                "agent::git_carry::carry_v2::probe",
                "run_probe(&mutlocal_probe(path),store)",
            ),
            (
                "agent::git_carry::carry_v2::ingest::probe",
                "run_probe(&mutlocal_probe(path),store)",
            ),
        ],
        why: "Runs PROBE_SCRIPT on the local source: `bash -s -- REPOSITORY` \
              with the script on stdin (run_probe writes nothing else). Every \
              Git call in the script goes through its `g` function, whose \
              flags, exports and unsets are tested entry for entry against \
              git_env (estimate::tests::probe_and_source_git_calls_are_hardened), \
              but the child is not built by git_carry::git, and its own \
              env_remove list is a hand copy of 9 of git_env::CLEARED's 11 keys.",
    },
    Bypass {
        id: "agent::git_carry::estimate::ssh_command(\"ssh\")",
        shape: Some(
            "Command::new(\"ssh\");ssh.args([\"-T\",\"-oBatchMode=yes\",\
             \"-oConnectTimeout=15\",\"-oServerAliveInterval=15\",\
             \"-oServerAliveCountMax=4\",host,command,]);\
             ssh.env(\"LC_ALL\",\"C\").env(\"LANGUAGE\",\"\");ssh}",
        ),
        callers: &[(
            "agent::git_carry::estimate::estimate_with",
            "run_probe(&mutssh_command(host,&remote_command(path)),store)",
        )],
        why: "Runs PROBE_SCRIPT on a remote source over ssh; the remote line \
              is remote_command's `bash -s -- PATH` and nothing else. The \
              script carries the git_env hardening, but the remote bash does \
              not enter background CPU or IO priority (WP0(f) covers only the \
              local verb).",
    },
    Bypass {
        id: "agent::main::pull_command(\"ssh\")",
        shape: Some(
            "Command::new(\"ssh\");ssh.args([\"-T\",\"-oBatchMode=yes\",\
             \"-oConnectTimeout=15\"]);ifletSome(config)=args.get(6){\
             if!Path::new(config).is_absolute(){\
             returnErr(BulkloadRefusal::PathNotAbsolute);}\
             ssh.arg(\"-F\").arg(config);}ssh.arg(\"--\").arg(host).arg(remote);\
             ifbulkload_agent::durable::durability()==\
             bulkload_agent::durable::Durability::Strict{\
             ssh.arg(\"--durability=strict\");}letmutchild=ssh.arg(\"serve\")\
             .stdin(Stdio::piped()).stdout(Stdio::piped())\
             .stderr(Stdio::inherit()).spawn()",
        ),
        callers: &[],
        why: "Starts the remote `bulkload-agent serve`, which is the source \
              reader and enters background priority itself (OI-1003-Q25). The \
              remote word is `bulkload-agent` or a validated absolute path, \
              and the ssh child touches no local source.",
    },
    Bypass {
        id: "agent::provider_sqlite::hydrate::hydrate_one(program)",
        shape: Some(
            "Command::new(program).args([\"-dc\",\"--\"]).arg(source)\
             .stdin(Stdio::null()).stdout(Stdio::piped())\
             .stderr(Stdio::null()).spawn()",
        ),
        callers: &[],
        why: "A caller-named decompressor (`-dc --`) reads a retained \
              compressed rollout. It is not one of WP0(b)'s typed source \
              access kinds (file read, allowlisted git read, SQLite backup), \
              though it only reads its input.",
    },
    Bypass {
        id: "bench::main::read(\"pmset\")",
        shape: None,
        callers: &[],
        why: "The benchmark's preflight reads the power source with `pmset -g \
              batt` (R-N81). It names no path and reads no source tree.",
    },
    Bypass {
        id: "bench::main::rclone_copy(binary)",
        shape: None,
        callers: &[],
        why: "The R23 baseline arm: rclone copies the sealed benchmark corpus, \
              which the bench generates itself. It is never aimed at a live \
              estate source, and S2 is not measured on this arm.",
    },
    Bypass {
        id: "bench::main::rclone_version(binary)",
        shape: None,
        callers: &[],
        why: "`rclone version` for the benchmark's record. It reads no source \
              tree at all.",
    },
    Bypass {
        id: "handoff::lib::claude_probes(\"claude\")",
        shape: None,
        callers: &[],
        why: "Credential-class probe (WP10, OI-1003-Q14): `claude -p ok` proves \
              the seat holds the credential. It is given no path.",
    },
    Bypass {
        id: "handoff::lib::cluster_info(\"kubectl\")",
        shape: None,
        callers: &[],
        why: "Credential-class probe: `kubectl --context C cluster-info`. It \
              talks to a cluster and is given no local path.",
    },
    Bypass {
        id: "handoff::lib::codex_probes(\"codex\")",
        shape: None,
        callers: &[],
        why: "Credential-class probe: `codex --version`. It is given no path.",
    },
    Bypass {
        id: "handoff::lib::gh_probes(\"gh\")",
        shape: None,
        callers: &[],
        why: "Credential-class probe: `gh auth status`. It is given no path.",
    },
    Bypass {
        id: "handoff::lib::gpg_probes(\"gpg\")",
        shape: None,
        callers: &[],
        why: "Credential-class probe: gpg signs and then verifies a blob the \
              probe wrote in its own private scratch directory. Two children \
              (sign, verify), pinned as two.",
    },
    Bypass {
        id: "handoff::lib::kubeconfig_probes(\"kubectl\")",
        shape: None,
        callers: &[],
        why: "Credential-class probe: `kubectl config get-contexts -o name` \
              reads the kubeconfig, never an estate source.",
    },
    Bypass {
        id: "handoff::lib::ls_remote(\"git\")",
        shape: Some(
            "Command::new(\"git\");command.args([\"ls-remote\",\"--exit-code\"])\
             .arg(remote).arg(\"HEAD\").env(\"GIT_TERMINAL_PROMPT\",\"0\")\
             .env(\"GIT_ASKPASS\",\"\").env(\"GIT_SSH_COMMAND\",\
             \"ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new\",);\
             letcapture=run(&mutcommand,StdoutPolicy::Identifiers,timeout)",
        ),
        callers: &[],
        why: "Credential-class probe: `git ls-remote --exit-code REMOTE HEAD` \
              asks a remote for one ref. It is NOT built by git_env: no \
              --no-optional-locks, and the system and global configuration \
              apply. ls-remote of a URL opens no index and takes no lock; run \
              from inside a repository it also reads that repository's \
              configuration. Filed on bulkload#188.",
    },
    Bypass {
        id: "handoff::lib::signing_key(\"git\")",
        shape: Some(
            "Command::new(\"git\");command.args([\"config\",\"--get\",\"user.signingkey\"]);\
             letcapture=run(&mutcommand,StdoutPolicy::Identifiers,timeout)",
        ),
        callers: &[],
        why: "Credential-class probe: `git config --get user.signingkey` reads \
              configuration only (the current directory's repository, if any, \
              plus global and system). It is NOT built by git_env. `config \
              --get` opens no index, runs no hook and takes no lock. Filed on \
              bulkload#188.",
    },
    Bypass {
        id: "handoff::lib::sops_probes(\"sops\")",
        shape: None,
        callers: &[],
        why: "Credential-class probe: `sops -d FIXTURE` decrypts the operator's \
              named fixture to a byte count. It is given no estate path.",
    },
    Bypass {
        id: "handoff::lib::ssh_probes(\"ssh\")",
        shape: None,
        callers: &[],
        why: "Credential-class probe: `ssh ALIAS true` proves the alias \
              authenticates. It runs nothing on a source tree.",
    },
];

/// How many times each bypass id occurs. Every one is 1 except the gpg
/// probe, whose function builds its sign and its verify child.
fn expected_count(id: &str) -> usize {
    if id == "handoff::lib::gpg_probes(\"gpg\")" {
        2
    } else {
        1
    }
}

/// The bypass ids as of P76 (2026-10-06). [`BYPASSES`] must stay a subset:
/// delete from both when a bypass is removed, and never add to this one. A
/// new child goes through `git_carry::git`.
const FROZEN: &[&str] = &[
    "agent::git_carry::estimate::local_probe(\"bash\")",
    "agent::git_carry::estimate::ssh_command(\"ssh\")",
    "agent::main::pull_command(\"ssh\")",
    "agent::provider_sqlite::hydrate::hydrate_one(program)",
    "bench::main::read(\"pmset\")",
    "bench::main::rclone_copy(binary)",
    "bench::main::rclone_version(binary)",
    "handoff::lib::claude_probes(\"claude\")",
    "handoff::lib::cluster_info(\"kubectl\")",
    "handoff::lib::codex_probes(\"codex\")",
    "handoff::lib::gh_probes(\"gh\")",
    "handoff::lib::gpg_probes(\"gpg\")",
    "handoff::lib::kubeconfig_probes(\"kubectl\")",
    "handoff::lib::ls_remote(\"git\")",
    "handoff::lib::signing_key(\"git\")",
    "handoff::lib::sops_probes(\"sops\")",
    "handoff::lib::ssh_probes(\"ssh\")",
];

/// Ways to start a child without `std::process::Command::new`. No non-test
/// code in any crate may name one.
const SPAWN_WITHOUT_COMMAND: &[&str] = &[
    "system",
    "popen",
    "fork",
    "vfork",
    "forkpty",
    "execv",
    "execve",
    "execvp",
    "execvpe",
    "execveat",
    "fexecve",
    "execl",
    "execle",
    "execlp",
    "posix_spawn",
    "posix_spawnp",
    "clone3",
    "SYS_execve",
    "SYS_execveat",
    "SYS_fork",
    "SYS_vfork",
    "SYS_clone",
    "SYS_clone3",
    "CommandExt",
    "link_name",
];

/// Git subcommands that rewrite or repack a repository's store. No non-test
/// agent code may name one, for any repository.
const MAINTENANCE: &[&str] = &[
    "gc",
    "maintenance",
    "repack",
    "prune",
    "prune-packed",
    "commit-graph",
    "multi-pack-index",
    "pack-refs",
];

/// Git subcommands that only read the repository they are aimed at (given
/// `--no-optional-locks`, which stops `status` and `diff` refreshing the
/// index). These are the "allowlisted git read commands" of OI-1003-Q16, and
/// the only ones a child aimed at a source may run. A name in
/// [`MULTI_FORM`] is a read only in a registered form.
const READS: &[&str] = &[
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-ref-format",
    "config",
    "count-objects",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "for-each-ref",
    "fsck",
    "log",
    "ls-files",
    "ls-tree",
    "merge-base",
    "name-rev",
    "pack-objects",
    "reflog",
    "remote",
    "rev-list",
    "rev-parse",
    "show",
    "show-ref",
    "status",
    "symbolic-ref",
    "var",
    "verify-pack",
    "version",
    "worktree",
];

/// Reads that also have a writing form. [`read_form`] decides from the
/// arguments: the ones the source spells (statically, through the builder
/// chain) and the ones a real child was given (dynamically).
const MULTI_FORM: &[&str] = &[
    "bundle",
    "config",
    "fsck",
    "pack-objects",
    "reflog",
    "remote",
    "symbolic-ref",
    "worktree",
];

/// Functions that name a [`MULTI_FORM`] subcommand in one statement and
/// complete its arguments in another, so the scan cannot read the form. Each
/// is argued; the dynamic leg sees the real arguments.
const OPEN_FORMS: &[(&str, &str, &str)] = &[(
    "agent::git_carry::partial_clone",
    "config",
    "Builds `config [--includes --file FILE] QUERY`, where each QUERY is one \
     of the function's own `--get`/`--get-regexp` lists: a read of the \
     source's configuration.",
)];

/// Git subcommands that write the repository they are aimed at, each with the
/// most uses the `git_carry` tree may hold and where they are aimed. Every
/// one must run on a capture's private repository or a destination, never a
/// source. The scan cannot prove which: the dynamic leg does for the verbs it
/// runs, and a typed source/private builder split is asked for on
/// bulkload#188. A ceiling only shrinks.
const WRITERS: &[(&str, usize, &str)] = &[
    (
        "add",
        1,
        "restore_intent_to_add: the destination's index, during a restore.",
    ),
    (
        "bundle (writing form)",
        1,
        "`bundle unbundle` into the destination repository of an import.",
    ),
    (
        "config (writing form)",
        3,
        "prepare_attachment and the configuration activation: the private \
         attachment repository's own core.bare, core.worktree and reviewed keys.",
    ),
    (
        "fast-import",
        3,
        "raw_tree and ref_table: the capture's private repository.",
    ),
    (
        "fetch",
        4,
        "From a bundle file into the capture's private repository, a chain's \
         scratch repository or a destination. Never from a source repository.",
    ),
    (
        "hash-object",
        4,
        "The capture's private write store and a bundle's envelope repository.",
    ),
    (
        "index-pack",
        2,
        "shallow::unpack and carry_v2 ingest: the destination's store.",
    ),
    (
        "init",
        5,
        "Creates a private capture, attachment, chain or envelope repository, \
         or the destination.",
    ),
    (
        "mktree",
        2,
        "The capture's private write store and a bundle's envelope repository.",
    ),
    (
        "read-tree",
        6,
        "A private index (GIT_INDEX_FILE or a private repository) or the \
         destination's, during repair, attachment and restore.",
    ),
    (
        "symbolic-ref (writing form)",
        2,
        "Sets HEAD of the private attachment repository and of the destination.",
    ),
    (
        "update-ref",
        10,
        "The capture's private repository, a chain's scratch repository, a \
         bundle's envelope, and the destination of an import or restore.",
    ),
    (
        "worktree (writing form)",
        2,
        "`worktree add --no-checkout` in the destination, during a restore.",
    ),
    (
        "write-tree",
        1,
        "export_pass: the capture's snapshot, a private repository, index and \
         write store over the source's worktree (the dynamic leg checks all \
         three are outside the source).",
    ),
];

/// Literals that spell a Git command but are not one where they stand (an
/// object type, a configuration field, a file or directory name): none is an
/// argument of a Git child. Each with the most the `git_carry` tree may hold.
const NOT_COMMANDS: &[(&str, usize, &str)] = &[
    (
        "clean",
        2,
        "A filter driver's variable name (filter.NAME.clean).",
    ),
    (
        "commit",
        6,
        "An object type, as cat-file and for-each-ref print it.",
    ),
    ("config", 2, "The repository's `config` file name."),
    ("fetch", 1, "The remote.origin.fetch configuration field."),
    ("merge", 1, "The branch.NAME.merge configuration field."),
    ("rebase", 1, "The branch.NAME.rebase configuration field."),
    ("refs", 1, "The `refs` directory name."),
    ("remote", 1, "The branch.NAME.remote configuration field."),
    (
        "tag",
        4,
        "An object type, as cat-file and for-each-ref print it.",
    ),
    ("worktree", 4, "The name of a capture's worktree revision."),
];

/// Every Git command name, so an unregistered one is refused rather than
/// overlooked.
const GIT_COMMANDS: &[&str] = &[
    "add",
    "am",
    "annotate",
    "apply",
    "archive",
    "backfill",
    "bisect",
    "blame",
    "branch",
    "bugreport",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-mailmap",
    "check-ref-format",
    "checkout",
    "checkout-index",
    "cherry",
    "cherry-pick",
    "clean",
    "clone",
    "column",
    "commit",
    "commit-graph",
    "commit-tree",
    "config",
    "count-objects",
    "credential",
    "describe",
    "diagnose",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "difftool",
    "fast-export",
    "fast-import",
    "fetch",
    "fetch-pack",
    "filter-branch",
    "fmt-merge-msg",
    "for-each-ref",
    "for-each-repo",
    "format-patch",
    "fsck",
    "gc",
    "grep",
    "hash-object",
    "hook",
    "index-pack",
    "init",
    "init-db",
    "interpret-trailers",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "mailinfo",
    "mailsplit",
    "maintenance",
    "merge",
    "merge-base",
    "merge-file",
    "merge-index",
    "merge-tree",
    "mergetool",
    "mktag",
    "mktree",
    "multi-pack-index",
    "mv",
    "name-rev",
    "notes",
    "pack-objects",
    "pack-redundant",
    "pack-refs",
    "patch-id",
    "prune",
    "prune-packed",
    "pull",
    "push",
    "range-diff",
    "read-tree",
    "rebase",
    "receive-pack",
    "reflog",
    "refs",
    "remote",
    "repack",
    "replace",
    "replay",
    "rerere",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "send-pack",
    "shortlog",
    "show",
    "show-branch",
    "show-index",
    "show-ref",
    "sparse-checkout",
    "stash",
    "status",
    "stripspace",
    "submodule",
    "switch",
    "symbolic-ref",
    "tag",
    "unpack-file",
    "unpack-objects",
    "update-index",
    "update-ref",
    "update-server-info",
    "upload-archive",
    "upload-pack",
    "var",
    "verify-commit",
    "verify-pack",
    "verify-tag",
    "version",
    "whatchanged",
    "worktree",
    "write-tree",
];

/// `-c` assignments a caller adds after the sanctioned builder's own, each
/// argued. None may name a key of `git_env::CONFIG` (the last `-c` wins).
const EXTRA_CONFIG: &[(&str, &str)] = &[
    (
        "core.bare=false",
        "snapshot_command: the capture's private repository is bare, and the \
         snapshot reads the source's worktree through it.",
    ),
    (
        "pack.useSparse=false",
        "The estimate's and carry_v2's pack-objects: a plain reachability walk.",
    ),
    (
        "pack.useBitmaps=false",
        "The estimate's and carry_v2's pack-objects: no bitmap is read or written.",
    ),
    (
        "pack.useSparse=false pack.useBitmaps=false pack.threads=2 pack.windowMemory=64m",
        "carry_v2's recorded pack settings (a wire string, not an argument).",
    ),
    (
        "core.fsync=committed,derived-metadata",
        "carry_v2 ingest: durability of the destination's own store.",
    ),
    (
        "core.fsyncMethod=fsync",
        "carry_v2 ingest: durability of the destination's own store.",
    ),
    (
        "core.commitGraph=false",
        "carry_v2 ingest: the destination reads no commit-graph.",
    ),
];

/// Functions that inject configuration through `GIT_CONFIG_COUNT` and
/// `GIT_CONFIG_KEY_n`, each argued. The dynamic oracle accepts only
/// `filter.*` keys from them.
const CONFIG_INJECTORS: &[(&str, &str)] = &[(
    "agent::git_carry::nest_status",
    "A nested repository's status must not run its filter drivers: every \
     `filter.<name>.clean|smudge|process` it can see is overridden to empty \
     and `required` to false, which a `-c` list cannot do for names read at \
     run time. It disables programs; it enables none.",
)];

/// Environment variables that inject configuration or name a program for Git
/// to run. With the keys of `git_env::SET`, they may be named only by the
/// `git_env` table and the probe builder.
const INJECTING_ENV: &[&str] = &[
    "GIT_CONFIG",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_EXEC_PATH",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "GIT_EDITOR",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
];

/// Non-test agent functions that return a `Command`. None is `pub` or
/// `pub(crate)`, so no code outside the `git_carry` tree can take a Git
/// builder and add to it.
const BUILDERS: &[&str] = &[
    "agent::git_carry::carry_v2::ingest::git",
    "agent::git_carry::carry_v2::ingest::quarantined",
    "agent::git_carry::carry_v2::pinned",
    "agent::git_carry::estimate::hardened",
    "agent::git_carry::estimate::local_probe",
    "agent::git_carry::estimate::ssh_command",
    "agent::git_carry::git",
    "agent::git_carry::git_writer",
    "agent::git_carry::nest_status",
    "agent::git_carry::snapshot_command",
    "agent::git_carry::writing_privately",
];

fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

// ---------------------------------------------------------------- the scan

/// A named brace block (a function or an inline module) and its span.
#[derive(Debug, Clone)]
struct Block {
    name: String,
    span: Range<usize>,
    /// Where the item's signature starts being a body (its `{`).
    open: usize,
}

/// One source file: its text, its text with comments, literals and test items
/// blanked (same byte offsets), its text with only comments and test items
/// blanked, and its string literals' content spans.
struct Scanned {
    code: String,
    plain: String,
    literals: Vec<Range<usize>>,
    test_modules: Vec<String>,
    functions: Vec<Block>,
    modules: Vec<Block>,
}

fn blank(bytes: &mut [u8]) {
    for byte in bytes {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

impl Scanned {
    fn new(original: &str) -> Self {
        let (masked, plain, literals) = mask(original);
        let spans = test_spans(&masked);
        let mut test_modules = Vec::new();
        for span in &spans {
            test_modules.extend(declared_modules(&masked[span.clone()]));
        }
        let mut code = masked.into_bytes();
        let mut plain = plain.into_bytes();
        for span in &spans {
            blank(&mut code[span.clone()]);
            blank(&mut plain[span.clone()]);
        }
        let code = String::from_utf8(code).unwrap();
        let plain = String::from_utf8_lossy(&plain).into_owned();
        let literals = literals
            .into_iter()
            .filter(|literal| !spans.iter().any(|span| span.contains(&literal.start)))
            .collect();
        let functions = blocks(&code, "fn");
        let modules = blocks(&code, "mod");
        Self {
            code,
            plain,
            literals,
            test_modules,
            functions,
            modules,
        }
    }

    fn literal(&self, span: &Range<usize>) -> &str {
        &self.plain[span.clone()]
    }

    fn line(&self, at: usize) -> usize {
        self.code[..at].matches('\n').count() + 1
    }

    /// The innermost function whose body holds `at`.
    fn function_at(&self, at: usize) -> Option<&Block> {
        self.functions
            .iter()
            .filter(|block| block.open < at && at < block.span.end)
            .max_by_key(|block| block.span.start)
    }

    fn function_name(&self, at: usize) -> &str {
        self.function_at(at)
            .map_or("<module>", |block| block.name.as_str())
    }

    /// `module` extended by the inline modules that hold `at`, outermost
    /// first, then the enclosing function.
    fn path_at(&self, module: &str, at: usize) -> String {
        let mut inline: Vec<&Block> = self
            .modules
            .iter()
            .filter(|block| block.open < at && at < block.span.end)
            .collect();
        inline.sort_by_key(|block| block.span.start);
        let mut path = module.to_owned();
        for block in inline {
            path.push_str("::");
            path.push_str(&block.name);
        }
        path.push_str("::");
        path.push_str(self.function_name(at));
        path
    }

    /// `range` of the file without comments or whitespace, literals kept
    /// whole.
    fn squeeze(&self, range: Range<usize>) -> String {
        let plain = self.plain.as_bytes();
        let mut out = Vec::new();
        for at in range {
            let byte = plain[at];
            let in_literal = self.literals.iter().any(|literal| literal.contains(&at));
            if in_literal || !byte.is_ascii_whitespace() {
                out.push(byte);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// `source` with comments and string/char literal contents blanked to spaces
/// (newlines kept, byte offsets unchanged); `source` with only its comments
/// blanked; and the content span of every string literal.
fn mask(source: &str) -> (String, String, Vec<Range<usize>>) {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut plain = bytes.to_vec();
    let mut literals = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        let ident_before = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if b == b'/' && next == Some(b'/') {
            let end = bytes[i..]
                .iter()
                .position(|c| *c == b'\n')
                .map_or(bytes.len(), |at| i + at);
            blank(&mut out[i..end]);
            blank(&mut plain[i..end]);
            i = end;
        } else if b == b'/' && next == Some(b'*') {
            let start = i;
            let mut depth = 0_usize;
            while i < bytes.len() {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            let end = i.min(bytes.len());
            blank(&mut out[start..end]);
            blank(&mut plain[start..end]);
        } else if (b == b'r' || b == b'b' || b == b'c')
            && !ident_before
            && raw_open(&bytes[i..]).is_some()
        {
            let (open, hashes) = raw_open(&bytes[i..]).unwrap();
            let mut close = vec![b'"'];
            close.extend(std::iter::repeat_n(b'#', hashes));
            let start = i;
            let content = i + open;
            let end = bytes[content..]
                .windows(close.len())
                .position(|window| window == close.as_slice())
                .map_or(bytes.len(), |at| content + at);
            literals.push(content..end);
            i = (end + close.len()).min(bytes.len());
            blank(&mut out[start..i]);
        } else if b == b'"' || ((b == b'b' || b == b'c') && next == Some(b'"') && !ident_before) {
            let start = i;
            let content = if b == b'"' { i + 1 } else { i + 2 };
            let mut j = content;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            let end = j.min(bytes.len());
            literals.push(content..end);
            i = (end + 1).min(bytes.len());
            blank(&mut out[start..i]);
        } else if b == b'\'' && char_literal_len(&bytes[i..]).is_some() {
            let len = char_literal_len(&bytes[i..]).unwrap();
            blank(&mut out[i..i + len]);
            i += len;
        } else {
            i += 1;
        }
    }
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8_lossy(&plain).into_owned(),
        literals,
    )
}

/// For `r"`, `r#"`, `br"`, `br#"`, `cr"`, `cr#"`: the opening's length and
/// its `#` count.
fn raw_open(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = usize::from(matches!(bytes.first(), Some(b'b' | b'c')));
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hashes = bytes[i..].iter().take_while(|b| **b == b'#').count();
    i += hashes;
    (bytes.get(i) == Some(&b'"')).then_some((i + 1, hashes))
}

/// The length of a char literal at the start of `bytes`, or `None` for a
/// lifetime.
fn char_literal_len(bytes: &[u8]) -> Option<usize> {
    if bytes.get(1) == Some(&b'\\') {
        let end = bytes.iter().skip(3).position(|b| *b == b'\'')?;
        return Some(end + 4);
    }
    let width = match *bytes.get(1)? {
        b if b < 0x80 => 1,
        b if b >= 0xF0 => 4,
        b if b >= 0xE0 => 3,
        _ => 2,
    };
    (bytes.get(1 + width) == Some(&b'\'')).then_some(width + 2)
}

/// The end (exclusive) of the bracket group opening at `code[open]`.
fn group_end(code: &str, open: usize) -> usize {
    let mut depth = 0_usize;
    for (offset, c) in code[open..].char_indices() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return open + offset + 1;
                }
            }
            _ => {}
        }
    }
    code.len()
}

/// From `from`, the first `{` or `;` outside any parenthesis or square
/// bracket: where an item's body opens, or where a bodiless item ends.
fn body_or_end(code: &str, from: usize) -> Option<(usize, char)> {
    let mut depth = 0_usize;
    for (offset, c) in code[from..].char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '{' | ';' if depth == 0 => return Some((from + offset, c)),
            _ => {}
        }
    }
    None
}

const fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `code[at..at + len]` is a whole token.
fn whole_token(code: &str, at: usize, len: usize) -> bool {
    let bytes = code.as_bytes();
    (at == 0 || !is_ident(bytes[at - 1])) && bytes.get(at + len).is_none_or(|next| !is_ident(*next))
}

/// Every whole-token occurrence of `word` in `code`.
fn tokens(code: &str, word: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find(word).map(|x| from + x) {
        from = at + word.len();
        if whole_token(code, at, word.len()) {
            found.push(at);
        }
    }
    found
}

fn identifier(text: &str) -> &str {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    &text[..end]
}

/// Words that start an item, after any visibility.
const ITEM_KEYWORDS: &[&str] = &[
    "mod",
    "fn",
    "impl",
    "use",
    "const",
    "static",
    "struct",
    "enum",
    "type",
    "trait",
    "union",
    "macro_rules",
    "extern",
];

/// Whether the text at `code[at..]` starts an item (rather than a field, a
/// variant, a statement, an expression or a match arm).
fn starts_item(code: &str, at: usize) -> bool {
    let mut rest = code[at..].trim_start();
    if let Some(after) = rest.strip_prefix("pub") {
        if !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            rest = after.trim_start();
            if rest.starts_with('(') {
                rest = rest[rest.find(')').map_or(rest.len(), |close| close + 1)..].trim_start();
            }
        }
    }
    loop {
        let word = identifier(rest);
        let after = rest[word.len()..].trim_start();
        match word {
            // `unsafe fn`, `async fn`, `default impl`: an item only when an
            // item keyword follows; `unsafe {` is an expression.
            "unsafe" | "async" | "default" => rest = after,
            // `const NAME`, `const fn`, `const _`: not the `const {` block.
            "const" => return !after.starts_with('{'),
            _ => return ITEM_KEYWORDS.contains(&word),
        }
    }
}

/// The end (exclusive) of whatever one `#[cfg(test)]` attribute applies to,
/// starting at `from` (just past the attribute and any that follow it).
///
/// An item ends at its `;` or at the end of its body. Anything else (a
/// struct field, an enum variant, a parameter, a statement, a match arm) ends
/// at the next `,` or `;` outside brackets, or before the bracket that closes
/// its parent, or at the end of its own block when nothing continues it. The
/// rule errs towards leaving test code in the scan, which can only add a
/// false alarm, never hide a child.
fn attribute_target_end(code: &str, from: usize) -> usize {
    if starts_item(code, from) {
        return match body_or_end(code, from) {
            Some((at, '{')) => group_end(code, at),
            Some((at, _)) => at + 1,
            None => code.len(),
        };
    }
    let bytes = code.as_bytes();
    let mut at = from;
    while at < bytes.len() {
        match bytes[at] {
            b',' | b';' => return at + 1,
            b')' | b']' | b'}' => return at,
            b'(' | b'[' => at = group_end(code, at),
            b'{' => {
                at = group_end(code, at);
                let rest = code[at..].trim_start();
                let skipped = code.len() - at - rest.len();
                if rest.starts_with([',', ';']) {
                    return at + skipped + 1;
                }
                if !rest.starts_with(['.', '?']) {
                    return at;
                }
            }
            _ => at += 1,
        }
    }
    code.len()
}

/// The spans of everything behind `#[cfg(test)]` or `#[cfg(all(test, ..))]`,
/// from the attribute to the end of what it applies to.
fn test_spans(code: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut from = 0;
    loop {
        let Some(at) = ["#[cfg(test)]", "#[cfg(all(test,", "#[cfg(all(test)"]
            .iter()
            .filter_map(|marker| code[from..].find(marker).map(|at| from + at))
            .min()
        else {
            return spans;
        };
        let mut cursor = group_end(code, at + 1);
        loop {
            let rest = code[cursor..].trim_start();
            if !rest.starts_with("#[") {
                break;
            }
            cursor = group_end(code, code.len() - rest.len() + 1);
        }
        let end = attribute_target_end(code, cursor).max(cursor);
        spans.push(at..end);
        from = end;
    }
}

/// The names of `mod NAME;` declarations in `code`.
fn declared_modules(code: &str) -> Vec<String> {
    let mut names = Vec::new();
    for at in tokens(code, "mod") {
        let rest = code[at + 3..].trim_start();
        let name = identifier(rest);
        if !name.is_empty() && rest[name.len()..].trim_start().starts_with(';') {
            names.push(name.to_owned());
        }
    }
    names
}

/// Every `keyword NAME .. { .. }` in `code`: functions (`fn`) or inline
/// modules (`mod`).
fn blocks(code: &str, keyword: &str) -> Vec<Block> {
    let mut found = Vec::new();
    for at in tokens(code, keyword) {
        let after = at + keyword.len();
        let rest = code[after..].trim_start();
        let name = identifier(rest);
        if name.is_empty() || !code[after..].starts_with(char::is_whitespace) {
            continue;
        }
        let Some((open, '{')) = body_or_end(code, after) else {
            continue;
        };
        found.push(Block {
            name: name.to_owned(),
            span: at..group_end(code, open),
            open,
        });
    }
    found
}

/// Every `.rs` file under `root`.
fn rust_files(root: &Path) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out.sort();
    out
}

/// The directory holding `file`'s child modules.
fn module_dir(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap();
    if stem == "mod" || stem == "lib" || stem == "main" {
        file.parent().unwrap().to_path_buf()
    } else {
        file.with_extension("")
    }
}

/// The module path of `file` under `root`: `git_carry/estimate.rs` is
/// `git_carry::estimate`, `io/mod.rs` is `io`, `main.rs` is `main`.
fn module_path(root: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap().with_extension("");
    let mut parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.len() > 1 && parts.last().is_some_and(|last| last == "mod") {
        parts.pop();
    }
    parts.join("::")
}

/// One non-test source file of one crate.
struct Source {
    /// `agent::git_carry::estimate`.
    module: String,
    /// `bulkload-agent/src/git_carry/estimate.rs`.
    shown: String,
    file: Scanned,
}

impl Source {
    fn at(&self, offset: usize) -> String {
        format!("{}:{}", self.shown, self.file.line(offset))
    }
}

/// Every non-test source file under `root` (a crate's `src`), scanned. A
/// file is test code only when a parent declares it as a module behind
/// `#[cfg(test)]`, or when it lies under such a module's directory: a file
/// named `tests.rs` that is declared without the attribute is scanned.
fn crate_sources(name: &str, root: &Path) -> Vec<Source> {
    let scanned: Vec<(PathBuf, Scanned)> = rust_files(root)
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).unwrap();
            (path, Scanned::new(&text))
        })
        .collect();
    let mut test_paths = Vec::new();
    for (path, file) in &scanned {
        let dir = module_dir(path);
        // A module also declared outside a test item is not test-only.
        let outside = declared_modules(&file.code);
        for module in &file.test_modules {
            if !outside.contains(module) {
                test_paths.push(dir.join(module));
                test_paths.push(dir.join(format!("{module}.rs")));
            }
        }
    }
    scanned
        .into_iter()
        .filter(|(path, _)| !test_paths.iter().any(|test| path.starts_with(test)))
        .map(|(path, file)| Source {
            module: format!("{name}::{}", module_path(root, &path)),
            shown: format!(
                "bulkload-{name}/src/{}",
                path.strip_prefix(root).unwrap().display()
            ),
            file,
        })
        .collect()
}

/// Every non-test source file of every crate in the workspace.
fn workspace_sources() -> Vec<Source> {
    let mut crates: Vec<PathBuf> = fs::read_dir(workspace_crates())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("src").is_dir())
        .collect();
    crates.sort();
    let mut sources = Vec::new();
    for path in crates {
        let dir = path.file_name().unwrap().to_string_lossy().into_owned();
        let name = dir.strip_prefix("bulkload-").unwrap_or(&dir).to_owned();
        sources.extend(crate_sources(&name, &path.join("src")));
    }
    sources
}

fn agent_sources() -> Vec<Source> {
    workspace_sources()
        .into_iter()
        .filter(|source| source.module.starts_with("agent::"))
        .collect()
}

/// A child builder in non-test code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Site {
    id: String,
    shown: String,
    shape: String,
}

/// Every `Command::new` in `source`'s non-test code, whatever path names the
/// type (`Command::new`, `std::process::Command::new`, `<Command>::new`).
fn sites_in(source: &Source) -> Vec<Site> {
    let file = &source.file;
    let code = &file.code;
    let mut sites = Vec::new();
    for at in tokens(code, "Command") {
        let mut rest = code[at + "Command".len()..].trim_start();
        rest = rest.strip_prefix('>').unwrap_or(rest).trim_start();
        let Some(method) = rest.strip_prefix("::") else {
            continue;
        };
        let method = method.trim_start();
        if identifier(method) != "new" {
            continue;
        }
        let after_new = code.len() - method.len() + 3;
        let program = if code[after_new..].trim_start().starts_with('(') {
            let open = after_new + code[after_new..].find('(').unwrap();
            let close = group_end(code, open) - 1;
            file.plain[open + 1..close].trim().to_owned()
        } else {
            "<fn pointer>".to_owned()
        };
        // The child as built: to its spawn, or to the end of the function
        // that returns it.
        let limit = file
            .function_at(at)
            .map_or_else(|| statement_end(code, at), |block| block.span.end);
        let end = [".spawn(", ".output(", ".status(", " run("]
            .iter()
            .filter_map(|call| code[at..limit].find(call).map(|x| at + x + call.len() - 1))
            .min()
            .map_or(limit, |open| group_end(code, open));
        sites.push(Site {
            id: format!("{}({program})", file.path_at(&source.module, at)),
            shown: source.at(at),
            shape: file.squeeze(at..end),
        });
    }
    sites
}

/// The end (exclusive) of the statement holding `at`.
fn statement_end(code: &str, at: usize) -> usize {
    let bytes = code.as_bytes();
    let mut cursor = at;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b';' => return cursor + 1,
            b')' | b']' | b'}' => return cursor,
            b'(' | b'[' | b'{' => cursor = group_end(code, cursor),
            _ => cursor += 1,
        }
    }
    code.len()
}

/// The start of the statement, or of the bracket group, holding `at`.
fn statement_start(code: &str, at: usize) -> usize {
    let bytes = code.as_bytes();
    let mut depth = 0_usize;
    let mut cursor = at;
    while cursor > 0 {
        match bytes[cursor - 1] {
            b')' | b']' => depth += 1,
            b'}' if depth > 0 => depth += 1,
            // A closing brace ends the statement before it, unless it closes
            // an expression that this one continues (`S { .. }, here`).
            b'}' => {
                if !code[cursor..]
                    .trim_start()
                    .starts_with([',', ')', ']', '.', '?'])
                {
                    return cursor;
                }
                depth += 1;
            }
            b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
            b';' | b'{' | b'(' | b'[' => return cursor,
            _ => {}
        }
        cursor -= 1;
    }
    0
}

fn all_sites(sources: &[Source]) -> Vec<Site> {
    let mut sites: Vec<Site> = sources.iter().flat_map(sites_in).collect();
    sites.sort();
    sites
}

/// Ways of reaching `std::process::Command` under another name, or of
/// starting a child without it. Each is a breach in any crate.
fn evasions(source: &Source) -> Vec<String> {
    let code = &source.file.code;
    let mut found = Vec::new();
    for at in tokens(code, "Command") {
        let start = statement_start(code, at);
        let head = code[start..at].trim_start();
        let after = code[at + "Command".len()..].trim_start();
        let renamed = after.strip_prefix("as").is_some_and(|rest| {
            rest.starts_with(char::is_whitespace) && identifier(rest.trim_start()) != "_"
        });
        let words: Vec<&str> = head
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|word| !word.is_empty())
            .collect();
        let aliased = words
            .iter()
            .position(|word| *word == "type")
            .is_some_and(|at| {
                words[..at]
                    .iter()
                    .all(|word| ["pub", "crate", "super"].contains(word))
            });
        let implemented = words.first() == Some(&"impl")
            && words.contains(&"for")
            && (after.starts_with('{') || after.starts_with("where"));
        if renamed {
            found.push(format!(
                "{}: `Command as ..` renames the type",
                source.at(at)
            ));
        }
        if aliased {
            found.push(format!("{}: a `type` alias of Command", source.at(at)));
        }
        if implemented {
            found.push(format!("{}: `impl .. for Command`", source.at(at)));
        }
    }
    for word in SPAWN_WITHOUT_COMMAND {
        for at in tokens(code, word) {
            found.push(format!(
                "{}: `{word}` starts a child without Command",
                source.at(at)
            ));
        }
    }
    for at in tokens(code, "clone") {
        if code[..at].trim_end().ends_with("libc::") {
            found.push(format!("{}: `libc::clone`", source.at(at)));
        }
    }
    for at in tokens(code, "exec") {
        if code[..at].ends_with('.') && code[at + 4..].trim_start().starts_with('(') {
            found.push(format!("{}: `.exec(` replaces the process", source.at(at)));
        }
    }
    found
}

/// Every breach of the registry in `sites`: an unregistered child, a child
/// built more or fewer times than registered, a child whose text changed,
/// or a stale entry.
fn registry_breaches(sites: &[Site], bypasses: &[Bypass]) -> Vec<String> {
    let mut found = Vec::new();
    let mut by_id: BTreeMap<&str, Vec<&Site>> = BTreeMap::new();
    for site in sites {
        by_id.entry(site.id.as_str()).or_default().push(site);
    }
    let sanctioned = by_id.remove(SANCTIONED).unwrap_or_default();
    if sanctioned.len() != 1 {
        found.push(format!(
            "exactly one sanctioned Git builder, found {}: {sanctioned:?}",
            sanctioned.len()
        ));
    }
    let mut registered = 0;
    for bypass in bypasses {
        let built = by_id.remove(bypass.id).unwrap_or_default();
        let expected = expected_count(bypass.id);
        registered += expected;
        if built.is_empty() {
            found.push(format!(
                "{}: the bypass no longer exists; remove it from BYPASSES and FROZEN",
                bypass.id
            ));
            continue;
        }
        if built.len() != expected {
            found.push(format!(
                "{}: registered as {expected} child(ren), built {} times: {:?}",
                bypass.id,
                built.len(),
                built.iter().map(|site| &site.shown).collect::<Vec<_>>()
            ));
        }
        if let Some(shape) = bypass.shape {
            for site in built {
                if site.shape != shape {
                    found.push(format!(
                        "{} at {}: the child is no longer the registered one.\n  \
                         registered: {shape}\n  built:      {}",
                        bypass.id, site.shown, site.shape
                    ));
                }
            }
        }
    }
    for (id, built) in &by_id {
        for site in built {
            found.push(format!(
                "{id} at {}: a child built outside git_carry::git and not registered",
                site.shown
            ));
        }
    }
    let others = sites.len().saturating_sub(sanctioned.len());
    if found.is_empty() && others != registered {
        found.push(format!(
            "{others} children outside the sanctioned builder, {registered} registered"
        ));
    }
    found
}

/// Every non-test call of the function `name` in `sources`, as (calling
/// function, the call within its enclosing call expression or statement).
fn calls_of(sources: &[Source], name: &str) -> Vec<(String, String)> {
    let mut calls = Vec::new();
    for source in sources {
        let file = &source.file;
        let code = &file.code;
        for at in tokens(code, name) {
            if !code[at + name.len()..].trim_start().starts_with('(')
                || code[..at].trim_end().ends_with("fn")
            {
                continue;
            }
            let start = statement_start(code, at);
            // Inside a call's parentheses: widen to the callee's name.
            let (from, to) = if start > 0 && code.as_bytes()[start - 1] == b'(' {
                let open = start - 1;
                let callee = code[..open]
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || "_:.".contains(c)))
                    .map_or(0, |x| x + 1);
                (callee, group_end(code, open))
            } else {
                (start, statement_end(code, at))
            };
            calls.push((file.path_at(&source.module, at), file.squeeze(from..to)));
        }
    }
    calls.sort();
    calls
}

/// The function named by a site id: `a::b::f("x")` is `f`.
fn function_of(id: &str) -> &str {
    let path = &id[..id.find('(').unwrap_or(id.len())];
    path.rsplit("::").next().unwrap_or(path)
}

/// Breaches of the registered callers of each builder function.
fn caller_breaches(sources: &[Source], bypasses: &[Bypass]) -> Vec<String> {
    let mut found = Vec::new();
    for bypass in bypasses.iter().filter(|bypass| !bypass.callers.is_empty()) {
        let krate = bypass.id.split("::").next().unwrap();
        let within: Vec<&Source> = sources
            .iter()
            .filter(|source| source.module.split("::").next() == Some(krate))
            .collect();
        let mut calls = Vec::new();
        for source in within {
            calls.extend(calls_of(
                std::slice::from_ref(source),
                function_of(bypass.id),
            ));
        }
        calls.sort();
        let mut registered: Vec<(String, String)> = bypass
            .callers
            .iter()
            .map(|(caller, call)| ((*caller).to_owned(), (*call).to_owned()))
            .collect();
        registered.sort();
        // Every call is a registered one. A registered call may be gone
        // only under carry_v2, which is frozen and being removed.
        let mut unmatched = registered;
        let mut unregistered = Vec::new();
        for call in &calls {
            match unmatched.iter().position(|entry| entry == call) {
                Some(at) => {
                    unmatched.remove(at);
                }
                None => unregistered.push(call),
            }
        }
        unmatched.retain(|(caller, _)| !caller.contains("::carry_v2::"));
        if !unregistered.is_empty() || !unmatched.is_empty() {
            found.push(format!(
                "{}: its callers changed.\n  not registered: {unregistered:#?}\n  \
                 registered and gone: {unmatched:#?}",
                bypass.id
            ));
        }
    }
    found
}

// ------------------------------------------------- what callers may not do

/// The agent's `git_env` table, read from the source.
struct Tables {
    /// The file that holds the table, and the table's span in it.
    shown: String,
    span: Range<usize>,
    /// `-c` overrides, as `key=value`.
    config: Vec<String>,
    /// Variables every Git child runs with.
    set: Vec<(String, String)>,
    /// Variables every Git child runs without.
    cleared: Vec<String>,
}

impl Tables {
    fn read(sources: &[Source]) -> Self {
        let source = sources
            .iter()
            .find(|source| source.module == "agent::git_carry")
            .expect("agent::git_carry");
        let file = &source.file;
        let table = file
            .modules
            .iter()
            .find(|block| block.name == "git_env")
            .expect("mod git_env");
        let item = |opener: &str| -> Vec<String> {
            let at = table.span.start
                + file.code[table.span.clone()]
                    .find(opener)
                    .unwrap_or_else(|| panic!("`{opener}` not found"));
            let end = at + file.code[at..].find("];").unwrap();
            file.literals
                .iter()
                .filter(|span| (at..end).contains(&span.start))
                .map(|span| file.literal(span).to_owned())
                .collect()
        };
        let set = item("const SET");
        Self {
            shown: source.shown.clone(),
            span: table.span.clone(),
            config: item("const CONFIG"),
            set: set
                .chunks_exact(2)
                .map(|pair| (pair[0].clone(), pair[1].clone()))
                .collect(),
            cleared: item("const CLEARED"),
        }
    }

    /// The table's `-c` keys, lower case (Git's keys ignore case).
    fn config_pairs(&self) -> Vec<(String, String)> {
        self.config
            .iter()
            .map(|entry| {
                let (key, value) = entry.split_once('=').unwrap();
                (key.to_ascii_lowercase(), value.to_ascii_lowercase())
            })
            .collect()
    }
}

/// The key of a literal that is one whole `section.key=value` assignment,
/// lower case.
fn assignment_key(literal: &str) -> Option<String> {
    let (key, _) = literal.split_once('=')?;
    let shaped = key.starts_with(|c: char| c.is_ascii_alphabetic())
        && key.contains('.')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_{}".contains(c));
    shaped.then(|| key.to_ascii_lowercase())
}

/// Every place `text` assigns `key` (`key=`) to something other than
/// `value`. Both are compared in lower case.
fn other_assignments(text: &str, key: &str, value: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let needle = format!("{key}=");
    let mut wrong = 0;
    let mut from = 0;
    while let Some(at) = lower[from..].find(&needle).map(|x| from + x) {
        from = at + needle.len();
        let starts_key = at == 0
            || !lower.as_bytes()[at - 1].is_ascii_alphanumeric()
                && !b"._-".contains(&lower.as_bytes()[at - 1]);
        let rest = &lower[from..];
        let same = rest.strip_prefix(value).is_some_and(|after| {
            after.is_empty()
                || after.starts_with(|c: char| c.is_whitespace() || "'\";)".contains(c))
        });
        if starts_key && !same {
            wrong += 1;
        }
    }
    wrong
}

/// The Git subcommands a shell line in `text` runs: the first word after
/// `git` (or after `g`, when `text` defines the probe's `g` function) that is
/// not an option.
fn shell_git_subcommands(text: &str) -> Vec<String> {
    let defines_g = text.contains("g() {");
    let mut found = Vec::new();
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        for (index, word) in words.iter().enumerate() {
            let program = word
                .rsplit(|c: char| "(=;|&`".contains(c))
                .next()
                .unwrap_or(word)
                .trim_start_matches(['\'', '"', '$', '{']);
            if program != "git" && !(defines_g && program == "g") {
                continue;
            }
            let mut next = index + 1;
            while let Some(argument) = words.get(next) {
                if *argument == "-c" || *argument == "-C" {
                    next += 2;
                } else if argument.starts_with('-') {
                    next += 1;
                } else {
                    found.push(
                        argument
                            .trim_matches(|c: char| "'\"();".contains(c))
                            .to_owned(),
                    );
                    break;
                }
            }
        }
    }
    found
}

/// One argument of a Git child as the source spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Arg {
    /// A string literal.
    Literal(String),
    /// One argument computed at run time.
    Expression,
    /// Any number of arguments computed at run time (`.args(list)`).
    Many,
}

/// Where a literal that spells a Git command stands.
#[derive(Debug, PartialEq, Eq)]
enum Position {
    /// The subcommand of a Git child: the first non-option literal of an
    /// argument array, or the sole argument of an `arg`/`args` call. With it,
    /// the arguments that follow, through every chained `arg`/`args`.
    Subcommand(Vec<Arg>),
    /// A later argument.
    Argument,
    /// Not an argument at all (a path, a pattern, a field name).
    Elsewhere,
}

/// The comma-separated elements of `file.code[from..to]`, outside any
/// bracket. An empty element (a trailing comma) is dropped.
fn elements(file: &Scanned, from: usize, to: usize) -> Vec<Range<usize>> {
    let code = &file.code;
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut start = from;
    let mut at = from;
    while at < to {
        match bytes[at] {
            b'(' | b'[' | b'{' => {
                at = group_end(code, at).min(to);
                continue;
            }
            b',' => {
                found.push(start..at);
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    found.push(start..to);
    found.retain(|range| {
        !code[range.clone()].trim().is_empty()
            || file
                .literals
                .iter()
                .any(|literal| range.contains(&literal.start))
    });
    found
}

/// `range` as one argument: a literal when it holds one string literal and
/// nothing else.
fn argument(file: &Scanned, range: &Range<usize>) -> Arg {
    let inside: Vec<&Range<usize>> = file
        .literals
        .iter()
        .filter(|literal| range.contains(&literal.start))
        .collect();
    match inside.as_slice() {
        [literal] if file.code[range.clone()].trim().is_empty() => {
            Arg::Literal(file.literal(literal).to_owned())
        }
        _ => Arg::Expression,
    }
}

/// The arguments of the `arg`/`args` call whose parenthesis opens at `open`.
fn call_arguments(file: &Scanned, open: usize, many: bool) -> Vec<Arg> {
    let code = &file.code;
    let close = group_end(code, open) - 1;
    let inner = code[open + 1..close].trim_start();
    let inner_start = close - inner.len();
    let array = inner
        .strip_prefix('&')
        .map_or(inner, str::trim_start)
        .starts_with('[');
    if array {
        let bracket = inner_start + code[inner_start..].find('[').unwrap();
        let end = group_end(code, bracket) - 1;
        elements(file, bracket + 1, end)
            .iter()
            .map(|range| argument(file, range))
            .collect()
    } else if many {
        vec![Arg::Many]
    } else {
        vec![argument(file, &(open + 1..close))]
    }
}

/// The `arg`/`args` calls chained after the call that closes at `close`
/// (other builder methods are stepped over).
fn chained_arguments(file: &Scanned, mut close: usize) -> Vec<Arg> {
    let code = &file.code;
    let mut found = Vec::new();
    loop {
        let rest = code[close..].trim_start();
        let Some(method) = rest.strip_prefix('.') else {
            return found;
        };
        let name = identifier(method.trim_start());
        let after = code.len() - method.trim_start().len() + name.len();
        if name.is_empty() || !code[after..].trim_start().starts_with('(') {
            return found;
        }
        let open = after + code[after..].find('(').unwrap();
        if name == "arg" || name == "args" {
            found.extend(call_arguments(file, open, name == "args"));
        }
        close = group_end(code, open);
    }
}

/// The name before the parenthesis at `open`, when it is a method call.
fn method_at(code: &str, open: usize) -> Option<&str> {
    let before = code[..open].trim_end();
    let start = before
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map_or(0, |x| x + 1);
    before[..start].ends_with('.').then(|| &before[start..])
}

fn position(file: &Scanned, literal: &Range<usize>) -> Position {
    let code = &file.code;
    let bytes = code.as_bytes();
    let Some(opener) = statement_start(code, literal.start).checked_sub(1) else {
        return Position::Elsewhere;
    };
    match bytes[opener] {
        b'[' => {
            let end = group_end(code, opener) - 1;
            let all = elements(file, opener + 1, end);
            let Some(index) = all.iter().position(|range| range.contains(&literal.start)) else {
                return Position::Elsewhere;
            };
            if argument(file, &all[index]) == Arg::Expression {
                return Position::Elsewhere;
            }
            // Before a subcommand stand only options, the values of `-c` and
            // `-C`, and run-time arguments.
            let mut value = false;
            for range in &all[..index] {
                match argument(file, range) {
                    Arg::Literal(_) if value => value = false,
                    Arg::Literal(option) if option.starts_with('-') => {
                        value = option == "-c" || option == "-C";
                    }
                    Arg::Literal(_) => return Position::Argument,
                    _ => value = false,
                }
            }
            if value {
                return Position::Argument;
            }
            let mut rest: Vec<Arg> = all[index + 1..]
                .iter()
                .map(|range| argument(file, range))
                .collect();
            // An array handed straight to `arg`/`args` continues down the
            // builder chain.
            let outer = statement_start(code, opener).checked_sub(1);
            if let Some(call) = outer.filter(|call| {
                bytes[*call] == b'(' && matches!(method_at(code, *call), Some("arg" | "args"))
            }) {
                rest.extend(chained_arguments(file, group_end(code, call)));
            }
            Position::Subcommand(rest)
        }
        b'(' if matches!(method_at(code, opener), Some("arg" | "args")) => {
            let close = group_end(code, opener);
            if argument(file, &(opener + 1..close - 1)) == Arg::Expression {
                return Position::Elsewhere;
            }
            Position::Subcommand(chained_arguments(file, close))
        }
        _ => Position::Elsewhere,
    }
}

/// Every way non-test agent code could undo the sanctioned builder's
/// contract or aim a writer at a repository without registering it.
#[allow(clippy::too_many_lines)]
fn contract_breaches(sources: &[Source], tables: &Tables) -> Vec<String> {
    let mut found = Vec::new();
    let config = tables.config_pairs();
    let guarded_env: Vec<(&str, &str)> = tables
        .set
        .iter()
        .filter(|(key, _)| key.starts_with("GIT_"))
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    // Uses of each WRITERS and each NOT_COMMANDS entry, keyed by the list.
    let mut counted: BTreeMap<(bool, &str), Vec<String>> = BTreeMap::new();
    for (extra, _) in EXTRA_CONFIG {
        let key = assignment_key(extra).unwrap_or_default();
        if config.iter().any(|(guarded, _)| *guarded == key) {
            found.push(format!("EXTRA_CONFIG {extra:?} overrides git_env::CONFIG"));
        }
    }
    for source in sources {
        let file = &source.file;
        for word in ["env_clear", "envs"] {
            for at in tokens(&file.code, word) {
                if file.code[..at].ends_with('.') {
                    found.push(format!(
                        "{}: `.{word}(` can drop or replace the Git environment",
                        source.at(at)
                    ));
                }
            }
        }
        let in_git_carry =
            source.module == "agent::git_carry" || source.module.starts_with("agent::git_carry::");
        for span in &file.literals {
            let literal = file.literal(span);
            let at = source.at(span.start);
            if source.shown == tables.shown && tables.span.contains(&span.start) {
                continue;
            }
            // Configuration: a whole assignment must be registered, and an
            // embedded one must repeat the table.
            if assignment_key(literal).is_some() {
                if !EXTRA_CONFIG.iter().any(|(extra, _)| *extra == literal) {
                    found.push(format!(
                        "{at}: {literal:?} assigns Git configuration outside git_env \
                         (the last -c wins); register it in EXTRA_CONFIG"
                    ));
                }
            } else {
                for (key, value) in &config {
                    if other_assignments(literal, key, value) > 0 {
                        found.push(format!(
                            "{at}: a literal assigns {key} to something other than \
                             git_env's {value}"
                        ));
                    }
                }
            }
            // Environment: the contract's variables and the ones that inject
            // configuration belong to git_env and the probe builder.
            let injects = INJECTING_ENV.contains(&literal)
                || literal.starts_with("GIT_CONFIG_KEY_")
                || literal.starts_with("GIT_CONFIG_VALUE_");
            let guarded = guarded_env.iter().any(|(key, _)| *key == literal);
            let function = file.path_at(&source.module, span.start);
            let injector = literal.starts_with("GIT_CONFIG_")
                && literal != "GIT_CONFIG_PARAMETERS"
                && CONFIG_INJECTORS.iter().any(|(id, _)| *id == function);
            if (injects || guarded)
                && !injector
                && function != "agent::git_carry::estimate::local_probe"
            {
                found.push(format!(
                    "{at}: {literal:?} is named outside git_env and the probe builder"
                ));
            }
            if !guarded {
                for (key, value) in &guarded_env {
                    if other_assignments(
                        literal,
                        &key.to_ascii_lowercase(),
                        &value.to_ascii_lowercase(),
                    ) > 0
                    {
                        found.push(format!(
                            "{at}: a literal sets {key} to something other than git_env's {value:?}"
                        ));
                    }
                }
            }
            if ["--config-env", "--exec-path", "--no-no-optional-locks"]
                .iter()
                .any(|flag| literal.starts_with(flag))
            {
                found.push(format!("{at}: {literal:?} overrides the Git contract"));
            }
            // Subcommands.
            if MAINTENANCE.contains(&literal) {
                found.push(format!("{at}: {literal:?} rewrites a repository's store"));
            } else if in_git_carry && GIT_COMMANDS.contains(&literal) {
                let function = file.path_at(&source.module, span.start);
                let mut count = |writer: bool, name: &str, what: &str| {
                    let list = if writer { WRITERS } else { NOT_COMMANDS };
                    match list.iter().find(|(entry, _, _)| *entry == name) {
                        Some((entry, _, _)) => {
                            counted.entry((writer, entry)).or_default().push(at.clone());
                        }
                        None => found.push(format!("{at}: {what}")),
                    }
                };
                match position(file, span) {
                    Position::Argument => {}
                    Position::Elsewhere => count(
                        false,
                        literal,
                        &format!(
                            "{literal:?} spells a Git command where it is not an \
                             argument, and is not in NOT_COMMANDS"
                        ),
                    ),
                    Position::Subcommand(rest) if READS.contains(&literal) => {
                        let spelled: Vec<String> = rest
                            .iter()
                            .map(|arg| match arg {
                                Arg::Literal(text) => text.clone(),
                                Arg::Expression => "<expr>".to_owned(),
                                Arg::Many => "<many>".to_owned(),
                            })
                            .collect();
                        let open = rest.is_empty() || rest.contains(&Arg::Many);
                        let registered = OPEN_FORMS
                            .iter()
                            .any(|(id, sub, _)| *id == function && *sub == literal);
                        if !MULTI_FORM.contains(&literal) || read_form(literal, &spelled).is_ok() {
                            if rest.contains(&Arg::Many)
                                && MULTI_FORM.contains(&literal)
                                && !registered
                            {
                                found.push(format!(
                                    "{at}: `git {literal}` takes arguments computed at run \
                                     time; register the function in OPEN_FORMS: {function}"
                                ));
                            }
                        } else if open {
                            if !registered {
                                found.push(format!(
                                    "{at}: `git {literal}` is completed elsewhere; register \
                                     the function in OPEN_FORMS: {function}"
                                ));
                            }
                        } else {
                            count(
                                true,
                                &format!("{literal} (writing form)"),
                                &format!(
                                    "`git {literal} {}` is a writing form and is not in WRITERS",
                                    spelled.join(" ")
                                ),
                            );
                        }
                    }
                    Position::Subcommand(_) => count(
                        true,
                        literal,
                        &format!(
                            "Git subcommand {literal:?} is neither a registered read \
                             (READS) nor a registered writer (WRITERS)"
                        ),
                    ),
                }
            }
            if literal.contains(char::is_whitespace) {
                for sub in shell_git_subcommands(literal) {
                    if GIT_COMMANDS.contains(&sub.as_str()) && !READS.contains(&sub.as_str()) {
                        found.push(format!(
                            "{at}: a shell line runs `git {sub}`, which is not a registered read"
                        ));
                    }
                }
            }
        }
    }
    for (writer, (name, ceiling, _)) in WRITERS
        .iter()
        .map(|entry| (true, entry))
        .chain(NOT_COMMANDS.iter().map(|entry| (false, entry)))
    {
        let places = counted.get(&(writer, *name)).map_or(&[][..], Vec::as_slice);
        if places.len() > *ceiling {
            found.push(format!(
                "{name:?}: {} uses, ceiling {ceiling} (a ceiling only shrinks; a new \
                 writer is argued on bulkload#188): {places:?}",
                places.len()
            ));
        }
    }
    found
}

/// Every non-test agent function that returns a `Command`, with whether it
/// is visible outside its parent module.
fn builders(sources: &[Source]) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for source in sources {
        let code = &source.file.code;
        for block in &source.file.functions {
            let signature = &code[block.span.start..block.open];
            let returns = signature
                .find("->")
                .is_some_and(|arrow| !tokens(&signature[arrow..], "Command").is_empty());
            if !returns {
                continue;
            }
            let head = &code[statement_start(code, block.span.start)..block.span.start];
            let public = tokens(head, "pub").into_iter().any(|at| {
                let scope = head[at + 3..].trim_start();
                !(scope.starts_with("(super)") || scope.starts_with("(self)"))
            });
            found.push((source.file.path_at(&source.module, block.open + 1), public));
        }
    }
    found.sort();
    found
}

// --------------------------------------------------------------- the tests

fn synthetic(module: &str, text: &str) -> Source {
    Source {
        module: module.to_owned(),
        shown: format!("{module}.rs"),
        file: Scanned::new(text),
    }
}

fn ids(source: &Source) -> Vec<String> {
    sites_in(source).into_iter().map(|site| site.id).collect()
}

/// The scan finds a child in non-test code, names it by module, inline
/// module, function and program, and ignores prose, literals and test items.
#[test]
fn the_scan_finds_a_new_child_and_skips_test_code() {
    let code = "use std::process::Command;\n\
        fn git(repo: &Path) -> Command { let mut c = Command::new(\"git\"); c }\n\
        pub fn sneaky(repo: &Path) -> bool {\n\
            let probe = |x: u8| x + 1;\n\
            Command::new(\"git\").arg(\"status\").status().is_ok()\n\
        }\n\
        // Command::new(\"in-a-comment\")\n\
        const S: &str = \"Command::new(\\\"in-a-literal\\\")\";\n\
        const R: &str = r#\"Command::new(\"in-a-raw-literal\")\"#;\n\
        #[cfg(test)]\nmod tests { fn t() { Command::new(\"in-a-test\"); } }\n\
        #[cfg(all(test, feature = \"io-trace\"))]\nfn traced() { Command::new(\"traced\"); }\n\
        #[cfg(test)]\n#[allow(clippy::panic)]\nmod more_tests;\n\
        fn sized(buffer: [u8; 4]) { let _ = ['{', '}']; Command::new(program); }\n\
        mod inner { pub mod deeper { fn nested() { std::process::Command::new(\"ssh\"); } } }\n\
        fn bracketed() { let make = <Command>::new; <std::process::Command>::new(\"sh\"); }\n";
    let source = synthetic("m", code);
    assert_eq!(
        ids(&source),
        [
            "m::git(\"git\")",
            "m::sneaky(\"git\")",
            "m::sized(program)",
            "m::inner::deeper::nested(\"ssh\")",
            "m::bracketed(<fn pointer>)",
            "m::bracketed(\"sh\")",
        ]
    );
    assert_eq!(source.file.test_modules, ["more_tests"]);
    let literals: Vec<&str> = source
        .file
        .literals
        .iter()
        .map(|span| source.file.literal(span))
        .collect();
    assert!(literals.contains(&"status"), "{literals:?}");
    assert!(!literals.contains(&"in-a-test"), "{literals:?}");
    assert!(evasions(&source).is_empty(), "{:?}", evasions(&source));
    let sites = sites_in(&source);
    assert_eq!(
        sites[1].shape,
        "Command::new(\"git\").arg(\"status\").status()"
    );
    assert_eq!(sites[0].shape, "Command::new(\"git\");c}");
}

/// A `#[cfg(test)]` on something that is not an item (an enum variant, a
/// struct field, a statement, a parameter) hides only that thing, never the
/// production item after it.
#[test]
fn a_test_attribute_on_a_variant_or_field_hides_nothing_after_it() {
    let code = "pub enum Sneak { A, #[cfg(test)] TestOnly, B }\n\
        pub fn after_variant(r: &Path) -> bool { std::process::Command::new(\"git\").status().is_ok() }\n\
        pub struct Holder { a: u8, #[cfg(test)] pub hook: Option<u8>, b: u8 }\n\
        pub fn after_field() { Command::new(\"ssh\"); }\n\
        pub fn statements(#[cfg(test)] extra: u8) {\n\
            #[cfg(test)]\n\
            let hidden = Command::new(\"in-a-test-statement\");\n\
            #[cfg(test)]\n\
            for stream in [1, 2] { Command::new(\"in-a-test-loop\"); }\n\
            Command::new(\"bash\");\n\
            match x { #[cfg(test)] 1 => { Command::new(\"in-a-test-arm\"); } _ => { Command::new(\"sh\"); } }\n\
        }\n\
        #[cfg(test)]\n\
        const FIXTURE: [u8; 2] = [1, 2];\n\
        pub fn after_const() { Command::new(\"gzip\"); }\n\
        #[cfg(test)]\n\
        fn helper(buffer: [u8; 4]) { Command::new(\"in-a-test-fn\"); }\n\
        pub fn last() { Command::new(\"zstd\"); }\n";
    assert_eq!(
        ids(&synthetic("m", code)),
        [
            "m::after_variant(\"git\")",
            "m::after_field(\"ssh\")",
            "m::statements(\"bash\")",
            "m::statements(\"sh\")",
            "m::after_const(\"gzip\")",
            "m::last(\"zstd\")",
        ]
    );
}

/// A renamed `Command`, and every way of starting a child without one, is a
/// breach on its own: the registry cannot count what it cannot name.
#[test]
fn a_renamed_command_or_a_raw_spawn_is_refused() {
    for (code, expected) in [
        (
            "use std::process::Command as Cmd;\nfn f() { Cmd::new(\"git\"); }",
            "renames",
        ),
        (
            "use std::process::{Command as Cmd, Stdio};\nfn f() { Cmd::new(\"git\"); }",
            "renames",
        ),
        ("type Cmd = std::process::Command;\nfn f() { Cmd::new(\"git\"); }", "alias"),
        ("pub(crate) type Cmd = Command;", "alias"),
        (
            "trait Make { fn make() -> Self; }\nimpl Make for Command { fn make() -> Self { Self::new(\"git\") } }",
            "impl",
        ),
        ("fn f() { unsafe { libc::system(c\"git gc\".as_ptr()) }; }", "system"),
        ("fn f() { unsafe { libc::fork() }; }", "fork"),
        ("fn f() { unsafe { libc::vfork() }; }", "vfork"),
        ("fn f() { unsafe { libc::execvp(a, b) }; }", "execvp"),
        ("fn f() { unsafe { libc::posix_spawnp(a, b, c, d, e, f) }; }", "posix_spawnp"),
        ("fn f() { unsafe { libc::syscall(libc::SYS_execve, a, b, c) }; }", "SYS_execve"),
        ("fn f() { unsafe { libc::clone(a, b, c, d) }; }", "libc::clone"),
        ("use std::os::unix::process::CommandExt as _;", "CommandExt"),
        ("fn f(mut c: X) { let _ = c.exec(); }", ".exec("),
        ("extern \"C\" { #[link_name = \"system\"] fn run(c: *const u8) -> i32; }", "link_name"),
    ] {
        let found = evasions(&synthetic("m", code));
        assert!(
            found.iter().any(|breach| breach.contains(expected)),
            "{code:?} was not refused as {expected:?}: {found:?}"
        );
    }
    // Ordinary uses are not refused.
    let fine = "use std::process::{Command, Stdio};\n\
        use std::process::Command as _;\n\
        fn run(command: &mut Command) -> Command { let copy = name.clone(); todo!() }\n\
        impl From<Command> for Wrapper { fn from(c: Command) -> Self { Self(c) } }\n";
    let found = evasions(&synthetic("m", fine));
    assert!(found.is_empty(), "{found:?}");
}

/// A file is test code because its parent gates it, never because of its
/// name: `tests.rs` declared without `#[cfg(test)]` is scanned.
#[test]
fn a_file_named_tests_is_scanned_unless_its_parent_gates_it() {
    let root = scratch("tests-rs");
    let src = root.0.join("src");
    fs::create_dir_all(src.join("walk")).unwrap();
    fs::create_dir_all(src.join("gated/deeper")).unwrap();
    fs::create_dir_all(src.join("both")).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod walk;\npub mod gated;\npub mod both;\n",
    )
    .unwrap();
    fs::write(src.join("walk.rs"), "pub mod tests;\n").unwrap();
    fs::write(
        src.join("walk/tests.rs"),
        "pub fn f() { std::process::Command::new(\"git\"); }\n",
    )
    .unwrap();
    fs::write(
        src.join("gated.rs"),
        "#[cfg(test)]\nmod tests;\n#[cfg(test)]\nmod deeper;\n",
    )
    .unwrap();
    fs::write(
        src.join("gated/tests.rs"),
        "fn t() { std::process::Command::new(\"fixture\"); }\n",
    )
    .unwrap();
    fs::write(
        src.join("gated/deeper/more.rs"),
        "fn t() { std::process::Command::new(\"fixture\"); }\n",
    )
    .unwrap();
    // Declared for tests and again for production: not test-only.
    fs::write(
        src.join("both.rs"),
        "#[cfg(test)]\nmod tests;\n#[cfg(not(test))]\nmod tests;\n",
    )
    .unwrap();
    fs::write(
        src.join("both/tests.rs"),
        "fn g() { std::process::Command::new(\"ssh\"); }\n",
    )
    .unwrap();
    let sources = crate_sources("x", &src);
    let found: Vec<String> = all_sites(&sources)
        .into_iter()
        .map(|site| site.id)
        .collect();
    assert_eq!(
        found,
        ["x::both::tests::g(\"ssh\")", "x::walk::tests::f(\"git\")"]
    );
}

/// The registry counts children, not names: a second child with a
/// registered id, a changed child, an unregistered one and a stale entry
/// each fail.
#[test]
fn the_registry_counts_children_and_pins_their_text() {
    const ENTRY: &[Bypass] = &[Bypass {
        id: "agent::main::pull_command(\"ssh\")",
        shape: Some("Command::new(\"ssh\").arg(host).arg(\"serve\").spawn()"),
        callers: &[],
        why: "self-test",
    }];
    let sanctioned = "fn git(repo: &Path) -> Command { let mut c = Command::new(\"git\"); c }\n";
    let good = "fn pull_command() { Command::new(\"ssh\").arg(host).arg(\"serve\").spawn(); }\n";
    let check = |main: &str| {
        let sources = [
            synthetic("agent::git_carry", sanctioned),
            synthetic("agent::main", main),
        ];
        registry_breaches(&all_sites(&sources), ENTRY)
    };
    assert!(check(good).is_empty(), "{:?}", check(good));
    // A second ssh child riding the same id (the reviewed survivor).
    let twice = "fn pull_command() {\n\
        let _ = Command::new(\"ssh\").arg(\"host\").arg(\"git -C /src update-index --refresh\").status();\n\
        Command::new(\"ssh\").arg(host).arg(\"serve\").spawn(); }\n";
    let found = check(twice);
    assert!(
        found.iter().any(|b| b.contains("built 2 times")),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|b| b.contains("no longer the registered one")),
        "{found:?}"
    );
    // The same id in an inline module is a different child.
    let nested = "fn pull_command() { Command::new(\"ssh\").arg(host).arg(\"serve\").spawn(); }\n\
        mod extra { fn pull_command() { Command::new(\"ssh\").arg(host).arg(\"serve\").spawn(); } }\n";
    let found = check(nested);
    assert!(
        found
            .iter()
            .any(|b| b.contains("agent::main::extra::pull_command(\"ssh\")")
                && b.contains("not registered")),
        "{found:?}"
    );
    // One child, same id, another argument.
    let changed =
        "fn pull_command() { Command::new(\"ssh\").arg(host).arg(\"git gc\").spawn(); }\n";
    let found = check(changed);
    assert!(
        found
            .iter()
            .any(|b| b.contains("no longer the registered one")),
        "{found:?}"
    );
    // A variable named like a registered program, in another function.
    let other = format!("{good}fn hydrate() {{ let program = \"git\"; Command::new(program); }}\n");
    let found = check(&other);
    assert!(
        found.iter().any(|b| b.contains("not registered")),
        "{found:?}"
    );
    // The entry outlives its child.
    let found = check("fn nothing() {}\n");
    assert!(
        found.iter().any(|b| b.contains("no longer exists")),
        "{found:?}"
    );
    // A second sanctioned builder.
    let sources = [
        synthetic(
            "agent::git_carry",
            &format!("{sanctioned}mod x {{ }}\n{sanctioned}"),
        ),
        synthetic("agent::main", good),
    ];
    let found = registry_breaches(&all_sites(&sources), ENTRY);
    assert!(
        found.iter().any(|b| b.contains("exactly one sanctioned")),
        "{found:?}"
    );
}

/// The scan reaches every crate and drops only gated test files: it sees
/// the sanctioned builder, the operator tools, and none of the many fixture
/// children in tests.
#[test]
fn the_scan_reads_the_workspace_and_drops_test_files() {
    let sources = workspace_sources();
    let names: BTreeSet<&str> = sources
        .iter()
        .map(|source| source.module.as_str())
        .collect();
    for module in [
        "agent::main",
        "agent::lib",
        "agent::git_carry",
        "agent::git_carry::estimate",
        "agent::transfer",
        "agent::walk",
        "agent::estate",
        "bench::main",
        "handoff::lib",
        "proto::frame",
    ] {
        assert!(names.contains(module), "{module} not scanned: {names:?}");
    }
    for module in [
        "agent::git_carry::source_inert_tests",
        "agent::git_carry::refs_scale_tests",
        "agent::io::tests",
        "agent::transfer::tests",
        "agent::materialize::adoption_power_loss",
        "proto::frame::tests",
    ] {
        assert!(!names.contains(module), "test module {module} scanned");
    }
    assert!(all_sites(&sources).iter().any(|site| site.id == SANCTIONED));
}

/// P76: every child any crate builds outside tests is the sanctioned Git
/// builder or a registered, argued bypass, built exactly as registered and
/// exactly as often. A new `Command::new` anywhere in non-test code fails
/// here, under any name and in any crate.
#[test]
fn every_child_goes_through_the_source_safe_builder() {
    let sources = workspace_sources();
    let evaded: Vec<String> = sources.iter().flat_map(evasions).collect();
    assert!(
        evaded.is_empty(),
        "children the registry cannot name (S2, OI-1003-Q16): {evaded:#?}"
    );
    let found = registry_breaches(&all_sites(&sources), BYPASSES);
    assert!(
        found.is_empty(),
        "child processes outside the registry (S2, OI-1003-Q16): build Git \
         children with git_carry::git; a bypass is argued on bulkload#188, \
         never added here:\n{}",
        found.join("\n")
    );
}

/// The builder functions on the bypass list are called only where
/// registered, with only the registered arguments: the probe children run
/// the probe script and nothing else.
#[test]
fn the_bypass_builders_are_called_only_as_registered() {
    let sources = workspace_sources();
    let found = caller_breaches(&sources, BYPASSES);
    assert!(found.is_empty(), "{}", found.join("\n"));

    let estimate = sources
        .iter()
        .find(|source| source.module == "agent::git_carry::estimate")
        .unwrap();
    let file = &estimate.file;
    let body = |name: &str| {
        let block = file
            .functions
            .iter()
            .find(|block| block.name == name)
            .unwrap_or_else(|| panic!("fn {name}"));
        file.squeeze(block.open..block.span.end)
    };
    // The remote line is the probe's bash and nothing else.
    assert_eq!(
        body("remote_command"),
        "{format!(\"env LC_ALL=C LANGUAGE= GIT_NO_LAZY_FETCH=1 bash -s -- '{path}'\")}"
    );
    // The probe's stdin is the probe script and nothing else.
    let probe = body("run_probe");
    assert_eq!(probe.matches("write_all(").count(), 1, "{probe}");
    assert!(
        probe.contains("stdin.write_all(PROBE_SCRIPT.as_bytes())"),
        "{probe}"
    );
}

#[test]
fn the_bypass_list_only_shrinks() {
    let ids: BTreeSet<&str> = BYPASSES.iter().map(|bypass| bypass.id).collect();
    assert_eq!(ids.len(), BYPASSES.len(), "duplicate bypass ids");
    let frozen: BTreeSet<&str> = FROZEN.iter().copied().collect();
    assert_eq!(frozen.len(), 17, "FROZEN never grows (17 at P76)");
    let added: Vec<&&str> = ids.difference(&frozen).collect();
    assert!(
        added.is_empty(),
        "bypasses that were not on the list at P76 (bulkload#188): {added:?}"
    );
    for bypass in BYPASSES {
        assert!(
            bypass.why.len() > 40,
            "{}: say why the bypass is tolerated",
            bypass.id
        );
        if bypass.id.starts_with("agent::") || bypass.id.ends_with("(\"git\")") {
            assert!(
                bypass.shape.is_some(),
                "{}: an agent child and a Git child pin their text",
                bypass.id
            );
        }
    }
}

/// The sanctioned builder holds the S2 contract (#145): optional locks off
/// twice over, hooks, fsmonitor, automatic gc and maintenance off, no lazy
/// fetch, no prompt, no system or global config, the redirecting variables
/// cleared, and a discovery ceiling.
#[test]
fn the_source_safe_builder_holds_the_s2_contract() {
    let sources = agent_sources();
    let tables = Tables::read(&sources);
    for required in [
        "core.hooksPath=/dev/null",
        "core.fsmonitor=false",
        "gc.auto=0",
        "maintenance.auto=false",
    ] {
        assert!(
            tables.config.iter().any(|entry| entry == required),
            "git_env::CONFIG lacks {required}"
        );
    }
    let pairs: BTreeMap<&str, &str> = tables
        .set
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    for (key, value) in [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
    ] {
        assert_eq!(pairs.get(key), Some(&value), "git_env::SET {key}");
    }
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        assert!(
            tables.cleared.iter().any(|entry| entry == key),
            "git_env::CLEARED lacks {key}"
        );
    }

    let source = sources
        .iter()
        .find(|source| source.module == "agent::git_carry")
        .unwrap();
    let file = &source.file;
    let builder = file
        .functions
        .iter()
        .find(|block| block.name == "git")
        .expect("fn git");
    let body = &file.code[builder.span.clone()];
    assert!(
        body.replace(char::is_whitespace, "")
            .starts_with("fngit(repo:&Path)->Command{"),
        "git_carry::git changed its signature"
    );
    for uses in [
        "git_env::CLEARED",
        "env_remove",
        "git_env::CONFIG",
        "git_env::SET",
    ] {
        assert!(body.contains(uses), "git_carry::git does not use {uses}");
    }
    let literals: Vec<&str> = file
        .literals
        .iter()
        .filter(|span| builder.span.contains(&span.start))
        .map(|span| file.literal(span))
        .collect();
    assert_eq!(
        literals,
        [
            "git",
            "--no-optional-locks",
            "-c",
            "-C",
            "GIT_CEILING_DIRECTORIES"
        ],
        "git_carry::git's own arguments changed"
    );
}

/// No agent code undoes the builder's contract after taking the command:
/// no `env_clear`/`envs`, no later `-c` on a guarded key, no contract
/// variable set elsewhere, no store-rewriting subcommand, and no Git
/// subcommand that is not a registered read or a registered writer.
#[test]
fn no_caller_undoes_the_contract_or_runs_an_unregistered_writer() {
    let sources = agent_sources();
    let tables = Tables::read(&sources);
    let found = contract_breaches(&sources, &tables);
    assert!(
        found.is_empty(),
        "S2 contract breaches:\n{}",
        found.join("\n")
    );
    let known: BTreeSet<&str> = GIT_COMMANDS.iter().copied().collect();
    for name in READS.iter().chain(MAINTENANCE).chain(MULTI_FORM) {
        assert!(known.contains(name), "{name} is not a Git command");
    }
    for name in MULTI_FORM {
        assert!(READS.contains(name), "{name} has no read form registered");
    }
    for (name, _, why) in WRITERS {
        let (command, form) = name
            .strip_suffix(" (writing form)")
            .map_or((*name, false), |command| (command, true));
        assert!(known.contains(command), "{name} is not a Git command");
        assert_eq!(
            MULTI_FORM.contains(&command),
            form,
            "{name}: only a subcommand with a read form has a writing form"
        );
        assert_eq!(
            READS.contains(&command),
            form,
            "{name}: a read and a writer"
        );
        assert!(!MAINTENANCE.contains(&command), "{name} is maintenance");
        assert!(why.len() > 20, "{name}: say where the writer is aimed");
    }
    for (name, _, what) in NOT_COMMANDS {
        assert!(known.contains(name), "{name} is not a Git command");
        assert!(what.len() > 10, "{name}: say what the literal is");
    }
}

/// The reviewed survivors: each way a caller could undo the contract
/// through the sanctioned builder is refused on its own.
#[test]
fn each_way_of_undoing_the_contract_is_refused() {
    let sources = agent_sources();
    let tables = Tables::read(&sources);
    for (code, expected) in [
        ("fn f(r: &Path) { let mut c = git(r); c.env_clear(); }", "env_clear"),
        ("fn f(r: &Path) { git(r).envs(extra); }", "envs"),
        (
            "fn f(r: &Path) { git(r).args([\"-c\", \"core.hooksPath=.git/hooks\"]); }",
            "core.hooksPath=.git/hooks",
        ),
        (
            "fn f(r: &Path) { git(r).args([\"-c\", \"GC.AUTO=1\"]); }",
            "GC.AUTO=1",
        ),
        (
            "fn f(r: &Path) { git(r).args([\"-c\", \"include.path=/elsewhere\"]); }",
            "include.path",
        ),
        (
            "fn f(r: &Path) { git(r).env(\"GIT_OPTIONAL_LOCKS\", \"1\"); }",
            "GIT_OPTIONAL_LOCKS",
        ),
        (
            "fn f(r: &Path) { git(r).env_remove(\"GIT_CONFIG_GLOBAL\"); }",
            "GIT_CONFIG_GLOBAL",
        ),
        (
            "fn f(r: &Path) { git(r).env(\"GIT_CONFIG_COUNT\", \"1\").env(\"GIT_CONFIG_KEY_0\", k); }",
            "GIT_CONFIG_KEY_0",
        ),
        (
            "fn f(r: &Path) { git(r).arg(\"--config-env=gc.auto=X\"); }",
            "--config-env",
        ),
        (
            "fn f(r: &Path) { git(r).args([\"update-index\", \"--really-refresh\"]); }",
            "update-index",
        ),
        (
            "fn f(r: &Path) { git(r).args([\"reflog\", \"expire\", \"--all\"]); }",
            "reflog",
        ),
        ("fn f(r: &Path) { git(r).args([\"gc\", \"--auto\"]); }", "\"gc\""),
        ("fn f(r: &Path) { git(r).arg(\"pack-refs\"); }", "pack-refs"),
        (
            "const S: &str = \"git -C \\\"$1\\\" update-index --refresh\";",
            "git update-index",
        ),
        (
            "const S: &str = \"set -eu\\nexport GIT_OPTIONAL_LOCKS=1\\ngit status\";",
            "GIT_OPTIONAL_LOCKS",
        ),
        (
            "const S: &str = \"git -c gc.auto=1 -c core.fsmonitor=true status\";",
            "gc.auto",
        ),
    ] {
        let mut with = agent_sources();
        with.push(synthetic("agent::git_carry::sneak", code));
        let found: Vec<String> = contract_breaches(&with, &tables)
            .into_iter()
            .filter(|breach| breach.starts_with("agent::git_carry::sneak.rs"))
            .collect();
        assert!(
            found.iter().any(|breach| breach.contains(expected)),
            "{code:?} was not refused as {expected:?}: {found:?}"
        );
    }
    assert!(contract_breaches(&sources, &tables).is_empty());
    assert_eq!(
        shell_git_subcommands("g() { git --no-optional-locks -c a.b=c \"$@\"; }\nif head=$(g --git-dir=\"$x\" rev-parse -q HEAD); then :; fi\nversion=$(git version) || exit 5"),
        ["$@", "rev-parse", "version"]
    );
}

/// No function hands a Git builder to code outside the `git_carry` tree,
/// and every function that returns a `Command` is a registered one.
#[test]
fn the_command_builders_are_registered_and_private() {
    let found = builders(&agent_sources());
    let unregistered: Vec<&String> = found
        .iter()
        .map(|(id, _)| id)
        .filter(|id| !BUILDERS.contains(&id.as_str()))
        .collect();
    assert!(
        unregistered.is_empty(),
        "functions that return a Command and are not in BUILDERS: {unregistered:#?}"
    );
    let public: Vec<&String> = found
        .iter()
        .filter(|(_, public)| *public)
        .map(|(id, _)| id)
        .collect();
    assert!(
        public.is_empty(),
        "a Command builder visible outside its parent module: {public:#?}"
    );
}
// ------------------------------------------------------- the dynamic check

/// A private scratch root, removed on drop.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Root {
    let root = std::env::temp_dir().join(format!(
        "bulkload-p76-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    Root(root.canonicalize().unwrap())
}

/// A fixture Git child (test identity, no user configuration).
fn fixture_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=Test", "-c", "user.email=test@localhost"])
        .args(["-c", "commit.gpgsign=false", "-C"])
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// The first executable `git` on `PATH`.
fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|candidate| {
            fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .expect("git on PATH")
}

/// One recorded Git child: its arguments and the S2 environment it saw.
#[derive(Debug)]
struct Recorded {
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

const RECORDED_ENV: &[&str] = &[
    "GIT_OPTIONAL_LOCKS",
    "GIT_NO_LAZY_FETCH",
    "GIT_TERMINAL_PROMPT",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "PWD",
];

/// Install a `git` wrapper in `bin` that appends one line per call to `log`
/// (tab-separated: the recorded environment, `--`, then the arguments) and
/// then runs the real Git with the same arguments. `PWD` is the shell's own
/// (the child's working directory).
fn install_wrapper(bin: &Path, log: &Path) {
    fs::create_dir_all(bin).unwrap();
    let mut env = String::new();
    for key in RECORDED_ENV {
        let _ = write!(env, "{key}=${{{key}-<unset>}}\t");
    }
    let script = format!(
        "#!/bin/sh\n\
         keys=$(env | sed -n 's/^GIT_CONFIG_KEY_[0-9]*=//p' | tr '\\n' ' ')\n\
         line=\"{env}GIT_CONFIG_KEYS=$keys\t--\"\n\
         for arg in \"$@\"; do line=\"$line\t$arg\"; done\n\
         printf '%s\\n' \"$line\" >> '{log}'\n\
         exec '{git}' \"$@\"\n",
        env = env,
        log = log.display(),
        git = real_git().display(),
    );
    let wrapper = bin.join("git");
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
}

fn read_log(log: &Path) -> Vec<Recorded> {
    let text = fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let mut env = BTreeMap::new();
            for field in fields.by_ref() {
                if field == "--" {
                    break;
                }
                let (key, value) = field.split_once('=').unwrap();
                env.insert(key.to_owned(), value.to_owned());
            }
            Recorded {
                args: fields.map(str::to_owned).collect(),
                env,
            }
        })
        .collect()
}

/// The subcommand of a recorded call: the first argument that is neither an
/// option nor an option's value.
fn subcommand(args: &[String]) -> Option<&str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" | "-C" => {
                iter.next();
            }
            option if option.starts_with('-') => {}
            command => return Some(command),
        }
    }
    None
}

/// The options before a recorded call's subcommand.
fn global_options(args: &[String]) -> &[String] {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "-c" | "-C" => index += 2,
            option if option.starts_with('-') => index += 1,
            _ => break,
        }
    }
    &args[..index.min(args.len())]
}

/// The S2 `-c` overrides every Git child carries, as the oracle reads them.
const CONTRACT_CONFIG: &[(&str, &str)] = &[
    ("core.hookspath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("gc.auto", "0"),
    ("maintenance.auto", "false"),
];

/// A recorded variable's value, when it was set.
fn recorded<'a>(call: &'a Recorded, key: &str) -> Option<&'a str> {
    call.env
        .get(key)
        .map(String::as_str)
        .filter(|value| *value != "<unset>")
}

/// Where a recorded call is aimed: the directory it runs in (`-C`, else its
/// working directory), the repository it opens (`GIT_DIR`, else
/// `--git-dir=`, else that directory) and its worktree, if redirected.
fn aim(call: &Recorded) -> (PathBuf, PathBuf, Option<PathBuf>) {
    let globals = global_options(&call.args);
    let mut directory = PathBuf::from(recorded(call, "PWD").unwrap_or("/"));
    for pair in globals.windows(2) {
        if pair[0] == "-C" {
            directory = directory.join(&pair[1]);
        }
    }
    let git_dir = recorded(call, "GIT_DIR").map(PathBuf::from).or_else(|| {
        globals
            .iter()
            .rev()
            .find_map(|arg| arg.strip_prefix("--git-dir="))
            .map(PathBuf::from)
    });
    let repository = git_dir.map_or_else(|| directory.clone(), |path| directory.join(path));
    let worktree = recorded(call, "GIT_WORK_TREE").map(|path| directory.join(path));
    (directory, repository, worktree)
}

/// Every breach of the S2 contract in one recorded call. `protected` are the
/// roots no child may write: a child aimed at one runs a registered read.
// One oracle, one list of rules.
#[allow(clippy::too_many_lines)]
fn breaches(call: &Recorded, protected: &[&Path]) -> Vec<String> {
    let mut out = Vec::new();
    let sub = subcommand(&call.args);
    for (key, value) in [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ] {
        if call.env.get(key).map(String::as_str) != Some(value) {
            out.push(format!("{key}={:?}", call.env.get(key)));
        }
    }
    if let Some(value) = recorded(call, "GIT_CONFIG_PARAMETERS") {
        out.push(format!(
            "GIT_CONFIG_PARAMETERS={value:?} injects configuration"
        ));
    }
    // `GIT_CONFIG_COUNT` may only switch filter drivers off (`nest_status`).
    if recorded(call, "GIT_CONFIG_COUNT").is_some() {
        let keys = recorded(call, "GIT_CONFIG_KEYS").unwrap_or_default();
        if let Some(key) = keys
            .split_whitespace()
            .find(|key| !key.to_ascii_lowercase().starts_with("filter."))
        {
            out.push(format!("GIT_CONFIG_KEY injects {key}"));
        }
        if keys.trim().is_empty() {
            out.push("GIT_CONFIG_COUNT with no recorded key".to_owned());
        }
    }
    if let Some(sub) = sub.filter(|sub| MAINTENANCE.contains(sub)) {
        out.push(format!("maintenance subcommand {sub}"));
    }
    let globals = global_options(&call.args);
    if let Some(flag) = globals
        .iter()
        .find(|arg| arg.starts_with("--config-env") || arg.starts_with("--exec-path"))
    {
        out.push(format!("{flag} overrides the contract"));
    }
    let configs: Vec<(String, &str)> = globals
        .windows(2)
        .filter(|pair| pair[0] == "-c")
        .filter_map(|pair| pair[1].split_once('='))
        .map(|(key, value)| (key.to_ascii_lowercase(), value))
        .collect();
    // The last `-c` wins, so every assignment of a guarded key must be the
    // contract's, wherever it stands.
    for (key, value) in &configs {
        if let Some((_, required)) = CONTRACT_CONFIG.iter().find(|(guarded, _)| guarded == key) {
            if value != required {
                out.push(format!("-c {key}={value} overrides the contract"));
            }
        }
    }
    // `git version` reads no repository; every other call carries the flags.
    if sub != Some("version") {
        for (key, value) in CONTRACT_CONFIG {
            if !configs.iter().any(|(k, v)| k == key && v == value) {
                out.push(format!("no -c {key}={value}"));
            }
        }
        if call.args.first().map(String::as_str) != Some("--no-optional-locks") {
            out.push("no leading --no-optional-locks".to_owned());
        }
    }
    // A child aimed at a protected root runs a read (OI-1003-Q16). One that
    // opens a private repository over a protected worktree (the capture's
    // snapshot) must also keep its index outside the root.
    let (directory, repository, worktree) = aim(call);
    let inside = |path: &Path| protected.iter().any(|root| path.starts_with(root));
    let opens = inside(&repository);
    let reads_worktree = worktree.as_deref().is_some_and(inside) || (!opens && inside(&directory));
    if opens || reads_worktree {
        let rest = &call.args[global_options(&call.args).len().min(call.args.len())..];
        let rest = rest.get(1..).unwrap_or_default();
        match sub {
            Some(sub) if READS.contains(&sub) => {
                if let Err(why) = read_form(sub, rest) {
                    out.push(format!(
                        "`{sub}` in a writing form ({why}), aimed at a protected root"
                    ));
                }
                // `bundle create FILE` writes FILE.
                if let ("bundle", Some("create"), Some(file)) = (
                    sub,
                    rest.iter()
                        .map(String::as_str)
                        .find(|arg| !arg.starts_with('-')),
                    rest.iter().filter(|arg| !arg.starts_with('-')).nth(1),
                ) {
                    if file != "-" && inside(&directory.join(file)) {
                        out.push(format!(
                            "bundle create writes {file} inside a protected root"
                        ));
                    }
                }
            }
            Some(sub) if !opens && SNAPSHOT_WRITERS.contains(&sub) => {}
            other => out.push(format!(
                "{other:?} is not a registered read, aimed at a protected root \
                 (repository {}, worktree {worktree:?})",
                repository.display()
            )),
        }
    }
    if reads_worktree && !opens {
        match recorded(call, "GIT_INDEX_FILE") {
            Some(index) if !inside(&directory.join(index)) => {}
            other => out.push(format!(
                "a private repository over a protected worktree with index {other:?}"
            )),
        }
    }
    out
}

/// Whether the arguments after a [`MULTI_FORM`] read are its read form.
fn read_form(sub: &str, rest: &[String]) -> Result<(), String> {
    let has = |flag: &str| {
        rest.iter().any(|arg| {
            arg == flag
                || arg
                    .strip_prefix(flag)
                    .is_some_and(|value| value.starts_with('='))
        })
    };
    // Positional arguments, without the values of options that take one.
    let mut positional: Vec<&str> = Vec::new();
    let mut value = false;
    for arg in rest {
        if value {
            value = false;
        } else if arg.starts_with('-') {
            value = sub == "config"
                && ["--file", "-f", "--blob", "--type", "-t", "--default"].contains(&arg.as_str());
        } else {
            positional.push(arg);
        }
    }
    let first = positional.first().copied();
    let read = match sub {
        "config" => {
            let reads = [
                "--get",
                "--get-all",
                "--get-regexp",
                "--get-urlmatch",
                "--list",
                "-l",
            ]
            .iter()
            .any(|flag| has(flag))
                || matches!(first, Some("get" | "list"))
                // `git config [--type] NAME` reads; `NAME VALUE` writes.
                || positional.len() == 1;
            let writes = [
                "--add",
                "--unset",
                "--unset-all",
                "--replace-all",
                "--rename-section",
                "--remove-section",
                "--edit",
                "-e",
            ]
            .iter()
            .any(|flag| has(flag))
                || matches!(
                    first,
                    Some("set" | "unset" | "rename-section" | "remove-section" | "edit")
                );
            reads && !writes
        }
        "symbolic-ref" => {
            positional.len() == 1 && !["-d", "--delete", "-m"].iter().any(|flag| has(flag))
        }
        "worktree" => first == Some("list"),
        "reflog" => matches!(first, Some("show" | "exists")),
        "remote" => matches!(first, None | Some("get-url")),
        // `create` writes only the file it is given, which the dynamic
        // oracle places.
        "bundle" => matches!(first, Some("create" | "verify" | "list-heads")),
        "pack-objects" => has("--stdout"),
        "fsck" => !has("--lost-found"),
        _ => true,
    };
    if read {
        Ok(())
    } else {
        Err(rest.join(" "))
    }
}

/// Subcommands the capture's snapshot runs with a *private* repository and a
/// *private* index over the source's worktree (`snapshot_command`): they
/// read the worktree's files and write only the private index and store.
const SNAPSHOT_WRITERS: &[&str] = &["write-tree"];

/// A fixture repository with a commit, a staged change, a worktree change
/// and an untracked file.
fn fixture_repository(repo: &Path) {
    fs::create_dir(repo).unwrap();
    fixture_git(repo, &["init", "--quiet", "--template=", "-b", "main"]);
    fs::write(repo.join("tracked"), b"tracked\n").unwrap();
    fixture_git(repo, &["add", "tracked"]);
    fixture_git(repo, &["commit", "--quiet", "-m", "one"]);
    fs::write(repo.join("tracked"), b"staged\n").unwrap();
    fixture_git(repo, &["add", "tracked"]);
    fs::write(repo.join("tracked"), b"worktree\n").unwrap();
    fs::write(repo.join("untracked"), b"untracked\n").unwrap();
}

/// Run the agent with the recording wrapper first on `PATH`.
fn agent(path: &std::ffi::OsStr, args: &[&std::ffi::OsStr]) {
    let output = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .env("PATH", path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bulkload-agent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// P76, dynamic half: every Git child a real `git-export`, a real local
/// `git-carry-estimate` and a real `estate-capture` start carries the S2
/// contract, never overrides it, and runs a registered read when it is
/// aimed at a source. A Git child built without `git_env`, or a writer
/// aimed at a source, fails here once a verb runs it.
#[test]
// One linear scenario: three verbs, then the oracle over every child.
#[allow(clippy::too_many_lines)]
fn every_git_child_of_a_capture_an_estimate_and_an_estate_is_hardened() {
    let root = scratch("children");
    let source = root.0.join("source");
    let second = root.0.join("second");
    let destination = root.0.join("destination");
    fixture_repository(&source);
    fixture_repository(&second);
    fs::create_dir(&destination).unwrap();
    fixture_git(
        &destination,
        &["init", "--quiet", "--template=", "-b", "main"],
    );

    let bin = root.0.join("bin");
    let log = root.0.join("git.log");
    install_wrapper(&bin, &log);
    let mut path = std::ffi::OsString::from(bin.as_os_str());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap());

    let capture = root.0.join("capture");
    agent(
        &path,
        &["git-export".as_ref(), source.as_ref(), capture.as_ref()],
    );
    let exported = read_log(&log).len();
    assert!(
        exported >= 5,
        "the wrapper saw only {exported} Git children"
    );

    agent(
        &path,
        &[
            "git-carry-estimate".as_ref(),
            source.as_ref(),
            destination.as_ref(),
        ],
    );
    let estimated = read_log(&log).len();
    assert!(estimated > exported, "the estimate started no Git child");

    let plan = root.0.join("plan.json");
    let state = root.0.join("state");
    let corpus = root.0.join("corpus");
    agent(
        &path,
        &[
            "estate-add-batch".as_ref(),
            plan.as_ref(),
            source.as_ref(),
            root.0.join("landed/source").as_ref(),
            "-".as_ref(),
            second.as_ref(),
            root.0.join("landed/second").as_ref(),
            "-".as_ref(),
        ],
    );
    agent(
        &path,
        &[
            "estate-capture".as_ref(),
            plan.as_ref(),
            state.as_ref(),
            corpus.as_ref(),
            "2".as_ref(),
        ],
    );
    let calls = read_log(&log);
    assert!(
        calls.len() > estimated,
        "the estate capture started no Git child"
    );

    // Both hosts of an estimate are live, so its destination is protected
    // too.
    let protected = [source.as_path(), second.as_path(), destination.as_path()];
    let mut aimed = 0;
    let mut breached = Vec::new();
    for call in &calls {
        let (directory, repository, worktree) = aim(call);
        if protected.iter().any(|root| {
            repository.starts_with(root)
                || directory.starts_with(root)
                || worktree
                    .as_deref()
                    .is_some_and(|tree| tree.starts_with(root))
        }) {
            aimed += 1;
        }
        if std::env::var_os("P76_DUMP").is_some() {
            eprintln!(
                "p76 call: sub={:?} dir={} repo={} worktree={worktree:?} index={:?} objects={:?}",
                subcommand(&call.args),
                directory.display(),
                repository.display(),
                recorded(call, "GIT_INDEX_FILE"),
                recorded(call, "GIT_OBJECT_DIRECTORY"),
            );
        }
        let found = breaches(call, &protected);
        if !found.is_empty() {
            breached.push((call.args.clone(), found));
        }
    }
    assert!(
        breached.is_empty(),
        "Git children outside the S2 contract: {breached:#?}"
    );
    assert!(
        aimed >= 10,
        "only {aimed} of {} Git children were aimed at a source: the oracle is not looking",
        calls.len()
    );
    eprintln!(
        "p76 dynamic: {} Git children ({exported} git-export, {} estimate, {} estate-capture), \
         {aimed} aimed at a protected root",
        calls.len(),
        estimated - exported,
        calls.len() - estimated
    );
}

fn recorded_call(args: &[&str], env: &[(&str, &str)]) -> Recorded {
    let mut all: BTreeMap<String, String> = [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("PWD", "/elsewhere"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    for (key, value) in env {
        all.insert((*key).to_owned(), (*value).to_owned());
    }
    Recorded {
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        env: all,
    }
}

/// The dynamic check's oracle refuses each breach on its own.
#[test]
// One table of breaches, each tried on its own.
#[allow(clippy::too_many_lines)]
fn the_dynamic_oracle_names_each_breach() {
    const HARDENED: &[&str] = &[
        "--no-optional-locks",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
    ];
    let source = Path::new("/src");
    let call = |rest: &[&str], env: &[(&str, &str)]| {
        let mut args = HARDENED.to_vec();
        args.extend_from_slice(rest);
        breaches(&recorded_call(&args, env), &[source])
    };
    let good = call(&["-C", "/src", "rev-parse", "HEAD"], &[]);
    assert!(good.is_empty(), "{good:?}");

    assert_eq!(
        call(
            &["-C", "/src", "rev-parse", "HEAD"],
            &[("GIT_OPTIONAL_LOCKS", "<unset>")]
        )
        .len(),
        1
    );
    let bare = breaches(&recorded_call(&["-C", "/r", "status"], &[]), &[source]);
    assert_eq!(bare.len(), 5, "{bare:?}");
    assert_eq!(
        call(&["-C", "/private", "gc", "--auto"], &[]),
        ["maintenance subcommand gc"]
    );
    assert!(breaches(&recorded_call(&["version"], &[]), &[source]).is_empty());

    // A later `-c` on a guarded key, in any case, overrides the contract.
    let overridden = call(
        &[
            "-c",
            "Core.HooksPath=.git/hooks",
            "-C",
            "/private",
            "status",
        ],
        &[],
    );
    assert_eq!(
        overridden,
        ["-c core.hookspath=.git/hooks overrides the contract"]
    );
    let injected = call(&["-C", "/private", "status"], &[("GIT_CONFIG_COUNT", "1")]);
    assert_eq!(injected.len(), 1, "{injected:?}");
    let injected = call(
        &["-C", "/private", "status"],
        &[("GIT_CONFIG_PARAMETERS", "'gc.auto=1'")],
    );
    assert_eq!(injected.len(), 1, "{injected:?}");
    let flagged = call(&["--config-env=gc.auto=X", "-C", "/private", "status"], &[]);
    assert_eq!(flagged.len(), 1, "{flagged:?}");
    // A subcommand's own `-c` (combined diff) is not configuration.
    assert!(call(&["-C", "/src", "log", "-c", "-1"], &[]).is_empty());

    // A writer aimed at the source, by every way of aiming.
    for (rest, env) in [
        (&["-C", "/src", "update-index", "--refresh"][..], &[][..]),
        (
            &["-C", "/src/nested", "reflog", "expire", "--all"][..],
            &[][..],
        ),
        (&["--git-dir=/src/.git", "fetch", "--all"][..], &[][..]),
        (
            &["-C", "/private", "update-ref", "refs/x", "HEAD"][..],
            &[("GIT_DIR", "/src/.git")][..],
        ),
        (&["checkout", "."][..], &[("PWD", "/src")][..]),
        (
            &["-C", "/private", "checkout-index", "-a"][..],
            &[("GIT_WORK_TREE", "/src")][..],
        ),
    ] {
        let found = call(rest, env);
        assert!(
            found
                .iter()
                .any(|breach| breach.contains("not a registered read")
                    || breach.contains("in a writing form")),
            "{rest:?} {env:?}: {found:?}"
        );
    }
    // A read name in a writing form.
    for rest in [
        &["-C", "/src", "config", "core.hooksPath", ".git/hooks"][..],
        &[
            "-C",
            "/src",
            "config",
            "--file",
            "f",
            "core.hooksPath",
            ".git/hooks",
        ][..],
        &["-C", "/src", "config"][..],
        &["-C", "/src", "symbolic-ref"][..],
        &["-C", "/src", "config", "--unset", "gc.auto"][..],
        &["-C", "/src", "symbolic-ref", "HEAD", "refs/heads/other"][..],
        &["-C", "/src", "symbolic-ref", "-d", "HEAD"][..],
        &["-C", "/src", "worktree", "prune"][..],
        &["-C", "/src", "reflog", "expire", "--all"][..],
        &["-C", "/src", "remote", "add", "x", "/y"][..],
        &["-C", "/src", "pack-objects", "pack"][..],
        &["-C", "/src", "fsck", "--lost-found"][..],
    ] {
        let found = call(rest, &[]);
        assert!(
            found
                .iter()
                .any(|breach| breach.contains("in a writing form")),
            "{rest:?}: {found:?}"
        );
    }
    let found = call(
        &["-C", "/src", "bundle", "create", "out.bundle", "--all"],
        &[],
    );
    assert_eq!(found.len(), 1, "{found:?}");
    for rest in [
        &["-C", "/src", "config", "--get", "core.bare"][..],
        &["-C", "/src", "config", "--bool", "core.sparseCheckout"][..],
        &[
            "-C",
            "/src",
            "config",
            "--file",
            "x",
            "--get-regexp",
            "^filter",
        ][..],
        &["-C", "/src", "symbolic-ref", "-q", "HEAD"][..],
        &["-C", "/src", "worktree", "list", "--porcelain"][..],
        &["-C", "/src", "reflog", "show", "refs/stash"][..],
        &["-C", "/src", "remote"][..],
        &["-C", "/src", "pack-objects", "--stdout", "--revs"][..],
        &[
            "-C",
            "/src",
            "bundle",
            "create",
            "/private/out.bundle",
            "--all",
        ][..],
    ] {
        let found = call(rest, &[]);
        assert!(found.is_empty(), "{rest:?}: {found:?}");
    }
    // Injected configuration may only switch filter drivers off.
    let filters = call(
        &["-C", "/src", "status"],
        &[
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEYS", "filter.lfs.clean filter.lfs.smudge "),
        ],
    );
    assert!(filters.is_empty(), "{filters:?}");
    let hooks = call(
        &["-C", "/src", "status"],
        &[
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEYS", "core.hooksPath "),
        ],
    );
    assert_eq!(hooks.len(), 1, "{hooks:?}");
    // The same writers on a private repository are not this oracle's
    // business.
    assert!(call(&["-C", "/private", "update-ref", "refs/x", "HEAD"], &[]).is_empty());
    // A private repository over the source's worktree needs a private index.
    let snapshot = call(
        &["-C", "/src", "status"],
        &[("GIT_DIR", "/private"), ("GIT_WORK_TREE", "/src")],
    );
    assert_eq!(snapshot.len(), 1, "{snapshot:?}");
    let snapshot = call(
        &["-C", "/src", "status"],
        &[
            ("GIT_DIR", "/private"),
            ("GIT_WORK_TREE", "/src"),
            ("GIT_INDEX_FILE", "/private/index"),
        ],
    );
    assert!(snapshot.is_empty(), "{snapshot:?}");
}
