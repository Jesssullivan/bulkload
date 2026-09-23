//! Open-file limits (`RLIMIT_NOFILE`) and the descriptor budget derived from
//! them.
//!
//! macOS starts processes at a soft limit of 256. The destination keeps
//! descriptors open for chunk reuse and for files waiting on their group
//! commit, so both are sized from the soft limit rather than fixed.

/// Descriptors held back for everything that is not a staged or reused
/// output: stores and their WAL files, sockets, pipes, capture workers.
const RESERVED: u64 = 64;

/// Darwin rejects `RLIM_INFINITY` for `RLIMIT_NOFILE`; this is its
/// `OPEN_MAX`, the largest soft limit it always accepts.
#[cfg(target_vendor = "apple")]
const DARWIN_OPEN_MAX: u64 = 10_240;

/// `(soft, hard)` `RLIMIT_NOFILE`.
///
/// # Errors
/// Returns a failed `getrlimit`.
pub fn descriptor_limit() -> std::io::Result<(u64, u64)> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live, writable `rlimit` for the duration of the
    // call, and RLIMIT_NOFILE is a valid resource.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((limit.rlim_cur, limit.rlim_max))
}

/// Set the soft `RLIMIT_NOFILE` to `soft`, keeping the hard limit.
///
/// # Errors
/// Returns a failed `getrlimit` or `setrlimit`, e.g. `soft` above the hard
/// limit.
pub fn set_soft_descriptor_limit(soft: u64) -> std::io::Result<()> {
    let (_, hard) = descriptor_limit()?;
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: `limit` is a live, initialized `rlimit` for the duration of the
    // call, and RLIMIT_NOFILE is a valid resource.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Raise the soft `RLIMIT_NOFILE` to the hard limit (on Darwin, to at most
/// `OPEN_MAX` when the hard limit is unlimited). Returns the soft limit in
/// force afterwards.
///
/// # Errors
/// Returns a failed `getrlimit`; a refused raise leaves the limit unchanged.
pub fn raise_descriptor_limit() -> std::io::Result<u64> {
    let (soft, hard) = descriptor_limit()?;
    #[cfg(target_vendor = "apple")]
    let hard = hard.min(DARWIN_OPEN_MAX.max(soft));
    if hard > soft && set_soft_descriptor_limit(hard).is_ok() {
        return Ok(hard);
    }
    Ok(soft)
}

/// Descriptors the destination may hold for staged and reused outputs: the
/// soft limit less a fixed reserve, never below 16.
#[must_use]
pub fn descriptor_budget() -> u64 {
    descriptor_limit()
        .map_or(256, |(soft, _)| soft)
        .saturating_sub(RESERVED)
        .max(16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_leaves_a_reserve_and_the_raise_never_lowers() -> std::io::Result<()> {
        let (soft, hard) = descriptor_limit()?;
        assert!(soft <= hard);
        assert!(descriptor_budget() >= 16);
        assert!(descriptor_budget() <= soft.max(16 + RESERVED));
        assert!(raise_descriptor_limit()? >= soft);
        Ok(())
    }
}
