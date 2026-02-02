use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use raster_core::trace::{TraceItem, TraceInputParam};
use raster_prover::bit_packer::BitPacker;
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::TraceCommitmentProducer;

use crate::tracing::subscriber::Subscriber;
/// A subscriber that computes trace commitments and verifies them against an expected file.
///
/// On `on_complete()`, reads the expected packed u64s from the file and compares
/// with the computed commitments, panicking on mismatch.
pub struct VerifySubscriber {
    expected_path: PathBuf,
    producer: Mutex<Option<TraceCommitmentProducer>>,
    bit_packer: Mutex<BitPacker>,
}

impl VerifySubscriber {
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

impl Subscriber for VerifySubscriber {
    fn on_trace(
        &self,
        function_name: &str,
        desc: Option<&str>,
        input_params: &[(&str, &str)],
        output_type: Option<&str>,
        input: &[u8],
        output: &[u8],
    ) {
        let item = TraceItem {
            fn_name: function_name.to_string(),
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

                for (i, (computed, expected)) in computed_packed
                    .iter()
                    .zip(expected_packed.iter())
                    .enumerate()
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