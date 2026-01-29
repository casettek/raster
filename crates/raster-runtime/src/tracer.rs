use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use raster_core::trace::{TileTraceItem, TraceInputParam};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{ExecutionCommitment, ExecutionCommitmentBuilder, ExecutionCommitmentBuilderWithWriter};

/// A trait for receiving trace events.
pub trait Subscriber: Send + Sync {
    /// Called when a function completes, with serialized input/output bytes and metadata.
    ///
    /// # Arguments
    /// - `function_name` - Name of the function being traced
    /// - `desc` - Optional human-readable description of the tile
    /// - `input_params` - Slice of (name, type) tuples for each input parameter
    /// - `output_type` - Optional return type as a string
    /// - `input` - Serialized input bytes (postcard-encoded)
    /// - `output` - Serialized output bytes (postcard-encoded)
    fn on_trace(
        &self,
        function_name: &str,
        desc: Option<&str>,
        input_params: &[(&str, &str)],
        output_type: Option<&str>,
        input: &[u8],
        output: &[u8],
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
        desc: Option<&str>,
        input_params: &[(&str, &str)],
        output_type: Option<&str>,
        input: &[u8],
        output: &[u8],
    ) {
        let item = TileTraceItem {
            tile: function_name.to_string(),
            desc: desc.map(|s| s.to_string()),
            inputs: input_params
                .iter()
                .map(|(name, ty)| TraceInputParam {
                    name: name.to_string(),
                    ty: ty.to_string(),
                })
                .collect(),
            input_data: BASE64_STANDARD.encode(input),
            output_type: output_type.map(|s| s.to_string()),
            output_data: BASE64_STANDARD.encode(output),
        };

        if let Ok(mut writer) = self.writer.lock() {
            if let Ok(json) = serde_json::to_string(&item) {
                let _ = writeln!(writer, "RASTER_TRACE:{}", json);
                let _ = writer.flush();
            }
        }
    }
}

/// A subscriber that builds a cryptographic commitment to the execution trace.
///
/// This subscriber incrementally builds an `ExecutionCommitment` by appending
/// trace items to an internal `ExecutionCommitmentBuilder`. After execution
/// completes, call `build()` to retrieve the final commitment.
pub struct ExecutionCommitmentSubscriber<W: Write + Send> {
    builder: Mutex<ExecutionCommitmentBuilderWithWriter<W>>,
}

impl<W: Write + Send> ExecutionCommitmentSubscriber<W> {
    /// Creates a new execution commitment subscriber.
    ///
    /// Uses the precomputed empty trie node as the seed for the Merkle tree.
    pub fn new(writer: W) -> Self {
        Self {
            builder: Mutex::new(ExecutionCommitmentBuilderWithWriter::new(&EMPTY_TRIE_NODES[0], writer)),
        }
    }

    /// Consume the subscriber and return the built commitment.
    pub fn build(self) -> ExecutionCommitment {
        self.builder.into_inner().unwrap().build()
    }
}

impl<W: Write + Send> Subscriber for ExecutionCommitmentSubscriber<W> {
    fn on_trace(
        &self,
        function_name: &str,
        desc: Option<&str>,
        input_params: &[(&str, &str)],
        output_type: Option<&str>,
        input: &[u8],
        output: &[u8],
    ) {
        let item = TileTraceItem {
            tile: function_name.to_string(),
            desc: desc.map(|s| s.to_string()),
            inputs: input_params
                .iter()
                .map(|(name, ty)| TraceInputParam {
                    name: name.to_string(),
                    ty: ty.to_string(),
                })
                .collect(),
            input_data: BASE64_STANDARD.encode(input),
            output_type: output_type.map(|s| s.to_string()),
            output_data: BASE64_STANDARD.encode(output),
        };

        if let Ok(mut builder) = self.builder.lock() {
            builder.try_append(&item).unwrap();
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

// Internal function used by the generated code from the #[tile] macro.
// This is not part of the public API.

#[doc(hidden)]
pub fn __emit_trace(
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