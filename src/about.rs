//! Program identity, shared by `--version`, `--help` and the About page.

pub const NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
/// Minimum supported Rust version, from `rust-version` in Cargo.toml.
pub const RUST_VERSION: &str = env!("CARGO_PKG_RUST_VERSION");
