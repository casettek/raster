pub mod assembler;
pub mod publisher;

use raster_core::trace::TraceEvent;

use crate::tracing::publisher::TraceEventPublisher;
use crate::tracing::publisher::{Publisher, GLOBAL_PUBLISHER};

pub const TRACE_EVENT_PREFIX: &str = "[trace-event]";

/// Initializes the global subscriber with a JSON subscriber that writes to stdout.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init() {
    init_with(TraceEventPublisher::new(std::io::stdout()));
}

/// Initializes the global sub:scriber with a custom subscriber.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init_with<P: Publisher + 'static>(publisher: P) {
    let _ = GLOBAL_PUBLISHER.set(Box::new(publisher));
}

pub fn finish() {
    if let Some(publisher) = GLOBAL_PUBLISHER.get() {
        publisher.finish();
    }
}

// Internal function used by the generated code from the #[tile] and #[sequence] macros.
// This is not part of the public API.

#[doc(hidden)]
pub fn publish_trace_event(event: TraceEvent) {
    if let Some(publisher) = GLOBAL_PUBLISHER.get() {
        publisher.publish(event);
    }
}
