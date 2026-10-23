//! A source root's device number is not a stable name for it.
//!
//! A session's output authority names its source root by the source
//! store's authority, the root's path, and the root's `(dev, ino)`, and
//! every row key the destination writes starts with it ([`row_key`]). macOS
//! renumbers an APFS volume's `st_dev` when it reboots (neo, 2026-10-22:
//! `16777234` before, `16777232` after), so a pass after a source reboot
//! keyed its rows under a new authority and found none of the rows its
//! earlier passes wrote: each changed seat whose output this store wrote
//! was refused `DESTINATION_OCCUPIED` in place of superseding it
//! (estate-20261022 pass 5: 981 of 988), and each unchanged one was read
//! again.
//!
//! The device number is the only part that moves. The store authority is a
//! random id kept in the source's own state, and the root's path and inode
//! (and every seat's inode, size and times) survive a reboot. So the rows
//! keyed under an authority that differs from this session's only in the
//! source root's device are this session's own earlier rows, and are read as
//! such: for ownership (a supersede still needs the file at the path to have
//! exactly the identity one of those rows recorded for it; that check is not
//! relaxed), for reuse and for consolidation records. Rows written by older
//! builds are found the same way, so ledgers already on disk are honoured.

use crate::transfer_store::{row_key, Store};
use crate::{Result, RowSchema};
use bulkload_proto::frame::decode_exact;

#[cfg(test)]
std::thread_local! {
    /// Test hook: while set, this thread's sessions find no earlier
    /// authority, as builds before this module did not.
    static OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test hook: while `on`, [`DevAliases::find`] on this thread finds none.
#[cfg(test)]
pub fn off(on: bool) {
    OFF.with(|flag| flag.set(on));
}

/// What an output authority names, all but its source root's device.
#[derive(PartialEq, Eq)]
struct Named {
    store: [u8; 32],
    source: Vec<u8>,
    source_ino: u64,
    destination: Vec<u8>,
    destination_dev: u64,
    destination_ino: u64,
}

/// An output authority as `transfer::receive` composes it, split into what
/// it names but the source root's device, and that device. `None` for any
/// other bytes (a test's opaque authority).
fn split(authority: &[u8]) -> Option<(Named, u64)> {
    let (source_authority, destination, destination_dev, destination_ino): (
        Vec<u8>,
        Vec<u8>,
        u64,
        u64,
    ) = decode_exact(authority).ok()?;
    let (store, source, source_dev, source_ino): ([u8; 32], Vec<u8>, u64, u64) =
        decode_exact(&source_authority).ok()?;
    Some((
        Named {
            store,
            source,
            source_ino,
            destination,
            destination_dev,
            destination_ino,
        },
        source_dev,
    ))
}

/// The earlier authorities of one session's roots: those the destination
/// store holds rows under that name the same source store, source root path
/// and inode and the same destination root, under another source root
/// device.
#[derive(Debug, Default)]
pub struct DevAliases {
    /// This session's source root device.
    current: u64,
    /// Each earlier authority, with the source root device it named.
    earlier: Vec<(Vec<u8>, u64)>,
}

impl DevAliases {
    /// The earlier authorities `store` holds rows under for this session's
    /// `authority`.
    ///
    /// # Errors
    /// Refuses database failures.
    pub fn find(store: &Store, authority: &[u8]) -> Result<Self> {
        #[cfg(test)]
        if OFF.with(std::cell::Cell::get) {
            return Ok(Self::default());
        }
        let Some((named, current)) = split(authority) else {
            return Ok(Self::default());
        };
        let earlier = store
            .output_authorities()?
            .into_iter()
            .filter_map(|other| {
                let (theirs, dev) = split(&other)?;
                (theirs == named && dev != current).then_some((other, dev))
            })
            .collect();
        Ok(Self { current, earlier })
    }

    /// How many earlier authorities there are.
    pub const fn len(&self) -> usize {
        self.earlier.len()
    }

    /// The earlier authorities.
    pub fn authorities(&self) -> Vec<&[u8]> {
        self.earlier
            .iter()
            .map(|(authority, _)| authority.as_slice())
            .collect()
    }

    /// `row`'s key under each earlier authority, as that pass keyed it: a
    /// seat on the root's device had the root's device then, and a seat on
    /// another device keeps its own.
    ///
    /// # Errors
    /// Refuses serialization failure.
    pub fn keys(&self, row: &RowSchema) -> Result<Vec<Vec<u8>>> {
        self.earlier
            .iter()
            .map(|(authority, dev)| {
                if row.dev == self.current {
                    let mut then = row.clone();
                    then.dev = *dev;
                    row_key(authority, &then)
                } else {
                    row_key(authority, row)
                }
            })
            .collect()
    }
}
