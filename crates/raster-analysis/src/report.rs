use crate::metrics::{Metrics, SequenceMetrics, TileMetrics};
use raster_core::Result;

/// Generates human-readable and machine-readable reports.
pub struct Report {
    metrics: Metrics,
}

impl Report {
    pub fn new(metrics: Metrics) -> Self {
        Self { metrics }
    }

    /// Generate a human-readable text report.
    pub fn to_text(&self) -> String {
        let mut lines = Vec::new();
        let avg_tile_overhead = average_duration(
            self.metrics.total_tile_raster_overhead_ns,
            self.metrics.total_tile_invocations,
        );
        let avg_sequence_self = average_duration(
            self.metrics.total_sequence_self_duration_ns,
            self.metrics.total_sequence_invocations,
        );
        let program_total = if self.metrics.program_total_known {
            format_duration(self.metrics.total_duration_ns)
        } else {
            String::from("pending")
        };
        lines.push(format!("Program total: {}", program_total));
        lines.push(format!(
            "Tile user time: {}",
            format_duration(self.metrics.total_tile_user_duration_ns)
        ));
        lines.push(format!(
            "Raster overhead: {}",
            format_duration(self.metrics.total_tile_raster_overhead_ns)
        ));
        if let Some(avg_tile_overhead) = avg_tile_overhead {
            lines.push(format!(
                "Avg tile overhead: {}",
                format_duration(avg_tile_overhead)
            ));
        }
        lines.push(format!(
            "Sequence self time: {}",
            format_duration(self.metrics.total_sequence_self_duration_ns)
        ));
        if let Some(avg_sequence_self) = avg_sequence_self {
            lines.push(format!(
                "Avg sequence self: {}",
                format_duration(avg_sequence_self)
            ));
        }
        if self.metrics.total_tile_raster_overhead_ns > 0 {
            lines.push(String::from("Tile overhead breakdown:"));
            lines.push(format_bucket_line(
                "external input resolve",
                self.metrics.total_tile_external_input_resolve_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "internal input resolve",
                self.metrics.total_tile_internal_input_resolve_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "output store",
                self.metrics.total_tile_output_store_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "trace serialization",
                self.metrics.total_tile_trace_serialize_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "draft capture",
                self.metrics.total_tile_draft_capture_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "scope enter",
                self.metrics.total_tile_scope_enter_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "output record build",
                self.metrics.total_tile_output_record_build_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "trace event publish",
                self.metrics.total_tile_trace_event_publish_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "output coordinate publish",
                self.metrics.total_tile_output_coordinate_publish_ns,
                self.metrics.total_tile_invocations,
            ));
            lines.push(format_bucket_line(
                "other wrapper",
                self.metrics.total_tile_other_wrapper_ns,
                self.metrics.total_tile_invocations,
            ));
        }
        if self.metrics.total_sequence_self_duration_ns > 0 {
            lines.push(String::from("Sequence self breakdown:"));
            lines.push(format_bucket_line(
                "body self",
                self.metrics.total_sequence_body_self_ns,
                self.metrics.total_sequence_invocations,
            ));
            lines.push(format_bucket_line(
                "scope enter",
                self.metrics.total_sequence_scope_enter_ns,
                self.metrics.total_sequence_invocations,
            ));
            lines.push(format_bucket_line(
                "synthetic coordinate alloc",
                self.metrics.total_sequence_synthetic_coordinate_alloc_ns,
                self.metrics.total_sequence_invocations,
            ));
            lines.push(format_bucket_line(
                "input trace",
                self.metrics.total_sequence_input_trace_ns,
                self.metrics.total_sequence_invocations,
            ));
            lines.push(format_bucket_line(
                "sequence start publish",
                self.metrics.total_sequence_start_event_publish_ns,
                self.metrics.total_sequence_invocations,
            ));
            lines.push(format_bucket_line(
                "output trace",
                self.metrics.total_sequence_output_trace_ns,
                self.metrics.total_sequence_invocations,
            ));
            lines.push(format_bucket_line(
                "sequence end publish",
                self.metrics.total_sequence_end_event_publish_ns,
                self.metrics.total_sequence_invocations,
            ));
            lines.push(format_bucket_line(
                "other wrapper",
                self.metrics.total_sequence_other_wrapper_ns,
                self.metrics.total_sequence_invocations,
            ));
        }

        if let Some(latest_tile) = &self.metrics.latest_tile_stats {
            lines.push(String::new());
            lines.push(String::from("Latest Tile:"));
            lines.push(format!(
                "  {} @ {:?}: total {}, user {}, overhead {}, invocation {}",
                latest_tile.tile_id,
                latest_tile.coordinates,
                format_duration(latest_tile.total_duration_ns),
                format_duration(latest_tile.user_duration_ns),
                format_duration(latest_tile.raster_overhead_ns),
                latest_tile.invocation_index
            ));
            lines.push(format!(
                "  overhead parts: ext {}, int {}, store {}, trace {}, draft {}, scope {}, record {}, publish {}, coords {}, other {}",
                format_duration(latest_tile.external_input_resolve_ns),
                format_duration(latest_tile.internal_input_resolve_ns),
                format_duration(latest_tile.output_store_ns),
                format_duration(latest_tile.trace_serialize_ns),
                format_duration(latest_tile.draft_capture_ns),
                format_duration(latest_tile.scope_enter_ns),
                format_duration(latest_tile.output_record_build_ns),
                format_duration(latest_tile.trace_event_publish_ns),
                format_duration(latest_tile.output_coordinate_publish_ns),
                format_duration(latest_tile.other_wrapper_ns),
            ));
        }

        if !self.metrics.tile_metrics.is_empty() {
            lines.push(String::new());
            lines.push(String::from("Tiles:"));
            for (tile_id, metrics) in sorted_tiles(&self.metrics.tile_metrics) {
                lines.push(format!(
                    "  {}: total {}, user {}, overhead {}, calls {}",
                    tile_id.0,
                    format_duration(metrics.total_duration_ns),
                    format_duration(metrics.total_user_duration_ns),
                    format_duration(metrics.total_raster_overhead_ns),
                    metrics.invocations
                ));
            }
        }

        if !self.metrics.sequence_metrics.is_empty() {
            lines.push(String::new());
            lines.push(String::from("Sequences:"));
            for (sequence_id, metrics) in sorted_sequences(&self.metrics.sequence_metrics) {
                lines.push(format!(
                    "  {}: total {}, self {}, child {}, calls {}",
                    sequence_id,
                    format_duration(metrics.total_duration_ns),
                    format_duration(metrics.total_self_duration_ns),
                    format_duration(metrics.total_child_duration_ns),
                    metrics.invocations
                ));
                lines.push(format!(
                    "  self parts: body {}, scope {}, coords {}, in-trace {}, start {}, out-trace {}, end {}, other {}",
                    format_duration(metrics.total_body_self_ns),
                    format_duration(metrics.total_scope_enter_ns),
                    format_duration(metrics.total_synthetic_coordinate_alloc_ns),
                    format_duration(metrics.total_input_trace_ns),
                    format_duration(metrics.total_start_event_publish_ns),
                    format_duration(metrics.total_output_trace_ns),
                    format_duration(metrics.total_end_event_publish_ns),
                    format_duration(metrics.total_other_wrapper_ns),
                ));
            }
        }

        lines.join("\n")
    }

