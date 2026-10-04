//! ALICE-style crash-state checker over an `io` syscall trace (R-N88).
//!
//! The approach follows Pillai et al., "All File Systems Are Not Created
//! Equal: On the Complexity of Crafting Crash-Consistent Applications",
//! OSDI '14 (the ALICE tool). A protocol runs once with the `io-trace`
//! recorder attached. The checker then builds every file-system state a power
//! loss could leave under the persistence model below and runs a
//! caller-supplied invariant on it: [`check`] materializes each state into a
//! fresh temporary directory, and [`check_view`] reads it in memory, which is
//! what the fault harness uses on the trace of a real `copy`. It runs in CI with no host changes: no loop device, no block tracing,
//! just the logical trace.
//!
//! # Persistence model
//!
//! The trace is a sequence of *operations*. A mutation is a write (class
//! data), an `fchmod` (class meta), or a directory-entry change: create,
//! mkdir, link, rename, unlink (class namespace; its objects are the one or
//! two directories it changes). A sync operation names one object and a
//! [`SyncKind`]. A crash after operation `c` keeps some subset `P` of the
//! mutations issued before `c`, and applies exactly those, in trace order, to
//! the pre-trace image. `P` is legal when all of these hold:
//!
//! 1. **Durability.** A mutation `m` is in `P` if, for every object `X` it
//!    changes, a sync on `X` issued after `m` and completed before the crash
//!    makes it durable: `DataSync` for data and namespace classes, `Fsync` and
//!    `FullFlush` for every class. `Kick` and `Barrier` make nothing durable.
//! 2. **Drain.** A completed `FullFlush` (Darwin `F_FULLFSYNC`) empties the
//!    whole drive cache, so a mutation already *sent* to the device before it
//!    is durable, whichever file it belongs to. Sent means every object of the
//!    mutation had a sync of any kind after it (`DataSync` does not send a
//!    mode change). Linux syncs are not credited with a device-wide drain;
//!    that is the conservative choice.
//! 3. **Barrier ordering.** A completed `Barrier` on `X` orders the mutations
//!    sent before it ahead of mutations issued after it. With
//!    [`BarrierScope::Device`] (the default, R-N103) the order is drive-wide:
//!    any later mutation in `P` requires every mutation sent before the
//!    barrier. With [`BarrierScope::Object`] (the strict option) only
//!    mutations on `X` are ordered: a later mutation of `X` in `P` requires
//!    every earlier mutation of `X`.
//! 4. **Parents.** A mutation inside a directory created by a traced mkdir, or
//!    a rename or link of such a directory, requires that mkdir.
//! 5. **Commits.** An `Event::Commit` is a store's `SQLite` commit returning.
//!    Its records are durable from then on, which the invariant reads from
//!    [`StateInfo::commits`]; the database's contents are not modelled. The
//!    state root and the database's entry are: `Store::open` makes them with
//!    traced calls, so an invariant can require them wherever a commit has
//!    returned (#161). With [`Options::commit_drains`] (Darwin,
//!    `fullfsync=ON`) the commit also drains the drive holding the store,
//!    like a full flush there.
//!
//! Drains and device-wide barriers act on one drive: the `dev` of the node
//! they name. Mutations of nodes the image does not hold (a store's own
//! files) are refused, or dropped and counted with
//! [`Options::ignore_foreign`]; their syncs are kept, since a full flush of
//! any file on a drive drains that drive.
//!
//! The Darwin rules follow Apple's documentation. `fcntl(2)`: `F_FULLFSYNC`
//! "does the same thing as fsync(2) then asks the drive to flush all buffered
//! data to the permanent storage device", so the flush is drive-wide, not
//! per file; `F_BARRIERFSYNC` does the same as `fsync(2)` and then issues a
//! barrier command to the drive, so I/O completed before the barrier reaches
//! stable media ahead of I/O issued after it. Apple's "Reducing disk writes"
//! guidance (developer.apple.com/documentation/xcode/reducing-disk-writes)
//! and its `fsync` guidance recommend `F_BARRIERFSYNC` over `F_FULLFSYNC`
//! when an app needs write ordering rather than immediate durability, and say
//! that plain `fsync` guarantees neither. The strict per-object scope does not
//! rely on the drive honouring the barrier device-wide. Use it to check a
//! protocol that must stay correct without that assumption.
//!
//! Everything else may be lost or reordered: writes not covered by a flush,
//! renames and links not followed by a parent-directory sync, mode changes
//! after `fdatasync`. Each traced write is atomic unless
//! [`Options::sector`] splits it into sector-sized pieces, which models torn
//! writes. An inode exists from its create onward whether or not its name
//! survives, so a surviving link or rename can expose a file whose data did
//! not persist; that is the case the checker exists to catch.
//!
//! # Enumeration bound
//!
//! At each crash point the durable-and-required mutations are fixed and the
//! rest are *optional*. With at most [`Options::exhaustive_limit`] optional
//! mutations every subset is tried, and the illegal ones are discarded. Above
//! the limit the checker tries a bounded family instead: everything issued
//! (the prefix state), only the required set, every drop-one state (one
//! optional mutation lost together with whatever depends on it) and every
//! keep-one state (one optional mutation persisted ahead of all other
//! optional mutations, with what it depends on). Every bounded crash point is
//! recorded in [`Report::bounded`] with the number of subsets it did not
//! visit, so the cap is never silent. A bounded run does not count as a pass
//! ([`Report::passed`]) unless the caller sets [`Options::accept_bounded`];
//! [`Report::exhaustive`] says whether any point was bounded.
//!
//! # Refused traces
//!
//! A trace holding an `Untraced` event (an event the recorder could not
//! build) is refused, and so is a trace that creates a node identity the
//! image or an earlier event already names. File systems reuse inode numbers
//! after an unlink, and the model cannot tell the two inodes apart.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use super::trace::{Event, SyncKind};
use super::NodeId;

