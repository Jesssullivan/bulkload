//! Wire v6: bounded frames for a full-duplex bulkload session (R-N118).
//!
//! Every frame is a 4-byte big-endian body length, then a one-byte tag, then
//! the body:
//!
//! - [`TAG_CONTROL`]: a postcard [`Control`] message;
//! - [`TAG_DATA`]: one content chunk, a fixed [`DATA_HEADER_BYTES`]-byte
//!   binary [`DataHeader`] followed by the raw payload (sent with `writev`, so
//!   the payload is never copied into a frame buffer);
//! - [`TAG_PACK_DATA`]: reserved for the Git sub-stream (W6), a fixed
//!   [`PACK_HEADER_BYTES`]-byte [`PackDataHeader`] and raw pack bytes.
//!
//! v6 is a hard cut (R-N59/R-N118): there is no v5 codec. It adds the
//! `SQLite` snapshot seat (#218): the session's [`SqliteMode`] in
//! [`Control::Open`], a database's `-wal` identity in [`Control::Entry`],
//! the [`Control::SqliteSidecar`] and [`Control::SqliteSnapshot`] frames,
//! and `SQLite`'s result code in [`Control::Refused`]. Every other frame is
//! v5's, the W6 reserved frames included. The session
//! opens with [`Control::Open`], which carries [`PROTO_VERSION`] and
//! [`wire_id`], the BLAKE3 of [`WIRE_SCHEMA`]; a peer with either one
//! different refuses the session. A corrupt, truncated or oversized frame is
//! refused as [`BulkloadRefusal::FrameCodec`] or
//! [`BulkloadRefusal::BudgetExceeded`], never re-synchronised.

use serde::{Deserialize, Serialize};

use crate::refusal::BulkloadRefusal;
use crate::row::RowSchema;
use crate::Result;

/// Wire protocol version (R-N118; v6 adds the `SQLite` snapshot seat, #218).
pub const PROTO_VERSION: u16 = 6;

/// Bytes of frame header carrying the body length.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Bytes of the length prefix plus the tag.
pub const FRAME_HEADER_BYTES: usize = LENGTH_PREFIX_BYTES + 1;

/// Largest body this codec will encode or accept, in bytes. Control frames
/// carry rows or a file's chunk manifest; data frames carry one chunk.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Largest payload one [`TAG_DATA`] frame may carry: one content-defined
/// chunk at the agent's 256 KiB maximum.
pub const MAX_DATA_PAYLOAD: usize = 256 * 1024;

/// Tag of a postcard [`Control`] frame.
pub const TAG_CONTROL: u8 = 0x01;
/// Tag of a binary content [`DataHeader`] frame.
pub const TAG_DATA: u8 = 0x02;
/// Tag of a binary Git [`PackDataHeader`] frame (reserved, W6).
pub const TAG_PACK_DATA: u8 = 0x03;

/// Bytes of a [`DataHeader`] on the wire.
pub const DATA_HEADER_BYTES: usize = 64;
/// Bytes of a [`PackDataHeader`] on the wire.
pub const PACK_HEADER_BYTES: usize = 32;

