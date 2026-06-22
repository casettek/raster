use crate::metrics::Metrics;
use crate::metrics::{LatestTileStats, SequenceMetrics, TileMetrics};
use raster_core::Result;
use raster_runtime::{ExecutionProfile, ProfileRecord};
use std::fs;
use std::path::Path;

/// Analyzes execution traces to extract performance metrics.
pub struct Analyzer {
    profile: ExecutionProfile,
}

impl Analyzer {
    pub fn new(profile: ExecutionProfile) -> Self {
        Self { profile }
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let profile = Self::load_profile(path)?;
        Ok(Self::new(profile))
    }

    pub fn load_profile(path: impl AsRef<Path>) -> Result<ExecutionProfile> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(raster_core::Error::Io)?;
        serde_json::from_slice(&bytes).map_err(|error| {
            raster_core::Error::Serialization(format!(
                "Failed to decode execution profile '{}': {}",
                path.display(),
                error
            ))
        })
    }

    /// Analyze an execution profile and produce metrics.
    pub fn analyze(&self) -> Result<Metrics> {
        let mut metrics = Metrics {
            run_id: self.profile.run_id.clone(),
            total_duration_ns: self.profile.program_total_duration_ns.unwrap_or_default(),
            program_total_known: self.profile.program_total_duration_ns.is_some(),
            profile_record_count: u64::try_from(self.profile.records.len()).unwrap_or(u64::MAX),
            ..Metrics::default()
        };

        for record in &self.profile.records {
            metrics.max_nesting_depth = metrics.max_nesting_depth.max(record_depth(record));
            ingest_record(&mut metrics, record);
        }

        finalize_tile_averages(&mut metrics.tile_metrics);
        finalize_sequence_averages(&mut metrics.sequence_metrics);

        Ok(metrics)
    }
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new(ExecutionProfile::new(Vec::new(), None, None))
    }
}

fn ingest_record(metrics: &mut Metrics, record: &ProfileRecord) {
    match record {
        ProfileRecord::Tile(record) => ingest_tile_record(metrics, record),
        ProfileRecord::Sequence(record) => ingest_sequence_record(metrics, record),
    }
}

fn record_depth(record: &ProfileRecord) -> u32 {
    match record {
        ProfileRecord::Tile(record) => record.depth,
        ProfileRecord::Sequence(record) => record.depth,
    }
}

