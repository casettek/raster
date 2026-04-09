use alloc::vec::Vec;

#[cfg(target_os = "zkvm")]
use risc0_zkvm::sha::{Impl as Risc0Sha256, Sha256 as _};
#[cfg(not(target_os = "zkvm"))]
use sha2::{Digest, Sha256};

pub fn sha256_bytes(bytes: &[u8]) -> Vec<u8> {
    #[cfg(not(target_os = "zkvm"))]
    {
        Sha256::digest(bytes).to_vec()
    }

    #[cfg(target_os = "zkvm")]
    {
        Risc0Sha256::hash_bytes(bytes).as_bytes().to_vec()
    }
}