/// The wire schema whose BLAKE3 is [`wire_id`]. Any change to a frame's
/// layout, a [`Control`] variant or its order must change this text.
pub const WIRE_SCHEMA: &str = "bulkload wire v6 (2026-10-08)
frame: u32be body_len, u8 tag, body
tag 0x01 control: postcard Control, the whole body and nothing after it
tag 0x02 data: 64-byte header (u64le entry, u32le index, u32le size, u64le offset, [u8;32] digest, [u8;8] zero) + size payload bytes
tag 0x03 pack_data (reserved, W6): 32-byte header (u32le sub, u32le segment, u64le offset, u32le size, [u8;12] zero) + size payload bytes
control 0 Open{proto u16, wire_id [u8;32], root bytes, state bytes, sqlite SqliteMode}
control 1 Start{authority bytes}
control 2 Entry{entry u64, row RowSchema, wal Option<SidecarId>}
control 3 Refused{entry Option<u64>, rel_path bytes, code string, sqlite_code Option<i32>}
control 4 EngineTemporary{rel_path bytes}
control 5 WalkDone{entries u64}
control 6 Decide{entry u64, decision Decision}
control 7 Manifest{entry u64, root [u8;32], chunks Vec<ChunkSpec>}
control 8 NeedChunks{entry u64, indices Vec<u32>}
control 9 End{entry u64, root [u8;32], chunks u32, size u64, racy bool}
control 10 Credit{bytes u64}
control 11 SourceDone{entries u64, source_bytes_read u64}
control 12 SubOpen{sub u32, kind SubKind} (reserved, W6)
control 13 GitHaves{sub u32, haves Vec<bytes>, done bool} (reserved, W6)
control 14 GitSegmentStart{sub u32, segment u32, objects u64, bytes u64} (reserved, W6)
control 15 SegmentEnd{sub u32, segment u32, pack_digest [u8;32], objects u64, bytes u64} (reserved, W6)
control 16 SegmentDurable{sub u32, segment u32} (reserved, W6)
control 17 GitRefs{sub u32, updates Vec<RefUpdate>} (reserved, W6)
control 18 GitCommitted{sub u32, transaction_digest [u8;32]} (reserved, W6)
control 19 GitResume{sub u32, durable_segments Vec<u32>} (reserved, W6)
control 20 Held{entry u64, held bool}
control 21 SqliteSidecar{rel_path bytes, database bytes}
control 22 SqliteSnapshot{entry u64, size u64, wal Option<SidecarId>}
sqlitemode 0 Refuse, 1 Snapshot
sidecarid {dev u64, ino u64, size u64, mtime_ns i128, ctime_ns i128}
decision 0 Skip, 1 Reuse, 2 Send, 3 WantManifest, 4 Refuse{code string}
subkind 0 GitPack{repo bytes, refs_digest [u8;32]}
chunkspec {digest [u8;32], size u64}
refupdate {name bytes, old Option<bytes>, new bytes}
manifest_root: blake3 derive_key(\"bulkload 2026-09-25 manifest root v1\") over digest||size_le per chunk";

/// BLAKE3 of [`WIRE_SCHEMA`]: both peers must agree on it.
#[must_use]
pub fn wire_id() -> [u8; 32] {
    *blake3::hash(WIRE_SCHEMA.as_bytes()).as_bytes()
}

/// Key-derivation context of [`manifest_root`].
pub const MANIFEST_ROOT_CONTEXT: &str = "bulkload 2026-09-25 manifest root v1";

/// The root of a file's chunk manifest: BLAKE3 in derive-key mode over
/// `digest || size_le` of every chunk in file order. It replaces the
/// whole-file hash, so neither side hashes a file a second time.
#[must_use]
pub fn manifest_root(chunks: &[ChunkSpec]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(MANIFEST_ROOT_CONTEXT);
    for chunk in chunks {
        hasher.update(&chunk.digest);
        hasher.update(&chunk.size.to_le_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// A content-defined chunk in a source file, in file order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkSpec {
    /// BLAKE3 of the chunk bytes.
    pub digest: [u8; 32],
    /// Exact plaintext byte length.
    pub size: u64,
}

/// What the destination wants for one [`Control::Entry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Nothing to send: a directory or symlink the destination made itself,
    /// or a row it refused on its own (the refusal is its to report).
    Skip,
    /// The destination already holds this exact source identity durably.
    Reuse,
    /// Stream every chunk, then [`Control::End`].
    Send,
    /// Send [`Control::Manifest`] first; the destination answers
    /// [`Control::NeedChunks`] with only what it cannot fill locally.
    WantManifest,
    /// The destination refuses this entry; the source sends nothing.
    Refuse { code: String },
}

/// How the destination asks the source to treat `SQLite` provider state
/// (#218). The destination chooses it; it is carried in
/// [`Control::Open`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SqliteMode {
    /// v5's behaviour: a seat whose first bytes are a `SQLite` database or
    /// WAL magic, and every `-wal`, `-shm` or `-journal` by name, is refused
    /// `SQLITE_STATE_CHANGED`. No raw `SQLite` byte crosses the wire.
    #[default]
    Refuse,
    /// A database seat is carried as a backup-API snapshot the source takes
    /// into its private state ([`Control::SqliteSnapshot`]); its live
    /// sidecars are never carried ([`Control::SqliteSidecar`]). WAL magic
    /// and orphan sidecars stay refused.
    Snapshot,
}

/// The stat identity of a database's `<name>-wal` sidecar (#218).
///
/// With the main file's row it keys a snapshot seat, because a WAL-mode
/// commit writes the `-wal` and leaves the main file untouched until a
/// checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarId {
    /// Containing device id.
    pub dev: u64,
    /// Inode number.
    pub ino: u64,
    /// Apparent size in bytes.
    pub size: u64,
    /// Modification time, nanoseconds since the unix epoch.
    pub mtime_ns: i128,
    /// Inode change time, nanoseconds since the unix epoch.
    pub ctime_ns: i128,
}

