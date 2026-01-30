use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use raster_core::trace::{TileTraceItem, TraceInputParam};
use raster_prover::bit_packer::BitPacker;
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::TraceCommitmentProducer;

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

    fn on_complete(&self);
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

    fn on_complete(&self) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.flush();
        }
    }
}

pub struct ExecCommitSubscriber<W>
where
    W: Write + Send,
{
    producer: Mutex<Option<TraceCommitmentProducer>>,
    writer: Mutex<W>,
    bit_packer: Mutex<BitPacker>,
}

impl<W: Write + Send> ExecCommitSubscriber<W> {
    /// Creates a new execution commitment subscriber.
    ///
    /// Uses the precomputed empty trie node as the seed for the Merkle tree.
    pub fn new(bits: usize, writer: W) -> Self {
        let writer = Mutex::new(writer);
        Self {
            // TODO: seed hardcoded for now
            producer: Mutex::new(Some(TraceCommitmentProducer::new(&EMPTY_TRIE_NODES[0]))),
            writer: writer,
            bit_packer: Mutex::new(BitPacker::new(bits)),
        }
    }
}

impl<W: Write + Send> Subscriber for ExecCommitSubscriber<W> {
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

        if let Ok(mut producer) = self.producer.lock() {
            if let Some(producer) = producer.as_mut() {
                producer.try_append(&item).unwrap();
            }
        }
    }

    fn on_complete(&self) {
        if let Ok(mut producer_guard) = self.producer.lock() {
            if let Some(producer) = producer_guard.take() {
                let trace_items_commitments = producer.finish();
                if let Ok(bit_packer) = self.bit_packer.lock() {
                    let packed = bit_packer.pack(&trace_items_commitments);

                    if let Ok(mut writer) = self.writer.lock() {
                        for block in packed {
                            writer.write_all(&block.to_le_bytes()).unwrap();
                        }
                    }
                }
            }
        }
    }
}

/// A subscriber that computes trace commitments and verifies them against an expected file.
///
/// On `on_complete()`, reads the expected packed u64s from the file and compares
/// with the computed commitments, panicking on mismatch.
pub struct ExecVerifySubscriber {
    expected_path: PathBuf,
    producer: Mutex<Option<TraceCommitmentProducer>>,
    bit_packer: Mutex<BitPacker>,
}

impl ExecVerifySubscriber {
    /// Creates a new trace verification subscriber.
    ///
    /// # Arguments
    /// - `bits` - Number of bits for the bit packer
    /// - `expected_path` - Path to the file containing expected packed commitments
    pub fn new(bits: usize, expected_path: PathBuf) -> Self {
        Self {
            expected_path,
            producer: Mutex::new(Some(TraceCommitmentProducer::new(&EMPTY_TRIE_NODES[0]))),
            bit_packer: Mutex::new(BitPacker::new(bits)),
        }
    }
}

impl Subscriber for ExecVerifySubscriber {
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

        if let Ok(mut producer) = self.producer.lock() {
            if let Some(producer) = producer.as_mut() {
                producer.try_append(&item).unwrap();
            }
        }
    }

    fn on_complete(&self) {
        if let Ok(mut producer_guard) = self.producer.lock() {
            if let Some(producer) = producer_guard.take() {
                let trace_items_commitments = producer.finish();

                let computed_packed = if let Ok(bit_packer) = self.bit_packer.lock() {
                    bit_packer.pack(&trace_items_commitments)
                } else {
                    panic!("Failed to lock bit_packer");
                };

                // Read expected packed u64s from file
                let mut file = std::fs::File::open(&self.expected_path).unwrap_or_else(|e| {
                    panic!(
                        "Failed to open expected commitment file '{}': {}",
                        self.expected_path.display(),
                        e
                    )
                });

                let mut expected_bytes = Vec::new();
                file.read_to_end(&mut expected_bytes).unwrap_or_else(|e| {
                    panic!(
                        "Failed to read expected commitment file '{}': {}",
                        self.expected_path.display(),
                        e
                    )
                });

                // Parse expected bytes as little-endian u64s
                let expected_packed: Vec<u64> = expected_bytes
                    .chunks_exact(8)
                    .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap()))
                    .collect();

                // Compare computed vs expected
                if computed_packed.len() != expected_packed.len() {
                    panic!(
                        "Trace commitment verification failed: length mismatch.\n\
                         Expected {} u64 values, got {}.",
                        expected_packed.len(),
                        computed_packed.len()
                    );
                }

                for (i, (computed, expected)) in
                    computed_packed.iter().zip(expected_packed.iter()).enumerate()
                {
                    if computed != expected {
                        panic!(
                            "Trace commitment verification failed at index {}.\n\
                             Expected: 0x{:016x}\n\
                             Computed: 0x{:016x}",
                            i, expected, computed
                        );
                    }
                }

                eprintln!(
                    "Trace commitment verification passed ({} values verified).",
                    computed_packed.len()
                );
            }
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

pub fn finish() {
    if let Some(subscriber) = GLOBAL_SUBSCRIBER.get() {
        subscriber.on_complete();
    }
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