/// Which mutations a `Barrier` orders (model rule 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierScope {
    /// Strict option: only mutations of the barrier's own object.
    Object,
    /// Default (R-N103): every mutation sent to the drive before the barrier,
    /// as Apple documents `F_BARRIERFSYNC`.
    Device,
}

/// Checker configuration.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub barrier_scope: BarrierScope,
    /// Optional mutations per crash point up to which every subset is tried.
    /// At most [`MAX_EXHAUSTIVE`].
    pub exhaustive_limit: usize,
    /// Split each write into pieces of this many bytes (torn writes).
    pub sector: Option<usize>,
    /// Let [`Report::passed`] succeed although some crash points were only
    /// explored within the bound. Off by default: a bounded run is not a
    /// pass unless the caller says so.
    pub accept_bounded: bool,
    /// Whether a completed [`Event::Commit`] drains the drive holding its
    /// store (model rule 2). True on Darwin, where the store's `SQLite`
    /// commit issues `F_FULLFSYNC` (`fullfsync=ON`); false elsewhere, where
    /// it only syncs its own files.
    pub commit_drains: bool,
    /// Drop mutations of nodes the image does not hold and the trace never
    /// creates (a store's own files, written outside the image), counting
    /// them in [`Report::foreign`]. Syncs are always kept: a full flush of a
    /// foreign file still drains its drive. Off by default: an unknown node
    /// is then an error.
    pub ignore_foreign: bool,
}

/// Largest accepted [`Options::exhaustive_limit`] (2^20 subsets per point).
pub const MAX_EXHAUSTIVE: usize = 20;

