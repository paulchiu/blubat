//! Verifies the Info.plist `build.rs` embeds, with nothing here touching
//! Bluetooth or needing TCC permission of its own.

use std::process::Command;

/// `build.rs` embeds Info.plist into the `__TEXT,__info_plist` Mach-O
/// section so TCC has an `NSBluetoothAlwaysUsageDescription` to read when
/// launchd runs this binary; see `daemon::bmap` for why that matters.
/// `otool -l` is the cheapest way to confirm the section actually landed in
/// what got built.
#[test]
fn the_binary_carries_an_embedded_info_plist_section() {
    let binary = env!("CARGO_BIN_EXE_blubat");
    let output = Command::new("otool")
        .args(["-l", binary])
        .output()
        .expect("otool is part of the Xcode command line tools");

    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(
        printed.contains("__info_plist"),
        "no __TEXT,__info_plist section in {binary}: {printed}"
    );
}
