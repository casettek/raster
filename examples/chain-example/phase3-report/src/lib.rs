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

/// Set the report's title. Set-once, so this runs exactly once.
#[tile(description = "Set the report title")]
pub fn set_report_title(label: String, draft: Draft<Report>) -> Draft<Report> {
    let mut draft = draft;
    draft.title().set(format!("Pipeline report for {label}"));
    draft
}

/// Append one `name: value` line. Two small scalars in, one line appended —
/// the draft pays for the increment, not for re-committing the whole report.
#[tile(description = "Append one metric line to the report")]
pub fn push_metric(name: String, value: u64, draft: Draft<Report>) -> Draft<Report> {
    let mut draft = draft;
    draft.lines().push(format!("{name:<8}: {value}"));
    draft
}

/// Append the mean, formatted back from its scaled integer form.
#[tile(description = "Append the formatted mean line to the report")]
pub fn push_mean(mean_scaled: u64, draft: Draft<Report>) -> Draft<Report> {
    let mut draft = draft;
    draft.lines().push(format!(
        "{:<8}: {}.{:02}",
        "mean",
        mean_scaled / 100,
        mean_scaled % 100
    ));
    draft
}
