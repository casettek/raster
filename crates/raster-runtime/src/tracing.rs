pub mod subscriber;

use std::collections::VecDeque;

use raster_core::trace::{FnCallRecord, FnInputParam, TraceEvent};

use crate::tracing::subscriber::json::JsonSubscriber;
use crate::tracing::subscriber::{Subscriber, GLOBAL_SUBSCRIBER};

/// Initializes the global subscriber with a JSON subscriber that writes to stdout.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init() {
    init_with(JsonSubscriber::new(std::io::stdout()));
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

#[doc(hidden)]
pub fn emit_trace(
    function_name: &str,
    desc: Option<&str>,
    input_params: &[(&str, &str)],
    output_type: Option<&str>,
    input: &[u8],
    output: &[u8],
) {
    let inputs: Vec<FnInputParam> = input_params
        .iter()
        .map(|(name, ty)| FnInputParam {
            name: (*name).to_string(),
            ty: (*ty).to_string(),
        })
        .collect();
    let record = FnCallRecord {
        fn_name: function_name.to_string(),
        desc: desc.map(|s| s.to_string()),
        inputs,
        input_data: input.to_vec(),
        output_type: output_type.map(|s| s.to_string()),
        output_data: output.to_vec(),
    };
    emit_trace_event(TraceEvent::Tile(record));
}

pub struct SequenceId(String);
pub struct SequenceStack(VecDeque<SequenceId>);
