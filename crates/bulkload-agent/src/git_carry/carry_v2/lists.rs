//! `<state>/git-carry-v2/lists/<pack_id>.list`: the persisted plans resume
//! reads back.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::io::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::super::estimate::{
    create_private, cstring, open_existing, private_file, private_subdirectory, PrivateState,
};
use super::{overlap, Outcome, PackPlan, Source};
use crate::BulkloadRefusal;

/// A private state directory holding persisted plans.
///
/// Opening it creates nothing; the first [`ListStore::persist`] creates `git-carry-v2/lists/`
/// at 0700. Every file is created `O_CREAT|O_EXCL|O_NOFOLLOW` at 0600 and
/// linked into place without replacing anything, and every read is by
/// descriptor with the private-file checks the stderr store uses.
#[derive(Debug)]
pub struct ListStore {
    state: PrivateState,
}

impl ListStore {
    /// Open `state_dir` (it must exist and be private: owned by the effective
    /// uid, no group or other bits, no ACL, not a symlink).
    ///
    /// # Errors
    /// Refuses a state dir that fails those checks, or any I/O failure.
    pub fn open(state_dir: &Path) -> crate::Result<Self> {
        let state = PrivateState::open(state_dir)?;
        // An existing `git-carry-v2/` or `lists/` must already be private.
        if let Some(carry) = private_subdirectory(state.directory(), "git-carry-v2", false)? {
            private_subdirectory(&carry, "lists", false)?;
        }
        Ok(Self { state })
    }

    /// Whether the state dir lies at or under `path`, by directory identity.
    #[must_use]
    pub fn is_inside(&self, path: &Path) -> bool {
        self.state.is_inside(path)
    }

    fn lists(&self, create: bool) -> crate::Result<Option<(File, File)>> {
        let Some(carry) = private_subdirectory(self.state.directory(), "git-carry-v2", create)?
        else {
            return Ok(None);
        };
        Ok(private_subdirectory(&carry, "lists", create)?.map(|lists| (carry, lists)))
    }

    /// Write `plan` durably as `<pack_id>.list`: a private temporary is
    /// written and synced, linked to its name (never replacing one), and the
    /// directories are synced. An existing file of that name is accepted only
    /// if it passes the private-file checks and its bytes hash to the same
    /// `pack_id`, so it holds this very list.
    ///
    /// # Errors
    /// Refuses `SNAPSHOT_ROOTS_OVERLAP` when the state dir lies inside
    /// `source` (a sender never writes into its source), a planted file that
    /// fails the checks, and any I/O failure.
    pub fn persist(&self, plan: &PackPlan, source: &Source) -> Outcome<PathBuf> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        if source.contains_state(|path| self.is_inside(path)) {
            return Err(overlap());
        }
        let (carry, lists) = self.lists(true)?.ok_or(BulkloadRefusal::Io(None))?;
        let bytes = plan.encode();
        let leaf = format!("{}.list", plan.pack_id());
        let name = cstring(leaf.as_bytes())?;
        let temporary = cstring(
            format!(
                ".list-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        )?;
        let mut file = create_private(&lists, &temporary)?;
        let written = (|| -> crate::Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            // SAFETY: both names are NUL-terminated and relative to the open
            // `lists` directory; `linkat` never replaces an existing name.
            let linked = unsafe {
                libc::linkat(
                    lists.as_raw_fd(),
                    temporary.as_ptr(),
                    lists.as_raw_fd(),
                    name.as_ptr(),
                    0,
                )
            };
            if linked != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EEXIST) {
                    return Err(error.into());
                }
                let existing = read_private(&lists, &name)?;
                if blake3::hash(&existing).to_hex().as_str() != plan.pack_id() {
                    return Err(BulkloadRefusal::DigestMismatch);
                }
            }
            Ok(())
        })();
        drop(file);
        // SAFETY: the temporary's name is NUL-terminated and relative to the
        // open `lists` directory, where this call just created it.
        unsafe { libc::unlinkat(lists.as_raw_fd(), temporary.as_ptr(), 0) };
        written?;
        lists.sync_all()?;
        carry.sync_all()?;
        self.state.directory().sync_all()?;
        Ok(self.state.root().join("git-carry-v2/lists").join(leaf))
    }

    /// Read `<pack_id>.list` back for resume. Its bytes must hash to
    /// `pack_id` and decode as a list.
    ///
    /// # Errors
    /// Refuses a `pack_id` that is not a 64-digit lowercase hex name
    /// (`FIELD_DOMAIN_VIOLATION`), an absent list (`SEALED_OBJECT_MISSING`), a
    /// file that fails the private checks, bytes that do not hash to
    /// `pack_id` (`DIGEST_MISMATCH`) or do not decode (`SCHEMA_MISMATCH`).
    pub fn load(&self, pack_id: &str) -> crate::Result<PackPlan> {
        if pack_id.len() != 64
            || !pack_id
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        let Some((_, lists)) = self.lists(false)? else {
            return Err(BulkloadRefusal::SealedObjectMissing);
        };
        let name = cstring(format!("{pack_id}.list").as_bytes())?;
        let bytes = match read_private(&lists, &name) {
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => {
                return Err(BulkloadRefusal::SealedObjectMissing)
            }
            other => other?,
        };
        if blake3::hash(&bytes).to_hex().as_str() != pack_id {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        PackPlan::decode(&bytes)
    }
}

/// Read a private file by descriptor after the private-file checks.
fn read_private(directory: &File, name: &std::ffi::CString) -> crate::Result<Vec<u8>> {
    let mut file = open_existing(directory, name)?;
    private_file(&file)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}