fn ingest_tile_record(metrics: &mut Metrics, record: &raster_runtime::TileProfileRecord) {
    metrics.total_tile_invocations += 1;
    metrics.total_tile_duration_ns = metrics
        .total_tile_duration_ns
        .saturating_add(record.total_duration_ns);
    metrics.total_tile_user_duration_ns = metrics
        .total_tile_user_duration_ns
        .saturating_add(record.user_duration_ns);
    metrics.total_tile_raster_overhead_ns = metrics
        .total_tile_raster_overhead_ns
        .saturating_add(record.raster_overhead_ns);
    metrics.total_tile_external_input_resolve_ns = metrics
        .total_tile_external_input_resolve_ns
        .saturating_add(record.external_input_resolve_ns);
    metrics.total_tile_internal_input_resolve_ns = metrics
        .total_tile_internal_input_resolve_ns
        .saturating_add(record.internal_input_resolve_ns);
    metrics.total_tile_output_store_ns = metrics
        .total_tile_output_store_ns
        .saturating_add(record.output_store_ns);
    metrics.total_tile_trace_serialize_ns = metrics
        .total_tile_trace_serialize_ns
        .saturating_add(record.trace_serialize_ns);
    metrics.total_tile_draft_capture_ns = metrics
        .total_tile_draft_capture_ns
        .saturating_add(record.draft_capture_ns);
    metrics.total_tile_scope_enter_ns = metrics
        .total_tile_scope_enter_ns
        .saturating_add(record.scope_enter_ns);
    metrics.total_tile_output_record_build_ns = metrics
        .total_tile_output_record_build_ns
        .saturating_add(record.output_record_build_ns);
    metrics.total_tile_trace_event_publish_ns = metrics
        .total_tile_trace_event_publish_ns
        .saturating_add(record.trace_event_publish_ns);
    metrics.total_tile_output_coordinate_publish_ns = metrics
        .total_tile_output_coordinate_publish_ns
        .saturating_add(record.output_coordinate_publish_ns);
    metrics.total_tile_other_wrapper_ns = metrics
        .total_tile_other_wrapper_ns
        .saturating_add(record.other_wrapper_ns);
    metrics.latest_tile_stats = Some(LatestTileStats {
        invocation_index: record.invocation_index,
        tile_id: record.tile_id.clone(),
        coordinates: record.coordinates.clone(),
        total_duration_ns: record.total_duration_ns,
        user_duration_ns: record.user_duration_ns,
        raster_overhead_ns: record.raster_overhead_ns,
        external_input_resolve_ns: record.external_input_resolve_ns,
        internal_input_resolve_ns: record.internal_input_resolve_ns,
        output_store_ns: record.output_store_ns,
        trace_serialize_ns: record.trace_serialize_ns,
        draft_capture_ns: record.draft_capture_ns,
        scope_enter_ns: record.scope_enter_ns,
        output_record_build_ns: record.output_record_build_ns,
        trace_event_publish_ns: record.trace_event_publish_ns,
        output_coordinate_publish_ns: record.output_coordinate_publish_ns,
        other_wrapper_ns: record.other_wrapper_ns,
    });

    let tile_metrics = metrics
        .tile_metrics
        .entry(raster_core::tile::TileId::new(record.tile_id.clone()))
        .or_default();
    tile_metrics.invocations += 1;
    tile_metrics.total_duration_ns = tile_metrics
        .total_duration_ns
        .saturating_add(record.total_duration_ns);
    tile_metrics.total_user_duration_ns = tile_metrics
        .total_user_duration_ns
        .saturating_add(record.user_duration_ns);
    tile_metrics.total_raster_overhead_ns = tile_metrics
        .total_raster_overhead_ns
        .saturating_add(record.raster_overhead_ns);
    tile_metrics.total_external_input_resolve_ns = tile_metrics
        .total_external_input_resolve_ns
        .saturating_add(record.external_input_resolve_ns);
    tile_metrics.total_internal_input_resolve_ns = tile_metrics
        .total_internal_input_resolve_ns
        .saturating_add(record.internal_input_resolve_ns);
    tile_metrics.total_output_store_ns = tile_metrics
        .total_output_store_ns
        .saturating_add(record.output_store_ns);
    tile_metrics.total_trace_serialize_ns = tile_metrics
        .total_trace_serialize_ns
        .saturating_add(record.trace_serialize_ns);
    tile_metrics.total_draft_capture_ns = tile_metrics
        .total_draft_capture_ns
        .saturating_add(record.draft_capture_ns);
    tile_metrics.total_scope_enter_ns = tile_metrics
        .total_scope_enter_ns
        .saturating_add(record.scope_enter_ns);
    tile_metrics.total_output_record_build_ns = tile_metrics
        .total_output_record_build_ns
        .saturating_add(record.output_record_build_ns);
    tile_metrics.total_trace_event_publish_ns = tile_metrics
        .total_trace_event_publish_ns
        .saturating_add(record.trace_event_publish_ns);
    tile_metrics.total_output_coordinate_publish_ns = tile_metrics
        .total_output_coordinate_publish_ns
        .saturating_add(record.output_coordinate_publish_ns);
    tile_metrics.total_other_wrapper_ns = tile_metrics
        .total_other_wrapper_ns
        .saturating_add(record.other_wrapper_ns);
}

