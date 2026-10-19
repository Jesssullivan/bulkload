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

/// The refusal for an I/O error statting, entering or opening a seat the
/// source walk listed, at `site` (S4, TIN-6592).
///
/// `ENOENT` is `SOURCE_VANISHED`: the seat, or a directory above it, was
/// removed after the listing. Anything else is [`io`], with its errno: a
/// component swapped for a symlink or a non-directory (`ELOOP`, `ENOTDIR`)
/// keeps the refusal its site gave it before.
#[must_use]
pub fn source_seat(error: &std::io::Error, site: &'static str) -> BulkloadRefusal {
    if error.raw_os_error() == Some(libc::ENOENT) {
        debug_assert!(is_site(site), "refusal site {site:?} is not a path");
        return BulkloadRefusal::SourceVanished;
    }
    io(error, site)
}

/// The refusal for an I/O error resolving, statting or opening the source
/// root itself, at `site` (#232).
///
/// `ENOENT` is `SOURCE_ROOT_MISSING`: the root the verb was given is not
/// there. Anything else is `SOURCE_ROOT_UNREADABLE` with its errno: an
/// unsearchable parent (`EACCES`), a symlink loop (`ELOOP`), a component
/// that is not a directory (`ENOTDIR`). A seat beneath a root keeps
/// [`source_seat`]'s refusal; the root's own kind is judged by its caller
/// (`SOURCE_ROOT_KIND_MISMATCH`), never read from an errno here.
#[must_use]
pub fn source_root(error: &std::io::Error, site: &'static str) -> BulkloadRefusal {
    debug_assert!(is_site(site), "refusal site {site:?} is not a path");
    if error.raw_os_error() == Some(libc::ENOENT) {
        return BulkloadRefusal::SourceRootMissing;
    }
    BulkloadRefusal::SourceRootUnreadable(error.raw_os_error())
}

/// The refusal for an I/O error reading or writing a session's frame
/// stream at `site` (S4, TIN-6592).
///
/// The stream ending at or inside a frame (`UnexpectedEof`), or a peer
/// that is gone (`EPIPE`, `ECONNRESET`), is `TRANSPORT_LOST`: the transport
/// ended before the session did. Anything else is [`io`], with its errno.
#[must_use]
pub fn transport(error: &std::io::Error, site: &'static str) -> BulkloadRefusal {
    if error.kind() == std::io::ErrorKind::UnexpectedEof
        || matches!(error.raw_os_error(), Some(libc::EPIPE | libc::ECONNRESET))
    {
        debug_assert!(is_site(site), "refusal site {site:?} is not a path");
        return BulkloadRefusal::TransportLost;
    }
    io(error, site)
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

    // S4 (TIN-6592): a listed seat that is gone is typed; any other errno
    // stays IO with its errno.
    #[test]
    fn a_vanished_seat_is_typed_and_other_errnos_keep_theirs() {
        let at = |errno| super::source_seat(&std::io::Error::from_raw_os_error(errno), "t::s");
        assert_eq!(at(libc::ENOENT), BulkloadRefusal::SourceVanished);
        for errno in [
            libc::EACCES,
            libc::EIO,
            libc::EPERM,
            libc::ELOOP,
            libc::ENOTDIR,
        ] {
            assert_eq!(at(errno), BulkloadRefusal::Io(Some(errno)));
        }
    }

    // #232: a root that is not there is typed; any other errno at the root
    // is typed with its errno, never read as a kind.
    #[test]
    fn a_missing_root_is_typed_and_other_errnos_are_unreadable_with_theirs() {
        let at = |errno| super::source_root(&std::io::Error::from_raw_os_error(errno), "t::s");
        assert_eq!(at(libc::ENOENT), BulkloadRefusal::SourceRootMissing);
        for errno in [libc::EACCES, libc::ELOOP, libc::ENOTDIR, libc::EIO] {
            assert_eq!(at(errno), BulkloadRefusal::SourceRootUnreadable(Some(errno)));
        }
        assert_eq!(
            super::source_root(&std::io::Error::other("no errno"), "t::s"),
            BulkloadRefusal::SourceRootUnreadable(None)
        );
        assert_eq!(
            BulkloadRefusal::SourceRootUnreadable(Some(libc::EACCES)).to_string(),
            "SOURCE_ROOT_UNREADABLE (errno 13)"
        );
    }

    // S4 (TIN-6592): a stream that ends, or a peer that is gone, is the
    // transport's loss; any other error stays IO.
    #[test]
    fn a_lost_transport_is_typed_and_other_errors_keep_their_errno() {
        let eof = std::io::Error::from(std::io::ErrorKind::UnexpectedEof);
        assert_eq!(
            super::transport(&eof, "t::s"),
            BulkloadRefusal::TransportLost
        );
        for errno in [libc::EPIPE, libc::ECONNRESET] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert_eq!(
                super::transport(&error, "t::s"),
                BulkloadRefusal::TransportLost
            );
        }
        let error = std::io::Error::from_raw_os_error(libc::EIO);
        assert_eq!(
            super::transport(&error, "t::s"),
            BulkloadRefusal::Io(Some(libc::EIO))
        );
        let other = std::io::Error::other("no errno");
        assert_eq!(super::transport(&other, "t::s"), BulkloadRefusal::Io(None));
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
