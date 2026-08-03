//! A bare `cargo build` binary has no app bundle, so macOS TCC has nowhere to
//! read `NSBluetoothAlwaysUsageDescription` from and hard crashes (SIGABRT)
//! instead of prompting when the daemon's BMAP sweep touches IOBluetooth.
//! Embedding Info.plist into the Mach-O `__TEXT,__info_plist` section is the
//! standard workaround, verified end to end by the `poc-bmap-battery` spike
//! this feature is built on: under launchd, which runs this exact binary,
//! TCC reads the description from here and blubat is the process attributed
//! rather than the terminal that would otherwise take the blame (and the
//! SIGABRT) for a permission blubat itself is asking for.
//!
//! `otool -l $(cargo build ...)` on the built binary should show an
//! `__info_plist` section; `blubat/tests/embedded_plist.rs` asserts exactly
//! that.
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let plist_path = std::path::Path::new(&manifest_dir).join("Info.plist");

    println!("cargo:rerun-if-changed={}", plist_path.display());
    println!(
        "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist_path.display()
    );
}