fn ingest_sequence_record(metrics: &mut Metrics, record: &raster_runtime::SequenceProfileRecord) {
    metrics.total_sequence_invocations += 1;
    metrics.total_sequence_self_duration_ns = metrics
        .total_sequence_self_duration_ns
        .saturating_add(record.self_duration_ns);
    metrics.total_sequence_body_self_ns = metrics
        .total_sequence_body_self_ns
        .saturating_add(record.self_breakdown.body_self_ns);
    metrics.total_sequence_scope_enter_ns = metrics
        .total_sequence_scope_enter_ns
        .saturating_add(record.self_breakdown.scope_enter_ns);
    metrics.total_sequence_synthetic_coordinate_alloc_ns = metrics
        .total_sequence_synthetic_coordinate_alloc_ns
        .saturating_add(record.self_breakdown.synthetic_coordinate_alloc_ns);
    metrics.total_sequence_input_trace_ns = metrics
        .total_sequence_input_trace_ns
        .saturating_add(record.self_breakdown.input_trace_ns);
    metrics.total_sequence_start_event_publish_ns = metrics
        .total_sequence_start_event_publish_ns
        .saturating_add(record.self_breakdown.start_event_publish_ns);
    metrics.total_sequence_output_trace_ns = metrics
        .total_sequence_output_trace_ns
        .saturating_add(record.self_breakdown.output_trace_ns);
    metrics.total_sequence_end_event_publish_ns = metrics
        .total_sequence_end_event_publish_ns
        .saturating_add(record.self_breakdown.end_event_publish_ns);
    metrics.total_sequence_other_wrapper_ns = metrics
        .total_sequence_other_wrapper_ns
        .saturating_add(record.self_breakdown.other_wrapper_ns);

    let sequence_metrics = metrics
        .sequence_metrics
        .entry(record.sequence_id.clone())
        .or_default();
    sequence_metrics.invocations += 1;
    sequence_metrics.total_duration_ns = sequence_metrics
        .total_duration_ns
        .saturating_add(record.total_duration_ns);
    sequence_metrics.total_self_duration_ns = sequence_metrics
        .total_self_duration_ns
        .saturating_add(record.self_duration_ns);
    sequence_metrics.total_child_duration_ns = sequence_metrics
        .total_child_duration_ns
        .saturating_add(record.child_duration_ns);
    sequence_metrics.total_body_self_ns = sequence_metrics
        .total_body_self_ns
        .saturating_add(record.self_breakdown.body_self_ns);
    sequence_metrics.total_scope_enter_ns = sequence_metrics
        .total_scope_enter_ns
        .saturating_add(record.self_breakdown.scope_enter_ns);
    sequence_metrics.total_synthetic_coordinate_alloc_ns = sequence_metrics
        .total_synthetic_coordinate_alloc_ns
        .saturating_add(record.self_breakdown.synthetic_coordinate_alloc_ns);
    sequence_metrics.total_input_trace_ns = sequence_metrics
        .total_input_trace_ns
        .saturating_add(record.self_breakdown.input_trace_ns);
    sequence_metrics.total_start_event_publish_ns = sequence_metrics
        .total_start_event_publish_ns
        .saturating_add(record.self_breakdown.start_event_publish_ns);
    sequence_metrics.total_output_trace_ns = sequence_metrics
        .total_output_trace_ns
        .saturating_add(record.self_breakdown.output_trace_ns);
    sequence_metrics.total_end_event_publish_ns = sequence_metrics
        .total_end_event_publish_ns
        .saturating_add(record.self_breakdown.end_event_publish_ns);
    sequence_metrics.total_other_wrapper_ns = sequence_metrics
        .total_other_wrapper_ns
        .saturating_add(record.self_breakdown.other_wrapper_ns);
}

