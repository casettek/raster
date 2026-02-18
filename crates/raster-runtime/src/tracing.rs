pub mod subscriber;

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

// Internal function used by the generated code from the #[tile] macro.
// This is not part of the public API.

#[doc(hidden)]
pub fn emit_trace(
    function_name: &str,
    desc: Option<&str>,
    input_params: &[(&str, &str)],
    output_type: Option<&str>,
    input: &[u8],
    output: &[u8],
) {
    if let Some(subscriber) = GLOBAL_SUBSCRIBER.get() {
        subscriber.on_trace(
            function_name,
            desc,
            input_params,
            output_type,
            input,
            output,
        );
    }
}
