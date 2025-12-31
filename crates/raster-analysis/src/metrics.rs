use serde::{Deserialize, Serialize};
use raster_core::tile::TileId;
use std::collections::HashMap;

/// Performance metrics extracted from execution traces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub total_duration_ns: u64,
    pub tile_metrics: HashMap<TileId, TileMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileMetrics {
    pub invocations: u64,
    pub total_duration_ns: u64,
    pub avg_duration_ns: u64,
    pub estimated_cycles: Option<u64>,
}
