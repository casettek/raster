//! Phase 1 — `normalize`.
//!
//! First stage of the `chain-example` chain. Takes a raw `Measurements` column plus
//! a `threshold` and keeps only the samples at or above it, emitting a
//! `Filtered` value that becomes the authorized input of phase 2.
//!
//! `no_std` so the same tiles compile into RISC0 replay guests.

#![no_std]

extern crate alloc;

use alloc::string::String;
use raster::prelude::*;
use serde::{Deserialize, Serialize};

/// Raw, committed input to the whole pipeline: a labelled column of readings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, raster::Selectable)]
pub struct Measurements {
    pub label: String,
    pub samples: List<u64>,
}

/// Phase 1 output / phase 2 input: the readings that survived the threshold.
///
/// The field layout here MUST match `Filtered` in `phase2-aggregate` — the
/// chain links the two stages by this value's structural commitment, not by
/// Rust type name.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, raster::Selectable)]
pub struct Filtered {
    pub label: String,
    pub kept: List<u64>,
}

/// Recur tile: fold each sample into a growing `Filtered`, keeping only values
/// `>= threshold`. The label is set once, on the first iteration.
///
/// One sample per replay unit — the whole point. The `if` is ordinary
/// computation *inside* a tile, which is fine; it is sequences that stay
/// linear.
#[tile(kind = recur, description = "Keep samples at or above the threshold")]
pub fn keep_above(
    input: RecurInput<u64>,
    output: RecurOutput<Filtered>,
    label: String,
    threshold: u64,
) -> RecurOutput<Filtered> {
    let mut output = output;
    if input.is_first() {
        output.label().set(label);
    }
    let value = input.into_value();
    if value >= threshold {
        output.kept().push(value);
    }
    output
}