/// The kind of a Git sub-stream (reserved, W6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubKind {
    /// Negotiated thin packs for one repository (R-N60).
    GitPack {
        repo: Vec<u8>,
        refs_digest: [u8; 32],
    },
}

/// One ref update of a Git ref transaction (reserved, W6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefUpdate {
    /// Full ref name, raw bytes.
    pub name: Vec<u8>,
    /// The value the destination must hold for the update to apply; `None`
    /// means the ref must not exist.
    pub old: Option<Vec<u8>>,
    /// The new object id, raw bytes.
    pub new: Vec<u8>,
}

/// Postcard control messages. Direction: `→D` source to destination, `→S`
/// destination to source.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    /// →S. Opens the session: protocol and schema identity, the source
    /// root and private source state to use, and how `SQLite` provider state
    /// is treated.
    Open {
        proto: u16,
        wire_id: [u8; 32],
        root: Vec<u8>,
        state: Vec<u8>,
        sqlite: SqliteMode,
    },
    /// →D. The source's authority: its store and root identity.
    Start { authority: Vec<u8> },
    /// →D. One scanned seat, numbered in stream order. `wal`: in
    /// [`SqliteMode::Snapshot`] only, the identity of a regular `<name>-wal`
    /// beside the seat, which keys a snapshot seat with the row; always
    /// `None` in [`SqliteMode::Refuse`].
    Entry {
        entry: u64,
        row: RowSchema,
        wal: Option<SidecarId>,
    },
    /// →D. A refusal: of a walked seat (`entry` `None`) or of an entry's
    /// capture (after any of its data frames).
    Refused {
        entry: Option<u64>,
        rel_path: Vec<u8>,
        code: String,
        /// `SQLite`'s extended result code, for `SQLITE_BACKUP_FAILED` only
        /// ([`BulkloadRefusal::sqlite_code`]): the receiver reads the
        /// backup's corruption from it (OI-1003-Q148). `None` for every
        /// other code; a peer that sends one with another code violates
        /// the protocol.
        sqlite_code: Option<i32>,
    },
    /// →D. A source file in the materializer's tagged temporary-name grammar:
    /// recorded by the walk and never carried. Not a refusal.
    EngineTemporary { rel_path: Vec<u8> },
    /// →D. Every entry has been offered.
    WalkDone { entries: u64 },
    /// →S. The destination's decision for one entry.
    Decide { entry: u64, decision: Decision },
    /// →D. An entry's chunk manifest, answering [`Decision::WantManifest`].
    Manifest {
        entry: u64,
        root: [u8; 32],
        chunks: Vec<ChunkSpec>,
    },
    /// →S. The chunk indices the destination still needs after a manifest.
    NeedChunks { entry: u64, indices: Vec<u32> },
    /// →D. An entry's data is complete: its manifest root, chunk count and
    /// size, for the destination to check coverage against. `racy`: the
    /// seat was stamped within one timestamp tick of its capture (or later
    /// than the source's clock), so a same-size rewrite in that tick could
    /// keep its stat identity. Its identity cannot vouch for these bytes, so
    /// neither side records it as a reuse key (R25, R-N58, R-N76); the next
    /// run reads the seat again.
    End {
        entry: u64,
        root: [u8; 32],
        chunks: u32,
        size: u64,
        racy: bool,
    },
    /// →S. Permission to send this many more data payload bytes.
    Credit { bytes: u64 },
    /// →D. The source is done: every entry answered, every capture of this
    /// session committed to the source ledger.
    SourceDone {
        entries: u64,
        source_bytes_read: u64,
    },
    /// →D. Opens a sub-stream (reserved, W6).
    SubOpen { sub: u32, kind: SubKind },
    /// →S. Object ids the destination already has (reserved, W6).
    GitHaves {
        sub: u32,
        haves: Vec<Vec<u8>>,
        done: bool,
    },
    /// →D. A thin-pack segment begins (reserved, W6).
    GitSegmentStart {
        sub: u32,
        segment: u32,
        objects: u64,
        bytes: u64,
    },
    /// →D. A segment's pack bytes are all sent (reserved, W6).
    SegmentEnd {
        sub: u32,
        segment: u32,
        pack_digest: [u8; 32],
        objects: u64,
        bytes: u64,
    },
    /// →S. A segment is ingested durably in quarantine (reserved, W6).
    SegmentDurable { sub: u32, segment: u32 },
    /// →D. The ref transaction to apply after every segment (reserved, W6).
    GitRefs { sub: u32, updates: Vec<RefUpdate> },
    /// →S. The ref transaction committed (reserved, W6).
    GitCommitted {
        sub: u32,
        transaction_digest: [u8; 32],
    },
    /// →S. On a resumed sub-stream, the segments already durable, so the
    /// source resends only the others (reserved, W6).
    GitResume {
        sub: u32,
        durable_segments: Vec<u32>,
    },
    /// →S. The answer to every [`Control::End`]: with `held`, the
    /// destination holds the entry's bytes durably (a sealed temporary, or an
    /// existing output verified against the manifest), so the source may
    /// commit the capture to its ledger. Without it, the entry was refused
    /// and the capture is not recorded (R25: a committed capture never
    /// costs a source read again).
    Held { entry: u64, held: bool },
    /// →D. [`SqliteMode::Snapshot`] only: a `-wal`, `-shm` or `-journal`
    /// beside its regular base file `database`, both relative paths. Never
    /// carried and never an entry; its outcome is its base's: covered when
    /// the base is published from a snapshot or reused as one, and refused
    /// by name otherwise.
    SqliteSidecar {
        rel_path: Vec<u8>,
        database: Vec<u8>,
    },
    /// →D. [`SqliteMode::Snapshot`] only, before an entry's first data frame
    /// or its [`Control::Manifest`]: the entry's content is a backup-API
    /// snapshot of `size` bytes the source took into its private state, not
    /// the live file. `wal` is the database's `-wal` identity after the
    /// backup, under which the destination keys its row; `End.racy` says
    /// the capture was not settled (an identity moved, or was stamped
    /// within the racy allowance), so no reuse row is kept.
    SqliteSnapshot {
        entry: u64,
        size: u64,
        wal: Option<SidecarId>,
    },
}

