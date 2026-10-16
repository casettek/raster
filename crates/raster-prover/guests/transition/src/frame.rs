//! Host inputs, read as frames.
//!
//! The host (`raster_prover::transition::write_frame`) sends every input as a
//! `u32` byte length followed by the bytes; structured inputs are postcard
//! inside the frame. A frame costs two bulk `read_slice` syscalls, and decoding
//! is ordinary guest code.
//!
//! Not `env::read()`: risc0's serde fetches one word per `sys_read_words` call,
//! one per `u8` of a `Vec<u8>`, and on risc0 3.x every syscall traps through
//! the v1compat kernel. That was 68% of a transition step's cycles.
//! `env::read_frame` does the same as `read_bytes`, but is behind risc0's
//! `unstable` feature.

use risc0_zkvm::guest::env;
use serde::de::DeserializeOwned;

/// Read one frame's raw bytes.
pub fn read_bytes() -> Vec<u8> {
    let mut len = [0u32; 1];
    env::read_slice(&mut len);
    let mut bytes = vec![0u8; len[0] as usize];
    env::read_slice(&mut bytes);
    bytes
}

/// Read one frame and decode it as postcard.
pub fn read<T: DeserializeOwned>() -> T {
    postcard::from_bytes(&read_bytes()).expect("host must supply a postcard frame")
}
