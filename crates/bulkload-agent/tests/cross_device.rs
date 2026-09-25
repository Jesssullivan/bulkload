//! PR #59 review: with the destination on another device than its state
//! store, a group-mode copy must fully flush the destination's device.
//! macOS only, and ignored by default because it attaches a disk image:
//! `cargo test -p bulkload-agent --test cross_device -- --ignored`.

#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::MetadataExt as _;
use std::process::Command;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::transfer::copy;

#[test]
#[ignore = "attaches a disk image with hdiutil"]
fn a_destination_on_another_device_is_fully_flushed() {
    let base = std::env::temp_dir().join(format!("bulkload-xdev-{}", std::process::id()));
    let mount = base.join("mnt");
    std::fs::create_dir_all(base.join("source/sub")).unwrap();
    std::fs::create_dir_all(&mount).unwrap();
    std::fs::write(base.join("source/a"), vec![7_u8; 3_000_000]).unwrap();
    std::fs::write(base.join("source/sub/b"), vec![9_u8; 700_000]).unwrap();
    let image = base.join("dest.dmg");
    let created = Command::new("hdiutil")
        .args([
            "create", "-size", "64m", "-fs", "APFS", "-volname", "bl59", "-o",
        ])
        .arg(&image)
        .status()
        .unwrap();
    assert!(created.success());
    let attached = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-mountpoint"])
        .arg(&mount)
        .arg(&image)
        .status()
        .unwrap();
    assert!(attached.success());
    let destination = mount.join("dest");
    std::fs::create_dir(&destination).unwrap();
    let before = Counters::snapshot();
    let outcome = copy(
        &base.join("source"),
        &destination,
        &base.join("source-state"),
        &base.join("destination-state"),
    );
    let counted = Counters::snapshot().since(before);
    let other_device =
        std::fs::metadata(&destination).unwrap().dev() != std::fs::metadata(&base).unwrap().dev();
    let _ = Command::new("hdiutil").arg("detach").arg(&mount).status();
    let _ = std::fs::remove_dir_all(&base);
    let stats = outcome.unwrap();
    assert!(other_device, "the disk image is a separate device");
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert!(
        counted.get(Counter::FlushFull) >= 1,
        "no full flush on the destination device: {}",
        counted.render()
    );
}
