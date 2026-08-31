//! The planner of the dynamic chain — it decides how long the chain is.
//!
//! Its authorized output is a single `u64` and nothing else. That is the whole
//! interface a `chain-repeat` trip count has
//! (`docs/proposals/chain-repeat.md` §3): the stage's output *is* the count, so
//! a verifier re-encodes the count it was handed and compares one hash against
//! what this stage committed, rather than decoding a payload to find a field.
//!
//! `no_std` so the tiles compile into RISC0 replay guests.

#![no_std]

use raster::prelude::*;

/// Turn a committed budget into a number of steps.
///
/// Arbitrary arithmetic on purpose: the point of the fixture is that the number
/// is not knowable from the manifest, only from running this stage.
#[tile(description = "Decide how many steps this budget calls for")]
pub fn plan_steps(budget: u64) -> u64 {
    budget % 4 + 1
}
