pub mod commitment;
pub mod publishers;
pub mod recorder;

use raster_core::trace::TraceEvent;

use crate::auth::AuthMode;
use crate::tracing::publishers::{
    BinaryTraceEventPublisher, JsonTraceEventPublisher, Publisher, GLOBAL_PUBLISHER,
};
use std::cell::Cell;
use std::str::FromStr;
use std::sync::Once;

pub const TRACE_FORMAT_ENV: &str = "RASTER_TRACE_FORMAT";
pub const TRACE_PATH_ENV: &str = "RASTER_TRACE_PATH";
/// Directory the program's output artifact (`output.bin` / `output.rindex` /
/// `output_manifest.json`) is written to on a successful, output-producing
/// run. Set by `cargo raster run`; unset for a plain `cargo run`, which then
/// produces no artifact files.
pub const OUTPUT_DIR_ENV: &str = "RASTER_OUTPUT_DIR";

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
/// `binary`. Plain Rust runs stay quiet by default.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
///
/// **This is also what marks the process as a Raster program run**, which is
/// what lowers the default to [`AuthMode::Unauthenticated`]. It is called from
/// exactly one place — the `fn main` the `#[sequence]` macro generates for a
/// program entry point. A test binary or a library embedder reaches the runtime
/// through [`init_with`] or not at all, and so stays authenticated. See
/// `docs/proposals/unauthenticated-execution.md` §1.
///
/// Calling this directly, outside a `#[sequence] main`, therefore opts the
/// binary into unauthenticated execution — no storage between tiles, no trace,
/// and no way to commit. Set `RASTER_AUTH=1` to override, or use
/// [`crate::auth::force_auth_mode`].
pub fn init() {
    // Must precede every `auth_mode()` below, or the mode caches as
    // `Authenticated` and the program silently runs the expensive way.
    crate::auth::note_program_entry();

    if crate::auth::auth_mode() == AuthMode::Unauthenticated {
        reject_profiling_without_authentication();
        // The external-input resolver is still needed — the program reads the
        // same `--input`/`--input-manifest` either way. Only the trace
        // publisher is skipped, and skipping it is the whole interlock: with no
        // trace there is no artifact for `--commit` to operate on, so an
        // unauthenticated run cannot produce a trace commitment.
        init_runtime_state();
        return;
    }

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
    }
}

/// Initializes the global subscriber with a custom subscriber.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
///
/// Deliberately does **not** mark a program entry, so a caller reaching for
/// this stays on the [`AuthMode::Authenticated`] default. That is forced rather
/// than convenient: this function exists to install a trace publisher, an
/// unauthenticated run emits no trace, so wanting one implies wanting
/// authenticated bindings to put in it.
pub fn init_with<P: Publisher + 'static>(publisher: P) {
    init_runtime_state();
    if GLOBAL_PUBLISHER.get().is_none() {
        install_publisher(publisher);
    }
}

/// A profile of an unauthenticated run measures a different program, not a
/// cheaper one: `record_tile_output_store_profile` and the storage-input
/// resolve timings account for work this mode deletes outright. Its numbers
/// cannot be compared against — or acted on for — a run that will be committed,
/// and emitting it behind a warning invites exactly that comparison. The env is
/// read directly rather than through `profiling::init_from_env` so the refusal
/// holds whether or not the `profiling` feature is on.
fn reject_profiling_without_authentication() {
    let requested = [
        crate::profiling::PROFILE_PATH_ENV,
        crate::profiling::PROFILE_STREAM_PATH_ENV,
        crate::profiling::PROFILE_RUN_ID_ENV,
    ]
    .into_iter()
    .find(|name| std::env::var_os(name).is_some());

    if let Some(name) = requested {
        panic!(
            "{name} is set, but this run is unauthenticated, and profiling an \
             unauthenticated run measures a different program: storage encode, \
             hash and resolve costs are absent by construction. Re-run with \
             authentication (drop `--no-auth`, or set {}=1) to profile.",
            crate::auth::AUTH_ENV
        );
    }
}

fn init_runtime_state() {
    RUNTIME_INIT.call_once(|| {
        crate::profiling::init_from_env();
        crate::entry_arguments::install_default_source_resolver()
            .unwrap_or_else(|error| panic!("Failed to read --input/--input-manifest: {}", error));
    });
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
                    TraceEvent::TileExec(record) => TraceEvent::RecurTileIterationExec(record),
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
