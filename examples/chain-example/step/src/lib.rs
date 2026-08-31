//! One iteration of the dynamic chain's repeat block.
//!
//! Deliberately trivial: what the fixture exercises is the *shape* — that a
//! block expands to as many of these as the planner asked for, and that each
//! one is fed by the previous, with the block's entry edge fed by the planner
//! itself. See `docs/proposals/chain-repeat.md` §2.
//!
//! `no_std` so the tiles compile into RISC0 replay guests.

#![no_std]

use raster::prelude::*;

/// Advance the carried value by one.
#[tile(description = "Advance one step")]
pub fn advance(prev: u64) -> u64 {
    prev + 1
}
