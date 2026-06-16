use raster_core::tile::TileId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Performance metrics extracted from execution traces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub total_duration_ns: u64,
    #[serde(default)]
    pub program_total_known: bool,
    pub total_tile_duration_ns: u64,
    pub total_tile_user_duration_ns: u64,
    pub total_tile_raster_overhead_ns: u64,
    #[serde(default)]
    pub total_tile_external_input_resolve_ns: u64,
    #[serde(default)]
    pub total_tile_internal_input_resolve_ns: u64,
    #[serde(default)]
    pub total_tile_output_store_ns: u64,
    #[serde(default)]
    pub total_tile_trace_serialize_ns: u64,
    #[serde(default)]
    pub total_tile_draft_capture_ns: u64,
    #[serde(default)]
    pub total_tile_other_wrapper_ns: u64,
    pub total_sequence_self_duration_ns: u64,
    pub tile_metrics: HashMap<TileId, TileMetrics>,
    pub sequence_metrics: HashMap<String, SequenceMetrics>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TileMetrics {
    pub invocations: u64,
    pub total_duration_ns: u64,
    pub avg_duration_ns: u64,
    pub total_user_duration_ns: u64,
    pub avg_user_duration_ns: u64,
    pub total_raster_overhead_ns: u64,
    pub avg_raster_overhead_ns: u64,
    #[serde(default)]
    pub total_external_input_resolve_ns: u64,
    #[serde(default)]
    pub avg_external_input_resolve_ns: u64,
    #[serde(default)]
    pub total_internal_input_resolve_ns: u64,
    #[serde(default)]
    pub avg_internal_input_resolve_ns: u64,
    #[serde(default)]
    pub total_output_store_ns: u64,
    #[serde(default)]
    pub avg_output_store_ns: u64,
    #[serde(default)]
    pub total_trace_serialize_ns: u64,
    #[serde(default)]
    pub avg_trace_serialize_ns: u64,
    #[serde(default)]
    pub total_draft_capture_ns: u64,
    #[serde(default)]
    pub avg_draft_capture_ns: u64,
    #[serde(default)]
    pub total_other_wrapper_ns: u64,
    #[serde(default)]
    pub avg_other_wrapper_ns: u64,
    pub estimated_cycles: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SequenceMetrics {
    pub invocations: u64,
    pub total_duration_ns: u64,
    pub avg_duration_ns: u64,
    pub total_self_duration_ns: u64,
    pub avg_self_duration_ns: u64,
    pub total_child_duration_ns: u64,
}
