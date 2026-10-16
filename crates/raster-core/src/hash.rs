//! SHA-256 for raster-core: RISC Zero's accelerated implementation when built
//! for the zkVM, RustCrypto `sha2` on the host. Both compute standard SHA-256,
//! so every commitment is byte-identical on either side.
//!
//! Use `crate::hash::Sha256` everywhere instead of `sha2::Sha256` — the latter
//! runs as plain RV32 code inside a guest (`tests/hash_guard.rs` enforces it).

pub use sha2::Digest;

#[cfg(target_os = "zkvm")]
pub use risc0_zkvm::sha::rust_crypto::Sha256;

#[cfg(not(target_os = "zkvm"))]
pub use sha2::Sha256;
