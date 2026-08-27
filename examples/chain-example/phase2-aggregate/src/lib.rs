//! Phase 2 — `aggregate`.
//!
//! Consumes phase 1's `Filtered` output as its committed input and folds the
//! kept samples into summary `Stats`, which become phase 3's input.
//!
//! `no_std` so the tiles compile into RISC0 replay guests.

#![no_std]

extern crate alloc;

use alloc::string::String;
use raster::prelude::*;
use serde::{Deserialize, Serialize};

/// Phase 1 output / phase 2 input. Field layout MUST match `Filtered` in
/// `phase1-normalize` — the chain links the stages by this value's structural
/// commitment.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, raster::Selectable)]
pub struct Filtered {
    pub label: String,
    pub kept: List<u64>,
}

/// Loop-carried accumulator. Scalar-small on purpose: `state` is re-committed
/// on every iteration, so anything that *grows* belongs in an `output` draft
/// instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, raster::Selectable)]
pub struct Acc {
    pub count: u64,
    pub sum: u64,
    pub max: u64,
}

/// Phase 2 output / phase 3 input. Field layout MUST match `Stats` in
/// `phase3-report`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, raster::Selectable)]
pub struct Stats {
    pub label: String,
    pub count: u64,
    pub sum: u64,
    pub max: u64,
}

/// State-only recur tile: reduce the kept samples to `(count, sum, max)`, one
/// sample per replay unit.
#[tile(kind = recur, description = "Fold one sample into the running stats")]
pub fn fold_stats(input: RecurInput<u64>, state: RecurState<Acc>) -> RecurState<Acc> {
    let mut state = state;
    let value = input.into_value();
    state.count += 1;
    state.sum += value;
    if value > state.max {
        state.max = value;
    }
    state
}

/// Combine the carried label with the folded accumulator into `Stats`.
/// Scalars in, scalars out — nothing here iterates.
#[tile(description = "Assemble the folded accumulator into Stats")]
pub fn assemble_stats(label: String, count: u64, sum: u64, max: u64) -> Stats {
    Stats {
        label,
        count,
        sum,
        max,
    }
}
