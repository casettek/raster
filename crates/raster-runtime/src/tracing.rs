pub mod commitment;
pub mod publishers;
pub mod recorder;

use raster_core::trace::TraceEvent;

use crate::tracing::publishers::{
    BinaryTraceEventPublisher, JsonTraceEventPublisher, Publisher, GLOBAL_PUBLISHER,
};
use std::cell::Cell;
use std::ffi::OsStr;
use std::str::FromStr;
use std::sync::Once;

pub const TRACE_EVENT_PREFIX: &str = "[trace-event]";
pub const TRACE_FORMAT_ENV: &str = "RASTER_TRACE_FORMAT";
pub const TRACE_PATH_ENV: &str = "RASTER_TRACE_PATH";
pub const TRACE_STDOUT_ENV: &str = "RASTER_TRACE_STDOUT";

static RUNTIME_INIT: Once = Once::new();

std::thread_local! {
    static RECUR_TRACE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceFormat {
    Binary,
    Json,
}

impl TraceFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Json => "json",
        }
    }
}

impl Default for TraceFormat {
    fn default() -> Self {
        Self::Binary
    }
}

impl FromStr for TraceFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "binary" => Ok(Self::Binary),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "Unsupported trace format '{other}'. Expected 'binary' or 'json'."
            )),
        }
    }
}

/// Initializes tracing for a program entry point.
///
/// `cargo raster run` sets `RASTER_TRACE_PATH`, which enables trace capture
/// for the CLI. `RASTER_TRACE_FORMAT` selects the file format and defaults to
/// `binary`. Plain Rust runs stay quiet by default. Set `RASTER_TRACE_STDOUT=1`
/// to opt into stdout JSON trace emission.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init() {
    init_runtime_state();
    if GLOBAL_PUBLISHER.get().is_some() {
        return;
    }

    if let Some(trace_path) = std::env::var_os(TRACE_PATH_ENV) {
        match trace_format_from_env() {
            TraceFormat::Binary => {
                let publisher = BinaryTraceEventPublisher::from_path(trace_path.into())
                    .unwrap_or_else(|error| {
                        panic!("Failed to initialize binary trace publisher: {}", error)
                    });
                install_publisher(publisher);
            }
            TraceFormat::Json => {
                let publisher = JsonTraceEventPublisher::from_path(trace_path.into())
                    .unwrap_or_else(|error| {
                        panic!("Failed to initialize JSON trace publisher: {}", error)
                    });
                install_publisher(publisher);
            }
        }
    } else if stdout_trace_enabled() {
        install_publisher(JsonTraceEventPublisher::with_prefix(
            std::io::stdout(),
            TRACE_EVENT_PREFIX,
        ));
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

fn trace_format_from_env() -> TraceFormat {
    let Some(value) = std::env::var_os(TRACE_FORMAT_ENV) else {
        return TraceFormat::default();
    };
    let value = value
        .to_str()
        .unwrap_or_else(|| panic!("{TRACE_FORMAT_ENV} must be valid UTF-8"));
    TraceFormat::from_str(value).unwrap_or_else(|error| panic!("{error}"))
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

    #[test]
    fn trace_format_parses_supported_values() {
        assert_eq!(
            "binary".parse::<TraceFormat>().unwrap(),
            TraceFormat::Binary
        );
        assert_eq!("json".parse::<TraceFormat>().unwrap(), TraceFormat::Json);
    }

    #[test]
    fn trace_format_rejects_unknown_values() {
        assert!("pretty".parse::<TraceFormat>().is_err());
    }
}
