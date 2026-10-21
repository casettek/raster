//! Guest-side code the transition and control-flow guests share: the framed
//! input reader, the trace tree's hashing and frontier plumbing, and the input
//! source anchor.
//!
//! A guest-side crate rather than `raster-core`, because hashing here uses
//! risc0's accelerated SHA (`Risc0Sha256`) where `raster-core` uses software
//! `sha2`, and because leaving `raster-core` alone leaves tile image ids alone.

pub mod frame;
pub mod input_source;
pub mod merkle_tree;
