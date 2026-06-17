use raster_core::cfs::CfsCoordinates;
use raster_core::tile::TileId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Performance metrics extracted from execution traces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub total_duration_ns: u64,
    #[serde(default)]
    pub program_total_known: bool,
    #[serde(default)]
    pub total_tile_invocations: u64,
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
    pub total_tile_scope_enter_ns: u64,
    #[serde(default)]
    pub total_tile_output_record_build_ns: u64,
    #[serde(default)]
    pub total_tile_trace_event_publish_ns: u64,
    #[serde(default)]
    pub total_tile_output_coordinate_publish_ns: u64,
    #[serde(default)]
    pub total_tile_other_wrapper_ns: u64,
    #[serde(default)]
    pub total_sequence_invocations: u64,
    pub total_sequence_self_duration_ns: u64,
    #[serde(default)]
    pub total_sequence_body_self_ns: u64,
    #[serde(default)]
    pub total_sequence_scope_enter_ns: u64,
    #[serde(default)]
    pub total_sequence_synthetic_coordinate_alloc_ns: u64,
    #[serde(default)]
    pub total_sequence_input_trace_ns: u64,
    #[serde(default)]
    pub total_sequence_start_event_publish_ns: u64,
    #[serde(default)]
    pub total_sequence_output_trace_ns: u64,
    #[serde(default)]
    pub total_sequence_end_event_publish_ns: u64,
    #[serde(default)]
    pub total_sequence_other_wrapper_ns: u64,
    #[serde(default)]
    pub latest_tile_stats: Option<LatestTileStats>,
    pub tile_metrics: HashMap<TileId, TileMetrics>,
    pub sequence_metrics: HashMap<String, SequenceMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestTileStats {
    pub invocation_index: u64,
    pub tile_id: String,
    pub coordinates: CfsCoordinates,
    pub total_duration_ns: u64,
    pub user_duration_ns: u64,
    pub raster_overhead_ns: u64,
    pub external_input_resolve_ns: u64,
    pub internal_input_resolve_ns: u64,
    pub output_store_ns: u64,
    pub trace_serialize_ns: u64,
    pub draft_capture_ns: u64,
    pub scope_enter_ns: u64,
    pub output_record_build_ns: u64,
    pub trace_event_publish_ns: u64,
    pub output_coordinate_publish_ns: u64,
    pub other_wrapper_ns: u64,
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
    pub total_scope_enter_ns: u64,
    #[serde(default)]
    pub avg_scope_enter_ns: u64,
    #[serde(default)]
    pub total_output_record_build_ns: u64,
    #[serde(default)]
    pub avg_output_record_build_ns: u64,
    #[serde(default)]
    pub total_trace_event_publish_ns: u64,
    #[serde(default)]
    pub avg_trace_event_publish_ns: u64,
    #[serde(default)]
    pub total_output_coordinate_publish_ns: u64,
    #[serde(default)]
    pub avg_output_coordinate_publish_ns: u64,
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
    #[serde(default)]
    pub total_body_self_ns: u64,
    #[serde(default)]
    pub avg_body_self_ns: u64,
    #[serde(default)]
    pub total_scope_enter_ns: u64,
    #[serde(default)]
    pub avg_scope_enter_ns: u64,
    #[serde(default)]
    pub total_synthetic_coordinate_alloc_ns: u64,
    #[serde(default)]
    pub avg_synthetic_coordinate_alloc_ns: u64,
    #[serde(default)]
    pub total_input_trace_ns: u64,
    #[serde(default)]
    pub avg_input_trace_ns: u64,
    #[serde(default)]
    pub total_start_event_publish_ns: u64,
    #[serde(default)]
    pub avg_start_event_publish_ns: u64,
    #[serde(default)]
    pub total_output_trace_ns: u64,
    #[serde(default)]
    pub avg_output_trace_ns: u64,
    #[serde(default)]
    pub total_end_event_publish_ns: u64,
    #[serde(default)]
    pub avg_end_event_publish_ns: u64,
    #[serde(default)]
    pub total_other_wrapper_ns: u64,
    #[serde(default)]
    pub avg_other_wrapper_ns: u64,
}
