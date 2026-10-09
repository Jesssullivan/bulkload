//! `.refuse_at(site)`: the one way a foreign error becomes a refusal (WP3).
//!
//! [`BulkloadRefusal`] has no blanket `From<std::io::Error>` or
//! `From<postcard::Error>`, so a bare `?` on a foreign error does not
//! compile. Every such site names itself instead:
//!
//! ```ignore
//! let file = File::open(path).refuse_at("walk::open_root")?;
//! ```
//!
//! An I/O error still refuses `IO` with its errno, and a postcard error
//! `FRAME_CODEC`. A site that knows a more specific cause raises that typed
//! refusal itself rather than going through this trait. The site is a
//! stable `module::function` label; WP3 PR 3 persists it in the typed
//! `Refusal{code, site, errno}` record, so it must stay a plain path.

use crate::{BulkloadRefusal, Result};

/// Turn a foreign error into a [`BulkloadRefusal`] at a named site.
pub trait RefuseAt<T> {
    /// Map the error to its refusal, naming the `site` that raised it.
    ///
    /// # Errors
    /// The mapped refusal, when `self` is an error.
    fn refuse_at(self, site: &'static str) -> Result<T>;
}

impl<T> RefuseAt<T> for core::result::Result<T, std::io::Error> {
    #[inline]
    fn refuse_at(self, site: &'static str) -> Result<T> {
        self.map_err(|error| io(&error, site))
    }
}

impl<T> RefuseAt<T> for core::result::Result<T, postcard::Error> {
    #[inline]
    fn refuse_at(self, site: &'static str) -> Result<T> {
        self.map_err(|_| codec(site))
    }
}

/// The refusal for an I/O error at `site`: `IO` with the OS errno, when
/// there is one.
#[must_use]
pub fn io(error: &std::io::Error, site: &'static str) -> BulkloadRefusal {
    debug_assert!(is_site(site), "refusal site {site:?} is not a path");
    BulkloadRefusal::Io(error.raw_os_error())
}

/// The refusal for an I/O error writing under `directory` at `site`.
///
/// A full filesystem or an exceeded quota (`ENOSPC`, `EDQUOT`) is
/// `SPACE_EXHAUSTED` naming `directory` (S4, #220); anything else is
/// [`io`]. For a write no space preflight charged, such as one of
/// bulkload's own temporaries under `TMPDIR`.
#[must_use]
pub fn io_in(
    error: &std::io::Error,
    directory: &std::path::Path,
    site: &'static str,
) -> BulkloadRefusal {
    use std::os::unix::ffi::OsStrExt as _;
    match error.raw_os_error() {
        Some(libc::ENOSPC | libc::EDQUOT) => {
            debug_assert!(is_site(site), "refusal site {site:?} is not a path");
            BulkloadRefusal::SpaceExhausted(Some(directory.as_os_str().as_bytes().to_vec()))
        }
        _ => io(error, site),
    }
}

/// The refusal for a postcard error at `site`.
#[must_use]
pub fn codec(site: &'static str) -> BulkloadRefusal {
    debug_assert!(is_site(site), "refusal site {site:?} is not a path");
    BulkloadRefusal::FrameCodec
}

/// Whether `site` is a `module::function` path: `::`-separated, non-empty
/// snake-case segments.
#[must_use]
pub fn is_site(site: &str) -> bool {
    site.split("::").all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{is_site, RefuseAt as _};
    use crate::BulkloadRefusal;

    #[test]
    fn io_errors_keep_their_errno_and_codec_errors_are_frame_codec() {
        let refused: crate::Result<()> =
            Err(std::io::Error::from_raw_os_error(2)).refuse_at("refuse::tests");
        assert_eq!(refused, Err(BulkloadRefusal::Io(Some(2))));
        let refused: crate::Result<()> =
            Err(std::io::Error::other("no errno")).refuse_at("refuse::tests");
        assert_eq!(refused, Err(BulkloadRefusal::Io(None)));
        let refused: crate::Result<u8> = postcard::from_bytes::<u8>(&[]).refuse_at("refuse::tests");
        assert_eq!(refused, Err(BulkloadRefusal::FrameCodec));
        assert_eq!(Ok::<_, std::io::Error>(7).refuse_at("refuse::tests"), Ok(7));
    }

    proptest::proptest! {
        // The shared fixed-seed corpus (OI-1003-Q7). The seed moved from this
        // block's own `0x7265_6675_7365_6174` to `test_support::CI_SEED`.
        #![proptest_config(crate::test_support::prop_config(256))]

        /// Every OS error keeps its errno through `.refuse_at`, and a value
        /// passes through untouched.
        #[test]
        fn refuse_at_keeps_every_errno(errno in 1_i32..4096, value in proptest::prelude::any::<u64>()) {
            let refused: crate::Result<u64> =
                Err(std::io::Error::from_raw_os_error(errno)).refuse_at("refuse::tests");
            proptest::prop_assert_eq!(refused, Err(BulkloadRefusal::Io(Some(errno))));
            proptest::prop_assert_eq!(
                Ok::<_, std::io::Error>(value).refuse_at("refuse::tests"),
                Ok(value)
            );
        }
    }

    // S4 (#220): a full or over-quota filesystem names the directory; any
    // other errno stays IO with its errno.
    #[test]
    fn a_full_disk_names_its_directory() {
        let dir = std::path::Path::new("/scratch/tmp");
        for errno in [libc::ENOSPC, libc::EDQUOT] {
            assert_eq!(
                super::io_in(
                    &std::io::Error::from_raw_os_error(errno),
                    dir,
                    "refuse::tests"
                ),
                BulkloadRefusal::SpaceExhausted(Some(b"/scratch/tmp".to_vec()))
            );
        }
        assert_eq!(
            super::io_in(
                &std::io::Error::from_raw_os_error(libc::EACCES),
                dir,
                "refuse::tests"
            ),
            BulkloadRefusal::Io(Some(libc::EACCES))
        );
    }

    #[test]
    fn sites_are_paths() {
        assert!(is_site("git_carry::raw_tree::capture"));
        assert!(is_site("walk"));
        assert!(!is_site(""));
        assert!(!is_site("walk::"));
        assert!(!is_site("Walk::open"));
        assert!(!is_site("walk open"));
    }
}
