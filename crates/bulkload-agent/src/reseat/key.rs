//! Where the destination's agents look for a lane's sessions: Claude Code's
//! project key for a working directory, and the destination working
//! directory's real path (R0, TIN-6587).
//!
//! The key function is Claude Code 2.1.299's, read from the binary and
//! checked against it (`docs/agent-notes/2026-10-16-reseat-investigation.md`,
//! section 1.1):
//!
//! ```js
//! function k(e){return e.replace(/[^a-zA-Z0-9]/g,"-")}
//! function dM(e){let n=k(e);if(n.length<=200)return n;
//!   return `${n.slice(0,200)}-${Math.abs(pae(e)).toString(36)}`}
//! function pae(t){let e=0;for(let n=0;n<t.length;n++)e=(e<<5)-e+t.charCodeAt(n)|0;return e}
//! ```
//!
//! Every UTF-16 code unit outside `[A-Za-z0-9]` becomes `-` (so `é` is one
//! `-` and an emoji two), and a key over 200 units is cut to 200 and gets
//! `-` plus the base-36 magnitude of a Java-style 32-bit hash of the path.

use std::path::{Component, Path, PathBuf};

use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The longest key Claude Code keeps unhashed.
const KEY_LIMIT: usize = 200;

/// Claude Code's project key for the canonical working directory `cwd`.
#[must_use]
pub fn project_key(cwd: &str) -> String {
    let units: Vec<u16> = cwd.encode_utf16().collect();
    let key: String = units
        .iter()
        .map(|unit| match u8::try_from(*unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => char::from(byte),
            _ => '-',
        })
        .collect();
    if key.len() <= KEY_LIMIT {
        return key;
    }
    let hash = units.iter().fold(0_i32, |hash, unit| {
        hash.wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(*unit))
    });
    format!(
        "{}-{}",
        key.get(..KEY_LIMIT).unwrap_or(&key),
        base36(i64::from(hash).unsigned_abs())
    )
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        let digit = usize::try_from(value % 36).unwrap_or(0);
        out.push(DIGITS.get(digit).copied().unwrap_or(b'0'));
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// The destination working directory's real path, as Claude Code and Codex
/// compute it there (`realpath(cwd)`), and whether it exists.
///
/// A directory that does not exist yet resolves through its deepest
/// existing ancestor: that ancestor's real path plus the remaining names, so
/// the key is the one a checkout created there later gets (unless a
/// symlink is put in its way, which the next pass reports as a key
/// change).
///
/// # Errors
/// `PATH_NOT_ABSOLUTE` for a relative path, `PATH_ESCAPES_ROOT` for one
/// with a `..` component, and the I/O refusal of a failed `realpath`.
pub fn resolve_dest(path: &Path) -> Result<(PathBuf, bool)> {
    if !path.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    for ancestor in path.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(real) => {
                let rest = path
                    .strip_prefix(ancestor)
                    .unwrap_or_else(|_| Path::new(""));
                let exists = rest.as_os_str().is_empty();
                return Ok((if exists { real } else { real.join(rest) }, exists));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).refuse_at("reseat::key::resolve_dest"),
        }
    }
    Err(BulkloadRefusal::PathNotAbsolute)
}

/// `path` as UTF-8, which every key and every Codex row needs.
///
/// # Errors
/// `PATH_NOT_PORTABLE` when it is not UTF-8.
pub fn utf8(path: &Path) -> Result<&str> {
    path.to_str().ok_or(BulkloadRefusal::PathNotPortable)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::project_key;

    /// The six cases the investigation checked against the real binary
    /// (section 1.1), including the long path's hash suffix.
    #[test]
    fn keys_match_claude_code_2_1_299() {
        assert_eq!(
            project_key("/srv/fast-local/jess/git/lab"),
            "-srv-fast-local-jess-git-lab"
        );
        assert_eq!(
            project_key("/Users/jess/git/GloriousFlywheel/.worktrees/tin-3768"),
            "-Users-jess-git-GloriousFlywheel--worktrees-tin-3768"
        );
        assert_eq!(
            project_key("/h/dot.dir_under-score"),
            "-h-dot-dir-under-score"
        );
        assert_eq!(project_key("/h/caf\u{e9}"), "-h-caf-");
        assert_eq!(project_key("/h/emoji\u{1f600}x"), "-h-emoji--x");
        assert_eq!(project_key("/h/sp ace"), "-h-sp-ace");
        let long = format!(
            "/dev/shm/reseat-lane/home-long{}",
            "/abcdefghijklmnop".repeat(13)
        );
        let key = project_key(&long);
        assert!(key.len() > 201);
        assert!(key.starts_with("-dev-shm-reseat-lane-home-long-abcdefghijklmnop"));
        assert_eq!(key.get(..200), project_key(&long).get(..200));
        assert!(key.get(200..).unwrap().starts_with('-'));
    }

    /// The hash is the JavaScript `(h<<5)-h+c|0` over UTF-16 units, so a
    /// long path's suffix is reproducible here (values from a reference
    /// port run in Python, `ccKey.py`).
    #[test]
    fn long_key_hash_is_java_style() {
        let path = format!("/x{}", "/y".repeat(120));
        let key = project_key(&path);
        assert!(key.len() > 201);
        let mut hash: i64 = 0;
        for unit in path.encode_utf16() {
            hash = ((hash << 5) - hash + i64::from(unit)) & 0xffff_ffff;
        }
        if hash >= 1 << 31 {
            hash -= 1 << 32;
        }
        let suffix = key.get(201..).unwrap();
        assert_eq!(i64::from_str_radix(suffix, 36).unwrap(), hash.abs());
    }
}