/// The fixed header of a [`TAG_DATA`] frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataHeader {
    /// The entry this chunk belongs to.
    pub entry: u64,
    /// The chunk's index in the entry's manifest.
    pub index: u32,
    /// Payload bytes that follow the header.
    pub size: u32,
    /// The chunk's byte offset in the file.
    pub offset: u64,
    /// BLAKE3 of the payload.
    pub digest: [u8; 32],
}

impl DataHeader {
    /// The header's wire bytes.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; DATA_HEADER_BYTES] {
        let mut out = [0_u8; DATA_HEADER_BYTES];
        let fields: [&[u8]; 5] = [
            &self.entry.to_le_bytes(),
            &self.index.to_le_bytes(),
            &self.size.to_le_bytes(),
            &self.offset.to_le_bytes(),
            &self.digest,
        ];
        let mut at = 0;
        for field in fields {
            if let Some(slot) = out.get_mut(at..at + field.len()) {
                slot.copy_from_slice(field);
            }
            at += field.len();
        }
        out
    }

    /// Parse a header's wire bytes.
    ///
    /// # Errors
    /// [`BulkloadRefusal::FrameCodec`] for a short header or non-zero padding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = Fields(bytes);
        let header = Self {
            entry: u64::from_le_bytes(reader.take()?),
            index: u32::from_le_bytes(reader.take()?),
            size: u32::from_le_bytes(reader.take()?),
            offset: u64::from_le_bytes(reader.take()?),
            digest: reader.take()?,
        };
        let padding: [u8; 8] = reader.take()?;
        if padding != [0; 8] || !reader.0.is_empty() {
            return Err(BulkloadRefusal::FrameCodec);
        }
        Ok(header)
    }
}

/// The fixed header of a [`TAG_PACK_DATA`] frame (reserved, W6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackDataHeader {
    /// The sub-stream.
    pub sub: u32,
    /// The segment within the sub-stream.
    pub segment: u32,
    /// Byte offset of this payload within the segment's pack.
    pub offset: u64,
    /// Payload bytes that follow the header.
    pub size: u32,
}

