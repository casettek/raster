//! Guest-side code the transition, control-flow and storage guests share: the
//! framed input reader, the trace tree's hashing and frontier plumbing, the
//! input source anchor, and the storage check.
//!
//! A guest-side crate rather than `raster-core`, because hashing here uses
//! risc0's accelerated SHA (`Risc0Sha256`) where `raster-core` uses software
//! `sha2`, and because leaving `raster-core` alone leaves tile image ids alone.

pub mod frame;
pub mod input_source;
pub mod merkle_tree;
pub mod storage;
#[cfg(feature = "test-fixtures")]
pub mod storage_fixtures;
