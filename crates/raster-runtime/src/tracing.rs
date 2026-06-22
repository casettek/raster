pub mod commitment;
pub mod publisher;
pub mod recorder;

use raster_core::trace::TraceEvent;

use crate::tracing::publisher::{BinaryTraceEventPublisher, TraceEventPublisher};
use crate::tracing::publisher::{Publisher, GLOBAL_PUBLISHER};
use std::cell::Cell;
use std::ffi::OsStr;
use std::sync::Once;

pub const TRACE_EVENT_PREFIX: &str = "[trace-event]";
pub const TRACE_PATH_ENV: &str = "RASTER_TRACE_PATH";
pub const TRACE_STDOUT_ENV: &str = "RASTER_TRACE_STDOUT";

static RUNTIME_INIT: Once = Once::new();

std::thread_local! {
    static RECUR_TRACE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Initializes tracing for a program entry point.
///
/// `cargo raster run` sets `RASTER_TRACE_PATH`, which enables binary trace
/// capture for the CLI. Plain Rust runs stay quiet by default. Set
/// `RASTER_TRACE_STDOUT=1` to opt into legacy stdout JSON trace emission.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init() {
    init_runtime_state();
    if GLOBAL_PUBLISHER.get().is_some() {
        return;
    }

    if let Some(trace_path) = std::env::var_os(TRACE_PATH_ENV) {
        let publisher =
            BinaryTraceEventPublisher::from_path(trace_path.into()).unwrap_or_else(|error| {
                panic!("Failed to initialize binary trace publisher: {}", error)
            });
        install_publisher(publisher);
    } else if stdout_trace_enabled() {
        install_publisher(TraceEventPublisher::new(std::io::stdout()));
    }
}

/// Initializes the global subscriber with a custom subscriber.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init_with<P: Publisher + 'static>(publisher: P) {
    init_runtime_state();
    if GLOBAL_PUBLISHER.get().is_none() {
        install_publisher(publisher);
    }
}

fn init_runtime_state() {
    RUNTIME_INIT.call_once(crate::profiling::init_from_env);
}

fn install_publisher<P: Publisher + 'static>(publisher: P) {
    let _ = GLOBAL_PUBLISHER.set(Box::new(publisher));
}

fn stdout_trace_enabled() -> bool {
    std::env::var_os(TRACE_STDOUT_ENV)
        .as_deref()
        .is_some_and(stdout_trace_value_enabled)
}

fn stdout_trace_value_enabled(value: &OsStr) -> bool {
    value
        .to_str()
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub fn finish() {
    if let Some(publisher) = GLOBAL_PUBLISHER.get() {
        publisher.finish();
    }
    if let Err(error) = crate::profiling::finish() {
        panic!("Failed to write Raster execution profile: {}", error);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn stdout_trace_opt_in_accepts_common_truthy_values() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            assert!(stdout_trace_value_enabled(OsStr::new(value)));
        }
    }

    #[test]
    fn stdout_trace_opt_in_rejects_missing_or_false_values() {
        for value in ["", "0", "false", "no", "off", "anything-else"] {
            assert!(!stdout_trace_value_enabled(OsStr::new(value)));
        }
    }
}