impl PackDataHeader {
    /// The header's wire bytes.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; PACK_HEADER_BYTES] {
        let mut out = [0_u8; PACK_HEADER_BYTES];
        let fields: [&[u8]; 4] = [
            &self.sub.to_le_bytes(),
            &self.segment.to_le_bytes(),
            &self.offset.to_le_bytes(),
            &self.size.to_le_bytes(),
        ];
        let mut at = 0;
        for field in fields {
            if let Some(slot) = out.get_mut(at..at + field.len()) {
                slot.copy_from_slice(field);
            }
            at += field.len();
        }
        out
    }

    /// Parse a header's wire bytes.
    ///
    /// # Errors
    /// [`BulkloadRefusal::FrameCodec`] for a short header or non-zero padding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = Fields(bytes);
        let header = Self {
            sub: u32::from_le_bytes(reader.take()?),
            segment: u32::from_le_bytes(reader.take()?),
            offset: u64::from_le_bytes(reader.take()?),
            size: u32::from_le_bytes(reader.take()?),
        };
        let padding: [u8; 12] = reader.take()?;
        if padding != [0; 12] || !reader.0.is_empty() {
            return Err(BulkloadRefusal::FrameCodec);
        }
        Ok(header)
    }
}

/// Fixed-width fields read from the front of a byte slice.
struct Fields<'a>(&'a [u8]);

impl Fields<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(BulkloadRefusal::FrameCodec)?;
        self.0 = rest;
        Ok(*head)
    }
}

/// Decode one postcard value that fills `bytes` exactly. Trailing bytes
/// after the value are refused, never ignored: a frame body or a record
/// with garbage after it is corrupt, not valid (#87, R33).
///
/// # Errors
/// [`BulkloadRefusal::FrameCodec`] for a malformed value or a non-empty
/// remainder.
pub fn decode_exact<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    match postcard::take_from_bytes(bytes) {
        Ok((value, [])) => Ok(value),
        _ => Err(BulkloadRefusal::FrameCodec),
    }
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A postcard control message.
    Control(Control),
    /// One content chunk.
    Data {
        header: DataHeader,
        payload: Vec<u8>,
    },
    /// One piece of a Git pack segment (reserved, W6).
    PackData {
        header: PackDataHeader,
        payload: Vec<u8>,
    },
}

/// The length prefix and tag of a frame with `body` body bytes.
///
/// # Errors
/// [`BulkloadRefusal::BudgetExceeded`] past [`MAX_FRAME_BYTES`].
pub fn frame_header(tag: u8, body: usize) -> Result<[u8; FRAME_HEADER_BYTES]> {
    let len = body.checked_add(1).ok_or(BulkloadRefusal::BudgetExceeded)?;
    if len > MAX_FRAME_BYTES {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let [a, b, c, d] = u32::try_from(len)
        .map_err(|_| BulkloadRefusal::BudgetExceeded)?
        .to_be_bytes();
    Ok([a, b, c, d, tag])
}

impl Control {
    /// Encode as one [`TAG_CONTROL`] frame.
    ///
    /// # Errors
    /// [`BulkloadRefusal::FrameCodec`] if postcard declines the value, and
    /// [`BulkloadRefusal::BudgetExceeded`] past [`MAX_FRAME_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>> {
        let body = postcard::to_stdvec(self).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let header = frame_header(TAG_CONTROL, body.len())?;
        let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + body.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&body);
        Ok(out)
    }
}

