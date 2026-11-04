pub mod audit;
pub mod binary;
pub mod commitment;
pub mod json;

use raster_core::trace::TraceEvent;
use std::sync::OnceLock;

pub use binary::BinaryTraceEventPublisher;
pub use json::JsonTraceEventPublisher;

/// The global publisher instance.
pub(crate) static GLOBAL_PUBLISHER: OnceLock<Box<dyn Publisher>> = OnceLock::new();

/// A trait for receiving trace events.
pub trait Publisher: Send + Sync {
    fn publish(&self, event: TraceEvent);

    fn finish(&self);
}
