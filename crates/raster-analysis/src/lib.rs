//! Analysis and profiling tools for Raster.
//!
//! This crate provides:
//! - Trace analysis and metrics extraction
//! - Performance profiling
//! - Cost estimation and tuning suggestions

pub mod analyzer;
pub mod metrics;
pub mod report;

pub use analyzer::Analyzer;
pub use metrics::Metrics;
pub use report::Report;
