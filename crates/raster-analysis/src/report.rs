use crate::metrics::Metrics;
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
        // TODO: Implement text report generation
        format!("Total duration: {} ns", self.metrics.total_duration_ns)
    }

    /// Generate a JSON report.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.metrics)?)
    }
}
