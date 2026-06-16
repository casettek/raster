pub mod commitment;
pub mod publisher;
pub mod recorder;

use raster_core::trace::TraceEvent;

use crate::tracing::publisher::TraceEventPublisher;
use crate::tracing::publisher::{Publisher, GLOBAL_PUBLISHER};
use std::cell::Cell;

pub const TRACE_EVENT_PREFIX: &str = "[trace-event]";

std::thread_local! {
    static RECUR_TRACE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

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
        let event = RECUR_TRACE_DEPTH.with(|depth| {
            if depth.get() > 0 {
                match event {
                    TraceEvent::TileExec(record) => TraceEvent::RecurTileExec(record),
                    other => other,
                }
            } else {
                event
            }
        });
        publisher.publish(event);
    }
}

#[doc(hidden)]
pub struct RecurTraceScopeGuard;

impl RecurTraceScopeGuard {
    pub fn enter() -> Self {
        RECUR_TRACE_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}

impl Drop for RecurTraceScopeGuard {
    fn drop(&mut self) {
        RECUR_TRACE_DEPTH.with(|depth| {
            let current = depth.get();
            if current > 0 {
                depth.set(current - 1);
            }
        });
    }
}
