use crate::metrics::{Metrics, TileMetrics};
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
        let program_total = if self.metrics.program_total_known {
            format_duration(self.metrics.total_duration_ns)
        } else {
            String::from("pending")
        };

        lines.push(String::from("Profile Summary"));
        lines.push(format!(
            "  Run: {}",
            self.metrics.run_id.as_deref().unwrap_or("unknown")
        ));
        lines.push(format!("  Program total: {}", program_total));
        lines.push(format!(
            "  Tiles executed: {} calls",
            format_count(self.metrics.total_tile_invocations)
        ));
        lines.push(format!(
            "  Tile execution: {} user, {} raster",
            format_duration(self.metrics.total_tile_user_duration_ns),
            format_duration(self.metrics.total_tile_raster_overhead_ns)
        ));
        lines.push(format!(
            "  Sequences: {} calls, {}",
            format_count(self.metrics.total_sequence_invocations),
            format_duration(self.metrics.total_sequence_self_duration_ns)
        ));

        let hot_tiles = sorted_tiles(&self.metrics.tile_metrics);
        if !hot_tiles.is_empty() {
            lines.push(String::from("  Hot Tiles"));
            for (tile_id, metrics) in hot_tiles.into_iter().take(3) {
                lines.push(format!(
                    "    {}: total {}, avg {}, calls {}",
                    tile_id.0,
                    format_duration(metrics.total_duration_ns),
                    format_duration(metrics.avg_duration_ns),
                    format_count(metrics.invocations)
                ));
            }
        }

        lines.push(String::from("  Execution Shape"));
        lines.push(format!(
            "    Max nesting depth: {}",
            self.metrics.max_nesting_depth
        ));
        lines.push(format!(
            "    Profile records: {}",
            format_count(self.metrics.profile_record_count)
        ));

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
    entries.sort_by(|left, right| {
        right
            .1
            .total_duration_ns
            .cmp(&left.1.total_duration_ns)
            .then_with(|| left.0 .0.cmp(&right.0 .0))
    });
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

fn format_count(count: u64) -> String {
    let value = count.to_string();
    let mut formatted = String::with_capacity(value.len() + value.len() / 3);
    for (index, ch) in value.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(ch);
    }
    formatted.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::tile::TileId;

    #[test]
    fn text_report_uses_compact_profile_summary() {
        let mut metrics = Metrics {
            run_id: Some("run-1".to_string()),
            total_duration_ns: 183_420_000,
            program_total_known: true,
            profile_record_count: 1_251,
            max_nesting_depth: 2,
            total_tile_invocations: 1_248,
            total_tile_user_duration_ns: 151_300_000,
            total_tile_raster_overhead_ns: 18_240_000,
            total_sequence_invocations: 3,
            total_sequence_self_duration_ns: 13_880_000,
            ..Metrics::default()
        };

        metrics.tile_metrics.insert(
            TileId::from("merge_tokens"),
            TileMetrics {
                invocations: 15,
                total_duration_ns: 21_740_000,
                avg_duration_ns: 1_449_333,
                ..TileMetrics::default()
            },
        );
        metrics.tile_metrics.insert(
            TileId::from("tokenize_chunk"),
            TileMetrics {
                invocations: 1_090,
                total_duration_ns: 122_410_000,
                avg_duration_ns: 112_302,
                ..TileMetrics::default()
            },
        );
        metrics.tile_metrics.insert(
            TileId::from("normalize"),
            TileMetrics {
                invocations: 141,
                total_duration_ns: 7_150_000,
                avg_duration_ns: 50_709,
                ..TileMetrics::default()
            },
        );
        metrics.tile_metrics.insert(
            TileId::from("cold_tile"),
            TileMetrics {
                invocations: 2,
                total_duration_ns: 1_000,
                avg_duration_ns: 500,
                ..TileMetrics::default()
            },
        );

        let text = Report::new(metrics).to_text();

        assert!(text.contains("Profile Summary"));
        assert!(text.contains("  Run: run-1"));
        assert!(text.contains("  Program total: 183.420 ms"));
        assert!(text.contains("  Tiles executed: 1,248 calls"));
        assert!(text.contains("  Tile execution: 151.300 ms user, 18.240 ms raster"));
        assert!(text.contains("  Sequences: 3 calls, 13.880 ms"));
        assert!(text.contains("  Hot Tiles"));
        assert!(text.contains("    tokenize_chunk: total 122.410 ms, avg 112.302 us, calls 1,090"));
        assert!(text.contains("    merge_tokens: total 21.740 ms, avg 1.449 ms, calls 15"));
        assert!(text.contains("    normalize: total 7.150 ms, avg 50.709 us, calls 141"));
        assert!(!text.contains("cold_tile"));
        assert!(text.contains("  Execution Shape"));
        assert!(text.contains("    Max nesting depth: 2"));
        assert!(text.contains("    Profile records: 1,251"));
    }
}