    /// Generate a JSON report.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.metrics)?)
    }
}

fn sorted_tiles(
    tile_metrics: &std::collections::HashMap<raster_core::tile::TileId, TileMetrics>,
) -> Vec<(&raster_core::tile::TileId, &TileMetrics)> {
    let mut entries: Vec<_> = tile_metrics.iter().collect();
    entries.sort_by(|left, right| right.1.total_duration_ns.cmp(&left.1.total_duration_ns));
    entries
}

fn sorted_sequences(
    sequence_metrics: &std::collections::HashMap<String, SequenceMetrics>,
) -> Vec<(&String, &SequenceMetrics)> {
    let mut entries: Vec<_> = sequence_metrics.iter().collect();
    entries.sort_by(|left, right| right.1.total_duration_ns.cmp(&left.1.total_duration_ns));
    entries
}

fn format_duration(duration_ns: u64) -> String {
    if duration_ns >= 1_000_000_000 {
        format!("{:.3} s", duration_ns as f64 / 1_000_000_000.0)
    } else if duration_ns >= 1_000_000 {
        format!("{:.3} ms", duration_ns as f64 / 1_000_000.0)
    } else if duration_ns >= 1_000 {
        format!("{:.3} us", duration_ns as f64 / 1_000.0)
    } else {
        format!("{} ns", duration_ns)
    }
}

fn average_duration(total_ns: u64, invocations: u64) -> Option<u64> {
    (invocations > 0).then(|| total_ns / invocations)
}

fn format_bucket_line(label: &str, total_ns: u64, invocations: u64) -> String {
    match average_duration(total_ns, invocations) {
        Some(avg_ns) => format!(
            "  {}: {} avg {}",
            label,
            format_duration(total_ns),
            format_duration(avg_ns)
        ),
        None => format!("  {}: {}", label, format_duration(total_ns)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{Metrics, SequenceMetrics};

    #[test]
    fn text_report_includes_sequence_self_breakdown() {
        let mut metrics = Metrics {
            total_duration_ns: 100,
            program_total_known: true,
            total_sequence_invocations: 2,
            total_sequence_self_duration_ns: 20,
            total_sequence_body_self_ns: 8,
            total_sequence_scope_enter_ns: 2,
            total_sequence_synthetic_coordinate_alloc_ns: 3,
            total_sequence_input_trace_ns: 2,
            total_sequence_start_event_publish_ns: 1,
            total_sequence_output_trace_ns: 2,
            total_sequence_end_event_publish_ns: 1,
            total_sequence_other_wrapper_ns: 1,
            ..Metrics::default()
        };
        metrics.sequence_metrics.insert(
            "main".to_string(),
            SequenceMetrics {
                invocations: 2,
                total_duration_ns: 100,
                avg_duration_ns: 50,
                total_self_duration_ns: 20,
                avg_self_duration_ns: 10,
                total_child_duration_ns: 80,
                total_body_self_ns: 8,
                avg_body_self_ns: 4,
                total_scope_enter_ns: 2,
                avg_scope_enter_ns: 1,
                total_synthetic_coordinate_alloc_ns: 3,
                avg_synthetic_coordinate_alloc_ns: 1,
                total_input_trace_ns: 2,
                avg_input_trace_ns: 1,
                total_start_event_publish_ns: 1,
                avg_start_event_publish_ns: 0,
                total_output_trace_ns: 2,
                avg_output_trace_ns: 1,
                total_end_event_publish_ns: 1,
                avg_end_event_publish_ns: 0,
                total_other_wrapper_ns: 1,
                avg_other_wrapper_ns: 0,
            },
        );

        let text = Report::new(metrics).to_text();
        assert!(text.contains("Sequence self breakdown:"));
        assert!(text.contains("synthetic coordinate alloc"));
        assert!(text.contains("self parts: body"));
    }
}
