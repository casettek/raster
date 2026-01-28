//! A minimal tracing library with JSON output.
//!
//! This crate provides two ways to instrument synchronous functions:
//!
//! - `#[trace]` attribute macro: Automatically trace all calls to a function
//! - `trace_call!()` function-like macro: Trace individual function calls at the call site
//!
//! All trace events are output as JSON with function arguments and return values.
//!
//! # Example: Using `#[trace]` attribute
//!
//! ```rust,no_run
//! use tracing_lite::trace;
//!
//! tracing_lite::init();
//!
//! #[trace]
//! fn add(a: i32, b: i32) -> i32 {
//!     a + b
//! }
//!
//! fn main() {
//!     let result = add(1, 2);
//!     println!("Result: {}", result);
//! }
//! ```
//!
//! # Example: Using `trace_call!()` macro
//!
//! ```rust,no_run
//! use tracing_lite::trace_call;
//!
//! tracing_lite::init();
//!
//! fn multiply(a: i32, b: i32) -> i32 {
//!     a * b
//! }
//!
//! fn main() {
//!     // Trace this specific call without modifying the function
//!     let result = trace_call!(multiply(3, 4));
//!     println!("Result: {}", result);
//! }
//! ```

use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};


/// A trait for receiving trace events.
pub trait Subscriber: Send + Sync {
    /// Called when a function completes, with full metadata and runtime values.
    ///
    /// # Arguments
    /// - `function_name` - Name of the function being traced
    /// - `param_names` - Names of the function parameters
    /// - `param_types` - Types of the function parameters
    /// - `return_type` - Return type of the function (None for unit)
    /// - `input_values` - Debug-formatted input values at runtime
    /// - `output_value` - Debug-formatted return value at runtime
    fn on_trace(
        &self,
        function_name: &str,
        param_names: &[&str],
        param_types: &[&str],
        return_type: Option<&str>,
        input_values: &[&str],
        output_value: &str,
    );
}

/// The global subscriber instance.
static GLOBAL_SUBSCRIBER: OnceLock<Box<dyn Subscriber>> = OnceLock::new();

/// A JSON-formatting subscriber that writes to a writer.
pub struct JsonSubscriber<W: Write + Send> {
    writer: Mutex<W>,
}

impl<W: Write + Send> JsonSubscriber<W> {
    /// Creates a new JSON subscriber that writes to the given writer.
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }
}

impl<W: Write + Send + Sync> Subscriber for JsonSubscriber<W> {
    fn on_trace(
        &self,
        function_name: &str,
        param_names: &[&str],
        param_types: &[&str],
        return_type: Option<&str>,
        input_values: &[&str],
        output_value: &str,
    ) {
        // Build params array with name, type, and value for each parameter
        let params: Vec<serde_json::Value> = param_names
            .iter()
            .zip(param_types.iter())
            .zip(input_values.iter())
            .map(|((name, ty), value)| {
                serde_json::json!({
                    "name": *name,
                    "type": *ty,
                    "value": *value
                })
            })
            .collect();

        // Build return object with type and value
        let return_obj = serde_json::json!({
            "type": return_type.unwrap_or("()"),
            "value": output_value
        });

        let event = serde_json::json!({
            "function": function_name,
            "params": params,
            "return": return_obj,
        });

        if let Ok(mut writer) = self.writer.lock() {
            let _ = writeln!(writer, "RASTER_TRACE:{}", event);
            let _ = writer.flush();
        }
    }
}

/// Initializes the global subscriber with a JSON subscriber that writes to stdout.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init() {
    init_with(JsonSubscriber::new(io::stdout()));
}

/// Initializes the global subscriber with a custom subscriber.
///
/// This function should be called once at the start of your program.
/// Subsequent calls will have no effect.
pub fn init_with<S: Subscriber + 'static>(subscriber: S) {
    let _ = GLOBAL_SUBSCRIBER.set(Box::new(subscriber));
}

// Internal function used by the generated code from the #[trace] macro.
// This is not part of the public API.

#[doc(hidden)]
pub fn __emit_trace(
    function_name: &str,
    param_names: &[&str],
    param_types: &[&str],
    return_type: Option<&str>,
    input_values: &[&str],
    output_value: &str,
) {
    if let Some(subscriber) = GLOBAL_SUBSCRIBER.get() {
        subscriber.on_trace(
            function_name,
            param_names,
            param_types,
            return_type,
            input_values,
            output_value,
        );
    }
}