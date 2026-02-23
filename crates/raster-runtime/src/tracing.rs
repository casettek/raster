pub mod subscriber;


use raster_core::trace::TraceEvent;

use crate::tracing::subscriber::ExecutionSubscriber;
use crate::tracing::subscriber::{Subscriber, GLOBAL_SUBSCRIBER};

/// Initializes the global subscriber with a JSON subscriber that writes to stdout.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init() {
    init_with(ExecutionSubscriber::new(std::io::stdout()));
}

/// Initializes the global sub:scriber with a custom subscriber.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init_with<S: Subscriber + 'static>(subscriber: S) {
    let _ = GLOBAL_SUBSCRIBER.set(Box::new(subscriber));
}

pub fn finish() {
    if let Some(subscriber) = GLOBAL_SUBSCRIBER.get() {
        subscriber.on_complete();
    }
}

// Internal function used by the generated code from the #[tile] and #[sequence] macros.
// This is not part of the public API.

#[doc(hidden)]
pub fn emit_trace_event(event: TraceEvent) {
    if let Some(subscriber) = GLOBAL_SUBSCRIBER.get() {
        subscriber.on_trace(event);
    }
}