impl Default for Options {
    fn default() -> Self {
        Self {
            barrier_scope: BarrierScope::Device,
            exhaustive_limit: 12,
            sector: None,
            accept_bounded: false,
            commit_drains: cfg!(target_vendor = "apple"),
            ignore_foreign: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    File {
        data: Vec<u8>,
        mode: u32,
    },
    Dir {
        entries: BTreeMap<Vec<u8>, NodeId>,
        mode: u32,
    },
    Symlink {
        target: Vec<u8>,
    },
}

/// A file-system image: the durable state before the trace, and each crash
/// state after replay.
#[derive(Clone, Debug)]
pub struct Image {
    root: NodeId,
    nodes: HashMap<NodeId, Node>,
}

fn other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn node_id(meta: &std::fs::Metadata) -> NodeId {
    NodeId {
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

impl Image {
    /// An image holding only an empty root directory.
    #[must_use]
    pub fn empty(root: NodeId) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(
            root,
            Node::Dir {
                entries: BTreeMap::new(),
                mode: 0o755,
            },
        );
        Self { root, nodes }
    }

    /// Add a durable regular file `name` in the root (hand-written traces).
    #[must_use]
    pub fn with_file(mut self, name: &[u8], node: NodeId, data: &[u8]) -> Self {
        self.nodes.insert(
            node,
            Node::File {
                data: data.to_vec(),
                mode: 0o644,
            },
        );
        if let Some(Node::Dir { entries, .. }) = self.nodes.get_mut(&self.root) {
            entries.insert(name.to_vec(), node);
        }
        self
    }

    /// Read the tree under `root` as the durable pre-trace image: regular
    /// files, directories and symlinks.
    ///
    /// # Errors
    /// Returns an I/O failure, or `Other` for a special file.
    pub fn scan(root: &Path) -> io::Result<Self> {
        let meta = std::fs::metadata(root)?;
        let mut image = Self {
            root: node_id(&meta),
            nodes: HashMap::new(),
        };
        image.scan_dir(root, node_id(&meta), meta.mode())?;
        Ok(image)
    }

    fn scan_dir(&mut self, path: &Path, id: NodeId, mode: u32) -> io::Result<()> {
        let mut entries = BTreeMap::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let meta = std::fs::symlink_metadata(entry.path())?;
            let child = node_id(&meta);
            if meta.is_dir() {
                self.scan_dir(&entry.path(), child, meta.mode())?;
            } else if meta.is_file() {
                self.nodes.insert(
                    child,
                    Node::File {
                        data: std::fs::read(entry.path())?,
                        mode: meta.mode() & 0o7777,
                    },
                );
            } else if meta.file_type().is_symlink() {
                self.nodes.insert(
                    child,
                    Node::Symlink {
                        target: std::fs::read_link(entry.path())?
                            .as_os_str()
                            .as_bytes()
                            .to_vec(),
                    },
                );
            } else {
                return Err(other(format!(
                    "{}: only regular files, directories and symlinks are modelled",
                    entry.path().display()
                )));
            }
            entries.insert(entry.file_name().as_bytes().to_vec(), child);
        }
        self.nodes.insert(
            id,
            Node::Dir {
                entries,
                mode: mode & 0o7777,
            },
        );
        Ok(())
    }

    /// Write the image's root contents into the existing empty directory
    /// `dest`.
    fn materialize(&self, dest: &Path) -> io::Result<()> {
        self.write_dir(self.root, dest, 0)
    }

    fn write_dir(&self, id: NodeId, path: &Path, depth: usize) -> io::Result<()> {
        if depth > 64 {
            return Err(other("directory nesting over 64: a rename cycle?"));
        }
        let Some(Node::Dir { entries, mode }) = self.nodes.get(&id) else {
            return Err(other("a directory entry names a non-directory as parent"));
        };
        for (name, child) in entries {
            let child_path = path.join(OsStr::from_bytes(name));
            match self.nodes.get(child) {
                Some(Node::File { data, mode }) => {
                    std::fs::write(&child_path, data)?;
                    std::fs::set_permissions(&child_path, std::fs::Permissions::from_mode(*mode))?;
                }
                Some(Node::Dir { .. }) => {
                    std::fs::create_dir(&child_path)?;
                    self.write_dir(*child, &child_path, depth + 1)?;
                }
                Some(Node::Symlink { target }) => {
                    std::os::unix::fs::symlink(OsStr::from_bytes(target), &child_path)?;
                }
                None => {
                    return Err(other(format!(
                        "entry {} names an unknown node",
                        String::from_utf8_lossy(name)
                    )))
                }
            }
        }
        // Keep the owner's access so the checker can read and clean up.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode | 0o700))
    }

    fn dir_mut(&mut self, id: NodeId) -> io::Result<&mut BTreeMap<Vec<u8>, NodeId>> {
        match self.nodes.get_mut(&id) {
            Some(Node::Dir { entries, .. }) => Ok(entries),
            _ => Err(other(format!("{id:?} is not a known directory"))),
        }
    }

    fn apply(&mut self, change: &Change) -> io::Result<()> {
        match change {
            Change::Entry { dir, name, node } => {
                self.dir_mut(*dir)?.insert(name.clone(), *node);
            }
            Change::Write { node, offset, data } => {
                let Some(Node::File { data: bytes, .. }) = self.nodes.get_mut(node) else {
                    return Err(other(format!("write to unknown file {node:?}")));
                };
                let start = usize::try_from(*offset).map_err(|_| other("offset overflow"))?;
                let end = start
                    .checked_add(data.len())
                    .ok_or_else(|| other("write end overflow"))?;
                if bytes.len() < end {
                    bytes.resize(end, 0);
                }
                bytes
                    .get_mut(start..end)
                    .ok_or_else(|| other("write range"))?
                    .copy_from_slice(data);
            }
            Change::Mode { node, mode: bits } => match self.nodes.get_mut(node) {
                Some(Node::File { mode, .. } | Node::Dir { mode, .. }) => *mode = *bits,
                Some(Node::Symlink { .. }) | None => {
                    return Err(other(format!("fchmod of unknown node {node:?}")))
                }
            },
            Change::Rename {
                node,
                from_dir,
                from,
                to_dir,
                to,
            } => {
                let source = self.dir_mut(*from_dir)?;
                if source.get(from) == Some(node) {
                    source.remove(from);
                }
                self.dir_mut(*to_dir)?.insert(to.clone(), *node);
            }
            Change::Unlink { dir, name } => {
                self.dir_mut(*dir)?.remove(name);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Data,
    Meta,
    Namespace,
}

#[derive(Clone, Debug)]
enum Change {
    Entry {
        dir: NodeId,
        name: Vec<u8>,
        node: NodeId,
    },
    Write {
        node: NodeId,
        offset: u64,
        data: Vec<u8>,
    },
    Mode {
        node: NodeId,
        mode: u32,
    },
    Rename {
        node: NodeId,
        from_dir: NodeId,
        from: Vec<u8>,
        to_dir: NodeId,
        to: Vec<u8>,
    },
    Unlink {
        dir: NodeId,
        name: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
enum Action {
    Mutate {
        class: Class,
        objects: Vec<NodeId>,
        change: Change,
    },
    Sync {
        node: NodeId,
        kind: SyncKind,
    },
    /// A store's `SQLite` commit returned; with `drain`, the drive holding
    /// `store` was drained.
    Commit {
        store: NodeId,
        drain: bool,
    },
}

#[derive(Clone, Debug)]
struct Op {
    /// Index of the trace event this operation came from.
    event: usize,
    action: Action,
}

impl Op {
    fn mutation(&self) -> Option<(Class, &[NodeId])> {
        match &self.action {
            Action::Mutate { class, objects, .. } => Some((*class, objects)),
            Action::Sync { .. } | Action::Commit { .. } => None,
        }
    }
}

const fn sends(kind: SyncKind, class: Class) -> bool {
    !matches!((kind, class), (SyncKind::DataSync, Class::Meta))
}

const fn durable_on_object(kind: SyncKind, class: Class) -> bool {
    match kind {
        SyncKind::Kick | SyncKind::Barrier => false,
        SyncKind::DataSync => !matches!(class, Class::Meta),
        SyncKind::Fsync | SyncKind::FullFlush => true,
    }
}

/// Where a crash state came from and how far the protocol had got.
#[derive(Clone, Debug)]
pub struct StateInfo {
    /// Operations completed before the crash.
    pub crash_point: usize,
    /// Operations in the whole trace.
    pub ops: usize,
    /// True when every operation completed: the protocol returned.
    pub complete: bool,
    /// Trace event indices whose effect persisted.
    pub persisted: Vec<usize>,
    /// Trace event indices of the [`Event::Commit`]s that completed before
    /// the crash: their records are durable in this state.
    pub commits: Vec<usize>,
}

/// A crash point where not every subset was tried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundNote {
    pub crash_point: usize,
    pub optional: usize,
    pub explored: usize,
    /// Subsets of the optional mutations not visited (saturating).
    pub skipped: u128,
}

/// One crash state that broke the invariant.
#[derive(Clone, Debug)]
pub struct Violation {
    pub crash_point: usize,
    pub persisted: Vec<usize>,
    pub lost: Vec<usize>,
    pub message: String,
}

/// The checker's result.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Copied from [`Options::accept_bounded`].
    pub accept_bounded: bool,
    pub events: usize,
    pub ops: usize,
    pub crash_points: usize,
    pub states: usize,
    /// Mutations dropped under [`Options::ignore_foreign`].
    pub foreign: usize,
    pub bounded: Vec<BoundNote>,
    pub violations: Vec<Violation>,
}

impl Report {
    /// No violation, and either every crash point was explored exhaustively
    /// or the caller set [`Options::accept_bounded`].
    #[must_use]
    pub const fn passed(&self) -> bool {
        self.violations.is_empty() && (self.exhaustive() || self.accept_bounded)
    }

    /// Every crash point had every legal subset of its optional mutations
    /// tried.
    #[must_use]
    pub const fn exhaustive(&self) -> bool {
        self.bounded.is_empty()
    }

    /// A human-readable account, bound notes and every violation included.
    #[must_use]
    pub fn summary(&self, events: &[Event]) -> String {
        let mut out = format!(
            "crash_check events={} ops={} crash_points={} states={} foreign={} exhaustive={} bounded_points={} violations={}\n",
            self.events,
            self.ops,
            self.crash_points,
            self.states,
            self.foreign,
            self.exhaustive(),
            self.bounded.len(),
            self.violations.len()
        );
        for note in &self.bounded {
            let _ = writeln!(
                out,
                "  bound: crash point {} has {} optional mutations; explored {} states, skipped {} subsets (prefix, required, drop-one, keep-one only)",
                note.crash_point, note.optional, note.explored, note.skipped
            );
        }
        for violation in &self.violations {
            let _ = writeln!(
                out,
                "  violation after op {}: {}\n    persisted: {}\n    lost: {}",
                violation.crash_point,
                violation.message,
                describe_all(events, &violation.persisted),
                describe_all(events, &violation.lost),
            );
        }
        out
    }
}

fn describe_all(events: &[Event], indices: &[usize]) -> String {
    let parts: Vec<String> = indices
        .iter()
        .map(|index| {
            events.get(*index).map_or_else(
                || format!("#{index}?"),
                |event| format!("#{index} {}", describe(event)),
            )
        })
        .collect();
    if parts.is_empty() {
        "(none)".to_owned()
    } else {
        parts.join("; ")
    }
}

fn describe(event: &Event) -> String {
    let name = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    match event {
        Event::Create { name: Some(n), .. } => format!("create {}", name(n)),
        Event::Create { name: None, .. } => "create (O_TMPFILE)".to_owned(),
        Event::Mkdir { name: n, .. } => format!("mkdir {}", name(n)),
        Event::Write { offset, data, .. } => format!("write {}@{offset}", data.len()),
        Event::SetMode { mode, .. } => format!("fchmod {mode:o}"),
        Event::Sync { kind, .. } => format!("sync {kind:?}"),
        Event::Link { name: n, .. } => format!("link {}", name(n)),
        Event::Rename { from, to, .. } => format!("rename {} -> {}", name(from), name(to)),
        Event::Unlink { name: n, .. } => format!("unlink {}", name(n)),
        Event::Symlink { name: n, .. } => format!("symlink {}", name(n)),
        Event::Commit { records, .. } => format!("commit {} records", records.len()),
        Event::Untraced { call, .. } => format!("untraced {call}"),
    }
}

/// Operations derived from a trace, plus the inodes it creates.
struct Plan {
    ops: Vec<Op>,
    fresh: Fresh,
    foreign: usize,
    /// `requires[a]`: mutations that must persist if mutation `a` does.
    requires: Vec<BTreeSet<usize>>,
    /// For each mutation, the op index by which it has been sent to its
    /// device (rule 2), if ever.
    sent_at: Vec<Option<usize>>,
    /// For each mutation, the op index after whose completion it is durable
    /// (rules 1 and 2), if ever.
    durable_at: Vec<Option<usize>>,
}

/// The device a mutation lives on: that of its first object.
fn device(objects: &[NodeId]) -> Option<u64> {
    objects.first().map(|node| node.dev)
}

/// Inodes a trace creates, with their initial state.
type Fresh = Vec<(NodeId, Node)>;

/// The ops of a trace, the inodes it creates, and how many foreign
/// mutations [`Options::ignore_foreign`] dropped.
struct Lowered {
    ops: Vec<Op>,
    fresh: Fresh,
    foreign: usize,
}

/// Whether every node an event mutates is known: in the image or created
/// earlier in the trace. Syncs and commits touch nothing.
fn touches_only(event: &Event, known: &HashSet<NodeId>) -> bool {
    let known = |node: &NodeId| known.contains(node);
    match event {
        Event::Create { dir, .. } => dir.as_ref().is_none_or(known),
        Event::Mkdir { dir, .. }
        | Event::Link { dir, .. }
        | Event::Unlink { dir, .. }
        | Event::Symlink { dir, .. } => known(dir),
        Event::Write { node, .. } | Event::SetMode { node, .. } => known(node),
        Event::Rename {
            from_dir, to_dir, ..
        } => known(from_dir) && known(to_dir),
        Event::Sync { .. } | Event::Commit { .. } | Event::Untraced { .. } => true,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one match arm per trace event kind; splitting it hides the lowering"
)]
fn build_ops(initial: &Image, events: &[Event], options: &Options) -> io::Result<Lowered> {
    let mut ops: Vec<Op> = Vec::new();
    let mut fresh = Vec::new();
    let mut foreign = 0_usize;
    // A node identity names one inode for the whole trace. A file system may
    // reuse an inode number after an unlink; the model cannot tell the two
    // apart, so such a trace is refused rather than conflated.
    let mut known: HashSet<NodeId> = initial.nodes.keys().copied().collect();
    for (index, event) in events.iter().enumerate() {
        if !touches_only(event, &known) {
            if options.ignore_foreign {
                foreign += 1;
                continue;
            }
            return Err(other(format!(
                "trace event {index} ({}) touches a node outside the image",
                describe(event)
            )));
        }
        if let Event::Create { node, .. }
        | Event::Mkdir { node, .. }
        | Event::Symlink { node, .. } = event
        {
            if !known.insert(*node) {
                return Err(other(format!(
                    "trace event {index} creates node {node:?}, which the image or an \
                     earlier event already names (inode reuse); refusing to conflate them"
                )));
            }
        }
        let mut push = |class: Class, objects: Vec<NodeId>, change: Change| {
            ops.push(Op {
                event: index,
                action: Action::Mutate {
                    class,
                    objects,
                    change,
                },
            });
        };
        match event {
            Event::Create {
                dir,
                name,
                node,
                mode,
            } => {
                fresh.push((
                    *node,
                    Node::File {
                        data: Vec::new(),
                        mode: *mode,
                    },
                ));
                match (dir, name) {
                    (Some(dir), Some(name)) => push(
                        Class::Namespace,
                        vec![*dir],
                        Change::Entry {
                            dir: *dir,
                            name: name.clone(),
                            node: *node,
                        },
                    ),
                    (_, None) => {}
                    (None, Some(_)) => return Err(other("create with a name but no directory")),
                }
            }
            Event::Mkdir {
                dir,
                name,
                node,
                mode,
            } => {
                fresh.push((
                    *node,
                    Node::Dir {
                        entries: BTreeMap::new(),
                        mode: *mode,
                    },
                ));
                push(
                    Class::Namespace,
                    vec![*dir],
                    Change::Entry {
                        dir: *dir,
                        name: name.clone(),
                        node: *node,
                    },
                );
            }
            Event::Symlink {
                dir,
                name,
                node,
                target,
            } => {
                fresh.push((
                    *node,
                    Node::Symlink {
                        target: target.clone(),
                    },
                ));
                push(
                    Class::Namespace,
                    vec![*dir],
                    Change::Entry {
                        dir: *dir,
                        name: name.clone(),
                        node: *node,
                    },
                );
            }
            Event::Write {
                node, offset, data, ..
            } => {
                let piece = options.sector.unwrap_or(data.len()).max(1);
                let mut at = *offset;
                for part in data.chunks(piece) {
                    push(
                        Class::Data,
                        vec![*node],
                        Change::Write {
                            node: *node,
                            offset: at,
                            data: part.to_vec(),
                        },
                    );
                    at = at.saturating_add(u64::try_from(part.len()).unwrap_or(u64::MAX));
                }
            }
            Event::SetMode { node, mode } => push(
                Class::Meta,
                vec![*node],
                Change::Mode {
                    node: *node,
                    mode: *mode,
                },
            ),
            Event::Sync { node, kind } => ops.push(Op {
                event: index,
                action: Action::Sync {
                    node: *node,
                    kind: *kind,
                },
            }),
            Event::Commit { store, .. } => ops.push(Op {
                event: index,
                action: Action::Commit {
                    store: *store,
                    drain: options.commit_drains,
                },
            }),
            Event::Link { node, dir, name } => push(
                Class::Namespace,
                vec![*dir],
                Change::Entry {
                    dir: *dir,
                    name: name.clone(),
                    node: *node,
                },
            ),
            Event::Rename {
                node,
                from_dir,
                from,
                to_dir,
                to,
            } => {
                let mut objects = vec![*from_dir];
                if to_dir != from_dir {
                    objects.push(*to_dir);
                }
                push(
                    Class::Namespace,
                    objects,
                    Change::Rename {
                        node: *node,
                        from_dir: *from_dir,
                        from: from.clone(),
                        to_dir: *to_dir,
                        to: to.clone(),
                    },
                );
            }
            Event::Unlink { dir, name } => push(
                Class::Namespace,
                vec![*dir],
                Change::Unlink {
                    dir: *dir,
                    name: name.clone(),
                },
            ),
            Event::Untraced { call, error } => {
                return Err(other(format!(
                "trace event {index} ({call}) was not recorded: {error}; refusing a partial trace"
            )))
            }
        }
    }
    Ok(Lowered {
        ops,
        fresh,
        foreign,
    })
}

impl Plan {
    fn new(initial: &Image, events: &[Event], options: &Options) -> io::Result<Self> {
        let Lowered {
            ops,
            fresh,
            foreign,
        } = build_ops(initial, events, options)?;
        let mut plan = Self {
            requires: vec![BTreeSet::new(); ops.len()],
            sent_at: vec![None; ops.len()],
            durable_at: vec![None; ops.len()],
            ops,
            fresh,
            foreign,
        };
        plan.compute_persistence();
        plan.add_barrier_edges(options.barrier_scope);
        plan.add_parent_edges();
        Ok(plan)
    }

    /// The first op after mutation `m` by which every object of `m` had a
    /// sync satisfying `accept`, if there is one.
    fn covered_at(&self, m: usize, accept: fn(SyncKind, Class) -> bool) -> Option<usize> {
        let (class, objects) = self.ops.get(m).and_then(Op::mutation)?;
        let mut latest = None;
        for object in objects {
            let first = self
                .ops
                .iter()
                .enumerate()
                .skip(m + 1)
                .find(|(_, op)| {
                    matches!(op.action, Action::Sync { node, kind } if node == *object && accept(kind, class))
                })
                .map(|(index, _)| index)?;
            latest = Some(latest.map_or(first, |at: usize| at.max(first)));
        }
        latest
    }

    /// The device an op drains, if it is a drain: a full flush drains the
    /// drive holding its file, and a store commit with `drain` the drive
    /// holding its store.
    const fn drains(op: &Op) -> Option<u64> {
        match op.action {
            Action::Sync {
                node,
                kind: SyncKind::FullFlush,
            } => Some(node.dev),
            Action::Commit { store, drain: true } => Some(store.dev),
            Action::Sync { .. } | Action::Commit { .. } | Action::Mutate { .. } => None,
        }
    }

    fn compute_persistence(&mut self) {
        for m in 0..self.ops.len() {
            let Some((_, objects)) = self.ops.get(m).and_then(Op::mutation) else {
                continue;
            };
            let dev = device(objects);
            let sent = self.covered_at(m, sends);
            let direct = self.covered_at(m, durable_on_object);
            let drained = sent.and_then(|sent| {
                self.ops
                    .iter()
                    .enumerate()
                    .skip(sent + 1)
                    .find(|(_, op)| Self::drains(op).is_some() && Self::drains(op) == dev)
                    .map(|(index, _)| index)
            });
            let durable = match (direct, drained) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if let Some(slot) = self.sent_at.get_mut(m) {
                *slot = sent;
            }
            if let Some(slot) = self.durable_at.get_mut(m) {
                *slot = durable;
            }
        }
    }

    fn durable(&self, m: usize, crash_point: usize) -> bool {
        self.durable_at
            .get(m)
            .copied()
            .flatten()
            .is_some_and(|at| at < crash_point)
    }

    /// Trace event indices of the commits completed before `crash_point`.
    fn commits_before(&self, crash_point: usize) -> Vec<usize> {
        self.ops
            .iter()
            .take(crash_point)
            .filter(|op| matches!(op.action, Action::Commit { .. }))
            .map(|op| op.event)
            .collect()
    }

    fn mutations(&self) -> impl Iterator<Item = (usize, Class, &[NodeId])> + '_ {
        self.ops.iter().enumerate().filter_map(|(index, op)| {
            op.mutation()
                .map(|(class, objects)| (index, class, objects))
        })
    }

    fn add_barrier_edges(&mut self, scope: BarrierScope) {
        let barriers: Vec<(usize, NodeId)> = self
            .ops
            .iter()
            .enumerate()
            .filter_map(|(index, op)| match op.action {
                Action::Sync {
                    node,
                    kind: SyncKind::Barrier,
                } => Some((index, node)),
                Action::Sync { .. } | Action::Commit { .. } | Action::Mutate { .. } => None,
            })
            .collect();
        let mut edges = Vec::new();
        for (barrier, object) in barriers {
            // A drive barrier orders only I/O on its own drive.
            let before: Vec<usize> = self
                .mutations()
                .filter(|(index, class, objects)| {
                    *index < barrier
                        && match scope {
                            BarrierScope::Object => {
                                objects.contains(&object) && sends(SyncKind::Barrier, *class)
                            }
                            BarrierScope::Device => {
                                device(objects) == Some(object.dev)
                                    && self
                                        .sent_at
                                        .get(*index)
                                        .copied()
                                        .flatten()
                                        .is_some_and(|sent| sent <= barrier)
                            }
                        }
                })
                .map(|(index, _, _)| index)
                .collect();
            for (after, _, objects) in self.mutations() {
                let ordered = after > barrier
                    && match scope {
                        BarrierScope::Device => device(objects) == Some(object.dev),
                        BarrierScope::Object => objects.contains(&object),
                    };
                if ordered {
                    edges.extend(before.iter().map(|earlier| (after, *earlier)));
                }
            }
        }
        for (after, earlier) in edges {
            if let Some(set) = self.requires.get_mut(after) {
                set.insert(earlier);
            }
        }
    }

    fn add_parent_edges(&mut self) {
        let mut made_by: HashMap<NodeId, usize> = HashMap::new();
        for (index, op) in self.ops.iter().enumerate() {
            if let Action::Mutate {
                change: Change::Entry { node, .. },
                ..
            } = &op.action
            {
                if matches!(
                    self.fresh.iter().find(|(id, _)| id == node),
                    Some((_, Node::Dir { .. }))
                ) {
                    made_by.entry(*node).or_insert(index);
                }
            }
        }
        let mut edges = Vec::new();
        for (index, op) in self.ops.iter().enumerate() {
            let Action::Mutate {
                objects, change, ..
            } = &op.action
            else {
                continue;
            };
            let moved = match change {
                Change::Entry { node, .. } | Change::Rename { node, .. } => Some(*node),
                Change::Write { .. } | Change::Mode { .. } | Change::Unlink { .. } => None,
            };
            for dir in objects.iter().copied().chain(moved) {
                if let Some(&mkdir) = made_by.get(&dir) {
                    if mkdir < index {
                        edges.push((index, mkdir));
                    }
                }
            }
        }
        for (after, earlier) in edges {
            if let Some(set) = self.requires.get_mut(after) {
                set.insert(earlier);
            }
        }
    }

    /// `set` plus everything it transitively requires.
    fn close_up(&self, mut set: BTreeSet<usize>) -> BTreeSet<usize> {
        let mut stack: Vec<usize> = set.iter().copied().collect();
        while let Some(next) = stack.pop() {
            for earlier in self.requires.get(next).into_iter().flatten() {
                if set.insert(*earlier) {
                    stack.push(*earlier);
                }
            }
        }
        set
    }

    fn is_closed(&self, set: &BTreeSet<usize>) -> bool {
        set.iter().all(|member| {
            self.requires
                .get(*member)
                .is_none_or(|needs| needs.iter().all(|need| set.contains(need)))
        })
    }

    /// `universe` minus `lost` and everything in it that transitively
    /// requires `lost`.
    fn drop_one(&self, universe: &BTreeSet<usize>, lost: usize) -> BTreeSet<usize> {
        let mut gone = BTreeSet::from([lost]);
        let mut changed = true;
        while changed {
            changed = false;
            for member in universe {
                if !gone.contains(member)
                    && self
                        .requires
                        .get(*member)
                        .is_some_and(|needs| needs.iter().any(|need| gone.contains(need)))
                {
                    gone.insert(*member);
                    changed = true;
                }
            }
        }
        universe.difference(&gone).copied().collect()
    }

    fn image(&self, initial: &Image, persisted: &BTreeSet<usize>) -> io::Result<Image> {
        let mut image = initial.clone();
        for (id, node) in &self.fresh {
            image.nodes.entry(*id).or_insert_with(|| node.clone());
        }
        for index in persisted {
            if let Some(Op {
                action: Action::Mutate { change, .. },
                ..
            }) = self.ops.get(*index)
            {
                image.apply(change)?;
            }
        }
        Ok(image)
    }

    fn events_of(&self, ops: impl IntoIterator<Item = usize>) -> Vec<usize> {
        let set: BTreeSet<usize> = ops
            .into_iter()
            .filter_map(|index| self.ops.get(index).map(|op| op.event))
            .collect();
        set.into_iter().collect()
    }
}

/// One entry of a crash state, as [`View`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry<'a> {
    File { data: &'a [u8], mode: u32 },
    Dir { mode: u32 },
    Symlink { target: &'a [u8] },
}

/// Read-only access to one crash state's tree, by path relative to the root
/// the image was built from.
#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    image: &'a Image,
}

impl<'a> View<'a> {
    fn entry(self, node: NodeId) -> Option<Entry<'a>> {
        match self.image.nodes.get(&node)? {
            Node::File { data, mode } => Some(Entry::File { data, mode: *mode }),
            Node::Dir { mode, .. } => Some(Entry::Dir { mode: *mode }),
            Node::Symlink { target } => Some(Entry::Symlink { target }),
        }
    }

    /// The entry at `rel` (`/`-separated, no leading `/`); the empty path is
    /// the root.
    #[must_use]
    pub fn get(self, rel: &[u8]) -> Option<Entry<'a>> {
        let mut node = self.image.root;
        for part in rel
            .split(|byte| *byte == b'/')
            .filter(|part| !part.is_empty())
        {
            let Node::Dir { entries, .. } = self.image.nodes.get(&node)? else {
                return None;
            };
            node = *entries.get(part)?;
        }
        self.entry(node)
    }

    /// Every entry beneath the root, by relative path, parents first.
    #[must_use]
    pub fn walk(self) -> Vec<(Vec<u8>, Entry<'a>)> {
        let mut out = Vec::new();
        let mut stack = vec![(Vec::new(), self.image.root, 0_usize)];
        while let Some((path, node, depth)) = stack.pop() {
            let Some(Node::Dir { entries, .. }) = self.image.nodes.get(&node) else {
                continue;
            };
            if depth > 64 {
                continue;
            }
            for (name, child) in entries.iter().rev() {
                let mut rel = path.clone();
                if !rel.is_empty() {
                    rel.push(b'/');
                }
                rel.extend_from_slice(name);
                if let Some(entry) = self.entry(*child) {
                    if matches!(entry, Entry::Dir { .. }) {
                        stack.push((rel.clone(), *child, depth + 1));
                    }
                    out.push((rel, entry));
                }
            }
        }
        out.sort_by(|left, right| left.0.cmp(&right.0));
        out
    }

    /// Whether inode `node` is reachable in this state: named by some entry.
    #[must_use]
    pub fn names(self, node: NodeId) -> bool {
        self.image.nodes.values().any(|entry| {
            matches!(entry, Node::Dir { entries, .. } if entries.values().any(|child| *child == node))
        })
    }

    /// The entry of inode `node` if any directory names it.
    #[must_use]
    pub fn node(self, node: NodeId) -> Option<Entry<'a>> {
        if self.names(node) {
            self.entry(node)
        } else {
            None
        }
    }
}

fn enumerate(
    initial: &Image,
    events: &[Event],
    options: &Options,
    mut visit: impl FnMut(&Image, &StateInfo) -> io::Result<Result<(), String>>,
) -> io::Result<Report> {
    if options.exhaustive_limit > MAX_EXHAUSTIVE {
        return Err(other(format!(
            "exhaustive_limit {} is over {MAX_EXHAUSTIVE}",
            options.exhaustive_limit
        )));
    }
    let plan = Plan::new(initial, events, options)?;
    let mut report = Report {
        accept_bounded: options.accept_bounded,
        events: events.len(),
        ops: plan.ops.len(),
        foreign: plan.foreign,
        ..Report::default()
    };
    for crash_point in 0..=plan.ops.len() {
        report.crash_points += 1;
        let issued: BTreeSet<usize> = plan
            .mutations()
            .map(|(index, _, _)| index)
            .filter(|index| *index < crash_point)
            .collect();
        let durable: BTreeSet<usize> = issued
            .iter()
            .copied()
            .filter(|m| plan.durable(*m, crash_point))
            .collect();
        let required = plan.close_up(durable);
        let optional: Vec<usize> = issued.difference(&required).copied().collect();
        let mut states: BTreeSet<Vec<usize>> = BTreeSet::new();
        if optional.len() <= options.exhaustive_limit {
            for mask in 0..(1_u64 << optional.len()) {
                let mut set = required.clone();
                for (bit, member) in optional.iter().enumerate() {
                    if mask & (1_u64 << bit) != 0 {
                        set.insert(*member);
                    }
                }
                if plan.is_closed(&set) {
                    states.insert(set.into_iter().collect());
                }
            }
        } else {
            states.insert(issued.iter().copied().collect());
            states.insert(required.iter().copied().collect());
            for member in &optional {
                let dropped = plan.drop_one(&issued, *member);
                if required.is_subset(&dropped) {
                    states.insert(dropped.into_iter().collect());
                }
                let mut kept = required.clone();
                kept.insert(*member);
                states.insert(plan.close_up(kept).into_iter().collect());
            }
            let total = u32::try_from(optional.len())
                .ok()
                .and_then(|bits| 1_u128.checked_shl(bits))
                .unwrap_or(u128::MAX);
            report.bounded.push(BoundNote {
                crash_point,
                optional: optional.len(),
                explored: states.len(),
                skipped: total.saturating_sub(u128::try_from(states.len()).unwrap_or(u128::MAX)),
            });
        }
        let commits = plan.commits_before(crash_point);
        for state in states {
            report.states += 1;
            let set: BTreeSet<usize> = state.iter().copied().collect();
            let image = plan.image(initial, &set)?;
            let info = StateInfo {
                crash_point,
                ops: plan.ops.len(),
                complete: crash_point == plan.ops.len(),
                persisted: plan.events_of(state.iter().copied()),
                commits: commits.clone(),
            };
            if let Err(message) = visit(&image, &info)? {
                report.violations.push(Violation {
                    crash_point,
                    persisted: info.persisted.clone(),
                    lost: plan.events_of(issued.difference(&set).copied()),
                    message,
                });
            }
        }
    }
    Ok(report)
}

/// Enumerate the crash states of `events` from the durable image `initial`
/// and run `invariant` on each, read in memory through a [`View`].
///
/// # Errors
/// Refuses a trace holding an [`Event::Untraced`], a foreign node (unless
/// [`Options::ignore_foreign`]), an invalid [`Options::exhaustive_limit`], or
/// a state that cannot be built.
pub fn check_view(
    initial: &Image,
    events: &[Event],
    options: &Options,
    mut invariant: impl FnMut(View<'_>, &StateInfo) -> Result<(), String>,
) -> io::Result<Report> {
    enumerate(initial, events, options, |image, info| {
        Ok(invariant(View { image }, info))
    })
}

static SCRATCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A private scratch directory, removed on drop.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "bulkload-crash-check-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Enumerate the crash states of `events` from the durable image `initial`
/// and run `invariant` on each, materialized in a fresh directory.
///
/// # Errors
/// As [`check_view`], or a state that cannot be materialized.
pub fn check(
    initial: &Image,
    events: &[Event],
    options: &Options,
    mut invariant: impl FnMut(&Path, &StateInfo) -> Result<(), String>,
) -> io::Result<Report> {
    let scratch = Scratch::new()?;
    let mut serial = 0_usize;
    enumerate(initial, events, options, |image, info| {
        serial += 1;
        let dir = scratch.0.join(format!("state-{serial}"));
        std::fs::create_dir(&dir)?;
        image.materialize(&dir)?;
        let verdict = invariant(&dir, info);
        std::fs::remove_dir_all(&dir)?;
        Ok(verdict)
    })
}

#[cfg(test)]
mod tests;
