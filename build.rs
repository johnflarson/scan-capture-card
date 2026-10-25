//! The web UI (`ui/dist`, built by Trunk) is embedded into the binary. The
//! folder is git-ignored, so create it when missing: the server then builds
//! and explains at runtime that the UI wasn't bundled.

fn main() {
    let dist = std::path::Path::new("ui/dist");
    if !dist.exists() {
        std::fs::create_dir_all(dist).expect("failed to create ui/dist");
    }
    // Re-embed when the UI is rebuilt.
    println!("cargo:rerun-if-changed=ui/dist");
    // How the build was made, for the usage statistics (`telemetry`).
    println!("cargo:rerun-if-env-changed=MTG_SCAN_BUILD");
}