/// The frame header and data header of a [`TAG_DATA`] frame carrying
/// `header.size` payload bytes; the payload follows on the wire.
///
/// # Errors
/// [`BulkloadRefusal::BudgetExceeded`] for a payload over
/// [`MAX_DATA_PAYLOAD`].
pub fn data_prefix(header: &DataHeader) -> Result<[u8; FRAME_HEADER_BYTES + DATA_HEADER_BYTES]> {
    let size = usize::try_from(header.size).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    if size > MAX_DATA_PAYLOAD {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let frame = frame_header(TAG_DATA, DATA_HEADER_BYTES + size)?;
    let mut out = [0_u8; FRAME_HEADER_BYTES + DATA_HEADER_BYTES];
    out.get_mut(..FRAME_HEADER_BYTES)
        .ok_or(BulkloadRefusal::FrameCodec)?
        .copy_from_slice(&frame);
    out.get_mut(FRAME_HEADER_BYTES..)
        .ok_or(BulkloadRefusal::FrameCodec)?
        .copy_from_slice(&header.to_bytes());
    Ok(out)
}

/// The frame header and pack header of a [`TAG_PACK_DATA`] frame.
///
/// # Errors
/// [`BulkloadRefusal::BudgetExceeded`] past [`MAX_FRAME_BYTES`].
pub fn pack_prefix(
    header: &PackDataHeader,
) -> Result<[u8; FRAME_HEADER_BYTES + PACK_HEADER_BYTES]> {
    let size = usize::try_from(header.size).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    let frame = frame_header(TAG_PACK_DATA, PACK_HEADER_BYTES + size)?;
    let mut out = [0_u8; FRAME_HEADER_BYTES + PACK_HEADER_BYTES];
    out.get_mut(..FRAME_HEADER_BYTES)
        .ok_or(BulkloadRefusal::FrameCodec)?
        .copy_from_slice(&frame);
    out.get_mut(FRAME_HEADER_BYTES..)
        .ok_or(BulkloadRefusal::FrameCodec)?
        .copy_from_slice(&header.to_bytes());
    Ok(out)
}

impl Frame {
    /// Encode to wire bytes, payload included.
    ///
    /// # Errors
    /// As [`Control::encode`], [`data_prefix`] and [`pack_prefix`]; a
    /// payload whose length differs from its header's `size` is
    /// [`BulkloadRefusal::FrameCodec`].
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Control(control) => control.encode(),
            Self::Data { header, payload } => {
                if usize::try_from(header.size).ok() != Some(payload.len()) {
                    return Err(BulkloadRefusal::FrameCodec);
                }
                let mut out = data_prefix(header)?.to_vec();
                out.extend_from_slice(payload);
                Ok(out)
            }
            Self::PackData { header, payload } => {
                if usize::try_from(header.size).ok() != Some(payload.len()) {
                    return Err(BulkloadRefusal::FrameCodec);
                }
                let mut out = pack_prefix(header)?.to_vec();
                out.extend_from_slice(payload);
                Ok(out)
            }
        }
    }

    /// Decode one frame body: `tag` and the `body` bytes after it.
    ///
    /// # Errors
    /// [`BulkloadRefusal::FrameCodec`] for an unknown tag, a malformed body,
    /// a control body with bytes after its message, or a payload length that differs from its header;
    /// [`BulkloadRefusal::BudgetExceeded`] for a data payload over
    /// [`MAX_DATA_PAYLOAD`].
    pub fn decode_body(tag: u8, body: &[u8]) -> Result<Self> {
        match tag {
            TAG_CONTROL => Ok(Self::Control(decode_exact(body)?)),
            TAG_DATA => {
                let (header, payload) = body
                    .split_at_checked(DATA_HEADER_BYTES)
                    .ok_or(BulkloadRefusal::FrameCodec)?;
                let header = DataHeader::from_bytes(header)?;
                if payload.len() > MAX_DATA_PAYLOAD {
                    return Err(BulkloadRefusal::BudgetExceeded);
                }
                if usize::try_from(header.size).ok() != Some(payload.len()) {
                    return Err(BulkloadRefusal::FrameCodec);
                }
                Ok(Self::Data {
                    header,
                    payload: payload.to_vec(),
                })
            }
            TAG_PACK_DATA => {
                let (header, payload) = body
                    .split_at_checked(PACK_HEADER_BYTES)
                    .ok_or(BulkloadRefusal::FrameCodec)?;
                let header = PackDataHeader::from_bytes(header)?;
                if usize::try_from(header.size).ok() != Some(payload.len()) {
                    return Err(BulkloadRefusal::FrameCodec);
                }
                Ok(Self::PackData {
                    header,
                    payload: payload.to_vec(),
                })
            }
            _ => Err(BulkloadRefusal::FrameCodec),
        }
    }

    /// Decode one frame from the front of `buf`; returns it and the bytes
    /// consumed.
    ///
    /// # Errors
    /// [`BulkloadRefusal::FrameCodec`] on a truncated header or body, or as
    /// [`Frame::decode_body`]; [`BulkloadRefusal::BudgetExceeded`] if the
    /// declared length exceeds [`MAX_FRAME_BYTES`].
    pub fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        let (length, rest) = buf
            .split_first_chunk::<LENGTH_PREFIX_BYTES>()
            .ok_or(BulkloadRefusal::FrameCodec)?;
        let len = usize::try_from(u32::from_be_bytes(*length))
            .map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        if len > MAX_FRAME_BYTES {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let frame = rest.get(..len).ok_or(BulkloadRefusal::FrameCodec)?;
        let (tag, body) = frame.split_first().ok_or(BulkloadRefusal::FrameCodec)?;
        Ok((Self::decode_body(*tag, body)?, LENGTH_PREFIX_BYTES + len))
    }
}

#[cfg(test)]
mod tests;
