//! Fails the build on any target outside the supported platform contract.
//!
//! macOS arm64 is the only supported target (see `docs/adr/0001-mvp-boundary.md`). The check runs
//! at build time so an unsupported binary is never produced, rather than failing at first use.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if arch != "aarch64" || os != "macos" {
        let target = std::env::var("TARGET").unwrap_or_else(|_| format!("{arch}-{os}"));
        println!(
            "cargo::error=agent-jit supports macOS arm64 only; refusing to build for `{target}`"
        );
    }
}