fn finalize_tile_averages(
    tile_metrics: &mut std::collections::HashMap<raster_core::tile::TileId, TileMetrics>,
) {
    for metrics in tile_metrics.values_mut() {
        if metrics.invocations == 0 {
            continue;
        }
        metrics.avg_duration_ns = metrics.total_duration_ns / metrics.invocations;
        metrics.avg_user_duration_ns = metrics.total_user_duration_ns / metrics.invocations;
        metrics.avg_raster_overhead_ns = metrics.total_raster_overhead_ns / metrics.invocations;
        metrics.avg_external_input_resolve_ns =
            metrics.total_external_input_resolve_ns / metrics.invocations;
        metrics.avg_internal_input_resolve_ns =
            metrics.total_internal_input_resolve_ns / metrics.invocations;
        metrics.avg_output_store_ns = metrics.total_output_store_ns / metrics.invocations;
        metrics.avg_trace_serialize_ns = metrics.total_trace_serialize_ns / metrics.invocations;
        metrics.avg_draft_capture_ns = metrics.total_draft_capture_ns / metrics.invocations;
        metrics.avg_scope_enter_ns = metrics.total_scope_enter_ns / metrics.invocations;
        metrics.avg_output_record_build_ns =
            metrics.total_output_record_build_ns / metrics.invocations;
        metrics.avg_trace_event_publish_ns =
            metrics.total_trace_event_publish_ns / metrics.invocations;
        metrics.avg_output_coordinate_publish_ns =
            metrics.total_output_coordinate_publish_ns / metrics.invocations;
        metrics.avg_other_wrapper_ns = metrics.total_other_wrapper_ns / metrics.invocations;
    }
}

