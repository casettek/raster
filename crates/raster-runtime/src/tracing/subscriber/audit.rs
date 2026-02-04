use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use raster_core::ipc;
use raster_core::trace::{AuditDiff, AuditResult, TraceInputParam, TraceItem};
use raster_prover::bit_packer::BitPacker;
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{SerializableFrontier, TraceCommitmentProducer};

use crate::tracing::subscriber::Subscriber;

/// Number of trace items to include in the trace window when a diff is detected.
/// This provides context around where execution diverged.
const AUDIT_WINDOW_SIZE: usize = 10;

/// A subscriber that computes trace commitments and verifies them against an expected file.
///
/// On `on_complete()`, reads the expected packed u64s from the file and compares
/// with the computed commitments, panicking on mismatch.
pub struct AuditSubscriber {
    expected_path: PathBuf,
    producer: Mutex<Option<TraceCommitmentProducer>>,
    trace: Mutex<Vec<TraceItem>>,
    /// Frontiers captured before each trace item is appended.
    /// frontiers[i] is the frontier state before trace item i was added.
    frontiers: Mutex<Vec<SerializableFrontier>>,
    bit_packer: Mutex<BitPacker>,
}

impl AuditSubscriber {
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
            trace: Mutex::new(Vec::new()),
            frontiers: Mutex::new(Vec::new()),
        }
    }
}

impl Subscriber for AuditSubscriber {
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
                // Capture the frontier before appending this item
                if let Ok(mut frontiers) = self.frontiers.lock() {
                    frontiers.push(SerializableFrontier::from_frontier(&producer.frontier()));
                }

                producer.try_append(&item).unwrap();

                if let Ok(mut trace) = self.trace.lock() {
                    trace.push(item);
                }
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
                    // Length mismatch - emit audit result with failure
                    let (trace_window, frontier_bytes) = if let Ok(trace) = self.trace.lock() {
                        let len = trace.len();
                        let window_start = len.saturating_sub(AUDIT_WINDOW_SIZE);
                        let window = trace[window_start..].to_vec();

                        // Get the frontier at window start position
                        let frontier = if let Ok(frontiers) = self.frontiers.lock() {
                            if window_start < frontiers.len() {
                                frontiers[window_start].to_bytes()
                            } else if !frontiers.is_empty() {
                                frontiers.last().unwrap().to_bytes()
                            } else {
                                Vec::new()
                            }
                        } else {
                            Vec::new()
                        };

                        (window, frontier)
                    } else {
                        (Vec::new(), Vec::new())
                    };

                    let result = AuditResult {
                        success: false,
                        verified_count: 0,
                        diff: Some(AuditDiff {
                            index: 0,
                            frontier: frontier_bytes,
                        }),
                        trace_window,
                    };
                    ipc::emit_audit(&result);
                    return;
                }

                let diff = if let Ok(bit_packer) = self.bit_packer.lock() {
                    bit_packer.diff(&computed_packed, &expected_packed)
                } else {
                    panic!("Failed to lock bit_packer");
                };

                let verified_count = computed_packed.len();

                if let Some((diff_index, _computed, _expected)) = diff {
                    // Extract trace window: last N items up to and including the diff point
                    let window_start = diff_index.saturating_sub(AUDIT_WINDOW_SIZE - 1);

                    let (trace_window, frontier_bytes) = if let Ok(trace) = self.trace.lock() {
                        let end = (diff_index + 1).min(trace.len());
                        let window = trace[window_start..end].to_vec();

                        // Get the frontier at window start position (before the first window item)
                        let frontier = if let Ok(frontiers) = self.frontiers.lock() {
                            if window_start < frontiers.len() {
                                frontiers[window_start].to_bytes()
                            } else if !frontiers.is_empty() {
                                frontiers.last().unwrap().to_bytes()
                            } else {
                                Vec::new()
                            }
                        } else {
                            Vec::new()
                        };

                        (window, frontier)
                    } else {
                        (Vec::new(), Vec::new())
                    };

                    let result = AuditResult {
                        success: false,
                        verified_count,
                        diff: Some(AuditDiff {
                            index: diff_index,
                            frontier: frontier_bytes,
                        }),
                        trace_window,
                    };
                    ipc::emit_audit(&result);
                } else {
                    // Verification passed
                    let result = AuditResult {
                        success: true,
                        verified_count,
                        diff: None,
                        trace_window: Vec::new(),
                    };
                    ipc::emit_audit(&result);
                }
            }
        }
    }
}
