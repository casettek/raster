//! A count of tile executions, available in **both** authentication modes.
//!
//! The execution profile (`profiling`) is refused for an unauthenticated run
//! because its *timings* describe a different program. A count does not: a
//! tile either ran or it did not, and `--no-auth` deletes storage and hashing
//! work, not tile invocations. So the census is a separate, cheaper artifact —
//! one `u64` per tile name, written when the program finishes.
//!
//! Off unless asked for: `RASTER_TILE_CENSUS_PATH` names the file, or
//! `RASTER_TILE_CENSUS=1` writes `tile_census.json` beside the run's other
//! artifacts in `RASTER_OUTPUT_DIR` — which is what a chain wants, since every
//! stage inherits one environment but owns a different output directory.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub const TILE_CENSUS_PATH_ENV: &str = "RASTER_TILE_CENSUS_PATH";
pub const TILE_CENSUS_ENV: &str = "RASTER_TILE_CENSUS";

fn census_path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        if let Some(path) = std::env::var_os(TILE_CENSUS_PATH_ENV) {
            return Some(PathBuf::from(path));
        }
        if std::env::var_os(TILE_CENSUS_ENV).is_none_or(|value| value == "0") {
            return None;
        }
        std::env::var_os(crate::tracing::OUTPUT_DIR_ENV)
            .map(|dir| PathBuf::from(dir).join("tile_census.json"))
    })
    .as_ref()
}

fn counts() -> &'static Mutex<BTreeMap<String, u64>> {
    static COUNTS: OnceLock<Mutex<BTreeMap<String, u64>>> = OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn events() -> &'static Mutex<BTreeMap<&'static str, u64>> {
    static EVENTS: OnceLock<Mutex<BTreeMap<&'static str, u64>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record one tile execution. A no-op unless the census was asked for.
pub fn note_tile_execution(tile_id: &str) {
    if census_path().is_none() {
        return;
    }
    let mut counts = counts().lock().expect("tile census poisoned");
    *counts.entry(tile_id.to_string()).or_insert(0) += 1;
}

/// Record one published trace event, by variant.
///
/// Counted separately from tiles because the trace item count — not the tile
/// count — is what a `TraceCommitment`'s fingerprint is charged per: one
/// `bits_per_item` slot each. A stage's commitment size is read against this.
pub fn note_trace_event(variant: &'static str) {
    if census_path().is_none() {
        return;
    }
    let mut events = events().lock().expect("tile census poisoned");
    *events.entry(variant).or_insert(0) += 1;
}

/// Write the census, if one was asked for. Called from [`crate::finish`].
pub fn finish() {
    let Some(path) = census_path() else {
        return;
    };
    let counts = counts().lock().expect("tile census poisoned");
    let events = events().lock().expect("tile census poisoned");
    let total: u64 = counts.values().sum();
    let total_events: u64 = events.values().sum();
    let mut json = String::from("{\n  \"total_tile_executions\": ");
    json.push_str(&total.to_string());
    json.push_str(",\n  \"total_trace_events\": ");
    json.push_str(&total_events.to_string());
    json.push_str(",\n  \"trace_events\": {");
    for (index, (name, count)) in events.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str("\n    \"");
        json.push_str(name);
        json.push_str("\": ");
        json.push_str(&count.to_string());
    }
    json.push_str("\n  },\n  \"tiles\": {");
    for (index, (name, count)) in counts.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str("\n    \"");
        json.push_str(name);
        json.push_str("\": ");
        json.push_str(&count.to_string());
    }
    json.push_str("\n  }\n}\n");
    if let Err(error) = std::fs::write(path, json) {
        panic!(
            "Failed to write tile census to {}: {error}",
            path.display()
        );
    }
}