fn finalize_sequence_averages(
    sequence_metrics: &mut std::collections::HashMap<String, SequenceMetrics>,
) {
    for metrics in sequence_metrics.values_mut() {
        if metrics.invocations == 0 {
            continue;
        }
        metrics.avg_duration_ns = metrics.total_duration_ns / metrics.invocations;
        metrics.avg_self_duration_ns = metrics.total_self_duration_ns / metrics.invocations;
        metrics.avg_body_self_ns = metrics.total_body_self_ns / metrics.invocations;
        metrics.avg_scope_enter_ns = metrics.total_scope_enter_ns / metrics.invocations;
        metrics.avg_synthetic_coordinate_alloc_ns =
            metrics.total_synthetic_coordinate_alloc_ns / metrics.invocations;
        metrics.avg_input_trace_ns = metrics.total_input_trace_ns / metrics.invocations;
        metrics.avg_start_event_publish_ns =
            metrics.total_start_event_publish_ns / metrics.invocations;
        metrics.avg_output_trace_ns = metrics.total_output_trace_ns / metrics.invocations;
        metrics.avg_end_event_publish_ns = metrics.total_end_event_publish_ns / metrics.invocations;
        metrics.avg_other_wrapper_ns = metrics.total_other_wrapper_ns / metrics.invocations;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_runtime::{
        ExecutionProfile, ProfileRecord, SequenceProfileRecord, SequenceProfileSelfBreakdown,
        TileProfileRecord,
    };

    #[test]
    fn aggregates_tile_and_sequence_metrics() {
        let profile = ExecutionProfile::new(
            vec![
                ProfileRecord::Tile(TileProfileRecord {
                    invocation_index: 1,
                    tile_id: "alpha".to_string(),
                    depth: 1,
                    coordinates: raster_core::cfs::CfsCoordinates(vec![0]),
                    total_duration_ns: 20,
                    user_duration_ns: 12,
                    raster_overhead_ns: 8,
                    external_input_resolve_ns: 3,
                    internal_input_resolve_ns: 1,
                    output_store_ns: 2,
                    trace_serialize_ns: 1,
                    draft_capture_ns: 0,
                    scope_enter_ns: 1,
                    output_record_build_ns: 1,
                    trace_event_publish_ns: 0,
                    output_coordinate_publish_ns: 0,
                    other_wrapper_ns: 1,
                }),
                ProfileRecord::Tile(TileProfileRecord {
                    invocation_index: 2,
                    tile_id: "alpha".to_string(),
                    depth: 2,
                    coordinates: raster_core::cfs::CfsCoordinates(vec![1]),
                    total_duration_ns: 10,
                    user_duration_ns: 6,
                    raster_overhead_ns: 4,
                    external_input_resolve_ns: 1,
                    internal_input_resolve_ns: 0,
                    output_store_ns: 1,
                    trace_serialize_ns: 1,
                    draft_capture_ns: 1,
                    scope_enter_ns: 0,
                    output_record_build_ns: 0,
                    trace_event_publish_ns: 1,
                    output_coordinate_publish_ns: 0,
                    other_wrapper_ns: 0,
                }),
                ProfileRecord::Sequence(SequenceProfileRecord {
                    invocation_index: 3,
                    sequence_id: "main".to_string(),
                    depth: 0,
                    total_duration_ns: 40,
                    self_duration_ns: 10,
                    child_duration_ns: 30,
                    self_breakdown: SequenceProfileSelfBreakdown {
                        body_self_ns: 4,
                        scope_enter_ns: 1,
                        synthetic_coordinate_alloc_ns: 2,
                        input_trace_ns: 1,
                        start_event_publish_ns: 1,
                        output_trace_ns: 1,
                        end_event_publish_ns: 0,
                        other_wrapper_ns: 0,
                    },
                }),
            ],
            Some(40),
            Some("run-1".to_string()),
        );

        let analyzer = Analyzer::new(profile);
        let metrics = analyzer.analyze().unwrap();

        let tile_metrics = metrics
            .tile_metrics
            .get(&raster_core::tile::TileId::from("alpha"))
            .unwrap();
        assert_eq!(metrics.total_duration_ns, 40);
        assert_eq!(metrics.run_id.as_deref(), Some("run-1"));
        assert_eq!(metrics.profile_record_count, 3);
        assert_eq!(metrics.max_nesting_depth, 2);
        assert_eq!(metrics.total_tile_duration_ns, 30);
        assert_eq!(metrics.total_tile_invocations, 2);
        assert_eq!(metrics.total_tile_user_duration_ns, 18);
        assert_eq!(metrics.total_tile_raster_overhead_ns, 12);
        assert_eq!(metrics.total_tile_external_input_resolve_ns, 4);
        assert_eq!(metrics.total_tile_internal_input_resolve_ns, 1);
        assert_eq!(metrics.total_tile_output_store_ns, 3);
        assert_eq!(metrics.total_tile_scope_enter_ns, 1);
        assert_eq!(metrics.total_tile_output_record_build_ns, 1);
        assert_eq!(metrics.total_tile_trace_event_publish_ns, 1);
        assert_eq!(tile_metrics.invocations, 2);
        assert_eq!(tile_metrics.avg_duration_ns, 15);
        assert_eq!(tile_metrics.avg_user_duration_ns, 9);
        assert_eq!(tile_metrics.avg_raster_overhead_ns, 6);
        assert_eq!(tile_metrics.avg_output_store_ns, 1);
        assert_eq!(tile_metrics.avg_trace_event_publish_ns, 0);
        let latest_tile_stats = metrics.latest_tile_stats.as_ref().unwrap();
        assert_eq!(latest_tile_stats.invocation_index, 2);
        assert_eq!(latest_tile_stats.tile_id, "alpha");
        assert_eq!(latest_tile_stats.total_duration_ns, 10);

        let sequence_metrics = metrics.sequence_metrics.get("main").unwrap();
        assert_eq!(sequence_metrics.total_self_duration_ns, 10);
        assert_eq!(sequence_metrics.total_child_duration_ns, 30);
        assert_eq!(metrics.total_sequence_invocations, 1);
        assert_eq!(metrics.total_sequence_body_self_ns, 4);
        assert_eq!(metrics.total_sequence_synthetic_coordinate_alloc_ns, 2);
        assert_eq!(sequence_metrics.avg_input_trace_ns, 1);
    }
}
