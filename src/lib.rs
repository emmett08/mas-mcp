//! MCP publication and durable admission around the existing MAS host CLI.
#![forbid(unsafe_code)]
mod catalog;
pub mod config;
mod execution;
pub mod gateway;
mod host;
mod journal;
pub mod server;
pub mod transport;

pub type Result<T> = std::result::Result<T, String>;
pub const MAX_INPUT: usize = 32_768;
pub const MAX_FRAME: usize = 262_144;
pub const MAX_RESULT: usize = 1_048_576;

pub(crate) fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}
