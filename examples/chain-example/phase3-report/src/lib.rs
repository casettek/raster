//! Phase 3 — `report` (terminal stage).
//!
//! Consumes phase 2's `Stats` as its committed input and formats the pipeline's
//! final `Report`. Being terminal, its output is the chain's result and feeds
//! nothing downstream.
//!
//! Note how the report's `lines` are built: one tile call appends one line,
//! through a `Draft<Report>` threaded across the sequence. A single tile that
//! took the whole `Stats` and built every line at once would be the "fake
//! Raster program" shape — a native function with `#[tile]` on it, and an
//! unbounded amount of work inside one replay unit.
//!
//! `no_std` so the tiles compile into RISC0 replay guests.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use raster::prelude::*;
use serde::{Deserialize, Serialize};

/// Phase 2 output / phase 3 input. Field layout MUST match `Stats` in
/// `phase2-aggregate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, raster::Selectable)]
pub struct Stats {
    pub label: String,
    pub count: u64,
    pub sum: u64,
    pub max: u64,
}

/// The pipeline's final, human-readable result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, raster::Selectable)]
pub struct Report {
    pub title: String,
    pub lines: List<String>,
}

/// Mean of the kept samples, scaled by 100 to stay integer — a deterministic
/// tile has no floating point (§3).
#[tile(description = "Integer mean, scaled by 100")]
pub fn mean_scaled(sum: u64, count: u64) -> u64 {
    if count == 0 {
        0
    } else {
        (sum * 100) / count
    }
}

/// Assemble the report: a title and one line per metric, the mean formatted
/// back from its scaled integer form.
///
/// Drafted in one tile and returned: the tile's close completes it into the
/// `Report` stored at this tile's coordinate. (This was a draft threaded
/// through four tiles and closed with `finalize`, which stored the report at a
/// coordinate no step wrote — `authenticated-chain-draft-output`'s
/// reproducer. A draft now lives inside one tile.)
#[tile(description = "Assemble the pipeline report")]
pub fn build_report(label: String, count: u64, sum: u64, max: u64, mean_scaled: u64) -> Draft<Report> {
    let mut report = Draft::<Report>::new();
    report.title().set(format!("Pipeline report for {label}"));
    report.lines().push(format!("{:<8}: {}", "count", count));
    report.lines().push(format!("{:<8}: {}", "sum", sum));
    report.lines().push(format!("{:<8}: {}", "max", max));
    report.lines().push(format!(
        "{:<8}: {}.{:02}",
        "mean",
        mean_scaled / 100,
        mean_scaled % 100
    ));
    report
}
